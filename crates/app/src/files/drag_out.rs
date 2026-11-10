//! Dragging remote items out to the desktop. Files are downloaded into a private
//! temporary folder first; the operating system only ever receives finished files.
//!
//! Experimental until verified on each platform: set `OPSSSH_EXPERIMENTAL_DRAG_OUT=1`.
use super::*;
use std::sync::{Arc, Mutex};

/// Larger selections are not staged; Download remains the way to fetch them.
pub(super) const STAGE_LIMIT: u64 = 1024 * 1024 * 1024;
/// Selections this small are staged as soon as a drag starts, so they are ready to leave
/// the window immediately.
pub(super) const EAGER_LIMIT: u64 = 32 * 1024 * 1024;
const STAGE_ROOT: &str = "opsssh-drag";
/// Leftovers from earlier sessions are removed after this long.
const STALE_AFTER: Duration = Duration::from_secs(24 * 60 * 60);

pub(super) fn enabled() -> bool {
    std::env::var_os("OPSSSH_EXPERIMENTAL_DRAG_OUT").is_some_and(|value| value == "1")
}

#[derive(Debug, Default)]
pub(super) enum Stage {
    #[default]
    Idle,
    Preparing(TransferControl),
    Ready(Vec<PathBuf>),
    TooLarge,
    Failed(String),
}
pub(super) type SharedStage = Arc<Mutex<Stage>>;

pub(super) fn lock(stage: &SharedStage) -> std::sync::MutexGuard<'_, Stage> {
    stage.lock().unwrap_or_else(|error| error.into_inner())
}

/// Short status for the drag preview, if staging has started.
pub(super) fn status(stage: &SharedStage) -> Option<String> {
    Some(match &*lock(stage) {
        Stage::Idle => return None,
        Stage::Preparing(control) => match control.total_bytes().filter(|total| *total > 0) {
            Some(total) => format!(
                "{} {:.0}%",
                tr("file-drag-out-preparing"),
                (control.batch_transferred() as f64 / total as f64 * 100.).min(99.)
            ),
            None => tr("file-drag-out-preparing"),
        },
        Stage::Ready(_) => tr("file-drag-out-ready"),
        Stage::TooLarge => tr("file-drag-out-too-large"),
        Stage::Failed(error) => error.clone(),
    })
}

fn stage_root() -> PathBuf {
    std::env::temp_dir().join(STAGE_ROOT)
}

