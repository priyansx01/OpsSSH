use std::convert::Infallible;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use alacritty_terminal::Term;
use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config, Osc52, TermDamage, TermMode};
use alacritty_terminal::vte::ansi::{self, Color, NamedColor, Processor, Timeout};

use crate::{
    Cell, CellStyle, Cursor, CursorShape, InputModes, KittyMode, Position, Rgb, Row,
    TerminalBackend, TerminalEvent, TerminalSize, TerminalSnapshot,
};

const SYNC_LIMIT: Duration = Duration::from_millis(150);

#[derive(Default)]
struct SyncTimeout {
    deadline: Option<Instant>,
}

impl Timeout for SyncTimeout {
    fn set_timeout(&mut self, _: Duration) {
        // Extensions must not postpone the safety timeout indefinitely.
        self.deadline
            .get_or_insert_with(|| Instant::now() + SYNC_LIMIT);
    }

    fn clear_timeout(&mut self) {
        self.deadline = None;
    }

    fn pending_timeout(&self) -> bool {
        self.deadline.is_some()
    }
}

#[derive(Clone, Default)]
struct Listener(Arc<Mutex<Vec<Event>>>);

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        self.0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(event);
    }
}

#[derive(Clone, Copy)]
struct Size(TerminalSize);

impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        self.screen_lines()
    }
    fn screen_lines(&self) -> usize {
        usize::from(self.0.rows())
    }
    fn columns(&self) -> usize {
        usize::from(self.0.columns()).max(2)
    }
}

/// VT emulator with bounded synchronization holds and write-only OSC 52.
/// Snapshot rows are shared until Alacritty reports damage to that row.
pub struct AlacrittyTerminal {
    term: Term<Listener>,
    parser: Processor<SyncTimeout>,
    listener: Listener,
    size: TerminalSize,
    generation: u64,
    rows: Vec<Arc<Row>>,
}

impl std::fmt::Debug for AlacrittyTerminal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AlacrittyTerminal")
            .field("size", &self.size)
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

impl AlacrittyTerminal {
    pub fn new(size: TerminalSize) -> Self {
        Self::with_scrollback(size, 100_000)
    }

    pub fn with_scrollback(size: TerminalSize, history: usize) -> Self {
        let listener = Listener::default();
        let term = Term::new(
            Config {
                scrolling_history: history,
                kitty_keyboard: true,
                osc52: Osc52::OnlyCopy,
                ..Config::default()
            },
            &Size(size),
            listener.clone(),
        );
        Self {
            term,
            parser: Processor::new(),
            listener,
            size,
            generation: 0,
            rows: Vec::new(),
        }
    }

    pub fn sync_deadline(&self) -> Option<Instant> {
        self.parser.sync_timeout().deadline
    }

    /// Must be called by a one-shot timer even if the PTY produces no more bytes.
    pub fn expire_sync(&mut self, now: Instant) -> Vec<TerminalEvent> {
        if self.sync_deadline().is_some_and(|deadline| now >= deadline) {
            self.parser.stop_sync(&mut self.term);
            self.generation += 1;
            let mut events = self.drain_events();
            events.push(TerminalEvent::SynchronizedOutput(false));
            events
        } else {
            Vec::new()
        }
    }

    pub fn scroll(&mut self, lines: i32) {
        self.term.scroll_display(Scroll::Delta(lines));
        self.generation += 1;
    }

    pub fn scroll_to_bottom(&mut self) {
        self.term.scroll_display(Scroll::Bottom);
    }

    pub fn is_alternate_screen(&self) -> bool {
        self.term.mode().contains(TermMode::ALT_SCREEN)
    }

    pub fn mouse_modes(&self) -> (bool, bool, bool, bool) {
        let mode = self.term.mode();
        (
            mode.intersects(TermMode::MOUSE_MODE),
            mode.contains(TermMode::SGR_MOUSE),
            mode.contains(TermMode::MOUSE_DRAG),
            mode.contains(TermMode::MOUSE_MOTION),
        )
    }

