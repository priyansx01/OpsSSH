use std::time::Instant;

use gpui::{
    App, Bounds, Context, Entity, FocusHandle, KeyBinding, Render, Window, WindowBounds,
    WindowOptions, actions, div, prelude::*, px, rgb, size,
};
use opsssh_term_view::TerminalView;

actions!(opsssh, [OpenLocalTerminal, GoHome, Quit]);

struct Workspace {
    home_focus: FocusHandle,
    terminal: Option<Entity<TerminalView>>,
    launched: Instant,
    first_frame_recorded: bool,
}

pub fn run(open_terminal: bool) {
    run_internal(open_terminal, None);
}

#[cfg(feature = "capture")]
pub fn capture(open_terminal: bool, path: std::path::PathBuf) {
    run_internal(open_terminal, Some(path));
}

fn run_internal(open_terminal: bool, capture_path: Option<std::path::PathBuf>) {
    let launched = Instant::now();
    opsssh_platform::application().run(move |cx: &mut App| {
        cx.bind_keys([
            KeyBinding::new("ctrl-enter", OpenLocalTerminal, Some("OpsSSHHome")),
            KeyBinding::new("cmd-enter", OpenLocalTerminal, Some("OpsSSHHome")),
            KeyBinding::new("ctrl-shift-h", GoHome, Some("OpsSSH")),
            KeyBinding::new("cmd-shift-h", GoHome, Some("OpsSSH")),
            KeyBinding::new("ctrl-shift-q", Quit, Some("OpsSSH")),
            KeyBinding::new("cmd-q", Quit, Some("OpsSSH")),
        ]);
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        let bounds = Bounds::centered(None, size(px(1120.), px(760.)), cx);
        let result = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..WindowOptions::default()
            },
            move |window, cx| {
                window.set_window_title("OpsSSH");
                cx.new(|cx| {
                    let home_focus = cx.focus_handle();
                    home_focus.focus(window, cx);
                    let terminal =
                        open_terminal.then(|| cx.new(|cx| TerminalView::new(window, cx)));
                    #[cfg(feature = "capture")]
                    if let Some(path) = capture_path {
                        let timer = cx
                            .background_executor()
                            .timer(std::time::Duration::from_secs(2));
                        cx.spawn_in(window, async move |this, cx| {
                            timer.await;
                            let _ = this.update_in(cx, |_, window, cx| {
                                let result = window
                                    .render_to_image()
                                    .and_then(|image| image.save(&path).map_err(Into::into));
                                match result {
                                    Ok(()) => {
                                        eprintln!("Saved rendered frame to {}", path.display())
                                    }
                                    Err(error) => eprintln!("Frame export failed: {error}"),
                                }
                                cx.quit();
                            });
                        })
                        .detach();
                    }
                    #[cfg(not(feature = "capture"))]
                    let _ = capture_path;
                    Workspace {
                        home_focus,
                        terminal,
                        launched,
                        first_frame_recorded: false,
                    }
                })
            },
        );
        if let Err(error) = result {
            eprintln!("Could not open OpsSSH: {error}");
            cx.quit();
            return;
        }
        cx.activate(true);
    });
}

impl Workspace {
    fn open_local(&mut self, _: &OpenLocalTerminal, window: &mut Window, cx: &mut Context<Self>) {
        self.terminal = Some(cx.new(|cx| TerminalView::new(window, cx)));
        cx.notify();
    }

    fn home(&mut self, _: &GoHome, window: &mut Window, cx: &mut Context<Self>) {
        self.terminal = None;
        self.home_focus.focus(window, cx);
        cx.notify();
    }

