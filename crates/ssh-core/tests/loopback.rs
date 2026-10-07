use opsssh_ssh_core::*;
use opsssh_term_core::TerminalSize;
use russh::{
    Channel, ChannelId,
    keys::{Algorithm, PrivateKey, PublicKey},
    server,
};
use std::{
    borrow::Cow,
    sync::{Arc, Mutex},
    time::Duration,
};
#[derive(Clone)]
struct Echo {
    sizes: Arc<Mutex<Vec<(u32, u32)>>>,
    key: PublicKey,
    forwarded: std::collections::HashSet<ChannelId>,
}
impl server::Handler for Echo {
    type Error = russh::Error;
    async fn auth_password(
        &mut self,
        user: &str,
        password: &str,
    ) -> std::result::Result<server::Auth, Self::Error> {
        Ok(if user == "fixture" && password == "fixture-password" {
            server::Auth::Accept
        } else {
            server::Auth::reject()
        })
    }
    async fn auth_publickey(
        &mut self,
        user: &str,
        key: &PublicKey,
    ) -> std::result::Result<server::Auth, Self::Error> {
        Ok(if user == "fixture" && key == &self.key {
            server::Auth::Accept
        } else {
            server::Auth::reject()
        })
    }
    async fn auth_keyboard_interactive<'a>(
        &'a mut self,
        _: &str,
        _: &str,
        response: Option<server::Response<'a>>,
    ) -> std::result::Result<server::Auth, Self::Error> {
        Ok(if let Some(mut response) = response {
            if response.next().is_some_and(|r| r.as_ref() == b"123456") {
                server::Auth::Accept
            } else {
                server::Auth::reject()
            }
        } else {
            server::Auth::Partial {
                name: "Fixture OTP".into(),
                instructions: "Enter test code".into(),
                prompts: Cow::Owned(vec![("One-time code".into(), false)]),
            }
        })
    }
    async fn channel_open_session(
        &mut self,
        _: Channel<server::Msg>,
        reply: server::ChannelOpenHandle,
        _: &mut server::Session,
    ) -> std::result::Result<(), Self::Error> {
        reply.accept().await;
        Ok(())
    }
    async fn channel_open_direct_tcpip(
        &mut self,
        channel: Channel<server::Msg>,
        host: &str,
        port: u32,
        _: &str,
        _: u32,
        reply: server::ChannelOpenHandle,
        _: &mut server::Session,
    ) -> std::result::Result<(), Self::Error> {
        if host != "127.0.0.1" {
            return Ok(());
        }
        let mut tcp = tokio::net::TcpStream::connect((host, port as u16)).await?;
        self.forwarded.insert(channel.id());
        reply.accept().await;
        tokio::spawn(async move {
            let mut stream = channel.into_stream();
            let _ = tokio::io::copy_bidirectional(&mut stream, &mut tcp).await;
        });
        Ok(())
    }
    async fn pty_request(
        &mut self,
        channel: ChannelId,
        _: &str,
        col: u32,
        row: u32,
        _: u32,
        _: u32,
        _: &[(russh::Pty, u32)],
        session: &mut server::Session,
    ) -> std::result::Result<(), Self::Error> {
        self.sizes.lock().unwrap().push((col, row));
        session.channel_success(channel)?;
        Ok(())
    }
    async fn window_change_request(
        &mut self,
        _: ChannelId,
        col: u32,
        row: u32,
        _: u32,
        _: u32,
        _: &mut server::Session,
    ) -> std::result::Result<(), Self::Error> {
        self.sizes.lock().unwrap().push((col, row));
        Ok(())
    }
    async fn shell_request(
        &mut self,
        channel: ChannelId,
        session: &mut server::Session,
    ) -> std::result::Result<(), Self::Error> {
        session.channel_success(channel)?;
        session.data(channel, b"fixture-ready".to_vec())?;
        Ok(())
    }
    async fn exec_request(
        &mut self,
        channel: ChannelId,
        command: &[u8],
        session: &mut server::Session,
    ) -> std::result::Result<(), Self::Error> {
        session.channel_success(channel)?;
        match command {
            b"exec-fixture" => {
                session.data(channel, b"stdout-marker".to_vec())?;
                session.extended_data(channel, 1, b"stderr-marker".to_vec())?;
                session.exit_status_request(channel, 7)?;
                session.eof(channel)?;
                session.close(channel)?;
                return Ok(());
            }
            b"large-exec-fixture" => {
                session.data(channel, vec![b'x'; 4096])?;
                session.exit_status_request(channel, 0)?;
                session.close(channel)?;
                return Ok(());
            }
            b"slow-exec-fixture" => return Ok(()),
            _ => {}
        }
        session.data(channel, b"fixture-ready".to_vec())?;
        Ok(())
    }
    async fn data(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        session: &mut server::Session,
    ) -> std::result::Result<(), Self::Error> {
        if data == b"disconnect-fixture" {
            return Err(russh::Error::Disconnect);
        }
        if data == b"close-without-status-fixture" {
            session.close(channel)?;
            return Ok(());
        }
        if !self.forwarded.contains(&channel) {
            session.data(channel, data.to_vec())?;
        }
        Ok(())
    }
}
struct Fixture {
    port: u16,
    host_key: PrivateKey,
    user_key: PrivateKey,
    sizes: Arc<Mutex<Vec<(u32, u32)>>>,
    task: tokio::task::JoinHandle<()>,
    fail_handshakes: Arc<std::sync::atomic::AtomicUsize>,
}
impl Fixture {
    async fn new() -> Self {
        let host_key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap();
        let user_key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap();
        let config = Arc::new(server::Config {
            keys: vec![host_key.clone()],
            auth_rejection_time: Duration::ZERO,
            auth_rejection_time_initial: Some(Duration::ZERO),
            ..Default::default()
        });
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        let sizes = Arc::new(Mutex::new(Vec::new()));
        let handler = Echo {
            sizes: sizes.clone(),
            key: user_key.public_key().clone(),
            forwarded: Default::default(),
        };
        let fail_handshakes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let failed = fail_handshakes.clone();
        let task = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                if failed
                    .try_update(
                        std::sync::atomic::Ordering::SeqCst,
                        std::sync::atomic::Ordering::SeqCst,
                        |n| n.checked_sub(1),
                    )
                    .is_ok()
                {
                    drop(stream);
                    continue;
                }
                let config = config.clone();
                let handler = handler.clone();
                tokio::spawn(async move {
                    if let Ok(session) = server::run_stream(config, stream, handler).await {
                        let _ = session.await;
                    }
                });
            }
        });
        Self {
            port,
            host_key,
            user_key,
            sizes,
            task,
            fail_handshakes,
        }
    }
    fn options(&self, path: std::path::PathBuf) -> ConnectionOptions {
        let mut options = ConnectionOptions::new("127.0.0.1", "fixture", path);
        options.port = self.port;
        options.auth = vec![AuthMethod::Password(SecretString::new("fixture-password"))];
        options
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn next(events: &async_channel::Receiver<SshEvent>) -> SshEvent {
    tokio::time::timeout(Duration::from_secs(10), events.recv())
        .await
        .expect("SSH event timeout")
        .expect("SSH event channel closed")
}
async fn trust_and_connect(session: &SshSession) {
    let events = session.events();
    loop {
        match next(&events).await {
            SshEvent::HostKeyPrompt { id, .. } => session
                .commands()
                .send(SshCommand::TrustHost {
                    id,
                    trust: true,
                    persist: true,
                })
                .await
                .unwrap(),
            SshEvent::Connected => break,
            SshEvent::Error(error) => panic!("{error}"),
            _ => {}
        }
    }
}
async fn start_exec(
    session: &SshSession,
    request: ExecRequest,
) -> async_channel::Receiver<std::result::Result<ExecOutput, String>> {
    let (reply, receive) = async_channel::bounded(1);
    session
        .commands()
        .send(SshCommand::Exec { request, reply })
        .await
        .unwrap();
    receive
}
async fn exec_reply(
    receive: async_channel::Receiver<std::result::Result<ExecOutput, String>>,
) -> std::result::Result<ExecOutput, String> {
    tokio::time::timeout(Duration::from_secs(3), receive.recv())
        .await
        .unwrap()
        .unwrap()
}
#[tokio::test]
async fn separate_exec_preserves_stdout_stderr_and_exit_status() {
    let fixture = Fixture::new().await;
    let dir = tempfile::tempdir().unwrap();
    let session = SshSession::spawn(
        fixture.options(dir.path().join("known_hosts")),
        TerminalSize::new(80, 24).unwrap(),
    )
    .unwrap();
    trust_and_connect(&session).await;
    let output = exec_reply(start_exec(&session, ExecRequest::new("exec-fixture")).await)
        .await
        .unwrap();
    assert_eq!(output.stdout, b"stdout-marker");
    assert_eq!(output.stderr, b"stderr-marker");
    assert_eq!(output.exit_status, Some(7));
}
#[tokio::test]
async fn timed_out_exec_does_not_block_interactive_terminal() {
    let fixture = Fixture::new().await;
    let dir = tempfile::tempdir().unwrap();
    let session = SshSession::spawn(
        fixture.options(dir.path().join("known_hosts")),
        TerminalSize::new(80, 24).unwrap(),
    )
    .unwrap();
    trust_and_connect(&session).await;
    let mut request = ExecRequest::new("slow-exec-fixture");
    request.timeout = Duration::from_millis(500);
    let receive = start_exec(&session, request).await;
    session
        .commands()
        .send(SshCommand::Write(b"interactive-during-exec".to_vec()))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_millis(400), async {
        loop {
            if let SshEvent::Data(data) = session.events().recv().await.unwrap()
                && data == b"interactive-during-exec"
            {
                break;
            }
        }
    })
    .await
    .expect("Exec blocked the PTY");
    assert!(exec_reply(receive).await.unwrap_err().contains("timed out"));
}
#[tokio::test]
async fn exec_can_be_cancelled_and_output_is_bounded() {
    let fixture = Fixture::new().await;
    let dir = tempfile::tempdir().unwrap();
    let session = SshSession::spawn(
        fixture.options(dir.path().join("known_hosts")),
        TerminalSize::new(80, 24).unwrap(),
    )
    .unwrap();
    trust_and_connect(&session).await;
    let request = ExecRequest::new("slow-exec-fixture");
    let control = request.control.clone();
    let receive = start_exec(&session, request).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    control.cancel();
    assert!(exec_reply(receive).await.unwrap_err().contains("cancelled"));
    let mut request = ExecRequest::new("large-exec-fixture");
    request.output_limit = 512;
    assert!(
        exec_reply(start_exec(&session, request).await)
            .await
            .unwrap_err()
            .contains("output exceeds")
    );
    let output = exec_reply(start_exec(&session, ExecRequest::new("exec-fixture")).await)
        .await
        .unwrap();
    assert_eq!(output.exit_status, Some(7));
}
#[tokio::test]
async fn protected_session_recovers_from_close_without_status_and_rejects_stale_exec() {
    let fixture = Fixture::new().await;
    let dir = tempfile::tempdir().unwrap();
    let mut options = fixture.options(dir.path().join("known_hosts"));
    options.tmux = Some(TmuxOptions {
        session_name: "exec-recovery".into(),
    });
    let session = SshSession::spawn(options, TerminalSize::new(80, 24).unwrap()).unwrap();
    trust_and_connect(&session).await;
    session
        .commands()
        .send(SshCommand::Write(b"close-without-status-fixture".to_vec()))
        .await
        .unwrap();
    let mut recovering = false;
    loop {
        match next(&session.events()).await {
            SshEvent::Reconnecting { .. } => {
                recovering = true;
                let error =
                    exec_reply(start_exec(&session, ExecRequest::new("exec-fixture")).await)
                        .await
                        .unwrap_err();
                assert!(error.contains("recovering"));
            }
            SshEvent::Connected => {
                assert!(recovering);
                break;
            }
            SshEvent::Error(error) => panic!("{error}"),
            SshEvent::Closed { .. } => panic!("Protected session closed unexpectedly"),
            _ => {}
        }
    }
    let output = exec_reply(start_exec(&session, ExecRequest::new("exec-fixture")).await)
        .await
        .unwrap();
    assert_eq!(output.stdout, b"stdout-marker");
}
#[tokio::test]
async fn stalled_sftp_does_not_block_terminal_or_disconnect() {
    let fixture = Fixture::new().await;
    let dir = tempfile::tempdir().unwrap();
    let session = SshSession::spawn(
        fixture.options(dir.path().join("known_hosts")),
        TerminalSize::new(80, 24).unwrap(),
    )
    .unwrap();
    trust_and_connect(&session).await;
    // The fixture's default subsystem handler deliberately never replies.
    let (reply, receive) = async_channel::bounded(1);
    session
        .commands()
        .send(SshCommand::OpenSftp { reply })
        .await
        .unwrap();
    session
        .commands()
        .send(SshCommand::Write(b"interactive-during-sftp".to_vec()))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if let SshEvent::Data(data) = session.events().recv().await.unwrap()
                && data == b"interactive-during-sftp"
            {
                break;
            }
        }
    })
    .await
    .expect("SFTP subsystem request blocked PTY input");
    session
        .commands()
        .send(SshCommand::Disconnect)
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if matches!(
                session.events().recv().await.unwrap(),
                SshEvent::Closed { .. }
            ) {
                break;
            }
        }
    })
    .await
    .expect("Stalled SFTP blocked disconnect");
    assert!(
        tokio::time::timeout(Duration::from_secs(1), receive.recv())
            .await
            .unwrap()
            .is_err()
    );
}
#[tokio::test]
async fn protected_reconnect_retries_transient_handshake_loss() {
    let fixture = Fixture::new().await;
    let dir = tempfile::tempdir().unwrap();
    let mut options = fixture.options(dir.path().join("known_hosts"));
    options.tmux = Some(TmuxOptions {
        session_name: "transient-handshake".into(),
    });
    let session = SshSession::spawn(options, TerminalSize::new(80, 24).unwrap()).unwrap();
    trust_and_connect(&session).await;
    // Drop the readiness probe and the first real recovery handshake, then accept normally.
    fixture
        .fail_handshakes
        .store(2, std::sync::atomic::Ordering::SeqCst);
    session
        .commands()
        .send(SshCommand::Write(b"disconnect-fixture".to_vec()))
        .await
        .unwrap();
    let mut reconnects = 0;
    loop {
        match next(&session.events()).await {
            SshEvent::Reconnecting { .. } => reconnects += 1,
            SshEvent::Connected => {
                assert!(reconnects >= 2, "Transient handshake did not retry");
                break;
            }
            SshEvent::Error(error) => panic!("{error}"),
            SshEvent::HostKeyPrompt { .. } => {
                panic!("Recovery unexpectedly prompted for host trust")
            }
            SshEvent::Closed { .. } => panic!("Recovery unexpectedly closed"),
            _ => {}
        }
    }
}
#[tokio::test]
async fn password_trust_pty_echo_resize() {
    let fixture = Fixture::new().await;
    let dir = tempfile::tempdir().unwrap();
    let session = SshSession::spawn(
        fixture.options(dir.path().join("known_hosts")),
        TerminalSize::new(80, 24).unwrap(),
    )
    .unwrap();
    trust_and_connect(&session).await;
    session
        .commands()
        .send(SshCommand::Resize(TerminalSize::new(100, 40).unwrap()))
        .await
        .unwrap();
    session
        .commands()
        .send(SshCommand::Write(b"echo-marker".to_vec()))
        .await
        .unwrap();
    loop {
        if let SshEvent::Data(data) = next(&session.events()).await
            && data == b"echo-marker"
        {
            break;
        }
    }
    assert_eq!(*fixture.sizes.lock().unwrap(), vec![(80, 24), (100, 40)]);
    assert!(
        std::fs::read_to_string(dir.path().join("known_hosts"))
            .unwrap()
            .contains("[127.0.0.1]:")
    );
}
#[tokio::test]
async fn unknown_host_can_be_denied() {
    let fixture = Fixture::new().await;
    let dir = tempfile::tempdir().unwrap();
    let session = SshSession::spawn(
        fixture.options(dir.path().join("known_hosts")),
        TerminalSize::new(80, 24).unwrap(),
    )
    .unwrap();
    loop {
        match next(&session.events()).await {
            SshEvent::HostKeyPrompt { id, .. } => session
                .commands()
                .send(SshCommand::TrustHost {
                    id,
                    trust: false,
                    persist: false,
                })
                .await
                .unwrap(),
            SshEvent::Connected => panic!("Untrusted host connected"),
            SshEvent::Error(_) => break,
            _ => {}
        }
    }
    assert!(!dir.path().join("known_hosts").exists());
}
#[tokio::test]
async fn changed_host_key_is_blocked_without_prompt() {
    let fixture = Fixture::new().await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("known_hosts");
    std::fs::write(
        &path,
        format!(
            "[127.0.0.1]:{} {}\n",
            fixture.port,
            fixture.user_key.public_key().to_openssh().unwrap()
        ),
    )
    .unwrap();
    let session =
        SshSession::spawn(fixture.options(path), TerminalSize::new(80, 24).unwrap()).unwrap();
    loop {
        match next(&session.events()).await {
            SshEvent::HostKeyPrompt { .. } | SshEvent::Connected => {
                panic!("Changed key was offered trust")
            }
            SshEvent::Error(_) => break,
            _ => {}
        }
    }
}
#[tokio::test]
async fn private_key_authentication() {
    let fixture = Fixture::new().await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("identity");
    std::fs::write(
        &path,
        fixture
            .user_key
            .to_openssh(Default::default())
            .unwrap()
            .as_bytes(),
    )
    .unwrap();
    let mut options = fixture.options(dir.path().join("known_hosts"));
    options.auth = vec![AuthMethod::PrivateKey {
        path,
        passphrase: None,
        certificate: None,
    }];
    let session = SshSession::spawn(options, TerminalSize::new(80, 24).unwrap()).unwrap();
    trust_and_connect(&session).await;
}
#[tokio::test]
async fn keyboard_interactive_otp() {
    let fixture = Fixture::new().await;
    let dir = tempfile::tempdir().unwrap();
    let mut options = fixture.options(dir.path().join("known_hosts"));
    options.auth = vec![AuthMethod::KeyboardInteractive];
    let session = SshSession::spawn(options, TerminalSize::new(80, 24).unwrap()).unwrap();
    let mut otp = false;
    loop {
        match next(&session.events()).await {
            SshEvent::HostKeyPrompt { id, .. } => session
                .commands()
                .send(SshCommand::TrustHost {
                    id,
                    trust: true,
                    persist: false,
                })
                .await
                .unwrap(),
            SshEvent::AuthenticationPrompt {
                id, kind, prompts, ..
            } => {
                assert_eq!(kind, PromptKind::KeyboardInteractive);
                assert_eq!(prompts.len(), 1);
                otp = true;
                session
                    .commands()
                    .send(SshCommand::AuthResponse {
                        id,
                        responses: vec![SecretString::new("123456")],
                        save: true,
                    })
                    .await
                    .unwrap();
            }
            SshEvent::Connected => break,
            SshEvent::Error(error) => panic!("{error}"),
            _ => {}
        }
    }
    assert!(otp);
}
#[tokio::test]
async fn revoked_key_blocks_even_with_matching_normal_entry() {
    let fixture = Fixture::new().await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("known_hosts");
    let key = fixture.host_key.public_key().to_openssh().unwrap();
    std::fs::write(
        &path,
        format!(
            "[127.0.0.1]:{} {key}\n@revoked [127.0.0.1]:{} {key}\n",
            fixture.port, fixture.port
        ),
    )
    .unwrap();
    let session =
        SshSession::spawn(fixture.options(path), TerminalSize::new(80, 24).unwrap()).unwrap();
    loop {
        match next(&session.events()).await {
            SshEvent::HostKeyPrompt { .. } | SshEvent::Connected => panic!("Revoked key trusted"),
            SshEvent::Error(_) => break,
            _ => {}
        }
    }
}

