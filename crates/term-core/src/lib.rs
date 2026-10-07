//! Terminal contracts shared by local PTY and future SSH sessions.
//!
//! Alacritty provides the VT emulator; rendering and OS side effects stay outside
//! this crate. Protocol and replay tests require no GPU.

mod emulator;
mod input;
mod osc;
mod snapshot;

pub use emulator::AlacrittyTerminal;
pub use input::*;
pub use snapshot::*;

use std::fmt;

/// A valid terminal size. Zero-sized surfaces never reach a terminal backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalSize {
    columns: u16,
    rows: u16,
}

impl TerminalSize {
    pub fn new(columns: u16, rows: u16) -> Result<Self, InvalidSize> {
        if columns == 0 || rows == 0 {
            return Err(InvalidSize);
        }
        Ok(Self { columns, rows })
    }

    pub fn columns(self) -> u16 {
        self.columns
    }

    pub fn rows(self) -> u16 {
        self.rows
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidSize;

impl fmt::Display for InvalidSize {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("terminal dimensions must be nonzero")
    }
}

impl std::error::Error for InvalidSize {}

/// Side effects are delivered to the application, never executed by a parser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalEvent {
    WriteToTransport(Vec<u8>),
    SetClipboard(String),
    Title(String),
    Bell,
    CurrentDirectory(String),
    PromptStarted,
    CommandStarted,
    CommandFinished,
    Notification(String),
    SynchronizedOutput(bool),
}

/// Implemented by the future Alacritty adapter, independent of the UI framework.
pub trait TerminalBackend: Send {
    type Error: std::error::Error + Send + Sync + 'static;

    fn ingest(&mut self, bytes: &[u8]) -> Result<Vec<TerminalEvent>, Self::Error>;
    fn resize(&mut self, size: TerminalSize) -> Result<(), Self::Error>;
    fn modes(&self) -> InputModes;
    fn snapshot(&mut self) -> TerminalSnapshot;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    Connecting,
    Authenticating,
    Connected,
    Disconnected,
    Reconnecting,
    NeedsUserAction,
    Closed,
}

impl SessionState {
    /// No input buffer exists: rejected input can never be replayed on reconnect.
    pub fn accepts_input(self) -> bool {
        self == Self::Connected
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_dimensions_are_rejected() {
        assert!(TerminalSize::new(0, 24).is_err());
        assert!(TerminalSize::new(80, 0).is_err());
        assert_eq!(TerminalSize::new(80, 24).unwrap().columns(), 80);
    }

    #[test]
    fn only_connected_sessions_accept_input() {
        for state in [
            SessionState::Connecting,
            SessionState::Authenticating,
            SessionState::Disconnected,
            SessionState::Reconnecting,
            SessionState::NeedsUserAction,
            SessionState::Closed,
        ] {
            assert!(!state.accepts_input());
        }
        assert!(SessionState::Connected.accepts_input());
    }
}
