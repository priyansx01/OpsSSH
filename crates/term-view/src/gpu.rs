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
use gpui_component::input::{Input, InputState};
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
    connection_generation: u64,
    at_prompt: bool,
    paste_preview: Option<String>,
    context_menu: bool,
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
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self::start(None, window, cx)
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

    pub fn current_directory(&self) -> Option<&str> {
        self.current_directory.as_deref()
    }

    pub fn verified_host_key(&self) -> Option<&str> {
        self.verified_host_key.as_deref()
    }

    /// Changes when an SSH transport begins reconnecting, invalidating SFTP work.
    pub fn connection_generation(&self) -> u64 {
        self.connection_generation
    }
    pub fn at_shell_prompt(&self) -> bool {
        self.at_prompt
    }

    pub fn insert_text(&mut self, text: &str, cx: &mut Context<Self>) {
        self.paste_text(text, cx);
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
        self.focus.focus(window, cx);
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
        let startup = cx.background_executor().spawn(async move {
            match remote {
                Some(remote) => Session::remote(remote, dimensions),
                None => Session::local(options),
            }
        });
        cx.spawn_in(window, async move |this, cx| {
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
        })
        .detach();
        Self {
            focus,
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
            connection_generation: 0,
            at_prompt: false,
            paste_preview: None,
            context_menu: false,
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
                self.connection_generation = self.connection_generation.wrapping_add(1);
                self.error = Some(format!(
                    "Connection lost. Reconnecting (attempt {attempt}) in {} seconds",
                    delay.as_secs()
                ));
                self.prompt = None;
            }
            SshEvent::Error(error) => self.error = Some(error),
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
        let panel = div()
            .p_4()
            .border_t_1()
            .border_color(rgb(0x425162))
            .bg(rgb(0x1b2530))
            .flex()
            .flex_col()
            .gap_3();
        match &self.prompt {
            Some(ConnectionPrompt::Host {
                host, fingerprint, ..
            }) => panel
                .child("Verify this server before connecting")
                .child(host.clone())
                .child(fingerprint.clone())
                .child(
                    div()
                        .text_sm()
                        .child("Check this fingerprint with your server administrator."),
                )
                .child(
                    div()
                        .flex()
                        .gap_3()
                        .child(
                            gpui_component::button::Button::new("trust-once")
                                .label("Trust once")
                                .on_click(cx.listener(|view, _, window, cx| {
                                    view.trust_host(false, window, cx)
                                })),
                        )
                        .child(
                            gpui_component::button::Button::new("trust-save")
                                .label("Trust and save")
                                .on_click(cx.listener(|view, _, window, cx| {
                                    view.trust_host(true, window, cx)
                                })),
                        )
                        .child(
                            gpui_component::button::Button::new("cancel-trust")
                                .label("Cancel")
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
                .child(if name.is_empty() {
                    "Sign in".to_string()
                } else {
                    name.clone()
                })
                .child(instructions.clone())
                .children(fields.iter().map(|(label, input)| {
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(label.clone())
                        .child(Input::new(input))
                }))
                .when(*can_save, |panel| {
                    panel.child(
                        gpui_component::button::Button::new("save-credential")
                            .label(if *save {
                                "✓ Save this credential on your computer"
                            } else {
                                "Save this credential on your computer"
                            })
                            .on_click(cx.listener(|view, _, _, cx| {
                                if let Some(ConnectionPrompt::Authentication { save, .. }) =
                                    &mut view.prompt
                                {
                                    *save = !*save;
                                    cx.notify();
                                }
                            })),
                    )
                })
                .child(
                    div()
                        .flex()
                        .gap_3()
                        .child(
                            gpui_component::button::Button::new("submit-auth")
                                .label("Continue")
                                .on_click(
                                    cx.listener(|view, _, window, cx| view.submit_auth(window, cx)),
                                ),
                        )
                        .child(
                            gpui_component::button::Button::new("cancel-auth")
                                .label("Cancel")
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
        let text = self.paste_preview.as_deref().unwrap_or_default();
        div()
            .p_4()
            .bg(rgb(0x1b2530))
            .flex()
            .flex_col()
            .gap_2()
            .child("Review multiline paste — it can execute commands")
            .child(text.chars().take(500).collect::<String>())
            .child(
                div()
                    .flex()
                    .gap_3()
                    .child(
                        gpui_component::button::Button::new("confirm-paste")
                            .label("Paste")
                            .on_click(cx.listener(|view, _, _, cx| {
                                if let Some(text) = view.paste_preview.take() {
                                    if let Some(session) = &view.session
                                        && let Ok(bytes) =
                                            encode_paste(session.state(), session.modes(), &text)
                                    {
                                        view.send(bytes, cx);
                                    }
                                    cx.notify();
                                }
                            })),
                    )
                    .child(
                        gpui_component::button::Button::new("cancel-paste")
                            .label("Cancel")
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.paste_preview = None;
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
        if let Some(session) = &self.session
            && let Err(error) = session.write(bytes)
        {
            self.error = Some(error.to_string());
            cx.notify();
        }
    }

    fn state(&self) -> SessionState {
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
        {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    fn paste_text(&mut self, text: &str, cx: &mut Context<Self>) {
        if let Some(session) = &self.session {
            if !session.modes().bracketed_paste && (text.contains('\n') || text.contains('\r')) {
                self.paste_preview = Some(text.to_string());
                cx.notify();
                return;
            }
            match encode_paste(session.state(), session.modes(), text) {
                Ok(bytes) => {
                    self.selection = None;
                    self.send(bytes, cx);
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

    fn selection_panel(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        div()
            .px_3()
            .py_1()
            .bg(rgb(0x1b2530))
            .flex()
            .gap_2()
            .child(
                gpui_component::button::Button::new("selection-copy")
                    .label("Copy")
                    .on_click(cx.listener(|view, _, _, cx| {
                        view.copy(cx);
                        view.context_menu = false;
                        cx.notify();
                    })),
            )
            .child(
                gpui_component::button::Button::new("selection-paste")
                    .label("Paste")
                    .on_click(cx.listener(|view, _, _, cx| {
                        if let Some(item) = cx.read_from_clipboard() {
                            view.paste_item(item, cx);
                        }
                        view.context_menu = false;
                        cx.notify();
                    })),
            )
            .child(
                gpui_component::button::Button::new("selection-all")
                    .label("Select all")
                    .on_click(cx.listener(|view, _, _, cx| {
                        if let Some(last) = view.snapshot.rows.last() {
                            view.selection = Some(Selection::Linear {
                                start: Position { row: 0, column: 0 },
                                end: Position {
                                    row: view.snapshot.rows.len() - 1,
                                    column: last.cells.len().saturating_sub(1),
                                },
                            });
                            cx.notify();
                        }
                    })),
            )
            .child(
                gpui_component::button::Button::new("selection-close")
                    .label("Dismiss")
                    .on_click(cx.listener(|view, _, _, cx| {
                        view.context_menu = false;
                        view.selection = None;
                        cx.notify();
                    })),
            )
            .into_any_element()
    }

    fn key_down(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if self.prompt.is_some() || self.paste_preview.is_some() {
            return;
        }
        let key = &event.keystroke;
        let primary = if opsssh_platform::info().primary_modifier == "Cmd" {
            key.modifiers.platform
        } else {
            key.modifiers.control
        };
        // Let the workspace handle its Home/Quit bindings before terminal input.
        if primary
            && (matches!(key.key.as_str(), "k" | "n" | "w")
                || (key.modifiers.shift && matches!(key.key.as_str(), "h" | "q"))
                || (key.modifiers.platform && key.key == "q"))
        {
            return;
        }
        if primary && key.key == "c" && self.selection.is_some() {
            self.copy(cx);
            cx.stop_propagation();
            return;
        }
        if (primary && key.key == "v") || (key.modifiers.shift && key.key == "insert") {
            if let Some(item) = cx.read_from_clipboard() {
                self.paste_item(item, cx);
            }
            cx.stop_propagation();
            return;
        }
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
        self.context_menu = false;
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
        self.selection = Some(Selection::Linear {
            start: position,
            end: position,
        });
        cx.notify();
    }

    fn mouse_up(&mut self, event: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.mouse_report(event.position, 0, true, event.modifiers, cx) {
            return;
        }
        self.anchor = None;
    }

    fn mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if let Some((start, rectangle)) = self.anchor {
            let end = self.position(event.position);
            self.selection = Some(if rectangle {
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
        let state = self.state();
        let status = self.error.clone().unwrap_or_else(|| match state {
            SessionState::Connected => self.title.clone(),
            SessionState::Closed => "Shell exited — return home to open another terminal".into(),
            _ => crate::SessionPresentation::from(state)
                .message
                .unwrap_or("Starting session…")
                .into(),
        });
        // Retain focus listeners for the lifetime of this view.
        let _ = &self.subscriptions;
        div()
            .id("terminal")
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(0x12141a))
            .text_color(rgb(0xdcdcdc))
            .track_focus(&self.focus)
            .on_drop(cx.listener(|view, paths: &ExternalPaths, _, cx| {
                if view.state().accepts_input() {
                    cx.emit(TerminalViewEvent::UploadFiles(
                        paths.0.iter().cloned().collect(),
                    ));
                }
            }))
            .on_key_down(cx.listener(Self::key_down))
            .on_key_up(cx.listener(Self::key_up))
            .child(
                div()
                    .h(px(32.))
                    .px_3()
                    .flex()
                    .items_center()
                    .text_sm()
                    .child(status),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(|view, _, _, cx| {
                            view.context_menu = true;
                            cx.notify();
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
                    ),
            )
            .when(!self.composition.text.is_empty(), |element| {
                element.child(div().h(px(26.)).px_3().child(self.composition.text.clone()))
            })
            .when(self.prompt.is_some(), |element| {
                element.child(self.prompt_panel(cx))
            })
            .child(div().h(px(36.)).flex_shrink_0().when(
                self.prompt.is_none() && (self.context_menu || self.selection.is_some()),
                |element| element.child(self.selection_panel(cx)),
            ))
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
