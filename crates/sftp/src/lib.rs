//! Real SFTP over an already authenticated SSH channel. No credentials are owned here.
use russh_sftp::{
    client::{SftpSession, error::Error as SftpError},
    protocol::{FileAttributes, OpenFlags, StatusCode},
};
use std::{
    fmt,
    future::Future,
    io,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncSeekExt, AsyncWrite, AsyncWriteExt};

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
#[derive(Debug)]
struct UploadCollision(FileAttributes);
impl fmt::Display for UploadCollision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("destination exists; replacement requires confirmation")
    }
}
impl std::error::Error for UploadCollision {}
#[derive(Debug, Clone, Default)]
pub struct TransferControl {
    cancelled: Arc<AtomicBool>,
    bytes: Arc<AtomicU64>,
    batch_bytes: Arc<AtomicU64>,
    total: Arc<AtomicU64>,
    total_known: Arc<AtomicBool>,
}
impl TransferControl {
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }
    pub fn transferred(&self) -> u64 {
        self.bytes.load(Ordering::Acquire)
    }
    pub fn batch_transferred(&self) -> u64 {
        self.batch_bytes.load(Ordering::Acquire)
    }
    pub fn total_bytes(&self) -> Option<u64> {
        self.total_known
            .load(Ordering::Acquire)
            .then(|| self.total.load(Ordering::Acquire))
    }
    pub fn set_total(&self, bytes: u64) {
        self.total.store(bytes, Ordering::Release);
        self.total_known.store(true, Ordering::Release);
    }
    pub fn ensure_active(&self) -> Result<()> {
        self.check()
    }
    fn check(&self) -> Result<()> {
        if self.cancelled.load(Ordering::Acquire) {
            Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "transfer cancelled; partial file retained for explicit resume",
            )
            .into())
        } else {
            Ok(())
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteIdentity {
    pub host_key: String,
    pub canonical_path: String,
    pub size: u64,
    pub modified: u32,
}
#[derive(Debug, Clone)]
pub enum WriteMode {
    CreateNew,
    OverwriteConfirmed,
    Resume(RemoteIdentity),
}
#[derive(Debug, Clone)]
pub struct Entry {
    pub name: String,
    pub path: String,
    pub size: Option<u64>,
    pub modified: Option<u32>,
    pub is_directory: bool,
    pub is_symlink: bool,
}

pub struct SftpClient {
    session: SftpSession,
    host_key: String,
    slots: tokio::sync::Semaphore,
}
impl fmt::Debug for SftpClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SftpClient").finish_non_exhaustive()
    }
}
impl SftpClient {
    /// `host_key` is the verified fingerprint from the owning SSH connection.
    pub async fn connect<S>(stream: S, host_key: String) -> Result<Self>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        if host_key.is_empty() {
            return Err("verified host fingerprint is required".into());
        }
        Ok(Self {
            session: SftpSession::new(stream).await?,
            host_key,
            slots: tokio::sync::Semaphore::new(3),
        })
    }
    pub async fn canonicalize(&self, path: &str) -> Result<String> {
        Ok(self.session.canonicalize(path).await?)
    }
    /// Validate the login user's actual write access, including ACLs and read-only mounts.
    /// The exclusive, empty probe is removed before the destination is accepted.
    pub async fn validate_upload_directory(&self, path: &str) -> Result<String> {
        let path = self.canonicalize(path).await?;
        self.list(&path).await?;
        let probe = opsssh_drop::join_remote(
            &path,
            &format!(".opsssh-write-check-{}", uuid::Uuid::new_v4()),
        )?;
        let file = self.session.open_with_flags_and_attributes(
            &probe,
            OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::EXCLUDE,
            FileAttributes { permissions: Some(0o600), ..FileAttributes::empty() },
        ).await.map_err(|error| format!(
            "Cannot upload to {path}: {error}. Choose a folder writable by your SSH login. Sudo in the terminal does not change SFTP permissions."
        ))?;
        let close = file.close().await;
        let cleanup = self.session.remove_file(&probe).await;
        cleanup.map_err(|error| format!("Could not remove upload write check {probe}: {error}"))?;
        close?;
        Ok(path)
    }
    /// SFTP v3 listing is collected off the UI thread; the UI must virtualize rows.
    pub async fn list(&self, path: &str) -> Result<Vec<Entry>> {
        let mut entries = Vec::new();
        for entry in self.session.read_dir(path).await? {
            let name = entry.file_name();
            let safe_path = opsssh_drop::join_remote(path, &name)?;
            let metadata = entry.metadata();
            entries.push(Entry {
                name,
                path: safe_path,
                size: metadata.size,
                modified: metadata.mtime,
                is_directory: metadata.is_dir(),
                is_symlink: metadata.is_symlink(),
            });
        }
        entries.sort_unstable_by(|a, b| {
            b.is_directory
                .cmp(&a.is_directory)
                .then_with(|| a.name.cmp(&b.name))
        });
        Ok(entries)
    }
    pub async fn identity(&self, path: &str) -> Result<RemoteIdentity> {
        let canonical_path = self.canonicalize(path).await?;
        let metadata = self.session.symlink_metadata(&canonical_path).await?;
        if !metadata.is_regular() {
            return Err("resume requires a regular file".into());
        }
        Ok(RemoteIdentity {
            host_key: self.host_key.clone(),
            canonical_path,
            size: metadata.size.ok_or("server omitted file size")?,
            modified: metadata.mtime.ok_or("server omitted modification time")?,
        })
    }
    pub async fn create_directory(&self, path: &str) -> Result<()> {
        Ok(self.session.create_dir(path).await?)
    }
    /// New, unpredictable child of a user-owned directory; never relax an existing directory.
    pub async fn create_private_directory(&self, path: &str) -> Result<()> {
        self.session.create_dir(path).await?;
        self.session
            .set_metadata(
                path,
                FileAttributes {
                    permissions: Some(0o700),
                    ..FileAttributes::empty()
                },
            )
            .await?;
        let metadata = self.session.symlink_metadata(path).await?;
        if !metadata.is_dir()
            || metadata
                .permissions
                .is_none_or(|mode| mode & 0o777 != 0o700)
        {
            return Err("server did not enforce private staging permissions".into());
        }
        Ok(())
    }
    pub async fn remove_file_confirmed(&self, path: &str) -> Result<()> {
        Ok(self.session.remove_file(path).await?)
    }
    /// Creates a fresh private staging directory under an existing user-owned parent.
    pub async fn private_staging(&self, parent: &str) -> Result<String> {
        let path =
            opsssh_drop::join_remote(parent, &format!("opsssh-drop-{}", uuid::Uuid::new_v4()))?;
        self.create_private_directory(&path).await?;
        Ok(path)
    }
    /// Uploads an image or text attachment as a new file, never replacing an existing one.
    pub async fn upload_bytes(
        &self,
        bytes: &[u8],
        remote: &str,
        control: &TransferControl,
    ) -> Result<u64> {
        let _slot = self.slots.acquire().await?;
        control.check()?;
        let mut destination = self
            .session
            .open_with_flags_and_attributes(
                remote,
                OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::EXCLUDE,
                FileAttributes {
                    permissions: Some(0o600),
                    ..FileAttributes::empty()
                },
            )
            .await?;
        let result = copy(&mut &bytes[..], &mut destination, 0, control).await;
        let close = destination.close().await;
        let bytes = result?;
        close?;
        Ok(bytes)
    }
    /// Recursive upload rejects symlinks and creates each destination exclusively.
    pub async fn upload_tree(
        &self,
        local: &Path,
        remote: &str,
        control: &TransferControl,
    ) -> Result<u64> {
        self.upload_tree_inner(local, remote, control, false, |_| async {
            Err("destination exists; replacement requires confirmation".into())
        })
        .await
    }

    /// Merge folders without removing anything; confirm every colliding regular file.
    /// New files still use exclusive creation, including after a concurrent collision.
    pub async fn upload_tree_with_confirmation<F, Fut>(
        &self,
        local: &Path,
        remote: &str,
        control: &TransferControl,
        confirm: F,
    ) -> Result<u64>
    where
        F: FnMut(String) -> Fut,
        Fut: Future<Output = Result<()>>,
    {
        self.upload_tree_inner(local, remote, control, true, confirm)
            .await
    }

    async fn existing_metadata(&self, path: &str) -> Result<Option<FileAttributes>> {
        match self.session.symlink_metadata(path).await {
            Ok(metadata) => Ok(Some(metadata)),
            Err(SftpError::Status(status)) if status.status_code == StatusCode::NoSuchFile => {
                Ok(None)
            }
            Err(error) => Err(error.into()),
        }
    }

    async fn upload_tree_inner<F, Fut>(
        &self,
        local: &Path,
        remote: &str,
        control: &TransferControl,
        merge_directories: bool,
        mut confirm: F,
    ) -> Result<u64>
    where
        F: FnMut(String) -> Fut,
        Fut: Future<Output = Result<()>>,
    {
        let mut pending = vec![(local.to_path_buf(), remote.to_string())];
        let mut total = 0;
        while let Some((source, destination)) = pending.pop() {
            control.check()?;
            let metadata = tokio::fs::symlink_metadata(&source).await?;
            if metadata.is_symlink() {
                return Err("recursive upload does not follow symbolic links".into());
            }
            if metadata.is_dir() {
                match self.create_directory(&destination).await {
                    Ok(()) => {}
                    Err(error) => {
                        if !merge_directories
                            || !self
                                .existing_metadata(&destination)
                                .await?
                                .is_some_and(|metadata| metadata.is_dir() && !metadata.is_symlink())
                        {
                            return Err(error);
                        }
                    }
                }
                let mut entries = tokio::fs::read_dir(&source).await?;
                while let Some(entry) = entries.next_entry().await? {
                    let name = entry
                        .file_name()
                        .into_string()
                        .map_err(|_| "non-Unicode filenames are not supported")?;
                    pending.push((entry.path(), opsssh_drop::join_remote(&destination, &name)?));
                }
            } else if metadata.is_file() {
                // SFTP v3 often reports a generic Failure for an exclusive-open collision.
                // Inspect the path instead of interpreting a failure message as permission.
                let result = self
                    .upload(&source, &destination, WriteMode::CreateNew, control)
                    .await;
                match result {
                    Ok(bytes) => total += bytes,
                    Err(error) => {
                        control.check()?;
                        let Some(UploadCollision(existing)) =
                            error.downcast_ref::<UploadCollision>()
                        else {
                            return Err(error);
                        };
                        if !existing.is_regular() || existing.is_symlink() {
                            return Err("cannot replace a folder, symbolic link, or special file with an upload".into());
                        }
                        confirm(destination.clone()).await?;
                        control.check()?;
                        let current = self.existing_metadata(&destination).await?.ok_or(
                            "destination changed while waiting for replacement confirmation",
                        )?;
                        if !current.is_regular()
                            || current.is_symlink()
                            || current.size != existing.size
                            || current.mtime != existing.mtime
                            || current.permissions != existing.permissions
                        {
                            return Err("destination changed while waiting for replacement confirmation; upload again to review it".into());
                        }
                        total += self
                            .upload(
                                &source,
                                &destination,
                                WriteMode::OverwriteConfirmed,
                                control,
                            )
                            .await?;
                    }
                }
            } else {
                return Err("only regular files and folders can be uploaded".into());
            }
        }
        Ok(total)
    }
    pub async fn rename(&self, old: &str, new: &str) -> Result<()> {
        if self.session.try_exists(new).await? {
            return Err("destination exists; rename will not overwrite".into());
        }
        Ok(self.session.rename(old, new).await?)
    }
    pub async fn upload(
        &self,
        local: &Path,
        remote: &str,
        mode: WriteMode,
        control: &TransferControl,
    ) -> Result<u64> {
        let _slot = self.slots.acquire().await?;
        control.check()?;
        let mut source = tokio::fs::File::open(local).await?;
        let local_size = source.metadata().await?.len();
        let create_new = matches!(mode, WriteMode::CreateNew);
        let (flags, offset) = match mode {
            WriteMode::CreateNew => (OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::EXCLUDE, 0),
            WriteMode::OverwriteConfirmed => (
                OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::TRUNCATE,
                0,
            ),
            WriteMode::Resume(expected) => {
                let actual = self.identity(remote).await?;
                if actual != expected || actual.size > local_size {
                    return Err("resume identity changed; restart requires confirmation".into());
                }
                let mut partial = self.session.open(remote).await?;
                verify_prefix(&mut source, &mut partial, actual.size, control).await?;
                partial.close().await?;
                (OpenFlags::WRITE, actual.size)
            }
        };
        let destination = self
            .session
            .open_with_flags_and_attributes(
                remote,
                flags,
                FileAttributes {
                    permissions: Some(0o600),
                    ..FileAttributes::empty()
                },
            )
            .await;
        let mut destination = match destination {
            Ok(file) => file,
            Err(error) => {
                if create_new && let Some(metadata) = self.existing_metadata(remote).await? {
                    return Err(Box::new(UploadCollision(metadata)));
                }
                return Err(error.into());
            }
        };
        source.seek(io::SeekFrom::Start(offset)).await?;
        destination.seek(io::SeekFrom::Start(offset)).await?;
        let result = copy(&mut source, &mut destination, offset, control).await;
        let close = destination.close().await;
        let bytes = result?;
        close?;
        Ok(bytes)
    }
    pub async fn download(
        &self,
        remote: &str,
        local: &Path,
        mode: WriteMode,
        control: &TransferControl,
    ) -> Result<u64> {
        let _slot = self.slots.acquire().await?;
        control.check()?;
        let mut source = self.session.open(remote).await?;
        let mut options = tokio::fs::OpenOptions::new();
        options.write(true);
        let offset = match mode {
            WriteMode::CreateNew => {
                options.create_new(true);
                0
            }
            WriteMode::OverwriteConfirmed => {
                options.create(true).truncate(true);
                0
            }
            WriteMode::Resume(expected) => {
                if self.identity(remote).await? != expected {
                    return Err("remote file changed; refusing resume".into());
                }
                let mut partial = tokio::fs::File::open(local).await?;
                let size = partial.metadata().await?.len();
                if size > expected.size {
                    return Err("local partial file exceeds remote file".into());
                }
                verify_prefix(&mut partial, &mut source, size, control).await?;
                size
            }
        };
        let mut destination = options.open(local).await?;
        destination.seek(io::SeekFrom::Start(offset)).await?;
        source.seek(io::SeekFrom::Start(offset)).await?;
        let result = copy(&mut source, &mut destination, offset, control).await;
        let close = source.close().await;
        let bytes = result?;
        close?;
        Ok(bytes)
    }
}

