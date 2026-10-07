//! SFTP pane: all protocol, disk and transfer work runs on one background runtime.
use async_channel::{Receiver, Sender};
use gpui::{
    ClipboardEntry, ClipboardItem, Context, Entity, EventEmitter, PathPromptOptions, Render,
    Window, div, prelude::*, px, rgb, uniform_list,
};
use gpui_component::{
    button::{Button, ButtonVariants},
    input::{Input, InputEvent, InputState},
};
use opsssh_sftp::{Entry, SftpClient, TransferControl, WriteMode};
use opsssh_ssh_core::SshCommand;
use std::{path::PathBuf, time::Duration};

pub enum FilePaneEvent {
    InsertText(String),
}
enum Job {
    List(String),
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
        bytes: Vec<u8>,
        name: String,
        control: TransferControl,
    },
    Mkdir(String),
    Delete(String),
}
enum Update {
    Listed(String, Vec<Entry>),
    Message(String),
    Insert(String),
    Finished,
}
pub struct FilePane {
    jobs: Sender<Job>,
    path: Entity<InputState>,
    folder: Entity<InputState>,
    entries: Vec<Entry>,
    selected: Option<usize>,
    message: String,
    pending_delete: Option<String>,
    controls: Vec<TransferControl>,
    pending: usize,
}
impl EventEmitter<FilePaneEvent> for FilePane {}
impl FilePane {
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
                .placeholder("Remote folder")
        });
        let folder = cx.new(|cx| InputState::new(window, cx).placeholder("New folder name"));
        cx.subscribe_in(&path, window, |this, _, event, _, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.refresh(cx);
            }
        })
        .detach();
        let thread = std::thread::Builder::new()
            .name("opsssh-files".into())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build();
                match runtime {
                    Ok(runtime) => {
                        if let Err(error) = runtime.block_on(worker(
                            commands,
                            fingerprint,
                            receiver,
                            updates.clone(),
                        )) {
                            let _ = updates.send_blocking(Update::Message(error.to_string()));
                        }
                    }
                    Err(error) => {
                        let _ = updates.send_blocking(Update::Message(error.to_string()));
                    }
                }
            });
        cx.spawn_in(window, async move |this, cx| {
            while let Ok(update) = receive.recv().await {
                if this
                    .update_in(cx, |this, window, cx| {
                        match update {
                            Update::Listed(path, entries) => {
                                this.path
                                    .update(cx, |state, cx| state.set_value(path, window, cx));
                                this.entries = entries;
                                this.selected = None;
                                this.message = format!("{} entries", this.entries.len());
                            }
                            Update::Message(message) => this.message = message,
                            Update::Insert(text) => cx.emit(FilePaneEvent::InsertText(text)),
                            Update::Finished => {
                                this.pending = this.pending.saturating_sub(1);
                                if this.pending == 0 {
                                    this.controls.clear();
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
        let mut pane = Self {
            jobs,
            path,
            folder,
            entries: vec![],
            selected: None,
            message: "Opening SFTP…".into(),
            pending_delete: None,
            controls: vec![],
            pending: 0,
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
            Err(error) => pane.message = format!("Cannot start file worker: {error}"),
        };
        pane
    }
    fn enqueue(&mut self, job: Job, cx: &mut Context<Self>) {
        match self.jobs.try_send(job) {
            Ok(()) => {
                self.pending += 1;
                self.message = format!("{} operation(s) queued", self.pending);
            }
            Err(_) => self.message = "File connection closed or queue full".into(),
        };
        cx.notify();
    }
    fn refresh(&mut self, cx: &mut Context<Self>) {
        self.enqueue(Job::List(self.path.read(cx).value().to_string()), cx);
    }
    pub fn follow_directory(&mut self, path: String, cx: &mut Context<Self>) {
        if self.path.read(cx).value().as_ref() != path {
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
        self.controls.push(control.clone());
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
                        self.message =
                            "Clipboard upload currently accepts PNG images up to 64 MiB".into();
                        cx.notify();
                        continue;
                    }
                    let control = TransferControl::default();
                    self.controls.push(control.clone());
                    self.enqueue(
                        Job::Attachment {
                            bytes: image.bytes,
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
                    let target = this.path.read(cx).value().to_string();
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
            self.message = "Select a file to download".into();
            cx.notify();
            return;
        }
        let directory = opsssh_platform::home_dir().unwrap_or_default();
        let picker = cx.prompt_for_new_path(&directory, Some(&entry.name));
        cx.spawn_in(window, async move |this, cx| match picker.await {
            Ok(Ok(Some(local))) => {
                let _ = this.update_in(cx, |this, _, cx| {
                    let control = TransferControl::default();
                    this.controls.push(control.clone());
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
impl Drop for FilePane {
    fn drop(&mut self) {
        for control in &self.controls {
            control.cancel();
        }
        self.jobs.close();
    }
}
impl Render for FilePane {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut pane = div()
            .flex()
            .flex_col()
            .w(px(360.))
            .h_full()
            .min_h_0()
            .bg(rgb(0x111b26))
            .p_3()
            .gap_2()
            .text_color(rgb(0xd6e2ec))
            .child(div().font_weight(gpui::FontWeight::BOLD).child("Files"))
            .child(Input::new(&self.path).aria_label("Remote folder"))
            .child(
                div()
                    .flex()
                    .gap_1()
                    .child(
                        Button::new("refresh")
                            .label("Refresh")
                            .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
                    )
                    .child(Button::new("up").label("Up").on_click(cx.listener(
                        |this, _, _, cx| {
                            let path = this.path.read(cx).value();
                            let parent = path
                                .trim_end_matches('/')
                                .rsplit_once('/')
                                .map(|(p, _)| if p.is_empty() { "/" } else { p })
                                .unwrap_or("/")
                                .to_string();
                            this.enqueue(Job::List(parent), cx);
                        },
                    ))),
            )
            .child(
                div()
                    .flex()
                    .gap_1()
                    .child(
                        Button::new("upload").label("Upload").on_click(
                            cx.listener(|this, _, window, cx| this.pick_upload(window, cx)),
                        ),
                    )
                    .child(Button::new("download").label("Download").on_click(
                        cx.listener(|this, _, window, cx| this.pick_download(window, cx)),
                    ))
                    .child(Button::new("cancel").label("Cancel").on_click(cx.listener(
                        |this, _, _, cx| {
                            for control in &this.controls {
                                control.cancel();
                            }
                            this.message =
                                "Cancellation requested; partial files are retained".into();
                            cx.notify();
                        },
                    ))),
            )
            .child(
                uniform_list(
                    "file-entries",
                    self.entries.len(),
                    cx.processor(|this, range: std::ops::Range<usize>, _, cx| {
                        range
                            .map(|index| {
                                let entry = &this.entries[index];
                                let label = format!(
                                    "{} {}",
                                    if entry.is_directory {
                                        "▸"
                                    } else if entry.is_symlink {
                                        "↗"
                                    } else {
                                        "·"
                                    },
                                    entry.name
                                );
                                div()
                                    .id(index)
                                    .h(px(30.))
                                    .px_2()
                                    .flex()
                                    .items_center()
                                    .overflow_hidden()
                                    .bg(if this.selected == Some(index) {
                                        rgb(0x294a50)
                                    } else {
                                        rgb(0x111b26)
                                    })
                                    .child(label)
                                    .on_click(cx.listener(
                                        move |this, event: &gpui::ClickEvent, _, cx| {
                                            this.selected = Some(index);
                                            if event.click_count() == 2
                                                && let Some(entry) = this.entries.get(index)
                                                && entry.is_directory
                                            {
                                                this.enqueue(Job::List(entry.path.clone()), cx);
                                            }
                                            cx.notify();
                                        },
                                    ))
                            })
                            .collect()
                    }),
                )
                .flex_1()
                .min_h_0(),
            )
            .child(
                div()
                    .flex()
                    .gap_1()
                    .child(Input::new(&self.folder).aria_label("New folder name"))
                    .child(Button::new("mkdir").label("Create").on_click(cx.listener(
                        |this, _, _, cx| match opsssh_drop::join_remote(
                            &this.path.read(cx).value(),
                            &this.folder.read(cx).value(),
                        ) {
                            Ok(path) => this.enqueue(Job::Mkdir(path), cx),
                            Err(error) => {
                                this.message = error.to_string();
                                cx.notify();
                            }
                        },
                    ))),
            )
            .child(
                Button::new("delete")
                    .label("Delete selected file…")
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(entry) = this.selected.and_then(|index| this.entries.get(index))
                        {
                            if entry.is_directory {
                                this.message = "Directory deletion is not available".into();
                            } else {
                                this.pending_delete = Some(entry.path.clone());
                            }
                        }
                        cx.notify();
                    })),
            );
        if let Some(path) = self.pending_delete.clone() {
            pane = pane.child(
                div()
                    .p_2()
                    .bg(rgb(0x502d2d))
                    .child(format!("Delete {path}? This cannot be undone."))
                    .child(
                        Button::new("confirm-delete")
                            .label("Delete permanently")
                            .danger()
                            .on_click(cx.listener(|this, _, _, cx| {
                                if let Some(path) = this.pending_delete.take() {
                                    this.enqueue(Job::Delete(path), cx);
                                }
                            })),
                    )
                    .child(
                        Button::new("keep-file")
                            .label("Keep file")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.pending_delete = None;
                                cx.notify();
                            })),
                    ),
            );
        }
        pane.child(div().text_xs().child(self.message.clone()))
    }
}

async fn worker(
    commands: Sender<SshCommand>,
    fingerprint: String,
    jobs: Receiver<Job>,
    updates: Sender<Update>,
) -> opsssh_sftp::Result<()> {
    let (reply, receive) = async_channel::bounded(1);
    commands.send(SshCommand::OpenSftp { reply }).await?;
    let stream = tokio::time::timeout(Duration::from_secs(20), receive.recv())
        .await??
        .map_err(std::io::Error::other)?;
    let client = SftpClient::connect(stream, fingerprint).await?;
    let home = client.canonicalize(".").await?;
    while let Ok(job) = jobs.recv().await {
        if updates.is_closed() || jobs.is_closed() {
            break;
        }
        let result = execute(&client, &home, job, &updates).await;
        if let Err(error) = result {
            let _ = updates.send(Update::Message(error.to_string())).await;
        }
        let _ = updates.send(Update::Finished).await;
    }
    Ok(())
}
async fn execute(
    client: &SftpClient,
    home: &str,
    job: Job,
    updates: &Sender<Update>,
) -> opsssh_sftp::Result<()> {
    match job {
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
            let mut inserted = vec![];
            for path in paths {
                let name = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .ok_or("Invalid filename")?;
                let remote = opsssh_drop::join_remote(&directory, name)?;
                client.upload_tree(&path, &remote, &control).await?;
                inserted.push(opsssh_drop::quote(&remote, opsssh_drop::Shell::Posix)?);
            }
            if insert {
                updates
                    .send(Update::Insert(format!("{} ", inserted.join(" "))))
                    .await?;
            }
            updates
                .send(Update::Message(format!("Upload complete: {directory}")))
                .await?;
        }
        Job::Download {
            remote,
            local,
            control,
        } => {
            let bytes = client
                .download(&remote, &local, WriteMode::CreateNew, &control)
                .await?;
            updates
                .send(Update::Message(format!("Downloaded {bytes} bytes")))
                .await?;
        }
        Job::Attachment {
            bytes,
            name,
            control,
        } => {
            let directory = client.private_staging(home).await?;
            let path = opsssh_drop::join_remote(&directory, &name)?;
            client.upload_bytes(&bytes, &path, &control).await?;
            updates
                .send(Update::Insert(format!(
                    "{} ",
                    opsssh_drop::quote(&path, opsssh_drop::Shell::Posix)?
                )))
                .await?;
            updates
                .send(Update::Message(format!("Uploaded attachment: {path}")))
                .await?;
        }
        Job::Mkdir(path) => {
            client.create_directory(&path).await?;
            updates
                .send(Update::Message(
                    "Folder created; Refresh to update listing".into(),
                ))
                .await?;
        }
        Job::Delete(path) => {
            client.remove_file_confirmed(&path).await?;
            updates
                .send(Update::Message(
                    "File deleted; Refresh to update listing".into(),
                ))
                .await?;
        }
    }
    Ok(())
}
