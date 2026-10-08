use opsssh_i18n::text as tr;
use std::ops::Range;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use gpui::{
    App, Bounds, ClipboardEntry, ClipboardItem, Context, ElementInputHandler, EntityInputHandler,
    EventEmitter, ExternalPaths, FocusHandle, Focusable, KeyDownEvent, KeyUpEvent, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, Render, ScrollDelta,
    ScrollWheelEvent, ShapedLine, Subscription, Task, TextRun, UTF16Selection, UnderlineStyle,
    Window, canvas, div, fill, font, point, prelude::*, px, rgb, rgba, size,
};
use gpui_component::{
    ActiveTheme, Disableable, Sizable,
    button::{Button, ButtonVariants},
    checkbox::Checkbox,
    input::{Input, InputState},
    menu::{ContextMenuExt, PopupMenuItem},
};
use opsssh_platform::{LocalShellOptions, terminal_font_family};
use opsssh_ssh_core::{ConnectionOptions, PromptKind, SecretString, SshCommand, SshEvent};
use opsssh_term_core::{
    Key, KeyEventKind, Modifiers, Position, Rgb, Row, Selection, SessionState, TerminalEvent,
    TerminalSize, TerminalSnapshot, encode_focus, encode_key, encode_paste, encode_text,
};

use crate::composition::Composition;
use crate::{Session, SessionNotice};

/// User initiated file/image upload requests. The workspace owns transfer policy.
#[derive(Clone, Debug)]
pub enum TerminalViewEvent {
    UploadFiles(Vec<PathBuf>),
    UploadClipboard(ClipboardItem),
}

impl EventEmitter<TerminalViewEvent> for TerminalView {}

enum ConnectionPrompt {
    Host {
        id: u64,
        host: String,
        fingerprint: String,
    },
    Authentication {
        id: u64,
        name: String,
        instructions: String,
        fields: Vec<(String, gpui::Entity<InputState>)>,
        save: bool,
        can_save: bool,
    },
}

#[derive(Debug)]
struct ShapedRow {
    source: Arc<Row>,
    line: ShapedLine,
    backgrounds: Vec<(Range<usize>, Rgb)>,
}

/// GPU-rendered terminal. Background workers own PTY and SSH I/O;
/// GPUI's display callback coalesces snapshots at the native frame rate.
pub struct TerminalView {
    focus: FocusHandle,
    connection_options: Option<ConnectionOptions>,
    stopped: bool,
    tmux_missing: bool,
    notice_task: Option<Task<()>>,
    session: Option<Session>,
    snapshot: TerminalSnapshot,
    shapes: Vec<ShapedRow>,
    bounds: Bounds<Pixels>,
    cell_width: Pixels,
    cell_height: Pixels,
    font_size: f32,
    dimensions: TerminalSize,
    selection: Option<Selection>,
    anchor: Option<(Position, bool)>,
    composition: Composition,
    prompt: Option<ConnectionPrompt>,
    current_directory: Option<String>,
    verified_host_key: Option<String>,
    authentication: Option<opsssh_ssh_core::AuthenticationInfo>,
    connection_generation: u64,
    at_prompt: bool,
    paste_preview: Option<String>,
    #[cfg(feature = "test-support")]
    sent_input: Vec<Vec<u8>>,
    error: Option<String>,
    title: String,
    frame_pending: bool,
    sync_task: Option<(Instant, Task<()>)>,
    blink_task: Option<Task<()>>,
    cursor_on: bool,
    subscriptions: Vec<Subscription>,
}

impl std::fmt::Debug for TerminalView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TerminalView")
            .field("dimensions", &self.dimensions)
            .finish_non_exhaustive()
    }
}

