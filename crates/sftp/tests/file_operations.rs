//! Real packet tests on a private in-memory server, never a user's VM.
use opsssh_sftp::{PermissionChange, SftpClient, TransferControl};
use russh_sftp::{protocol::*, server::Handler};
use std::{
    collections::{BTreeMap, HashSet},
    sync::{Arc, Mutex},
};

#[derive(Clone)]
struct Node {
    mode: u32,
    uid: u32,
    gid: u32,
    bytes: Vec<u8>,
}
impl Node {
    fn new(mode: u32, bytes: &[u8], gid: u32) -> Self {
        Self {
            mode,
            bytes: bytes.to_vec(),
            uid: 1000,
            gid,
        }
    }
    fn attrs(&self) -> FileAttributes {
        FileAttributes {
            permissions: Some(self.mode),
            uid: Some(self.uid),
            gid: Some(self.gid),
            size: Some(self.bytes.len() as u64),
            ..FileAttributes::empty()
        }
    }
}
#[derive(Default)]
struct Remote {
    nodes: BTreeMap<String, Node>,
    changes: Vec<String>,
    denied: Option<String>,
    cancel_after: Option<TransferControl>,
}
struct Server {
    remote: Arc<Mutex<Remote>>,
    read: HashSet<String>,
    atomic: bool,
    openssh_links: bool,
}
fn status(id: u32) -> Status {
    Status {
        id,
        status_code: StatusCode::Ok,
        error_message: String::new(),
        language_tag: String::new(),
    }
}
impl Handler for Server {
    type Error = StatusCode;
    fn unimplemented(&self) -> StatusCode {
        StatusCode::OpUnsupported
    }
    async fn init(
        &mut self,
        _: u32,
        _: std::collections::HashMap<String, String>,
    ) -> Result<Version, StatusCode> {
        let mut version = Version::new();
        if self.atomic {
            version
                .extensions
                .insert("posix-rename@openssh.com".into(), "1".into());
        }
        Ok(version)
    }
    async fn extended(
        &mut self,
        id: u32,
        request: String,
        data: Vec<u8>,
    ) -> Result<Packet, StatusCode> {
        if !self.atomic || request != "posix-rename@openssh.com" {
            return Err(StatusCode::OpUnsupported);
        }
        fn path(data: &mut &[u8]) -> Result<String, StatusCode> {
            let size = u32::from_be_bytes(
                data.get(..4)
                    .ok_or(StatusCode::BadMessage)?
                    .try_into()
                    .unwrap(),
            ) as usize;
            let value = String::from_utf8(
                data.get(4..4 + size)
                    .ok_or(StatusCode::BadMessage)?
                    .to_vec(),
            )
            .map_err(|_| StatusCode::BadMessage)?;
            *data = &data[4 + size..];
            Ok(value)
        }
        let mut data = data.as_slice();
        let source = path(&mut data)?;
        let target = path(&mut data)?;
        let mut r = self.remote.lock().unwrap();
        if r.denied.as_ref() == Some(&target) {
            return Err(StatusCode::Failure);
        }
        let node = r.nodes.remove(&source).ok_or(StatusCode::NoSuchFile)?;
        r.nodes.insert(target, node);
        Ok(Packet::Status(status(id)))
    }
    async fn realpath(&mut self, id: u32, path: String) -> Result<Name, StatusCode> {
        let path = if path == "/alias" {
            "/tree".into()
        } else {
            path
        };
        Ok(Name {
            id,
            files: vec![File::dummy(path)],
        })
    }
    async fn lstat(&mut self, id: u32, path: String) -> Result<Attrs, StatusCode> {
        Ok(Attrs {
            id,
            attrs: self
                .remote
                .lock()
                .unwrap()
                .nodes
                .get(&path)
                .ok_or(StatusCode::NoSuchFile)?
                .attrs(),
        })
    }
    async fn stat(&mut self, id: u32, path: String) -> Result<Attrs, StatusCode> {
        self.lstat(id, path).await
    }
    async fn setstat(
        &mut self,
        id: u32,
        path: String,
        attrs: FileAttributes,
    ) -> Result<Status, StatusCode> {
        let mut r = self.remote.lock().unwrap();
        if r.denied.as_ref() == Some(&path) {
            return Err(StatusCode::PermissionDenied);
        }
        let n = r.nodes.get_mut(&path).ok_or(StatusCode::NoSuchFile)?;
        if let Some(uid) = attrs.uid {
            n.uid = uid;
            n.mode &= !0o6000;
        }
        if let Some(gid) = attrs.gid {
            n.gid = gid;
        }
        if let Some(mode) = attrs.permissions {
            n.mode = (n.mode & !0o7777) | mode;
        }
        r.changes.push(path);
        if let Some(c) = &r.cancel_after {
            c.cancel();
        }
        Ok(status(id))
    }
    async fn opendir(&mut self, id: u32, path: String) -> Result<Handle, StatusCode> {
        self.read.remove(&path);
        Ok(Handle { id, handle: path })
    }
    async fn readdir(&mut self, id: u32, handle: String) -> Result<Name, StatusCode> {
        if !self.read.insert(handle.clone()) {
            return Err(StatusCode::Eof);
        }
        let r = self.remote.lock().unwrap();
        let files = r
            .nodes
            .iter()
            .filter(|(path, _)| {
                path.rsplit_once('/')
                    .is_some_and(|(parent, _)| parent == handle)
            })
            .map(|(path, n)| File::new(path.rsplit('/').next().unwrap(), n.attrs()))
            .collect();
        Ok(Name { id, files })
    }
    async fn close(&mut self, id: u32, _: String) -> Result<Status, StatusCode> {
        Ok(status(id))
    }
    async fn mkdir(
        &mut self,
        id: u32,
        path: String,
        _: FileAttributes,
    ) -> Result<Status, StatusCode> {
        let mut r = self.remote.lock().unwrap();
        if r.nodes.contains_key(&path) {
            return Err(StatusCode::Failure);
        }
        r.nodes.insert(path, Node::new(0o40755, &[], 100));
        Ok(status(id))
    }
    async fn remove(&mut self, id: u32, path: String) -> Result<Status, StatusCode> {
        self.remote
            .lock()
            .unwrap()
            .nodes
            .remove(&path)
            .ok_or(StatusCode::NoSuchFile)?;
        Ok(status(id))
    }
    async fn rmdir(&mut self, id: u32, path: String) -> Result<Status, StatusCode> {
        let mut r = self.remote.lock().unwrap();
        if r.nodes.keys().any(|p| p.starts_with(&format!("{path}/"))) {
            return Err(StatusCode::Failure);
        }
        r.nodes.remove(&path).ok_or(StatusCode::NoSuchFile)?;
        Ok(status(id))
    }
    async fn rename(
        &mut self,
        id: u32,
        oldpath: String,
        newpath: String,
    ) -> Result<Status, StatusCode> {
        let mut r = self.remote.lock().unwrap();
        if r.nodes.contains_key(&newpath) {
            return Err(StatusCode::Failure);
        }
        if !r.nodes.contains_key(&oldpath) {
            return Err(StatusCode::NoSuchFile);
        }
        let paths: Vec<_> = r
            .nodes
            .keys()
            .filter(|p| **p == oldpath || p.starts_with(&format!("{oldpath}/")))
            .cloned()
            .collect();
        for p in paths {
            let n = r.nodes.remove(&p).unwrap();
            r.nodes
                .insert(format!("{newpath}{}", p.strip_prefix(&oldpath).unwrap()), n);
        }
        Ok(status(id))
    }
    async fn open(
        &mut self,
        id: u32,
        path: String,
        flags: OpenFlags,
        attrs: FileAttributes,
    ) -> Result<Handle, StatusCode> {
        let mut r = self.remote.lock().unwrap();
        if flags.contains(OpenFlags::EXCLUDE) && r.nodes.contains_key(&path) {
            return Err(StatusCode::Failure);
        }
        if !r.nodes.contains_key(&path) {
            if !flags.contains(OpenFlags::CREATE) {
                return Err(StatusCode::NoSuchFile);
            }
            r.nodes.insert(
                path.clone(),
                Node::new(0o100000 | attrs.permissions.unwrap_or(0o600), &[], 100),
            );
        }
        if flags.contains(OpenFlags::TRUNCATE) {
            r.nodes.get_mut(&path).unwrap().bytes.clear();
        }
        Ok(Handle { id, handle: path })
    }
    async fn read(
        &mut self,
        id: u32,
        path: String,
        offset: u64,
        len: u32,
    ) -> Result<Data, StatusCode> {
        let r = self.remote.lock().unwrap();
        let bytes = &r.nodes.get(&path).ok_or(StatusCode::NoSuchFile)?.bytes;
        let start = offset as usize;
        if start >= bytes.len() {
            return Err(StatusCode::Eof);
        }
        Ok(Data {
            id,
            data: bytes[start..bytes.len().min(start + len as usize)].to_vec(),
        })
    }
    async fn write(
        &mut self,
        id: u32,
        path: String,
        offset: u64,
        bytes: Vec<u8>,
    ) -> Result<Status, StatusCode> {
        let mut r = self.remote.lock().unwrap();
        let n = r.nodes.get_mut(&path).ok_or(StatusCode::NoSuchFile)?;
        let start = offset as usize;
        n.bytes.resize(n.bytes.len().max(start + bytes.len()), 0);
        n.bytes[start..start + bytes.len()].copy_from_slice(&bytes);
        Ok(status(id))
    }
    async fn readlink(&mut self, id: u32, path: String) -> Result<Name, StatusCode> {
        let r = self.remote.lock().unwrap();
        let node = r.nodes.get(&path).ok_or(StatusCode::NoSuchFile)?;
        Ok(Name {
            id,
            files: vec![File::dummy(String::from_utf8(node.bytes.clone()).unwrap())],
        })
    }
    async fn symlink(
        &mut self,
        id: u32,
        linkpath: String,
        targetpath: String,
    ) -> Result<Status, StatusCode> {
        let (linkpath, targetpath) = if self.openssh_links {
            (targetpath, linkpath)
        } else {
            (linkpath, targetpath)
        };
        let mut r = self.remote.lock().unwrap();
        if r.nodes.contains_key(&linkpath) {
            return Err(StatusCode::Failure);
        }
        r.nodes
            .insert(linkpath, Node::new(0o120777, targetpath.as_bytes(), 100));
        Ok(status(id))
    }
}
async fn fixture() -> (SftpClient, Arc<Mutex<Remote>>) {
    let remote = Arc::new(Mutex::new(Remote::default()));
    {
        let mut r = remote.lock().unwrap();
        for (path, mode, bytes, gid) in [
            ("/tree", 0o40755, &b""[..], 100),
            ("/tree/sub", 0o40750, &b""[..], 101),
            ("/tree/sub/a b'雪.txt", 0o100640, &b"hello world"[..], 102),
            ("/tree/link", 0o120777, &b"/outside"[..], 103),
            ("/outside", 0o100600, &b"keep me"[..], 104),
        ] {
            r.nodes.insert(path.into(), Node::new(mode, bytes, gid));
        }
    }
    let (client, server) = tokio::io::duplex(256 * 1024);
    tokio::spawn(russh_sftp::server::run(
        server,
        Server {
            remote: remote.clone(),
            read: HashSet::new(),
            atomic: true,
            openssh_links: false,
        },
    ));
    (
        SftpClient::connect(client, "verified-fixture".into())
            .await
            .unwrap(),
        remote,
    )
}

