//! One OpsSSH window per user session. A later launch (for example from the File Explorer
//! context menu) hands its request to the running window and exits.
use std::path::{Path, PathBuf};

/// What a launch asks the OpsSSH window to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    /// Show the server list.
    Home,
    /// Open a local terminal, optionally in an existing folder.
    Terminal(Option<PathBuf>),
}

#[derive(Debug)]
pub enum Instance {
    /// Another OpsSSH window took the request; this process should exit.
    Delivered,
    /// This process is the OpsSSH window; later launches arrive on the receiver.
    Primary(async_channel::Receiver<Request>),
}

/// Messages are single short lines; anything larger is refused unread.
const MAX_MESSAGE: usize = 4096;

impl Request {
    fn encode(&self) -> String {
        match self {
            Self::Home => "home\n".into(),
            Self::Terminal(None) => "terminal\n".into(),
            Self::Terminal(Some(path)) => format!("terminal\t{}\n", path.display()),
        }
    }

    /// Strict decoding: unknown requests are refused, and a folder that is not an existing
    /// absolute directory is dropped so the terminal opens in the home folder instead.
    fn decode(line: &str) -> Option<Self> {
        let line = line.strip_suffix('\n').unwrap_or(line);
        match line.split_once('\t') {
            None if line == "home" => Some(Self::Home),
            None if line == "terminal" => Some(Self::Terminal(None)),
            Some(("terminal", path)) => Some(Self::Terminal(usable_directory(Path::new(path)))),
            _ => None,
        }
    }
}

fn usable_directory(path: &Path) -> Option<PathBuf> {
    (path.is_absolute() && path.is_dir()).then(|| path.to_path_buf())
}

/// Deliver `request` to a running OpsSSH window, or become that window.
pub fn hand_off_or_listen(request: Request) -> Instance {
    native::hand_off_or_listen(request, &native::pipe_name())
}

#[cfg(target_os = "windows")]
mod native {
    use super::{Instance, MAX_MESSAGE, Request};
    use std::{io, time::Duration};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::windows::named_pipe::{ClientOptions, NamedPipeServer, ServerOptions},
    };

    /// All pipe instances are busy for a moment while the window accepts another launch.
    const ERROR_PIPE_BUSY: i32 = 231;

    /// Per user and per Windows session, so other users and sessions never share a window.
    pub fn pipe_name() -> String {
        let user: String = std::env::var("USERNAME")
            .unwrap_or_default()
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
            .take(64)
            .collect();
        format!(r"\\.\pipe\opsssh-{}-{user}", session_id())
    }

    #[allow(unsafe_code)]
    fn session_id() -> u32 {
        let mut session = 0u32;
        // SAFETY: `session` is a valid out-pointer for the duration of the call.
        let ok = unsafe {
            windows_sys::Win32::System::RemoteDesktop::ProcessIdToSessionId(
                std::process::id(),
                &mut session,
            )
        };
        if ok == 0 { 0 } else { session }
    }

    /// Let the running window take the foreground; Windows only allows that when the
    /// process the user just started grants it.
    #[allow(unsafe_code)]
    fn allow_foreground() {
        // SAFETY: takes no pointers; ASFW_ANY grants the right to whichever process asks.
        unsafe {
            windows_sys::Win32::UI::WindowsAndMessaging::AllowSetForegroundWindow(
                windows_sys::Win32::UI::WindowsAndMessaging::ASFW_ANY,
            );
        }
    }

    pub fn hand_off_or_listen(request: Request, name: &str) -> Instance {
        // A second attempt covers another launch becoming the window at the same moment.
        for _ in 0..2 {
            if deliver(&request, name).is_ok() {
                return Instance::Delivered;
            }
            if let Ok(receiver) = listen(name) {
                return Instance::Primary(receiver);
            }
        }
        // Neither: run on our own rather than refuse to start.
        Instance::Primary(async_channel::bounded(1).1)
    }

    fn runtime() -> io::Result<tokio::runtime::Runtime> {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
    }

    pub(super) fn deliver(request: &Request, name: &str) -> io::Result<()> {
        runtime()?.block_on(async {
            let mut waited = Duration::ZERO;
            let mut client = loop {
                match ClientOptions::new().open(name) {
                    Ok(client) => break client,
                    Err(error)
                        if error.raw_os_error() == Some(ERROR_PIPE_BUSY)
                            && waited < Duration::from_secs(2) =>
                    {
                        tokio::time::sleep(Duration::from_millis(50)).await;
                        waited += Duration::from_millis(50);
                    }
                    Err(error) => return Err(error),
                }
            };
            allow_foreground();
            client.write_all(request.encode().as_bytes()).await?;
            let mut reply = [0u8; 2];
            tokio::time::timeout(Duration::from_secs(3), client.read_exact(&mut reply))
                .await
                .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "no reply from OpsSSH"))??;
            if &reply == b"ok" {
                Ok(())
            } else {
                Err(io::Error::other("OpsSSH refused the request"))
            }
        })
    }

    pub(super) fn listen(name: &str) -> io::Result<async_channel::Receiver<Request>> {
        let runtime = runtime()?;
        // Create the first instance now, so a launch racing us is detected before we return.
        let first = {
            let _guard = runtime.enter();
            ServerOptions::new()
                .first_pipe_instance(true)
                .reject_remote_clients(true)
                .create(name)?
        };
        let (sender, receiver) = async_channel::bounded(16);
        let name = name.to_owned();
        std::thread::Builder::new()
            .name("opsssh-instance".into())
            .spawn(move || runtime.block_on(serve(first, name, sender)))?;
        Ok(receiver)
    }

    async fn serve(
        mut server: NamedPipeServer,
        name: String,
        sender: async_channel::Sender<Request>,
    ) {
        loop {
            if server.connect().await.is_err() {
                match ServerOptions::new()
                    .reject_remote_clients(true)
                    .create(&name)
                {
                    Ok(next) => {
                        server = next;
                        continue;
                    }
                    Err(_) => return,
                }
            }
            // Open the next instance before handling this one, so launches are never refused.
            let next = ServerOptions::new()
                .reject_remote_clients(true)
                .create(&name);
            let mut connected = server;
            let accepted = match read_line(&mut connected)
                .await
                .and_then(|line| Request::decode(&line))
            {
                Some(request) => sender.send(request).await.is_ok(),
                None => false,
            };
            let _ = connected
                .write_all(if accepted { b"ok" } else { b"no" })
                .await;
            let _ = connected.flush().await;
            let _ = connected.disconnect();
            if sender.is_closed() {
                return;
            }
            match next {
                Ok(next) => server = next,
                Err(_) => return,
            }
        }
    }

    async fn read_line(pipe: &mut NamedPipeServer) -> Option<String> {
        let mut buffer = Vec::new();
        let mut chunk = [0u8; 512];
        let read = async {
            loop {
                let count = pipe.read(&mut chunk).await.ok()?;
                if count == 0 {
                    return None;
                }
                buffer.extend_from_slice(&chunk[..count]);
                if buffer.len() > MAX_MESSAGE {
                    return None;
                }
                if buffer.contains(&b'\n') {
                    return String::from_utf8(buffer).ok();
                }
            }
        };
        tokio::time::timeout(Duration::from_secs(2), read)
            .await
            .ok()?
    }
}

