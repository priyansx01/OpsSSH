//! SFTP pane: all protocol, disk and transfer work runs on one background runtime.
use crate::localization::text as tr;
use async_channel::{Receiver, Sender};
use gpui::{
    ClipboardEntry, ClipboardItem, Context, Entity, EventEmitter, PathPromptOptions, Render, Task,
    Window, div, prelude::*, px, uniform_list,
};
use gpui_component::{
    ActiveTheme, Disableable, Icon, IconName, Selectable, Sizable, WindowExt,
    button::{Button, ButtonVariants},
    input::{Input, InputEvent, InputState},
    menu::{ContextMenuExt, DropdownMenu, PopupMenu, PopupMenuItem},
};
use opsssh_sftp::{
    Conflict, ConflictDecision, ConflictReply, Entry, PermissionChange, RemoteAttributes,
    SftpClient, TransferControl, TransferItem, TransferMode,
};
use opsssh_ssh_core::{ExecRequest, SshCommand};
use std::{path::PathBuf, time::Duration};
mod actions;
mod browser;
mod conflict;
mod dnd;
mod places;
mod selection;
use actions::FileEditor;
use browser::{FileClipboard, History, InlineRename, Navigation};
use conflict::PendingConflict;

pub enum FilePaneEvent {
    ClosePanel,
    InsertText(u64, String),
    FocusTerminal,
    DestinationValidated(u64, String),
    DestinationFailed(u64, String),
    ChooseUploadDestination,
    OpenTerminal {
        directory: String,
        new_tab: bool,
    },
    /// The user changed this server's Places bookmarks; persist them.
    BookmarksChanged(Vec<String>),
}
#[derive(Clone)]
enum Job {
    List(String),
    ValidateDestination {
        request: u64,
        path: String,
    },
    Upload {
        paths: Vec<PathBuf>,
        target: Option<String>,
        insert: bool,
        control: TransferControl,
    },
    Download {
        remote: String,
        local: PathBuf,
        /// The native save dialog already confirmed replacing this existing file.
        confirmed_existing: bool,
        control: TransferControl,
    },
    Mkdir(String),
    Delete {
        paths: Vec<String>,
        control: TransferControl,
    },
    Attributes {
        path: String,
        serial: u64,
    },
    Permissions {
        path: String,
        serial: u64,
        expected: RemoteAttributes,
        change: PermissionChange,
        owner: Option<String>,
        group: Option<String>,
        control: TransferControl,
    },
    /// Copy or move items into a folder, asking about each name conflict.
    Transfer {
        items: Vec<TransferItem>,
        destination: String,
        mode: TransferMode,
        control: TransferControl,
    },
}
enum Update {
    Listed(String, Vec<Entry>),
    Refreshed(String, Vec<Entry>),
    Message(String),
    Unavailable(String),
    Insert(u64, String),
    Conflict(u64, Conflict, Sender<ConflictReply>),
    DestinationValidated(u64, String),
    DestinationFailed(u64, String),
    Started(u64),
    Finished(u64, Result<(), String>),
    Attributes {
        serial: u64,
        result: Result<(RemoteAttributes, String, String), String>,
    },
    PermissionsFinished {
        serial: u64,
        result: Result<(RemoteAttributes, String), String>,
    },
}
struct QueuedJob {
    id: u64,
    job: Job,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum TransferState {
    Queued,
    Running,
    AwaitingReplace,
    Cancelling,
    Completed,
    Cancelled,
    Failed,
}
impl TransferState {
    fn active(self) -> bool {
        matches!(
            self,
            Self::Queued | Self::Running | Self::AwaitingReplace | Self::Cancelling
        )
    }
    fn label(self) -> String {
        tr(match self {
            Self::Queued => "file-task-queued",
            Self::Running => "file-task-running",
            Self::AwaitingReplace => "upload-awaiting-replace",
            Self::Cancelling => "file-task-cancelling",
            Self::Completed => "file-task-completed",
            Self::Cancelled => "file-task-cancelled",
            Self::Failed => "file-task-failed",
        })
    }
}
struct TransferTask {
    id: u64,
    label: String,
    control: TransferControl,
    state: TransferState,
    bytes: u64,
    retry: Option<Job>,
    failure: Option<String>,
    completed_at: Option<std::time::Instant>,
    remote_paths: Option<String>,
    pending_insert: bool,
    insert_generation: Option<u64>,
    dismissed: bool,
    conflict: Option<PendingConflict>,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum FileSort {
    Name,
    Size,
    Modified,
}
pub struct FilePane {
    jobs: Sender<QueuedJob>,
    list_focus: gpui::FocusHandle,
    list_scroll: gpui::UniformListScrollHandle,
    path: Entity<InputState>,
    directory: String,
    folder: Entity<InputState>,
    search: Entity<InputState>,
    full_page: bool,
    grid: bool,
    columns: usize,
    show_hidden: bool,
    sort: FileSort,
    descending: bool,
    visible: Vec<usize>,
    browser_dirty: bool,
    entries: Vec<Entry>,
    selection: selection::Selection,
    history: History,
    /// Pending listing job and how it was requested, for Back/Forward history.
    navigation: Option<(u64, Navigation)>,
    clipboard: Option<FileClipboard>,
    rename: Option<InlineRename>,
    /// Folder under a drag and when the pointer arrived, for spring-loaded opening.
    spring: Option<(String, std::time::Instant)>,
    /// Folder that just received a drop, for a short highlight.
    drop_pulse: Option<(String, std::time::Instant)>,
    drop_serial: u64,
    band: Option<dnd::BandDrag>,
    /// Changes with every new folder listing, so rows animate in once.
    listing_generation: u64,
    bookmarks: Vec<String>,
    /// The terminal's current folder, offered as a place.
    terminal_directory: Option<String>,
    /// The first folder listed after connecting, which is the login home.
    home_hint: String,
    message: String,
    pending_delete: Option<Vec<String>>,
    pending: usize,
    next_id: u64,
    transfers: Vec<TransferTask>,
    progress_task: Option<Task<()>>,
    worker_generation: u64,
    available: bool,
    pending_validation: Option<u64>,
    editor: Option<FileEditor>,
    editor_serial: u64,
    /// Transfer whose conflict is shown in the decision dialog.
    conflict_dialog: Option<u64>,
    conflict_apply_all: bool,
    #[cfg(feature = "capture")]
    _preview_jobs: Option<Receiver<QueuedJob>>,
}
impl EventEmitter<FilePaneEvent> for FilePane {}
impl FilePane {
    pub fn directory(&self) -> &str {
        &self.directory
    }
    pub fn folders(&self) -> Vec<Entry> {
        self.entries
            .iter()
            .filter(|e| e.is_directory && !e.is_symlink)
            .cloned()
            .collect()
    }
    pub fn validate_destination(&mut self, request: u64, path: String, cx: &mut Context<Self>) {
        if !self.available {
            cx.emit(FilePaneEvent::DestinationFailed(
                request,
                self.message.clone(),
            ));
            return;
        }
        self.pending_validation = Some(request);
        self.enqueue(Job::ValidateDestination { request, path }, cx);
    }
    pub fn change_retry_destination(&mut self, path: String, cx: &mut Context<Self>) {
        for task in &mut self.transfers {
            if !task.state.active()
                && let Some(Job::Upload {
                    target,
                    insert: true,
                    ..
                }) = &mut task.retry
            {
                *target = Some(path.clone());
            }
        }
        cx.notify();
    }