impl TerminalView {
    /// Keep the toolkit's root focus/clipboard bindings off terminal key events.
    /// Register after toolkit initialization, once per application.
    pub fn bind_keys(cx: &mut App) {
        cx.bind_keys(
            ["tab", "shift-tab", "ctrl-c", "cmd-c"]
                .map(|key| gpui::KeyBinding::new(key, gpui::NoAction {}, Some("OpsSSHTerminal"))),
        );
    }
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self::start(None, window, cx)
    }

    /// Development fixture with a real local PTY, without asynchronous startup races.
    #[cfg(feature = "test-support")]
    pub fn preview_connected_local(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut view = Self::connect(ConnectionOptions::new("", "", PathBuf::new()), window, cx);
        view.notice_task = None;
        view.connection_options = None;
        view.session =
            Some(Session::local(LocalShellOptions::default()).expect("local preview PTY"));
        view.error = None;
        view
    }

    pub fn connect(
        options: ConnectionOptions,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::start(Some(options), window, cx)
    }

    pub fn ssh_commands(&self) -> Option<async_channel::Sender<SshCommand>> {
        self.session.as_ref().and_then(Session::ssh_commands)
    }

    /// Stop transport recovery without destroying the retained terminal buffer.
    pub fn disconnect(&mut self, cx: &mut Context<Self>) {
        if let Some(commands) = self.ssh_commands() {
            let _ = commands.try_send(SshCommand::Disconnect);
        }
        self.notice_task = None;
        self.session = None;
        self.stopped = true;
        self.prompt = None;
        self.blink_task = None;
        self.sync_task = None;
        self.error = None;
        self.authentication = None;
        self.connection_generation = self.connection_generation.wrapping_add(1);
        cx.notify();
    }
    pub fn is_tmux_protected(&self) -> bool {
        self.connection_options
            .as_ref()
            .is_some_and(|o| o.tmux.is_some())
    }
    fn connect_without_protection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.tmux_missing {
            return;
        }
        let Some(mut options) = self.connection_options.clone() else {
            return;
        };
        options.tmux = None;
        let generation = self.connection_generation.wrapping_add(1);
        let font_size = self.font_size;
        let mut replacement = Self::start(Some(options), window, cx);
        replacement.connection_generation = generation;
        replacement.set_font_size(font_size, cx);
        *self = replacement;
        cx.notify();
    }

    pub fn current_directory(&self) -> Option<&str> {
        self.current_directory.as_deref()
    }

    pub fn verified_host_key(&self) -> Option<&str> {
        self.verified_host_key.as_deref()
    }

    /// Successful target authentication, cleared when the transport is disconnected.
    pub fn authentication(&self) -> Option<&opsssh_ssh_core::AuthenticationInfo> {
        self.authentication.as_ref()
    }

    /// Changes when an SSH transport begins reconnecting, invalidating remote work.
    pub fn connection_generation(&self) -> u64 {
        self.connection_generation
    }
    /// A concise status label suitable for workspace session tabs.
    pub fn connection_status(&self) -> String {
        tr(match &self.prompt {
            Some(ConnectionPrompt::Host { .. }) => "term-status-verify",
            Some(ConnectionPrompt::Authentication { .. }) => "term-status-auth",
            None => match self.state() {
                SessionState::Connected if self.error.is_none() => "term-status-connected",
                SessionState::Reconnecting => "term-status-reconnecting",
                _ if self.error.is_some() => "term-status-error",
                SessionState::Connecting => "term-status-connecting",
                SessionState::Authenticating => "term-status-signing-in",
                SessionState::Connected => "term-status-connected",
                SessionState::Disconnected => "term-status-disconnected",
                SessionState::NeedsUserAction => "term-status-action",
                SessionState::Closed => "term-status-ended",
            },
        })
    }

    pub fn session_state(&self) -> SessionState {
        self.state()
    }

    pub fn at_shell_prompt(&self) -> bool {
        self.at_prompt
    }

    pub fn insert_text(&mut self, text: &str, cx: &mut Context<Self>) {
        self.paste_text(text, cx);
    }

    /// A completed upload inserts a literal path as one paste transaction, without Enter.
    pub fn insert_uploaded_path(&mut self, text: &str, cx: &mut Context<Self>) -> bool {
        if !self.accepts_terminal_input() || text.contains(['\n', '\r']) {
            return false;
        }
        let Some(session) = &self.session else {
            return false;
        };
        let Ok(bytes) = encode_paste(session.state(), session.modes(), text) else {
            return false;
        };
        self.selection = None;
        self.anchor = None;
        session.scroll_to_bottom();
        let sent = self.send_checked(bytes, cx);
        cx.notify();
        sent
    }

    #[cfg(feature = "test-support")]
    pub fn preview_sent_input(&self) -> Vec<u8> {
        self.sent_input.concat()
    }

    pub fn accepts_terminal_input(&self) -> bool {
        self.state().accepts_input() && self.prompt.is_none() && self.paste_preview.is_none()
    }

    pub fn accepts_terminal_keys(&self, window: &Window) -> bool {
        window.is_window_active() && self.focus.is_focused(window) && self.accepts_terminal_input()
    }

    /// Native system keys are already suppressed by the platform hook, so use the
    /// encoder directly rather than re-injecting them into the Windows input queue.
    pub fn captured_key(
        &mut self,
        event: opsssh_platform::keyboard_capture::CapturedKey,
        cx: &mut Context<Self>,
    ) {
        if self.prompt.is_some()
            || self.paste_preview.is_some()
            || self.connection_generation != event.generation
        {
            return;
        }
        if let Some(session) = &self.session
            && let Ok(bytes) = encode_key(
                session.state(),
                session.modes(),
                event.key,
                event.modifiers,
                event.kind,
            )
        {
            if event.kind != KeyEventKind::Release {
                self.selection = None;
                session.scroll_to_bottom();
            }
            self.send(bytes, cx);
            cx.notify();
        }
    }

    pub fn set_font_size(&mut self, font_size: f32, cx: &mut Context<Self>) {
        let font_size = font_size.clamp(8.0, 48.0);
        if self.font_size != font_size {
            self.font_size = font_size;
            self.cell_height = px(font_size * 1.375);
            self.shapes.clear();
            cx.notify();
        }
    }

    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        if let Some(ConnectionPrompt::Authentication { fields, .. }) = &self.prompt
            && let Some((_, input)) = fields.first()
        {
            if !fields
                .iter()
                .any(|(_, input)| input.read(cx).focus_handle(cx).is_focused(window))
            {
                input.update(cx, |input, cx| input.focus(window, cx));
            }
            return;
        }
        self.focus.focus(window, cx);
    }

    /// Open the real context menu for development-only native render captures.
    #[cfg(feature = "test-support")]
    pub fn preview_clipboard_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(row) = self.snapshot.rows.first()
            && !row.cells.is_empty()
        {
            self.selection = Some(Selection::Linear {
                start: Position { row: 0, column: 0 },
                end: Position {
                    row: 0,
                    column: 9.min(row.cells.len() - 1),
                },
            });
        }
        let position = self.bounds.origin + point(px(120.), px(60.));
        window.defer(cx, move |window, cx| {
            window.dispatch_event(
                gpui::PlatformInput::MouseDown(MouseDownEvent {
                    button: MouseButton::Right,
                    position,
                    modifiers: Default::default(),
                    click_count: 1,
                    first_mouse: false,
                }),
                cx,
            );
        });
        cx.notify();
    }

    fn start(
        remote: Option<ConnectionOptions>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        let subscriptions = vec![
            cx.on_focus(&focus, window, |this, window, cx| {
                this.focus_changed(true, window, cx);
            }),
            cx.on_blur(&focus, window, |this, window, cx| {
                this.focus_changed(false, window, cx);
            }),
            cx.observe_window_activation(window, |this, window, cx| this.refresh_blink(window, cx)),
        ];
        focus.focus(window, cx);
        let options = LocalShellOptions::default();
        let dimensions = options.size;
        let connection_options = remote.clone();
        let startup = cx.background_executor().spawn(async move {
            match remote {
                Some(remote) => Session::remote(remote, dimensions),
                None => Session::local(options),
            }
        });
        let notice_task = cx.spawn_in(window, async move |this, cx| {
            let result = startup.await;
            let notices = this
                .update_in(cx, |view, window, cx| match result {
                    Ok(session) => {
                        let notices = session.notices();
                        view.session = Some(session);
                        view.refresh_blink(window, cx);
                        view.queue_frame(window, cx);
                        Some(notices)
                    }
                    Err(error) => {
                        view.error = Some(error.to_string());
                        cx.notify();
                        None
                    }
                })
                .ok()
                .flatten();
            if let Some(notices) = notices {
                while let Ok(notice) = notices.recv().await {
                    if this
                        .update_in(cx, |view, window, cx| {
                            view.notice(notice, window, cx);
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            }
        });
        Self {
            focus,
            connection_options,
            stopped: false,
            tmux_missing: false,
            notice_task: Some(notice_task),
            session: None,
            snapshot: TerminalSnapshot::default(),
            shapes: Vec::new(),
            bounds: Bounds::default(),
            cell_width: px(9.6),
            cell_height: px(22.),
            font_size: 16.0,
            dimensions,
            selection: None,
            anchor: None,
            composition: Composition::default(),
            prompt: None,
            current_directory: None,
            verified_host_key: None,
            authentication: None,
            connection_generation: 0,
            at_prompt: false,
            paste_preview: None,
            #[cfg(feature = "test-support")]
            sent_input: Vec::new(),
            error: None,
            title: "Local terminal".into(),
            frame_pending: false,
            sync_task: None,
            blink_task: None,
            cursor_on: true,
            subscriptions,
        }
    }

    fn notice(&mut self, notice: SessionNotice, window: &mut Window, cx: &mut Context<Self>) {
        match notice {
            SessionNotice::Event(event) => self.event(event, window, cx),
            SessionNotice::Error(error) => self.error = Some(error),
            SessionNotice::Ssh(event) => self.ssh_event(event, window, cx),
            _ => {}
        }
        self.refresh_blink(window, cx);
        if let Some(deadline) = self.session.as_ref().and_then(Session::sync_deadline) {
            self.arm_sync(deadline, window, cx);
        } else {
            self.sync_task = None;
            self.queue_frame(window, cx);
        }
    }

    fn ssh_event(&mut self, event: SshEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            SshEvent::Authenticated(info) => self.authentication = Some(info),
            SshEvent::Connected => {
                self.error = None;
                self.prompt = None;
                if let Some(session) = &self.session {
                    let _ = session.resize(self.dimensions);
                }
                if self.focus.is_focused(window) {
                    self.focus.focus(window, cx);
                }
            }
            SshEvent::VerifiedHost { fingerprint, .. } => {
                self.verified_host_key = Some(fingerprint)
            }
            SshEvent::Reconnecting { attempt, delay } => {
                self.authentication = None;
                self.connection_generation = self.connection_generation.wrapping_add(1);
                self.error = Some(format!(
                    "Connection lost. Reconnecting (attempt {attempt}) in {} seconds",
                    delay.as_secs()
                ));
                self.prompt = None;
            }
            SshEvent::Error(error) => self.error = Some(error),
            SshEvent::Closed {
                exit_status: Some(127),
            } if self.is_tmux_protected() => {
                self.tmux_missing = true;
                self.error = Some(tr("term-tmux-missing"));
            }
            SshEvent::HostKeyPrompt {
                id,
                host,
                fingerprint,
            } => {
                self.prompt = Some(ConnectionPrompt::Host {
                    id,
                    host,
                    fingerprint,
                })
            }
            SshEvent::AuthenticationPrompt {
                id,
                name,
                instructions,
                prompts,
                kind,
            } => {
                let fields = prompts
                    .into_iter()
                    .map(|field| {
                        let input = cx.new(|cx| InputState::new(window, cx).masked(!field.echo));
                        (field.prompt, input)
                    })
                    .collect::<Vec<_>>();
                if self.focus.is_focused(window)
                    && let Some((_, input)) = fields.first()
                {
                    input.update(cx, |input, cx| input.focus(window, cx));
                }
                self.prompt = Some(ConnectionPrompt::Authentication {
                    id,
                    name,
                    instructions,
                    fields,
                    save: false,
                    can_save: kind != PromptKind::KeyboardInteractive,
                });
            }
            _ => {}
        }
        cx.notify();
    }

    fn ssh_command(&mut self, command: SshCommand, cx: &mut Context<Self>) {
        if let Some(commands) = self.ssh_commands()
            && let Err(error) = commands.try_send(command)
        {
            self.error = Some(error.to_string());
        }
        cx.notify();
    }

    fn trust_host(&mut self, persist: bool, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ConnectionPrompt::Host { id, .. }) = self.prompt.take() {
            self.ssh_command(
                SshCommand::TrustHost {
                    id,
                    trust: true,
                    persist,
                },
                cx,
            );
            self.focus.focus(window, cx);
        }
    }

    fn submit_auth(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ConnectionPrompt::Authentication {
            id, fields, save, ..
        }) = self.prompt.take()
        {
            let responses = fields
                .into_iter()
                .map(|(_, input)| {
                    let secret = SecretString::new(input.read(cx).value().to_string());
                    input.update(cx, |input, cx| input.set_value("", window, cx));
                    secret
                })
                .collect();
            self.ssh_command(
                SshCommand::AuthResponse {
                    id,
                    responses,
                    save,
                },
                cx,
            );
            self.focus.focus(window, cx);
        }
    }

    fn cancel_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.prompt = None;
        self.ssh_command(SshCommand::Disconnect, cx);
        self.focus.focus(window, cx);
    }

    fn prompt_panel(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let colors = cx.theme().semantic_tokens().colors;
        let warning = cx.theme().warning;
        let panel = div()
            .id("connection-prompt-panel")
            .max_h(px(420.))
            .overflow_y_scroll()
            .px_5()
            .py_4()
            .border_t_1()
            .border_color(colors.border)
            .bg(colors.surface)
            .text_color(colors.surface_foreground)
            .flex()
            .flex_col()
            .gap_3()
            .flex_shrink_0();
        match &self.prompt {
            Some(ConnectionPrompt::Host {
                host, fingerprint, ..
            }) => panel
                .child(
                    div()
                        .text_xs()
                        .text_color(warning)
                        .child(tr("term-server-identity")),
                )
                .child(
                    div()
                        .text_lg()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(tr("term-verify-server")),
                )
                .child(div().text_sm().child(format!("First connection to {host}")))
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap_3()
                        .p_3()
                        .rounded_md()
                        .border_1()
                        .border_color(colors.border)
                        .bg(colors.background)
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(colors.muted_foreground)
                                        .child(tr("term-fingerprint")),
                                )
                                .child(
                                    div()
                                        .font_family(terminal_font_family())
                                        .text_sm()
                                        .child(fingerprint.clone()),
                                ),
                        )
                        .child(
                            Button::new("copy-fingerprint")
                                .small()
                                .outline()
                                .label(tr("term-copy-fingerprint"))
                                .on_click(cx.listener(|view, _, _, cx| {
                                    if let Some(ConnectionPrompt::Host { fingerprint, .. }) =
                                        &view.prompt
                                    {
                                        cx.write_to_clipboard(ClipboardItem::new_string(
                                            fingerprint.clone(),
                                        ));
                                    }
                                })),
                        ),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(colors.muted_foreground)
                        .child(tr("term-trust-help")),
                )
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap_2()
                        .child(
                            Button::new("trust-save")
                                .primary()
                                .label(tr("term-trust-save"))
                                .on_click(cx.listener(|view, _, window, cx| {
                                    view.trust_host(true, window, cx)
                                })),
                        )
                        .child(
                            Button::new("trust-once")
                                .outline()
                                .label(tr("term-trust-once"))
                                .on_click(cx.listener(|view, _, window, cx| {
                                    view.trust_host(false, window, cx)
                                })),
                        )
                        .child(
                            Button::new("cancel-trust")
                                .ghost()
                                .label(tr("term-cancel-connection"))
                                .on_click(cx.listener(|view, _, window, cx| {
                                    view.cancel_prompt(window, cx)
                                })),
                        ),
                )
                .into_any_element(),
            Some(ConnectionPrompt::Authentication {
                name,
                instructions,
                fields,
                save,
                can_save,
                ..
            }) => panel
                .child(
                    div()
                        .text_xs()
                        .text_color(colors.muted_foreground)
                        .child(tr("term-authentication")),
                )
                .child(
                    div()
                        .text_lg()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(if name.is_empty() {
                            tr("term-sign-in-title")
                        } else {
                            name.clone()
                        }),
                )
                .when(!instructions.is_empty(), |panel| {
                    panel.child(
                        div()
                            .text_sm()
                            .text_color(colors.muted_foreground)
                            .child(instructions.clone()),
                    )
                })
                .children(fields.iter().map(|(label, input)| {
                    div()
                        .w_full()
                        .max_w(px(640.))
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(div().text_sm().child(label.clone()))
                        .child(Input::new(input).aria_label(label.clone()))
                }))
                .when(*can_save, |panel| {
                    panel.child(
                        Checkbox::new("save-credential")
                            .checked(*save)
                            .label(tr("term-save-credential"))
                            .on_click(cx.listener(|view, checked, _, cx| {
                                if let Some(ConnectionPrompt::Authentication { save, .. }) =
                                    &mut view.prompt
                                {
                                    *save = *checked;
                                    cx.notify();
                                }
                            })),
                    )
                })
                .when(!*can_save, |panel| {
                    panel.child(
                        div()
                            .text_xs()
                            .text_color(colors.muted_foreground)
                            .child(tr("term-otp-help")),
                    )
                })
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap_2()
                        .child(
                            Button::new("submit-auth")
                                .primary()
                                .label(tr("term-continue"))
                                .on_click(
                                    cx.listener(|view, _, window, cx| view.submit_auth(window, cx)),
                                ),
                        )
                        .child(
                            Button::new("cancel-auth")
                                .ghost()
                                .label(tr("term-cancel-connection"))
                                .on_click(cx.listener(|view, _, window, cx| {
                                    view.cancel_prompt(window, cx)
                                })),
                        ),
                )
                .into_any_element(),
            None => div().into_any_element(),
        }
    }

    fn paste_panel(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let colors = cx.theme().semantic_tokens().colors;
        let text = self.paste_preview.as_deref().unwrap_or_default();
        let preview = text.chars().take(500).collect::<String>();
        let truncated = text.chars().count() > 500;
        div()
            .px_5()
            .py_4()
            .border_t_1()
            .border_color(colors.border)
            .bg(colors.surface)
            .text_color(colors.surface_foreground)
            .flex()
            .flex_col()
            .gap_3()
            .flex_shrink_0()
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().warning)
                    .child(tr("term-review-paste")),
            )
            .child(div().font_weight(gpui::FontWeight::SEMIBOLD).child(format!(
                "Paste {} lines into this session?",
                text.lines().count()
            )))
            .child(
                div()
                    .text_sm()
                    .text_color(colors.muted_foreground)
                    .child(tr("term-paste-help")),
            )
            .child(
                div()
                    .id("paste-preview-content")
                    .max_h(px(144.))
                    .overflow_y_scroll()
                    .p_3()
                    .rounded_md()
                    .border_1()
                    .border_color(colors.border)
                    .bg(colors.background)
                    .font_family(terminal_font_family())
                    .text_sm()
                    .child(preview),
            )
            .when(truncated, |panel| {
                panel.child(
                    div()
                        .text_xs()
                        .text_color(colors.muted_foreground)
                        .child(tr("term-paste-truncated")),
                )
            })
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_2()
                    .child(
                        Button::new("confirm-paste")
                            .primary()
                            .label(tr("term-paste-text"))
                            .disabled(!self.state().accepts_input())
                            .on_click(cx.listener(|view, _, window, cx| {
                                if let Some(text) = view.paste_preview.take() {
                                    if let Some(session) = &view.session
                                        && let Ok(bytes) =
                                            encode_paste(session.state(), session.modes(), &text)
                                    {
                                        view.send(bytes, cx);
                                    }
                                    view.focus.focus(window, cx);
                                    cx.notify();
                                }
                            })),
                    )
                    .child(
                        Button::new("cancel-paste")
                            .ghost()
                            .label(tr("cancel"))
                            .on_click(cx.listener(|view, _, window, cx| {
                                view.paste_preview = None;
                                view.focus.focus(window, cx);
                                cx.notify();
                            })),
                    ),
            )
            .into_any_element()
    }

    fn event(&mut self, event: TerminalEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            TerminalEvent::SetClipboard(text) => {
                cx.write_to_clipboard(ClipboardItem::new_string(text))
            }
            TerminalEvent::Title(title) => {
                self.title = if title.is_empty() {
                    "Local terminal".into()
                } else {
                    title
                }
            }
            TerminalEvent::Bell | TerminalEvent::Notification(_) => window.play_system_bell(),
            TerminalEvent::WriteToTransport(bytes) => self.send(bytes, cx),
            TerminalEvent::CurrentDirectory(path) => self.current_directory = Some(path),
            TerminalEvent::PromptStarted | TerminalEvent::CommandFinished => self.at_prompt = true,
            TerminalEvent::CommandStarted => self.at_prompt = false,
            _ => {}
        }
    }

    fn arm_sync(&mut self, deadline: Instant, window: &mut Window, cx: &mut Context<Self>) {
        if self
            .sync_task
            .as_ref()
            .is_some_and(|(current, _)| *current == deadline)
        {
            return;
        }
        let timer = cx
            .background_executor()
            .timer(deadline.saturating_duration_since(Instant::now()));
        let task = cx.spawn_in(window, async move |this, cx| {
            timer.await;
            let _ = this.update_in(cx, |view, window, cx| {
                view.sync_task = None;
                if let Some(session) = &view.session {
                    let events = session.expire_sync(Instant::now());
                    for event in events {
                        view.event(event, window, cx);
                    }
                }
                view.queue_frame(window, cx);
            });
        });
        self.sync_task = Some((deadline, task));
    }

    fn queue_frame(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.frame_pending {
            return;
        }
        self.frame_pending = true;
        let weak = cx.weak_entity();
        window.on_next_frame(move |window, cx| {
            let _ = weak.update(cx, |view, cx| {
                view.frame_pending = false;
                if let Some(deadline) = view.session.as_ref().and_then(Session::sync_deadline) {
                    view.arm_sync(deadline, window, cx);
                } else {
                    if let Some(session) = &view.session {
                        view.snapshot = session.snapshot();
                    }
                    cx.notify();
                }
            });
        });
    }

    fn send(&mut self, bytes: Vec<u8>, cx: &mut Context<Self>) {
        self.send_checked(bytes, cx);
    }

    fn send_checked(&mut self, bytes: Vec<u8>, cx: &mut Context<Self>) -> bool {
        #[cfg(feature = "test-support")]
        if self.sent_input.len() >= 512 {
            self.sent_input.clear();
        }
        #[cfg(feature = "test-support")]
        self.sent_input.push(bytes.clone());
        let Some(session) = &self.session else {
            return false;
        };
        match session.write(bytes) {
            Ok(()) => true,
            Err(error) => {
                self.error = Some(error.to_string());
                cx.notify();
                false
            }
        }
    }

    fn state(&self) -> SessionState {
        if self.stopped {
            return SessionState::Disconnected;
        }
        self.session
            .as_ref()
            .map(Session::state)
            .unwrap_or(SessionState::Connecting)
    }

    fn focus_changed(&mut self, focused: bool, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(session) = &self.session {
            self.send(encode_focus(session.state(), session.modes(), focused), cx);
        }
        self.refresh_blink(window, cx);
        cx.notify();
    }

    fn refresh_blink(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !window.is_window_active()
            || !self.focus.is_focused(window)
            || !self.state().accepts_input()
        {
            self.blink_task = None;
            self.cursor_on = true;
            return;
        }
        if self.blink_task.is_some() {
            return;
        }
        self.blink_task = Some(cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(600))
                    .await;
                let active = this
                    .update_in(cx, |view, window, cx| {
                        if !window.is_window_active()
                            || !view.focus.is_focused(window)
                            || !view.state().accepts_input()
                        {
                            view.blink_task = None;
                            view.cursor_on = true;
                            return false;
                        }
                        view.cursor_on = !view.cursor_on;
                        cx.notify();
                        true
                    })
                    .unwrap_or(false);
                if !active {
                    break;
                }
            }
        }));
    }

    fn copy(&mut self, cx: &mut Context<Self>) {
        if let Some(selection) = self.selection
            && let Ok(text) = self.snapshot.copy_selection(selection)
            && !text.is_empty()
        {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    fn select_all(&mut self, cx: &mut Context<Self>) {
        self.anchor = None;
        if let Some(last) = self.snapshot.rows.last() {
            self.selection = Some(Selection::Linear {
                start: Position { row: 0, column: 0 },
                end: Position {
                    row: self.snapshot.rows.len() - 1,
                    column: last.cells.len().saturating_sub(1),
                },
            });
            cx.notify();
        }
    }

    fn paste_text(&mut self, text: &str, cx: &mut Context<Self>) {
        if !self.state().accepts_input() || self.prompt.is_some() {
            return;
        }
        if let Some(session) = &self.session {
            if !session.modes().bracketed_paste && (text.contains('\n') || text.contains('\r')) {
                self.paste_preview = Some(text.to_string());
                cx.notify();
                return;
            }
            match encode_paste(session.state(), session.modes(), text) {
                Ok(bytes) => {
                    self.selection = None;
                    self.anchor = None;
                    session.scroll_to_bottom();
                    self.send(bytes, cx);
                    cx.notify();
                }
                Err(error) => {
                    self.error = Some(error.to_string());
                    cx.notify();
                }
            }
        }
    }

    fn paste_item(&mut self, item: ClipboardItem, cx: &mut Context<Self>) {
        if !self.state().accepts_input() {
            return;
        }
        if item.entries().iter().any(|entry| {
            matches!(
                entry,
                ClipboardEntry::Image(_) | ClipboardEntry::ExternalPaths(_)
            )
        }) {
            cx.emit(TerminalViewEvent::UploadClipboard(item));
        } else if let Some(text) = item.text() {
            self.paste_text(&text, cx);
        }
    }

    fn key_down(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if self.prompt.is_some() || self.paste_preview.is_some() {
            return;
        }
        let key = &event.keystroke;
        let Some(session) = &self.session else {
            return;
        };
        let modes = session.modes();
        // Ordinary printable input proceeds through GPUI's IME/text handler.
        let key_value = key_value(&key.key, key.key_char.as_deref());
        if matches!(key_value, Some(Key::Character(_)))
            && (event.prefer_character_input
                || (key.key_char.is_some()
                    && !key.modifiers.control
                    && !key.modifiers.alt
                    && !key.modifiers.platform
                    && !modes.kitty.report_all_keys))
        {
            return;
        }
        if let Some(key_value) = key_value {
            let kind = if event.is_held {
                KeyEventKind::Repeat
            } else {
                KeyEventKind::Press
            };
            if let Ok(bytes) = encode_key(
                session.state(),
                modes,
                key_value,
                modifiers(key.modifiers),
                kind,
            ) {
                self.selection = None;
                session.scroll_to_bottom();
                self.send(bytes, cx);
                cx.stop_propagation();
            }
        }
    }

    fn key_up(&mut self, event: &KeyUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.prompt.is_some() || self.paste_preview.is_some() {
            return;
        }
        if let Some(session) = &self.session
            && let Some(key) = key_value(&event.keystroke.key, event.keystroke.key_char.as_deref())
            && let Ok(bytes) = encode_key(
                session.state(),
                session.modes(),
                key,
                modifiers(event.keystroke.modifiers),
                KeyEventKind::Release,
            )
        {
            self.send(bytes, cx);
        }
    }

    fn send_alt_tab(&mut self, cx: &mut Context<Self>) {
        if self.prompt.is_some() || self.paste_preview.is_some() || !self.state().accepts_input() {
            return;
        }
        let Some(session) = &self.session else {
            return;
        };
        let modes = session.modes();
        let modifiers = Modifiers {
            alt: true,
            ..Modifiers::default()
        };
        let state = session.state();
        let Ok(mut bytes) = encode_key(state, modes, Key::Tab, modifiers, KeyEventKind::Press)
        else {
            return;
        };
        let release = encode_key(state, modes, Key::Tab, modifiers, KeyEventKind::Release)
            .unwrap_or_default();
        session.scroll_to_bottom();
        self.selection = None;
        self.anchor = None;
        bytes.extend(release);
        self.send(bytes, cx);
        cx.notify();
    }

    fn position(&self, position: Point<Pixels>) -> Position {
        Position {
            row: ((f32::from(position.y - self.bounds.top()) / f32::from(self.cell_height)).max(0.)
                as usize)
                .min(usize::from(self.dimensions.rows()) - 1),
            column: ((f32::from(position.x - self.bounds.left()) / f32::from(self.cell_width))
                .max(0.) as usize)
                .min(usize::from(self.dimensions.columns()) - 1),
        }
    }

    fn mouse_report(
        &mut self,
        position: Point<Pixels>,
        code: u8,
        release: bool,
        mods: gpui::Modifiers,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(session) = &self.session else {
            return false;
        };
        let (enabled, sgr, _, _) = session.mouse_modes();
        if !enabled || mods.shift {
            return false;
        }
        let position = self.position(position);
        let code = code + if mods.alt { 8 } else { 0 } + if mods.control { 16 } else { 0 };
        let bytes = if sgr {
            format!(
                "\x1b[<{};{};{}{}",
                code,
                position.column + 1,
                position.row + 1,
                if release { 'm' } else { 'M' }
            )
            .into_bytes()
        } else {
            if position.column >= 223 || position.row >= 223 {
                return true;
            }
            vec![
                0x1b,
                b'[',
                b'M',
                32 + if release { 3 } else { code },
                33 + position.column as u8,
                33 + position.row as u8,
            ]
        };
        self.send(bytes, cx);
        true
    }

    fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.focus.focus(window, cx);
        if event.modifiers.control || event.modifiers.platform {
            let position = self.position(event.position);
            if let Some(uri) = self
                .snapshot
                .rows
                .get(position.row)
                .and_then(|row| row.cells.get(position.column))
                .and_then(|cell| cell.hyperlink.as_deref())
                && (uri.starts_with("https://") || uri.starts_with("http://"))
            {
                cx.open_url(uri);
                cx.stop_propagation();
                return;
            }
        }
        if self.mouse_report(event.position, 0, false, event.modifiers, cx) {
            return;
        }
        let position = self.position(event.position);
        self.anchor = Some((position, event.modifiers.alt));
        // A focus click is not a selection: Ctrl+C must still interrupt the shell.
        self.selection = None;
        cx.notify();
    }

    fn mouse_up(&mut self, event: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        let selecting = self.anchor.take().is_some();
        if selecting {
            return;
        }
        self.mouse_report(event.position, 0, true, event.modifiers, cx);
    }

    fn mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if let Some((start, rectangle)) = self.anchor {
            if event.pressed_button != Some(MouseButton::Left) {
                self.anchor = None;
                return;
            }
            let end = self.position(event.position);
            self.selection = (start != end).then_some(if rectangle {
                Selection::Rectangle { start, end }
            } else {
                Selection::Linear { start, end }
            });
            cx.notify();
        } else if let Some(session) = &self.session {
            let (_, _, drag, any) = session.mouse_modes();
            if any || (drag && event.pressed_button.is_some()) {
                let code = if event.pressed_button.is_some() {
                    32
                } else {
                    35
                };
                self.mouse_report(event.position, code, false, event.modifiers, cx);
            }
        }
    }

    fn scroll(&mut self, event: &ScrollWheelEvent, window: &mut Window, cx: &mut Context<Self>) {
        let lines = match event.delta {
            ScrollDelta::Lines(point) => point.y,
            ScrollDelta::Pixels(point) => f32::from(point.y) / f32::from(self.cell_height),
        };
        let count = lines.abs().ceil().min(20.) as usize;
        if count == 0 {
            return;
        }
        let code = if lines > 0. { 64 } else { 65 };
        if self.mouse_report(event.position, code, false, event.modifiers, cx) {
            for _ in 1..count {
                self.mouse_report(event.position, code, false, event.modifiers, cx);
            }
        } else if let Some(session) = &self.session {
            session.scroll(if lines > 0. {
                count as i32
            } else {
                -(count as i32)
            });
            self.queue_frame(window, cx);
        }
        cx.stop_propagation();
    }

    fn layout(
        &mut self,
        bounds: Bounds<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<ShapedLine> {
        self.bounds = bounds;
        let terminal_font = font(terminal_font_family());
        let sample = TextRun {
            len: 1,
            font: terminal_font.clone(),
            color: rgb(0xffffff).into(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let width = window
            .text_system()
            .shape_line("M".into(), px(self.font_size), &[sample], None)
            .width;
        self.cell_width = width.max(px(1.));
        let columns = (f32::from(bounds.size.width) / f32::from(self.cell_width))
            .floor()
            .clamp(2., 1000.) as u16;
        let rows = (f32::from(bounds.size.height) / f32::from(self.cell_height))
            .floor()
            .clamp(1., 500.) as u16;
        let dimensions = TerminalSize::new(columns, rows).unwrap();
        if dimensions != self.dimensions
            && let Some(session) = &self.session
            && session.resize(dimensions).is_ok()
        {
            self.dimensions = dimensions;
            self.selection = None;
        }
        if self.shapes.len() != self.snapshot.rows.len() {
            self.shapes.clear();
        }
        for (index, row) in self.snapshot.rows.iter().enumerate() {
            if self
                .shapes
                .get(index)
                .is_some_and(|cached| Arc::ptr_eq(&cached.source, row))
            {
                continue;
            }
            let mut text = String::new();
            let mut runs = Vec::new();
            for cell in &row.cells {
                if cell.wide_continuation {
                    continue;
                }
                text.push_str(&cell.text);
                let mut font = terminal_font.clone();
                if cell.style.bold {
                    font = font.bold();
                }
                if cell.style.italic {
                    font = font.italic();
                }
                let color = if cell.style.inverse {
                    cell.background
                } else {
                    cell.foreground
                };
                runs.push(TextRun {
                    len: cell.text.len(),
                    font,
                    color: rgb_value(color).into(),
                    background_color: None,
                    underline: cell.style.underline.then_some(UnderlineStyle {
                        color: None,
                        thickness: px(1.),
                        wavy: false,
                    }),
                    strikethrough: None,
                });
            }
            let line = window.text_system().shape_line(
                text.into(),
                px(self.font_size),
                &runs,
                Some(self.cell_width),
            );
            let shaped = ShapedRow {
                source: row.clone(),
                line,
                backgrounds: background_spans(row),
            };
            if index < self.shapes.len() {
                self.shapes[index] = shaped;
            } else {
                self.shapes.push(shaped);
            }
        }
        let _ = cx;
        self.shapes.iter().map(|row| row.line.clone()).collect()
    }

    fn selection_contains(&self, row: usize, column: usize) -> bool {
        let point = Position { row, column };
        match self.selection {
            Some(Selection::Linear { start, end }) => {
                point >= start.min(end) && point <= start.max(end)
            }
            Some(Selection::Rectangle { start, end }) => {
                row >= start.row.min(end.row)
                    && row <= start.row.max(end.row)
                    && column >= start.column.min(end.column)
                    && column <= start.column.max(end.column)
            }
            None => false,
        }
    }
}

impl Focusable for TerminalView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for TerminalView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let prepaint_view = cx.entity();
        let paint_view = cx.entity();
        let menu_view = cx.entity().downgrade();
        let state = self.state();
        let colors = cx.theme().semantic_tokens().colors;
        let status_label = self.connection_status();
        let status_color = match state {
            SessionState::Reconnecting | SessionState::NeedsUserAction => cx.theme().warning,
            _ if self.error.is_some() => colors.destructive,
            SessionState::Connected => cx.theme().success,
            _ => colors.muted_foreground,
        };
        let status = self.error.clone().unwrap_or_else(|| match state {
            SessionState::Connected => self.title.clone(),
            SessionState::Closed => tr("term-shell-ended"),
            _ => tr(match state {
                SessionState::Connecting => "session-connecting",
                SessionState::Authenticating => "session-authenticating",
                SessionState::Disconnected => "session-disconnected",
                SessionState::Reconnecting => "session-reconnecting",
                SessionState::NeedsUserAction => "session-needs-user-action",
                _ => "session-closed",
            }),
        });
        // Retain focus listeners for the lifetime of this view.
        let _ = &self.subscriptions;
        div()
            .id("terminal")
            .key_context(if self.prompt.is_some() {
                "OpsSSHAuthentication"
            } else if self.paste_preview.is_some() {
                "OpsSSHPasteReview"
            } else {
                "OpsSSHTerminal"
            })
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(0x12141a))
            .text_color(colors.foreground)
            .track_focus(&self.focus)
            .on_drop(cx.listener(|view, paths: &ExternalPaths, window, cx| {
                if view.state().accepts_input()
                    && view.prompt.is_none()
                    && view.paste_preview.is_none()
                {
                    view.focus(window, cx);
                    cx.emit(TerminalViewEvent::UploadFiles(
                        paths.0.iter().cloned().collect(),
                    ));
                }
            }))
            .on_key_down(cx.listener(Self::key_down))
            .on_key_up(cx.listener(Self::key_up))
            .child(
                div()
                    .min_h(px(40.))
                    .px_3()
                    .py_2()
                    .flex_shrink_0()
                    .border_b_1()
                    .border_color(colors.border)
                    .bg(colors.surface)
                    .flex()
                    .items_start()
                    .gap_3()
                    .text_sm()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .flex_shrink_0()
                            .child(div().size(px(6.)).rounded_full().bg(status_color))
                            .child(div().text_color(status_color).child(status_label)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_color(if self.error.is_some() {
                                status_color
                            } else {
                                colors.muted_foreground
                            })
                            .child(status),
                    ),
            )
            .when(self.tmux_missing, |d| {
                d.child(
                    div()
                        .px_3()
                        .py_2()
                        .bg(colors.surface)
                        .flex()
                        .gap_2()
                        .child(
                            Button::new("connect-without-tmux")
                                .label(tr("term-connect-unprotected"))
                                .on_click(cx.listener(|view, _, window, cx| {
                                    view.connect_without_protection(window, cx)
                                })),
                        )
                        .child(div().text_sm().child(tr("term-tmux-install-help"))),
                )
            })
            .child(
                div()
                    .id("terminal-content")
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .cursor_text()
                    .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(|view, _, window, cx| {
                            view.anchor = None;
                            view.focus.focus(window, cx);
                        }),
                    )
                    .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
                    .on_mouse_up_out(MouseButton::Left, cx.listener(Self::mouse_up))
                    .on_mouse_move(cx.listener(Self::mouse_move))
                    .on_scroll_wheel(cx.listener(Self::scroll))
                    .child(
                        canvas(
                            move |bounds, window, cx| {
                                prepaint_view.update(cx, |view, cx| view.layout(bounds, window, cx))
                            },
                            move |bounds, lines, window, cx| {
                                paint_view.update(cx, |view, cx| {
                                    window.handle_input(
                                        &view.focus,
                                        ElementInputHandler::new(bounds, paint_view.clone()),
                                        cx,
                                    );
                                    for (row_index, row) in view.snapshot.rows.iter().enumerate() {
                                        let y = bounds.top() + view.cell_height * row_index as f32;
                                        if y >= bounds.bottom() {
                                            break;
                                        }
                                        if let Some(shaped) = view.shapes.get(row_index) {
                                            for (columns, color) in &shaped.backgrounds {
                                                window.paint_quad(fill(
                                                    Bounds::new(
                                                        point(
                                                            bounds.left()
                                                                + view.cell_width
                                                                    * columns.start as f32,
                                                            y,
                                                        ),
                                                        size(
                                                            view.cell_width * columns.len() as f32,
                                                            view.cell_height,
                                                        ),
                                                    ),
                                                    rgb_value(*color),
                                                ));
                                            }
                                        }
                                        if view.selection.is_some() {
                                            let selected: Vec<_> = (0..row.cells.len())
                                                .filter(|column| {
                                                    view.selection_contains(row_index, *column)
                                                })
                                                .collect();
                                            if let (Some(first), Some(last)) =
                                                (selected.first(), selected.last())
                                            {
                                                window.paint_quad(fill(
                                                    Bounds::new(
                                                        point(
                                                            bounds.left()
                                                                + view.cell_width * *first as f32,
                                                            y,
                                                        ),
                                                        size(
                                                            view.cell_width
                                                                * (last - first + 1) as f32,
                                                            view.cell_height,
                                                        ),
                                                    ),
                                                    rgba(0x5699cc80),
                                                ));
                                            }
                                        }
                                        if let Some(line) = lines.get(row_index) {
                                            let _ = line.paint(
                                                point(bounds.left(), y),
                                                view.cell_height,
                                                gpui::TextAlign::Left,
                                                None,
                                                window,
                                                cx,
                                            );
                                        }
                                    }
                                    if view.focus.is_focused(window)
                                        && view.cursor_on
                                        && let Some(cursor) =
                                            view.snapshot.cursor.filter(|cursor| cursor.visible)
                                    {
                                        let origin = point(
                                            bounds.left()
                                                + view.cell_width * cursor.position.column as f32,
                                            bounds.top()
                                                + view.cell_height * cursor.position.row as f32,
                                        );
                                        let cursor_size = match cursor.shape {
                                            opsssh_term_core::CursorShape::Beam => {
                                                size(px(2.), view.cell_height)
                                            }
                                            opsssh_term_core::CursorShape::Underline => {
                                                size(view.cell_width, px(2.))
                                            }
                                            _ => size(view.cell_width, view.cell_height),
                                        };
                                        window.paint_quad(fill(
                                            Bounds::new(origin, cursor_size),
                                            rgba(0x88d0dd80),
                                        ));
                                    }
                                });
                            },
                        )
                        .size_full(),
                    )
                    .context_menu(move |menu, _, cx| {
                        let Some(view) = menu_view.upgrade() else {
                            return menu;
                        };
                        let terminal = view.read(cx);
                        let copy_enabled = terminal.selection.is_some();
                        let paste_enabled = terminal.state().accepts_input()
                            && terminal.prompt.is_none()
                            && terminal.paste_preview.is_none()
                            && cx.read_from_clipboard().is_some();
                        let copy_view = menu_view.clone();
                        let paste_view = menu_view.clone();
                        let select_view = menu_view.clone();
                        let send_view = menu_view.clone();
                        let send_enabled = terminal.state().accepts_input()
                            && terminal.prompt.is_none()
                            && terminal.paste_preview.is_none();
                        menu.item(
                            PopupMenuItem::new(tr("term-copy"))
                                .disabled(!copy_enabled)
                                .on_click(move |_, window, cx| {
                                    let _ = copy_view.update(cx, |view, cx| {
                                        view.copy(cx);
                                        view.focus.focus(window, cx);
                                    });
                                }),
                        )
                        .item(
                            PopupMenuItem::new(tr("term-paste"))
                                .disabled(!paste_enabled)
                                .on_click(move |_, window, cx| {
                                    let _ = paste_view.update(cx, |view, cx| {
                                        if let Some(item) = cx.read_from_clipboard() {
                                            view.paste_item(item, cx);
                                        }
                                        view.focus.focus(window, cx);
                                        cx.notify();
                                    });
                                }),
                        )
                        .separator()
                        .item(PopupMenuItem::new(tr("term-select-all")).on_click(
                            move |_, window, cx| {
                                let _ = select_view.update(cx, |view, cx| {
                                    view.select_all(cx);
                                    view.focus.focus(window, cx);
                                });
                            },
                        ))
                        .separator()
                        .item(
                            PopupMenuItem::new(tr("term-send-alt-tab"))
                                .disabled(!send_enabled)
                                .on_click(move |_, window, cx| {
                                    let _ = send_view.update(cx, |view, cx| {
                                        view.send_alt_tab(cx);
                                        view.focus.focus(window, cx);
                                    });
                                }),
                        )
                    }),
            )
            .when(!self.composition.text.is_empty(), |element| {
                element.child(
                    div()
                        .min_h(px(26.))
                        .px_3()
                        .py_1()
                        .bg(colors.surface)
                        .text_color(colors.surface_foreground)
                        .child(self.composition.text.clone()),
                )
            })
            .when(self.prompt.is_some(), |element| {
                element.child(self.prompt_panel(cx))
            })
            .when(self.paste_preview.is_some(), |element| {
                element.child(self.paste_panel(cx))
            })
    }
}