#[tokio::test]
async fn two_jump_hosts_verify_every_hop() {
    let first = Fixture::new().await;
    let second = Fixture::new().await;
    let target = Fixture::new().await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("known_hosts");
    let mut options = target.options(path.clone());
    options.jumps = vec![first.options(path.clone()), second.options(path.clone())];
    let session = SshSession::spawn(options, TerminalSize::new(80, 24).unwrap()).unwrap();
    let mut verified = 0;
    loop {
        match next(&session.events()).await {
            SshEvent::HostKeyPrompt { id, .. } => session
                .commands()
                .send(SshCommand::TrustHost {
                    id,
                    trust: true,
                    persist: true,
                })
                .await
                .unwrap(),
            SshEvent::VerifiedHost { .. } => verified += 1,
            SshEvent::Connected => break,
            SshEvent::Error(error) => panic!("{error}"),
            _ => {}
        }
    }
    assert_eq!(verified, 3);
    session
        .commands()
        .send(SshCommand::Write(b"jump-marker".to_vec()))
        .await
        .unwrap();
    loop {
        if let SshEvent::Data(data) = next(&session.events()).await
            && data == b"jump-marker"
        {
            break;
        }
    }
}
#[test]
fn secrets_are_redacted() {
    assert!(!format!("{:?}", SecretString::new("must-not-leak")).contains("must-not-leak"));
    assert!(
        !format!(
            "{:?}",
            SshCommand::AuthResponse {
                id: 1,
                responses: vec![SecretString::new("must-not-leak")],
                save: false
            }
        )
        .contains("must-not-leak")
    );
}

