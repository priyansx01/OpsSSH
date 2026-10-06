use std::fmt;
use std::sync::Arc;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CellStyle {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub inverse: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cell {
    /// A base character plus any combining characters; empty for a continuation.
    pub text: String,
    pub foreground: Rgb,
    pub background: Rgb,
    pub style: CellStyle,
    pub wide_continuation: bool,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            text: " ".into(),
            foreground: Rgb(220, 220, 220),
            background: Rgb(18, 20, 26),
            style: CellStyle::default(),
            wide_continuation: false,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Row {
    pub cells: Vec<Cell>,
    /// This row wraps directly into the next, with no hard newline.
    pub soft_wrapped: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Position {
    pub row: usize,
    pub column: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorShape {
    Block,
    Beam,
    Underline,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cursor {
    pub position: Position,
    pub shape: CursorShape,
    pub visible: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TerminalSnapshot {
    pub generation: u64,
    /// Immutable rows are shared across snapshots; unchanged rows are not copied.
    pub rows: Vec<Arc<Row>>,
    pub cursor: Option<Cursor>,
    pub changed_rows: Vec<usize>,
    pub full_damage: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Selection {
    /// Inclusive endpoints; reversed endpoints are normalized.
    Linear {
        start: Position,
        end: Position,
    },
    Rectangle {
        start: Position,
        end: Position,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidSelection;

impl fmt::Display for InvalidSelection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("selection endpoint is outside the snapshot")
    }
}

impl std::error::Error for InvalidSelection {}

impl TerminalSnapshot {
    /// Copy displayed text, joining soft wraps and trimming end-of-line spaces.
    /// Terminal escape sequences are never reconstructed for clipboard output.
    pub fn copy_selection(&self, selection: Selection) -> Result<String, InvalidSelection> {
        let (start, end, rectangle) = match selection {
            Selection::Linear { start, end } => (start.min(end), start.max(end), false),
            Selection::Rectangle { start, end } => (
                Position {
                    row: start.row.min(end.row),
                    column: start.column.min(end.column),
                },
                Position {
                    row: start.row.max(end.row),
                    column: start.column.max(end.column),
                },
                true,
            ),
        };
        for endpoint in [start, end] {
            if self
                .rows
                .get(endpoint.row)
                .is_none_or(|row| endpoint.column >= row.cells.len())
            {
                return Err(InvalidSelection);
            }
        }
        let mut output = String::new();
        for index in start.row..=end.row {
            let row = &self.rows[index];
            let from = if rectangle || index == start.row {
                start.column
            } else {
                0
            };
            let to = if rectangle || index == end.row {
                end.column.saturating_add(1)
            } else {
                row.cells.len()
            };
            let mut line = String::new();
            for cell in row.cells.iter().take(to).skip(from) {
                if !cell.wide_continuation {
                    line.push_str(&cell.text);
                }
            }
            // Spaces within a soft-wrapped line are meaningful.
            if !rectangle && row.soft_wrapped && index < end.row {
                output.push_str(&line);
            } else {
                output.push_str(line.trim_end_matches(' '));
            }
            if index < end.row && (rectangle || !row.soft_wrapped) {
                output.push('\n');
            }
        }
        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(text: &str, soft_wrapped: bool) -> Arc<Row> {
        Arc::new(Row {
            cells: text
                .chars()
                .map(|c| Cell {
                    text: c.into(),
                    ..Cell::default()
                })
                .collect(),
            soft_wrapped,
        })
    }

    #[test]
    fn linear_copy_joins_soft_wraps_and_normalizes_backwards_selection() {
        let snapshot = TerminalSnapshot {
            rows: vec![
                row("hello ", true),
                row("world ", false),
                row("end   ", false),
            ],
            ..TerminalSnapshot::default()
        };
        assert_eq!(
            snapshot
                .copy_selection(Selection::Linear {
                    start: Position { row: 2, column: 5 },
                    end: Position::default()
                })
                .unwrap(),
            "hello world\nend"
        );
    }

    #[test]
    fn rectangle_preserves_row_boundaries_even_when_wrapped() {
        let snapshot = TerminalSnapshot {
            rows: vec![row("abcde", true), row("fghij", false)],
            ..TerminalSnapshot::default()
        };
        assert_eq!(
            snapshot
                .copy_selection(Selection::Rectangle {
                    start: Position { row: 0, column: 1 },
                    end: Position { row: 1, column: 3 }
                })
                .unwrap(),
            "bcd\nghi"
        );
    }

    #[test]
    fn wide_continuations_and_combining_characters_copy_once() {
        let snapshot = TerminalSnapshot {
            rows: vec![Arc::new(Row {
                cells: vec![
                    Cell {
                        text: "界".into(),
                        ..Cell::default()
                    },
                    Cell {
                        text: String::new(),
                        wide_continuation: true,
                        ..Cell::default()
                    },
                    Cell {
                        text: "e\u{301}".into(),
                        ..Cell::default()
                    },
                ],
                soft_wrapped: false,
            })],
            ..TerminalSnapshot::default()
        };
        assert_eq!(
            snapshot
                .copy_selection(Selection::Linear {
                    start: Position::default(),
                    end: Position { row: 0, column: 2 }
                })
                .unwrap(),
            "界e\u{301}"
        );
    }

    #[test]
    fn stale_selection_returns_an_error_instead_of_panicking() {
        assert_eq!(
            TerminalSnapshot::default().copy_selection(Selection::Linear {
                start: Position::default(),
                end: Position::default()
            }),
            Err(InvalidSelection)
        );
    }
}