fn rgb_value(color: Rgb) -> gpui::Rgba {
    rgb((u32::from(color.0) << 16) | (u32::from(color.1) << 8) | u32::from(color.2))
}

fn background_spans(row: &Row) -> Vec<(Range<usize>, Rgb)> {
    let mut spans: Vec<(Range<usize>, Rgb)> = Vec::new();
    for (column, cell) in row.cells.iter().enumerate() {
        let color = if cell.style.inverse {
            cell.foreground
        } else {
            cell.background
        };
        if let Some((range, last)) = spans.last_mut()
            && *last == color
        {
            range.end = column + 1;
        } else {
            spans.push((column..column + 1, color));
        }
    }
    spans
}

fn modifiers(modifiers: gpui::Modifiers) -> Modifiers {
    Modifiers {
        shift: modifiers.shift,
        alt: modifiers.alt,
        control: modifiers.control,
        super_key: modifiers.platform,
    }
}

fn key_value(key: &str, character: Option<&str>) -> Option<Key> {
    if let Some(number) = key
        .strip_prefix('f')
        .and_then(|number| number.parse::<u8>().ok())
        .filter(|number| (1..=35).contains(number))
    {
        return Some(Key::Function(number));
    }
    Some(match key {
        "enter" => Key::Enter,
        "tab" => Key::Tab,
        "backspace" => Key::Backspace,
        "escape" => Key::Escape,
        "up" => Key::Up,
        "down" => Key::Down,
        "left" => Key::Left,
        "right" => Key::Right,
        "home" => Key::Home,
        "end" => Key::End,
        "insert" => Key::Insert,
        "delete" => Key::Delete,
        "pageup" => Key::PageUp,
        "pagedown" => Key::PageDown,
        "space" => Key::Character(' '),
        _ => {
            let text = character.unwrap_or(key);
            let mut chars = text.chars();
            let character = chars.next()?;
            if chars.next().is_some() {
                return None;
            }
            Key::Character(character)
        }
    })
}

