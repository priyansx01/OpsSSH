//! Exercise real SFTP packets, including v3's generic Failure for name collisions.
use opsssh_sftp::{SftpClient, TransferControl};
use russh_sftp::{
    protocol::{Attrs, FileAttributes, Handle, OpenFlags, Status, StatusCode},
    server::Handler,
};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

#[derive(Default)]
struct Remote {
    files: HashMap<String, (Vec<u8>, u32)>,
    opens: Vec<OpenFlags>,
    fail_writes: bool,
}
struct Server(Arc<Mutex<Remote>>);
fn ok(id: u32) -> Status {
    Status {
        id,
        status_code: StatusCode::Ok,
        error_message: String::new(),
        language_tag: String::new(),
    }
}
impl Handler for Server {
    type Error = StatusCode;
    fn unimplemented(&self) -> Self::Error {
        StatusCode::OpUnsupported
    }
    async fn open(
        &mut self,
        id: u32,
        path: String,
        flags: OpenFlags,
        _: FileAttributes,
    ) -> Result<Handle, Self::Error> {
        let mut remote = self.0.lock().unwrap();
        remote.opens.push(flags);
        if flags.contains(OpenFlags::EXCLUDE) && remote.files.contains_key(&path) {
            return Err(StatusCode::Failure);
        }
        if !remote.files.contains_key(&path) && !flags.contains(OpenFlags::CREATE) {
            return Err(StatusCode::NoSuchFile);
        }
        let file = remote
            .files
            .entry(path.clone())
            .or_insert_with(|| (Vec::new(), 0o100600));
        if flags.contains(OpenFlags::TRUNCATE) {
            file.0.clear();
        }
        Ok(Handle { id, handle: path })
    }
    async fn close(&mut self, id: u32, _: String) -> Result<Status, Self::Error> {
        Ok(ok(id))
    }
    async fn write(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        bytes: Vec<u8>,
    ) -> Result<Status, Self::Error> {
        let mut remote = self.0.lock().unwrap();
        if remote.fail_writes {
            return Err(StatusCode::PermissionDenied);
        }
        let file = remote
            .files
            .get_mut(&handle)
            .ok_or(StatusCode::NoSuchFile)?;
        let start = usize::try_from(offset).unwrap();
        file.0.resize(file.0.len().max(start + bytes.len()), 0);
        file.0[start..start + bytes.len()].copy_from_slice(&bytes);
        Ok(ok(id))
    }
    async fn lstat(&mut self, id: u32, path: String) -> Result<Attrs, Self::Error> {
        let remote = self.0.lock().unwrap();
        let (bytes, permissions) = remote.files.get(&path).ok_or(StatusCode::NoSuchFile)?;
        Ok(Attrs {
            id,
            attrs: FileAttributes {
                size: Some(bytes.len() as u64),
                permissions: Some(*permissions),
                mtime: Some(1),
                ..FileAttributes::empty()
            },
        })
    }
    async fn mkdir(
        &mut self,
        id: u32,
        path: String,
        _: FileAttributes,
    ) -> Result<Status, Self::Error> {
        let mut remote = self.0.lock().unwrap();
        if remote.files.contains_key(&path) {
            return Err(StatusCode::Failure);
        }
        remote.files.insert(path, (Vec::new(), 0o40700));
        Ok(ok(id))
    }
}
async fn fixture() -> (SftpClient, Arc<Mutex<Remote>>, tempfile::TempDir) {
    let remote = Arc::new(Mutex::new(Remote::default()));
    let (client, server) = tokio::io::duplex(256 * 1024);
    tokio::spawn(russh_sftp::server::run(server, Server(remote.clone())));
    (
        SftpClient::connect(client, "verified-loopback".into())
            .await
            .unwrap(),
        remote,
        tempfile::tempdir().unwrap(),
    )
}

#[tokio::test]
async fn repeated_upload_requires_confirmation_then_truncates_and_counts_once() {
    let (client, remote, local) = fixture().await;
    let path = local.path().join("test.txt");
    tokio::fs::write(&path, b"old content that is longer")
        .await
        .unwrap();
    client
        .upload_tree(&path, "/opt/test.txt", &TransferControl::default())
        .await
        .unwrap();
    tokio::fs::write(&path, b"new").await.unwrap();
    assert!(
        client
            .upload_tree(&path, "/opt/test.txt", &TransferControl::default())
            .await
            .is_err()
    );
    assert_eq!(
        remote.lock().unwrap().files["/opt/test.txt"].0,
        b"old content that is longer"
    );
    let mut confirmations = 0;
    let control = TransferControl::default();
    let bytes = client
        .upload_tree_with_confirmation(&path, "/opt/test.txt", &control, |destination| {
            confirmations += 1;
            assert_eq!(destination, "/opt/test.txt");
            assert_eq!(
                remote.lock().unwrap().files[&destination].0,
                b"old content that is longer"
            );
            async { Ok(()) }
        })
        .await
        .unwrap();
    assert_eq!(confirmations, 1);
    assert_eq!(bytes, 3);
    assert_eq!(control.batch_transferred(), 3);
    assert_eq!(remote.lock().unwrap().files["/opt/test.txt"].0, b"new");
}