    /// In-memory visual fixture: no SSH channel, filesystem work, or user data.
    #[cfg(feature = "capture")]
    pub fn preview(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (jobs, preview_jobs) = async_channel::bounded(32);
        let path = cx.new(|cx| InputState::new(window, cx).default_value("/srv/application"));
        let folder =
            cx.new(|cx| InputState::new(window, cx).placeholder(tr("file-folder-placeholder")));
        let search =
            cx.new(|cx| InputState::new(window, cx).placeholder(tr("file-search-placeholder")));
        cx.subscribe(&search, |this, _, event, cx| {
            if matches!(event, InputEvent::Change) {
                this.browser_dirty = true;
                cx.notify();
            }
        })
        .detach();
        Self {
            jobs,
            list_focus: cx.focus_handle(),
            list_scroll: Default::default(),
            path,
            directory: "/srv/application".into(),
            folder,
            search,
            full_page: false,
            grid: true,
            columns: 1,
            show_hidden: false,
            sort: FileSort::Name,
            descending: false,
            visible: vec![],
            browser_dirty: true,
            entries: [
                ("logs", true, 0),
                ("releases", true, 0),
                ("application.toml", false, 2048),
                (
                    "a-long-file-name-to-check-the-panel-width.log",
                    false,
                    1048576,
                ),
                ("architecture.png", false, 65536),
                ("start-harness.sh", false, 1800),
                ("README.md", false, 1536),
            ]
            .into_iter()
            .map(|(name, directory, size)| Entry {
                name: name.into(),
                path: format!("/srv/application/{name}"),
                size: Some(size),
                modified: None,
                is_directory: directory,
                is_symlink: false,
            })
            .collect(),
            selection: {
                let mut selection = selection::Selection::default();
                selection.select_only("/srv/application/README.md");
                selection
            },
            history: History::default(),
            navigation: None,
            clipboard: None,
            rename: None,
            spring: None,
            drop_pulse: None,
            drop_serial: 0,
            band: None,
            listing_generation: 0,
            bookmarks: vec![],
            terminal_directory: None,
            home_hint: String::new(),
            message: tr("file-entry-count"),
            pending_delete: None,
            pending: 0,
            next_id: 1,
            transfers: vec![TransferTask {
                id: 1,
                label: "application.toml".into(),
                control: TransferControl::default(),
                state: TransferState::Completed,
                bytes: 2048,
                retry: None,
                failure: None,
                completed_at: None,
                remote_paths: None,
                pending_insert: false,
                insert_generation: None,
                dismissed: false,
                conflict: None,
            }],
            progress_task: None,
            worker_generation: 0,
            available: true,
            pending_validation: None,
            editor: None,
            editor_serial: 0,
            conflict_dialog: None,
            conflict_apply_all: false,
            _preview_jobs: Some(preview_jobs),
        }
    }
    pub fn new(
        commands: Sender<SshCommand>,
        fingerprint: String,
        initial_path: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (jobs, receiver) = async_channel::bounded(32);
        let (updates, receive) = async_channel::bounded(32);
        let path = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(initial_path.clone())
                .placeholder(tr("file-path-placeholder"))
        });
        let folder =
            cx.new(|cx| InputState::new(window, cx).placeholder(tr("file-folder-placeholder")));
        let search =
            cx.new(|cx| InputState::new(window, cx).placeholder(tr("file-search-placeholder")));
        cx.subscribe(&search, |this, _, event, cx| {
            if matches!(event, InputEvent::Change) {
                this.browser_dirty = true;
                cx.notify();
            }
        })
        .detach();
        cx.subscribe_in(&path, window, |this, _, event, _, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.refresh(cx);
            }
        })
        .detach();
        let thread = Self::start_worker(
            commands,
            fingerprint,
            (jobs.clone(), receiver),
            (updates, receive),
            0,
            window,
            cx,
        );
        let mut pane = Self {
            jobs,
            list_focus: cx.focus_handle(),
            list_scroll: Default::default(),
            path,
            directory: initial_path.clone(),
            folder,
            search,
            full_page: false,
            grid: true,
            columns: 1,
            show_hidden: false,
            sort: FileSort::Name,
            descending: false,
            visible: vec![],
            browser_dirty: true,
            entries: vec![],
            selection: selection::Selection::default(),
            history: History::default(),
            navigation: None,
            clipboard: None,
            rename: None,
            spring: None,
            drop_pulse: None,
            drop_serial: 0,
            band: None,
            listing_generation: 0,
            bookmarks: vec![],
            terminal_directory: None,
            home_hint: String::new(),
            message: tr("file-opening"),
            pending_delete: None,
            pending: 0,
            next_id: 0,
            transfers: vec![],
            progress_task: None,
            worker_generation: 0,
            available: true,
            pending_validation: None,
            editor: None,
            editor_serial: 0,
            conflict_dialog: None,
            conflict_apply_all: false,
            #[cfg(feature = "capture")]
            _preview_jobs: None,
        };
        match thread {
            Ok(_) => pane.enqueue(
                Job::List(if initial_path.is_empty() {
                    ".".into()
                } else {
                    initial_path
                }),
                cx,
            ),
            Err(error) => pane.message = format!("{}: {error}", tr("file-worker-error")),
        };
        pane
    }
    #[cfg(feature = "capture")]
    pub fn preview_transfer(&mut self, failed: bool) {
        let task = &mut self.transfers[0];
        task.label = format!("{}: deployment.tar.gz", tr("file-upload"));
        task.state = if failed {
            TransferState::Failed
        } else {
            TransferState::Running
        };
        task.bytes = 42 * 1024 * 1024;
        task.control.set_total(64 * 1024 * 1024);
        task.completed_at = None;
        task.retry = Some(Job::Upload {
            paths: vec![PathBuf::from("deployment.tar.gz")],
            target: Some("/srv/uploads".into()),
            insert: true,
            control: task.control.clone(),
        });
        if failed {
            task.failure = Some("Permission denied: choose another upload destination".into());
        }
    }

    #[cfg(feature = "capture")]
    pub fn preview_completed_upload(&mut self, text: String, cx: &mut Context<Self>) -> u64 {
        self.preview_transfer(false);
        let task = self.transfers.first_mut().expect("preview transfer");
        task.state = TransferState::Completed;
        task.bytes = task.control.total_bytes().unwrap_or(0);
        task.retry = None;
        task.remote_paths = Some(text);
        task.pending_insert = true;
        task.insert_generation = Some(self.worker_generation);
        task.completed_at = Some(std::time::Instant::now());
        let id = task.id;
        cx.notify();
        id
    }

    #[cfg(feature = "capture")]
    pub fn preview_upload_replacement(&mut self, cx: &mut Context<Self>) {
        self.preview_transfer(false);
        self.transfers[0].label = format!("{}: test.txt", tr("file-upload"));
        self.transfers[0].bytes = 0;
        let (reply, receive) = async_channel::bounded(1);
        cx.spawn(async move |_, _| {
            let _ = receive.recv().await;
        })
        .detach();
        // The dialog opens on the next frame, once the window can show one.
        self.transfers[0].state = TransferState::AwaitingReplace;
        self.transfers[0].conflict = Some(PendingConflict {
            conflict: conflict::sample_conflict("/opt/test.txt"),
            reply,
        });
        cx.notify();
    }
    #[cfg(feature = "capture")]
    pub fn preview_list(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut view = Self::preview(window, cx);
        view.grid = false;
        view
    }
    fn start_worker(
        commands: Sender<SshCommand>,
        fingerprint: String,
        queue: (Sender<QueuedJob>, Receiver<QueuedJob>),
        channels: (Sender<Update>, Receiver<Update>),
        generation: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let (updates, receive) = channels;
        let (stop, receiver) = queue;
        let thread = std::thread::Builder::new()
            .name("opsssh-files".into())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build();
                match runtime {
                    Ok(runtime) => {
                        if let Err(error) = runtime.block_on(async {
                            tokio::select! {
                                result = worker(commands, fingerprint, receiver, updates.clone()) => result,
                                _ = stop.closed() => Ok(()),
                            }
                        }) {
                            let _ = updates.send_blocking(Update::Unavailable(error.to_string()));
                        }
                    }
                    Err(error) => {
                        let _ = updates.send_blocking(Update::Unavailable(error.to_string()));
                    }
                }
            });
        cx.spawn_in(window, async move |this, cx| {
            while let Ok(update) = receive.recv().await {
                if this
                    .update_in(cx, |this, window, cx| {
                        if this.worker_generation != generation {
                            return;
                        }
                        match update {
                            Update::Attributes { serial, result } => {
                                this.receive_attributes(serial, result, window, cx);
                            }
                            Update::PermissionsFinished { serial, result } => {
                                this.receive_permissions(serial, result, window, cx);
                            }
                            Update::DestinationValidated(request, path) => {
                                this.pending_validation = None;
                                cx.emit(FilePaneEvent::DestinationValidated(request, path));
                            }
                            Update::DestinationFailed(request, error) => {
                                this.pending_validation = None;
                                cx.emit(FilePaneEvent::DestinationFailed(request, error));
                            }
                            Update::Listed(path, entries) => {
                                let previous = std::mem::take(&mut this.directory);
                                if previous != path {
                                    this.search
                                        .update(cx, |state, cx| state.set_value("", window, cx));
                                    this.record_navigation(&previous, &path);
                                    this.reset_selection();
                                    this.listing_generation =
                                        this.listing_generation.wrapping_add(1);
                                }
                                if this.home_hint.is_empty() {
                                    this.home_hint = path.clone();
                                }
                                this.directory = path.clone();
                                this.path
                                    .update(cx, |state, cx| state.set_value(path, window, cx));
                                this.entries = entries;
                                this.browser_dirty = true;
                                this.message =
                                    format!("{}: {}", tr("file-entry-count"), this.entries.len());
                            }
                            Update::Refreshed(path, entries) => {
                                if this.directory == path {
                                    // Selection is kept by path and pruned after filtering.
                                    this.entries = entries;
                                    this.browser_dirty = true;
                                }
                            }
                            Update::Message(message) => this.message = message,
                            Update::Unavailable(message) => {
                                this.message = message;
                                this.available = false;
                                if let Some(request) = this.pending_validation.take() {
                                    cx.emit(FilePaneEvent::DestinationFailed(
                                        request,
                                        this.message.clone(),
                                    ));
                                }
                                this.pending = 0;
                                for task in &mut this.transfers {
                                    if task.state.active() {
                                        task.control.cancel();
                                        task.conflict = None;
                                        task.state = if task.state == TransferState::Cancelling {
                                            TransferState::Cancelled
                                        } else {
                                            TransferState::Failed
                                        };
                                    }
                                }
                            }
                            Update::Insert(id, text) => {
                                if let Some(task) =
                                    this.transfers.iter_mut().find(|task| task.id == id)
                                {
                                    task.remote_paths = Some(text.clone());
                                    task.pending_insert = true;
                                    task.insert_generation = Some(this.worker_generation);
                                }
                                cx.emit(FilePaneEvent::InsertText(id, text));
                            }
                            Update::Conflict(id, conflict, reply) => {
                                this.receive_conflict(id, conflict, reply, window, cx);
                            }
                            Update::Started(id) => {
                                if let Some(task) =
                                    this.transfers.iter_mut().find(|task| task.id == id)
                                    && task.state != TransferState::Cancelling
                                {
                                    task.state = TransferState::Running;
                                }
                                this.watch_progress(window, cx);
                            }
                            Update::Finished(id, result) => {
                                let success = result.is_ok();
                                if let Some(task) =
                                    this.transfers.iter_mut().find(|task| task.id == id)
                                {
                                    task.bytes = task.control.batch_transferred();
                                    task.conflict = None;
                                    task.failure = result.err();
                                    task.state = if success {
                                        TransferState::Completed
                                    } else if task.state == TransferState::Cancelling {
                                        TransferState::Cancelled
                                    } else {
                                        TransferState::Failed
                                    };
                                    if success {
                                        task.completed_at = Some(std::time::Instant::now());
                                        task.retry = None;
                                        task.failure = None;
                                    }
                                }
                                this.pending = this.pending.saturating_sub(1);
                                if !success && this.navigation.is_some_and(|(job, _)| job == id) {
                                    // The folder could not be opened; history stays as it was.
                                    this.navigation = None;
                                }
                                if this.conflict_dialog == Some(id) {
                                    // The worker no longer waits; never leave a stale question open.
                                    this.conflict_dialog = None;
                                    window.close_dialog(cx);
                                }
                                if success {
                                    cx.spawn_in(window, async move |this, cx| {
                                        cx.background_executor()
                                            .timer(Duration::from_secs(4))
                                            .await;
                                        let _ = this.update(cx, |_, cx| cx.notify());
                                    })
                                    .detach();
                                }
                            }
                        };
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        thread.map(|_| ()).map_err(|error| error.to_string())
    }
    /// Preserve the pane and failed transfer history while the SSH transport recovers.
    pub fn suspend(&mut self, cx: &mut Context<Self>) {
        self.worker_generation = self.worker_generation.wrapping_add(1);
        self.jobs.close();
        self.available = false;
        self.pending = 0;
        self.pending_delete = None;
        self.editor = None;
        self.editor_serial = self.editor_serial.wrapping_add(1);
        if let Some(request) = self.pending_validation.take() {
            cx.emit(FilePaneEvent::DestinationFailed(
                request,
                tr("file-interrupted"),
            ));
        }
        self.progress_task = None;
        interrupt_transfers(&mut self.transfers);
        self.message = tr("file-interrupted");
        cx.notify();
    }
    /// An empty directory preserves the last browsed canonical path.
    pub fn rebind(
        &mut self,
        commands: Sender<SshCommand>,
        fingerprint: String,
        directory: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.suspend(cx);
        let (jobs, receiver) = async_channel::bounded(32);
        let (updates, receive) = async_channel::bounded(32);
        self.jobs = jobs;
        let result = Self::start_worker(
            commands,
            fingerprint,
            (self.jobs.clone(), receiver),
            (updates, receive),
            self.worker_generation,
            window,
            cx,
        );
        match result {
            Ok(()) => {
                self.available = true;
                let path = if directory.is_empty() {
                    self.directory.clone()
                } else {
                    directory
                };
                self.enqueue(
                    Job::List(if path.is_empty() { ".".into() } else { path }),
                    cx,
                );
            }
            Err(error) => {
                self.message = format!("{}: {error}", tr("file-worker-error"));
                cx.notify();
            }
        }
    }
    fn retry_transfer(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        if !self.available {
            return;
        }
        let Some(mut job) = self
            .transfers
            .iter()
            .find(|task| task.id == id && !task.state.active())
            .and_then(|task| task.retry.clone())
        else {
            return;
        };
        match &mut job {
            Job::Upload {
                target,
                insert,
                control,
                ..
            } => {
                let _ = (target, insert);
                *control = TransferControl::default();
            }
            Job::Download { remote, local, .. } => {
                let directory = local.parent().map(PathBuf::from).unwrap_or_default();
                let picker = cx.prompt_for_new_path(
                    &directory,
                    local.file_name().and_then(|name| name.to_str()),
                );
                let remote = remote.clone();
                let generation = self.worker_generation;
                cx.spawn_in(window, async move |this, cx| {
                    if let Ok(Ok(Some(local))) = picker.await {
                        let confirmed_existing = picked_existing_file(&local);
                        let _ = this.update_in(cx, |this, _, cx| {
                            if this.available && this.worker_generation == generation {
                                this.enqueue(
                                    Job::Download {
                                        remote,
                                        local,
                                        confirmed_existing,
                                        control: TransferControl::default(),
                                    },
                                    cx,
                                );
                            }
                        });
                    }
                })
                .detach();
                return;
            }
            _ => return,
        }
        self.enqueue(job, cx);
    }
    pub fn set_full_page(&mut self, full_page: bool, cx: &mut Context<Self>) {
        if self.full_page != full_page {
            self.full_page = full_page;
            cx.notify();
        }
    }
    pub fn active_transfers(&self) -> usize {
        self.transfers
            .iter()
            .filter(|task| task.state.active())
            .count()
    }
    pub fn cancel_transfers(&mut self, cx: &mut Context<Self>) {
        for task in &mut self.transfers {
            if task.state.active() {
                task.control.cancel();
                task.state = TransferState::Cancelling;
            }
        }
        cx.notify();
    }
    fn enqueue(&mut self, job: Job, cx: &mut Context<Self>) {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        let transfer = match &job {
            Job::Upload { paths, control, .. } => Some((
                format!(
                    "{} · {}",
                    tr("file-upload"),
                    paths
                        .iter()
                        .filter_map(|p| p.file_name())
                        .map(|n| n.to_string_lossy().into_owned())
                        .take(3)
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                control.clone(),
            )),
            Job::Download {
                remote, control, ..
            } => Some((
                format!(
                    "{} · {}",
                    tr("file-download"),
                    remote.rsplit('/').next().unwrap_or(remote)
                ),
                control.clone(),
            )),
            Job::Transfer {
                items,
                mode,
                control,
                ..
            } => Some((
                format!(
                    "{} · {}",
                    tr(if *mode == TransferMode::Copy {
                        "file-copy"
                    } else {
                        "file-move"
                    }),
                    match items.as_slice() {
                        [item] => item.name.clone(),
                        items => format!("{} {}", items.len(), tr("file-items")),
                    }
                ),
                control.clone(),
            )),
            Job::Delete { paths, control } => Some((
                format!(
                    "{} · {}",
                    tr("file-delete"),
                    match paths.as_slice() {
                        [path] => path.clone(),
                        paths => format!("{} {}", paths.len(), tr("file-items")),
                    }
                ),
                control.clone(),
            )),
            Job::Permissions { path, control, .. } => {
                Some((format!("Permissions · {path}"), control.clone()))
            }
            _ => None,
        };
        // Mutations need a fresh item review, rather than replaying old ownership
        // or a partially completed delete/copy against a changed remote tree.
        let retry = match &job {
            Job::Upload { .. } | Job::Download { .. } => Some(job.clone()),
            _ => None,
        };
        match self.jobs.try_send(QueuedJob { id, job }) {
            Ok(()) => {
                self.pending += 1;
                if let Some((label, control)) = transfer {
                    if self.transfers.len() >= 32 {
                        self.transfers.retain(|task| task.state.active());
                    }
                    self.transfers.push(TransferTask {
                        id,
                        label,
                        control,
                        state: TransferState::Queued,
                        bytes: 0,
                        retry,
                        failure: None,
                        completed_at: None,
                        remote_paths: None,
                        pending_insert: false,
                        insert_generation: None,
                        dismissed: false,
                        conflict: None,
                    });
                }
                self.message = format!("{}: {}", tr("file-pending-count"), self.pending);
            }
            Err(error) => {
                if let Job::ValidateDestination { request, .. } = error.into_inner().job {
                    self.pending_validation = None;
                    cx.emit(FilePaneEvent::DestinationFailed(
                        request,
                        tr("file-queue-error"),
                    ));
                }
                self.message = tr("file-queue-error");
            }
        };
        cx.notify();
    }
    fn watch_progress(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.progress_task.is_some()
            || !self.transfers.iter().any(|t| {
                matches!(
                    t.state,
                    TransferState::Running
                        | TransferState::AwaitingReplace
                        | TransferState::Cancelling
                )
            })
        {
            return;
        }
        let timer = cx.background_executor().timer(Duration::from_millis(200));
        self.progress_task = Some(cx.spawn_in(window, async move |this, cx| {
            timer.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.progress_task = None;
                let mut changed = false;
                for task in &mut this.transfers {
                    let bytes = task.control.batch_transferred();
                    if bytes != task.bytes {
                        task.bytes = bytes;
                        changed = true;
                    }
                }
                if changed {
                    cx.notify();
                }
                this.watch_progress(window, cx);
            });
        }));
    }
    fn cancel_transfer(&mut self, id: u64, cx: &mut Context<Self>) {
        if let Some(task) = self
            .transfers
            .iter_mut()
            .find(|task| task.id == id && task.state.active())
        {
            task.control.cancel();
            task.conflict = None;
            task.state = TransferState::Cancelling;
            self.message = tr("file-cancel-requested");
            cx.notify();
        }
    }
    fn refresh(&mut self, cx: &mut Context<Self>) {
        let path = self.path.read(cx).value().to_string();
        self.navigate(path, Navigation::Visit, cx);
    }
    pub fn follow_directory(&mut self, path: String, cx: &mut Context<Self>) {
        self.terminal_directory = Some(path.clone());
        if self.directory != path {
            self.navigate(path, Navigation::Visit, cx);
        }
    }
    pub fn upload_files(
        &mut self,
        paths: Vec<PathBuf>,
        target: Option<String>,
        insert: bool,
        cx: &mut Context<Self>,
    ) {
        let control = TransferControl::default();
        self.enqueue(
            Job::Upload {
                paths,
                target,
                insert,
                control,
            },
            cx,
        );
    }
    fn pick_upload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let picker = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: true,
            multiple: true,
            prompt: None,
        });
        cx.spawn_in(window, async move |this, cx| match picker.await {
            Ok(Ok(Some(paths))) => {
                let _ = this.update_in(cx, |this, _, cx| {
                    let target = this.directory.clone();
                    this.upload_files(paths, Some(target), false, cx);
                });
            }
            Ok(Err(error)) => {
                let _ = this.update_in(cx, |this, _, cx| {
                    this.message = error.to_string();
                    cx.notify();
                });
            }
            _ => {}
        })
        .detach();
    }
    fn pick_download(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.download_selection(window, cx);
    }
}
impl FilePane {
    pub fn dismiss_transfer_notice(&mut self, id: u64, cx: &mut Context<Self>) {
        if let Some(task) = self.transfers.iter_mut().find(|task| task.id == id) {
            if task.state == TransferState::AwaitingReplace {
                task.control.cancel();
                task.conflict = None;
                task.state = TransferState::Cancelling;
            }
            task.dismissed = true;
            if !task.state.active() {
                task.pending_insert = false;
            }
            cx.notify();
        }
    }