impl EntityInputHandler for TerminalView {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        adjusted: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let (bytes, actual) = self.composition.range(range);
        *adjusted = Some(actual);
        Some(self.composition.text[bytes].to_string())
    }
    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.composition.selected.clone(),
            reversed: false,
        })
    }
    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        (!self.composition.text.is_empty()).then(|| 0..self.composition.text.encode_utf16().count())
    }
    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.composition.clear();
        cx.notify();
    }
    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let committed = if self.composition.text.is_empty() {
            text.to_string()
        } else {
            self.composition.replace(range, text, None);
            std::mem::take(&mut self.composition.text)
        };
        self.composition.clear();
        self.selection = None;
        if let Ok(bytes) = encode_text(self.state(), &committed) {
            self.send(bytes, cx);
        }
        cx.notify();
    }
    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        selection: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.composition.replace(range, text, selection);
        cx.notify();
    }
    fn bounds_for_range(
        &mut self,
        _: Range<usize>,
        _: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let cursor = self.snapshot.cursor?;
        Some(Bounds::new(
            point(
                self.bounds.left() + self.cell_width * cursor.position.column as f32,
                self.bounds.top() + self.cell_height * cursor.position.row as f32,
            ),
            size(self.cell_width, self.cell_height),
        ))
    }
    fn character_index_for_point(
        &mut self,
        _: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        Some(0)
    }
    fn accepts_text_input(&self, _: &mut Window, _: &mut Context<Self>) -> bool {
        self.state().accepts_input()
    }
    fn paste(&mut self, item: ClipboardItem, _: &mut Window, cx: &mut Context<Self>) {
        self.paste_item(item, cx);
    }
}

