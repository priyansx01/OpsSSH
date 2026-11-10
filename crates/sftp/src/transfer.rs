//! Batch copy and move with explicit conflict decisions. Never follows symbolic links,
//! never replaces across item types, and keeps completed work when a batch stops.
use super::*;
use crate::operations::{AtomicRenamer, OperationReport, RemoteAttributes};

/// Longest single path component accepted by common Linux filesystems.
pub const MAX_NAME_BYTES: usize = 255;
/// Archive suffixes kept together when numbering a duplicate name.
const COMPOUND_EXTENSIONS: [&str; 6] = [
    ".tar.gz",
    ".tar.bz2",
    ".tar.xz",
    ".tar.zst",
    ".tar.lz",
    ".tar.lzma",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferMode {
    Copy,
    Move,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictDecision {
    /// Replace a file, or merge two folders item by item.
    Replace,
    /// Place the incoming item under a new numbered name.
    KeepBoth,
    Skip,
    /// Stop the batch; completed items are kept.
    Cancel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConflictReply {
    pub decision: ConflictDecision,
    /// Reuse this decision for later conflicts of the same kind in this batch.
    pub apply_to_all: bool,
}
impl ConflictReply {
    pub fn once(decision: ConflictDecision) -> Self {
        Self {
            decision,
            apply_to_all: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictKind {
    /// Two regular files: Replace overwrites the destination.
    Files,
    /// Two real folders: Replace merges their contents.
    Folders,
    /// Different types, or a symbolic link/special file: only Keep both or Skip.
    Mismatch,
}
impl ConflictKind {
    pub fn classify(incoming: &RemoteAttributes, existing: &RemoteAttributes) -> Self {
        let folder = |a: &RemoteAttributes| a.is_directory && !a.is_symlink;
        let file = |a: &RemoteAttributes| a.is_regular && !a.is_symlink;
        if folder(incoming) && folder(existing) {
            Self::Folders
        } else if file(incoming) && file(existing) {
            Self::Files
        } else {
            Self::Mismatch
        }
    }
}

/// A destination that already holds an item with the incoming name.
#[derive(Debug, Clone)]
pub struct Conflict {
    pub source: String,
    pub destination: String,
    pub incoming: RemoteAttributes,
    pub existing: RemoteAttributes,
}
impl Conflict {
    pub fn kind(&self) -> ConflictKind {
        ConflictKind::classify(&self.incoming, &self.existing)
    }
}

/// One top-level item: its source path and the name it should have in the destination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferItem {
    pub source: String,
    pub name: String,
}
impl TransferItem {
    /// Keep the source's own name.
    pub fn named_as_source(source: &str) -> Option<Self> {
        let name = source.trim_end_matches('/').rsplit('/').next()?;
        (!name.is_empty()).then(|| Self {
            source: source.to_owned(),
            name: name.to_owned(),
        })
    }
}

/// `report.txt` → `report (2).txt`; folders and dotfiles are numbered at the end.
/// The result never exceeds [`MAX_NAME_BYTES`] and never splits a character.
pub fn numbered_name(name: &str, number: u32, directory: bool) -> String {
    let lower = name.to_ascii_lowercase();
    let extension_start = if directory {
        None
    } else {
        COMPOUND_EXTENSIONS
            .iter()
            .find(|ext| lower.ends_with(*ext) && lower.len() > ext.len())
            .map(|ext| name.len() - ext.len())
            .or_else(|| {
                name.rfind('.')
                    .filter(|&index| index > 0 && index + 1 < name.len())
            })
    };
    let (stem, extension) = match extension_start {
        Some(index) => name.split_at(index),
        None => (name, ""),
    };
    let suffix = format!(" ({number})");
    let (stem, extension) = if suffix.len() + extension.len() >= MAX_NAME_BYTES {
        (name, "")
    } else {
        (stem, extension)
    };
    let budget = MAX_NAME_BYTES - suffix.len() - extension.len();
    let mut end = stem.len().min(budget);
    while !stem.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{suffix}{extension}", &stem[..end])
}

/// Attributes of a local item, for showing it beside a remote one in a conflict.
pub(crate) fn local_attributes(metadata: &std::fs::Metadata) -> RemoteAttributes {
    RemoteAttributes {
        mode: None,
        uid: None,
        gid: None,
        is_directory: metadata.is_dir(),
        is_symlink: metadata.file_type().is_symlink(),
        is_regular: metadata.is_file(),
        size: metadata.is_file().then_some(metadata.len()),
        modified: metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .and_then(|elapsed| u32::try_from(elapsed.as_secs()).ok()),
    }
}

fn leaf(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}
pub(crate) fn parent(path: &str) -> &str {
    match path.rsplit_once('/') {
        Some(("", _)) => "/",
        Some((parent, _)) => parent,
        None => "/",
    }
}

enum Work {
    Place {
        source: String,
        target: String,
    },
    /// After a move merged a folder's contents, remove it if nothing was left behind.
    RemoveEmptySource(String),
}

struct Remembered {
    files: Option<ConflictDecision>,
    folders: Option<ConflictDecision>,
    mismatched: Option<ConflictDecision>,
}
impl Remembered {
    fn slot(&mut self, kind: ConflictKind) -> &mut Option<ConflictDecision> {
        match kind {
            ConflictKind::Files => &mut self.files,
            ConflictKind::Folders => &mut self.folders,
            ConflictKind::Mismatch => &mut self.mismatched,
        }
    }
}

impl SftpClient {
    /// Copy or move items into `destination_dir`, asking `confirm` for each name conflict.
    /// Per-item failures are reported and the batch continues; Cancel stops it.
    pub async fn transfer_items<F, Fut>(
        &self,
        items: &[TransferItem],
        destination_dir: &str,
        mode: TransferMode,
        renamer: Option<&AtomicRenamer>,
        control: &TransferControl,
        mut confirm: F,
    ) -> Result<OperationReport>
    where
        F: FnMut(Conflict) -> Fut,
        Fut: Future<Output = Result<ConflictReply>>,
    {
        let directory = self.canonicalize(destination_dir).await?;
        let destination = self.attributes(&directory).await?;
        if !destination.is_directory || destination.is_symlink {
            return Err("Destination is not a folder".into());
        }
        if mode == TransferMode::Copy {
            let mut total = 0u64;
            for item in items {
                // A vanished item is reported when it is reached; it only lacks a size here.
                if let Ok(tree) = self.tree(&item.source, true, control).await {
                    total = total.saturating_add(
                        tree.iter()
                            .filter_map(|(_, a)| a.size.filter(|_| a.is_regular))
                            .sum(),
                    );
                }
            }
            control.set_total(total);
        }
        let mut report = OperationReport::default();
        let mut remembered = Remembered {
            files: None,
            folders: None,
            mismatched: None,
        };
        'items: for item in items {
            if control.check().is_err() {
                report.cancelled = true;
                break;
            }
            let target = match opsssh_drop::join_remote(&directory, &item.name) {
                Ok(target) => target,
                Err(error) => {
                    report.failures.push(format!("{}: {error}", item.name));
                    continue;
                }
            };
            let source = match self.operation_path(&item.source).await {
                Ok(source) => source,
                Err(error) => {
                    report.failures.push(format!("{}: {error}", item.source));
                    continue;
                }
            };
            let mut stack = vec![Work::Place { source, target }];
            while let Some(work) = stack.pop() {
                if control.check().is_err() {
                    report.cancelled = true;
                    break 'items;
                }
                let (source, target) = match work {
                    Work::RemoveEmptySource(path) => {
                        if self.session.remove_dir(&path).await.is_err() {
                            report
                                .failures
                                .push(format!("{path}: kept because it still contains items"));
                        }
                        continue;
                    }
                    Work::Place { source, target } => (source, target),
                };
                let outcome = self
                    .place(
                        &source,
                        target,
                        mode,
                        renamer,
                        control,
                        &mut remembered,
                        &mut confirm,
                        &mut stack,
                        &mut report,
                    )
                    .await;
                match outcome {
                    Ok(true) => {}
                    Ok(false) => {
                        report.cancelled = true;
                        break 'items;
                    }
                    Err(error) => {
                        if control.check().is_err() {
                            report.cancelled = true;
                            break 'items;
                        }
                        report.failures.push(format!("{source}: {error}"));
                    }
                }
            }
        }
        Ok(report)
    }

    /// Returns `Ok(false)` when the user cancelled the batch.
    #[allow(clippy::too_many_arguments)]
    async fn place<F, Fut>(
        &self,
        source: &str,
        mut target: String,
        mode: TransferMode,
        renamer: Option<&AtomicRenamer>,
        control: &TransferControl,
        remembered: &mut Remembered,
        confirm: &mut F,
        stack: &mut Vec<Work>,
        report: &mut OperationReport,
    ) -> Result<bool>
    where
        F: FnMut(Conflict) -> Fut,
        Fut: Future<Output = Result<ConflictReply>>,
    {
        let incoming = self.attributes(source).await?;
        let folder = incoming.is_directory && !incoming.is_symlink;
        if target == source {
            if mode == TransferMode::Move {
                // Moving an item into its own folder changes nothing.
                report.skipped += 1;
                return Ok(true);
            }
            // Copying into the same folder always keeps both, without asking.
            target = self.free_name(parent(source), leaf(source), folder).await?;
        }
        if folder {
            let canonical = self.canonicalize(source).await?;
            if target.starts_with(&format!("{source}/"))
                || target.starts_with(&format!("{canonical}/"))
            {
                return Err("A folder cannot be placed inside itself".into());
            }
        }
        let Some(existing) = self.existing_metadata(&target).await? else {
            self.place_new(source, &target, &incoming, mode, control, report)
                .await?;
            return Ok(true);
        };
        let existing = RemoteAttributes::from(existing);
        let conflict = Conflict {
            source: source.to_owned(),
            destination: target.clone(),
            incoming: incoming.clone(),
            existing: existing.clone(),
        };
        let kind = conflict.kind();
        let decision = match *remembered.slot(kind) {
            Some(decision) => decision,
            None => {
                let reply = confirm(conflict).await?;
                if reply.apply_to_all {
                    *remembered.slot(kind) = Some(reply.decision);
                }
                reply.decision
            }
        };
        control.check()?;
        match decision {
            ConflictDecision::Cancel => {
                control.cancel();
                return Ok(false);
            }
            ConflictDecision::Skip => report.skipped += 1,
            ConflictDecision::KeepBoth => {
                let free = self
                    .free_name(parent(&target), leaf(&target), folder)
                    .await?;
                stack.push(Work::Place {
                    source: source.to_owned(),
                    target: free,
                });
            }
            ConflictDecision::Replace => match kind {
                ConflictKind::Mismatch => {
                    return Err(
                        "An item of a different type already uses this name; it was kept".into(),
                    );
                }
                ConflictKind::Files => {
                    self.replace_file(
                        source, &target, &incoming, &existing, mode, renamer, control,
                    )
                    .await?;
                    report.changed += 1;
                }
                ConflictKind::Folders => {
                    if mode == TransferMode::Move {
                        stack.push(Work::RemoveEmptySource(source.to_owned()));
                    }
                    let mut children = self.list(source).await?;
                    // The stack is last-in first-out; keep conflicts in name order.
                    children.sort_by(|a, b| b.name.cmp(&a.name));
                    for child in children {
                        stack.push(Work::Place {
                            target: opsssh_drop::join_remote(&target, &child.name)?,
                            source: child.path,
                        });
                    }
                }
            },
        }
        Ok(true)
    }

    async fn place_new(
        &self,
        source: &str,
        target: &str,
        incoming: &RemoteAttributes,
        mode: TransferMode,
        control: &TransferControl,
        report: &mut OperationReport,
    ) -> Result<()> {
        if mode == TransferMode::Move {
            control.check()?;
            match self.session.rename(source, target).await {
                Ok(()) => {
                    report.changed += 1;
                    return Ok(());
                }
                // Servers report a cross-filesystem rename as a generic failure.
                Err(SftpError::Status(status))
                    if matches!(
                        status.status_code,
                        StatusCode::Failure | StatusCode::OpUnsupported
                    ) && self.exists(source).await?
                        && !self.exists(target).await? => {}
                Err(error) => return Err(error.into()),
            }
            let copied = self.copy_new_tree(source, target, control).await?;
            let clean = copied.failures.is_empty() && !copied.cancelled;
            merge_report(report, copied);
            if !clean {
                return Err("Copied partially; the original was kept".into());
            }
            let removed = self.delete_tree(source, control).await?;
            if !removed.failures.is_empty() || removed.cancelled {
                return Err(
                    format!("Copied, but the original could not be removed: {removed}").into(),
                );
            }
            return Ok(());
        }
        self.same_type(source, incoming).await?;
        let copied = self.copy_new_tree(source, target, control).await?;
        merge_report(report, copied);
        Ok(())
    }

    /// Copy a tree to a path that does not exist yet. Every item is created exclusively.
    async fn copy_new_tree(
        &self,
        source: &str,
        target: &str,
        control: &TransferControl,
    ) -> Result<OperationReport> {
        let items = self.tree(source, true, control).await?;
        let mut report = OperationReport::default();
        let mut failed_folders: Vec<String> = vec![];
        for (path, attrs) in items {
            if control.check().is_err() {
                report.cancelled = true;
                break;
            }
            if failed_folders
                .iter()
                .any(|folder| path.starts_with(&format!("{folder}/")))
            {
                continue; // Already reported once with its folder.
            }
            let destination = format!(
                "{target}{}",
                path.strip_prefix(source).ok_or("Invalid tree path")?
            );
            let result: Result<bool> = async {
                self.same_type(&path, &attrs).await?;
                if attrs.is_symlink {
                    let link = self.session.read_link(&path).await?;
                    self.copy_symlink(&link, &destination, control).await?;
                } else if attrs.is_directory {
                    self.session.create_dir(&destination).await?;
                } else if attrs.is_regular {
                    self.copy_file(&path, &destination, &attrs, control).await?;
                } else {
                    return Ok(false);
                }
                Ok(true)
            }
            .await;
            match result {
                Ok(true) => report.changed += 1,
                Ok(false) => report.skipped += 1,
                Err(error) => {
                    if control.check().is_err() {
                        report.cancelled = true;
                        break;
                    }
                    if attrs.is_directory && !attrs.is_symlink {
                        failed_folders.push(path.clone());
                    }
                    report.failures.push(format!("{path}: {error}"));
                }
            }
        }
        Ok(report)
    }

    async fn copy_file(
        &self,
        source: &str,
        destination: &str,
        attrs: &RemoteAttributes,
        control: &TransferControl,
    ) -> Result<()> {
        let mut input = self.session.open(source).await?;
        let mut output = self
            .session
            .open_with_flags_and_attributes(
                destination,
                OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::EXCLUDE,
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
        Ok(())
    }

    /// Replace one regular file. A copy is written beside the target first, so the
    /// original is never truncated; the final swap is atomic when the server allows it.
    #[allow(clippy::too_many_arguments)]
    async fn replace_file(
        &self,
        source: &str,
        target: &str,
        incoming: &RemoteAttributes,
        existing: &RemoteAttributes,
        mode: TransferMode,
        renamer: Option<&AtomicRenamer>,
        control: &TransferControl,
    ) -> Result<()> {
        if self.attributes(source).await? != *incoming
            || self.attributes(target).await? != *existing
        {
            return Err("Source or destination changed during confirmation; review again".into());
        }
        let staged = match mode {
            TransferMode::Move => None,
            TransferMode::Copy => {
                let temporary = opsssh_drop::join_remote(
                    parent(target),
                    &format!(".opsssh-copy-{}.tmp", uuid::Uuid::new_v4()),
                )?;
                if let Err(error) = self.copy_file(source, &temporary, incoming, control).await {
                    let _ = self.session.remove_file(&temporary).await;
                    return Err(error);
                }
                Some(temporary)
            }
        };
        let replacement = staged.as_deref().unwrap_or(source);
        let result = match renamer {
            Some(renamer) => {
                let attrs = self.attributes(replacement).await?;
                renamer
                    .replace(replacement, target, &attrs, existing, control)
                    .await
            }
            None => self.swap_into_place(replacement, target, control).await,
        };
        if result.is_err()
            && let Some(temporary) = &staged
        {
            let _ = self.session.remove_file(temporary).await;
        }
        result
    }

    /// Plain SFTP rename never overwrites, so set the original aside, move the new file in,
    /// and restore the original if that fails.
    async fn swap_into_place(
        &self,
        replacement: &str,
        target: &str,
        control: &TransferControl,
    ) -> Result<()> {
        control.check()?;
        let backup = opsssh_drop::join_remote(
            parent(target),
            &format!(".opsssh-replaced-{}.tmp", uuid::Uuid::new_v4()),
        )?;
        self.session.rename(target, &backup).await?;
        if let Err(error) = self.session.rename(replacement, target).await {
            return match self.session.rename(&backup, target).await {
                Ok(()) => Err(error.into()),
                Err(_) => Err(format!(
                    "Replacement failed ({error}); the original is preserved as {backup}"
                )
                .into()),
            };
        }
        if self.session.remove_file(&backup).await.is_err() {
            return Err(format!("Replaced, but the previous file remains at {backup}").into());
        }
        Ok(())
    }

    /// First unused numbered name in `directory`.
    pub(crate) async fn free_name(
        &self,
        directory: &str,
        name: &str,
        folder: bool,
    ) -> Result<String> {
        for number in 2..=10_000 {
            let candidate =
                opsssh_drop::join_remote(directory, &numbered_name(name, number, folder))?;
            if self.existing_metadata(&candidate).await?.is_none() {
                return Ok(candidate);
            }
        }
        Err("Too many items already use this name".into())
    }
}

fn merge_report(into: &mut OperationReport, from: OperationReport) {
    into.changed += from.changed;
    into.skipped += from.skipped;
    into.failures.extend(from.failures);
    into.cancelled |= from.cancelled;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbered_names_keep_extensions_dotfiles_and_archives() {
        assert_eq!(numbered_name("report.txt", 2, false), "report (2).txt");
        assert_eq!(numbered_name("report", 3, false), "report (3)");
        assert_eq!(numbered_name(".bashrc", 2, false), ".bashrc (2)");
        assert_eq!(numbered_name(".config.json", 2, false), ".config (2).json");
        assert_eq!(
            numbered_name("backup.TAR.GZ", 2, false),
            "backup (2).TAR.GZ"
        );
        assert_eq!(numbered_name("v1.2", 2, true), "v1.2 (2)");
        assert_eq!(numbered_name("trailing.", 2, false), "trailing. (2)");
        assert_eq!(numbered_name("雪.txt", 2, false), "雪 (2).txt");
    }

    #[test]
    fn numbered_names_fit_the_filesystem_limit_without_splitting_characters() {
        let long = format!("{}.log", "雪".repeat(100));
        let numbered = numbered_name(&long, 12, false);
        assert!(numbered.len() <= MAX_NAME_BYTES, "{}", numbered.len());
        assert!(numbered.ends_with(" (12).log"));
        let extension = format!("a.{}", "x".repeat(260));
        assert!(numbered_name(&extension, 2, false).len() <= MAX_NAME_BYTES);
    }

    #[test]
    fn conflicts_never_replace_across_types() {
        let attrs = |directory: bool, regular: bool, symlink: bool| RemoteAttributes {
            mode: None,
            uid: None,
            gid: None,
            is_directory: directory,
            is_symlink: symlink,
            is_regular: regular,
            size: None,
            modified: None,
        };
        let file = attrs(false, true, false);
        let folder = attrs(true, false, false);
        let link = attrs(false, false, true);
        assert_eq!(ConflictKind::classify(&file, &file), ConflictKind::Files);
        assert_eq!(
            ConflictKind::classify(&folder, &folder),
            ConflictKind::Folders
        );
        assert_eq!(
            ConflictKind::classify(&file, &folder),
            ConflictKind::Mismatch
        );
        assert_eq!(ConflictKind::classify(&link, &file), ConflictKind::Mismatch);
        assert_eq!(
            TransferItem::named_as_source("/srv/app/").unwrap().name,
            "app"
        );
        assert!(TransferItem::named_as_source("/").is_none());
        assert_eq!(parent("/a"), "/");
        assert_eq!(parent("/a/b"), "/a");
    }
}