    pub fn pending_upload_paths(&self) -> Vec<(u64, String)> {
        self.transfers
            .iter()
            .filter(|task| {
                task.state == TransferState::Completed
                    && task.pending_insert
                    && task.insert_generation == Some(self.worker_generation)
            })
            .filter_map(|task| task.remote_paths.clone().map(|text| (task.id, text)))
            .collect()
    }

    pub fn path_inserted(&mut self, id: u64, cx: &mut Context<Self>) {
        if let Some(task) = self.transfers.iter_mut().find(|task| task.id == id) {
            task.pending_insert = false;
            cx.notify();
        }
    }

    pub fn require_manual_insert(&mut self, id: u64, cx: &mut Context<Self>) {
        if let Some(task) = self.transfers.iter_mut().find(|task| task.id == id) {
            task.pending_insert = true;
            task.insert_generation = None;
            cx.notify();
        }
    }

    pub fn defer_insert(&mut self, id: u64, cx: &mut Context<Self>) {
        if let Some(task) = self.transfers.iter_mut().find(|task| task.id == id) {
            task.pending_insert = true;
        }
        cx.notify();
    }

    pub fn terminal_tray(&self, reduced: bool, cx: &mut Context<Self>) -> gpui::AnyElement {
        use gpui::{Animation, AnimationExt};
        use gpui_component::progress::ProgressCircle;
        let mut tray = div()
            .id("terminal-upload-tray")
            .max_h(px(160.))
            .overflow_y_scroll()
            .flex_shrink_0()
            .flex()
            .flex_col()
            .gap_2();
        for task in self.transfers.iter().rev().filter(|task| {
            !task.dismissed
                && (task.state != TransferState::Completed
                    || task.pending_insert
                    || task
                        .completed_at
                        .is_some_and(|at| at.elapsed() < Duration::from_secs(4)))
        }) {
            let id = task.id;
            let total = task.control.total_bytes();
            let percent = if task.state == TransferState::Completed {
                100.
            } else {
                total
                    .filter(|total| *total > 0)
                    .map(|total| (task.bytes as f64 / total as f64 * 100.).min(99.) as f32)
                    .unwrap_or(0.)
            };
            let active = task.state.active();
            let destination = match task.retry.as_ref() {
                Some(Job::Upload {
                    target: Some(path), ..
                }) => path.clone(),
                _ => String::new(),
            };
            let paths = task.remote_paths.clone();
            let mut card = div()
                .id(("terminal-transfer", id))
                .flex_shrink_0()
                .flex()
                .items_center()
                .gap_3()
                .p_3()
                .rounded_lg()
                .bg(cx.theme().background)
                .border_1()
                .border_color(cx.theme().border)
                .child(if task.failure.is_some() {
                    Icon::new(IconName::Info)
                        .small()
                        .text_color(cx.theme().danger)
                        .into_any_element()
                } else {
                    ProgressCircle::new(("drop-progress", id))
                        .small()
                        .value(percent)
                        .loading(active && total.is_none() && !reduced)
                        .accessibility_label(format!("{}: {:.0}%", task.state.label(), percent))
                        .into_any_element()
                })
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(div().text_sm().overflow_hidden().child(task.label.clone()))
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(if let Some(pending) = &task.conflict {
                                    format!(
                                        "{} {}",
                                        tr("upload-file-exists"),
                                        pending.conflict.destination
                                    )
                                } else if let Some(error) = &task.failure {
                                    error.clone()
                                } else {
                                    format!(
                                        "{} · {:.0}% · {}{}",
                                        task.state.label(),
                                        percent,
                                        format_bytes(task.bytes),
                                        if destination.is_empty() {
                                            String::new()
                                        } else {
                                            format!(" → {destination}")
                                        }
                                    )
                                }),
                        ),
                )
                .when(active, |d| {
                    d.child(
                        Button::new(("drop-cancel", id))
                            .small()
                            .ghost()
                            .label(tr("file-cancel-transfer"))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.cancel_transfer(id, cx);
                                cx.emit(FilePaneEvent::FocusTerminal);
                            })),
                    )
                })
                .when(task.conflict.is_some(), |d| {
                    d.child(
                        crate::design::PrimaryAction::new(("drop-review", id))
                            .small()
                            .label(tr("conflict-review"))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.review_conflict(id, window, cx);
                            })),
                    )
                })
                .when(!active && task.retry.is_some(), |d| {
                    d.child(
                        Button::new(("drop-retry", id))
                            .small()
                            .ghost()
                            .label(tr("file-retry-transfer"))
                            .disabled(!self.available)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.retry_transfer(id, window, cx);
                                cx.emit(FilePaneEvent::FocusTerminal);
                            })),
                    )
                })
                .when(
                    !active
                        && matches!(task.retry.as_ref(), Some(Job::Upload { insert: true, .. })),
                    |d| {
                        d.child(
                            Button::new(("drop-destination", id))
                                .small()
                                .ghost()
                                .label(tr("upload-change-folder"))
                                .disabled(!self.available)
                                .on_click(cx.listener(|_, _, _, cx| {
                                    cx.emit(FilePaneEvent::ChooseUploadDestination);
                                })),
                        )
                    },
                )
                .when_some(paths.clone(), |d, text| {
                    d.child(
                        Button::new(("drop-copy", id))
                            .small()
                            .ghost()
                            .label(tr("upload-copy-path"))
                            .on_click(cx.listener(move |_, _, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
                                cx.emit(FilePaneEvent::FocusTerminal);
                            })),
                    )
                })
                .when(task.pending_insert && self.available, |d| {
                    d.child(
                        Button::new(("drop-insert", id))
                            .small()
                            .ghost()
                            .label(tr("upload-insert-path"))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(task) =
                                    this.transfers.iter_mut().find(|task| task.id == id)
                                {
                                    task.pending_insert = false;
                                    if let Some(text) = task.remote_paths.clone() {
                                        cx.emit(FilePaneEvent::FocusTerminal);
                                        cx.emit(FilePaneEvent::InsertText(id, text));
                                    }
                                }
                                cx.notify();
                            })),
                    )
                })
                .child(
                    Button::new(("drop-dismiss", id))
                        .small()
                        .ghost()
                        .icon(IconName::Close)
                        .tooltip(tr("upload-dismiss"))
                        .accessibility_label(tr("upload-dismiss"))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.dismiss_transfer_notice(id, cx);
                            cx.emit(FilePaneEvent::FocusTerminal);
                        })),
                );
            // Animate the feedback card, never the terminal grid or its input handler.
            card = card.opacity(1.);
            tray = tray.child(
                card.with_animation(
                    ("drop-enter", id),
                    Animation::new(Duration::from_millis(if reduced { 0 } else { 240 }))
                        .with_easing(crate::design::spring_out),
                    |d, t| {
                        d.opacity(t.clamp(0., 1.))
                            .relative()
                            .top(px(12. * (1. - t)))
                    },
                ),
            );
        }
        tray.into_any_element()
    }
    fn entry_element(&self, index: usize, cx: &mut Context<Self>) -> gpui::AnyElement {
        let entry = &self.entries[index];
        let theme = cx.theme();
        let (background, foreground, muted, border, hover, selected, accent, danger) = (
            theme.background,
            theme.foreground,
            theme.muted_foreground,
            theme.border,
            theme.muted,
            theme.accent,
            theme.primary,
            theme.danger,
        );
        let full_path = entry.path.clone();
        let grid = self.full_page && self.grid;
        let is_selected = self.is_selected(&entry.path);
        let is_cursor = self.selection.cursor() == Some(entry.path.as_str());
        let folder = entry.is_directory && !entry.is_symlink;
        let spring = folder && self.spring.as_ref().is_some_and(|(p, _)| *p == entry.path);
        let renaming = self
            .rename
            .as_ref()
            .filter(|rename| rename.path == entry.path);
        let size = entry
            .size
            .filter(|_| !entry.is_directory)
            .map(format_bytes)
            .unwrap_or_else(|| "—".into());
        let modified = entry
            .modified
            .map(format_modified)
            .unwrap_or_else(|| "—".into());
        let icon = Icon::new(if spring {
            gpui_kit_assets::IconName::FolderOpen
        } else {
            entry_icon(entry)
        })
        .text_color(if entry.is_directory { accent } else { muted });
        let name: gpui::AnyElement = match renaming {
            Some(rename) => div()
                .flex()
                .flex_col()
                .gap_1()
                .min_w_0()
                .child(Input::new(&rename.input).small())
                .when_some(rename.error.clone(), |d, error| {
                    d.child(div().text_xs().text_color(danger).child(error))
                })
                .into_any_element(),
            None => div()
                .text_sm()
                .truncate()
                .when(grid, |d| d.h(px(20.)).font_weight(gpui::FontWeight::MEDIUM))
                .child(entry.name.clone())
                .into_any_element(),
        };
        let mut cell = div()
            .id(("file-entry", index))
            .tooltip(move |window, cx| {
                gpui_component::tooltip::Tooltip::new(full_path.clone()).build(window, cx)
            })
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .border_1()
            .border_color(if is_cursor && !grid {
                accent.opacity(0.5)
            } else {
                gpui::transparent_black()
            })
            .bg(if is_selected { selected } else { background })
            .when(self.is_cut(&entry.path), |d| d.opacity(0.5))
            .text_color(foreground)
            .hover(move |style| style.bg(hover));
        if grid {
            cell = cell
                .relative()
                .flex_col()
                .p_3()
                .gap_2()
                .rounded_md()
                .border_color(if is_selected || spring {
                    accent
                } else {
                    border
                })
                .child(
                    div()
                        .flex_1()
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(icon.size(px(30.))),
                )
                .child(div().flex_shrink_0().child(name))
                .child(
                    div()
                        .flex()
                        .justify_between()
                        .text_xs()
                        .text_color(muted)
                        .child(if entry.is_directory {
                            tr("file-type-folder")
                        } else {
                            size
                        })
                        .when(!entry.is_directory, |metadata| {
                            metadata.child(file_kind(entry))
                        }),
                )
                .child(div().text_xs().text_color(muted).child(modified));
        } else {
            cell = cell
                .items_center()
                .gap_3()
                .px_4()
                .child(icon.size(px(18.)))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(name)
                        .child(div().text_xs().text_color(muted).child(file_kind(entry))),
                )
                .child(
                    div()
                        .w(px(76.))
                        .flex_shrink_0()
                        .text_xs()
                        .text_color(muted)
                        .child(size),
                );
            if self.full_page {
                cell = cell.child(
                    div()
                        .w(px(135.))
                        .flex_shrink_0()
                        .text_xs()
                        .text_color(muted)
                        .child(modified),
                );
            }
        }
        let target = entry.clone();
        let overflow_target = entry.clone();
        let weak = cx.entity().downgrade();
        let overflow_view = weak.clone();
        let options = Button::new(("file-options", index))
            .ghost()
            .small()
            .icon(IconName::Ellipsis)
            .tooltip("File actions")
            .dropdown_menu(move |menu, _, cx| {
                if let Some(view) = overflow_view.upgrade() {
                    view.update(cx, |state, cx| {
                        state.ensure_selected(&overflow_target.path);
                        state.action_menu(overflow_target.clone(), menu, cx)
                    })
                } else {
                    menu
                }
            });
        cell = if grid {
            cell.child(div().absolute().top_2().right_2().child(options))
        } else {
            cell.child(options)
        };
        let clicked = entry.path.clone();
        cell = cell.on_click(
            cx.listener(move |this, event: &gpui::ClickEvent, window, cx| {
                if this.rename.is_some() {
                    return;
                }
                this.list_focus.focus(window, cx);
                let modifiers = event.modifiers();
                if event.click_count() == 2 && !modifiers.shift && !modifiers.control {
                    if let Some(entry) = this.entry_for(&clicked).cloned() {
                        this.selection.select_only(&clicked);
                        this.open_entry(entry, window, cx);
                    }
                } else {
                    this.click_entry(&clicked, &modifiers);
                }
                cx.notify();
            }),
        );
        if self.available && renaming.is_none() {
            let value = self.drag_value(entry, cx.entity_id());
            cell = cell.on_drag(value, |items, _, _, cx| {
                cx.new(|_| dnd::DragGhost::new(items.clone()))
            });
        }
        if folder {
            let hovered = entry.path.clone();
            cell = self
                .folder_drop_target(cell, entry.path.clone(), cx)
                .on_drag_move(cx.listener(
                    move |this, event: &gpui::DragMoveEvent<dnd::DraggedItems>, window, cx| {
                        this.spring_hover(&hovered, event.bounds, event.event.position, window, cx);
                    },
                ));
        }
        let pulse = self
            .drop_pulse
            .as_ref()
            .filter(|(path, at)| *path == entry.path && at.elapsed() < Duration::from_millis(600))
            .map(|(_, at)| *at);
        let _ = pulse.map(|at| at.elapsed());
        let reduced = cx.reduce_motion();
        let element = cell
            .context_menu(move |menu, _, cx| {
                if let Some(view) = weak.upgrade() {
                    view.update(cx, |state, cx| {
                        state.ensure_selected(&target.path);
                        cx.notify();
                        state.action_menu(target.clone(), menu, cx)
                    })
                } else {
                    menu
                }
            })
            .into_any_element();
        match pulse {
            // A folder that just received a drop pulses once.
            Some(_) if !reduced => gpui::AnimationExt::with_animation(
                div().flex_1().min_w_0().h_full().flex().child(element),
                ("drop-pulse", self.drop_serial as usize),
                gpui::Animation::new(Duration::from_millis(500))
                    .with_easing(crate::design::spring_out),
                |d, t| d.opacity(0.55 + 0.45 * t),
            )
            .into_any_element(),
            _ => element,
        }
    }
}
impl Drop for FilePane {
    fn drop(&mut self) {
        for task in &self.transfers {
            task.control.cancel();
        }
        self.jobs.close();
    }
}
impl FilePane {
    // Erase each section before composing it. GPUI debug builders otherwise keep
    // all section temporaries in one Windows UI-thread stack frame.
    fn full_page_title(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        div()
            .flex()
            .items_center()
            .gap_3()
            .px_4()
            .py_2()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                div()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(tr("file-title")),
            )
            .child(
                div()
                    .flex_1()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(tr("file-connection-label")),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!("{} {}", self.visible.len(), tr("file-items"))),
            )
            .into_any_element()
    }
    fn full_page_path_controls(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        div()
            .flex()
            .items_center()
            .gap_2()
            .p_3()
            .child(self.history_controls(cx))
            .child(
                Button::new("refresh")
                    .small()
                    .ghost()
                    .label(tr("file-refresh"))
                    .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
            )
            .child(
                div().flex_1().min_w_0().child(
                    Input::new(&self.path)
                        .small()
                        .aria_label(tr("file-path-placeholder")),
                ),
            )
            .child(
                crate::design::PrimaryAction::new("upload")
                    .small()
                    .label(tr("file-upload"))
                    .disabled(!self.available)
                    .on_click(cx.listener(|this, _, window, cx| this.pick_upload(window, cx))),
            )
            .into_any_element()
    }
    fn full_page_file_actions(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        div()
            .flex()
            .items_center()
            .gap_2()
            .p_3()
            .border_t_1()
            .border_color(cx.theme().border)
            .child(
                Button::new("download")
                    .small()
                    .label(tr("file-download"))
                    .disabled(self.selection.is_empty() || !self.available)
                    .on_click(cx.listener(|this, _, window, cx| this.pick_download(window, cx))),
            )
            .child(
                Button::new("delete")
                    .small()
                    .ghost()
                    .label(tr("file-delete"))
                    .disabled(self.selection.is_empty() || !self.available)
                    .on_click(cx.listener(|this, _, _, cx| this.request_delete_selection(cx))),
            )
            .child(
                div().flex_1().min_w_0().max_w(px(480.)).child(
                    Input::new(&self.folder)
                        .small()
                        .aria_label(tr("file-folder-placeholder")),
                ),
            )
            .child(
                Button::new("mkdir")
                    .small()
                    .label(tr("file-create-folder"))
                    .disabled(!self.available)
                    .on_click(cx.listener(|this, _, _, cx| {
                        match opsssh_drop::join_remote(
                            &this.directory,
                            &this.folder.read(cx).value(),
                        ) {
                            Ok(path) => this.enqueue(Job::Mkdir(path), cx),
                            Err(error) => {
                                this.message = error.to_string();
                                cx.notify();
                            }
                        }
                    })),
            )
            .into_any_element()
    }
    fn completed_transfer_card(
        &self,
        task: &TransferTask,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        div()
            .flex()
            .items_center()
            .gap_3()
            .p_2()
            .rounded_md()
            .bg(cx.theme().sidebar)
            .border_1()
            .border_color(cx.theme().border)
            .text_xs()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(task.label.clone()),
            )
            .child(
                div()
                    .text_color(cx.theme().muted_foreground)
                    .child(task.state.label()),
            )
            .child(
                div()
                    .text_color(cx.theme().muted_foreground)
                    .child(format_bytes(task.bytes)),
            )
            .into_any_element()
    }
    fn file_title(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        if self.full_page {
            return self.full_page_title(cx);
        }
        let theme = cx.theme();
        let border = theme.border;
        let muted = theme.muted_foreground;
        div()
            .flex()
            .items_center()
            .justify_between()
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(border)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(tr("file-title")),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child(tr("file-connection-label")),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(div().text_xs().text_color(muted).child(format!(
                        "{} {}",
                        self.visible.len(),
                        tr("file-items")
                    )))
                    .child(
                        Button::new("close-remote-files")
                            .ghost()
                            .icon(IconName::Close)
                            .tooltip(tr("close-remote-files"))
                            .accessibility_label(tr("close-remote-files"))
                            .on_click(
                                cx.listener(|_, _, _, cx| cx.emit(FilePaneEvent::ClosePanel)),
                            ),
                    ),
            )
            .into_any_element()
    }
    fn path_controls(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        if self.full_page {
            return self.full_page_path_controls(cx);
        }
        div()
            .p_2()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                Input::new(&self.path)
                    .small()
                    .aria_label(tr("file-path-placeholder")),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div().flex().gap_1().child(self.history_controls(cx)).child(
                            Button::new("refresh")
                                .small()
                                .ghost()
                                .label(tr("file-refresh"))
                                .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
                        ),
                    )
                    .child(
                        crate::design::PrimaryAction::new("upload")
                            .small()
                            .label(tr("file-upload"))
                            .disabled(!self.available)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.pick_upload(window, cx)),
                            ),
                    ),
            )
            .into_any_element()
    }
    fn list_header(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        if self.full_page {
            return self.full_page_list_header(cx);
        }
        div()
            .flex()
            .justify_between()
            .px_4()
            .py_2()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .border_y_1()
            .border_color(cx.theme().border)
            .child(
                Button::new("focus-file-list")
                    .ghost()
                    .small()
                    .label(tr("file-name-column"))
                    .tooltip(tr("file-list-help"))
                    .accessibility_label(tr("file-list-help"))
                    .on_click(cx.listener(|this, _, window, cx| this.list_focus.focus(window, cx))),
            )
            .child(tr("file-size-column"))
            .into_any_element()
    }
    fn full_page_list_header(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = cx.theme();
        let border = theme.border;
        let muted = theme.muted_foreground;
        div()
            .flex()
            .items_center()
            .gap_3()
            .px_4()
            .py_2()
            .text_xs()
            .text_color(muted)
            .border_y_1()
            .border_color(border)
            .child(
                div().flex_1().min_w_0().flex().items_center().child(
                    Button::new("focus-file-list")
                        .small()
                        .ghost()
                        .label(tr("file-name-column"))
                        .tooltip(tr("file-list-help"))
                        .accessibility_label(tr("file-list-help"))
                        .on_click(
                            cx.listener(|this, _, window, cx| this.list_focus.focus(window, cx)),
                        ),
                ),
            )
            .child(
                div()
                    .w(px(76.))
                    .flex_shrink_0()
                    .child(tr("file-size-column")),
            )
            .when(self.full_page, |header| {
                header.child(
                    div()
                        .w(px(135.))
                        .flex_shrink_0()
                        .child(tr("file-modified-column")),
                )
            })
            .into_any_element()
    }
    fn breadcrumb_controls(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let muted = cx.theme().muted_foreground;
        let mut bar = div().flex().items_center().gap_1();
        for (index, (label, path)) in breadcrumbs(&self.directory).into_iter().enumerate() {
            let target = path.clone();
            let crumb = div()
                .id(("file-breadcrumb-target", index))
                .rounded_md()
                .border_1()
                .border_color(gpui::transparent_black());
            bar = bar
                .child(
                    self.folder_drop_target(crumb, target, cx).child(
                        Button::new(("file-breadcrumb", index))
                            .small()
                            .ghost()
                            .label(label)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.navigate(path.clone(), Navigation::Visit, cx)
                            })),
                    ),
                )
                .child(div().text_color(muted).child("/"));
        }
        div()
            .id("file-breadcrumbs")
            .px_3()
            .pb_2()
            .overflow_x_scroll()
            .child(bar)
            .into_any_element()
    }
    fn browser_controls(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let mut browser_tools = div()
            .px_3()
            .py_2()
            .flex()
            .items_center()
            .gap_2()
            .flex_wrap()
            .child(
                div().flex_1().min_w(px(140.)).child(
                    Input::new(&self.search)
                        .small()
                        .aria_label(tr("file-search-placeholder")),
                ),
            )
            .child(
                Button::new("file-hidden")
                    .small()
                    .ghost()
                    .label(if self.show_hidden {
                        tr("file-hide-hidden")
                    } else {
                        tr("file-show-hidden")
                    })
                    .selected(self.show_hidden)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.show_hidden = !this.show_hidden;
                        this.browser_dirty = true;
                        cx.notify();
                    })),
            )
            .child(
                Button::new("file-sort")
                    .small()
                    .ghost()
                    .label(match self.sort {
                        FileSort::Name => tr("file-name-column"),
                        FileSort::Size => tr("file-size-column"),
                        FileSort::Modified => tr("file-modified-column"),
                    })
                    .tooltip(tr("file-sort-help"))
                    .dropdown_menu({
                        let pane = cx.entity().downgrade();
                        move |mut menu, _, _| {
                            for (sort, key) in [
                                (FileSort::Name, "file-name-column"),
                                (FileSort::Size, "file-size-column"),
                                (FileSort::Modified, "file-modified-column"),
                            ] {
                                let pane = pane.clone();
                                menu = menu.item(PopupMenuItem::new(tr(key)).on_click(
                                    move |_, _, cx| {
                                        let _ = pane.update(cx, |this, cx| {
                                            this.sort = sort;
                                            this.browser_dirty = true;
                                            cx.notify();
                                        });
                                    },
                                ));
                            }
                            menu
                        }
                    }),
            )
            .child(
                Button::new("file-sort-direction")
                    .small()
                    .ghost()
                    .icon(if self.descending {
                        IconName::ArrowDown
                    } else {
                        IconName::ArrowUp
                    })
                    .tooltip(if self.descending {
                        tr("file-sort-ascending")
                    } else {
                        tr("file-sort-descending")
                    })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.descending = !this.descending;
                        this.browser_dirty = true;
                        cx.notify();
                    })),
            );
        if self.full_page {
            browser_tools = browser_tools
                .child(
                    Button::new("file-grid")
                        .small()
                        .ghost()
                        .icon(IconName::LayoutDashboard)
                        .selected(self.grid)
                        .tooltip(tr("file-grid-view"))
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.grid = true;
                            this.list_focus.focus(window, cx);
                            cx.notify();
                        })),
                )
                .child(
                    Button::new("file-list")
                        .small()
                        .ghost()
                        .icon(gpui_kit_assets::IconName::List)
                        .selected(!self.grid)
                        .tooltip(tr("file-list-view"))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.grid = false;
                            cx.notify();
                        })),
                );
        }
        browser_tools.into_any_element()
    }
    fn entries_view(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let (band_fill, band_border) = (theme.primary.opacity(0.12), theme.primary.opacity(0.7));
        let listing = div()
            .id("file-listing")
            .relative()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .border_1()
            .border_color(gpui::transparent_black());
        let listing = self.listing_interactions(listing, cx);
        if self.visible.is_empty() {
            return listing
                .justify_center()
                .items_center()
                .gap_2()
                .p_4()
                .text_color(muted)
                .child(if self.entries.is_empty() {
                    tr("file-empty-title")
                } else {
                    tr("file-no-matches")
                })
                .child(div().text_xs().child(if self.entries.is_empty() {
                    tr("file-empty-help")
                } else {
                    tr("file-no-matches-help")
                }))
                .into_any_element();
        }
        let rows = self.visible.len().div_ceil(self.columns);
        let generation = self.listing_generation;
        let reduced = cx.reduce_motion();
        let list = uniform_list(
            "file-entries",
            rows,
            cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                range
                    .map(|row| {
                        let start = row * this.columns;
                        let end = (start + this.columns).min(this.visible.len());
                        let indices = this.visible[start..end].to_vec();
                        let grid = this.full_page && this.grid;
                        let mut line =
                            div()
                                .w_full()
                                .flex()
                                .gap_3()
                                .h(px(if grid { 164. } else { 56. }));
                        if grid {
                            line = line.px_3().py_2();
                        }
                        for index in indices {
                            line = line.child(this.entry_element(index, cx));
                        }
                        if grid {
                            // Fillers match a cell's padding and border so every row lines up.
                            for _ in end - start..this.columns {
                                line = line.child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .p_3()
                                        .border_1()
                                        .border_color(gpui::transparent_black()),
                                );
                            }
                        }
                        // A new folder's first rows fade and slide in once.
                        if row < 24 && !reduced {
                            gpui::AnimationExt::with_animation(
                                line,
                                (
                                    "file-row",
                                    generation.wrapping_mul(64).wrapping_add(row as u64) as usize,
                                ),
                                gpui::Animation::new(Duration::from_millis(180 + row as u64 * 12))
                                    .with_easing(crate::design::spring_out),
                                |d, t| d.opacity(t).mt(px(6. * (1. - t))),
                            )
                            .into_any_element()
                        } else {
                            line.into_any_element()
                        }
                    })
                    .collect()
            }),
        )
        .track_scroll(&self.list_scroll)
        .flex_1()
        .min_h_0();
        listing
            .child(list)
            .when_some(self.band_rect(), |d, rect| {
                d.child(
                    div()
                        .absolute()
                        .left(rect.origin.x)
                        .top(rect.origin.y)
                        .w(rect.size.width)
                        .h(rect.size.height)
                        .bg(band_fill)
                        .border_1()
                        .border_color(band_border),
                )
            })
            .into_any_element()
    }
    fn file_actions(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        if self.full_page {
            return self.full_page_file_actions(cx);
        }
        let theme = cx.theme();
        let border = theme.border;
        div()
            .p_2()
            .flex()
            .flex_col()
            .gap_1()
            .border_t_1()
            .border_color(border)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(
                        Button::new("download")
                            .small()
                            .label(tr("file-download"))
                            .disabled(self.selection.is_empty() || !self.available)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.pick_download(window, cx)),
                            ),
                    )
                    .child(
                        Button::new("delete")
                            .small()
                            .ghost()
                            .label(tr("file-delete"))
                            .disabled(self.selection.is_empty() || !self.available)
                            .on_click(
                                cx.listener(|this, _, _, cx| this.request_delete_selection(cx)),
                            ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(
                        Input::new(&self.folder)
                            .small()
                            .aria_label(tr("file-folder-placeholder")),
                    )
                    .child(
                        Button::new("mkdir")
                            .small()
                            .label(tr("file-create-folder"))
                            .disabled(!self.available)
                            .on_click(cx.listener(
                                |this, _, _, cx| match opsssh_drop::join_remote(
                                    &this.directory,
                                    &this.folder.read(cx).value(),
                                ) {
                                    Ok(path) => this.enqueue(Job::Mkdir(path), cx),
                                    Err(error) => {
                                        this.message = error.to_string();
                                        cx.notify();
                                    }
                                },
                            )),
                    ),
            )
            .into_any_element()
    }
    fn delete_confirmation(&self, paths: Vec<String>, cx: &mut Context<Self>) -> gpui::AnyElement {
        let names = paths
            .iter()
            .take(5)
            .map(|path| path.rsplit('/').next().unwrap_or(path).to_owned())
            .collect::<Vec<_>>()
            .join(", ");
        let summary = if paths.len() > 5 {
            format!(
                "{names} {} {} {}",
                tr("file-and"),
                paths.len() - 5,
                tr("file-more")
            )
        } else {
            names
        };
        div()
            .mx_3()
            .mb_3()
            .p_3()
            .rounded_md()
            .border_1()
            .border_color(cx.theme().danger)
            .flex()
            .flex_col()
            .gap_2()
            .child(if paths.len() == 1 {
                tr("file-delete-one-help")
            } else {
                format!("{} {}", paths.len(), tr("file-delete-many-help"))
            })
            .child(div().text_xs().child(summary))
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        Button::new("confirm-delete")
                            .small()
                            .danger()
                            .label(tr("file-delete-permanently"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                if let Some(paths) = this.pending_delete.take() {
                                    this.enqueue(
                                        Job::Delete {
                                            paths,
                                            control: TransferControl::default(),
                                        },
                                        cx,
                                    );
                                }
                            })),
                    )
                    .child(
                        Button::new("keep-file")
                            .small()
                            .label(tr("file-keep-file"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.pending_delete = None;
                                cx.notify();
                            })),
                    ),
            )
            .into_any_element()
    }
    fn transfers_view(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = cx.theme();
        let border = theme.border;
        let muted = theme.muted_foreground;
        let subtle = theme.muted;
        let mut tasks = div()
            .id("transfer-tasks")
            .max_h(px(if self.full_page { 220. } else { 140. }))
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_1()
            .p_2()
            .border_t_1()
            .border_color(border)
            .bg(subtle)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(tr("file-transfers")),
                    )
                    .child(
                        Button::new("clear-transfers")
                            .xsmall()
                            .ghost()
                            .label(tr("file-clear-finished"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.transfers.retain(|task| task.state.active());
                                cx.notify();
                            })),
                    ),
            );
        if self.transfers.is_empty() {
            tasks = tasks.child(
                div()
                    .text_xs()
                    .text_color(muted)
                    .child(tr("file-no-transfers")),
            );
        }
        for task in self.transfers.iter().rev() {
            tasks = tasks.child(self.transfer_card(task, cx));
        }
        tasks.into_any_element()
    }
    fn transfer_card(&self, task: &TransferTask, cx: &mut Context<Self>) -> gpui::AnyElement {
        if self.full_page && task.state == TransferState::Completed {
            return self.completed_transfer_card(task, cx);
        }
        let theme = cx.theme();
        let foreground = theme.sidebar_foreground;
        let border = theme.border;
        let muted = theme.muted_foreground;
        let id = task.id;
        let active = task.state.active();
        let mut card = div()
            .p_2()
            .border_b_1()
            .border_color(border)
            .flex()
            .flex_col()
            .gap_2()
            .child(div().text_xs().overflow_hidden().child(task.label.clone()))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_xs()
                            .text_color(if active { foreground } else { muted })
                            .child(task.state.label()),
                    )
                    .child(div().text_xs().text_color(muted).child(format!(
                        "{}: {}",
                        tr("file-current-file"),
                        format_bytes(task.bytes)
                    ))),
            );
        if active {
            card = card.child(
                Button::new(("cancel-transfer", id))
                    .xsmall()
                    .ghost()
                    .label(tr("file-cancel-transfer"))
                    .on_click(cx.listener(move |this, _, _, cx| this.cancel_transfer(id, cx))),
            );
        } else if task.retry.is_some() {
            card = card.child(
                Button::new(("retry-transfer", id))
                    .xsmall()
                    .ghost()
                    .label(tr("file-retry-transfer"))
                    .tooltip(tr("file-retry-help"))
                    .disabled(!self.available)
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.retry_transfer(id, window, cx)),
                    ),
            );
        }
        if let Some(pending) = &task.conflict {
            card = card
                .child(div().text_xs().child(format!(
                    "{} {}",
                    tr("upload-file-exists"),
                    pending.conflict.destination
                )))
                .child(
                    crate::design::PrimaryAction::new(("transfer-review", id))
                        .small()
                        .label(tr("conflict-review"))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.review_conflict(id, window, cx)
                        })),
                );
        }
        if let Some(failure) = &task.failure {
            card = card.child(div().text_xs().text_color(muted).child(failure.clone()));
        }
        card.into_any_element()
    }
    fn status_bar(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let selected = self.selected_entries();
        let summary = (!selected.is_empty()).then(|| {
            let bytes: u64 = selected
                .iter()
                .filter(|entry| !entry.is_directory)
                .filter_map(|entry| entry.size)
                .sum();
            format!(
                "{} {} · {}",
                selected.len(),
                tr("file-selected"),
                format_bytes(bytes)
            )
        });
        div()
            .px_3()
            .py_2()
            .flex()
            .gap_3()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .border_t_1()
            .border_color(cx.theme().border)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(self.message.clone()),
            )
            .when_some(summary, |d, summary| {
                d.child(div().flex_shrink_0().child(summary))
            })
            .into_any_element()
    }
    /// Back, Forward and Up; Up also accepts drops into the parent folder.
    fn history_controls(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let up = div()
            .id("file-up-target")
            .rounded_md()
            .border_1()
            .border_color(gpui::transparent_black());
        let up = match browser::parent_of(&self.directory) {
            Some(parent) => self.folder_drop_target(up, parent, cx),
            None => up,
        };
        div()
            .flex()
            .items_center()
            .gap_1()
            .child(
                Button::new("file-back")
                    .small()
                    .ghost()
                    .icon(IconName::ArrowLeft)
                    .tooltip(tr("file-back"))
                    .accessibility_label(tr("file-back"))
                    .disabled(!self.history.can_go_back() || !self.available)
                    .on_click(cx.listener(|this, _, _, cx| this.go_back(cx))),
            )
            .child(
                Button::new("file-forward")
                    .small()
                    .ghost()
                    .icon(IconName::ArrowRight)
                    .tooltip(tr("file-forward"))
                    .accessibility_label(tr("file-forward"))
                    .disabled(!self.history.can_go_forward() || !self.available)
                    .on_click(cx.listener(|this, _, _, cx| this.go_forward(cx))),
            )
            .child(
                up.child(
                    Button::new("up")
                        .small()
                        .ghost()
                        .label(tr("file-up"))
                        .disabled(browser::parent_of(&self.directory).is_none())
                        .on_click(cx.listener(|this, _, _, cx| this.go_up(cx))),
                ),
            )
            .into_any_element()
    }
}
impl Render for FilePane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.schedule_conflict(window, cx);
        if self.browser_dirty {
            self.visible = visible_entries(
                &self.entries,
                &self.search.read(cx).value(),
                self.show_hidden,
                self.sort,
                self.descending,
            );
            self.browser_dirty = false;
        }
        self.columns = if self.full_page && self.grid {
            ((f32::from(window.viewport_size().width) - 480.) / 210.)
                .floor()
                .max(1.) as usize
        } else {
            1
        };
        self.prune_selection();
        let theme = cx.theme();
        let (background, foreground) = (theme.sidebar, theme.sidebar_foreground);
        let mut pane = div()
            .id("file-pane")
            .track_focus(&self.list_focus)
            .on_mouse_down(
                gpui::MouseButton::Navigate(gpui::NavigationDirection::Back),
                cx.listener(|this, _, _, cx| this.go_back(cx)),
            )
            .on_mouse_down(
                gpui::MouseButton::Navigate(gpui::NavigationDirection::Forward),
                cx.listener(|this, _, _, cx| this.go_forward(cx)),
            )
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                // Inputs (path bar, search, rename) keep their own keys.
                if !this.list_focus.is_focused(window) {
                    return;
                }
                if this.handle_list_key(event, window, cx) {
                    cx.stop_propagation();
                }
            }))
            .flex()
            .flex_col()
            .w_full()
            .h_full()
            .min_h_0()
            .min_w_0()
            .overflow_y_scroll()
            .bg(background)
            .text_color(foreground);
        pane = pane
            .child(self.file_title(cx))
            .child(self.path_controls(cx));
        if self.full_page {
            pane = pane.child(self.breadcrumb_controls(cx));
        }
        pane = pane.child(self.browser_controls(cx));
        if !self.full_page || !self.grid {
            pane = pane.child(self.list_header(cx));
        }
        pane = pane
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h(px(120.))
                    .when(self.full_page, |d| d.child(self.places_view(cx)))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .child(self.entries_view(cx)),
                    ),
            )
            .child(self.file_actions(cx));
        if let Some(paths) = self.pending_delete.clone() {
            pane = pane.child(self.delete_confirmation(paths, cx));
        }
        if !self.full_page || !self.transfers.is_empty() {
            pane = pane.child(self.transfers_view(cx));
        }
        pane.child(self.status_bar(cx)).into_any_element()
    }
}

