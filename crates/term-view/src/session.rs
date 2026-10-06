use std::io::{self, Read, Write};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use opsssh_platform::{
    LocalShellOptions, LocalTerminal, LocalTerminalFactory, NativeTerminalFactory, TerminalKiller,
};
use opsssh_term_core::{
    AlacrittyTerminal, InputModes, SessionState, TerminalBackend, TerminalEvent, TerminalSize,
    TerminalSnapshot,
};

#[derive(Debug, Clone)]
pub enum SessionNotice {
    Updated,
    Event(TerminalEvent),
    Closed,
    Error(String),
}

enum Command {
    Write(Vec<u8>),
    Resize(TerminalSize),
}

/// Dedicated PTY/parser and writer/control workers. Byte output is never dropped;
/// only redundant UI wakeups are coalesced. No I/O runs on GPUI's main thread.
pub struct LocalSession {
    pub(crate) backend: Arc<Mutex<AlacrittyTerminal>>,
    state: Arc<Mutex<SessionState>>,
    sender: mpsc::SyncSender<Command>,
    notices: async_channel::Receiver<SessionNotice>,
    stop: Arc<AtomicBool>,
    killer: Arc<Mutex<TerminalKiller>>,
    workers: Vec<JoinHandle<()>>,
}

impl std::fmt::Debug for LocalSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalSession")
            .field("state", &self.state())
            .finish_non_exhaustive()
    }
}

impl LocalSession {
    /// Call on a background executor: process creation can block.
    pub fn spawn(options: LocalShellOptions) -> io::Result<Self> {
        let dimensions = options.size;
        let mut terminal = NativeTerminalFactory.spawn(options)?;
        let killer = Arc::new(Mutex::new(terminal.killer()));
        let mut reader = terminal.reader()?;
        let mut writer = terminal.writer()?;
        let backend = Arc::new(Mutex::new(AlacrittyTerminal::new(dimensions)));
        let state = Arc::new(Mutex::new(SessionState::Connected));
        let stop = Arc::new(AtomicBool::new(false));
        let (sender, commands) = mpsc::sync_channel(128);
        let (updates, notices) = async_channel::bounded(64);

        let reader_backend = backend.clone();
        let reader_sender = sender.clone();
        let reader_stop = stop.clone();
        let reader_updates = updates.clone();
        let reader_worker = thread::Builder::new()
            .name("opsssh-pty-reader".into())
            .spawn(move || {
                let mut bytes = [0; 16 * 1024];
                loop {
                    match reader.read(&mut bytes) {
                        Ok(0) => break,
                        Ok(count) => {
                            // Drain shutdown output so ClosePseudoConsole cannot
                            // deadlock on a full output pipe.
                            if reader_stop.load(Ordering::Acquire) {
                                continue;
                            }
                            let events = reader_backend
                                .lock()
                                .unwrap_or_else(|error| error.into_inner())
                                .ingest(&bytes[..count])
                                .unwrap();
                            for event in events {
                                match event {
                                    TerminalEvent::WriteToTransport(bytes) => {
                                        if reader_sender.send(Command::Write(bytes)).is_err() {
                                            break;
                                        }
                                    }
                                    event => {
                                        if reader_updates
                                            .send_blocking(SessionNotice::Event(event))
                                            .is_err()
                                        {
                                            break;
                                        }
                                    }
                                }
                            }
                            let _ = reader_updates.try_send(SessionNotice::Updated);
                        }
                        Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                        Err(error) => {
                            if !reader_stop.load(Ordering::Acquire) {
                                let _ = reader_updates
                                    .try_send(SessionNotice::Error(error.to_string()));
                            }
                            break;
                        }
                    }
                }
                reader_stop.store(true, Ordering::Release);
            })?;

        let writer_state = state.clone();
        let writer_stop = stop.clone();
        let writer_backend = backend.clone();
        let writer_worker = thread::Builder::new()
            .name("opsssh-pty-writer".into())
            .spawn(move || {
                let result: io::Result<()> = (|| {
                    while !writer_stop.load(Ordering::Acquire) {
                        match commands.recv_timeout(Duration::from_millis(100)) {
                            Ok(Command::Write(bytes)) => {
                                writer.write_all(&bytes)?;
                                writer.flush()?;
                            }
                            Ok(Command::Resize(size)) => {
                                terminal.resize(size)?;
                                writer_backend
                                    .lock()
                                    .unwrap_or_else(|error| error.into_inner())
                                    .resize(size)
                                    .unwrap();
                                let _ = updates.try_send(SessionNotice::Updated);
                            }
                            Err(mpsc::RecvTimeoutError::Disconnected) => break,
                            Err(mpsc::RecvTimeoutError::Timeout) => {}
                        }
                        if terminal.try_wait()?.is_some() {
                            break;
                        }
                    }
                    Ok(())
                })();
                *writer_state
                    .lock()
                    .unwrap_or_else(|error| error.into_inner()) = SessionState::Closed;
                writer_stop.store(true, Ordering::Release);
                if let Err(error) = result {
                    let _ = updates.try_send(SessionNotice::Error(error.to_string()));
                }
                let _ = updates.send_blocking(SessionNotice::Closed);
                drop(writer);
                // Dropping the PTY closes the pseudoconsole and unblocks its reader.
                let _ = terminal.terminate();
                drop(terminal);
            });
        let writer_worker = match writer_worker {
            Ok(worker) => worker,
            Err(error) => {
                stop.store(true, Ordering::Release);
                notices.close();
                // The failed spawn drops its closure, including the PTY master.
                let _ = reader_worker.join();
                return Err(error);
            }
        };
        Ok(Self {
            backend,
            state,
            sender,
            notices,
            stop,
            killer,
            workers: vec![reader_worker, writer_worker],
        })
    }

