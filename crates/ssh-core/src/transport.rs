use crate::known_hosts::{self, HostStatus};
use crate::*;
use anyhow::{Context, bail};
use russh::{
    ChannelMsg, client,
    keys::{HashAlg, PrivateKeyWithHashAlg, PublicKeyOrCertificate, agent::AgentIdentity},
};
use std::{
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    task::{Context as TaskContext, Poll},
};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
static PROMPT_ID: AtomicU64 = AtomicU64::new(1);
#[derive(Debug, Default)]
struct SessionMemory {
    trusted: std::collections::HashMap<(String, u16), String>,
    passwords: std::collections::HashMap<(String, u16, String), SecretString>,
}
type SharedMemory = Arc<Mutex<SessionMemory>>;
fn credential_key(options: &ConnectionOptions) -> (String, u16, String) {
    (options.host.clone(), options.port, options.username.clone())
}
#[derive(Debug, Clone)]
pub struct HostHandler {
    options: ConnectionOptions,
    events: Sender<SshEvent>,
    commands: Receiver<SshCommand>,
    prompt_active: Arc<AtomicBool>,
    memory: SharedMemory,
}
impl client::Handler for HostHandler {
    type Error = anyhow::Error;
    async fn check_server_key(&mut self, key: &PublicKeyOrCertificate) -> Result<bool> {
        let fingerprint = key.public_key().fingerprint(HashAlg::Sha256).to_string();
        let endpoint = (self.options.host.clone(), self.options.port);
        let cached = self
            .memory
            .lock()
            .expect("SSH session memory poisoned")
            .trusted
            .get(&endpoint)
            == Some(&fingerprint);
        match known_hosts::check(
            &self.options.known_hosts,
            &self.options.host,
            self.options.port,
            key,
        )? {
            HostStatus::Trusted => {}
            HostStatus::Unknown
                if self.options.reconnect
                    && !self.options.strict_host_key
                    && cached
                    && key.certificate().is_none() => {}
            HostStatus::Unknown => {
                anyhow::ensure!(
                    !self.options.reconnect && !self.options.strict_host_key,
                    "Unknown host key; strict verification requires a trusted known_hosts entry"
                );
                anyhow::ensure!(
                    key.certificate().is_none(),
                    "Unknown host certificate authority; add a verified @cert-authority entry first"
                );
                let _guard = PromptGuard::new(self.prompt_active.clone());
                let id = PROMPT_ID.fetch_add(1, Ordering::Relaxed);
                self.events
                    .send(SshEvent::HostKeyPrompt {
                        id,
                        host: format!("{}:{}", self.options.host, self.options.port),
                        fingerprint: key.public_key().fingerprint(HashAlg::Sha256).to_string(),
                    })
                    .await?;
                loop {
                    match self.commands.recv().await? {
                        SshCommand::TrustHost {
                            id: reply,
                            trust,
                            persist,
                        } if reply == id => {
                            if !trust {
                                return Ok(false);
                            }
                            if persist {
                                known_hosts::remember(
                                    &self.options.known_hosts,
                                    &self.options.host,
                                    self.options.port,
                                    &key.public_key(),
                                )?;
                            }
                            break;
                        }
                        SshCommand::Disconnect => bail!("Connection cancelled"),
                        SshCommand::OpenSftp { reply } => {
                            let _ = reply
                                .send(Err("SSH authentication is not complete".into()))
                                .await;
                        }
                        _ => {}
                    }
                }
            }
        }
        self.memory
            .lock()
            .expect("SSH session memory poisoned")
            .trusted
            .insert(endpoint, fingerprint);
        self.events
            .send(SshEvent::VerifiedHost {
                host: self.options.host.clone(),
                fingerprint: key.public_key().fingerprint(HashAlg::Sha256).to_string(),
            })
            .await?;
        Ok(true)
    }
}
pub struct AuthenticatedConnection {
    handle: client::Handle<HostHandler>,
    hops: Vec<client::Handle<HostHandler>>,
    _proxy: Option<tokio::process::Child>,
}
impl fmt::Debug for AuthenticatedConnection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AuthenticatedConnection")
            .field("hops", &self.hops.len())
            .finish_non_exhaustive()
    }
}
impl AuthenticatedConnection {
    pub async fn open_sftp(&self) -> Result<SftpStream> {
        let mut channel = self.handle.channel_open_session().await?;
        channel.request_subsystem(true, "sftp").await?;
        wait_success(&mut channel)
            .await
            .context("Server has no working SFTP subsystem")?;
        Ok(channel.into_stream())
    }
    pub async fn disconnect(&self) -> Result<()> {
        self.handle
            .disconnect(russh::Disconnect::ByApplication, "Session closed", "en")
            .await?;
        Ok(())
    }
    /// Executes an explicitly requested command with an output cap.
    pub async fn exec(&self, command: &str) -> Result<(Vec<u8>, Option<u32>)> {
        let mut channel = self.handle.channel_open_session().await?;
        channel.exec(true, command).await?;
        let (mut bytes, mut status) = (Vec::new(), None);
        while let Some(message) = channel.wait().await {
            match message {
                ChannelMsg::Data { data } | ChannelMsg::ExtendedData { data, .. } => {
                    anyhow::ensure!(
                        bytes.len() + data.len() <= 16 * 1024 * 1024,
                        "Remote command output exceeds 16 MiB"
                    );
                    bytes.extend_from_slice(&data);
                }
                ChannelMsg::ExitStatus { exit_status } => status = Some(exit_status),
                ChannelMsg::Failure => bail!("Remote command rejected"),
                _ => {}
            }
        }
        Ok((bytes, status))
    }
}
trait Transport: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Transport for T {}
struct ProxyStream {
    read: tokio::process::ChildStdout,
    write: tokio::process::ChildStdin,
}
impl AsyncRead for ProxyStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.read).poll_read(cx, buf)
    }
}
impl AsyncWrite for ProxyStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.write).poll_write(cx, buf)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.write).poll_flush(cx)
    }
    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.write).poll_shutdown(cx)
    }
}
/// Opens a verified, authenticated connection; prompt responses use the command channel.
pub async fn connect_authenticated(
    options: ConnectionOptions,
    events: Sender<SshEvent>,
    commands: Receiver<SshCommand>,
) -> Result<AuthenticatedConnection> {
    connect_cached(options, events, commands, Arc::default()).await
}
async fn connect_cached(
    options: ConnectionOptions,
    events: Sender<SshEvent>,
    commands: Receiver<SshCommand>,
    memory: SharedMemory,
) -> Result<AuthenticatedConnection> {
    options.validate()?;
    let mut hops: Vec<client::Handle<HostHandler>> = Vec::new();
    let mut proxy_child = None;
    let mut chain = options.jumps.clone();
    chain.push(options);
    for (index, option) in chain.into_iter().enumerate() {
        let stream: Box<dyn Transport> = if let Some(previous) = hops.last() {
            anyhow::ensure!(
                option.proxy.is_none(),
                "A proxy command cannot be combined with a preceding jump hop"
            );
            Box::new(
                previous
                    .channel_open_direct_tcpip(&option.host, u32::from(option.port), "127.0.0.1", 0)
                    .await?
                    .into_stream(),
            )
        } else if let Some(proxy) = &option.proxy {
            let mut command = tokio::process::Command::new(&proxy.program);
            command
                .args(&proxy.arguments)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .kill_on_drop(true);
            let mut child = command.spawn().context("Proxy command could not start")?;
            let stream = ProxyStream {
                read: child.stdout.take().context("Missing proxy stdout")?,
                write: child.stdin.take().context("Missing proxy stdin")?,
            };
            proxy_child = Some(child);
            Box::new(stream)
        } else {
            let tcp = tokio::time::timeout(
                option.connect_timeout,
                tokio::net::TcpStream::connect((option.host.as_str(), option.port)),
            )
            .await
            .context("TCP connection timed out")??;
            tcp.set_nodelay(true)?;
            Box::new(tcp)
        };
        let config = Arc::new(client::Config {
            keepalive_interval: if option.keepalive.is_zero() {
                None
            } else {
                Some(option.keepalive)
            },
            keepalive_max: 3,
            ..Default::default()
        });
        let prompt_active = Arc::new(AtomicBool::new(false));
        let handler = HostHandler {
            options: option.clone(),
            events: events.clone(),
            commands: commands.clone(),
            prompt_active: prompt_active.clone(),
            memory: memory.clone(),
        };
        let mut handle = network_timeout(
            client::connect_stream(config, stream, handler),
            option.connect_timeout,
            prompt_active.clone(),
        )
        .await
        .with_context(|| format!("SSH handshake failed at hop {}", index + 1))?;
        network_timeout(
            authenticate(
                &mut handle,
                &option,
                &events,
                &commands,
                &prompt_active,
                &memory,
            ),
            option.connect_timeout,
            prompt_active.clone(),
        )
        .await
        .with_context(|| format!("SSH authentication failed at hop {}", index + 1))?;
        hops.push(handle);
    }
    let handle = hops.pop().context("Empty SSH connection chain")?;
    Ok(AuthenticatedConnection {
        handle,
        hops,
        _proxy: proxy_child,
    })
}
async fn ask(
    events: &Sender<SshEvent>,
    commands: &Receiver<SshCommand>,
    prompt_active: &Arc<AtomicBool>,
    kind: PromptKind,
    name: String,
    instructions: String,
    prompts: Vec<AuthenticationField>,
) -> Result<(Vec<SecretString>, bool)> {
    let _guard = PromptGuard::new(prompt_active.clone());
    let id = PROMPT_ID.fetch_add(1, Ordering::Relaxed);
    let count = prompts.len();
    events
        .send(SshEvent::AuthenticationPrompt {
            id,
            kind,
            name,
            instructions,
            prompts,
        })
        .await?;
    loop {
        match commands.recv().await? {
            SshCommand::AuthResponse {
                id: reply,
                responses,
                save,
            } if reply == id => {
                anyhow::ensure!(
                    responses.len() == count,
                    "Incorrect authentication response count"
                );
                return Ok((responses, save && kind != PromptKind::KeyboardInteractive));
            }
            SshCommand::Disconnect => bail!("Connection cancelled"),
            SshCommand::OpenSftp { reply } => {
                let _ = reply
                    .send(Err("SSH authentication is not complete".into()))
                    .await;
            }
            _ => {}
        }
    }
}
async fn authenticate(
    handle: &mut client::Handle<HostHandler>,
    options: &ConnectionOptions,
    events: &Sender<SshEvent>,
    commands: &Receiver<SshCommand>,
    prompt_active: &Arc<AtomicBool>,
    memory: &SharedMemory,
) -> Result<()> {
    if handle.authenticate_none(&options.username).await?.success() {
        return Ok(());
    }
    for method in &options.auth {
        let success = match method {
            AuthMethod::Password(secret) => handle
                .authenticate_password(&options.username, secret.expose())
                .await?
                .success(),
            AuthMethod::PasswordPrompt => {
                let cached = memory
                    .lock()
                    .expect("SSH session memory poisoned")
                    .passwords
                    .get(&credential_key(options))
                    .cloned();
                let stored = cached.or_else(|| {
                    options
                        .credential_id
                        .as_deref()
                        .and_then(|id| opsssh_vault::Vault.read(id).ok())
                        .map(|secret| SecretString::new(secret.as_str()))
                });
                if let Some(secret) = stored
                    && handle
                        .authenticate_password(&options.username, secret.expose())
                        .await?
                        .success()
                {
                    return Ok(());
                }
                memory
                    .lock()
                    .expect("SSH session memory poisoned")
                    .passwords
                    .remove(&credential_key(options));
                // A rejected saved password is never retried silently.
                let (answer, save) = ask(
                    events,
                    commands,
                    prompt_active,
                    PromptKind::LoginPassword,
                    "SSH password".into(),
                    format!("Sign in to {}", options.host),
                    vec![AuthenticationField {
                        prompt: "Password".into(),
                        echo: false,
                    }],
                )
                .await?;
                let accepted = handle
                    .authenticate_password(&options.username, answer[0].expose())
                    .await?
                    .success();
                if accepted {
                    memory
                        .lock()
                        .expect("SSH session memory poisoned")
                        .passwords
                        .insert(credential_key(options), answer[0].clone());
                }
                if accepted
                    && save
                    && let Some(id) = &options.credential_id
                {
                    opsssh_vault::Vault
                        .save(id, answer[0].expose(), opsssh_vault::SaveConsent::Granted)
                        .context("Signed in, but could not save password to OS vault")?;
                }
                accepted
            }
            AuthMethod::PrivateKey {
                path,
                passphrase,
                certificate,
            } => {
                let stored = options
                    .credential_id
                    .as_deref()
                    .and_then(|id| opsssh_vault::Vault.read(&format!("{id}:key")).ok());
                let key = match load_private_key(
                    path,
                    passphrase
                        .as_ref()
                        .map(SecretString::expose)
                        .or_else(|| stored.as_deref().map(|s| s.as_str())),
                ) {
                    Ok(key) => key,
                    Err(first) if passphrase.is_none() => {
                        let (answer, save) = ask(
                            events,
                            commands,
                            prompt_active,
                            PromptKind::KeyPassphrase,
                            "Private key passphrase".into(),
                            "Unlock the selected private key".into(),
                            vec![AuthenticationField {
                                prompt: "Passphrase".into(),
                                echo: false,
                            }],
                        )
                        .await?;
                        let key = load_private_key(path, Some(answer[0].expose()))
                            .with_context(|| format!("Could not load private key: {first}"))?;
                        if save && let Some(id) = &options.credential_id {
                            opsssh_vault::Vault.save(
                                &format!("{id}:key"),
                                answer[0].expose(),
                                opsssh_vault::SaveConsent::Granted,
                            )?;
                        }
                        key
                    }
                    Err(error) => return Err(error.into()),
                };
                anyhow::ensure!(
                    !key.algorithm().is_rsa(),
                    "RSA private keys are disabled pending a dependency security fix; select Ed25519 or ECDSA"
                );
                if let Some(path) = certificate {
                    handle
                        .authenticate_openssh_cert(
                            &options.username,
                            Arc::new(key),
                            russh::keys::load_openssh_certificate(path)?,
                        )
                        .await?
                        .success()
                } else {
                    handle
                        .authenticate_publickey(
                            &options.username,
                            PrivateKeyWithHashAlg::new(Arc::new(key), None),
                        )
                        .await?
                        .success()
                }
            }
            AuthMethod::Agent { identity_agent } => {
                let agent = tokio::time::timeout(
                    options.connect_timeout,
                    opsssh_platform::agent::connect_agent(identity_agent.as_deref()),
                )
                .await;
                if let Ok(Ok(mut agent)) = agent {
                    let identities = agent.request_identities().await?;
                    let mut success = false;
                    for identity in identities
                        .into_iter()
                        .filter(|identity| !identity.public_key().algorithm().is_rsa())
                        .take(6)
                    {
                        let result = match identity {
                            AgentIdentity::PublicKey { key, .. } => {
                                handle
                                    .authenticate_publickey_with(
                                        &options.username,
                                        key,
                                        None,
                                        &mut agent,
                                    )
                                    .await?
                            }
                            AgentIdentity::Certificate { certificate, .. } => {
                                handle
                                    .authenticate_certificate_with(
                                        &options.username,
                                        certificate,
                                        None,
                                        &mut agent,
                                    )
                                    .await?
                            }
                        };
                        if result.success() {
                            success = true;
                            break;
                        }
                    }
                    success
                } else {
                    false
                }
            }
            AuthMethod::KeyboardInteractive => {
                let mut response = handle
                    .authenticate_keyboard_interactive_start(&options.username, None::<String>)
                    .await?;
                loop {
                    match response {
                        client::KeyboardInteractiveAuthResponse::Success => break true,
                        client::KeyboardInteractiveAuthResponse::Failure { .. } => break false,
                        client::KeyboardInteractiveAuthResponse::InfoRequest {
                            name,
                            instructions,
                            prompts,
                        } => {
                            let (answer, _) = ask(
                                events,
                                commands,
                                prompt_active,
                                PromptKind::KeyboardInteractive,
                                name,
                                instructions,
                                prompts
                                    .into_iter()
                                    .map(|p| AuthenticationField {
                                        prompt: p.prompt,
                                        echo: p.echo,
                                    })
                                    .collect(),
                            )
                            .await?;
                            response = handle
                                .authenticate_keyboard_interactive_respond(
                                    answer.iter().map(|s| s.expose().to_owned()).collect(),
                                )
                                .await?;
                        }
                    }
                }
            }
        };
        if success {
            return Ok(());
        }
    }
    bail!("Authentication rejected; credentials are not retried automatically")
}
async fn run_once(
    options: ConnectionOptions,
    size: &mut TerminalSize,
    events: Sender<SshEvent>,
    commands: Receiver<SshCommand>,
    memory: SharedMemory,
) -> Result<bool> {
    let tmux = options.tmux.clone();
    let connection = connect_cached(options, events.clone(), commands.clone(), memory).await?;
    let mut channel = connection.handle.channel_open_session().await?;
    channel
        .request_pty(
            true,
            "xterm-256color",
            size.columns().into(),
            size.rows().into(),
            0,
            0,
            &[],
        )
        .await?;
    wait_success(&mut channel).await?;
    if let Some(tmux) = tmux {
        channel.exec(true, tmux.attach_command()?).await?;
    } else {
        channel.request_shell(true).await?;
    }
    wait_success(&mut channel).await?;
    events.send(SshEvent::Connected).await?;
    let mut status = None;
    let mut lost = false;
    loop {
        tokio::select! {
         message=channel.wait()=>match message {
          Some(ChannelMsg::Data{data})|Some(ChannelMsg::ExtendedData{data,..})=>{events.send(SshEvent::Data(data.to_vec())).await?;},
          Some(ChannelMsg::ExitStatus{exit_status})=>status=Some(exit_status),
          Some(ChannelMsg::Failure)=>bail!("Server rejected the terminal or shell request"),
          Some(ChannelMsg::Close)=>break,None=>{lost=status.is_none();break;},_=>{}
         },
         command=commands.recv()=>match command {
          Ok(SshCommand::Write(data))=>if channel.data(data.as_slice()).await.is_err(){lost=true;break;},
          Ok(SshCommand::Resize(new_size))=>{*size=new_size;if channel.window_change(size.columns().into(),size.rows().into(),0,0).await.is_err(){lost=true;break;}},
          Ok(SshCommand::OpenSftp{reply})=>{let result=connection.open_sftp().await.map_err(|e|e.to_string());let _=reply.send(result).await;},
          Ok(SshCommand::Disconnect)|Err(_)=>{let _=channel.close().await;break;},_=>{}
         }
        }
    }
    let _ = connection.disconnect().await;
    if !lost {
        events
            .send(SshEvent::Closed {
                exit_status: status,
            })
            .await?;
    }
    Ok(lost)
}