#[cfg(all(test, feature = "test-support"))]
mod clipboard_tests {
    use super::*;
    use gpui::{Entity, TestAppContext, VisualTestContext};
    use opsssh_term_core::{Cell, TerminalBackend};

    #[gpui::test]
    fn root_focus_bindings_cannot_consume_terminal_tab_keys(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            TerminalView::bind_keys(cx);
        });
        let session = Session::local(LocalShellOptions::default()).unwrap();
        let mut view = None;
        let (_root, cx) = cx.add_window_view(|window, cx| {
            let terminal = cx.new(|cx| {
                let mut view = fixture(window, cx);
                view.stopped = false;
                view.session = Some(session);
                view.focus.focus(window, cx);
                view
            });
            view = Some(terminal.clone());
            gpui_component::Root::new(terminal, window, cx)
        });
        let view = view.unwrap();
        draw(cx);
        for (keys, expected) in [
            ("shift-tab", b"\x1b[Z".as_slice()),
            ("tab", b"\t".as_slice()),
            ("left", b"\x1b[D".as_slice()),
            ("ctrl-c", b"\x03".as_slice()),
            ("ctrl-v", b"\x16".as_slice()),
            ("ctrl-n", b"\x0e".as_slice()),
            ("ctrl-k", b"\x0b".as_slice()),
            ("ctrl-w", b"\x17".as_slice()),
            ("ctrl-shift-v", b"\x16".as_slice()),
            ("alt-left", b"\x1b[1;3D".as_slice()),
            ("shift-left", b"\x1b[1;2D".as_slice()),
        ] {
            cx.update(|_, cx| view.update(cx, |view, _| view.sent_input.clear()));
            cx.simulate_keystrokes(keys);
            assert_eq!(
                view.read_with(cx, |view, _| view.sent_input.concat()),
                expected,
                "{keys}"
            );
            cx.update(|window, cx| assert!(view.read(cx).focus.is_focused(window)));
        }
        cx.update(|_, cx| {
            view.update(cx, |view, _| {
                view.session
                    .as_ref()
                    .unwrap()
                    .backend()
                    .lock()
                    .unwrap()
                    .ingest(b"\x1b[>1u")
                    .unwrap();
                view.sent_input.clear();
            })
        });
        cx.simulate_keystrokes("shift-tab");
        assert_eq!(
            view.read_with(cx, |view, _| view.sent_input.concat()),
            b"\x1b[9;2u"
        );
        cx.update(|_, cx| view.update(cx, |view, cx| view.disconnect(cx)));
    }

    #[gpui::test]
    fn uploaded_file_paths_use_one_paste_transaction_without_enter(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let session = Session::local(LocalShellOptions::default()).unwrap();
        let (view, cx) = cx.add_window_view(fixture);
        cx.update(|_, cx| {
            view.update(cx, |view, cx| {
                view.stopped = false;
                view.session = Some(session);
                view.session
                    .as_ref()
                    .unwrap()
                    .backend()
                    .lock()
                    .unwrap()
                    .ingest(b"\x1b[?2004h")
                    .unwrap();
                assert!(view.insert_uploaded_path("'/home/backend/image.png' ", cx));
                assert_eq!(
                    view.sent_input.concat(),
                    b"\x1b[200~'/home/backend/image.png' \x1b[201~"
                );
                let count = view.sent_input.len();
                view.paste_preview = Some("review".into());
                assert!(!view.insert_uploaded_path("'/ignored.png' ", cx));
                view.paste_preview = None;
                assert!(!view.insert_uploaded_path("bad\npath", cx));
                view.disconnect(cx);
                assert!(!view.insert_uploaded_path("'/disconnected.png' ", cx));
                assert_eq!(view.sent_input.len(), count);
            })
        });
    }

    #[gpui::test]
    fn native_captured_keys_use_the_encoder_and_reject_stale_generations(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let session = Session::local(LocalShellOptions::default()).unwrap();
        let (view, cx) = cx.add_window_view(fixture);
        cx.update(|_, cx| {
            view.update(cx, |view, cx| {
                view.stopped = false;
                view.session = Some(session);
                view.session
                    .as_ref()
                    .unwrap()
                    .backend()
                    .lock()
                    .unwrap()
                    .ingest(b"\x1b[>11u")
                    .unwrap();
                let mut event = opsssh_platform::keyboard_capture::CapturedKey {
                    target: 1,
                    generation: view.connection_generation,
                    key: Key::Tab,
                    modifiers: Modifiers {
                        alt: true,
                        ..Default::default()
                    },
                    kind: KeyEventKind::Press,
                    release_capture: false,
                };
                for kind in [
                    KeyEventKind::Press,
                    KeyEventKind::Repeat,
                    KeyEventKind::Release,
                ] {
                    event.kind = kind;
                    view.captured_key(event, cx);
                }
                assert_eq!(
                    view.sent_input.concat(),
                    b"\x1b[9;3:1u\x1b[9;3:2u\x1b[9;3:3u"
                );
                view.sent_input.clear();
                event.generation += 1;
                view.captured_key(event, cx);
                assert!(view.sent_input.is_empty());
                event.generation = view.connection_generation;
                view.paste_preview = Some("review".into());
                view.captured_key(event, cx);
                assert!(view.sent_input.is_empty());
                view.paste_preview = None;
                view.disconnect(cx);
                view.captured_key(event, cx);
                assert!(view.sent_input.is_empty());
            })
        });
    }

    fn fixture(window: &mut Window, cx: &mut Context<TerminalView>) -> TerminalView {
        // Invalid options fail before opening any SSH connection.
        let mut view =
            TerminalView::connect(ConnectionOptions::new("", "", PathBuf::new()), window, cx);
        view.disconnect(cx);
        view.snapshot.rows = ["alpha beta", "gamma delta"]
            .into_iter()
            .map(|text| {
                Arc::new(Row {
                    cells: text
                        .chars()
                        .map(|character| Cell {
                            text: character.to_string(),
                            ..Cell::default()
                        })
                        .collect(),
                    soft_wrapped: false,
                })
            })
            .collect();
        view
    }

    fn draw(cx: &mut VisualTestContext) {
        cx.run_until_parked();
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
    }

    fn cell(
        view: &Entity<TerminalView>,
        cx: &VisualTestContext,
        row: usize,
        column: usize,
    ) -> Point<Pixels> {
        view.read_with(cx, |view, _| {
            point(
                view.bounds.left() + view.cell_width * (column as f32 + 0.5),
                view.bounds.top() + view.cell_height * (row as f32 + 0.5),
            )
        })
    }

    fn drag(
        view: &Entity<TerminalView>,
        cx: &mut VisualTestContext,
        start: (usize, usize),
        end: (usize, usize),
        modifiers: gpui::Modifiers,
    ) {
        let start = cell(view, cx, start.0, start.1);
        let end = cell(view, cx, end.0, end.1);
        cx.simulate_mouse_down(start, MouseButton::Left, modifiers);
        cx.simulate_mouse_move(end, Some(MouseButton::Left), modifiers);
        cx.simulate_mouse_up(end, MouseButton::Left, modifiers);
        draw(cx);
    }

    #[gpui::test]
    fn returning_to_an_authenticating_terminal_focuses_the_credentials_field(
        cx: &mut TestAppContext,
    ) {
        cx.update(|cx| {
            gpui_component::init(cx);
            TerminalView::bind_keys(cx);
        });
        let mut view = None;
        let (_root, cx) = cx.add_window_view(|window, cx| {
            let terminal = cx.new(|cx| fixture(window, cx));
            view = Some(terminal.clone());
            gpui_component::Root::new(terminal, window, cx)
        });
        let view = view.unwrap();
        draw(cx);
        let (password, otp) = cx.update(|window, cx| {
            let password = cx.new(|cx| InputState::new(window, cx));
            let otp = cx.new(|cx| InputState::new(window, cx));
            view.update(cx, |view, cx| {
                view.prompt = Some(ConnectionPrompt::Authentication {
                    id: 1,
                    name: "Sign in".into(),
                    instructions: String::new(),
                    fields: vec![
                        ("Password".into(), password.clone()),
                        ("Code".into(), otp.clone()),
                    ],
                    save: false,
                    can_save: false,
                });
                view.focus(window, cx);
                cx.notify();
            });
            (password, otp)
        });
        draw(cx);
        cx.simulate_input("password");
        cx.simulate_keystrokes("tab");
        cx.update(|window, cx| assert_eq!(window.focused(cx), Some(otp.read(cx).focus_handle(cx))));
        cx.simulate_keystrokes("shift-tab");
        cx.update(|window, cx| {
            assert_eq!(window.focused(cx), Some(password.read(cx).focus_handle(cx)))
        });
        cx.update(|window, cx| {
            assert_eq!(password.read(cx).value().to_string(), "password");
            otp.update(cx, |input, cx| input.focus(window, cx));
            view.update(cx, |view, cx| view.focus(window, cx));
            assert_eq!(window.focused(cx), Some(otp.read(cx).focus_handle(cx)));
        });
        draw(cx);
        cx.simulate_input("123456");
        assert_eq!(
            otp.read_with(cx, |input, _| input.value().to_string()),
            "123456"
        );
        assert!(view.read_with(cx, |view, _| view.sent_input.is_empty()));
    }

    #[gpui::test]
    fn harness_keys_reach_the_transport_with_their_modifiers(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let session = Session::local(LocalShellOptions::default()).unwrap();
        let (view, cx) = cx.add_window_view(fixture);
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.stopped = false;
                view.session = Some(session);
                view.focus.focus(window, cx);
            })
        });
        draw(cx);
        for (keys, expected) in [
            ("ctrl-k", b"\x0b".as_slice()),
            ("ctrl-n", b"\x0e".as_slice()),
            ("ctrl-w", b"\x17".as_slice()),
            ("ctrl-j", b"\n".as_slice()),
            ("ctrl-c", b"\x03".as_slice()),
            ("ctrl-r", b"\x12".as_slice()),
            ("ctrl-t", b"\x14".as_slice()),
            ("ctrl-o", b"\x0f".as_slice()),
            ("ctrl-shift-h", b"\x08".as_slice()),
            ("ctrl-shift-q", b"\x11".as_slice()),
            ("shift-tab", b"\x1b[Z".as_slice()),
            ("alt-tab", b"\x1b\t".as_slice()),
            ("ctrl-v", b"\x16".as_slice()),
            ("ctrl-shift-c", b"\x03".as_slice()),
            ("ctrl-shift-v", b"\x16".as_slice()),
            ("shift-insert", b"\x1b[2;2~".as_slice()),
            ("alt-p", b"\x1bp".as_slice()),
            ("alt-enter", b"\x1b\r".as_slice()),
            ("ctrl-alt-v", b"\x1b\x16".as_slice()),
            ("ctrl-alt-c", b"\x1b\x03".as_slice()),
            ("escape", b"\x1b".as_slice()),
            ("up", b"\x1b[A".as_slice()),
        ] {
            cx.update(|_, cx| view.update(cx, |view, _| view.sent_input.clear()));
            cx.simulate_keystrokes(keys);
            assert_eq!(
                view.read_with(cx, |view, _| view.sent_input.concat()),
                expected,
                "{keys} was not forwarded correctly"
            );
        }
        cx.update(|_, cx| {
            view.update(cx, |view, _| {
                view.session
                    .as_ref()
                    .unwrap()
                    .backend()
                    .lock()
                    .unwrap()
                    .ingest(b"\x1b[>1u")
                    .unwrap();
                view.sent_input.clear();
            })
        });
        cx.simulate_keystrokes("shift-enter");
        assert_eq!(
            view.read_with(cx, |view, _| view.sent_input.concat()),
            b"\x1b[13;2u"
        );
        cx.update(|_, cx| view.update(cx, |view, cx| view.disconnect(cx)));
    }

    #[gpui::test]
    fn all_terminal_shortcuts_forward_press_and_release_without_clipboard_actions(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_component::init);
        let session = Session::local(LocalShellOptions::default()).unwrap();
        let (view, cx) = cx.add_window_view(fixture);
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.stopped = false;
                view.session = Some(session);
                view.focus.focus(window, cx);
                view.session
                    .as_ref()
                    .unwrap()
                    .backend()
                    .lock()
                    .unwrap()
                    .ingest(b"\x1b[>11u")
                    .unwrap();
            });
            cx.write_to_clipboard(ClipboardItem::new_string("clipboard unchanged".into()));
        });
        draw(cx);
        for (keys, press, release) in [
            ("ctrl-c", "\x1b[99;5:1u", "\x1b[99;5:3u"),
            ("ctrl-v", "\x1b[118;5:1u", "\x1b[118;5:3u"),
            ("ctrl-shift-c", "\x1b[99;6:1u", "\x1b[99;6:3u"),
            ("ctrl-shift-v", "\x1b[118;6:1u", "\x1b[118;6:3u"),
            ("cmd-c", "\x1b[99;9:1u", "\x1b[99;9:3u"),
            ("cmd-v", "\x1b[118;9:1u", "\x1b[118;9:3u"),
            ("shift-insert", "\x1b[2;2:1~", "\x1b[2;2:3~"),
            ("alt-tab", "\x1b[9;3:1u", "\x1b[9;3:3u"),
        ] {
            cx.update(|_, cx| {
                view.update(cx, |view, _| {
                    view.sent_input.clear();
                    view.selection = Some(Selection::Linear {
                        start: Position { row: 0, column: 0 },
                        end: Position { row: 0, column: 4 },
                    });
                })
            });
            cx.simulate_keystrokes(keys);
            cx.simulate_event(KeyUpEvent {
                keystroke: gpui::Keystroke::parse(keys).unwrap(),
            });
            cx.update(|_, cx| {
                let terminal = view.read(cx);
                assert_eq!(
                    terminal.sent_input.concat(),
                    [press.as_bytes(), release.as_bytes()].concat(),
                    "{keys} was intercepted"
                );
                assert!(terminal.paste_preview.is_none());
                assert_eq!(
                    cx.read_from_clipboard()
                        .and_then(|item| item.text())
                        .as_deref(),
                    Some("clipboard unchanged")
                );
            });
        }
        cx.update(|_, cx| view.update(cx, |view, cx| view.disconnect(cx)));
    }

    #[gpui::test]
    fn context_menu_sends_os_reserved_alt_tab_in_legacy_and_kitty_modes(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let session = Session::local(LocalShellOptions::default()).unwrap();
        let (view, cx) = cx.add_window_view(fixture);
        cx.update(|_, cx| {
            view.update(cx, |view, _| {
                view.stopped = false;
                view.session = Some(session);
            })
        });
        draw(cx);
        for (mode, expected) in [
            (b"\x1b[<u".as_slice(), b"\x1b\t".as_slice()),
            (
                b"\x1b[>11u".as_slice(),
                b"\x1b[9;3:1u\x1b[9;3:3u".as_slice(),
            ),
        ] {
            cx.update(|_, cx| {
                view.update(cx, |view, _| {
                    view.session
                        .as_ref()
                        .unwrap()
                        .backend()
                        .lock()
                        .unwrap()
                        .ingest(mode)
                        .unwrap();
                    view.sent_input.clear();
                })
            });
            let pointer = cell(&view, cx, 0, 2);
            cx.simulate_mouse_down(pointer, MouseButton::Right, Default::default());
            draw(cx);
            cx.simulate_click(pointer + point(px(24.), px(118.)), Default::default());
            draw(cx);
            cx.update(|window, cx| {
                assert_eq!(view.read(cx).sent_input.concat(), expected);
                assert_eq!(window.focused(cx), Some(view.read(cx).focus.clone()));
            });
        }
        cx.update(|_, cx| view.update(cx, |view, cx| view.disconnect(cx)));
        cx.update(|_, cx| {
            view.update(cx, |view, cx| {
                view.sent_input.clear();
                view.send_alt_tab(cx);
                assert!(view.sent_input.is_empty(), "disconnected key was queued");
            })
        });
    }

    fn copy_from_menu(view: &Entity<TerminalView>, cx: &mut VisualTestContext) {
        let pointer = cell(view, cx, 1, 8);
        cx.simulate_mouse_down(pointer, MouseButton::Right, Default::default());
        draw(cx);
        cx.simulate_click(pointer + point(px(24.), px(17.)), Default::default());
        draw(cx);
    }

    #[gpui::test]
    fn drag_then_right_click_copy_uses_the_selection_and_restores_focus(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let (view, cx) = cx.add_window_view(fixture);
        draw(cx);
        drag(&view, cx, (0, 0), (0, 4), Default::default());
        let selection = view.read_with(cx, |view, _| view.selection);
        let pointer = cell(&view, cx, 1, 8);
        cx.simulate_mouse_down(pointer, MouseButton::Right, Default::default());
        draw(cx);
        assert_eq!(view.read_with(cx, |view, _| view.selection), selection);
        // Click Copy beside the pointer, exercising the actual mouse menu path.
        cx.simulate_click(pointer + point(px(24.), px(17.)), Default::default());
        draw(cx);
        cx.update(|window, cx| {
            assert_eq!(
                cx.read_from_clipboard()
                    .and_then(|item| item.text())
                    .as_deref(),
                Some("alpha")
            );
            assert_eq!(window.focused(cx), Some(view.read(cx).focus.clone()));
        });
    }

    #[gpui::test]
    fn reverse_and_rectangle_drags_copy_displayed_text(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let (view, cx) = cx.add_window_view(fixture);
        draw(cx);
        drag(&view, cx, (1, 4), (0, 0), Default::default());
        copy_from_menu(&view, cx);
        cx.update(|_, cx| {
            assert_eq!(
                cx.read_from_clipboard()
                    .and_then(|item| item.text())
                    .as_deref(),
                Some("alpha beta\ngamma")
            );
        });
        drag(
            &view,
            cx,
            (0, 0),
            (1, 4),
            gpui::Modifiers {
                alt: true,
                ..Default::default()
            },
        );
        copy_from_menu(&view, cx);
        cx.update(|_, cx| {
            assert_eq!(
                cx.read_from_clipboard()
                    .and_then(|item| item.text())
                    .as_deref(),
                Some("alpha\ngamma")
            );
        });
    }

    #[gpui::test]
    fn focus_click_and_escape_do_not_create_or_destroy_a_selection(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let (view, cx) = cx.add_window_view(fixture);
        draw(cx);
        let pointer = cell(&view, cx, 0, 2);
        cx.simulate_click(pointer, Default::default());
        assert!(view.read_with(cx, |view, _| view.selection.is_none()));
        cx.update(|_, cx| {
            cx.write_to_clipboard(ClipboardItem::new_string("keep clipboard".into()))
        });
        drag(&view, cx, (0, 0), (0, 4), Default::default());
        let selection = view.read_with(cx, |view, _| view.selection);
        cx.simulate_mouse_down(pointer, MouseButton::Right, Default::default());
        draw(cx);
        cx.simulate_keystrokes("escape");
        draw(cx);
        assert_eq!(view.read_with(cx, |view, _| view.selection), selection);
        cx.update(|window, cx| {
            assert_eq!(
                cx.read_from_clipboard()
                    .and_then(|item| item.text())
                    .as_deref(),
                Some("keep clipboard")
            );
            assert_eq!(window.focused(cx), Some(view.read(cx).focus.clone()));
        });
    }

    #[gpui::test]
    fn menu_paste_preserves_multiline_review_and_sends_single_line_to_pty(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let session = Session::local(LocalShellOptions::default()).unwrap();
        let (view, cx) = cx.add_window_view(fixture);
        cx.update(|_, cx| {
            view.update(cx, |view, _| {
                view.stopped = false;
                view.session = Some(session);
            });
            cx.write_to_clipboard(ClipboardItem::new_string("first\nsecond".into()));
        });
        draw(cx);
        let pointer = cell(&view, cx, 0, 2);
        cx.simulate_mouse_down(pointer, MouseButton::Right, Default::default());
        draw(cx);
        // Paste is the second row, directly beside the original pointer.
        cx.simulate_click(pointer + point(px(24.), px(45.)), Default::default());
        draw(cx);
        cx.update(|window, cx| {
            assert_eq!(
                view.read(cx).paste_preview.as_deref(),
                Some("first\nsecond")
            );
            assert_eq!(window.focused(cx), Some(view.read(cx).focus.clone()));
        });
        // A single-line paste should go straight to the PTY, once, without review.
        let command = if opsssh_platform::info().os == "windows" {
            "Write-Output ('OPSSSH_' + 'PASTE_OK')"
        } else {
            "printf 'OPSSSH_%s\\n' PASTE_OK"
        };
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.paste_preview = None;
                view.focus.focus(window, cx);
                cx.notify();
            });
            cx.write_to_clipboard(ClipboardItem::new_string(command.into()));
        });
        draw(cx);
        let pointer = cell(&view, cx, 0, 2);
        cx.simulate_mouse_down(pointer, MouseButton::Right, Default::default());
        draw(cx);
        cx.simulate_click(pointer + point(px(24.), px(45.)), Default::default());
        draw(cx);
        assert!(view.read_with(cx, |view, _| view.paste_preview.is_none()));
        cx.simulate_keystrokes("enter");
        let deadline = Instant::now() + std::time::Duration::from_secs(15);
        let mut found = false;
        while Instant::now() < deadline {
            let text = view.read_with(cx, |view, _| {
                view.session
                    .as_ref()
                    .unwrap()
                    .snapshot()
                    .rows
                    .iter()
                    .flat_map(|row| row.cells.iter())
                    .map(|cell| cell.text.as_str())
                    .collect::<String>()
            });
            if text.contains("OPSSSH_PASTE_OK") {
                found = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        cx.update(|_, cx| view.update(cx, |view, cx| view.disconnect(cx)));
        assert!(found, "right-click paste did not reach the interactive PTY");
    }
}