    fn drain_events(&mut self) -> Vec<TerminalEvent> {
        let events = std::mem::take(
            &mut *self
                .listener
                .0
                .lock()
                .unwrap_or_else(|error| error.into_inner()),
        );
        events
            .into_iter()
            .filter_map(|event| match event {
                Event::Title(title) => Some(TerminalEvent::Title(title)),
                Event::ResetTitle => Some(TerminalEvent::Title(String::new())),
                Event::Bell => Some(TerminalEvent::Bell),
                Event::ClipboardStore(_, text) => Some(TerminalEvent::SetClipboard(text)),
                Event::PtyWrite(text) => Some(TerminalEvent::WriteToTransport(text.into_bytes())),
                Event::TextAreaSizeRequest(format) => Some(TerminalEvent::WriteToTransport(
                    format(WindowSize {
                        num_lines: self.size.rows(),
                        num_cols: self.size.columns().max(2),
                        cell_width: 0,
                        cell_height: 0,
                    })
                    .into_bytes(),
                )),
                // Never answer clipboard reads. Only report our own known palette.
                Event::ColorRequest(index, format) => {
                    let rgb = palette(index);
                    Some(TerminalEvent::WriteToTransport(
                        format(ansi::Rgb {
                            r: rgb.0,
                            g: rgb.1,
                            b: rgb.2,
                        })
                        .into_bytes(),
                    ))
                }
                _ => None,
            })
            .collect()
    }
}

impl TerminalBackend for AlacrittyTerminal {
    type Error = Infallible;

    fn ingest(&mut self, bytes: &[u8]) -> Result<Vec<TerminalEvent>, Self::Error> {
        let before = self.sync_deadline().is_some();
        self.parser.advance(&mut self.term, bytes);
        self.generation += 1;
        let mut events = self.drain_events();
        let after = self.sync_deadline().is_some();
        if before != after {
            events.push(TerminalEvent::SynchronizedOutput(after));
        }
        Ok(events)
    }

    fn resize(&mut self, size: TerminalSize) -> Result<(), Self::Error> {
        if self.size != size {
            self.size = size;
            self.term.resize(Size(size));
            self.rows.clear();
            self.generation += 1;
        }
        Ok(())
    }

    fn modes(&self) -> InputModes {
        let mode = self.term.mode();
        InputModes {
            application_cursor: mode.contains(TermMode::APP_CURSOR),
            bracketed_paste: mode.contains(TermMode::BRACKETED_PASTE),
            focus_reporting: mode.contains(TermMode::FOCUS_IN_OUT),
            kitty: KittyMode {
                disambiguate: mode.contains(TermMode::DISAMBIGUATE_ESC_CODES),
                report_events: mode.contains(TermMode::REPORT_EVENT_TYPES),
                report_all_keys: mode.contains(TermMode::REPORT_ALL_KEYS_AS_ESC),
            },
        }
    }

