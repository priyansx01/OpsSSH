//! Linux metrics collected through bounded independent SSH channels, never the PTY.
use crate::design;
use crate::localization::text as tr;
use async_channel::Sender;
use gpui::{Context, Render, Task, Window, div, prelude::*, px, rgb, uniform_list};
use gpui_component::{
    Disableable, Sizable, WindowExt,
    button::{Button, ButtonVariants},
};
use opsssh_ssh_core::{ExecControl, ExecOutput, ExecRequest, SshCommand};
use std::{collections::HashMap, time::Duration};

const METRICS: &str = r#"LC_ALL=C; export LC_ALL
[ "$(uname -s)" = Linux ] || { echo 'Infrastructure metrics currently support Linux servers.' >&2; exit 64; }
echo OPS_METRICS_V1
echo '[cpu]'; head -n 1 /proc/stat
echo '[cores]'; grep -c '^cpu[0-9]' /proc/stat
echo '[memory]'; cat /proc/meminfo
echo '[uptime]'; cat /proc/uptime
echo '[disk]'; df -Pk / | tail -n 1
echo '[pagesize]'; getconf PAGESIZE
echo '[processes]'
n=0
for f in /proc/[0-9]*/stat; do
  [ "$n" -lt 1000 ] || break
  [ -r "$f" ] || continue
  IFS= read -r line < "$f" 2>/dev/null || continue
  printf '%s\n' "$line"
  n=$((n + 1))
done
"#;