fn format_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KiB", bytes as f64 / 1024.)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:.1} MiB", bytes as f64 / (1024. * 1024.))
    } else {
        format!("{:.1} GiB", bytes as f64 / (1024. * 1024. * 1024.))
    }
}
fn interrupt_transfers(transfers: &mut [TransferTask]) {
    for task in transfers {
        if task.state.active() {
            task.bytes = task.control.batch_transferred();
            task.control.cancel();
            task.conflict = None;
            task.state = TransferState::Failed;
            task.failure = Some(tr("file-interrupted-transfer"));
        }
    }
}
fn visible_entries(
    entries: &[Entry],
    search: &str,
    show_hidden: bool,
    sort: FileSort,
    descending: bool,
) -> Vec<usize> {
    let query = search.to_lowercase();
    let mut indices: Vec<_> = entries
        .iter()
        .enumerate()
        .filter(|(_, entry)| {
            (show_hidden || !entry.name.starts_with('.'))
                && entry.name.to_lowercase().contains(&query)
        })
        .map(|(index, _)| index)
        .collect();
    indices.sort_by(|&a, &b| {
        let (left, right) = (&entries[a], &entries[b]);
        right.is_directory.cmp(&left.is_directory).then_with(|| {
            let order = match sort {
                FileSort::Name => left.name.to_lowercase().cmp(&right.name.to_lowercase()),
                FileSort::Size => left.size.cmp(&right.size),
                FileSort::Modified => left.modified.cmp(&right.modified),
            }
            .then_with(|| left.name.cmp(&right.name));
            if descending { order.reverse() } else { order }
        })
    });
    indices
}
fn breadcrumbs(path: &str) -> Vec<(String, String)> {
    let mut result = vec![(tr("file-root"), "/".into())];
    let mut current = String::new();
    for component in path.split('/').filter(|part| !part.is_empty()) {
        current.push('/');
        current.push_str(component);
        result.push((component.into(), current.clone()));
    }
    result
}
fn entry_icon(entry: &Entry) -> gpui_kit_assets::IconName {
    use gpui_kit_assets::IconName as I;
    if entry.is_symlink {
        return if entry.is_directory {
            I::FolderSymlink
        } else {
            I::FileSymlink
        };
    }
    if entry.is_directory {
        return I::Folder;
    }
    let extension = entry
        .name
        .rsplit_once('.')
        .map(|(_, ext)| ext.to_ascii_lowercase())
        .unwrap_or_default();
    match extension.as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "bmp" => I::FileImage,
        "sh" | "bash" | "py" | "rs" | "js" | "ts" | "tsx" | "jsx" | "html" | "css" => I::FileCode,
        "toml" | "yaml" | "yml" | "json" | "ini" | "conf" => I::FileCog,
        "zip" | "gz" | "tgz" | "tar" | "xz" | "7z" => I::FileArchive,
        "txt" | "md" | "log" | "csv" => I::FileText,
        _ => I::File,
    }
}
fn file_kind(entry: &Entry) -> String {
    if entry.is_symlink {
        tr("file-type-link")
    } else if entry.is_directory {
        tr("file-type-folder")
    } else {
        entry
            .name
            .rsplit_once('.')
            .filter(|(name, ext)| !name.is_empty() && !ext.is_empty())
            .map(|(_, ext)| ext.chars().take(8).collect::<String>().to_uppercase())
            .unwrap_or_else(|| tr("file-type-file"))
    }
}
/// Render SFTP's UTC Unix modification time without depending on local time zones.
fn format_modified(timestamp: u32) -> String {
    let days = i64::from(timestamp) / 86400 + 719468;
    let era = days / 146097;
    let day_of_era = days - era * 146097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36524 - day_of_era / 146096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_position = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_position + 2) / 5 + 1;
    let month = month_position + if month_position < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02} UTC")
}

