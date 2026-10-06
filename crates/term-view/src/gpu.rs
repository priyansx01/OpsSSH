use std::ops::Range;
use std::sync::Arc;
use std::time::Instant;

use gpui::{
    App, Bounds, ClipboardItem, Context, ElementInputHandler, EntityInputHandler, FocusHandle,
    Focusable, KeyDownEvent, KeyUpEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    Pixels, Point, Render, ScrollDelta, ScrollWheelEvent, ShapedLine, Subscription, Task, TextRun,
    UTF16Selection, UnderlineStyle, Window, canvas, div, fill, font, point, prelude::*, px, rgb,
    rgba, size,
};
use opsssh_platform::{LocalShellOptions, terminal_font_family};
use opsssh_term_core::{
    Key, KeyEventKind, Modifiers, Position, Rgb, Row, Selection, SessionState, TerminalEvent,
    TerminalSize, TerminalSnapshot, encode_focus, encode_key, encode_paste, encode_text,
};

use crate::{LocalSession, SessionNotice};

#[derive(Debug)]
struct ShapedRow {
    source: Arc<Row>,
    line: ShapedLine,
}

/// GPU-rendered local terminal spike. Background workers own all PTY I/O;
/// GPUI's display callback coalesces snapshots at the native frame rate.
pub struct TerminalView {
    focus: FocusHandle,
    session: Option<LocalSession>,
    snapshot: TerminalSnapshot,
    shapes: Vec<ShapedRow>,
    bounds: Bounds<Pixels>,
    cell_width: Pixels,
    cell_height: Pixels,
    dimensions: TerminalSize,
    selection: Option<Selection>,
    anchor: Option<(Position, bool)>,
    composition: String,
    error: Option<String>,
    title: String,
    frame_pending: bool,
    sync_task: Option<(Instant, Task<()>)>,
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
        let focus = cx.focus_handle();
        let subscriptions = vec![
            cx.on_focus(&focus, window, |this, _, cx| {
                this.focus_changed(true, cx);
            }),
            cx.on_blur(&focus, window, |this, _, cx| {
                this.focus_changed(false, cx);
            }),
        ];
        focus.focus(window, cx);
        let options = LocalShellOptions::default();
        let dimensions = options.size;
        let startup = cx
            .background_executor()
            .spawn(async move { LocalSession::spawn(options) });
        cx.spawn_in(window, async move |this, cx| {
            let result = startup.await;
            let notices = this
                .update_in(cx, |view, window, cx| match result {
                    Ok(session) => {
                        let notices = session.notices();
                        view.session = Some(session);
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
            dimensions,
            selection: None,
            anchor: None,
            composition: String::new(),
            error: None,
            title: "Local terminal".into(),
            frame_pending: false,
            sync_task: None,
            subscriptions,
        }
    }

    fn notice(&mut self, notice: SessionNotice, window: &mut Window, cx: &mut Context<Self>) {
        match notice {
            SessionNotice::Event(event) => self.event(event, window, cx),
            SessionNotice::Error(error) => self.error = Some(error),
            _ => {}
        }
        if let Some(deadline) = self.session.as_ref().and_then(LocalSession::sync_deadline) {
            self.arm_sync(deadline, window, cx);
        } else {
            self.sync_task = None;
            self.queue_frame(window, cx);
        }
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
            TerminalEvent::Bell => window.play_system_bell(),
            TerminalEvent::WriteToTransport(bytes) => self.send(bytes, cx),
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
                if let Some(deadline) = view.session.as_ref().and_then(LocalSession::sync_deadline)
                {
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
            .map(LocalSession::state)
            .unwrap_or(SessionState::Connecting)
    }

    fn focus_changed(&mut self, focused: bool, cx: &mut Context<Self>) {
        if let Some(session) = &self.session {
            self.send(encode_focus(session.state(), session.modes(), focused), cx);
        }
        cx.notify();
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

    fn key_down(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let key = &event.keystroke;
        let primary = if opsssh_platform::info().primary_modifier == "Cmd" {
            key.modifiers.platform
        } else {
            key.modifiers.control
        };
        // Let the workspace handle its Home/Quit bindings before terminal input.
        if primary
            && ((key.modifiers.shift && matches!(key.key.as_str(), "h" | "q"))
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
            if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                self.paste_text(&text, cx);
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
        let (enabled, sgr, _, _) = session
            .backend
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .mouse_modes();
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
            let (_, _, drag, any) = session
                .backend
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .mouse_modes();
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
            .shape_line("M".into(), px(16.), &[sample], None)
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
            let line =
                window
                    .text_system()
                    .shape_line(text.into(), px(16.), &runs, Some(self.cell_width));
            let shaped = ShapedRow {
                source: row.clone(),
                line,
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
            _ => "Starting local shell…".into(),
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
                                        for (column, cell) in row.cells.iter().enumerate() {
                                            let background = if cell.style.inverse {
                                                cell.foreground
                                            } else {
                                                cell.background
                                            };
                                            let cell_bounds = Bounds::new(
                                                point(
                                                    bounds.left() + view.cell_width * column as f32,
                                                    y,
                                                ),
                                                size(view.cell_width, view.cell_height),
                                            );
                                            window.paint_quad(fill(
                                                cell_bounds,
                                                rgb_value(background),
                                            ));
                                            if view.selection_contains(row_index, column) {
                                                window.paint_quad(fill(
                                                    cell_bounds,
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
            .when(!self.composition.is_empty(), |element| {
                element.child(div().h(px(26.)).px_3().child(self.composition.clone()))
            })
    }
}

fn rgb_value(color: Rgb) -> gpui::Rgba {
    rgb((u32::from(color.0) << 16) | (u32::from(color.1) << 8) | u32::from(color.2))
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
        _: Range<usize>,
        _: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        Some(self.composition.clone())
    }
    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let length = self.composition.encode_utf16().count();
        Some(UTF16Selection {
            range: length..length,
            reversed: false,
        })
    }
    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        (!self.composition.is_empty()).then(|| 0..self.composition.encode_utf16().count())
    }
    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.composition.clear();
        cx.notify();
    }
    fn replace_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.composition.clear();
        self.selection = None;
        if let Ok(bytes) = encode_text(self.state(), text) {
            self.send(bytes, cx);
        }
        cx.notify();
    }
    fn replace_and_mark_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        text: &str,
        _: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.composition = text.into();
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
        if let Some(text) = item.text() {
            self.paste_text(&text, cx);
        }
    }
}
