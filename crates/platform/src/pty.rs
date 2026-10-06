use std::io::{self, Read, Write};

use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};

use crate::{LocalShellOptions, LocalTerminal, LocalTerminalFactory, ProcessExit};
use opsssh_term_core::TerminalSize;

#[derive(Debug, Clone, Copy, Default)]
pub struct NativeTerminalFactory;

/// A separate cancellation handle can interrupt blocked PTY I/O.
pub struct TerminalKiller(Box<dyn portable_pty::ChildKiller + Send + Sync>);

impl std::fmt::Debug for TerminalKiller {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TerminalKiller")
    }
}

impl TerminalKiller {
    pub fn terminate(&mut self) -> io::Result<()> {
        self.0.kill()
    }
}

pub struct NativeTerminal {
    child: Box<dyn Child + Send + Sync>,
    master: Box<dyn MasterPty + Send>,
    reader_taken: bool,
}

impl NativeTerminal {
    pub fn killer(&self) -> TerminalKiller {
        TerminalKiller(self.child.clone_killer())
    }
}

impl std::fmt::Debug for NativeTerminal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeTerminal")
            .field("process_id", &self.child.process_id())
            .finish_non_exhaustive()
    }
}

fn size(size: TerminalSize) -> PtySize {
    PtySize {
        rows: size.rows(),
        cols: size.columns().max(2),
        pixel_width: 0,
        pixel_height: 0,
    }
}

impl NativeTerminalFactory {
    /// Used by PTY acceptance tests and development tooling; arguments are passed
    /// as distinct argv entries, never concatenated into a local shell command.
    pub fn spawn_command(
        &self,
        options: LocalShellOptions,
        arguments: &[&str],
    ) -> io::Result<NativeTerminal> {
        let pair = native_pty_system()
            .openpty(size(options.size))
            .map_err(io::Error::other)?;
        let mut command = CommandBuilder::new(&options.executable);
        command.args(arguments);
        command.env("TERM", "xterm-256color");
        command.env("COLORTERM", "truecolor");
        if let Some(directory) = options.working_directory {
            command.cwd(directory);
        }
        let child = pair
            .slave
            .spawn_command(command)
            .map_err(io::Error::other)?;
        // No slave handle is retained; otherwise EOF may never reach the reader.
        drop(pair.slave);
        Ok(NativeTerminal {
            child,
            master: pair.master,
            reader_taken: false,
        })
    }
}

impl LocalTerminalFactory for NativeTerminalFactory {
    type Terminal = NativeTerminal;

    fn spawn(&self, options: LocalShellOptions) -> io::Result<Self::Terminal> {
        self.spawn_command(options, &[])
    }
}

impl LocalTerminal for NativeTerminal {
    fn reader(&mut self) -> io::Result<Box<dyn Read + Send>> {
        if self.reader_taken {
            return Err(io::Error::other("PTY reader already taken"));
        }
        let reader = self.master.try_clone_reader().map_err(io::Error::other)?;
        self.reader_taken = true;
        Ok(reader)
    }

    fn writer(&mut self) -> io::Result<Box<dyn Write + Send>> {
        self.master.take_writer().map_err(io::Error::other)
    }

    fn resize(&mut self, dimensions: TerminalSize) -> io::Result<()> {
        self.master
            .resize(size(dimensions))
            .map_err(io::Error::other)
    }

    fn try_wait(&mut self) -> io::Result<Option<ProcessExit>> {
        self.child.try_wait().map(|status| {
            status.map(|status| ProcessExit {
                code: status.signal().is_none().then_some(status.exit_code()),
            })
        })
    }

    fn terminate(&mut self) -> io::Result<()> {
        if self.child.try_wait()?.is_none() {
            self.child.kill()?;
        }
        self.child.wait()?;
        Ok(())
    }
}

impl Drop for NativeTerminal {
    fn drop(&mut self) {
        // Cleanup also runs when setup, window construction, or worker I/O fails.
        let _ = self.terminate();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    fn command_options() -> (LocalShellOptions, Vec<&'static str>) {
        #[cfg(target_os = "windows")]
        {
            (
                LocalShellOptions {
                    executable: "cmd.exe".into(),
                    ..LocalShellOptions::default()
                },
                vec!["/d", "/c", "echo OPSSSH_PTY_OK"],
            )
        }
        #[cfg(not(target_os = "windows"))]
        {
            (
                LocalShellOptions {
                    executable: "/bin/sh".into(),
                    ..LocalShellOptions::default()
                },
                vec!["-c", "printf OPSSSH_PTY_OK"],
            )
        }
    }

    #[test]
    fn real_pty_emits_output_and_reports_exit() {
        let (options, arguments) = command_options();
        let mut terminal = NativeTerminalFactory
            .spawn_command(options, &arguments)
            .unwrap();
        let mut reader = terminal.reader().unwrap();
        let mut writer = terminal.writer().unwrap();
        assert!(terminal.reader().is_err());
        let (sender, receiver) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            use opsssh_term_core::{AlacrittyTerminal, TerminalBackend, TerminalEvent};
            let mut emulator = AlacrittyTerminal::new(TerminalSize::new(80, 24).unwrap());
            let mut output = Vec::new();
            let mut buffer = [0; 1024];
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(count) => {
                        for event in emulator.ingest(&buffer[..count]).unwrap() {
                            if let TerminalEvent::WriteToTransport(bytes) = event {
                                writer.write_all(&bytes).unwrap();
                                writer.flush().unwrap();
                            }
                        }
                        output.extend_from_slice(&buffer[..count]);
                        if String::from_utf8_lossy(&output).contains("OPSSSH_PTY_OK") {
                            break;
                        }
                    }
                }
            }
            let _ = sender.send(output);
        });
        let output = receiver.recv_timeout(Duration::from_secs(10)).unwrap();
        assert!(String::from_utf8_lossy(&output).contains("OPSSSH_PTY_OK"));
        let deadline = Instant::now() + Duration::from_secs(5);
        while terminal.try_wait().unwrap().is_none() {
            assert!(Instant::now() < deadline, "PTY child failed to exit");
            std::thread::sleep(Duration::from_millis(10));
        }
        worker.join().unwrap();
    }

    #[test]
    fn resize_and_terminate_live_shell() {
        let mut terminal = NativeTerminalFactory
            .spawn(LocalShellOptions::default())
            .unwrap();
        terminal
            .resize(TerminalSize::new(120, 40).unwrap())
            .unwrap();
        assert_eq!(terminal.master.get_size().unwrap().cols, 120);
        terminal.terminate().unwrap();
        assert!(terminal.try_wait().unwrap().is_some());
    }
}
