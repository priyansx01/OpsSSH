//! Background SSH sessions with strict host verification and explicit prompts.
mod known_hosts;
mod tmux;
mod transport;
use async_channel::{Receiver, Sender};
use opsssh_term_core::TerminalSize;
use std::{fmt, path::PathBuf, time::Duration};
pub use tmux::TmuxOptions;
pub use transport::{AuthenticatedConnection, connect_authenticated};
use zeroize::Zeroizing;
pub type Result<T> = anyhow::Result<T>;
/// A redacted secret, wiped when its final owner is dropped.
#[derive(Clone)]
pub struct SecretString(Zeroizing<String>);
impl SecretString {
    pub fn new(value: impl Into<String>) -> Self {
        Self(Zeroizing::new(value.into()))
    }
    pub fn expose(&self) -> &str {
        &self.0
    }
}
impl fmt::Debug for SecretString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretString([REDACTED])")
    }
}
#[derive(Debug, Clone)]
pub enum AuthMethod {
    PasswordPrompt,
    Password(SecretString),
    PrivateKey {
        path: PathBuf,
        passphrase: Option<SecretString>,
        certificate: Option<PathBuf>,
    },
    Agent {
        identity_agent: Option<String>,
    },
    KeyboardInteractive,
}
/// Exact program and arguments approved by the user. No shell is invoked implicitly.
#[derive(Debug, Clone)]
pub struct ApprovedProxyCommand {
    pub program: String,
    pub arguments: Vec<String>,
    pub approved: bool,
}
#[derive(Debug, Clone)]
pub struct ConnectionOptions {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub known_hosts: PathBuf,
    pub auth: Vec<AuthMethod>,
    pub jumps: Vec<ConnectionOptions>,
    pub proxy: Option<ApprovedProxyCommand>,
    pub tmux: Option<TmuxOptions>,
    pub keepalive: Duration,
    pub connect_timeout: Duration,
    /// On reconnect, unknown host keys are blocked without offering trust.
    pub reconnect: bool,
    /// Require an existing known_hosts entry even on the first connection.
    pub strict_host_key: bool,
    pub credential_id: Option<String>,
}
impl ConnectionOptions {
    pub fn new(host: impl Into<String>, username: impl Into<String>, known_hosts: PathBuf) -> Self {
        Self {
            host: host.into(),
            port: 22,
            username: username.into(),
            known_hosts,
            auth: vec![
                AuthMethod::Agent {
                    identity_agent: None,
                },
                AuthMethod::KeyboardInteractive,
                AuthMethod::PasswordPrompt,
            ],
            jumps: vec![],
            proxy: None,
            tmux: None,
            keepalive: Duration::from_secs(30),
            connect_timeout: Duration::from_secs(15),
            reconnect: false,
            strict_host_key: false,
            credential_id: None,
        }
    }
    pub fn validate(&self) -> Result<()> {
        anyhow::ensure!(
            !self.host.is_empty() && !self.host.chars().any(char::is_control),
            "Invalid SSH hostname"
        );
        anyhow::ensure!(
            self.port > 0 && !self.username.is_empty(),
            "SSH port and username are required"
        );
        anyhow::ensure!(
            self.jumps.len() <= 8,
            "At most eight jump hosts are supported"
        );
        anyhow::ensure!(
            !self.connect_timeout.is_zero(),
            "Connection timeout must be positive"
        );
        if let Some(proxy) = &self.proxy {
            anyhow::ensure!(proxy.approved, "Proxy command requires explicit approval");
            anyhow::ensure!(!proxy.program.is_empty(), "Proxy program is empty");
        }
        for jump in &self.jumps {
            anyhow::ensure!(
                jump.jumps.is_empty(),
                "Nested jump definitions are not supported; flatten the chain"
            );
            jump.validate()?;
        }
        if let Some(tmux) = &self.tmux {
            tmux.validate()?;
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptKind {
    LoginPassword,
    KeyPassphrase,
    KeyboardInteractive,
}
#[derive(Debug, Clone)]
pub struct AuthenticationField {
    pub prompt: String,
    pub echo: bool,
}
#[derive(Debug)]
pub enum SshEvent {
    Authenticated(AuthenticationInfo),
    Data(Vec<u8>),
    Connected,
    Closed {
        exit_status: Option<u32>,
    },
    Error(String),
    VerifiedHost {
        host: String,
        fingerprint: String,
    },
    Reconnecting {
        attempt: u32,
        delay: Duration,
    },
    HostKeyPrompt {
        id: u64,
        host: String,
        fingerprint: String,
    },
    AuthenticationPrompt {
        id: u64,
        kind: PromptKind,
        name: String,
        instructions: String,
        prompts: Vec<AuthenticationField>,
    },
}
pub type SftpStream = russh::ChannelStream<russh::client::Msg>;
/// Cancellation shared by a caller and its independent SSH exec channel.
#[derive(Debug, Clone, Default)]
pub struct ExecControl(std::sync::Arc<std::sync::atomic::AtomicBool>);
impl ExecControl {
    pub fn cancel(&self) {
        self.0.store(true, std::sync::atomic::Ordering::Release);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(std::sync::atomic::Ordering::Acquire)
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthenticationInfo {
    pub username: String,
    pub method: AuthenticationMethod,
    pub fingerprint: Option<String>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthenticationMethod {
    None,
    Password,
    PublicKey,
    Certificate,
    Agent,
    KeyboardInteractive,
}
/// Explicit remote shell command. Debug output never includes the command text.
pub struct ExecRequest {
    pub command: String,
    /// UTF-8 payload sent only on this independent channel and wiped after use.
    pub stdin: Option<SecretString>,
    pub timeout: Duration,
    pub output_limit: usize,
    pub control: ExecControl,
}
impl ExecRequest {
    pub fn new(command: impl Into<String>) -> Self {
        Self {
            command: command.into(),
            stdin: None,
            timeout: Duration::from_secs(5),
            output_limit: 2 * 1024 * 1024,
            control: ExecControl::default(),
        }
    }
}
impl fmt::Debug for ExecRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ExecRequest").finish_non_exhaustive()
    }
}
#[derive(Debug, Default)]
pub struct ExecOutput {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub exit_status: Option<u32>,
}
pub enum SshCommand {
    Write(Vec<u8>),
    Resize(TerminalSize),
    TrustHost {
        id: u64,
        trust: bool,
        persist: bool,
    },
    AuthResponse {
        id: u64,
        responses: Vec<SecretString>,
        save: bool,
    },
    OpenSftp {
        reply: Sender<std::result::Result<SftpStream, String>>,
    },
    Exec {
        request: ExecRequest,
        reply: Sender<std::result::Result<ExecOutput, String>>,
    },
    Disconnect,
}
impl fmt::Debug for SshCommand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Write(_) => "Write([REDACTED])",
            Self::Resize(_) => "Resize",
            Self::TrustHost { .. } => "TrustHost",
            Self::AuthResponse { .. } => "AuthResponse([REDACTED])",
            Self::OpenSftp { .. } => "OpenSftp",
            Self::Exec { .. } => "Exec([REDACTED])",
            Self::Disconnect => "Disconnect",
        })
    }
}
#[derive(Debug)]
pub struct SshSession {
    commands: Sender<SshCommand>,
    events: Receiver<SshEvent>,
}
impl SshSession {
    pub fn spawn(options: ConnectionOptions, size: TerminalSize) -> Result<Self> {
        options.validate()?;
        let (commands, receiver) = async_channel::bounded(128);
        let (sender, events) = async_channel::bounded(128);
        std::thread::Builder::new()
            .name("opsssh-ssh".into())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build();
                let result = match runtime {
                    Ok(runtime) => {
                        runtime.block_on(transport::run(options, size, sender.clone(), receiver))
                    }
                    Err(error) => Err(error.into()),
                };
                if let Err(error) = result {
                    let _ = sender.send_blocking(SshEvent::Error(format!("{error:#}")));
                }
                let _ = sender.send_blocking(SshEvent::Closed { exit_status: None });
            })?;
        Ok(Self { commands, events })
    }
    pub fn commands(&self) -> Sender<SshCommand> {
        self.commands.clone()
    }
    pub fn events(&self) -> Receiver<SshEvent> {
        self.events.clone()
    }
    pub async fn open_sftp(&self) -> std::result::Result<SftpStream, String> {
        let (reply, receive) = async_channel::bounded(1);
        self.commands
            .send(SshCommand::OpenSftp { reply })
            .await
            .map_err(|_| "SSH session closed".to_string())?;
        receive
            .recv()
            .await
            .map_err(|_| "SSH session closed".to_string())?
    }
}
impl Drop for SshSession {
    fn drop(&mut self) {
        let _ = self.commands.try_send(SshCommand::Disconnect);
        self.commands.close();
        self.events.close();
    }
}
