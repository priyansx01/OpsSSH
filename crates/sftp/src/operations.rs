//! Explicit file operations. Recursion never follows a symbolic link.
use super::*;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteAttributes {
    pub mode: Option<u32>,
    pub uid: Option<u32>,
    pub gid: Option<u32>,
    pub is_directory: bool,
    pub is_symlink: bool,
    pub is_regular: bool,
    pub size: Option<u64>,
    pub modified: Option<u32>,
}
impl From<FileAttributes> for RemoteAttributes {
    fn from(a: FileAttributes) -> Self {
        Self {
            mode: a.permissions.map(|m| m & 0o7777),
            uid: a.uid,
            gid: a.gid,
            is_directory: a.is_dir(),
            is_symlink: a.is_symlink(),
            is_regular: a.is_regular(),
            size: a.size,
            modified: a.mtime,
        }
    }
}
/// OpenSSH atomic rename on a separate channel of the same verified SSH transport.
/// No shell or cross-filesystem copy/delete fallback is used.
pub struct AtomicRenamer(russh_sftp::client::RawSftpSession);
impl fmt::Debug for AtomicRenamer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AtomicRenamer").finish_non_exhaustive()
    }
}
impl AtomicRenamer {
    pub async fn connect<S>(stream: S) -> Result<Self>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let session = russh_sftp::client::RawSftpSession::new(stream);
        session.set_timeout(10);
        let version = session.init().await?;
        if version
            .extensions
            .get("posix-rename@openssh.com")
            .is_none_or(|v| v != "1")
        {
            return Err(
                "Atomic replacement is unavailable on this server; choose another destination name"
                    .into(),
            );
        }
        Ok(Self(session))
    }
    pub async fn replace(
        &self,
        source: &str,
        target: &str,
        expected_source: &RemoteAttributes,
        expected_target: &RemoteAttributes,
        control: &TransferControl,
    ) -> Result<()> {
        control.check()?;
        if source == target || !expected_source.is_regular || !expected_target.is_regular {
            return Err("Only distinct regular files may be atomically replaced".into());
        }
        let actual_source = RemoteAttributes::from(self.0.lstat(source).await?.attrs);
        let actual_target = RemoteAttributes::from(self.0.lstat(target).await?.attrs);
        if actual_source != *expected_source || actual_target != *expected_target {
            return Err("Source or destination changed during confirmation; review again".into());
        }
        control.check()?;
        let mut payload = Vec::new();
        for path in [source, target] {
            payload.extend_from_slice(&u32::try_from(path.len())?.to_be_bytes());
            payload.extend_from_slice(path.as_bytes());
        }
        match self.0.extended("posix-rename@openssh.com", payload).await? {
            russh_sftp::protocol::Packet::Status(status)
                if status.status_code == StatusCode::Ok =>
            {
                Ok(())
            }
            russh_sftp::protocol::Packet::Status(status) => Err(format!(
                "Atomic move failed: {:?} ({}). No copy/delete fallback was attempted.",
                status.status_code, status.error_message
            )
            .into()),
            _ => Err("Server returned an unexpected atomic rename response".into()),
        }
    }
}
#[derive(Debug, Clone, Default)]
pub struct PermissionChange {
    pub mode: Option<u32>,
    pub uid: Option<u32>,
    pub gid: Option<u32>,
    pub recursive: bool,
}
#[derive(Debug, Clone, Default)]
pub struct OperationReport {
    pub changed: usize,
    pub skipped: usize,
    pub failures: Vec<String>,
    pub cancelled: bool,
}
impl fmt::Display for OperationReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} completed, {} skipped, {} failed{}",
            self.changed,
            self.skipped,
            self.failures.len(),
            if self.cancelled {
                "; cancelled (completed changes retained)"
            } else {
                ""
            }
        )?;
        for error in self.failures.iter().take(3) {
            write!(f, "\n{error}")?;
        }
        Ok(())
    }
}
pub fn parse_mode(value: &str) -> Result<u32> {
    if !(3..=4).contains(&value.len()) || !value.bytes().all(|b| (b'0'..=b'7').contains(&b)) {
        return Err("Enter 3 or 4 octal digits (000–7777)".into());
    }
    Ok(u32::from_str_radix(value, 8)?)
}
pub fn symbolic_mode(mode: u32) -> String {
    let mut s = String::new();
    for shift in [6, 3, 0] {
        s.push(if mode & (4 << shift) != 0 { 'r' } else { '-' });
        s.push(if mode & (2 << shift) != 0 { 'w' } else { '-' });
        let exec = mode & (1 << shift) != 0;
        let special =
            mode & match shift {
                6 => 0o4000,
                3 => 0o2000,
                _ => 0o1000,
            } != 0;
        s.push(match (special, exec, shift) {
            (true, true, 0) => 't',
            (true, false, 0) => 'T',
            (true, true, _) => 's',
            (true, false, _) => 'S',
            (false, true, _) => 'x',
            _ => '-',
        });
    }
    s
}
impl SftpClient {
    /// OpenSSH reverses v3's two symlink strings. Never guess using a real target:
    /// negotiate inside an exclusively-created private directory first.
    /// https://github.com/openssh/openssh-portable/blob/master/sftp-server.c
    async fn symlink_order(&self, parent: &str, control: &TransferControl) -> Result<bool> {
        let mut cached = self.symlink_target_first.lock().await;
        if let Some(order) = *cached {
            return Ok(order);
        }
        control.check()?;
        let probe = self.private_staging(parent).await?;
        let existing = opsssh_drop::join_remote(&probe, "existing")?;
        let link = opsssh_drop::join_remote(&probe, "link")?;
        let result: Result<bool> = async {
            let file = self
                .session
                .open_with_flags_and_attributes(
                    &existing,
                    OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::EXCLUDE,
                    FileAttributes {
                        permissions: Some(0o600),
                        ..FileAttributes::empty()
                    },
                )
                .await?;
            file.close().await?;
            // On a draft-order server this must fail because the first string exists.
            let first = self.session.symlink(&existing, &link).await;
            let target_first = if first.is_ok() {
                true
            } else {
                if self.exists(&link).await? {
                    return Err("Unexpected symlink probe result".into());
                }
                self.session.symlink(&link, &existing).await?;
                false
            };
            if !self.attributes(&link).await?.is_symlink
                || self.session.read_link(&link).await? != existing
            {
                return Err("Server symlink ordering could not be verified".into());
            }
            Ok(target_first)
        }
        .await;
        // All possible probe writes are contained in this private directory.
        if self.exists(&link).await? {
            self.session.remove_file(&link).await?;
        }
        if self.exists(&existing).await? {
            self.session.remove_file(&existing).await?;
        }
        self.session.remove_dir(&probe).await?;
        let order = result?;
        control.check()?;
        *cached = Some(order);
        Ok(order)
    }
    async fn copy_symlink(
        &self,
        target: &str,
        destination: &str,
        control: &TransferControl,
    ) -> Result<()> {
        let (parent, _) = destination
            .rsplit_once('/')
            .ok_or("Invalid symlink destination")?;
        let target_first = self
            .symlink_order(if parent.is_empty() { "/" } else { parent }, control)
            .await?;
        control.check()?;
        if target_first {
            self.session.symlink(target, destination).await?;
        } else {
            self.session.symlink(destination, target).await?;
        }
        if !self.attributes(destination).await?.is_symlink
            || self.session.read_link(destination).await? != target
        {
            return Err("Server did not preserve the symlink target".into());
        }
        Ok(())
    }
    pub async fn attributes(&self, path: &str) -> Result<RemoteAttributes> {
        Ok(self.session.symlink_metadata(path).await?.into())
    }
    pub async fn exists(&self, path: &str) -> Result<bool> {
        match self.session.symlink_metadata(path).await {
            Ok(_) => Ok(true),
            Err(SftpError::Status(status)) if status.status_code == StatusCode::NoSuchFile => {
                Ok(false)
            }
            Err(e) => Err(e.into()),
        }
    }
    /// Canonicalize the parent, leaving the final symlink itself intact.
    pub async fn operation_path(&self, path: &str) -> Result<String> {
        let (parent, leaf) = path
            .trim_end_matches('/')
            .rsplit_once('/')
            .ok_or("Use an absolute remote path")?;
        let parent = self
            .canonicalize(if parent.is_empty() { "/" } else { parent })
            .await?;
        Ok(opsssh_drop::join_remote(&parent, leaf)?)
    }
    async fn tree(
        &self,
        root: &str,
        recursive: bool,
        control: &TransferControl,
    ) -> Result<Vec<(String, RemoteAttributes)>> {
        let mut pending = vec![root.to_owned()];
        let mut found = vec![];
        while let Some(path) = pending.pop() {
            control.check()?;
            let attrs = self.attributes(&path).await?;
            if recursive && attrs.is_directory && !attrs.is_symlink {
                for e in self.list(&path).await? {
                    pending.push(e.path);
                }
            }
            found.push((path, attrs));
        }
        Ok(found)
    }
    async fn same_type(&self, path: &str, expected: &RemoteAttributes) -> Result<RemoteAttributes> {
        let fresh = self.attributes(path).await?;
        if fresh.is_directory != expected.is_directory
            || fresh.is_symlink != expected.is_symlink
            || fresh.is_regular != expected.is_regular
        {
            return Err("Item type changed; reopen the action".into());
        }
        Ok(fresh)
    }
    pub async fn change_permissions(
        &self,
        path: &str,
        expected: &RemoteAttributes,
        change: &PermissionChange,
        control: &TransferControl,
    ) -> Result<OperationReport> {
        if change.mode.is_some_and(|m| m > 0o7777) {
            return Err("Invalid permission mode".into());
        }
        self.same_type(path, expected).await?;
        if expected.is_symlink {
            return Err("Open the actual target to change its permissions".into());
        }
        let mut items = self.tree(path, change.recursive, control).await?;
        items.reverse(); // descendants before directories, preserving traversal access
        let mut report = OperationReport::default();
        for (path, attrs) in items {
            if control.check().is_err() {
                report.cancelled = true;
                break;
            }
            if attrs.is_symlink || (!attrs.is_directory && !attrs.is_regular) {
                report.skipped += 1;
                continue;
            }
            let result: Result<()> = async {
                let fresh = self.same_type(&path, &attrs).await?;
                if change.uid.is_some() || change.gid.is_some() {
                    self.session
                        .set_metadata(
                            &path,
                            FileAttributes {
                                uid: Some(change.uid.or(fresh.uid).ok_or("Server omitted UID")?),
                                gid: Some(change.gid.or(fresh.gid).ok_or("Server omitted GID")?),
                                ..FileAttributes::empty()
                            },
                        )
                        .await?;
                }
                if let Some(mode) = change.mode {
                    self.session
                        .set_metadata(
                            &path,
                            FileAttributes {
                                permissions: Some(mode),
                                ..FileAttributes::empty()
                            },
                        )
                        .await?;
                }
                Ok(())
            }
            .await;
            match result {
                Ok(()) => report.changed += 1,
                Err(e) => report.failures.push(format!("{path}: {e}")),
            }
        }
        Ok(report)
    }
    pub async fn delete_tree(
        &self,
        path: &str,
        control: &TransferControl,
    ) -> Result<OperationReport> {
        let path = self.operation_path(path).await?; // refuses filesystem root
        let mut items = self.tree(&path, true, control).await?;
        items.reverse();
        let mut report = OperationReport::default();
        for (path, attrs) in items {
            if control.check().is_err() {
                report.cancelled = true;
                break;
            }
            let result: Result<()> = async {
                self.same_type(&path, &attrs).await?;
                if attrs.is_directory && !attrs.is_symlink {
                    self.session.remove_dir(&path).await?;
                } else {
                    self.session.remove_file(&path).await?;
                }
                Ok(())
            }
            .await;
            match result {
                Ok(()) => report.changed += 1,
                Err(e) => report.failures.push(format!("{path}: {e}")),
            }
        }
        Ok(report)
    }
    pub async fn transfer_target(&self, source: &str, target: &str) -> Result<(String, String)> {
        let source = self.operation_path(source).await?;
        let target = self.operation_path(target).await?;
        if target == source || target.starts_with(&format!("{source}/")) {
            return Err("Choose a destination outside the source folder".into());
        }
        // A destination parent canonicalized through an alias must also be outside the source.
        if self.attributes(&source).await?.is_directory {
            let canonical = self.canonicalize(&source).await?;
            if target.starts_with(&format!("{canonical}/")) {
                return Err("Destination is inside the source folder".into());
            }
        }
        Ok((source, target))
    }
    pub async fn rename_item(
        &self,
        source: &str,
        target: &str,
        control: &TransferControl,
    ) -> Result<()> {
        let (source, target) = self.transfer_target(source, target).await?;
        control.check()?;
        if self.exists(&target).await? {
            return Err("Destination exists. Atomic replacement is unavailable on this SFTP connection; choose another name.".into());
        }
        self.session.rename(source, target).await?;
        Ok(())
    }
    pub async fn copy_tree(
        &self,
        source: &str,
        target: &str,
        replace: bool,
        control: &TransferControl,
    ) -> Result<OperationReport> {
        let (source, target) = self.transfer_target(source, target).await?;
        let items = self.tree(&source, true, control).await?;
        let existing = if self.exists(&target).await? {
            Some(self.attributes(&target).await?)
        } else {
            None
        };
        if let Some(existing) = &existing
            && (items[0].1.is_directory
                || existing.is_directory
                || existing.is_symlink
                || items[0].1.is_symlink
                || !existing.is_regular
                || !items[0].1.is_regular
                || !replace)
        {
            return Err(
                "Destination exists; choose another name (folders are never merged)".into(),
            );
        }
        control.set_total(
            items
                .iter()
                .filter_map(|(_, a)| a.size.filter(|_| a.is_regular))
                .sum(),
        );
        let mut report = OperationReport::default();
        for (path, attrs) in items {
            if control.check().is_err() {
                report.cancelled = true;
                break;
            }
            let destination = format!(
                "{target}{}",
                path.strip_prefix(&source).ok_or("Invalid tree path")?
            );
            let result: Result<bool> = async {
                self.same_type(&path, &attrs).await?;
                if let Some(expected) = &existing
                    && path == source
                {
                    self.same_type(&destination, expected).await?;
                }
                if attrs.is_symlink {
                    let link = self.session.read_link(&path).await?;
                    self.copy_symlink(&link, &destination, control).await?;
                } else if attrs.is_directory {
                    self.session.create_dir(&destination).await?;
                } else if attrs.is_regular {
                    let mut input = self.session.open(&path).await?;
                    let flags = if existing.is_some() && path == source {
                        OpenFlags::WRITE | OpenFlags::TRUNCATE
                    } else {
                        OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::EXCLUDE
                    };
                    let mut output = self
                        .session
                        .open_with_flags_and_attributes(
                            &destination,
                            flags,
                            FileAttributes {
                                permissions: Some(attrs.mode.unwrap_or(0o600) & 0o777),
                                ..FileAttributes::empty()
                            },
                        )
                        .await?;
                    let result = copy(&mut input, &mut output, 0, control).await;
                    let input_close = input.close().await;
                    let output_close = output.close().await;
                    result?;
                    input_close?;
                    output_close?;
                } else {
                    return Ok(false);
                }
                Ok(true)
            }
            .await;
            match result {
                Ok(true) => report.changed += 1,
                Ok(false) => report.skipped += 1,
                Err(e) => {
                    report.failures.push(format!("{path}: {e}"));
                    report.cancelled = control.check().is_err();
                    break;
                }
            }
        }
        Ok(report)
    }
    pub async fn download_tree(
        &self,
        source: &str,
        local: &Path,
        control: &TransferControl,
    ) -> Result<OperationReport> {
        let items = self.tree(source, true, control).await?;
        control.set_total(
            items
                .iter()
                .filter_map(|(_, a)| a.size.filter(|_| a.is_regular))
                .sum(),
        );
        let mut report = OperationReport::default();
        for (path, attrs) in items {
            if control.check().is_err() {
                report.cancelled = true;
                break;
            }
            if attrs.is_symlink || (!attrs.is_directory && !attrs.is_regular) {
                report.skipped += 1;
                continue;
            }
            let mut destination = PathBuf::from(local);
            let relative = path
                .strip_prefix(source)
                .ok_or("Invalid tree path")?
                .trim_start_matches('/');
            for part in relative.split('/').filter(|p| !p.is_empty()) {
                // A remote filename must remain a single safe local path component.
                if !safe_local_component(part) {
                    return Err(format!(
                        "Filename cannot be safely downloaded on this platform: {part}"
                    )
                    .into());
                }
                destination.push(part);
            }
            let result: Result<()> = async {
                self.same_type(&path, &attrs).await?;
                if attrs.is_directory {
                    tokio::fs::create_dir(&destination).await?;
                } else {
                    self.download(&path, &destination, WriteMode::CreateNew, control)
                        .await?;
                }
                Ok(())
            }
            .await;
            match result {
                Ok(()) => report.changed += 1,
                Err(e) => {
                    report.failures.push(format!("{path}: {e}"));
                    report.cancelled = control.check().is_err();
                    break;
                }
            }
        }
        Ok(report)
    }
}

fn safe_local_component(part: &str) -> bool {
    if part.is_empty() || part == "." || part == ".." || part.contains(['/', '\\', '\0']) {
        return false;
    }
    if cfg!(windows) {
        let base = part
            .split('.')
            .next()
            .unwrap_or_default()
            .to_ascii_uppercase();
        if part.contains([':', '<', '>', '"', '|', '?', '*'])
            || part.ends_with(['.', ' '])
            || part.chars().any(char::is_control)
            || matches!(base.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || ["COM", "LPT"].iter().any(|p| {
                base.strip_prefix(p).is_some_and(|s| {
                    matches!(s, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")
                })
            })
        {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn modes_include_special_bits_and_reject_invalid_input() {
        assert_eq!(parse_mode("755").unwrap(), 0o755);
        assert_eq!(symbolic_mode(0o755), "rwxr-xr-x");
        assert_eq!(symbolic_mode(0o4755), "rwsr-xr-x");
        assert_eq!(symbolic_mode(0o2640), "rw-r-S---");
        assert_eq!(symbolic_mode(0o1777), "rwxrwxrwt");
        for value in ["", "78", "888", "10000", " 755", "755\n"] {
            assert!(parse_mode(value).is_err());
        }
    }
}