#[tokio::test]
async fn reconnect_retains_trust_password_and_size_without_replaying_input() {
    let fixture = Fixture::new().await;
    let dir = tempfile::tempdir().unwrap();
    let mut options = fixture.options(dir.path().join("known_hosts"));
    options.auth = vec![AuthMethod::PasswordPrompt];
    options.tmux = Some(TmuxOptions {
        session_name: "fixture-tab".into(),
    });
    let session = SshSession::spawn(options, TerminalSize::new(80, 24).unwrap()).unwrap();
    let (mut connections, mut passwords, mut trusts) = (0, 0, 0);
    loop {
        match next(&session.events()).await {
            SshEvent::HostKeyPrompt { id, .. } => {
                trusts += 1;
                session
                    .commands()
                    .send(SshCommand::TrustHost {
                        id,
                        trust: true,
                        persist: false,
                    })
                    .await
                    .unwrap();
            }
            SshEvent::AuthenticationPrompt { id, kind, .. } => {
                assert_eq!(kind, PromptKind::LoginPassword);
                passwords += 1;
                session
                    .commands()
                    .send(SshCommand::AuthResponse {
                        id,
                        responses: vec![SecretString::new("fixture-password")],
                        save: false,
                    })
                    .await
                    .unwrap();
            }
            SshEvent::Connected => {
                connections += 1;
                if connections == 1 {
                    session
                        .commands()
                        .send(SshCommand::Resize(TerminalSize::new(100, 40).unwrap()))
                        .await
                        .unwrap();
                    session
                        .commands()
                        .send(SshCommand::Write(b"disconnect-fixture".to_vec()))
                        .await
                        .unwrap();
                } else {
                    session
                        .commands()
                        .send(SshCommand::Write(b"recovered-marker".to_vec()))
                        .await
                        .unwrap();
                }
            }
            SshEvent::Reconnecting { .. } => {
                session
                    .commands()
                    .send(SshCommand::Write(b"must-not-replay".to_vec()))
                    .await
                    .unwrap();
            }
            SshEvent::Data(data) => {
                assert_ne!(data, b"must-not-replay");
                if data == b"recovered-marker" {
                    break;
                }
            }
            SshEvent::Error(error) => panic!("{error}"),
            SshEvent::Closed { .. } => panic!("Persistent session unexpectedly closed"),
            _ => {}
        }
    }
    assert_eq!((connections, passwords, trusts), (2, 1, 1));
    assert_eq!(fixture.sizes.lock().unwrap().last(), Some(&(100, 40)));
    assert!(!dir.path().join("known_hosts").exists());
}
#[tokio::test]
async fn strict_host_policy_never_prompts_for_unknown_key() {
    let fixture = Fixture::new().await;
    let dir = tempfile::tempdir().unwrap();
    let mut options = fixture.options(dir.path().join("known_hosts"));
    options.strict_host_key = true;
    let session = SshSession::spawn(options, TerminalSize::new(80, 24).unwrap()).unwrap();
    loop {
        match next(&session.events()).await {
            SshEvent::HostKeyPrompt { .. } | SshEvent::Connected => panic!("Strict trust bypass"),
            SshEvent::Error(error) => {
                assert!(error.contains("strict verification"));
                break;
            }
            _ => {}
        }
    }
}