async fn atomic_fixture(
    remote: Arc<Mutex<Remote>>,
    supported: bool,
) -> opsssh_sftp::Result<opsssh_sftp::AtomicRenamer> {
    let (client, server) = tokio::io::duplex(65536);
    tokio::spawn(russh_sftp::server::run(
        server,
        Server {
            remote,
            read: HashSet::new(),
            atomic: supported,
            openssh_links: false,
        },
    ));
    opsssh_sftp::AtomicRenamer::connect(client).await
}
#[tokio::test]
async fn openssh_symlink_wire_order_is_probed_privately_and_never_writes_the_original_target() {
    let (_, remote) = fixture().await;
    let (client, server) = tokio::io::duplex(65536);
    tokio::spawn(russh_sftp::server::run(
        server,
        Server {
            remote: remote.clone(),
            read: HashSet::new(),
            atomic: true,
            openssh_links: true,
        },
    ));
    let client = SftpClient::connect(client, "verified-openssh-fixture".into())
        .await
        .unwrap();
    let report = client
        .copy_tree("/tree", "/openssh-copy", false, &TransferControl::default())
        .await
        .unwrap();
    assert!(report.failures.is_empty(), "{report}");
    let r = remote.lock().unwrap();
    assert_eq!(r.nodes["/openssh-copy/link"].bytes, b"/outside");
    assert_eq!(r.nodes["/outside"].bytes, b"keep me");
    assert!(
        r.nodes.keys().all(|path| !path.contains("opsssh-drop-")),
        "probe must be cleaned up"
    );
}
#[tokio::test]
async fn atomic_move_negotiates_extension_and_preserves_both_files_on_failure_or_stale_review() {
    let (client, remote) = fixture().await;
    remote
        .lock()
        .unwrap()
        .nodes
        .insert("/replacement".into(), Node::new(0o100600, b"old", 1));
    assert!(atomic_fixture(remote.clone(), false).await.is_err());
    let renamer = atomic_fixture(remote.clone(), true).await.unwrap();
    let source = client.attributes("/outside").await.unwrap();
    let target = client.attributes("/replacement").await.unwrap();
    remote.lock().unwrap().denied = Some("/replacement".into());
    assert!(
        renamer
            .replace(
                "/outside",
                "/replacement",
                &source,
                &target,
                &TransferControl::default()
            )
            .await
            .is_err()
    );
    assert_eq!(remote.lock().unwrap().nodes["/outside"].bytes, b"keep me");
    assert_eq!(remote.lock().unwrap().nodes["/replacement"].bytes, b"old");
    remote.lock().unwrap().denied = None;
    remote
        .lock()
        .unwrap()
        .nodes
        .get_mut("/replacement")
        .unwrap()
        .bytes = b"changed".to_vec();
    assert!(
        renamer
            .replace(
                "/outside",
                "/replacement",
                &source,
                &target,
                &TransferControl::default()
            )
            .await
            .is_err()
    );
    assert!(client.exists("/outside").await.unwrap());
    let target = client.attributes("/replacement").await.unwrap();
    renamer
        .replace(
            "/outside",
            "/replacement",
            &source,
            &target,
            &TransferControl::default(),
        )
        .await
        .unwrap();
    assert!(!client.exists("/outside").await.unwrap());
    assert_eq!(
        remote.lock().unwrap().nodes["/replacement"].bytes,
        b"keep me"
    );
}
#[tokio::test]
async fn recursive_exact_mode_and_owner_preserve_each_group_and_skip_links() {
    let (client, remote) = fixture().await;
    let attrs = client.attributes("/tree").await.unwrap();
    let report = client
        .change_permissions(
            "/tree",
            &attrs,
            &PermissionChange {
                mode: Some(0o4755),
                uid: Some(2000),
                recursive: true,
                ..Default::default()
            },
            &TransferControl::default(),
        )
        .await
        .unwrap();
    assert_eq!(
        (report.changed, report.skipped, report.failures.len()),
        (3, 1, 0)
    );
    let r = remote.lock().unwrap();
    for (path, gid) in [
        ("/tree", 100),
        ("/tree/sub", 101),
        ("/tree/sub/a b'雪.txt", 102),
    ] {
        assert_eq!(r.nodes[path].mode & 0o7777, 0o4755);
        assert_eq!((r.nodes[path].uid, r.nodes[path].gid), (2000, gid));
    }
    assert_eq!(r.nodes["/outside"].mode, 0o100600);
    assert_eq!(r.nodes["/tree/link"].uid, 1000);
    assert_eq!(r.changes.last().unwrap(), "/tree");
}
#[tokio::test]
async fn denied_cancelled_and_replaced_items_report_without_claiming_rollback() {
    let (client, remote) = fixture().await;
    let attrs = client.attributes("/tree").await.unwrap();
    remote.lock().unwrap().denied = Some("/tree/sub".into());
    let report = client
        .change_permissions(
            "/tree",
            &attrs,
            &PermissionChange {
                mode: Some(0o700),
                recursive: true,
                ..Default::default()
            },
            &TransferControl::default(),
        )
        .await
        .unwrap();
    assert_eq!(report.failures.len(), 1);
    assert_eq!(report.changed, 2);
    let control = TransferControl::default();
    remote.lock().unwrap().cancel_after = Some(control.clone());
    let report = client
        .change_permissions(
            "/tree",
            &attrs,
            &PermissionChange {
                mode: Some(0o755),
                recursive: true,
                ..Default::default()
            },
            &control,
        )
        .await
        .unwrap();
    assert!(report.cancelled);
    assert_eq!(report.changed, 1);
    remote.lock().unwrap().nodes.get_mut("/tree").unwrap().mode = 0o120777;
    assert!(
        client
            .change_permissions(
                "/tree",
                &attrs,
                &PermissionChange::default(),
                &TransferControl::default()
            )
            .await
            .is_err()
    );
}
#[tokio::test]
async fn ownership_only_does_not_restore_cleared_special_bits() {
    let (client, remote) = fixture().await;
    remote
        .lock()
        .unwrap()
        .nodes
        .get_mut("/outside")
        .unwrap()
        .mode = 0o104755;
    let attrs = client.attributes("/outside").await.unwrap();
    client
        .change_permissions(
            "/outside",
            &attrs,
            &PermissionChange {
                uid: Some(42),
                ..Default::default()
            },
            &TransferControl::default(),
        )
        .await
        .unwrap();
    assert_eq!(
        remote.lock().unwrap().nodes["/outside"].mode & 0o7777,
        0o755
    );
}
#[tokio::test]
async fn copy_move_download_delete_handle_trees_without_following_links() {
    let (client, remote) = fixture().await;
    let control = TransferControl::default();
    let report = client
        .copy_tree("/tree", "/copied", false, &control)
        .await
        .unwrap();
    assert_eq!(report.changed, 4);
    assert!(report.failures.is_empty());
    assert_eq!(
        remote.lock().unwrap().nodes["/copied/link"].bytes,
        b"/outside"
    );
    assert_eq!(control.batch_transferred(), 11);
    client
        .rename_item("/copied", "/moved", &TransferControl::default())
        .await
        .unwrap();
    assert!(!client.exists("/copied").await.unwrap());
    let local = tempfile::tempdir().unwrap();
    let destination = local.path().join("downloaded");
    let report = client
        .download_tree("/moved", &destination, &TransferControl::default())
        .await
        .unwrap();
    assert_eq!((report.changed, report.skipped), (3, 1));
    assert_eq!(
        std::fs::read(destination.join("sub/a b'雪.txt")).unwrap(),
        b"hello world"
    );
    let report = client
        .delete_tree("/moved", &TransferControl::default())
        .await
        .unwrap();
    assert_eq!(report.changed, 4);
    assert!(report.failures.is_empty());
    assert_eq!(remote.lock().unwrap().nodes["/outside"].bytes, b"keep me");
}
#[tokio::test]
async fn collisions_and_descendant_aliases_keep_existing_data() {
    let (client, remote) = fixture().await;
    assert!(
        client
            .transfer_target("/tree", "/alias/nested")
            .await
            .is_err()
    );
    assert!(
        client
            .copy_tree("/tree", "/tree/sub", false, &TransferControl::default())
            .await
            .is_err()
    );
    assert!(
        client
            .delete_tree("/", &TransferControl::default())
            .await
            .is_err()
    );
    assert!(
        client
            .rename_item("/outside", "/tree", &TransferControl::default())
            .await
            .is_err()
    );
    remote
        .lock()
        .unwrap()
        .nodes
        .insert("/existing".into(), Node::new(0o100600, b"old", 1));
    assert!(
        client
            .copy_tree("/outside", "/existing", false, &TransferControl::default())
            .await
            .is_err()
    );
    assert_eq!(remote.lock().unwrap().nodes["/existing"].bytes, b"old");
    client
        .copy_tree("/outside", "/existing", true, &TransferControl::default())
        .await
        .unwrap();
    assert_eq!(remote.lock().unwrap().nodes["/existing"].bytes, b"keep me");
    let local = tempfile::tempdir().unwrap();
    std::fs::write(local.path().join("keep"), b"old").unwrap();
    let report = client
        .download_tree("/tree", local.path(), &TransferControl::default())
        .await
        .unwrap();
    assert_eq!(report.changed, 0);
    assert_eq!(report.failures.len(), 1);
    assert!(!local.path().join("sub").exists());
}