async fn verify_prefix<A: AsyncRead + Unpin, B: AsyncRead + Unpin>(
    a: &mut A,
    b: &mut B,
    mut bytes: u64,
    control: &TransferControl,
) -> Result<()> {
    let mut left = vec![0; 65536];
    let mut right = vec![0; 65536];
    while bytes > 0 {
        control.check()?;
        let n = bytes.min(left.len() as u64) as usize;
        a.read_exact(&mut left[..n]).await?;
        b.read_exact(&mut right[..n]).await?;
        if left[..n] != right[..n] {
            return Err("partial file content does not match; refusing resume".into());
        }
        bytes -= n as u64;
    }
    Ok(())
}
async fn copy<A: AsyncRead + Unpin, B: AsyncWrite + Unpin>(
    source: &mut A,
    destination: &mut B,
    offset: u64,
    control: &TransferControl,
) -> Result<u64> {
    let mut buffer = vec![0; 65536];
    let mut total = offset;
    control.bytes.store(total, Ordering::Release);
    control.batch_bytes.fetch_add(offset, Ordering::Release);
    loop {
        control.check()?;
        let bytes = source.read(&mut buffer).await?;
        if bytes == 0 {
            break;
        }
        control.check()?;
        destination.write_all(&buffer[..bytes]).await?;
        total += bytes as u64;
        control.bytes.store(total, Ordering::Release);
        control
            .batch_bytes
            .fetch_add(bytes as u64, Ordering::Release);
    }
    destination.flush().await?;
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn cancellation_preserves_prefix_without_writing() {
        let control = TransferControl::default();
        control.cancel();
        let mut output = Vec::new();
        assert!(
            copy(&mut &b"hello"[..], &mut output, 0, &control)
                .await
                .is_err()
        );
        assert!(output.is_empty());
    }
    #[tokio::test]
    async fn resume_compares_content_not_just_length() {
        let control = TransferControl::default();
        assert!(
            verify_prefix(&mut &b"abc"[..], &mut &b"abd"[..], 3, &control)
                .await
                .is_err()
        );
        assert!(
            verify_prefix(&mut &b"abcx"[..], &mut &b"abc"[..], 3, &control)
                .await
                .is_ok()
        );
    }
    #[tokio::test]
    async fn streaming_copy_reports_bytes() {
        let control = TransferControl::default();
        let data = vec![42; 200000];
        let mut output = Vec::new();
        assert_eq!(
            copy(&mut data.as_slice(), &mut output, 0, &control)
                .await
                .unwrap(),
            200000
        );
        assert_eq!(output, data);
        assert_eq!(control.transferred(), 200000);
    }
    #[tokio::test]
    async fn batch_progress_accumulates_across_files_and_resume_offsets() {
        let control = TransferControl::default();
        assert_eq!(control.total_bytes(), None);
        control.set_total(12);
        let mut output = Vec::new();
        copy(&mut &b"hello"[..], &mut output, 0, &control)
            .await
            .unwrap();
        assert_eq!(control.batch_transferred(), 5);
        copy(&mut &b"world"[..], &mut output, 2, &control)
            .await
            .unwrap();
        assert_eq!(control.transferred(), 7);
        assert_eq!(control.batch_transferred(), 12);
        assert_eq!(control.total_bytes(), Some(12));
    }
}
