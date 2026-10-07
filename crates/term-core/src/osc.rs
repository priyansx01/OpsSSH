use crate::TerminalEvent;

#[derive(Debug, Default)]
enum State {
    #[default]
    Ground,
    Escape,
    Osc(Vec<u8>),
    OscEscape(Vec<u8>),
    String,
    StringEscape,
}

/// Observes bounded application metadata that Alacritty does not surface.
/// Alacritty remains the only grid/VT parser. Ordinary output is skipped in bulk.
#[derive(Debug, Default)]
pub(crate) struct Observer {
    state: State,
}

impl Observer {
    pub fn ingest(&mut self, bytes: &[u8]) -> Vec<TerminalEvent> {
        let mut events = Vec::new();
        let mut index = 0;
        while index < bytes.len() {
            if matches!(self.state, State::Ground) {
                let Some(offset) = memchr::memchr(0x1b, &bytes[index..]) else {
                    break;
                };
                index += offset + 1;
                self.state = State::Escape;
                continue;
            }
            let byte = bytes[index];
            index += 1;
            self.state = match std::mem::take(&mut self.state) {
                State::Escape => match byte {
                    b']' => State::Osc(Vec::new()),
                    b'P' | b'_' | b'^' | b'X' => State::String,
                    0x1b => State::Escape,
                    _ => State::Ground,
                },
                State::Osc(mut payload) => match byte {
                    7 => {
                        if let Some(event) = decode(&payload) {
                            events.push(event);
                        }
                        State::Ground
                    }
                    0x1b => State::OscEscape(payload),
                    _ if payload.len() < 8192 => {
                        payload.push(byte);
                        State::Osc(payload)
                    }
                    _ => State::String,
                },
                State::OscEscape(payload) => {
                    if byte == b'\\' {
                        if let Some(event) = decode(&payload) {
                            events.push(event);
                        }
                        State::Ground
                    } else {
                        State::Ground
                    }
                }
                State::String => {
                    if byte == 0x1b {
                        State::StringEscape
                    } else {
                        State::String
                    }
                }
                State::StringEscape => {
                    if byte == b'\\' {
                        State::Ground
                    } else {
                        State::String
                    }
                }
                State::Ground => State::Ground,
            };
        }
        events
    }
}

fn decode(payload: &[u8]) -> Option<TerminalEvent> {
    let text = std::str::from_utf8(payload).ok()?;
    let (command, value) = text.split_once(';')?;
    match command {
        "7" => {
            let uri = url::Url::parse(value).ok()?;
            if uri.scheme() != "file"
                || !uri.username().is_empty()
                || uri.password().is_some()
                || uri.port().is_some()
                || uri.query().is_some()
                || uri.fragment().is_some()
            {
                return None;
            }
            let path = percent_encoding::percent_decode_str(uri.path())
                .decode_utf8()
                .ok()?
                .to_string();
            if !path.starts_with('/') || path.chars().any(char::is_control) {
                return None;
            }
            Some(TerminalEvent::CurrentDirectory(path))
        }
        "133" => match value.split(';').next()? {
            "A" => Some(TerminalEvent::PromptStarted),
            "C" => Some(TerminalEvent::CommandStarted),
            "D" => Some(TerminalEvent::CommandFinished),
            _ => None,
        },
        "9" => Some(TerminalEvent::Notification(
            value
                .chars()
                .filter(|ch| !ch.is_control())
                .take(1024)
                .collect(),
        )),
        "777" => value.strip_prefix("notify;").map(|value| {
            TerminalEvent::Notification(
                value
                    .chars()
                    .filter(|ch| !ch.is_control())
                    .take(1024)
                    .collect(),
            )
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fragmented_metadata_never_interprets_dcs_or_unsafe_paths() {
        let mut observer = Observer::default();
        let data=b"hello\x1b]7;file://server/home/a%20b\x1b\\\x1b]133;A\x07\x1bP\x1b]7;file:///wrong\x07\x1b\\\x1b]7;file:///bad%0aname\x07";
        let events: Vec<_> = data
            .iter()
            .flat_map(|byte| observer.ingest(&[*byte]))
            .collect();
        assert_eq!(
            events,
            vec![
                TerminalEvent::CurrentDirectory("/home/a b".into()),
                TerminalEvent::PromptStarted
            ]
        );
    }
    #[test]
    fn oversized_metadata_is_bounded_and_recovers_after_terminator() {
        let mut observer = Observer::default();
        assert!(
            observer
                .ingest(
                    &[
                        b"\x1b]9;".as_slice(),
                        vec![b'x'; 9000].as_slice(),
                        b"\x1b\\"
                    ]
                    .concat()
                )
                .is_empty()
        );
        assert_eq!(
            observer.ingest(b"\x1b]133;C\x07"),
            vec![TerminalEvent::CommandStarted]
        );
    }
}