/// Only a lost persistent session reconnects. Authentication and trust failures stop
/// immediately; disconnected keystrokes are discarded rather than replayed.
pub(crate) async fn run(
    mut options: ConnectionOptions,
    mut size: TerminalSize,
    events: Sender<SshEvent>,
    commands: Receiver<SshCommand>,
) -> Result<()> {
    let memory: SharedMemory = Arc::default();
    loop {
        let lost = run_once(
            options.clone(),
            &mut size,
            events.clone(),
            commands.clone(),
            memory.clone(),
        )
        .await?;
        if !lost || options.tmux.is_none() {
            return Ok(());
        }
        options.reconnect = true;
        for jump in &mut options.jumps {
            jump.reconnect = true;
        }
        let mut attempt = 0u32;
        loop {
            attempt = attempt.saturating_add(1);
            let delay = Duration::from_secs((2u64.saturating_pow(attempt.min(6))).min(60));
            events
                .send(SshEvent::Reconnecting { attempt, delay })
                .await?;
            let sleep = tokio::time::sleep(delay);
            tokio::pin!(sleep);
            loop {
                tokio::select! {
                 _=&mut sleep=>break,
                 command=commands.recv()=>match command {
                  Ok(SshCommand::Disconnect)|Err(_)=>return Ok(()),
                  Ok(SshCommand::Resize(new_size))=>size=new_size,
                  Ok(SshCommand::OpenSftp{reply})=>{let _=reply.send(Err("SSH connection is recovering".into())).await;},
                  _=>{} // Never replay keys or stale authentication replies.
                 }
                }
            }
            // Proxy transports perform their own connectivity checks during handshake.
            if options.proxy.is_some() {
                break;
            }
            let first = options.jumps.first().unwrap_or(&options);
            if let Ok(Ok(stream)) = tokio::time::timeout(
                Duration::from_secs(3),
                tokio::net::TcpStream::connect((first.host.as_str(), first.port)),
            )
            .await
            {
                drop(stream);
                break;
            }
        }
        // A handshake/auth error is surfaced once, never retried indefinitely.
    }
}

