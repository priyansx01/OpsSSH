//! SFTP pane: all protocol, disk and transfer work runs on one background runtime.
use crate::localization::text as tr;
use async_channel::{Receiver, Sender};
use gpui::{
    ClipboardEntry, ClipboardItem, Context, Entity, EventEmitter, PathPromptOptions, Render, Task,
    Window, div, prelude::*, px, uniform_list,
};
use gpui_component::{
    ActiveTheme, Disableable, Icon, IconName, Selectable, Sizable,
    button::{Button, ButtonVariants},
    input::{Input, InputEvent, InputState},
    menu::{DropdownMenu, PopupMenuItem},
};
use opsssh_sftp::{Entry, SftpClient, TransferControl, WriteMode};
use opsssh_ssh_core::SshCommand;
use std::{path::PathBuf, sync::Arc, time::Duration};

pub enum FilePaneEvent {
    InsertText(u64, String),
    FocusTerminal,
    DestinationValidated(u64, String),
    DestinationFailed(u64, String),
    ChooseUploadDestination,
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
        control: TransferControl,
    },
    Attachment {
        bytes: Arc<[u8]>,
        name: String,
        control: TransferControl,
    },
    Mkdir(String),
    Delete(String),
}
enum Update {
    Listed(String, Vec<Entry>),
    Refreshed(String, Vec<Entry>),
    Message(String),
    Unavailable(String),
    Insert(u64, String),
    DestinationValidated(u64, String),
    DestinationFailed(u64, String),
    Started(u64),
    Finished(u64, Result<(), String>),
}
struct QueuedJob {
    id: u64,
    job: Job,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum TransferState {
    Queued,
    Running,
    Cancelling,
    Completed,
    Cancelled,
    Failed,
}
impl TransferState {
    fn active(self) -> bool {
        matches!(self, Self::Queued | Self::Running | Self::Cancelling)
    }
    fn label(self) -> String {
        tr(match self {
            Self::Queued => "file-task-queued",
            Self::Running => "file-task-running",
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
    selected: Option<usize>,
    message: String,
    pending_delete: Option<String>,
    pending: usize,
    next_id: u64,
    transfers: Vec<TransferTask>,
    progress_task: Option<Task<()>>,
    worker_generation: u64,
    available: bool,
    pending_validation: Option<u64>,
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
            selected: Some(2),
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
            }],
            progress_task: None,
            worker_generation: 0,
            available: true,
            pending_validation: None,
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
            selected: None,
            message: tr("file-opening"),
            pending_delete: None,
            pending: 0,
            next_id: 0,
            transfers: vec![],
            progress_task: None,
            worker_generation: 0,
            available: true,
            pending_validation: None,
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
                            Update::DestinationValidated(request, path) => {
                                this.pending_validation = None;
                                cx.emit(FilePaneEvent::DestinationValidated(request, path));
                            }
                            Update::DestinationFailed(request, error) => {
                                this.pending_validation = None;
                                cx.emit(FilePaneEvent::DestinationFailed(request, error));
                            }
                            Update::Listed(path, entries) => {
                                if this.directory != path {
                                    this.search
                                        .update(cx, |state, cx| state.set_value("", window, cx));
                                }
                                this.directory = path.clone();
                                this.path
                                    .update(cx, |state, cx| state.set_value(path, window, cx));
                                this.entries = entries;
                                this.browser_dirty = true;
                                this.selected = None;
                                this.message =
                                    format!("{}: {}", tr("file-entry-count"), this.entries.len());
                            }
                            Update::Refreshed(path, entries) => {
                                if this.directory == path {
                                    let selected_path = this
                                        .selected
                                        .and_then(|index| this.entries.get(index))
                                        .map(|entry| entry.path.clone());
                                    this.entries = entries;
                                    this.selected = selected_path.and_then(|path| {
                                        this.entries.iter().position(|entry| entry.path == path)
                                    });
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
                        let _ = this.update_in(cx, |this, _, cx| {
                            if this.available && this.worker_generation == generation {
                                this.enqueue(
                                    Job::Download {
                                        remote,
                                        local,
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
            Job::Attachment { control, .. } => *control = TransferControl::default(),
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
            Job::Attachment { name, control, .. } => Some((
                format!("{} · {name}", tr("file-attachment")),
                control.clone(),
            )),
            _ => None,
        };
        let retry = transfer.as_ref().map(|_| job.clone());
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
            || !self
                .transfers
                .iter()
                .any(|t| matches!(t.state, TransferState::Running | TransferState::Cancelling))
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
            task.state = TransferState::Cancelling;
            self.message = tr("file-cancel-requested");
            cx.notify();
        }
    }
    fn refresh(&mut self, cx: &mut Context<Self>) {
        self.enqueue(Job::List(self.path.read(cx).value().to_string()), cx);
    }
    pub fn follow_directory(&mut self, path: String, cx: &mut Context<Self>) {
        if self.directory != path {
            self.enqueue(Job::List(path), cx);
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
    pub fn upload_clipboard(&mut self, item: ClipboardItem, cx: &mut Context<Self>) {
        for entry in item.entries {
            match entry {
                ClipboardEntry::ExternalPaths(paths) => {
                    self.upload_files(paths.0.iter().cloned().collect(), None, true, cx)
                }
                ClipboardEntry::Image(image) => {
                    if image.format != gpui::ImageFormat::Png
                        || image.bytes.len() > 64 * 1024 * 1024
                    {
                        self.message = tr("file-image-format-error");
                        cx.notify();
                        continue;
                    }
                    let control = TransferControl::default();
                    self.enqueue(
                        Job::Attachment {
                            bytes: image.bytes.into(),
                            name: "screenshot.png".into(),
                            control,
                        },
                        cx,
                    );
                }
                ClipboardEntry::String(_) => {}
            }
        }
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
        let Some(entry) = self
            .selected
            .and_then(|index| self.entries.get(index))
            .cloned()
        else {
            return;
        };
        if entry.is_directory {
            self.message = tr("file-select-download");
            cx.notify();
            return;
        }
        let directory = opsssh_platform::home_dir().unwrap_or_default();
        let picker = cx.prompt_for_new_path(&directory, Some(&entry.name));
        cx.spawn_in(window, async move |this, cx| match picker.await {
            Ok(Ok(Some(local))) => {
                let _ = this.update_in(cx, |this, _, cx| {
                    let control = TransferControl::default();
                    this.enqueue(
                        Job::Download {
                            remote: entry.path,
                            local,
                            control,
                        },
                        cx,
                    );
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
}
impl FilePane {
    pub fn dismiss_transfer_notice(&mut self, id: u64, cx: &mut Context<Self>) {
        if let Some(task) = self.transfers.iter_mut().find(|task| task.id == id) {
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
                                .child(if let Some(error) = &task.failure {
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
        let (background, foreground, muted, border, hover, selected, accent) = (
            theme.background,
            theme.foreground,
            theme.muted_foreground,
            theme.border,
            theme.muted,
            theme.accent,
            theme.primary,
        );
        let grid = self.full_page && self.grid;
        let size = entry
            .size
            .filter(|_| !entry.is_directory)
            .map(format_bytes)
            .unwrap_or_else(|| "—".into());
        let modified = entry
            .modified
            .map(format_modified)
            .unwrap_or_else(|| "—".into());
        let icon = Icon::new(entry_icon(entry)).text_color(if entry.is_directory {
            accent
        } else {
            muted
        });
        let mut cell = div()
            .id(("file-entry", index))
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .bg(if self.selected == Some(index) {
                selected
            } else {
                background
            })
            .text_color(foreground)
            .hover(move |style| style.bg(hover));
        if grid {
            cell = cell
                .flex_col()
                .p_3()
                .gap_2()
                .rounded_md()
                .border_1()
                .border_color(if self.selected == Some(index) {
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
                .child(
                    div()
                        .text_sm()
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .truncate()
                        .child(entry.name.clone()),
                )
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
                        .child(div().text_sm().truncate().child(entry.name.clone()))
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
        cell.on_click(
            cx.listener(move |this, event: &gpui::ClickEvent, window, cx| {
                this.list_focus.focus(window, cx);
                this.selected = Some(index);
                if event.click_count() == 2
                    && let Some(entry) = this.entries.get(index)
                {
                    if entry.is_directory {
                        this.enqueue(Job::List(entry.path.clone()), cx);
                    } else {
                        this.pick_download(window, cx);
                    }
                }
                cx.notify();
            }),
        )
        .into_any_element()
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
            .child(
                Button::new("up")
                    .small()
                    .ghost()
                    .label(tr("file-up"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        let parent = this
                            .directory
                            .trim_end_matches('/')
                            .rsplit_once('/')
                            .map(|(path, _)| if path.is_empty() { "/" } else { path })
                            .unwrap_or("/")
                            .to_string();
                        this.enqueue(Job::List(parent), cx);
                    })),
            )
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
                Button::new("upload")
                    .small()
                    .primary()
                    .label(tr("file-upload"))
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
                    .disabled(self.selected.is_none())
                    .on_click(cx.listener(|this, _, window, cx| this.pick_download(window, cx))),
            )
            .child(
                Button::new("delete")
                    .small()
                    .ghost()
                    .label(tr("file-delete"))
                    .disabled(self.selected.is_none())
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(entry) = this.selected.and_then(|index| this.entries.get(index))
                        {
                            if entry.is_directory {
                                this.message = tr("file-directory-delete-unavailable");
                            } else {
                                this.pending_delete = Some(entry.path.clone());
                            }
                        }
                        cx.notify();
                    })),
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
            .px_4()
            .py_3()
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
            .child(div().text_xs().text_color(muted).child(format!(
                "{} {}",
                self.visible.len(),
                tr("file-items")
            )))
            .into_any_element()
    }
    fn path_controls(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        if self.full_page {
            return self.full_page_path_controls(cx);
        }
        div()
            .p_3()
            .flex()
            .flex_col()
            .gap_2()
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
                        div()
                            .flex()
                            .gap_1()
                            .child(
                                Button::new("up")
                                    .small()
                                    .ghost()
                                    .label(tr("file-up"))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        let path = &this.directory;
                                        let parent = path
                                            .trim_end_matches('/')
                                            .rsplit_once('/')
                                            .map(|(p, _)| if p.is_empty() { "/" } else { p })
                                            .unwrap_or("/")
                                            .to_string();
                                        this.enqueue(Job::List(parent), cx);
                                    })),
                            )
                            .child(
                                Button::new("refresh")
                                    .small()
                                    .ghost()
                                    .label(tr("file-refresh"))
                                    .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
                            ),
                    )
                    .child(
                        Button::new("upload")
                            .small()
                            .primary()
                            .label(tr("file-upload"))
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
            bar = bar
                .child(
                    Button::new(("file-breadcrumb", index))
                        .small()
                        .ghost()
                        .label(label)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.enqueue(Job::List(path.clone()), cx)
                        })),
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
        if self.visible.is_empty() {
            (div()
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
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
                })))
            .into_any_element()
        } else {
            let rows = self.visible.len().div_ceil(self.columns);
            (uniform_list(
                "file-entries",
                rows,
                cx.processor(|this, range: std::ops::Range<usize>, _, cx| {
                    range
                        .map(|row| {
                            let start = row * this.columns;
                            let end = (start + this.columns).min(this.visible.len());
                            let indices = this.visible[start..end].to_vec();
                            let mut line = div().w_full().flex().gap_3().h(px(
                                if this.full_page && this.grid {
                                    164.
                                } else {
                                    56.
                                },
                            ));
                            if this.full_page && this.grid {
                                line = line.px_3().py_2();
                            }
                            for index in indices {
                                line = line.child(this.entry_element(index, cx));
                            }
                            if this.full_page && this.grid {
                                for _ in end - start..this.columns {
                                    line = line.child(div().flex_1().min_w_0());
                                }
                            }
                            line
                        })
                        .collect()
                }),
            )
            .track_scroll(&self.list_scroll)
            .flex_1()
            .min_h_0())
            .into_any_element()
        }
    }
    fn file_actions(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        if self.full_page {
            return self.full_page_file_actions(cx);
        }
        let theme = cx.theme();
        let border = theme.border;
        div()
            .p_3()
            .flex()
            .flex_col()
            .gap_2()
            .border_t_1()
            .border_color(border)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        Button::new("download")
                            .small()
                            .label(tr("file-download"))
                            .disabled(self.selected.is_none())
                            .on_click(
                                cx.listener(|this, _, window, cx| this.pick_download(window, cx)),
                            ),
                    )
                    .child(
                        Button::new("delete")
                            .small()
                            .ghost()
                            .label(tr("file-delete"))
                            .disabled(self.selected.is_none())
                            .on_click(cx.listener(|this, _, _, cx| {
                                if let Some(entry) =
                                    this.selected.and_then(|index| this.entries.get(index))
                                {
                                    if entry.is_directory {
                                        this.message = tr("file-directory-delete-unavailable");
                                    } else {
                                        this.pending_delete = Some(entry.path.clone());
                                    }
                                }
                                cx.notify();
                            })),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        Input::new(&self.folder)
                            .small()
                            .aria_label(tr("file-folder-placeholder")),
                    )
                    .child(
                        Button::new("mkdir")
                            .small()
                            .label(tr("file-create-folder"))
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
    fn delete_confirmation(&self, path: String, cx: &mut Context<Self>) -> gpui::AnyElement {
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
            .child(tr("file-delete-confirm"))
            .child(div().text_xs().child(path))
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
                                if let Some(path) = this.pending_delete.take() {
                                    this.enqueue(Job::Delete(path), cx);
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
            .max_h(px(220.))
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_2()
            .p_3()
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
        let background = theme.sidebar;
        let foreground = theme.sidebar_foreground;
        let border = theme.border;
        let muted = theme.muted_foreground;
        let id = task.id;
        let active = task.state.active();
        let mut card = div()
            .p_2()
            .rounded_md()
            .bg(background)
            .border_1()
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
        if let Some(failure) = &task.failure {
            card = card.child(div().text_xs().text_color(muted).child(failure.clone()));
        }
        card.into_any_element()
    }
    fn status_bar(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        div()
            .px_3()
            .py_2()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .border_t_1()
            .border_color(cx.theme().border)
            .child(self.message.clone())
            .into_any_element()
    }
}
impl Render for FilePane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
            ((f32::from(window.viewport_size().width) - 280.) / 210.)
                .floor()
                .max(1.) as usize
        } else {
            1
        };
        if self
            .selected
            .is_some_and(|index| !self.visible.contains(&index))
        {
            self.selected = None;
        }
        let theme = cx.theme();
        let (background, foreground) = (theme.sidebar, theme.sidebar_foreground);
        let mut pane = div()
            .id("file-pane")
            .track_focus(&self.list_focus)
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                if !this.list_focus.is_focused(window) || this.visible.is_empty() {
                    return;
                }
                let position = this
                    .selected
                    .and_then(|index| this.visible.iter().position(|&i| i == index));
                let step = this.columns;
                match event.keystroke.key.as_str() {
                    "down" => {
                        this.selected = Some(
                            this.visible
                                [position.map_or(0, |i| (i + step).min(this.visible.len() - 1))],
                        )
                    }
                    "up" => {
                        this.selected =
                            Some(this.visible[position.map_or(0, |i| i.saturating_sub(step))])
                    }
                    "right" => {
                        this.selected = Some(
                            this.visible
                                [position.map_or(0, |i| (i + 1).min(this.visible.len() - 1))],
                        )
                    }
                    "left" => {
                        this.selected =
                            Some(this.visible[position.map_or(0, |i| i.saturating_sub(1))])
                    }
                    "home" => this.selected = Some(this.visible[0]),
                    "end" => this.selected = this.visible.last().copied(),
                    "enter" => {
                        if let Some(entry) = this.selected.and_then(|i| this.entries.get(i)) {
                            if entry.is_directory {
                                this.enqueue(Job::List(entry.path.clone()), cx);
                            } else {
                                this.pick_download(window, cx);
                            }
                        }
                    }
                    _ => return,
                }
                if let Some(index) = this
                    .selected
                    .and_then(|index| this.visible.iter().position(|&i| i == index))
                {
                    this.list_scroll
                        .scroll_to_item(index / this.columns, gpui::ScrollStrategy::Nearest);
                }
                cx.stop_propagation();
                cx.notify();
            }))
            .flex()
            .flex_col()
            .w_full()
            .h_full()
            .min_h_0()
            .min_w_0()
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
            .child(self.entries_view(cx))
            .child(self.file_actions(cx));
        if let Some(path) = self.pending_delete.clone() {
            pane = pane.child(self.delete_confirmation(path, cx));
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
        let result = execute(&client, &home, job, &updates, id).await;
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
                client.upload_tree(&path, &remote, &control).await?;
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
            control,
        } => {
            control.set_total(client.identity(&remote).await?.size);
            let bytes = client
                .download(&remote, &local, WriteMode::CreateNew, &control)
                .await?;
            updates
                .send(Update::Message(format!(
                    "{}: {bytes} B",
                    tr("file-download-complete")
                )))
                .await?;
        }
        Job::Attachment {
            bytes,
            name,
            control,
        } => {
            control.set_total(bytes.len() as u64);
            let directory = client.private_staging(home).await?;
            let path = opsssh_drop::join_remote(&directory, &name)?;
            client.upload_bytes(&bytes, &path, &control).await?;
            updates
                .send(Update::Insert(
                    id,
                    format!("{} ", opsssh_drop::quote(&path, opsssh_drop::Shell::Posix)?),
                ))
                .await?;
            updates
                .send(Update::Message(format!(
                    "{}: {path}",
                    tr("file-attachment-complete")
                )))
                .await?;
        }
        Job::Mkdir(path) => {
            client.create_directory(&path).await?;
            updates
                .send(Update::Message(tr("file-folder-created")))
                .await?;
            refresh_parent(client, &path, updates).await;
        }
        Job::Delete(path) => {
            client.remove_file_confirmed(&path).await?;
            updates.send(Update::Message(tr("file-deleted"))).await?;
            refresh_parent(client, &path, updates).await;
        }
    }
    Ok(())
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