    fn snapshot(&mut self) -> TerminalSnapshot {
        let (mut full_damage, mut changed_rows) = match self.term.damage() {
            TermDamage::Full => (true, Vec::new()),
            TermDamage::Partial(lines) => (false, lines.map(|line| line.line).collect::<Vec<_>>()),
        };
        let content = self.term.renderable_content();
        let count = usize::from(self.size.rows());
        let columns = usize::from(self.size.columns()).max(2);
        if self.rows.len() != count {
            self.rows = (0..count).map(|_| Arc::new(Row::default())).collect();
            full_damage = true;
        }
        if full_damage {
            changed_rows = (0..count).collect();
        }
        let mut updates: Vec<Option<Row>> = (0..count)
            .map(|index| {
                changed_rows.contains(&index).then(|| Row {
                    cells: vec![Cell::default(); columns],
                    soft_wrapped: false,
                })
            })
            .collect();
        for indexed in content.display_iter {
            let line = indexed.point.line.0 + content.display_offset as i32;
            if line < 0 {
                continue;
            }
            let Some(Some(row)) = updates.get_mut(line as usize) else {
                continue;
            };
            let index = indexed.point.column.0;
            if index >= columns {
                continue;
            }
            let cell = indexed.cell;
            let text = if cell.flags.contains(Flags::HIDDEN) {
                " ".to_string()
            } else {
                let mut text = cell.c.to_string();
                if let Some(chars) = cell.zerowidth() {
                    text.extend(chars);
                }
                text
            };
            row.soft_wrapped |= cell.flags.contains(Flags::WRAPLINE);
            row.cells[index] = Cell {
                text,
                foreground: color(cell.fg, content.colors),
                background: color(cell.bg, content.colors),
                style: CellStyle {
                    bold: cell.flags.contains(Flags::BOLD),
                    italic: cell.flags.contains(Flags::ITALIC),
                    underline: cell.flags.intersects(Flags::ALL_UNDERLINES),
                    inverse: cell.flags.contains(Flags::INVERSE),
                },
                wide_continuation: cell
                    .flags
                    .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER),
            };
        }
        for (index, update) in updates.into_iter().enumerate() {
            if let Some(row) = update {
                self.rows[index] = Arc::new(row);
            }
        }
        let cursor_row = content.cursor.point.line.0 + content.display_offset as i32;
        let cursor = (cursor_row >= 0 && (cursor_row as usize) < count).then_some(Cursor {
            position: Position {
                row: cursor_row.max(0) as usize,
                column: content.cursor.point.column.0,
            },
            shape: match content.cursor.shape {
                ansi::CursorShape::Beam => CursorShape::Beam,
                ansi::CursorShape::Underline => CursorShape::Underline,
                _ => CursorShape::Block,
            },
            visible: content.cursor.shape != ansi::CursorShape::Hidden,
        });
        self.term.reset_damage();
        TerminalSnapshot {
            generation: self.generation,
            rows: self.rows.clone(),
            cursor,
            changed_rows,
            full_damage,
        }
    }
}

fn color(color: Color, colors: &alacritty_terminal::term::color::Colors) -> Rgb {
    match color {
        Color::Spec(rgb) => Rgb(rgb.r, rgb.g, rgb.b),
        Color::Named(named) => colors[named]
            .map(|rgb| Rgb(rgb.r, rgb.g, rgb.b))
            .unwrap_or_else(|| palette(named as usize)),
        Color::Indexed(index) => colors[usize::from(index)]
            .map(|rgb| Rgb(rgb.r, rgb.g, rgb.b))
            .unwrap_or_else(|| palette(usize::from(index))),
    }
}