#[cfg(not(target_os = "windows"))]
mod native {
    use super::{Instance, Request};

    pub fn pipe_name() -> String {
        String::new()
    }

    /// Other platforms keep one process per launch; the receiver never yields.
    pub fn hand_off_or_listen(_: Request, _: &str) -> Instance {
        Instance::Primary(async_channel::bounded(1).1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_round_trip_and_unknown_or_unsafe_ones_are_refused() {
        let folder = std::env::temp_dir();
        for request in [
            Request::Home,
            Request::Terminal(None),
            Request::Terminal(Some(folder.clone())),
        ] {
            assert_eq!(Request::decode(&request.encode()), Some(request));
        }
        assert_eq!(Request::decode("format c:\n"), None);
        assert_eq!(Request::decode("home\textra\n"), None);
        // A folder that is relative or missing is dropped, not trusted.
        assert_eq!(
            Request::decode("terminal\trelative\\path\n"),
            Some(Request::Terminal(None))
        );
        let missing = folder.join("opsssh-missing-folder-for-test");
        assert_eq!(
            Request::decode(&format!("terminal\t{}\n", missing.display())),
            Some(Request::Terminal(None))
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn a_second_launch_hands_its_request_to_the_first() {
        let name = format!(r"\\.\pipe\opsssh-test-{}", std::process::id());
        let Instance::Primary(requests) = native::hand_off_or_listen(Request::Home, &name) else {
            panic!("the first launch becomes the window");
        };
        let folder = std::env::temp_dir();
        let second = native::hand_off_or_listen(Request::Terminal(Some(folder.clone())), &name);
        assert!(matches!(second, Instance::Delivered));
        assert_eq!(
            requests.recv_blocking(),
            Ok(Request::Terminal(Some(folder)))
        );
        // The window keeps listening for further launches.
        native::deliver(&Request::Home, &name).unwrap();
        assert_eq!(requests.recv_blocking(), Ok(Request::Home));
    }
}
