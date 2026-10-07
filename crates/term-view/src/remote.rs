use std::io;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use async_channel::{Receiver, Sender};
use opsssh_platform::LocalShellOptions;
use opsssh_ssh_core::{ConnectionOptions, SshCommand, SshEvent, SshSession};
use opsssh_term_core::{
    AlacrittyTerminal, InputModes, SessionState, TerminalBackend, TerminalEvent, TerminalSize,
    TerminalSnapshot,
};

use crate::{LocalSession, SessionNotice};

#[derive(Debug)]
pub(crate) struct RemoteSession {
    backend: Arc<Mutex<AlacrittyTerminal>>,
    state: Arc<Mutex<SessionState>>,
    ssh: SshSession,
    notices: Receiver<SessionNotice>,
}

impl RemoteSession {
    fn spawn(options: ConnectionOptions, size: TerminalSize) -> io::Result<Self> {
        let ssh = SshSession::spawn(options, size)
            .map_err(|error| io::Error::other(error.to_string()))?;
        let events = ssh.events();
        let commands = ssh.commands();
        let backend = Arc::new(Mutex::new(AlacrittyTerminal::new(size)));
        let state = Arc::new(Mutex::new(SessionState::Connecting));
        let (updates, notices) = async_channel::bounded(64);
        let reader_backend = backend.clone();
        let reader_state = state.clone();
        std::thread::Builder::new()
            .name("opsssh-ssh-parser".into())
            .spawn(move || {
                while let Ok(event) = events.recv_blocking() {
                    match event {
                        SshEvent::Data(bytes) => {
                            let terminal_events = reader_backend
                                .lock()
                                .unwrap_or_else(|error| error.into_inner())
                                .ingest(&bytes)
                                .unwrap();
                            for event in terminal_events {
                                if let TerminalEvent::WriteToTransport(bytes) = event {
                                    if commands.send_blocking(SshCommand::Write(bytes)).is_err() {
                                        return;
                                    }
                                } else if updates
                                    .send_blocking(SessionNotice::Event(event))
                                    .is_err()
                                {
                                    return;
                                }
                            }
                            let _ = updates.try_send(SessionNotice::Updated);
                        }
                        event => {
                            let next = match &event {
                                SshEvent::Connected => Some(SessionState::Connected),
                                SshEvent::Reconnecting { .. } => Some(SessionState::Reconnecting),
                                SshEvent::Closed { .. } => Some(SessionState::Closed),
                                SshEvent::Error(_) => Some(SessionState::NeedsUserAction),
                                SshEvent::HostKeyPrompt { .. }
                                | SshEvent::AuthenticationPrompt { .. } => {
                                    Some(SessionState::NeedsUserAction)
                                }
                                _ => None,
                            };
                            if let Some(next) = next {
                                *reader_state
                                    .lock()
                                    .unwrap_or_else(|error| error.into_inner()) = next;
                            }
                            if updates.send_blocking(SessionNotice::Ssh(event)).is_err() {
                                break;
                            }
                        }
                    }
                }
                *reader_state
                    .lock()
                    .unwrap_or_else(|error| error.into_inner()) = SessionState::Closed;
                let _ = updates.try_send(SessionNotice::Closed);
            })?;
        Ok(Self {
            backend,
            state,
            ssh,
            notices,
        })
    }
}

impl Drop for RemoteSession {
    fn drop(&mut self) {
        self.notices.close();
    }
}

#[derive(Debug)]
pub(crate) enum Session {
    Local(LocalSession),
    Remote(RemoteSession),
}

impl Session {
    pub fn local(options: LocalShellOptions) -> io::Result<Self> {
        LocalSession::spawn(options).map(Self::Local)
    }
    pub fn remote(options: ConnectionOptions, size: TerminalSize) -> io::Result<Self> {
        RemoteSession::spawn(options, size).map(Self::Remote)
    }
    pub fn backend(&self) -> &Arc<Mutex<AlacrittyTerminal>> {
        match self {
            Self::Local(session) => &session.backend,
            Self::Remote(session) => &session.backend,
        }
    }
    pub fn state(&self) -> SessionState {
        match self {
            Self::Local(session) => session.state(),
            Self::Remote(session) => *session
                .state
                .lock()
                .unwrap_or_else(|error| error.into_inner()),
        }
    }
    pub fn notices(&self) -> Receiver<SessionNotice> {
        match self {
            Self::Local(session) => session.notices(),
            Self::Remote(session) => session.notices.clone(),
        }
    }
    pub fn ssh_commands(&self) -> Option<Sender<SshCommand>> {
        match self {
            Self::Remote(session) => Some(session.ssh.commands()),
            _ => None,
        }
    }
    pub fn modes(&self) -> InputModes {
        self.backend()
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .modes()
    }
    pub fn mouse_modes(&self) -> (bool, bool, bool, bool) {
        self.backend()
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .mouse_modes()
    }
    pub fn snapshot(&self) -> TerminalSnapshot {
        self.backend()
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .snapshot()
    }
    pub fn sync_deadline(&self) -> Option<Instant> {
        self.backend()
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .sync_deadline()
    }
    pub fn expire_sync(&self, now: Instant) -> Vec<TerminalEvent> {
        self.backend()
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .expire_sync(now)
    }
    pub fn scroll(&self, lines: i32) {
        self.backend()
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .scroll(lines);
    }
    pub fn scroll_to_bottom(&self) {
        self.backend()
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .scroll_to_bottom();
    }
    pub fn write(&self, bytes: Vec<u8>) -> io::Result<()> {
        if bytes.is_empty() {
            return Ok(());
        }
        if !self.state().accepts_input() {
            return Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "Session is not connected",
            ));
        }
        match self {
            Self::Local(session) => session.write(bytes),
            Self::Remote(session) => session
                .ssh
                .commands()
                .try_send(SshCommand::Write(bytes))
                .map_err(|error| io::Error::new(io::ErrorKind::WouldBlock, error.to_string())),
        }
    }
    pub fn resize(&self, size: TerminalSize) -> io::Result<()> {
        match self {
            Self::Local(session) => session.resize(size),
            Self::Remote(session) => {
                session
                    .ssh
                    .commands()
                    .try_send(SshCommand::Resize(size))
                    .map_err(|error| {
                        io::Error::new(io::ErrorKind::WouldBlock, error.to_string())
                    })?;
                session
                    .backend
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .resize(size)
                    .unwrap();
                Ok(())
            }
        }
    }
}