#[derive(Clone, Debug)]
struct Process {
    pid: u32,
    name: String,
    start: u64,
    ticks: u64,
    rss: Option<u64>,
    cpu: Option<f64>,
}
#[derive(Clone, Debug, Default)]
struct Snapshot {
    cpu_total: Option<u64>,
    cpu_idle: Option<u64>,
    cpu_percent: Option<f64>,
    cores: Option<u64>,
    memory_total: Option<u64>,
    memory_available: Option<u64>,
    swap_total: Option<u64>,
    swap_free: Option<u64>,
    uptime: Option<f64>,
    disk_total: Option<u64>,
    disk_used: Option<u64>,
    disk_available: Option<u64>,
    processes: Vec<Process>,
}
fn process_stat(line: &str, page_size: Option<u64>) -> Option<Process> {
    let (pid, rest) = line.split_once(" (")?;
    let end = rest.rfind(") ")?;
    let fields: Vec<&str> = rest[end + 2..].split_whitespace().collect();
    Some(Process {
        pid: pid.parse().ok()?,
        name: rest[..end]
            .chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .take(128)
            .collect(),
        start: fields.get(19)?.parse().ok()?,
        ticks: fields
            .get(11)?
            .parse::<u64>()
            .ok()?
            .checked_add(fields.get(12)?.parse().ok()?)?,
        rss: fields
            .get(21)?
            .parse::<u64>()
            .ok()?
            .checked_mul(page_size.unwrap_or(0))
            .filter(|_| page_size.is_some()),
        cpu: None,
    })
}
fn parse_snapshot(bytes: &[u8]) -> Result<Snapshot, String> {
    let text = std::str::from_utf8(bytes).map_err(|_| tr("infra-invalid-encoding"))?;
    let mut lines = text.lines();
    if lines.next() != Some("OPS_METRICS_V1") {
        return Err(tr("infra-unexpected-response"));
    }
    let mut snapshot = Snapshot::default();
    let mut section = "";
    let mut page_size = None;
    for line in lines {
        if line.starts_with('[') && line.ends_with(']') {
            section = line;
            continue;
        }
        match section {
            "[cpu]" => {
                let values: Vec<u64> = line
                    .split_whitespace()
                    .skip(1)
                    .take(8)
                    .map(|v| v.parse())
                    .collect::<Result<Vec<_>, _>>()
                    .unwrap_or_default();
                if values.len() >= 4 {
                    snapshot.cpu_total = values.iter().try_fold(0u64, |a, b| a.checked_add(*b));
                    snapshot.cpu_idle = values[3].checked_add(values.get(4).copied().unwrap_or(0));
                }
            }
            "[cores]" => snapshot.cores = line.parse().ok().filter(|n| *n > 0),
            "[memory]" => {
                let mut fields = line.split_whitespace();
                let key = fields.next().unwrap_or("");
                let value = fields
                    .next()
                    .and_then(|v| v.parse::<u64>().ok())
                    .and_then(|v| v.checked_mul(1024));
                match key {
                    "MemTotal:" => snapshot.memory_total = value,
                    "MemAvailable:" => snapshot.memory_available = value,
                    "SwapTotal:" => snapshot.swap_total = value,
                    "SwapFree:" => snapshot.swap_free = value,
                    _ => {}
                }
            }
            "[uptime]" => {
                snapshot.uptime = line
                    .split_whitespace()
                    .next()
                    .and_then(|v| v.parse::<f64>().ok())
                    .filter(|v| v.is_finite() && *v >= 0.)
            }
            "[disk]" => {
                let values: Vec<_> = line.split_whitespace().collect();
                let blocks = |i: usize| {
                    values
                        .get(i)
                        .and_then(|v| v.parse::<u64>().ok())
                        .and_then(|v| v.checked_mul(1024))
                };
                snapshot.disk_total = blocks(1);
                snapshot.disk_used = blocks(2);
                snapshot.disk_available = blocks(3);
            }
            "[pagesize]" => page_size = line.parse().ok().filter(|n| *n > 0),
            "[processes]" if snapshot.processes.len() < 1000 => {
                if let Some(process) = process_stat(line, page_size) {
                    snapshot.processes.push(process);
                }
            }
            _ => {}
        }
    }
    Ok(snapshot)
}
fn sample_delta(current: &mut Snapshot, previous: &Snapshot) {
    if current
        .uptime
        .zip(previous.uptime)
        .is_some_and(|(a, b)| a < b)
    {
        return;
    }
    let delta = current
        .cpu_total
        .zip(previous.cpu_total)
        .and_then(|(a, b)| a.checked_sub(b))
        .filter(|d| *d > 0);
    if let Some(total) = delta {
        current.cpu_percent = current
            .cpu_idle
            .zip(previous.cpu_idle)
            .and_then(|(a, b)| a.checked_sub(b))
            .map(|idle| (100. * (1. - idle as f64 / total as f64)).clamp(0., 100.));
        let processes: HashMap<_, _> = previous
            .processes
            .iter()
            .map(|p| ((p.pid, p.start), p.ticks))
            .collect();
        for process in &mut current.processes {
            process.cpu = processes
                .get(&(process.pid, process.start))
                .and_then(|old| process.ticks.checked_sub(*old))
                .map(|ticks| {
                    100. * ticks as f64 * current.cores.unwrap_or(1) as f64 / total as f64
                });
        }
    }
}
fn bytes_label(value: Option<u64>) -> String {
    value
        .map(|v| format!("{:.2} GiB", v as f64 / 1_073_741_824.))
        .unwrap_or_else(|| tr("infra-unavailable"))
}
fn process_memory_label(value: Option<u64>) -> String {
    value
        .map(|bytes| {
            if bytes >= 1_073_741_824 {
                format!("{:.2} GiB", bytes as f64 / 1_073_741_824.)
            } else if bytes >= 1_048_576 {
                format!("{:.1} MiB", bytes as f64 / 1_048_576.)
            } else if bytes >= 1024 {
                format!("{:.1} KiB", bytes as f64 / 1024.)
            } else {
                format!("{bytes} B")
            }
        })
        .unwrap_or_else(|| tr("infra-unavailable"))
}
fn percent(used: Option<u64>, total: Option<u64>) -> Option<f64> {
    used.zip(total)
        .filter(|(_, total)| *total > 0)
        .map(|(used, total)| 100. * used as f64 / total as f64)
}
fn percent_label(value: Option<f64>) -> String {
    value
        .map(|v| format!("{v:.1}%"))
        .unwrap_or_else(|| "—".into())
}

