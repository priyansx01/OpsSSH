//! Disposable fixture probe. Never accepts unknown keys or reads user credentials.
use opsssh_sftp::{SftpClient, TransferControl, WriteMode};
use opsssh_ssh_core::*;
#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    anyhow::ensure!(
        args.len() == 5,
        "usage: compat_client HOST PORT USER PRIVATE_KEY KNOWN_HOSTS"
    );
    let mut options = ConnectionOptions::new(&args[0], &args[2], args[4].clone().into());
    options.port = args[1].parse()?;
    options.reconnect = true;
    options.auth = vec![AuthMethod::PrivateKey {
        path: args[3].clone().into(),
        passphrase: None,
        certificate: None,
    }];
    let (event_tx, event_rx) = async_channel::bounded(32);
    let (_command_tx, command_rx) = async_channel::bounded(32);
    let connection = connect_authenticated(options, event_tx, command_rx).await?;
    let mut fingerprint = None;
    while let Ok(event) = event_rx.try_recv() {
        if let SshEvent::VerifiedHost {
            fingerprint: value, ..
        } = event
        {
            fingerprint = Some(value);
        }
    }
    let (output, status) = connection.exec("printf opsssh-compat-ok").await?;
    anyhow::ensure!(
        status == Some(0) && output == b"opsssh-compat-ok",
        "SSH exec fixture failed"
    );
    let stream = connection.open_sftp().await?;
    let sftp = SftpClient::connect(
        stream,
        fingerprint.ok_or_else(|| anyhow::anyhow!("Missing verified fingerprint"))?,
    )
    .await
    .map_err(|e| anyhow::anyhow!("{e}"))?;
    let home = sftp
        .canonicalize(".")
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let remote = format!("{home}/opsssh-compat-{}", std::process::id());
    let control = TransferControl::default();
    let dir = tempfile::tempdir()?;
    let destination = dir.path().join("roundtrip");
    let payload = b"OpsSSH encrypted SFTP roundtrip\n";
    sftp.upload_bytes(payload, &remote, &control)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let transfer = sftp
        .download(
            &remote,
            &destination,
            WriteMode::CreateNew,
            &TransferControl::default(),
        )
        .await;
    let cleanup = sftp.remove_file_confirmed(&remote).await;
    transfer.map_err(|e| anyhow::anyhow!("{e}"))?;
    cleanup.map_err(|e| anyhow::anyhow!("{e}"))?;
    anyhow::ensure!(
        std::fs::read(destination)? == payload,
        "SFTP roundtrip mismatch"
    );
    connection.disconnect().await?;
    println!("SSH exec and SFTP roundtrip passed");
    Ok(())
}