fn palette(index: usize) -> Rgb {
    const ANSI: [Rgb; 16] = [
        Rgb(18, 20, 26),
        Rgb(224, 108, 117),
        Rgb(152, 195, 121),
        Rgb(229, 192, 123),
        Rgb(97, 175, 239),
        Rgb(198, 120, 221),
        Rgb(86, 182, 194),
        Rgb(171, 178, 191),
        Rgb(92, 99, 112),
        Rgb(255, 128, 128),
        Rgb(178, 220, 148),
        Rgb(255, 216, 148),
        Rgb(128, 196, 255),
        Rgb(224, 156, 255),
        Rgb(120, 220, 232),
        Rgb(255, 255, 255),
    ];
    match index {
        0..=15 => ANSI[index],
        16..=231 => {
            let value = index - 16;
            let component = |n: usize| if n == 0 { 0 } else { (55 + n * 40) as u8 };
            Rgb(
                component(value / 36),
                component(value / 6 % 6),
                component(value % 6),
            )
        }
        232..=255 => {
            let grey = (8 + (index - 232) * 10) as u8;
            Rgb(grey, grey, grey)
        }
        n if n == NamedColor::Background as usize => ANSI[0],
        _ => Rgb(220, 220, 220),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(snapshot: &TerminalSnapshot, row: usize) -> String {
        snapshot.rows[row]
            .cells
            .iter()
            .filter(|cell| !cell.wide_continuation)
            .map(|cell| cell.text.as_str())
            .collect::<String>()
            .trim_end()
            .to_string()
    }

    #[test]
    fn fragmented_unicode_and_vt_redraw_produce_the_same_grid() {
        let bytes = "hello\r\n界e\u{301}\x1b[1;1H\x1b[31mHELLO".as_bytes();
        let mut whole = AlacrittyTerminal::new(TerminalSize::new(20, 4).unwrap());
        let mut split = AlacrittyTerminal::new(TerminalSize::new(20, 4).unwrap());
        whole.ingest(bytes).unwrap();
        for byte in bytes {
            split.ingest(&[*byte]).unwrap();
        }
        let whole = whole.snapshot();
        let split = split.snapshot();
        assert_eq!(whole.rows, split.rows);
        assert_eq!(text(&whole, 0), "HELLO");
        assert_eq!(text(&whole, 1), "界e\u{301}");
    }

    #[test]
    fn synchronized_output_is_atomic_and_has_a_safety_timeout() {
        let mut term = AlacrittyTerminal::new(TerminalSize::new(20, 4).unwrap());
        term.ingest(b"before\r\x1b[?2026hafter ").unwrap();
        assert_eq!(text(&term.snapshot(), 0), "before");
        term.ingest(b"\x1b[?2026l").unwrap();
        assert_eq!(text(&term.snapshot(), 0), "after");
        term.ingest(b"\r\x1b[?2026htimeout").unwrap();
        let deadline = term.sync_deadline().unwrap();
        assert!(
            term.expire_sync(deadline - Duration::from_nanos(1))
                .is_empty()
        );
        assert!(!term.expire_sync(deadline).is_empty());
        assert_eq!(text(&term.snapshot(), 0), "timeout");
    }

    #[test]
    fn osc52_can_write_but_cannot_request_clipboard_contents() {
        let mut term = AlacrittyTerminal::new(TerminalSize::new(20, 4).unwrap());
        assert!(
            term.ingest(b"\x1b]52;c;aGVsbG8=\x07")
                .unwrap()
                .contains(&TerminalEvent::SetClipboard("hello".into()))
        );
        assert!(term.ingest(b"\x1b]52;c;?\x07").unwrap().is_empty());
    }

    #[test]
    fn alternate_screen_restores_shell_and_negotiates_input_modes() {
        let mut term = AlacrittyTerminal::new(TerminalSize::new(20, 4).unwrap());
        term.ingest(b"shell\x1b[?1049happ\x1b[?2004h\x1b[?1004h\x1b[>1u")
            .unwrap();
        assert!(term.is_alternate_screen());
        assert!(term.modes().bracketed_paste);
        assert!(term.modes().focus_reporting);
        assert!(term.modes().kitty.disambiguate);
        term.ingest(b"\x1b[?1049l").unwrap();
        assert_eq!(text(&term.snapshot(), 0), "shell");
        assert!(!term.modes().kitty.disambiguate);
    }

    #[test]
    fn resize_and_scrollback_preserve_content_and_share_unchanged_rows() {
        let mut term = AlacrittyTerminal::new(TerminalSize::new(10, 3).unwrap());
        term.ingest(b"one\r\ntwo\r\nthree\r\nfour").unwrap();
        term.scroll(1);
        let previous = term.snapshot();
        assert_eq!(text(&previous, 0), "one");
        let next = term.snapshot();
        assert!(Arc::ptr_eq(&previous.rows[0], &next.rows[0]));
        term.resize(TerminalSize::new(15, 5).unwrap()).unwrap();
        let resized = term.snapshot();
        assert!(resized.full_damage);
        assert_eq!(resized.rows.len(), 5);
        assert_eq!(resized.rows[0].cells.len(), 15);
    }
}