/// One entity per connection generation. Caller sets visibility whenever navigation changes.
pub struct InfrastructureView {
    commands: Sender<SshCommand>,
    generation: u64,
    visible: bool,
    collect_enabled: bool,
    snapshot: Option<Snapshot>,
    message: String,
    busy: bool,
    request_serial: u64,
    operation: Option<ExecControl>,
    poll: Option<Task<()>>,
    sort: Sort,
    descending: bool,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Sort {
    Pid,
    Name,
    Cpu,
    Memory,
}
impl InfrastructureView {
    pub fn new(
        commands: Sender<SshCommand>,
        generation: u64,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Self {
        Self {
            commands,
            generation,
            visible: false,
            collect_enabled: true,
            snapshot: None,
            message: tr("infra-linux-metrics"),
            busy: false,
            request_serial: 0,
            operation: None,
            poll: None,
            sort: Sort::Cpu,
            descending: true,
        }
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
    /// Visual fixture; no SSH, filesystem, timers, or user data.
    #[cfg(feature = "capture")]
    pub fn preview(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (commands, _) = async_channel::bounded(1);
        let mut view = Self::new(commands, 0, window, cx);
        view.collect_enabled = false;
        view.visible = true;
        view.snapshot = Some(Snapshot {
            cpu_percent: Some(24.6),
            cores: Some(8),
            memory_total: Some(16 * 1_073_741_824),
            memory_available: Some(9 * 1_073_741_824),
            swap_total: Some(2 * 1_073_741_824),
            swap_free: Some(2 * 1_073_741_824),
            disk_total: Some(120 * 1_073_741_824),
            disk_used: Some(48 * 1_073_741_824),
            disk_available: Some(72 * 1_073_741_824),
            uptime: Some(1_146_600.),
            processes: [
                "application-worker",
                "postgres",
                "nginx",
                "redis-server",
                "sshd",
                "systemd",
                "journald",
                "node",
            ]
            .into_iter()
            .enumerate()
            .map(|(index, name)| Process {
                pid: 1024 + index as u32,
                name: name.into(),
                start: index as u64,
                ticks: 0,
                rss: Some((512 - index as u64 * 48) * 1_048_576),
                cpu: Some(21.6 - index as f64 * 2.5),
            })
            .collect(),
            ..Default::default()
        });
        view.message = tr("infra-preview");
        view
    }
    pub fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        if self.visible == visible {
            return;
        }
        self.visible = visible;
        self.poll = None;
        if !self.collect_enabled {
            cx.notify();
            return;
        }
        if visible {
            self.refresh(cx);
            self.poll = Some(cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor().timer(Duration::from_secs(5)).await;
                    if !this
                        .update(cx, |this, cx| {
                            if this.visible {
                                this.refresh(cx);
                            }
                            this.visible
                        })
                        .unwrap_or(false)
                    {
                        break;
                    }
                }
            }));
        } else {
            self.request_serial = self.request_serial.wrapping_add(1);
            if let Some(control) = self.operation.take() {
                control.cancel();
            }
            self.busy = false;
        }
        cx.notify();
    }
    fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.busy || !self.visible || !self.collect_enabled {
            return;
        }
        let request = ExecRequest::new(METRICS);
        self.operation = Some(request.control.clone());
        self.busy = true;
        self.request_serial = self.request_serial.wrapping_add(1);
        let serial = self.request_serial;
        let (reply, receive) = async_channel::bounded(1);
        if self
            .commands
            .try_send(SshCommand::Exec { request, reply })
            .is_err()
        {
            self.busy = false;
            self.operation = None;
            self.message = tr("infra-session-unavailable");
            cx.notify();
            return;
        }
        cx.spawn(async move |this, cx| {
            let result = receive
                .recv()
                .await
                .unwrap_or_else(|_| Err(tr("infra-session-closed")));
            let _ = this.update(cx, |this, cx| {
                if serial != this.request_serial {
                    return;
                }
                this.busy = false;
                this.operation = None;
                match result
                    .and_then(check_output)
                    .and_then(|output| parse_snapshot(&output.stdout))
                {
                    Ok(mut snapshot) => {
                        if let Some(previous) = &this.snapshot {
                            sample_delta(&mut snapshot, previous);
                        }
                        this.snapshot = Some(snapshot);
                        this.sort_processes();
                        this.message = tr("infra-refresh-help");
                    }
                    Err(error) => {
                        this.snapshot = None;
                        this.message = format!("{}: {error}", tr("infra-metrics-unavailable"));
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
    fn sort_processes(&mut self) {
        if let Some(snapshot) = &mut self.snapshot {
            snapshot.processes.sort_by(|a, b| {
                let order = match self.sort {
                    Sort::Pid => a.pid.cmp(&b.pid),
                    Sort::Name => a.name.cmp(&b.name),
                    Sort::Cpu => a.cpu.unwrap_or(-1.).total_cmp(&b.cpu.unwrap_or(-1.)),
                    Sort::Memory => a.rss.cmp(&b.rss),
                };
                if self.descending {
                    order.reverse()
                } else {
                    order
                }
            });
        }
    }
    fn confirm_terminate(&mut self, process: Process, window: &mut Window, cx: &mut Context<Self>) {
        let entity = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, _, _| {
            let entity = entity.clone();
            let process = process.clone();
            let target = process.clone();
            dialog
                .title(tr("infra-terminate-title"))
                .width(px(440.))
                .overlay_closable(false)
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_4()
                        .child(format!("{} · PID {}", process.name, process.pid))
                        .child(tr("infra-terminate-help"))
                        .child(
                            div()
                                .flex()
                                .gap_2()
                                .child(
                                    Button::new("terminate-process")
                                        .danger()
                                        .label(tr("infra-terminate"))
                                        .on_click(move |_, window, cx| {
                                            let target = target.clone();
                                            let _ = entity
                                                .update(cx, |this, cx| this.terminate(target, cx));
                                            window.close_dialog(cx);
                                        }),
                                )
                                .child(
                                    Button::new("cancel-terminate")
                                        .label(tr("cancel"))
                                        .on_click(|_, window, cx| window.close_dialog(cx)),
                                ),
                        ),
                )
        });
    }
    fn terminate(&mut self, process: Process, cx: &mut Context<Self>) {
        if self.busy || !self.visible || !self.collect_enabled {
            self.message = tr("infra-terminate-wait");
            cx.notify();
            return;
        }
        if process.pid <= 1 {
            self.message = tr("infra-init-protected");
            cx.notify();
            return;
        }
        // Only parsed numeric identity fields enter the shell command. Re-check in the same command immediately before kill.
        let command = format!(
            "LC_ALL=C; s=$(cat /proc/{}/stat 2>/dev/null) || exit 3; r=${{s##*) }}; set -- $r; [ \"${{20}}\" = '{}' ] || {{ echo 'Process identity changed; nothing was terminated.' >&2; exit 4; }}; kill -TERM {}",
            process.pid, process.start, process.pid
        );
        let request = ExecRequest::new(command);
        self.operation = Some(request.control.clone());
        self.busy = true;
        self.request_serial = self.request_serial.wrapping_add(1);
        let serial = self.request_serial;
        let (reply, receive) = async_channel::bounded(1);
        if self
            .commands
            .try_send(SshCommand::Exec { request, reply })
            .is_err()
        {
            self.busy = false;
            self.operation = None;
            self.message = tr("infra-session-unavailable");
            cx.notify();
            return;
        }
        cx.spawn(async move |this, cx| {
            let result = receive
                .recv()
                .await
                .unwrap_or_else(|_| Err(tr("infra-session-closed")))
                .and_then(check_output);
            let _ = this.update(cx, |this, cx| {
                if serial != this.request_serial {
                    return;
                }
                this.busy = false;
                this.operation = None;
                this.message = match result {
                    Ok(_) => format!("{} {}", tr("infra-terminate-sent"), process.pid),
                    Err(error) => format!("{}: {error}", tr("infra-terminate-failed")),
                };
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}
fn check_output(output: ExecOutput) -> Result<ExecOutput, String> {
    match output.exit_status {
        Some(0) => Ok(output),
        Some(64) => Err(tr("infra-linux-only")),
        status => {
            let stderr: String = String::from_utf8_lossy(&output.stderr)
                .chars()
                .filter(|c| !c.is_control() || *c == '\n')
                .take(300)
                .collect();
            Err(if stderr.trim().is_empty() {
                format!("{} ({status:?})", tr("infra-command-failed"))
            } else {
                stderr.trim().into()
            })
        }
    }
}
impl Drop for InfrastructureView {
    fn drop(&mut self) {
        if let Some(control) = &self.operation {
            control.cancel();
        }
    }
}
impl Render for InfrastructureView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = design::palette(cx);
        let compact = window.viewport_size().width < px(850.);
        let pid_width = if compact { 64. } else { 100. };
        let cpu_width = if compact { 76. } else { 100. };
        let memory_width = if compact { 88. } else { 100. };
        let action_width = if compact { 80. } else { 86. };
        let snapshot = self.snapshot.clone().unwrap_or_default();
        let memory_used = snapshot
            .memory_total
            .zip(snapshot.memory_available)
            .and_then(|(a, b)| a.checked_sub(b));
        let card =
            |title: &str, value: String, detail: String, usage: Option<f64>| {
                div()
                    .flex_1()
                    .min_w(px(150.))
                    .p_5()
                    .rounded_lg()
                    .border_1()
                    .border_color(rgb(p.border))
                    .bg(rgb(p.surface))
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgb(p.muted))
                            .child(title.to_string()),
                    )
                    .child(
                        div()
                            .text_2xl()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(value),
                    )
                    .child(
                        div()
                            .w_full()
                            .h(px(5.))
                            .rounded_full()
                            .bg(rgb(p.hover))
                            .child(div().h_full().rounded_full().bg(rgb(p.ruby)).w(
                                gpui::relative((usage.unwrap_or(0.).clamp(0., 100.) / 100.) as f32),
                            )),
                    )
                    .child(div().text_xs().text_color(rgb(p.muted)).child(detail))
            };
        let uptime = snapshot
            .uptime
            .map(|v| {
                format!(
                    "{}d {}h {}m",
                    v as u64 / 86400,
                    v as u64 % 86400 / 3600,
                    v as u64 % 3600 / 60
                )
            })
            .unwrap_or_else(|| tr("infra-unavailable"));
        let cpu = snapshot.cpu_percent;
        let memory = percent(memory_used, snapshot.memory_total);
        let disk = percent(snapshot.disk_used, snapshot.disk_total);
        let mut header = div()
            .w_full()
            .min_w_0()
            .flex_shrink_0()
            .flex()
            .items_center()
            .gap_2()
            .h(px(42.))
            .px_3()
            .bg(rgb(p.raised));
        for (index, label, sort) in [
            (0usize, "infra-pid", Sort::Pid),
            (1, "infra-process", Sort::Name),
            (2, "infra-cpu-column", Sort::Cpu),
            (3, "infra-memory", Sort::Memory),
        ] {
            header = header.child(
                div()
                    .flex()
                    .items_center()
                    .when(index == 1, |d| d.flex_1().min_w_0())
                    .when(index != 1, |d| {
                        d.w(px(match index {
                            0 => pid_width,
                            2 => cpu_width,
                            _ => memory_width,
                        }))
                        .flex_shrink_0()
                    })
                    .child(
                        Button::new(("process-sort", index))
                            .small()
                            .ghost()
                            .label(format!(
                                "{}{}",
                                tr(label),
                                if self.sort == sort {
                                    if self.descending { " ↓" } else { " ↑" }
                                } else {
                                    ""
                                }
                            ))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if this.sort == sort {
                                    this.descending = !this.descending;
                                } else {
                                    this.sort = sort;
                                    this.descending = matches!(sort, Sort::Cpu | Sort::Memory);
                                }
                                this.sort_processes();
                                cx.notify();
                            })),
                    ),
            );
        }
        header = header.child(
            div()
                .w(px(action_width))
                .flex_shrink_0()
                .text_xs()
                .text_color(rgb(p.muted))
                .child(tr("infra-action")),
        );
        let process_count = snapshot.processes.len();
        div()
            .size_full()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_4()
            .p_5()
            .text_color(rgb(p.text))
            .bg(rgb(p.canvas))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_xl()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(tr("nav-infrastructure")),
                    )
                    .child(
                        Button::new("refresh-metrics")
                            .label(tr(if self.busy {
                                "infra-refreshing"
                            } else {
                                "infra-refresh"
                            }))
                            .disabled(self.busy || !self.visible || !self.collect_enabled)
                            .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_3()
                    .child(card(
                        &tr("infra-processor"),
                        percent_label(cpu),
                        format!(
                            "{} {} · {} {uptime}",
                            snapshot
                                .cores
                                .map(|v| v.to_string())
                                .unwrap_or_else(|| "—".into()),
                            tr("infra-cores"),
                            tr("infra-uptime")
                        ),
                        cpu,
                    ))
                    .child(card(
                        &tr("infra-memory"),
                        percent_label(memory),
                        format!(
                            "{} / {} · {} {} · {} {} / {}",
                            bytes_label(memory_used),
                            bytes_label(snapshot.memory_total),
                            tr("infra-available"),
                            bytes_label(snapshot.memory_available),
                            tr("infra-swap"),
                            bytes_label(
                                snapshot
                                    .swap_total
                                    .zip(snapshot.swap_free)
                                    .and_then(|(a, b)| a.checked_sub(b))
                            ),
                            bytes_label(snapshot.swap_total)
                        ),
                        memory,
                    ))
                    .child(card(
                        &tr("infra-root-disk"),
                        percent_label(disk),
                        format!(
                            "{} / {} · {} {}",
                            bytes_label(snapshot.disk_used),
                            bytes_label(snapshot.disk_total),
                            tr("infra-available"),
                            bytes_label(snapshot.disk_available)
                        ),
                        disk,
                    )),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(p.muted))
                    .child(self.message.clone()),
            )
            .child(
                div()
                    .text_sm()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(format!(
                        "{} ({process_count}) · {}",
                        tr("infra-processes"),
                        tr("infra-process-cpu-help")
                    )),
            )
            .child(
                div()
                    .flex_1()
                    .w_full()
                    .min_w_0()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .rounded_lg()
                    .border_1()
                    .border_color(rgb(p.border))
                    .overflow_hidden()
                    .child(header)
                    .child(
                        uniform_list(
                            "infrastructure-processes",
                            process_count,
                            cx.processor(
                                move |this, range: std::ops::Range<usize>, _window, cx| {
                                    range
                                        .filter_map(|index| {
                                            this.snapshot
                                                .as_ref()?
                                                .processes
                                                .get(index)
                                                .cloned()
                                                .map(|process| (index, process))
                                        })
                                        .map(|(index, process)| {
                                            let target = process.clone();
                                            div()
                                                .id(("process-row", index))
                                                .w_full()
                                                .min_w_0()
                                                .flex()
                                                .items_center()
                                                .gap_2()
                                                .px_3()
                                                .h(px(42.))
                                                .border_b_1()
                                                .border_color(rgb(p.border))
                                                .hover(|d| d.bg(rgb(p.hover)))
                                                .child(
                                                    div()
                                                        .w(px(pid_width))
                                                        .flex_shrink_0()
                                                        .text_sm()
                                                        .child(process.pid.to_string()),
                                                )
                                                .child(
                                                    div()
                                                        .flex_1()
                                                        .min_w_0()
                                                        .text_sm()
                                                        .truncate()
                                                        .child(process.name),
                                                )
                                                .child(
                                                    div()
                                                        .w(px(cpu_width))
                                                        .flex_shrink_0()
                                                        .text_sm()
                                                        .child(percent_label(process.cpu)),
                                                )
                                                .child(
                                                    div()
                                                        .w(px(memory_width))
                                                        .flex_shrink_0()
                                                        .text_sm()
                                                        .child(process_memory_label(process.rss)),
                                                )
                                                .child(
                                                    div()
                                                        .w(px(action_width))
                                                        .flex_shrink_0()
                                                        .child(
                                                            Button::new((
                                                                "process-terminate",
                                                                index,
                                                            ))
                                                            .xsmall()
                                                            .ghost()
                                                            .label(tr("infra-terminate"))
                                                            .disabled(this.busy || process.pid <= 1)
                                                            .on_click(cx.listener(
                                                                move |this, _, window, cx| {
                                                                    this.confirm_terminate(
                                                                        target.clone(),
                                                                        window,
                                                                        cx,
                                                                    )
                                                                },
                                                            )),
                                                        ),
                                                )
                                        })
                                        .collect()
                                },
                            ),
                        )
                        .w_full()
                        .min_w_0()
                        .flex_1()
                        .min_h_0(),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn stat(pid: u32, name: &str, start: u64, ticks: u64) -> String {
        let mut fields = vec!["0".to_string(); 22];
        fields[0] = "S".into();
        fields[11] = ticks.to_string();
        fields[19] = start.to_string();
        fields[21] = "10".into();
        format!("{pid} ({name}) {}", fields.join(" "))
    }
    fn fixture(total: u64, idle: u64, process: &str) -> Vec<u8> {
        format!("OPS_METRICS_V1\n[cpu]\ncpu {total} 0 0 {idle}\n[cores]\n2\n[memory]\nMemTotal: 1024 kB\nMemAvailable: 256 kB\nSwapTotal: 0 kB\nSwapFree: 0 kB\n[uptime]\n100.0 0\n[disk]\n/dev/root 1000 250 750 25% /\n[pagesize]\n4096\n[processes]\n{process}\n").into_bytes()
    }
    #[test]
    fn parses_metrics_and_process_names_with_parentheses() {
        let s = parse_snapshot(&fixture(100, 100, &stat(42, "worker (a) b", 7, 10))).unwrap();
        assert_eq!(s.memory_total, Some(1048576));
        assert_eq!(s.disk_used, Some(256000));
        assert_eq!(s.processes[0].name, "worker (a) b");
        assert_eq!(s.processes[0].rss, Some(40960));
    }
    #[test]
    fn computes_cpu_deltas_and_ignores_reused_pid() {
        let previous = parse_snapshot(&fixture(100, 100, &stat(42, "work", 7, 10))).unwrap();
        let mut current = parse_snapshot(&fixture(150, 150, &stat(42, "work", 7, 20))).unwrap();
        sample_delta(&mut current, &previous);
        assert_eq!(current.cpu_percent, Some(50.));
        assert_eq!(current.processes[0].cpu, Some(20.));
        current.processes[0].start = 8;
        sample_delta(&mut current, &previous);
        assert_eq!(current.processes[0].cpu, None);
    }
    #[test]
    fn missing_values_are_unavailable_and_malformed_processes_are_skipped() {
        let s =
            parse_snapshot(b"OPS_METRICS_V1\n[memory]\nMemTotal: bad kB\n[processes]\ninvalid\n")
                .unwrap();
        assert!(s.memory_total.is_none());
        assert!(s.processes.is_empty());
        assert!(parse_snapshot(b"Darwin").is_err());
    }
    #[test]
    fn counter_reset_does_not_create_false_cpu_usage() {
        let previous = parse_snapshot(&fixture(150, 150, &stat(42, "work", 7, 20))).unwrap();
        let mut current = parse_snapshot(&fixture(100, 100, &stat(42, "work", 7, 10))).unwrap();
        sample_delta(&mut current, &previous);
        assert!(current.cpu_percent.is_none());
        assert!(current.processes[0].cpu.is_none());
    }
    #[test]
    fn missing_page_size_keeps_process_memory_unavailable() {
        let s = parse_snapshot(
            format!(
                "OPS_METRICS_V1\n[processes]\n{}\n",
                stat(42, "worker", 7, 10)
            )
            .as_bytes(),
        )
        .unwrap();
        assert_eq!(s.processes.len(), 1);
        assert!(s.processes[0].rss.is_none());
    }
    #[test]
    fn malformed_cpu_counter_does_not_shift_idle_field() {
        let s = parse_snapshot(b"OPS_METRICS_V1\n[cpu]\ncpu 100 invalid 200 300 400\n").unwrap();
        assert!(s.cpu_total.is_none());
        assert!(s.cpu_idle.is_none());
    }
}
