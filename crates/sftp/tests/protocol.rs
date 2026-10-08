//! Exercises real SFTP packets against an isolated in-memory server, no credentials.
use opsssh_sftp::{SftpClient, TransferControl, WriteMode};
use russh_sftp::{protocol::*, server::Handler};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

#[derive(Clone, Default)]
struct MemoryServer {
    files: Arc<Mutex<HashMap<String, Vec<u8>>>>,
    directories: Arc<Mutex<HashMap<String, u32>>>,
    denied_writes: Vec<String>,
    listed: bool,
}
fn ok(id: u32) -> Status {
    Status {
        id,
        status_code: StatusCode::Ok,
        error_message: String::new(),
        language_tag: String::new(),
    }
}
fn attrs(data: &[u8]) -> FileAttributes {
    FileAttributes {
        size: Some(data.len() as u64),
        mtime: Some(1),
        permissions: Some(0o100600),
        ..FileAttributes::empty()
    }
}
impl Handler for MemoryServer {
    type Error = StatusCode;
    fn unimplemented(&self) -> Self::Error {
        StatusCode::OpUnsupported
    }
    async fn open(
        &mut self,
        id: u32,
        filename: String,
        flags: OpenFlags,
        _: FileAttributes,
    ) -> Result<Handle, StatusCode> {
        if flags.contains(OpenFlags::WRITE)
            && self.denied_writes.iter().any(|path| {
                filename
                    .rsplit_once('/')
                    .is_some_and(|(parent, _)| parent == path)
            })
        {
            return Err(StatusCode::PermissionDenied);
        }
        let mut files = self.files.lock().unwrap();
        if files.contains_key(&filename) && flags.contains(OpenFlags::EXCLUDE) {
            return Err(StatusCode::Failure);
        }
        if !files.contains_key(&filename) && !flags.contains(OpenFlags::CREATE) {
            return Err(StatusCode::NoSuchFile);
        }
        let data = files.entry(filename.clone()).or_default();
        if flags.contains(OpenFlags::TRUNCATE) {
            data.clear();
        }
        Ok(Handle {
            id,
            handle: filename,
        })
    }
    async fn close(&mut self, id: u32, _: String) -> Result<Status, StatusCode> {
        Ok(ok(id))
    }
    async fn remove(&mut self, id: u32, filename: String) -> Result<Status, StatusCode> {
        self.files
            .lock()
            .unwrap()
            .remove(&filename)
            .ok_or(StatusCode::NoSuchFile)?;
        Ok(ok(id))
    }
    async fn read(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        len: u32,
    ) -> Result<Data, StatusCode> {
        let files = self.files.lock().unwrap();
        let bytes = files.get(&handle).ok_or(StatusCode::NoSuchFile)?;
        let offset = usize::try_from(offset).map_err(|_| StatusCode::Failure)?;
        if offset >= bytes.len() {
            return Err(StatusCode::Eof);
        }
        Ok(Data {
            id,
            data: bytes[offset..bytes.len().min(offset + len as usize)].to_vec(),
        })
    }
    async fn write(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        data: Vec<u8>,
    ) -> Result<Status, StatusCode> {
        let mut files = self.files.lock().unwrap();
        let bytes = files.get_mut(&handle).ok_or(StatusCode::NoSuchFile)?;
        let offset = usize::try_from(offset).map_err(|_| StatusCode::Failure)?;
        if offset + data.len() > 4 * 1024 * 1024 {
            return Err(StatusCode::Failure);
        }
        bytes.resize(bytes.len().max(offset + data.len()), 0);
        bytes[offset..offset + data.len()].copy_from_slice(&data);
        Ok(ok(id))
    }
    async fn realpath(&mut self, id: u32, path: String) -> Result<Name, StatusCode> {
        Ok(Name {
            id,
            files: vec![File::dummy(path)],
        })
    }
    async fn stat(&mut self, id: u32, path: String) -> Result<Attrs, StatusCode> {
        if let Some(mode) = self.directories.lock().unwrap().get(&path) {
            return Ok(Attrs {
                id,
                attrs: FileAttributes {
                    permissions: Some(*mode | 0o40000),
                    ..FileAttributes::empty()
                },
            });
        }
        let files = self.files.lock().unwrap();
        Ok(Attrs {
            id,
            attrs: attrs(files.get(&path).ok_or(StatusCode::NoSuchFile)?),
        })
    }
    async fn lstat(&mut self, id: u32, path: String) -> Result<Attrs, StatusCode> {
        self.stat(id, path).await
    }
    async fn mkdir(
        &mut self,
        id: u32,
        path: String,
        _: FileAttributes,
    ) -> Result<Status, StatusCode> {
        let mut directories = self.directories.lock().unwrap();
        if directories.contains_key(&path) {
            return Err(StatusCode::Failure);
        }
        directories.insert(path, 0o755);
        Ok(ok(id))
    }
    async fn setstat(
        &mut self,
        id: u32,
        path: String,
        attrs: FileAttributes,
    ) -> Result<Status, StatusCode> {
        let mut directories = self.directories.lock().unwrap();
        let mode = directories.get_mut(&path).ok_or(StatusCode::NoSuchFile)?;
        *mode = attrs.permissions.ok_or(StatusCode::BadMessage)?;
        Ok(ok(id))
    }
    async fn opendir(&mut self, id: u32, _: String) -> Result<Handle, StatusCode> {
        self.listed = false;
        Ok(Handle {
            id,
            handle: "directory".into(),
        })
    }
    async fn readdir(&mut self, id: u32, _: String) -> Result<Name, StatusCode> {
        if self.listed {
            return Err(StatusCode::Eof);
        }
        self.listed = true;
        Ok(Name {
            id,
            files: self
                .files
                .lock()
                .unwrap()
                .iter()
                .map(|(path, data)| File::new(path.trim_start_matches('/'), attrs(data)))
                .collect(),
        })
    }
}

