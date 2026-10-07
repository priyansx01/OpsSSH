//! SFTP pane: all protocol, disk and transfer work runs on one background runtime.
use crate::localization::text as tr;
use async_channel::{Receiver, Sender};
use gpui::{
    ClipboardEntry, ClipboardItem, Context, Entity, EventEmitter, PathPromptOptions, Render, Task,
    Window, div, prelude::*, px, uniform_list,
};
use gpui_component::{
    ActiveTheme, Disableable, Sizable,
    button::{Button, ButtonVariants},
    input::{Input, InputEvent, InputState},
};
use opsssh_sftp::{Entry, SftpClient, TransferControl, WriteMode};
use opsssh_ssh_core::SshCommand;
use std::{path::PathBuf, time::Duration};

pub enum FilePaneEvent {
    InsertText(String),
}
enum Job {
    List(String),
    Upload {
        paths: Vec<PathBuf>,
        target: Option<String>,
        insert: bool,
        control: TransferControl,
    },
    Download {
        remote: String,
        local: PathBuf,
        control: TransferControl,
    },
    Attachment {
        bytes: Vec<u8>,
        name: String,
        control: TransferControl,
    },
    Mkdir(String),
    Delete(String),
}
enum Update {
    Listed(String, Vec<Entry>),
    Message(String),
    Unavailable(String),
    Insert(String),
    Started(u64),
    Finished(u64, bool),
}
struct QueuedJob {
    id: u64,
    job: Job,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum TransferState {
    Queued,
    Running,
    Cancelling,
    Completed,
    Cancelled,
    Failed,
}
impl TransferState {
    fn active(self) -> bool {
        matches!(self, Self::Queued | Self::Running | Self::Cancelling)
    }
    fn label(self) -> String {
        tr(match self {
            Self::Queued => "file-task-queued",
            Self::Running => "file-task-running",
            Self::Cancelling => "file-task-cancelling",
            Self::Completed => "file-task-completed",
            Self::Cancelled => "file-task-cancelled",
            Self::Failed => "file-task-failed",
        })
    }
}
struct TransferTask {
    id: u64,
    label: String,
    control: TransferControl,
    state: TransferState,
    bytes: u64,
}
pub struct FilePane {
    jobs: Sender<QueuedJob>,
    list_focus: gpui::FocusHandle,
    list_scroll: gpui::UniformListScrollHandle,
    path: Entity<InputState>,
    folder: Entity<InputState>,
    entries: Vec<Entry>,
    selected: Option<usize>,
    message: String,
    pending_delete: Option<String>,
    pending: usize,
    next_id: u64,
    transfers: Vec<TransferTask>,
    progress_task: Option<Task<()>>,
}
impl EventEmitter<FilePaneEvent> for FilePane {}
impl FilePane {
    /// In-memory visual fixture: no SSH channel, filesystem work, or user data.
    #[cfg(feature = "capture")]
    pub fn preview(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (jobs, _) = async_channel::bounded(32);
        let path = cx.new(|cx| InputState::new(window, cx).default_value("/srv/application"));
        let folder =
            cx.new(|cx| InputState::new(window, cx).placeholder(tr("file-folder-placeholder")));
        Self {
            jobs,
            list_focus: cx.focus_handle(),
            list_scroll: Default::default(),
            path,
            folder,
            entries: [
                ("logs", true, 0),
                ("releases", true, 0),
                ("application.toml", false, 2048),
                (
                    "a-long-file-name-to-check-the-panel-width.log",
                    false,
                    1048576,
                ),
            ]
            .into_iter()
            .map(|(name, directory, size)| Entry {
                name: name.into(),
                path: format!("/srv/application/{name}"),
                size: Some(size),
                modified: None,
                is_directory: directory,
                is_symlink: false,
            })
            .collect(),
            selected: Some(2),
            message: tr("file-entry-count"),
            pending_delete: None,
            pending: 0,
            next_id: 1,
            transfers: vec![TransferTask {
                id: 1,
                label: "application.toml".into(),
                control: TransferControl::default(),
                state: TransferState::Completed,
                bytes: 2048,
            }],
            progress_task: None,
        }
    }
    pub fn new(
        commands: Sender<SshCommand>,
        fingerprint: String,
        initial_path: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (jobs, receiver) = async_channel::bounded(32);
        let (updates, receive) = async_channel::bounded(32);
        let path = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(initial_path.clone())
                .placeholder(tr("file-path-placeholder"))
        });
        let folder =
            cx.new(|cx| InputState::new(window, cx).placeholder(tr("file-folder-placeholder")));
        cx.subscribe_in(&path, window, |this, _, event, _, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.refresh(cx);
            }
        })
        .detach();
        let thread = std::thread::Builder::new()
            .name("opsssh-files".into())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build();
                match runtime {
                    Ok(runtime) => {
                        if let Err(error) = runtime.block_on(worker(
                            commands,
                            fingerprint,
                            receiver,
                            updates.clone(),
                        )) {
                            let _ = updates.send_blocking(Update::Unavailable(error.to_string()));
                        }
                    }
                    Err(error) => {
                        let _ = updates.send_blocking(Update::Unavailable(error.to_string()));
                    }
                }
            });
        cx.spawn_in(window, async move |this, cx| {
            while let Ok(update) = receive.recv().await {
                if this
                    .update_in(cx, |this, window, cx| {
                        match update {
                            Update::Listed(path, entries) => {
                                this.path
                                    .update(cx, |state, cx| state.set_value(path, window, cx));
                                this.entries = entries;
                                this.selected = None;
                                this.message =
                                    format!("{}: {}", tr("file-entry-count"), this.entries.len());
                            }
                            Update::Message(message) => this.message = message,
                            Update::Unavailable(message) => {
                                this.message = message;
                                this.pending = 0;
                                for task in &mut this.transfers {
                                    if task.state.active() {
                                        task.state = if task.state == TransferState::Cancelling {
                                            TransferState::Cancelled
                                        } else {
                                            TransferState::Failed
                                        };
                                    }
                                }
                            }
                            Update::Insert(text) => cx.emit(FilePaneEvent::InsertText(text)),
                            Update::Started(id) => {
                                if let Some(task) =
                                    this.transfers.iter_mut().find(|task| task.id == id)
                                    && task.state != TransferState::Cancelling
                                {
                                    task.state = TransferState::Running;
                                }
                                this.watch_progress(window, cx);
                            }
                            Update::Finished(id, success) => {
                                if let Some(task) =
                                    this.transfers.iter_mut().find(|task| task.id == id)
                                {
                                    task.bytes = task.control.transferred();
                                    task.state = if success {
                                        TransferState::Completed
                                    } else if task.state == TransferState::Cancelling {
                                        TransferState::Cancelled
                                    } else {
                                        TransferState::Failed
                                    };
                                }
                                this.pending = this.pending.saturating_sub(1);
                            }
                        };
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        let mut pane = Self {
            jobs,
            list_focus: cx.focus_handle(),
            list_scroll: Default::default(),
            path,
            folder,
            entries: vec![],
            selected: None,
            message: tr("file-opening"),
            pending_delete: None,
            pending: 0,
            next_id: 0,
            transfers: vec![],
            progress_task: None,
        };
        match thread {
            Ok(_) => pane.enqueue(
                Job::List(if initial_path.is_empty() {
                    ".".into()
                } else {
                    initial_path
                }),
                cx,
            ),
            Err(error) => pane.message = format!("{}: {error}", tr("file-worker-error")),
        };
        pane
    }
    fn enqueue(&mut self, job: Job, cx: &mut Context<Self>) {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        let transfer = match &job {
            Job::Upload { paths, control, .. } => Some((
                format!(
                    "{} · {}",
                    tr("file-upload"),
                    paths
                        .iter()
                        .filter_map(|p| p.file_name())
                        .map(|n| n.to_string_lossy().into_owned())
                        .take(3)
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                control.clone(),
            )),
            Job::Download {
                remote, control, ..
            } => Some((
                format!(
                    "{} · {}",
                    tr("file-download"),
                    remote.rsplit('/').next().unwrap_or(remote)
                ),
                control.clone(),
            )),
            Job::Attachment { name, control, .. } => Some((
                format!("{} · {name}", tr("file-attachment")),
                control.clone(),
            )),
            _ => None,
        };
        match self.jobs.try_send(QueuedJob { id, job }) {
            Ok(()) => {
                self.pending += 1;
                if let Some((label, control)) = transfer {
                    if self.transfers.len() >= 32 {
                        self.transfers.retain(|task| task.state.active());
                    }
                    self.transfers.push(TransferTask {
                        id,
                        label,
                        control,
                        state: TransferState::Queued,
                        bytes: 0,
                    });
                }
                self.message = format!("{}: {}", tr("file-pending-count"), self.pending);
            }
            Err(_) => self.message = tr("file-queue-error"),
        };
        cx.notify();
    }
    fn watch_progress(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.progress_task.is_some()
            || !self
                .transfers
                .iter()
                .any(|t| matches!(t.state, TransferState::Running | TransferState::Cancelling))
        {
            return;
        }
        let timer = cx.background_executor().timer(Duration::from_millis(200));
        self.progress_task = Some(cx.spawn_in(window, async move |this, cx| {
            timer.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.progress_task = None;
                let mut changed = false;
                for task in &mut this.transfers {
                    let bytes = task.control.transferred();
                    if bytes != task.bytes {
                        task.bytes = bytes;
                        changed = true;
                    }
                }
                if changed {
                    cx.notify();
                }
                this.watch_progress(window, cx);
            });
        }));
    }
    fn cancel_transfer(&mut self, id: u64, cx: &mut Context<Self>) {
        if let Some(task) = self
            .transfers
            .iter_mut()
            .find(|task| task.id == id && task.state.active())
        {
            task.control.cancel();
            task.state = TransferState::Cancelling;
            self.message = tr("file-cancel-requested");
            cx.notify();
        }
    }
    fn refresh(&mut self, cx: &mut Context<Self>) {
        self.enqueue(Job::List(self.path.read(cx).value().to_string()), cx);
    }
    pub fn follow_directory(&mut self, path: String, cx: &mut Context<Self>) {
        if self.path.read(cx).value().as_ref() != path {
            self.enqueue(Job::List(path), cx);
        }
    }
    pub fn upload_files(
        &mut self,
        paths: Vec<PathBuf>,
        target: Option<String>,
        insert: bool,
        cx: &mut Context<Self>,
    ) {
        let control = TransferControl::default();
        self.enqueue(
            Job::Upload {
                paths,
                target,
                insert,
                control,
            },
            cx,
        );
    }
    pub fn upload_clipboard(&mut self, item: ClipboardItem, cx: &mut Context<Self>) {
        for entry in item.entries {
            match entry {
                ClipboardEntry::ExternalPaths(paths) => {
                    self.upload_files(paths.0.iter().cloned().collect(), None, true, cx)
                }
                ClipboardEntry::Image(image) => {
                    if image.format != gpui::ImageFormat::Png
                        || image.bytes.len() > 64 * 1024 * 1024
                    {
                        self.message = tr("file-image-format-error");
                        cx.notify();
                        continue;
                    }
                    let control = TransferControl::default();
                    self.enqueue(
                        Job::Attachment {
                            bytes: image.bytes,
                            name: "screenshot.png".into(),
                            control,
                        },
                        cx,
                    );
                }
                ClipboardEntry::String(_) => {}
            }
        }
    }
    fn pick_upload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let picker = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: true,
            multiple: true,
            prompt: None,
        });
        cx.spawn_in(window, async move |this, cx| match picker.await {
            Ok(Ok(Some(paths))) => {
                let _ = this.update_in(cx, |this, _, cx| {
                    let target = this.path.read(cx).value().to_string();
                    this.upload_files(paths, Some(target), false, cx);
                });
            }
            Ok(Err(error)) => {
                let _ = this.update_in(cx, |this, _, cx| {
                    this.message = error.to_string();
                    cx.notify();
                });
            }
            _ => {}
        })
        .detach();
    }
    fn pick_download(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(entry) = self
            .selected
            .and_then(|index| self.entries.get(index))
            .cloned()
        else {
            return;
        };
        if entry.is_directory {
            self.message = tr("file-select-download");
            cx.notify();
            return;
        }
        let directory = opsssh_platform::home_dir().unwrap_or_default();
        let picker = cx.prompt_for_new_path(&directory, Some(&entry.name));
        cx.spawn_in(window, async move |this, cx| match picker.await {
            Ok(Ok(Some(local))) => {
                let _ = this.update_in(cx, |this, _, cx| {
                    let control = TransferControl::default();
                    this.enqueue(
                        Job::Download {
                            remote: entry.path,
                            local,
                            control,
                        },
                        cx,
                    );
                });
            }
            Ok(Err(error)) => {
                let _ = this.update_in(cx, |this, _, cx| {
                    this.message = error.to_string();
                    cx.notify();
                });
            }
            _ => {}
        })
        .detach();
    }
}
impl Drop for FilePane {
    fn drop(&mut self) {
        for task in &self.transfers {
            task.control.cancel();
        }
        self.jobs.close();
    }
}
impl Render for FilePane {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (background, foreground, border, muted, subtle) = (
            theme.sidebar,
            theme.sidebar_foreground,
            theme.border,
            theme.muted_foreground,
            theme.muted,
        );
        let mut pane = div()
            .id("file-pane")
            .track_focus(&self.list_focus)
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                if !this.list_focus.is_focused(window) || this.entries.is_empty() {
                    return;
                }
                match event.keystroke.key.as_str() {
                    "down" => {
                        this.selected = Some(
                            this.selected
                                .map_or(0, |i| (i + 1).min(this.entries.len() - 1)),
                        )
                    }
                    "up" => this.selected = Some(this.selected.map_or(0, |i| i.saturating_sub(1))),
                    "enter" => {
                        if let Some(entry) = this.selected.and_then(|i| this.entries.get(i)) {
                            if entry.is_directory {
                                this.enqueue(Job::List(entry.path.clone()), cx);
                            } else {
                                this.pick_download(window, cx);
                            }
                        }
                    }
                    _ => return,
                }
                if let Some(index) = this.selected {
                    this.list_scroll
                        .scroll_to_item(index, gpui::ScrollStrategy::Nearest);
                }
                cx.stop_propagation();
                cx.notify();
            }))
            .flex()
            .flex_col()
            .w_full()
            .h_full()
            .min_h_0()
            .min_w_0()
            .bg(background)
            .text_color(foreground)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .px_4()
                    .py_3()
                    .border_b_1()
                    .border_color(border)
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child(tr("file-title")),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(muted)
                                    .child(tr("file-connection-label")),
                            ),
                    )
                    .child(div().text_xs().text_color(muted).child(format!(
                        "{} {}",
                        self.entries.len(),
                        tr("file-items")
                    ))),
            )
            .child(
                div()
                    .p_3()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        Input::new(&self.path)
                            .small()
                            .aria_label(tr("file-path-placeholder")),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .flex()
                                    .gap_1()
                                    .child(
                                        Button::new("up")
                                            .small()
                                            .ghost()
                                            .label(tr("file-up"))
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                let path = this.path.read(cx).value();
                                                let parent = path
                                                    .trim_end_matches('/')
                                                    .rsplit_once('/')
                                                    .map(
                                                        |(p, _)| if p.is_empty() { "/" } else { p },
                                                    )
                                                    .unwrap_or("/")
                                                    .to_string();
                                                this.enqueue(Job::List(parent), cx);
                                            })),
                                    )
                                    .child(
                                        Button::new("refresh")
                                            .small()
                                            .ghost()
                                            .label(tr("file-refresh"))
                                            .on_click(
                                                cx.listener(|this, _, _, cx| this.refresh(cx)),
                                            ),
                                    ),
                            )
                            .child(
                                Button::new("upload")
                                    .small()
                                    .primary()
                                    .label(tr("file-upload"))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.pick_upload(window, cx)
                                    })),
                            ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .justify_between()
                    .px_4()
                    .py_2()
                    .text_xs()
                    .text_color(muted)
                    .border_y_1()
                    .border_color(border)
                    .child(
                        Button::new("focus-file-list")
                            .ghost()
                            .label(tr("file-name-column"))
                            .tooltip(tr("file-list-help"))
                            .accessibility_label(tr("file-list-help"))
                            .on_click(
                                cx.listener(|this, _, window, cx| {
                                    this.list_focus.focus(window, cx)
                                }),
                            ),
                    )
                    .child(tr("file-size-column")),
            );
        if self.entries.is_empty() {
            pane = pane.child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .justify_center()
                    .items_center()
                    .gap_2()
                    .p_4()
                    .text_color(muted)
                    .child(tr("file-empty-title"))
                    .child(div().text_xs().child(tr("file-empty-help"))),
            );
        } else {
            pane = pane.child(
                uniform_list(
                    "file-entries",
                    self.entries.len(),
                    cx.processor(|this, range: std::ops::Range<usize>, _, cx| {
                        let theme = cx.theme();
                        let (bg, hover, selected, fg, muted) = (
                            theme.sidebar,
                            theme.muted,
                            theme.accent,
                            theme.foreground,
                            theme.muted_foreground,
                        );
                        range
                            .map(|index| {
                                let entry = &this.entries[index];
                                let kind = if entry.is_directory {
                                    tr("file-type-folder")
                                } else if entry.is_symlink {
                                    tr("file-type-link")
                                } else {
                                    tr("file-type-file")
                                };
                                div()
                                    .id(index)
                                    .w_full()
                                    .h(px(48.))
                                    .px_4()
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .gap_3()
                                    .bg(if this.selected == Some(index) {
                                        selected
                                    } else {
                                        bg
                                    })
                                    .hover(move |s| s.bg(hover))
                                    .text_color(fg)
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .flex()
                                            .flex_col()
                                            .gap_1()
                                            .child(
                                                div()
                                                    .text_sm()
                                                    .truncate()
                                                    .child(entry.name.clone()),
                                            )
                                            .child(div().text_xs().text_color(muted).child(kind)),
                                    )
                                    .child(
                                        div()
                                            .w(px(72.))
                                            .flex_shrink_0()
                                            .text_xs()
                                            .text_color(muted)
                                            .child(
                                                entry
                                                    .size
                                                    .filter(|_| !entry.is_directory)
                                                    .map(format_bytes)
                                                    .unwrap_or_else(|| "—".into()),
                                            ),
                                    )
                                    .on_click(cx.listener(
                                        move |this, event: &gpui::ClickEvent, window, cx| {
                                            this.list_focus.focus(window, cx);
                                            this.selected = Some(index);
                                            if event.click_count() == 2
                                                && let Some(entry) = this.entries.get(index)
                                                && entry.is_directory
                                            {
                                                this.enqueue(Job::List(entry.path.clone()), cx);
                                            }
                                            cx.notify();
                                        },
                                    ))
                            })
                            .collect()
                    }),
                )
                .track_scroll(&self.list_scroll)
                .flex_1()
                .min_h_0(),
            );
        }
        pane = pane.child(
            div()
                .p_3()
                .flex()
                .flex_col()
                .gap_2()
                .border_t_1()
                .border_color(border)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            Button::new("download")
                                .small()
                                .label(tr("file-download"))
                                .disabled(self.selected.is_none())
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.pick_download(window, cx)
                                })),
                        )
                        .child(
                            Button::new("delete")
                                .small()
                                .ghost()
                                .label(tr("file-delete"))
                                .disabled(self.selected.is_none())
                                .on_click(cx.listener(|this, _, _, cx| {
                                    if let Some(entry) =
                                        this.selected.and_then(|index| this.entries.get(index))
                                    {
                                        if entry.is_directory {
                                            this.message = tr("file-directory-delete-unavailable");
                                        } else {
                                            this.pending_delete = Some(entry.path.clone());
                                        }
                                    }
                                    cx.notify();
                                })),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            Input::new(&self.folder)
                                .small()
                                .aria_label(tr("file-folder-placeholder")),
                        )
                        .child(
                            Button::new("mkdir")
                                .small()
                                .label(tr("file-create-folder"))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    match opsssh_drop::join_remote(
                                        &this.path.read(cx).value(),
                                        &this.folder.read(cx).value(),
                                    ) {
                                        Ok(path) => this.enqueue(Job::Mkdir(path), cx),
                                        Err(error) => {
                                            this.message = error.to_string();
                                            cx.notify();
                                        }
                                    }
                                })),
                        ),
                ),
        );
        if let Some(path) = self.pending_delete.clone() {
            pane = pane.child(
                div()
                    .mx_3()
                    .mb_3()
                    .p_3()
                    .rounded_md()
                    .border_1()
                    .border_color(cx.theme().danger)
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(tr("file-delete-confirm"))
                    .child(div().text_xs().child(path))
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .child(
                                Button::new("confirm-delete")
                                    .small()
                                    .danger()
                                    .label(tr("file-delete-permanently"))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        if let Some(path) = this.pending_delete.take() {
                                            this.enqueue(Job::Delete(path), cx);
                                        }
                                    })),
                            )
                            .child(
                                Button::new("keep-file")
                                    .small()
                                    .label(tr("file-keep-file"))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.pending_delete = None;
                                        cx.notify();
                                    })),
                            ),
                    ),
            );
        }
        let mut tasks = div()
            .id("transfer-tasks")
            .max_h(px(220.))
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_2()
            .p_3()
            .border_t_1()
            .border_color(border)
            .bg(subtle)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(tr("file-transfers")),
                    )
                    .child(
                        Button::new("clear-transfers")
                            .xsmall()
                            .ghost()
                            .label(tr("file-clear-finished"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.transfers.retain(|task| task.state.active());
                                cx.notify();
                            })),
                    ),
            );
        if self.transfers.is_empty() {
            tasks = tasks.child(
                div()
                    .text_xs()
                    .text_color(muted)
                    .child(tr("file-no-transfers")),
            );
        }
        for task in self.transfers.iter().rev() {
            let id = task.id;
            let active = task.state.active();
            let mut card = div()
                .p_2()
                .rounded_md()
                .bg(background)
                .border_1()
                .border_color(border)
                .flex()
                .flex_col()
                .gap_2()
                .child(div().text_xs().overflow_hidden().child(task.label.clone()))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(
                            div()
                                .text_xs()
                                .text_color(if active { foreground } else { muted })
                                .child(task.state.label()),
                        )
                        .child(div().text_xs().text_color(muted).child(format!(
                            "{}: {}",
                            tr("file-current-file"),
                            format_bytes(task.bytes)
                        ))),
                );
            if active {
                card = card.child(
                    Button::new(("cancel-transfer", id))
                        .xsmall()
                        .ghost()
                        .label(tr("file-cancel-transfer"))
                        .on_click(cx.listener(move |this, _, _, cx| this.cancel_transfer(id, cx))),
                );
            }
            tasks = tasks.child(card);
        }

        pane.child(tasks).child(
            div()
                .px_3()
                .py_2()
                .text_xs()
                .text_color(muted)
                .border_t_1()
                .border_color(border)
                .child(self.message.clone()),
        )
    }
}
fn format_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KiB", bytes as f64 / 1024.)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:.1} MiB", bytes as f64 / (1024. * 1024.))
    } else {
        format!("{:.1} GiB", bytes as f64 / (1024. * 1024. * 1024.))
    }
}
async fn worker(
    commands: Sender<SshCommand>,
    fingerprint: String,
    jobs: Receiver<QueuedJob>,
    updates: Sender<Update>,
) -> opsssh_sftp::Result<()> {
    let (reply, receive) = async_channel::bounded(1);
    commands.send(SshCommand::OpenSftp { reply }).await?;
    let stream = tokio::time::timeout(Duration::from_secs(20), receive.recv())
        .await??
        .map_err(std::io::Error::other)?;
    let client = SftpClient::connect(stream, fingerprint).await?;
    let home = client.canonicalize(".").await?;
    while let Ok(QueuedJob { id, job }) = jobs.recv().await {
        if updates.is_closed() || jobs.is_closed() {
            break;
        }
        let _ = updates.send(Update::Started(id)).await;
        let result = execute(&client, &home, job, &updates).await;
        let success = result.is_ok();
        if let Err(error) = result {
            let _ = updates.send(Update::Message(error.to_string())).await;
        }
        let _ = updates.send(Update::Finished(id, success)).await;
    }
    Ok(())
}
async fn execute(
    client: &SftpClient,
    home: &str,
    job: Job,
    updates: &Sender<Update>,
) -> opsssh_sftp::Result<()> {
    match job {
        Job::List(path) => {
            let path = client.canonicalize(&path).await?;
            let entries = client.list(&path).await?;
            updates.send(Update::Listed(path, entries)).await?;
        }
        Job::Upload {
            paths,
            target,
            insert,
            control,
        } => {
            let directory = match target {
                Some(path) => client.canonicalize(&path).await?,
                None => client.private_staging(home).await?,
            };
            let mut inserted = vec![];
            for path in paths {
                let name = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .ok_or_else(|| tr("file-invalid-name"))?;
                let remote = opsssh_drop::join_remote(&directory, name)?;
                client.upload_tree(&path, &remote, &control).await?;
                inserted.push(opsssh_drop::quote(&remote, opsssh_drop::Shell::Posix)?);
            }
            if insert {
                updates
                    .send(Update::Insert(format!("{} ", inserted.join(" "))))
                    .await?;
            }
            updates
                .send(Update::Message(format!(
                    "{}: {directory}",
                    tr("file-upload-complete")
                )))
                .await?;
        }
        Job::Download {
            remote,
            local,
            control,
        } => {
            let bytes = client
                .download(&remote, &local, WriteMode::CreateNew, &control)
                .await?;
            updates
                .send(Update::Message(format!(
                    "{}: {bytes} B",
                    tr("file-download-complete")
                )))
                .await?;
        }
        Job::Attachment {
            bytes,
            name,
            control,
        } => {
            let directory = client.private_staging(home).await?;
            let path = opsssh_drop::join_remote(&directory, &name)?;
            client.upload_bytes(&bytes, &path, &control).await?;
            updates
                .send(Update::Insert(format!(
                    "{} ",
                    opsssh_drop::quote(&path, opsssh_drop::Shell::Posix)?
                )))
                .await?;
            updates
                .send(Update::Message(format!(
                    "{}: {path}",
                    tr("file-attachment-complete")
                )))
                .await?;
        }
        Job::Mkdir(path) => {
            client.create_directory(&path).await?;
            updates
                .send(Update::Message(tr("file-folder-created")))
                .await?;
        }
        Job::Delete(path) => {
            client.remove_file_confirmed(&path).await?;
            updates.send(Update::Message(tr("file-deleted"))).await?;
        }
    }
    Ok(())
}