    fn quit(&mut self, _: &Quit, _: &mut Window, cx: &mut Context<Self>) {
        cx.quit();
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.first_frame_recorded {
            self.first_frame_recorded = true;
            let launched = self.launched;
            // This is the first layout frame, not measured GPU presentation.
            window.on_next_frame(move |_, _| {
                eprintln!(
                    "OpsSSH first-frame callback: {:.2} ms (presentation unverified)",
                    launched.elapsed().as_secs_f64() * 1000.
                );
            });
        }
        let root = div()
            .id("opsssh-workspace")
            .key_context("OpsSSH")
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(0x10141b))
            .text_color(rgb(0xe7ecf4))
            .on_action(cx.listener(Self::open_local))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::quit));
        if let Some(terminal) = &self.terminal {
            return root
                .child(
                    div()
                        .h(px(52.))
                        .px_5()
                        .flex()
                        .items_center()
                        .justify_between()
                        .border_b_1()
                        .border_color(rgb(0x29303c))
                        .child(
                            div()
                                .flex()
                                .gap_3()
                                .child("OpsSSH")
                                .child(div().text_color(rgb(0x8ebbc0)).child("/ Local terminal")),
                        )
                        .child(
                            div()
                                .id("home-button")
                                .px_3()
                                .py_1()
                                .rounded_md()
                                .bg(rgb(0x202936))
                                .cursor_pointer()
                                .child("Home")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.home(&GoHome, window, cx)
                                })),
                        ),
                )
                .child(div().flex_1().min_h_0().child(terminal.clone()));
        }
        root.track_focus(&self.home_focus).key_context("OpsSSH OpsSSHHome")
            .child(div().flex_1().min_h_0().flex()
                .child(div().w(px(224.)).p_5().flex().flex_col().gap_6().bg(rgb(0x151a23)).border_r_1().border_color(rgb(0x29303c))
                    .child(div().text_xl().font_weight(gpui::FontWeight::BOLD).child("OpsSSH"))
                    .child(div().text_xs().text_color(rgb(0x78869b)).child("YOUR WORKSPACE"))
                    .child(div().px_3().py_2().rounded_md().bg(rgb(0x25343a)).text_color(rgb(0xa9e3d6)).child("Servers"))
                    .child(div().text_sm().text_color(rgb(0x93a1b7)).child("One place for your remote work."))
                    .child(div().flex_1())
                    .child(div().text_xs().text_color(rgb(0x78869b)).child("Local terminal preview")))
                .child(div().id("home-content").flex_1().min_w_0().overflow_y_scroll().p_8().flex().flex_col().gap_4()
                    .child(div().flex().justify_between().items_center()
                        .child(div().flex().flex_col().gap_2()
                            .child(div().text_xs().text_color(rgb(0x8abdaf)).child("GET STARTED"))
                            .child(div().text_3xl().font_weight(gpui::FontWeight::BOLD).child("Your servers, within reach."))
                            .child(div().text_sm().text_color(rgb(0x94a2b7)).child("Start with a terminal on your computer."))))
                    .child(div().p_6().rounded_lg().bg(rgb(0x1b2530)).border_1().border_color(rgb(0x344857)).flex().flex_col().gap_4()
                        .child(div().text_lg().child("Local terminal"))
                        .child(div().text_sm().text_color(rgb(0xa7b4c6)).child("Open your shell with GPU-rendered text, selection, and clipboard support."))
                        .child(div().flex().items_center().gap_4()
                            .child(div().id("open-local").px_4().py_2().rounded_md().bg(rgb(0xa9e3d6)).text_color(rgb(0x122b28)).cursor_pointer().child("Open local terminal")
                                .on_click(cx.listener(|this, _, window, cx| this.open_local(&OpenLocalTerminal, window, cx))))
                            .child(div().text_sm().text_color(rgb(0x8b9caf)).child(format!("{} + Enter", opsssh_platform::info().primary_modifier)))))
                    .child(div().flex().justify_between().items_center().child(div().text_lg().child("Server list preview")).child(div().text_xs().text_color(rgb(0x78869b)).child("SAMPLE DATA")))
                    .children([("Development", "demo@dev.example:22", "DEVELOPMENT", 0x8abdaf), ("Staging", "demo@staging.example:22", "STAGING", 0xdbb477), ("Production", "demo@prod.example:22", "PRODUCTION", 0xd98b91)].into_iter().map(|(name, address, environment, color)| {
                        div().p_4().rounded_md().border_1().border_color(rgb(0x2c3441)).flex().justify_between().items_center()
                            .child(div().flex().flex_col().gap_1().child(name).child(div().text_sm().text_color(rgb(0x8797ad)).child(address)))
                            .child(div().text_xs().text_color(rgb(color)).child(environment))
                    }))
                    .child(div().text_sm().text_color(rgb(0x78869b)).child("SSH connections will be available in a later preview."))))
    }
}