#[tokio::test]
async fn upload_destination_checks_write_access_and_removes_its_probe() {
    let (client_io, server_io) = tokio::io::duplex(65536);
    let server = MemoryServer {
        denied_writes: vec!["/home".into()],
        ..Default::default()
    };
    let files = server.files.clone();
    russh_sftp::server::run(server_io, server).await;
    let client = SftpClient::connect(client_io, "SHA256:fixture".into())
        .await
        .unwrap();
    assert_eq!(
        client
            .validate_upload_directory("/home/backend")
            .await
            .unwrap(),
        "/home/backend"
    );
    assert!(
        files.lock().unwrap().is_empty(),
        "successful probe left a file behind"
    );
    let error = client
        .validate_upload_directory("/home")
        .await
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("/home") && error.contains("SSH login"),
        "{error}"
    );
    assert!(files.lock().unwrap().is_empty());
}

#[tokio::test]
async fn real_protocol_upload_download_list_resume_and_conflicts() {
    let (client_io, server_io) = tokio::io::duplex(65536);
    let server = MemoryServer::default();
    let remote = server.files.clone();
    russh_sftp::server::run(server_io, server).await;
    let client = SftpClient::connect(client_io, "SHA256:fixture".into())
        .await
        .unwrap();
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source");
    let download = directory.path().join("download");
    let data: Vec<u8> = (0..300000).map(|n| (n % 251) as u8).collect();
    tokio::fs::write(&source, &data).await.unwrap();
    assert_eq!(
        client
            .upload(
                &source,
                "/file",
                WriteMode::CreateNew,
                &TransferControl::default()
            )
            .await
            .unwrap(),
        data.len() as u64
    );
    assert!(
        client
            .upload(
                &source,
                "/file",
                WriteMode::CreateNew,
                &TransferControl::default()
            )
            .await
            .is_err()
    );
    assert_eq!(client.list("/").await.unwrap()[0].name, "file");
    client
        .download(
            "/file",
            &download,
            WriteMode::CreateNew,
            &TransferControl::default(),
        )
        .await
        .unwrap();
    assert_eq!(tokio::fs::read(&download).await.unwrap(), data);
    tokio::fs::write(&download, &data[..100000]).await.unwrap();
    let identity = client.identity("/file").await.unwrap();
    client
        .download(
            "/file",
            &download,
            WriteMode::Resume(identity.clone()),
            &TransferControl::default(),
        )
        .await
        .unwrap();
    assert_eq!(tokio::fs::read(&download).await.unwrap(), data);
    tokio::fs::write(&download, b"incorrect prefix")
        .await
        .unwrap();
    assert!(
        client
            .download(
                "/file",
                &download,
                WriteMode::Resume(identity.clone()),
                &TransferControl::default()
            )
            .await
            .is_err()
    );
    remote
        .lock()
        .unwrap()
        .get_mut("/file")
        .unwrap()
        .truncate(100000);
    assert!(
        client
            .download(
                "/file",
                &download,
                WriteMode::Resume(identity),
                &TransferControl::default()
            )
            .await
            .is_err()
    );
    let partial = client.identity("/file").await.unwrap();
    client
        .upload(
            &source,
            "/file",
            WriteMode::Resume(partial),
            &TransferControl::default(),
        )
        .await
        .unwrap();
    assert_eq!(*remote.lock().unwrap().get("/file").unwrap(), data);
    let cancelled = TransferControl::default();
    cancelled.cancel();
    assert!(
        client
            .upload(&source, "/cancelled", WriteMode::CreateNew, &cancelled)
            .await
            .is_err()
    );
    assert!(!remote.lock().unwrap().contains_key("/cancelled"));
}

#[tokio::test]
async fn private_staging_and_recursive_upload_use_real_sftp_operations() {
    let (client_io, server_io) = tokio::io::duplex(65536);
    let server = MemoryServer::default();
    let directories = server.directories.clone();
    let files = server.files.clone();
    russh_sftp::server::run(server_io, server).await;
    let client = SftpClient::connect(client_io, "SHA256:fixture".into())
        .await
        .unwrap();
    let stage = client.private_staging("/home/fixture").await.unwrap();
    assert_eq!(directories.lock().unwrap()[&stage], 0o700);
    assert!(client.create_private_directory(&stage).await.is_err());
    let attachment = format!("{stage}/screenshot.png");
    client
        .upload_bytes(b"fixture", &attachment, &TransferControl::default())
        .await
        .unwrap();
    assert_eq!(files.lock().unwrap()[&attachment], b"fixture");
    assert!(
        client
            .upload_bytes(b"replacement", &attachment, &TransferControl::default())
            .await
            .is_err()
    );
    let local = tempfile::tempdir().unwrap();
    tokio::fs::write(local.path().join("one"), b"one")
        .await
        .unwrap();
    tokio::fs::create_dir(local.path().join("nested"))
        .await
        .unwrap();
    tokio::fs::write(local.path().join("nested/two"), b"two")
        .await
        .unwrap();
    let progress = TransferControl::default();
    progress.set_total(6);
    assert_eq!(
        client
            .upload_tree(local.path(), "/tree", &progress)
            .await
            .unwrap(),
        6
    );
    assert_eq!(files.lock().unwrap()["/tree/nested/two"], b"two");
    assert_eq!(progress.batch_transferred(), 6);
    assert_eq!(progress.total_bytes(), Some(6));
}