/// Remove staging folders left by earlier sessions.
pub(super) fn sweep_stale() {
    let Ok(entries) = std::fs::read_dir(stage_root()) else {
        return;
    };
    for entry in entries.flatten() {
        let old = entry
            .metadata()
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|modified| modified.elapsed().ok())
            .is_some_and(|age| age > STALE_AFTER);
        if old {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

impl FilePane {
    /// Queue a download of the dragged items into a fresh private folder.
    pub(super) fn start_stage(&mut self, items: &dnd::DraggedItems, cx: &mut Context<Self>) {
        let mut stage = lock(&items.stage);
        if !matches!(*stage, Stage::Idle) || !self.available {
            return;
        }
        let folder = stage_root().join(uuid::Uuid::new_v4().to_string());
        let control = TransferControl::default();
        *stage = Stage::Preparing(control.clone());
        drop(stage);
        self.staged_folders.push(folder.clone());
        self.enqueue(
            Job::Stage {
                paths: items.paths.clone(),
                folder,
                stage: items.stage.clone(),
                control,
            },
            cx,
        );
    }
    /// Small file-only selections are staged immediately; others wait until the drag
    /// leaves the window.
    pub(super) fn eager_stage(&self, items: &dnd::DraggedItems) -> bool {
        items.folders.is_empty()
            && items
                .paths
                .iter()
                .map(|path| self.entry_for(path).and_then(|entry| entry.size))
                .try_fold(0u64, |total, size| Some(total.saturating_add(size?)))
                .is_some_and(|total| total <= EAGER_LIMIT)
    }
}

/// Runs on the transfer worker.
pub(super) async fn stage(
    client: &SftpClient,
    paths: &[String],
    folder: &std::path::Path,
    stage: &SharedStage,
    control: &TransferControl,
) -> opsssh_sftp::Result<()> {
    let result = async {
        let mut total = 0u64;
        for path in paths {
            total = total.saturating_add(client.tree_size(path, control).await?);
            if total > STAGE_LIMIT {
                *lock(stage) = Stage::TooLarge;
                return Ok(());
            }
        }
        control.set_total(total);
        tokio::fs::create_dir_all(folder).await?;
        let mut staged = vec![];
        for path in paths {
            let name = path.rsplit('/').next().unwrap_or_default();
            let local = folder.join(name);
            let report = client.download_tree(path, &local, control).await?;
            actions::report_result(&report)?;
            staged.push(local);
        }
        *lock(stage) = Stage::Ready(staged);
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    };
    if let Err(error) = result.await {
        let _ = tokio::fs::remove_dir_all(folder).await;
        *lock(stage) = Stage::Failed(error.to_string());
        return Err(error);
    }
    Ok(())
}

/// Called once when a drag of remote items leaves the window.
/// GPUI asks for a payload once when the drag leaves the window, on platforms where it
/// can start native drags (macOS, Linux Wayland). Windows is handled by
/// [`FilePane::drag_left_window`].
pub(super) fn leave_window(
    items: &dnd::DraggedItems,
    _window: &mut Window,
    _cx: &mut gpui::App,
    _pane: gpui::WeakEntity<FilePane>,
) -> Option<gpui::ExternalDragPayload> {
    if !enabled() {
        return None;
    }
    let ready = match &*lock(&items.stage) {
        Stage::Ready(paths) => Some(paths.clone()),
        _ => None,
    };
    {
        // The platform asks only once, so only finished files can leave the window.
        ready.map(|paths| {
            gpui::ExternalDragPayload::Files(gpui::FileDragPaths::new(paths.into_iter().map(
                |path| {
                    let directory = path.is_dir();
                    (path, directory)
                },
            )))
        })
    }
}

impl FilePane {
    /// GPUI keeps the mouse captured during a drag on Windows, so moves outside the
    /// window still arrive here. The first one starts the shell's drag.
    pub(super) fn drag_left_window(
        &mut self,
        items: &dnd::DraggedItems,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !cfg!(target_os = "windows")
            || !enabled()
            || items
                .left_window
                .swap(true, std::sync::atomic::Ordering::AcqRel)
        {
            return;
        }
        #[cfg(target_os = "windows")]
        self.start_windows_drag(items, window, cx);
        #[cfg(not(target_os = "windows"))]
        let _ = window;
    }

    /// Start the shell's drag at once. Prepared files are offered when ready; otherwise
    /// empty stand-ins give the drag its appearance and are never copied anywhere.
    #[cfg(target_os = "windows")]
    fn start_windows_drag(
        &mut self,
        items: &dnd::DraggedItems,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use opsssh_platform::file_drag::{self, DragOutcome};
        let prepared = match &*lock(&items.stage) {
            Stage::Ready(paths) => Some(paths.clone()),
            _ => None,
        };
        let files_ready = prepared.is_some();
        let (paths, stand_ins) = match prepared {
            Some(paths) => (paths, None),
            None => match stand_ins(&items.paths, &items.folders) {
                Ok((folder, paths)) => (paths, Some(folder)),
                Err(error) => {
                    self.message = error;
                    cx.notify();
                    return;
                }
            },
        };
        let remote = items.paths.clone();
        let generation = items.generation;
        let stage = items.stage.clone();
        let handle = window.window_handle();
        cx.spawn(async move |pane, cx| {
            let _ = handle.update(cx, |_, window, cx| {
                cx.stop_active_drag(window);
            });
            // No GPUI state is borrowed while the shell runs its modal drag loop.
            let outcome = file_drag::start_file_drag(&paths, files_ready);
            if let Some(folder) = stand_ins {
                let _ = std::fs::remove_dir_all(folder);
            }
            if let Stage::Preparing(control) = &*lock(&stage) {
                control.cancel();
            }
            let _ = pane.update(cx, |pane, cx| {
                pane.message = match outcome {
                    Ok(DragOutcome::Folder(folder)) => {
                        pane.download_into(remote, generation, &folder, cx)
                    }
                    Ok(DragOutcome::Dropped) => tr("file-drag-out-done"),
                    Ok(DragOutcome::NeedsPreparedFiles) => tr("file-drag-out-other-app"),
                    Ok(DragOutcome::Cancelled) => tr("file-drag-out-cancelled"),
                    Err(error) => error,
                };
                cx.notify();
            });
        })
        .detach();
    }

    /// Download dragged items into a local folder chosen by dropping on it. Existing
    /// files are asked about in the usual conflict dialog.
    pub(super) fn download_into(
        &mut self,
        remote: Vec<String>,
        generation: u64,
        folder: &std::path::Path,
        cx: &mut Context<Self>,
    ) -> String {
        if !self.available || generation != self.worker_generation {
            return tr("file-reconnect-first");
        }
        let mut queued = 0;
        for path in remote {
            let Some(name) = path.rsplit('/').next().filter(|name| !name.is_empty()) else {
                continue;
            };
            if self.entry_for(&path).is_some_and(|entry| entry.is_symlink) {
                continue;
            }
            self.enqueue(
                Job::Download {
                    local: folder.join(name),
                    remote: path,
                    confirmed_existing: false,
                    control: TransferControl::default(),
                },
                cx,
            );
            queued += 1;
        }
        format!(
            "{} {queued} → {}",
            tr("file-drag-out-downloading"),
            folder.display()
        )
    }
}

/// Empty files and folders named like the dragged items, in a private folder.
fn stand_ins(paths: &[String], folders: &[String]) -> Result<(PathBuf, Vec<PathBuf>), String> {
    let folder = stage_root().join(format!("drag-{}", uuid::Uuid::new_v4()));
    let create = || -> std::io::Result<Vec<PathBuf>> {
        std::fs::create_dir_all(&folder)?;
        let mut created = vec![];
        for path in paths {
            let name = path.rsplit('/').next().unwrap_or_default();
            if name.is_empty() || name.contains(['\\', ':']) {
                continue;
            }
            let local = folder.join(name);
            // A name Windows rejects is left out; the other items can still be dragged.
            let made = if folders.contains(path) {
                std::fs::create_dir(&local)
            } else {
                std::fs::File::create(&local).map(drop)
            };
            if made.is_ok() {
                created.push(local);
            }
        }
        Ok(created)
    };
    match create() {
        Ok(created) if !created.is_empty() => Ok((folder, created)),
        Ok(_) => {
            let _ = std::fs::remove_dir_all(&folder);
            Err(tr("file-drag-out-unsupported-name"))
        }
        Err(error) => {
            let _ = std::fs::remove_dir_all(&folder);
            Err(error.to_string())
        }
    }
}
