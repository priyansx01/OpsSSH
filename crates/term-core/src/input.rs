use crate::SessionState;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Modifiers {
    pub shift: bool,
    pub alt: bool,
    pub control: bool,
    pub super_key: bool,
}

impl Modifiers {
    fn parameter(self) -> u8 {
        1 + u8::from(self.shift)
            + 2 * u8::from(self.alt)
            + 4 * u8::from(self.control)
            + 8 * u8::from(self.super_key)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    /// Layout-resolved character. CSI-u uses its unshifted ASCII codepoint;
    /// IME commits should use `encode_text` instead.
    Character(char),
    Enter,
    Tab,
    Backspace,
    Escape,
    Up,
    Down,
    Right,
    Left,
    Home,
    End,
    Insert,
    Delete,
    PageUp,
    PageDown,
    Function(u8),
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum KeyEventKind {
    #[default]
    Press,
    Repeat,
    Release,
}

/// Supported Kitty enhancements. Alternate-key and associated-text reporting
/// require richer platform key events and are deliberately not advertised yet.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct KittyMode {
    pub disambiguate: bool,
    pub report_events: bool,
    pub report_all_keys: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct InputModes {
    pub application_cursor: bool,
    pub bracketed_paste: bool,
    pub focus_reporting: bool,
    pub kitty: KittyMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputBlocked {
    NotConnected,
    UnsafePaste,
    UnsupportedKey,
}

impl std::fmt::Display for InputBlocked {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotConnected => f.write_str("session is not connected"),
            Self::UnsafePaste => f.write_str("paste contains a bracketed-paste control sequence"),
            Self::UnsupportedKey => f.write_str("unsupported function key"),
        }
    }
}

impl std::error::Error for InputBlocked {}

fn require_connected(state: SessionState) -> Result<(), InputBlocked> {
    if state.accepts_input() {
        Ok(())
    } else {
        Err(InputBlocked::NotConnected)
    }
}

pub fn encode_key(
    state: SessionState,
    modes: InputModes,
    key: Key,
    modifiers: Modifiers,
    kind: KeyEventKind,
) -> Result<Vec<u8>, InputBlocked> {
    require_connected(state)?;
    if matches!(key, Key::Function(number) if !(1..=35).contains(&number)) {
        return Err(InputBlocked::UnsupportedKey);
    }
    let plain_control =
        matches!(key, Key::Enter | Key::Tab | Key::Backspace) && modifiers.parameter() == 1;
    let produces_text = matches!(key, Key::Character(_))
        && !modifiers.control
        && !modifiers.alt
        && !modifiers.super_key;
    let enhanced = modes.kitty.report_all_keys
        || (!plain_control
            && !produces_text
            && (modes.kitty.disambiguate || modes.kitty.report_events));
    if kind == KeyEventKind::Release
        && (!modes.kitty.report_events
            || !enhanced
            || (matches!(key, Key::Enter | Key::Tab | Key::Backspace)
                && !modes.kitty.report_all_keys))
    {
        return Ok(Vec::new());
    }
    if enhanced {
        return Ok(encode_kitty(
            key,
            modifiers,
            kind,
            modes.kitty.report_events,
        ));
    }
    Ok(encode_legacy(key, modifiers, modes.application_cursor))
}

fn encode_kitty(key: Key, modifiers: Modifiers, kind: KeyEventKind, events: bool) -> Vec<u8> {
    let event = if events {
        match kind {
            KeyEventKind::Press => ":1",
            KeyEventKind::Repeat => ":2",
            KeyEventKind::Release => ":3",
        }
    } else {
        ""
    };
    // Functional keys retain their CSI final byte in the Kitty protocol.
    let (code, final_byte) = match key {
        Key::Character(c) => (c.to_ascii_lowercase() as u32, 'u'),
        Key::Enter => (13, 'u'),
        Key::Tab => (9, 'u'),
        Key::Backspace => (127, 'u'),
        Key::Escape => (27, 'u'),
        Key::Up => (1, 'A'),
        Key::Down => (1, 'B'),
        Key::Right => (1, 'C'),
        Key::Left => (1, 'D'),
        Key::Home => (1, 'H'),
        Key::End => (1, 'F'),
        Key::Insert => (2, '~'),
        Key::Delete => (3, '~'),
        Key::PageUp => (5, '~'),
        Key::PageDown => (6, '~'),
        Key::Function(1) => (1, 'P'),
        Key::Function(2) => (1, 'Q'),
        Key::Function(3) => (13, '~'),
        Key::Function(4) => (1, 'S'),
        Key::Function(number @ 5..=12) => (function_tilde(number), '~'),
        Key::Function(number) => (57363 + u32::from(number), 'u'),
    };
    format!("\x1b[{code};{}{event}{final_byte}", modifiers.parameter()).into_bytes()
}

fn encode_legacy(key: Key, modifiers: Modifiers, application_cursor: bool) -> Vec<u8> {
    let parameter = modifiers.parameter();
    if let Key::Function(number) = key {
        if number > 12 {
            if number <= 24 {
                return encode_legacy(
                    Key::Function(number - 12),
                    Modifiers {
                        shift: true,
                        ..modifiers
                    },
                    application_cursor,
                );
            }
            return Vec::new();
        }
        if number <= 4 {
            let final_byte = char::from(b'P' + number - 1);
            return if parameter == 1 {
                format!("\x1bO{final_byte}").into_bytes()
            } else {
                format!("\x1b[1;{parameter}{final_byte}").into_bytes()
            };
        }
        let code = function_tilde(number);
        return if parameter == 1 {
            format!("\x1b[{code}~").into_bytes()
        } else {
            format!("\x1b[{code};{parameter}~").into_bytes()
        };
    }
    let cursor = match key {
        Key::Up => Some('A'),
        Key::Down => Some('B'),
        Key::Right => Some('C'),
        Key::Left => Some('D'),
        Key::Home => Some('H'),
        Key::End => Some('F'),
        _ => None,
    };
    if let Some(final_byte) = cursor {
        return if parameter != 1 {
            format!("\x1b[1;{parameter}{final_byte}").into_bytes()
        } else if application_cursor {
            format!("\x1bO{final_byte}").into_bytes()
        } else {
            format!("\x1b[{final_byte}").into_bytes()
        };
    }
    let tilde = match key {
        Key::Insert => Some(2),
        Key::Delete => Some(3),
        Key::PageUp => Some(5),
        Key::PageDown => Some(6),
        _ => None,
    };
    if let Some(code) = tilde {
        return if parameter == 1 {
            format!("\x1b[{code}~").into_bytes()
        } else {
            format!("\x1b[{code};{parameter}~").into_bytes()
        };
    }
    if key == Key::Tab && modifiers.shift {
        return b"\x1b[Z".to_vec();
    }
    let mut bytes = Vec::with_capacity(5);
    if modifiers.alt {
        bytes.push(0x1b);
    }
    match key {
        Key::Character(c) if modifiers.control && c.is_ascii() => match c.to_ascii_uppercase() {
            '@' | ' ' | '2' => bytes.push(0),
            'A'..='Z' | '[' | '\\' | ']' | '^' | '_' => {
                bytes.push((c.to_ascii_uppercase() as u8) & 0x1f)
            }
            '?' => bytes.push(0x7f),
            _ => bytes.extend_from_slice(c.encode_utf8(&mut [0; 4]).as_bytes()),
        },
        Key::Character(c) => bytes.extend_from_slice(c.encode_utf8(&mut [0; 4]).as_bytes()),
        Key::Enter => bytes.push(b'\r'),
        Key::Tab => bytes.push(b'\t'),
        Key::Backspace => bytes.push(if modifiers.control { 0x08 } else { 0x7f }),
        Key::Escape => bytes.push(0x1b),
        _ => unreachable!("functional keys handled above"),
    }
    bytes
}

fn function_tilde(number: u8) -> u32 {
    match number {
        5 => 15,
        6 => 17,
        7 => 18,
        8 => 19,
        9 => 20,
        10 => 21,
        11 => 23,
        12 => 24,
        _ => 0,
    }
}

/// IME text commits are text, not synthesized physical key presses.
pub fn encode_text(state: SessionState, text: &str) -> Result<Vec<u8>, InputBlocked> {
    require_connected(state)?;
    Ok(text.as_bytes().to_vec())
}

/// Encode the entire paste transaction before transport chunking. Chunking must
/// preserve order and must not interleave other input between these brackets.
pub fn encode_paste(
    state: SessionState,
    modes: InputModes,
    text: &str,
) -> Result<Vec<u8>, InputBlocked> {
    require_connected(state)?;
    if !modes.bracketed_paste {
        return Ok(text.as_bytes().to_vec());
    }
    // A pasted terminator could turn the remainder into shell commands.
    if text.contains('\x1b') || text.contains('\u{009b}') {
        return Err(InputBlocked::UnsafePaste);
    }
    let mut bytes = Vec::with_capacity(text.len() + 12);
    bytes.extend_from_slice(b"\x1b[200~");
    bytes.extend_from_slice(text.as_bytes());
    bytes.extend_from_slice(b"\x1b[201~");
    Ok(bytes)
}

pub fn encode_focus(state: SessionState, modes: InputModes, focused: bool) -> Vec<u8> {
    if !state.accepts_input() || !modes.focus_reporting {
        return Vec::new();
    }
    if focused { b"\x1b[I" } else { b"\x1b[O" }.to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn function_keys_preserve_legacy_and_kitty_f3_disambiguation() {
        assert_eq!(
            key(
                InputModes::default(),
                Key::Function(1),
                Modifiers::default()
            ),
            b"\x1bOP"
        );
        assert_eq!(
            key(
                InputModes::default(),
                Key::Function(12),
                Modifiers::default()
            ),
            b"\x1b[24~"
        );
        let modes = InputModes {
            kitty: KittyMode {
                disambiguate: true,
                ..KittyMode::default()
            },
            ..InputModes::default()
        };
        assert_eq!(
            key(modes, Key::Function(3), Modifiers::default()),
            b"\x1b[13;1~"
        );
        assert_eq!(
            key(modes, Key::Function(13), Modifiers::default()),
            b"\x1b[57376;1u"
        );
        assert_eq!(
            encode_key(
                SessionState::Connected,
                modes,
                Key::Function(0),
                Modifiers::default(),
                KeyEventKind::Press
            ),
            Err(InputBlocked::UnsupportedKey)
        );
    }

    fn key(modes: InputModes, key: Key, modifiers: Modifiers) -> Vec<u8> {
        encode_key(
            SessionState::Connected,
            modes,
            key,
            modifiers,
            KeyEventKind::Press,
        )
        .unwrap()
    }

    #[test]
    fn legacy_interrupt_alt_unicode_and_cursor_modes() {
        assert_eq!(
            key(
                InputModes::default(),
                Key::Character('c'),
                Modifiers {
                    control: true,
                    ..Modifiers::default()
                }
            ),
            b"\x03"
        );
        assert_eq!(
            key(
                InputModes::default(),
                Key::Character('é'),
                Modifiers {
                    alt: true,
                    ..Modifiers::default()
                }
            ),
            "\x1bé".as_bytes()
        );
        assert_eq!(
            key(
                InputModes {
                    application_cursor: true,
                    ..InputModes::default()
                },
                Key::Up,
                Modifiers::default()
            ),
            b"\x1bOA"
        );
        assert_eq!(
            key(
                InputModes::default(),
                Key::Up,
                Modifiers {
                    control: true,
                    ..Modifiers::default()
                }
            ),
            b"\x1b[1;5A"
        );
        assert_eq!(
            key(
                InputModes::default(),
                Key::Tab,
                Modifiers {
                    shift: true,
                    ..Modifiers::default()
                }
            ),
            b"\x1b[Z"
        );
    }

    #[test]
    fn kitty_disambiguates_shift_enter_and_control_i() {
        let modes = InputModes {
            kitty: KittyMode {
                disambiguate: true,
                ..KittyMode::default()
            },
            ..InputModes::default()
        };
        assert_eq!(
            key(
                modes,
                Key::Enter,
                Modifiers {
                    shift: true,
                    ..Modifiers::default()
                }
            ),
            b"\x1b[13;2u"
        );
        assert_eq!(
            key(
                modes,
                Key::Character('i'),
                Modifiers {
                    control: true,
                    ..Modifiers::default()
                }
            ),
            b"\x1b[105;5u"
        );
        assert_eq!(key(modes, Key::Tab, Modifiers::default()), b"\t");
        assert_eq!(key(modes, Key::Character('x'), Modifiers::default()), b"x");
    }

    #[test]
    fn kitty_reports_repeat_and_release_when_requested() {
        let modes = InputModes {
            kitty: KittyMode {
                report_events: true,
                ..KittyMode::default()
            },
            ..InputModes::default()
        };
        assert_eq!(
            encode_key(
                SessionState::Connected,
                modes,
                Key::Up,
                Modifiers::default(),
                KeyEventKind::Release
            )
            .unwrap(),
            b"\x1b[1;1:3A"
        );
        assert_eq!(
            encode_key(
                SessionState::Connected,
                modes,
                Key::Character('x'),
                Modifiers::default(),
                KeyEventKind::Repeat
            )
            .unwrap(),
            b"x"
        );
        assert_eq!(
            encode_key(
                SessionState::Connected,
                InputModes {
                    kitty: KittyMode {
                        report_all_keys: true,
                        report_events: true,
                        ..KittyMode::default()
                    },
                    ..modes
                },
                Key::Character('x'),
                Modifiers::default(),
                KeyEventKind::Repeat
            )
            .unwrap(),
            b"\x1b[120;1:2u"
        );
        assert!(
            encode_key(
                SessionState::Connected,
                InputModes::default(),
                Key::Enter,
                Modifiers::default(),
                KeyEventKind::Release
            )
            .unwrap()
            .is_empty()
        );
    }

    #[test]
    fn large_paste_is_one_transaction() {
        let text = "line\n".repeat(500);
        let bytes = encode_paste(
            SessionState::Connected,
            InputModes {
                bracketed_paste: true,
                ..InputModes::default()
            },
            &text,
        )
        .unwrap();
        assert!(bytes.starts_with(b"\x1b[200~"));
        assert!(bytes.ends_with(b"\x1b[201~"));
        assert_eq!(&bytes[6..bytes.len() - 6], text.as_bytes());
    }

    #[test]
    fn kitty_retains_plain_controls_and_suppresses_text_releases() {
        let modes = InputModes {
            kitty: KittyMode {
                disambiguate: true,
                report_events: true,
                ..KittyMode::default()
            },
            ..InputModes::default()
        };
        assert_eq!(key(modes, Key::Enter, Modifiers::default()), b"\r");
        assert_eq!(key(modes, Key::Backspace, Modifiers::default()), b"\x7f");
        assert!(
            encode_key(
                SessionState::Connected,
                modes,
                Key::Enter,
                Modifiers::default(),
                KeyEventKind::Release
            )
            .unwrap()
            .is_empty()
        );
        assert!(
            encode_key(
                SessionState::Connected,
                modes,
                Key::Character('x'),
                Modifiers::default(),
                KeyEventKind::Release
            )
            .unwrap()
            .is_empty()
        );
        assert_eq!(
            key(
                modes,
                Key::Character('A'),
                Modifiers {
                    shift: true,
                    control: true,
                    ..Modifiers::default()
                }
            ),
            b"\x1b[97;6:1u"
        );
    }

    #[test]
    fn paste_cannot_escape_its_brackets() {
        assert_eq!(
            encode_paste(
                SessionState::Connected,
                InputModes {
                    bracketed_paste: true,
                    ..InputModes::default()
                },
                "\x1b[201~rm -rf x"
            ),
            Err(InputBlocked::UnsafePaste)
        );
    }

    #[test]
    fn disconnected_input_is_rejected_for_all_entrypoints() {
        assert_eq!(
            encode_text(SessionState::Disconnected, "hello"),
            Err(InputBlocked::NotConnected)
        );
        assert_eq!(
            encode_paste(SessionState::Reconnecting, InputModes::default(), "hello"),
            Err(InputBlocked::NotConnected)
        );
        assert_eq!(
            encode_key(
                SessionState::Closed,
                InputModes::default(),
                Key::Enter,
                Modifiers::default(),
                KeyEventKind::Press
            ),
            Err(InputBlocked::NotConnected)
        );
        assert!(
            encode_focus(
                SessionState::Disconnected,
                InputModes {
                    focus_reporting: true,
                    ..InputModes::default()
                },
                true
            )
            .is_empty()
        );
    }

    #[test]
    fn ime_and_focus_preserve_protocol_boundaries() {
        assert_eq!(
            encode_text(SessionState::Connected, "नमस्ते").unwrap(),
            "नमस्ते".as_bytes()
        );
        assert_eq!(
            encode_focus(
                SessionState::Connected,
                InputModes {
                    focus_reporting: true,
                    ..InputModes::default()
                },
                true
            ),
            b"\x1b[I"
        );
    }
}