async fn worker(
    commands: Sender<SshCommand>,
    fingerprint: String,
    jobs: Receiver<QueuedJob>,
    updates: Sender<Update>,
) -> opsssh_sftp::Result<()> {
    let (reply, receive) = async_channel::bounded(1);
    commands.send(SshCommand::OpenSftp { reply }).await?;
    let stream = tokio::time::timeout(Duration::from_secs(20), receive.recv())
        .await??
        .map_err(std::io::Error::other)?;
    let client = SftpClient::connect(stream, fingerprint).await?;
    let home = client.canonicalize(".").await?;
    while let Ok(QueuedJob { id, job }) = jobs.recv().await {
        if updates.is_closed() || jobs.is_closed() {
            break;
        }
        let _ = updates.send(Update::Started(id)).await;
        let validating = match &job {
            Job::ValidateDestination { request, .. } => Some(*request),
            _ => None,
        };
        let result = execute(&client, &commands, &home, job, &updates, id).await;
        let result = result.map_err(|error| error.to_string());
        if let Err(error) = &result {
            let _ = updates.send(Update::Message(error.to_string())).await;
            if let Some(request) = validating {
                let _ = updates
                    .send(Update::DestinationFailed(request, error.clone()))
                    .await;
            }
        }
        let _ = updates.send(Update::Finished(id, result)).await;
    }
    Ok(())
}
/// Runs on the transfer worker, never on GPUI's input/render thread.
async fn upload_size(paths: &[PathBuf], control: &TransferControl) -> opsssh_sftp::Result<u64> {
    let mut pending = paths.to_vec();
    let mut bytes = 0u64;
    while let Some(path) = pending.pop() {
        control.ensure_active()?;
        let metadata = tokio::fs::symlink_metadata(&path).await?;
        if metadata.is_symlink() {
            return Err("recursive upload does not follow symbolic links".into());
        }
        if metadata.is_dir() {
            let mut entries = tokio::fs::read_dir(&path).await?;
            while let Some(entry) = entries.next_entry().await? {
                pending.push(entry.path());
            }
        } else if metadata.is_file() {
            bytes = bytes
                .checked_add(metadata.len())
                .ok_or("upload size exceeds limits")?;
        } else {
            return Err("only regular files and folders can be uploaded".into());
        }
    }
    Ok(bytes)
}
async fn execute(
    client: &SftpClient,
    commands: &Sender<SshCommand>,
    home: &str,
    job: Job,
    updates: &Sender<Update>,
    id: u64,
) -> opsssh_sftp::Result<()> {
    match job {
        Job::ValidateDestination { request, path } => {
            let path = client.validate_upload_directory(&path).await?;
            updates
                .send(Update::DestinationValidated(request, path))
                .await?;
        }
        Job::List(path) => {
            let path = client.canonicalize(&path).await?;
            let entries = client.list(&path).await?;
            updates.send(Update::Listed(path, entries)).await?;
        }
        Job::Upload {
            paths,
            target,
            insert,
            control,
        } => {
            let directory = match target {
                Some(path) => client.canonicalize(&path).await?,
                None => client.private_staging(home).await?,
            };
            control.set_total(upload_size(&paths, &control).await?);
            let mut inserted = vec![];
            for path in paths {
                let name = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .ok_or_else(|| tr("file-invalid-name"))?;
                let remote = opsssh_drop::join_remote(&directory, name)?;
                client
                    .upload_tree_with_decisions(&path, &remote, &control, |conflict| {
                        conflict::request_conflict(updates, id, conflict, &control)
                    })
                    .await?;
                inserted.push(opsssh_drop::quote(&remote, opsssh_drop::Shell::Posix)?);
            }
            if insert {
                updates
                    .send(Update::Insert(id, format!("{} ", inserted.join(" "))))
                    .await?;
            } else {
                refresh_directory(client, &directory, updates).await;
            }
            updates
                .send(Update::Message(format!(
                    "{}: {directory}",
                    tr("file-upload-complete")
                )))
                .await?;
        }
        Job::Download {
            remote,
            local,
            confirmed_existing,
            control,
        } => {
            let report = client
                .download_tree_with_decisions(&remote, &local, &control, |conflict| {
                    let preconfirmed = preconfirmed_download(
                        &local,
                        confirmed_existing,
                        std::path::Path::new(&conflict.destination),
                    );
                    let control = control.clone();
                    async move {
                        if preconfirmed {
                            // The save dialog already asked; never ask twice for the same file.
                            control.ensure_active()?;
                            Ok(ConflictReply::once(ConflictDecision::Replace))
                        } else {
                            conflict::request_conflict(updates, id, conflict, &control).await
                        }
                    }
                })
                .await?;
            updates
                .send(Update::Message(format!(
                    "{}: {report}",
                    tr("file-download-complete")
                )))
                .await?;
            actions::report_result(&report)?;
        }
        Job::Mkdir(path) => {
            client.create_directory(&path).await?;
            updates
                .send(Update::Message(tr("file-folder-created")))
                .await?;
            refresh_parent(client, &path, updates).await;
        }
        Job::Delete { paths, control } => {
            let mut report = opsssh_sftp::OperationReport::default();
            for path in &paths {
                if control.ensure_active().is_err() {
                    report.cancelled = true;
                    break;
                }
                // One failing item is reported; the rest are still deleted.
                match client.delete_tree(path, &control).await {
                    Ok(part) => {
                        report.changed += part.changed;
                        report.skipped += part.skipped;
                        report.failures.extend(part.failures);
                        report.cancelled |= part.cancelled;
                    }
                    Err(error) => report.failures.push(format!("{path}: {error}")),
                }
            }
            if let Some(path) = paths.first() {
                refresh_parent(client, path, updates).await;
            }
            updates
                .send(Update::Message(format!("{}: {report}", tr("file-delete"))))
                .await?;
            actions::report_result(&report)?;
        }
        Job::Attributes { path, serial } => {
            let result = actions::load_attributes(client, commands, &path)
                .await
                .map_err(|e| e.to_string());
            updates.send(Update::Attributes { serial, result }).await?;
        }
        Job::Permissions {
            path,
            serial,
            expected,
            mut change,
            owner,
            group,
            control,
        } => {
            let result = async {
                if let Some(owner) = owner {
                    change.uid = Some(actions::resolve_identity(commands, &owner, false).await?);
                }
                if let Some(group) = group {
                    change.gid = Some(actions::resolve_identity(commands, &group, true).await?);
                }
                let report = client
                    .change_permissions(&path, &expected, &change, &control)
                    .await?;
                let attrs = client.attributes(&path).await?;
                Ok::<_, Box<dyn std::error::Error + Send + Sync>>((attrs, report))
            }
            .await;
            let message = result
                .as_ref()
                .map(|(_, r)| r.to_string())
                .map_err(|e| e.to_string());
            updates
                .send(Update::PermissionsFinished {
                    serial,
                    result: result
                        .as_ref()
                        .map(|(a, r)| (a.clone(), r.to_string()))
                        .map_err(|e| e.to_string()),
                })
                .await?;
            refresh_parent(client, &path, updates).await;
            let (_, report) = result?;
            updates
                .send(Update::Message(format!("Permissions: {}", message?)))
                .await?;
            actions::report_result(&report)?;
        }
        Job::Transfer {
            items,
            destination,
            mode,
            control,
        } => {
            // posix-rename makes a confirmed move-replace atomic; without it a safe swap is used.
            let renamer = if mode == TransferMode::Move {
                actions::atomic_renamer(commands, &control).await.ok()
            } else {
                None
            };
            let report = client
                .transfer_items(
                    &items,
                    &destination,
                    mode,
                    renamer.as_ref(),
                    &control,
                    |conflict| conflict::request_conflict(updates, id, conflict, &control),
                )
                .await;
            for item in &items {
                refresh_parent(client, &item.source, updates).await;
            }
            refresh_directory(client, &destination, updates).await;
            let report = report?;
            updates
                .send(Update::Message(format!(
                    "{}: {report}",
                    tr(if mode == TransferMode::Copy {
                        "file-copy-complete"
                    } else {
                        "file-move-complete"
                    })
                )))
                .await?;
            actions::report_result(&report)?;
        }
    }
    Ok(())
}
/// True when the native save dialog chose an existing regular file, which the OS
/// dialog has already asked to replace.
fn picked_existing_file(path: &std::path::Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_file())
}
/// Only the exact file confirmed by the save dialog skips OpsSSH's replacement prompt.
/// Files inside a downloaded folder were never shown by the dialog and still ask.
fn preconfirmed_download(
    chosen: &std::path::Path,
    confirmed_existing: bool,
    conflict: &std::path::Path,
) -> bool {
    confirmed_existing && conflict == chosen
}
async fn refresh_parent(client: &SftpClient, path: &str, updates: &Sender<Update>) {
    let parent = path
        .rsplit_once('/')
        .map(|(parent, _)| if parent.is_empty() { "/" } else { parent })
        .unwrap_or("/");
    refresh_directory(client, parent, updates).await;
}
async fn refresh_directory(client: &SftpClient, path: &str, updates: &Sender<Update>) {
    // A completed upload must remain completed even if the optional listing refresh fails.
    if let Ok(entries) = client.list(path).await {
        let _ = updates.send(Update::Refreshed(path.into(), entries)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "capture")]
    #[gpui::test]
    fn upload_collision_waits_for_explicit_replace_and_cancel_keeps_the_file(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(gpui_component::init);
        let mut pane = None;
        let (_root, cx) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| FilePane::preview(window, cx));
            pane = Some(view.clone());
            gpui_component::Root::new(view, window, cx)
        });
        let pane = pane.unwrap();
        let replace = ConflictReply::once(ConflictDecision::Replace);
        cx.update(|window, cx| {
            pane.update(cx, |pane, cx| {
                pane.preview_transfer(false);
                let id = pane.transfers[0].id;
                pane.dismiss_transfer_notice(id, cx);
                let (reply, receive) = async_channel::bounded(1);
                let conflict = conflict::sample_conflict("/opt/test.txt");
                pane.receive_conflict(id, conflict, reply, window, cx);
                assert!(!pane.transfers[0].dismissed);
                assert!(pane.transfers[0].state == TransferState::AwaitingReplace);
                assert_eq!(pane.conflict_dialog, Some(id), "the dialog asks right away");
                assert!(receive.try_recv().is_err(), "replace must not be automatic");
                assert!(pane.resolve_conflict(id, replace, cx));
                assert_eq!(receive.try_recv(), Ok(replace));
                assert!(pane.transfers[0].state == TransferState::Running);
                assert!(
                    !pane.resolve_conflict(id, replace, cx),
                    "answered only once"
                );
                assert!(receive.try_recv().is_err());
                let (reply, receive) = async_channel::bounded(1);
                pane.conflict_dialog = None;
                let conflict = conflict::sample_conflict("/opt/test.txt");
                pane.receive_conflict(id, conflict, reply, window, cx);
                pane.dismiss_transfer_notice(id, cx);
                assert!(pane.transfers[0].state == TransferState::Cancelling);
                assert!(receive.try_recv().is_err());
                assert!(pane.transfers[0].control.ensure_active().is_err());
            })
        });
    }

    #[test]
    fn replacement_worker_waits_for_decision_and_honors_cancellation() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let (updates, receive) = async_channel::bounded(1);
            let control = TransferControl::default();
            let (result, ()) = tokio::join!(
                conflict::request_conflict(
                    &updates,
                    42,
                    conflict::sample_conflict("/opt/test.txt"),
                    &control
                ),
                async {
                    let Update::Conflict(id, conflict, reply) = receive.recv().await.unwrap()
                    else {
                        panic!("expected collision");
                    };
                    assert_eq!((id, conflict.destination.as_str()), (42, "/opt/test.txt"));
                    reply
                        .send(ConflictReply::once(ConflictDecision::KeepBoth))
                        .await
                        .unwrap();
                }
            );
            assert_eq!(result.unwrap().decision, ConflictDecision::KeepBoth);
            let (result, ()) = tokio::join!(
                conflict::request_conflict(
                    &updates,
                    43,
                    conflict::sample_conflict("/opt/test.txt"),
                    &control
                ),
                async {
                    let Update::Conflict(_, _, _reply) = receive.recv().await.unwrap() else {
                        panic!("expected collision");
                    };
                    control.cancel();
                    tokio::time::sleep(Duration::from_millis(120)).await;
                }
            );
            assert!(result.is_err());
        });
    }

    #[cfg(feature = "capture")]
    fn queued(pane: &FilePane) -> Vec<Job> {
        let receiver = pane._preview_jobs.as_ref().unwrap();
        std::iter::from_fn(|| receiver.try_recv().ok().map(|queued| queued.job)).collect()
    }

    #[cfg(feature = "capture")]
    fn transfer(job: &Job) -> (Vec<String>, String, TransferMode) {
        let Job::Transfer {
            items,
            destination,
            mode,
            ..
        } = job
        else {
            panic!("expected a transfer");
        };
        (
            items.iter().map(|item| item.name.clone()).collect(),
            destination.clone(),
            *mode,
        )
    }

    #[cfg(feature = "capture")]
    #[gpui::test]
    fn dragging_a_selection_moves_it_and_refuses_unsafe_targets(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        let (pane, cx) = cx.add_window_view(FilePane::preview);
        cx.update(|window, cx| {
            pane.update(cx, |this, cx| {
                this.visible = (0..this.entries.len()).collect();
                let toml = this.entries[2].clone();
                let readme = this.entries[6].clone();
                this.selection.select_only(&toml.path);
                this.click_entry(
                    &readme.path,
                    &gpui::Modifiers {
                        control: !cfg!(target_os = "macos"),
                        platform: cfg!(target_os = "macos"),
                        ..Default::default()
                    },
                );
                let items = this.drag_value(&toml, cx.entity_id());
                assert_eq!(items.paths.len(), 2, "the whole selection travels");
                this.drop_items(&items, "/srv/application/logs".into(), window, cx);
                let jobs = queued(this);
                assert_eq!(jobs.len(), 1);
                assert_eq!(
                    transfer(&jobs[0]),
                    (
                        vec!["application.toml".into(), "README.md".into()],
                        "/srv/application/logs".into(),
                        TransferMode::Move
                    )
                );

                // An unselected item is dragged alone.
                let logs = this.entries[0].clone();
                let folder = this.drag_value(&logs, cx.entity_id());
                assert_eq!(folder.paths, vec![logs.path.clone()]);
                for target in [
                    logs.path.clone(),
                    format!("{}/nested", logs.path),
                    "/srv/application".into(),
                ] {
                    this.drop_items(&folder, target, window, cx);
                }
                let mut stale = folder.clone();
                stale.generation += 1;
                this.drop_items(&stale, "/srv".into(), window, cx);
                assert!(
                    queued(this).is_empty(),
                    "self, descendant, no-op and stale drops"
                );
            })
        });
    }

    #[cfg(feature = "capture")]
    #[gpui::test]
    fn cut_items_fade_until_pasted_and_copies_paste_repeatedly(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        let (pane, cx) = cx.add_window_view(FilePane::preview);
        cx.update(|_, cx| {
            pane.update(cx, |this, cx| {
                this.visible = (0..this.entries.len()).collect();
                let readme = this.entries[6].path.clone();
                this.selection.select_only(&readme);
                this.copy_selection(TransferMode::Move, cx);
                assert!(this.is_cut(&readme));
                // With one folder selected, Paste goes into that folder.
                this.selection.select_only("/srv/application/releases");
                let target = this.paste_target();
                assert_eq!(target, "/srv/application/releases");
                this.paste(target, cx);
                assert!(this.clipboard.is_none() && !this.is_cut(&readme));
                assert_eq!(transfer(&queued(this)[0]).2, TransferMode::Move);

                this.selection.select_only(&readme);
                this.copy_selection(TransferMode::Copy, cx);
                this.selection.clear();
                assert_eq!(this.paste_target(), "/srv/application");
                this.paste(this.paste_target(), cx);
                this.paste(this.paste_target(), cx);
                assert_eq!(queued(this).len(), 2);
                // A clipboard from an earlier connection is never replayed.
                this.worker_generation += 1;
                this.paste("/srv/application".into(), cx);
                assert!(queued(this).is_empty() && this.clipboard.is_none());
            })
        });
    }

    #[cfg(feature = "capture")]
    #[gpui::test]
    fn inline_rename_validates_and_moves_within_the_folder(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        let (pane, cx) = cx.add_window_view(FilePane::preview);
        cx.update(|window, cx| {
            pane.update(cx, |this, cx| {
                this.visible = (0..this.entries.len()).collect();
                this.selection.select_only("/srv/application/README.md");
                this.begin_rename(window, cx);
                let input = this.rename.as_ref().unwrap().input.clone();
                input.update(cx, |input, cx| input.set_value("bad/name", window, cx));
                this.commit_rename(window, cx);
                assert!(this.rename.as_ref().unwrap().error.is_some());
                assert!(queued(this).is_empty());
                input.update(cx, |input, cx| input.set_value("NOTES.md", window, cx));
                this.commit_rename(window, cx);
                assert!(this.rename.is_none());
                assert_eq!(
                    transfer(&queued(this)[0]),
                    (
                        vec!["NOTES.md".into()],
                        "/srv/application".into(),
                        TransferMode::Move
                    )
                );
                // Escape-style cancel leaves nothing queued.
                this.begin_rename(window, cx);
                this.cancel_rename(window, cx);
                assert!(this.rename.is_none() && queued(this).is_empty());
            })
        });
    }

    #[cfg(feature = "capture")]
    #[gpui::test]
    fn keyboard_selects_all_asks_before_delete_and_clears(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        let (pane, cx) = cx.add_window_view(FilePane::preview);
        cx.update(|window, cx| {
            pane.update(cx, |this, cx| {
                this.list_focus.focus(window, cx);
                cx.notify();
            })
        });
        cx.run_until_parked();
        let select_all = if cfg!(target_os = "macos") {
            "cmd-a"
        } else {
            "ctrl-a"
        };
        cx.simulate_keystrokes(select_all);
        cx.update(|_, cx| {
            let this = pane.read(cx);
            assert_eq!(this.selected_entries().len(), this.entries.len());
        });
        cx.simulate_keystrokes("delete");
        cx.update(|_, cx| {
            let this = pane.read(cx);
            assert_eq!(this.pending_delete.as_ref().map(Vec::len), Some(7));
            assert!(
                queued(this).is_empty(),
                "nothing is deleted before confirmation"
            );
        });
        cx.simulate_keystrokes("escape");
        cx.update(|_, cx| assert!(pane.read(cx).selected_entries().is_empty()));
        cx.simulate_keystrokes("end shift-up");
        cx.update(|_, cx| {
            let names: Vec<_> = pane
                .read(cx)
                .selected_entries()
                .into_iter()
                .map(|entry| entry.name)
                .collect();
            assert_eq!(names.len(), 2, "{names:?}");
        });
    }

    #[cfg(feature = "capture")]
    #[gpui::test]
    fn dismissing_upload_notices_retains_history_and_does_not_cancel_work(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(gpui_component::init);
        let (pane, cx) = cx.add_window_view(FilePane::preview);
        cx.update(|_, cx| {
            pane.update(cx, |pane, cx| {
                pane.preview_transfer(false);
                let id = pane.transfers[0].id;
                let count = pane.transfers.len();
                pane.dismiss_transfer_notice(id, cx);
                assert!(pane.transfers[0].dismissed);
                assert!(pane.transfers[0].control.ensure_active().is_ok());
                assert_eq!(pane.active_transfers(), 1);
                pane.preview_completed_upload("'/srv/upload.png' ".into(), cx);
                assert_eq!(
                    pane.pending_upload_paths().len(),
                    1,
                    "dismissed running uploads still complete"
                );
                pane.dismiss_transfer_notice(id, cx);
                assert!(pane.pending_upload_paths().is_empty());
                assert_eq!(pane.transfers.len(), count);
                assert_eq!(
                    pane.transfers[0].remote_paths.as_deref(),
                    Some("'/srv/upload.png' ")
                );
                assert!(pane.transfers[0].dismissed);
            })
        });
    }

    #[test]
    fn preparation_counts_nested_files_and_obeys_cancellation() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("nested")).unwrap();
        std::fs::write(root.path().join("one"), b"hello").unwrap();
        std::fs::write(root.path().join("nested/測試.txt"), b"world!").unwrap();
        std::fs::write(root.path().join("empty"), b"").unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let control = TransferControl::default();
        assert_eq!(
            runtime
                .block_on(upload_size(&[root.path().to_owned()], &control))
                .unwrap(),
            11
        );
        control.cancel();
        assert!(
            runtime
                .block_on(upload_size(&[root.path().to_owned()], &control))
                .is_err()
        );
    }

    #[cfg(feature = "capture")]
    #[gpui::test]
    fn failed_jobs_keep_destination_and_deferred_paths_survive_recovery(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(gpui_component::init);
        let (pane, cx) = cx.add_window_view(FilePane::preview);
        cx.update(|_,cx|pane.update(cx,|pane,cx| {
            pane.preview_transfer(true);
            let id=pane.transfers[0].id;
            pane.transfers[0].remote_paths=Some("'/srv/uploads/example.txt' ".into());
            pane.defer_insert(id,cx);
            pane.directory="/unrelated".into();
            pane.change_retry_destination("/new/uploads".into(),cx);
            assert!(matches!(&pane.transfers[0].retry,Some(Job::Upload{target:Some(path),insert:true,..}) if path=="/new/uploads"));
            pane.suspend(cx);
            assert!(pane.transfers[0].pending_insert);
            assert_eq!(pane.transfers[0].remote_paths.as_deref(),Some("'/srv/uploads/example.txt' "));
        }));
    }

    fn entry(name: &str, directory: bool, size: Option<u64>, modified: Option<u32>) -> Entry {
        Entry {
            name: name.into(),
            path: format!("/srv/{name}"),
            size,
            modified,
            is_directory: directory,
            is_symlink: false,
        }
    }

    #[test]
    fn filtering_is_case_insensitive_and_hidden_files_are_opt_in() {
        let entries = vec![
            entry(".Config", false, None, None),
            entry("config.TOML", false, None, None),
            entry("logs", true, None, None),
        ];
        assert_eq!(
            visible_entries(&entries, "CONFIG", false, FileSort::Name, false),
            vec![1]
        );
        assert_eq!(
            visible_entries(&entries, "config", true, FileSort::Name, false),
            vec![0, 1]
        );
        assert!(visible_entries(&entries, "missing", true, FileSort::Name, false).is_empty());
    }

    #[test]
    fn directories_stay_first_in_both_sort_directions() {
        let entries = vec![
            entry("large", false, Some(100), Some(20)),
            entry("z-folder", true, None, None),
            entry("small", false, Some(5), Some(30)),
            entry("a-folder", true, None, None),
        ];
        assert_eq!(
            visible_entries(&entries, "", false, FileSort::Name, false),
            vec![3, 1, 0, 2]
        );
        assert_eq!(
            visible_entries(&entries, "", false, FileSort::Size, true),
            vec![1, 3, 0, 2]
        );
        assert_eq!(
            visible_entries(&entries, "", false, FileSort::Modified, true),
            vec![1, 3, 2, 0]
        );
    }

    #[test]
    fn missing_metadata_and_equal_names_have_deterministic_order() {
        let entries = vec![
            entry("z", false, None, None),
            entry("a", false, Some(1), Some(1)),
            entry("A", false, Some(1), Some(1)),
        ];
        assert_eq!(
            visible_entries(&entries, "", false, FileSort::Size, false),
            vec![0, 2, 1]
        );
    }

    #[test]
    fn breadcrumb_targets_preserve_absolute_unicode_paths() {
        assert_eq!(
            breadcrumbs("/srv/测试/releases/"),
            vec![
                ("Root".into(), "/".into()),
                ("srv".into(), "/srv".into()),
                ("测试".into(), "/srv/测试".into()),
                ("releases".into(), "/srv/测试/releases".into())
            ]
        );
        assert_eq!(breadcrumbs("/"), vec![("Root".into(), "/".into())]);
    }

    #[test]
    fn modification_dates_use_utc_and_handle_leap_days() {
        assert_eq!(format_modified(0), "1970-01-01 UTC");
        assert_eq!(format_modified(946684800), "2000-01-01 UTC");
        assert_eq!(format_modified(1709164800), "2024-02-29 UTC");
        assert_eq!(format_modified(u32::MAX), "2106-02-07 UTC");
    }
    #[test]
    fn save_dialog_confirmation_covers_only_the_existing_chosen_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("report.txt");
        let folder = dir.path().join("logs");
        std::fs::write(&file, b"old").unwrap();
        std::fs::create_dir(&folder).unwrap();
        assert!(picked_existing_file(&file));
        assert!(!picked_existing_file(&folder));
        assert!(!picked_existing_file(&dir.path().join("missing.txt")));

        assert!(preconfirmed_download(&file, true, &file));
        // A file that did not exist when the dialog closed was never confirmed.
        assert!(!preconfirmed_download(&file, false, &file));
        // Files inside a downloaded folder were never shown by the save dialog.
        assert!(!preconfirmed_download(
            &folder,
            true,
            &folder.join("app.log")
        ));
    }
    #[test]
    fn interrupted_transfers_retain_retry_descriptors_and_completed_work() {
        let control = TransferControl::default();
        let job = Job::Upload {
            paths: vec![PathBuf::from("example.txt")],
            target: Some("/srv".into()),
            insert: false,
            control: control.clone(),
        };
        let mut tasks = vec![
            TransferTask {
                id: 1,
                label: "upload".into(),
                control,
                state: TransferState::Running,
                bytes: 0,
                retry: Some(job),
                failure: None,
                completed_at: None,
                remote_paths: None,
                pending_insert: false,
                insert_generation: None,
                dismissed: false,
                conflict: None,
            },
            TransferTask {
                id: 2,
                label: "done".into(),
                control: TransferControl::default(),
                state: TransferState::Completed,
                bytes: 32,
                retry: None,
                failure: None,
                completed_at: None,
                remote_paths: None,
                pending_insert: false,
                insert_generation: None,
                dismissed: false,
                conflict: None,
            },
        ];
        interrupt_transfers(&mut tasks);
        assert!(tasks[0].state == TransferState::Failed);
        assert!(tasks[0].retry.is_some());
        assert!(tasks[0].failure.is_some());
        assert!(tasks[1].state == TransferState::Completed);
        assert_eq!(tasks[1].bytes, 32);
        assert!(tasks[1].failure.is_none());
    }
}