struct PromptGuard(Arc<AtomicBool>);
impl PromptGuard {
    fn new(active: Arc<AtomicBool>) -> Self {
        active.store(true, Ordering::Release);
        Self(active)
    }
}
impl Drop for PromptGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}
async fn network_timeout<T, E: Into<anyhow::Error>>(
    future: impl std::future::Future<Output = std::result::Result<T, E>>,
    timeout: Duration,
    prompt_active: Arc<AtomicBool>,
) -> Result<T> {
    tokio::pin!(future);
    let mut deadline = tokio::time::Instant::now() + timeout;
    loop {
        tokio::select! {result=&mut future=>return result.map_err(Into::into),_=tokio::time::sleep(Duration::from_millis(100))=>{
         if prompt_active.load(Ordering::Acquire){deadline=tokio::time::Instant::now()+timeout;}
         else if tokio::time::Instant::now()>=deadline{bail!("SSH server response timed out");}
        }}
    }
}
async fn wait_success(channel: &mut russh::Channel<client::Msg>) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            match channel.wait().await {
                Some(ChannelMsg::Success) => return Ok(()),
                Some(ChannelMsg::Failure) => bail!("Server rejected the terminal or shell request"),
                Some(ChannelMsg::Close) | None => {
                    bail!("SSH channel closed before request succeeded")
                }
                _ => {}
            }
        }
    })
    .await
    .context("SSH terminal request timed out")?
}

fn load_private_key(
    path: &std::path::Path,
    passphrase: Option<&str>,
) -> std::result::Result<russh::keys::PrivateKey, russh::keys::Error> {
    let metadata = std::fs::metadata(path)?;
    if metadata.len() > 1024 * 1024 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "Private key exceeds 1 MiB limit",
        )
        .into());
    }
    let secret = Zeroizing::new(std::fs::read_to_string(path)?);
    russh::keys::decode_secret_key(&secret, passphrase)
}