    pub fn state(&self) -> SessionState {
        *self.state.lock().unwrap_or_else(|error| error.into_inner())
    }

    pub fn notices(&self) -> async_channel::Receiver<SessionNotice> {
        self.notices.clone()
    }

    pub fn modes(&self) -> InputModes {
        self.backend
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .modes()
    }

    pub fn snapshot(&self) -> TerminalSnapshot {
        self.backend
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .snapshot()
    }

    pub fn sync_deadline(&self) -> Option<Instant> {
        self.backend
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .sync_deadline()
    }

    pub fn expire_sync(&self, now: Instant) -> Vec<TerminalEvent> {
        self.backend
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .expire_sync(now)
    }

    pub fn write(&self, bytes: Vec<u8>) -> io::Result<()> {
        if bytes.is_empty() {
            return Ok(());
        }
        if !self.state().accepts_input() {
            return Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "session is closed",
            ));
        }
        self.sender
            .try_send(Command::Write(bytes))
            .map_err(|error| io::Error::new(io::ErrorKind::WouldBlock, error.to_string()))
    }

    pub fn resize(&self, size: TerminalSize) -> io::Result<()> {
        self.sender
            .try_send(Command::Resize(size))
            .map_err(|error| io::Error::new(io::ErrorKind::WouldBlock, error.to_string()))
    }

    pub fn scroll(&self, lines: i32) {
        self.backend
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .scroll(lines);
    }

    /// Blocking shutdown for tests/tools; a view should simply drop the session.
    pub fn shutdown(mut self) {
        self.stop.store(true, Ordering::Release);
        self.notices.close();
        let _ = self
            .killer
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .terminate();
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

impl Drop for LocalSession {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.notices.close();
        // Never wait for process teardown on the UI thread. The cleanup thread
        // owns the join handles; workers retain their state until fully stopped.
        let workers = std::mem::take(&mut self.workers);
        let killer = self.killer.clone();
        if !workers.is_empty() {
            let _ = thread::Builder::new()
                .name("opsssh-pty-cleanup".into())
                .spawn(move || {
                    let _ = killer
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .terminate();
                    for worker in workers {
                        let _ = worker.join();
                    }
                });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_round_trip_resize_and_shutdown() {
        let session = LocalSession::spawn(LocalShellOptions::default()).unwrap();
        let notices = session.notices();
        session.resize(TerminalSize::new(100, 30).unwrap()).unwrap();
        let command = if opsssh_platform::info().os == "windows" {
            "Write-Output ('OPSSSH_' + 'SESSION_OK')\r"
        } else {
            "printf 'OPSSSH_%s\\n' SESSION_OK\r"
        };
        session.write(command.as_bytes().to_vec()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut found = false;
        while Instant::now() < deadline {
            while notices.try_recv().is_ok() {}
            let snapshot = session.snapshot();
            let text: String = snapshot
                .rows
                .iter()
                .flat_map(|row| row.cells.iter())
                .map(|cell| cell.text.as_str())
                .collect();
            if text.contains("OPSSSH_SESSION_OK") {
                found = true;
                assert_eq!(snapshot.rows.len(), 30);
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        session.shutdown();
        assert!(found, "interactive PTY round trip did not complete");
    }
}
