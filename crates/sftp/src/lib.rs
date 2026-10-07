//! Real SFTP over an already authenticated SSH channel. No credentials are owned here.
use russh_sftp::{
    client::SftpSession,
    protocol::{FileAttributes, OpenFlags},
};
use std::{
    fmt, io,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncSeekExt, AsyncWrite, AsyncWriteExt};

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
#[derive(Debug, Clone, Default)]
pub struct TransferControl {
    cancelled: Arc<AtomicBool>,
    bytes: Arc<AtomicU64>,
}
impl TransferControl {
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }
    pub fn transferred(&self) -> u64 {
        self.bytes.load(Ordering::Acquire)
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
        let mut pending = vec![(local.to_path_buf(), remote.to_string())];
        let mut total = 0;
        while let Some((source, destination)) = pending.pop() {
            control.check()?;
            let metadata = tokio::fs::symlink_metadata(&source).await?;
            if metadata.is_symlink() {
                return Err("recursive upload does not follow symbolic links".into());
            }
            if metadata.is_dir() {
                self.create_directory(&destination).await?;
                let mut entries = tokio::fs::read_dir(&source).await?;
                while let Some(entry) = entries.next_entry().await? {
                    let name = entry
                        .file_name()
                        .into_string()
                        .map_err(|_| "non-Unicode filenames are not supported")?;
                    pending.push((entry.path(), opsssh_drop::join_remote(&destination, &name)?));
                }
            } else if metadata.is_file() {
                total += self
                    .upload(&source, &destination, WriteMode::CreateNew, control)
                    .await?;
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
        let mut destination = self
            .session
            .open_with_flags_and_attributes(
                remote,
                flags,
                FileAttributes {
                    permissions: Some(0o600),
                    ..FileAttributes::empty()
                },
            )
            .await?;
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
}