#[tokio::test]
async fn cancelled_confirmation_preserves_existing_content_and_never_truncates() {
    let (client, remote, local) = fixture().await;
    remote
        .lock()
        .unwrap()
        .files
        .insert("/opt/test.txt".into(), (b"keep me".to_vec(), 0o100600));
    let path = local.path().join("test.txt");
    tokio::fs::write(&path, b"replacement").await.unwrap();
    let mut confirmations = 0;
    assert!(
        client
            .upload_tree_with_confirmation(
                &path,
                "/opt/test.txt",
                &TransferControl::default(),
                |_| {
                    confirmations += 1;
                    async {
                        Err(
                            std::io::Error::new(std::io::ErrorKind::Interrupted, "cancelled")
                                .into(),
                        )
                    }
                }
            )
            .await
            .is_err()
    );
    assert_eq!(confirmations, 1);
    let remote = remote.lock().unwrap();
    assert_eq!(remote.files["/opt/test.txt"].0, b"keep me");
    assert!(
        remote
            .opens
            .iter()
            .all(|flags| !flags.contains(OpenFlags::TRUNCATE))
    );
}

#[tokio::test]
async fn directory_and_symlink_collisions_cannot_be_replaced_as_files() {
    let (client, remote, local) = fixture().await;
    let path = local.path().join("test.txt");
    tokio::fs::write(&path, b"replacement").await.unwrap();
    for permissions in [0o40700, 0o120777] {
        remote
            .lock()
            .unwrap()
            .files
            .insert("/opt/test.txt".into(), (b"keep".to_vec(), permissions));
        assert!(
            client
                .upload_tree_with_confirmation(
                    &path,
                    "/opt/test.txt",
                    &TransferControl::default(),
                    |_| {
                        panic!("nonregular files must never offer replacement");
                        #[allow(unreachable_code)]
                        async {
                            Ok(())
                        }
                    }
                )
                .await
                .is_err()
        );
        assert_eq!(remote.lock().unwrap().files["/opt/test.txt"].0, b"keep");
    }
}

#[tokio::test]
async fn destination_changed_during_confirmation_requires_a_new_review() {
    let (client, remote, local) = fixture().await;
    remote
        .lock()
        .unwrap()
        .files
        .insert("/opt/test.txt".into(), (b"old".to_vec(), 0o100600));
    let path = local.path().join("test.txt");
    tokio::fs::write(&path, b"replacement").await.unwrap();
    assert!(
        client
            .upload_tree_with_confirmation(
                &path,
                "/opt/test.txt",
                &TransferControl::default(),
                |destination| {
                    remote
                        .lock()
                        .unwrap()
                        .files
                        .get_mut(&destination)
                        .unwrap()
                        .0 = b"changed by another process".to_vec();
                    async { Ok(()) }
                }
            )
            .await
            .is_err()
    );
    let remote = remote.lock().unwrap();
    assert_eq!(
        remote.files["/opt/test.txt"].0,
        b"changed by another process"
    );
    assert!(
        remote
            .opens
            .iter()
            .all(|flags| !flags.contains(OpenFlags::TRUNCATE))
    );
}

#[tokio::test]
async fn failed_write_is_not_misreported_as_a_collision() {
    let (client, remote, local) = fixture().await;
    remote.lock().unwrap().fail_writes = true;
    let path = local.path().join("test.txt");
    tokio::fs::write(&path, b"replacement").await.unwrap();
    assert!(
        client
            .upload_tree_with_confirmation(
                &path,
                "/opt/test.txt",
                &TransferControl::default(),
                |_| {
                    panic!("a write error must not trigger replacement");
                    #[allow(unreachable_code)]
                    async {
                        Ok(())
                    }
                }
            )
            .await
            .is_err()
    );
    assert!(
        remote
            .lock()
            .unwrap()
            .opens
            .iter()
            .all(|flags| !flags.contains(OpenFlags::TRUNCATE))
    );
}

#[tokio::test]
async fn repeated_folder_upload_merges_and_confirms_only_colliding_files() {
    let (client, remote, local) = fixture().await;
    let folder = local.path().join("folder");
    tokio::fs::create_dir(&folder).await.unwrap();
    tokio::fs::write(folder.join("test.txt"), b"new")
        .await
        .unwrap();
    {
        let mut remote = remote.lock().unwrap();
        remote
            .files
            .insert("/opt/folder".into(), (Vec::new(), 0o40700));
        remote
            .files
            .insert("/opt/folder/test.txt".into(), (b"old".to_vec(), 0o100600));
        remote.files.insert(
            "/opt/folder/untouched.txt".into(),
            (b"keep".to_vec(), 0o100600),
        );
    }
    let mut confirmations = Vec::new();
    client
        .upload_tree_with_confirmation(
            &folder,
            "/opt/folder",
            &TransferControl::default(),
            |path| {
                confirmations.push(path);
                async { Ok(()) }
            },
        )
        .await
        .unwrap();
    assert_eq!(confirmations, ["/opt/folder/test.txt"]);
    let remote = remote.lock().unwrap();
    assert_eq!(remote.files["/opt/folder/test.txt"].0, b"new");
    assert_eq!(remote.files["/opt/folder/untouched.txt"].0, b"keep");
}
