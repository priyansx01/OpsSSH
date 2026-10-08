//! File menus and native permission editor; all remote work goes through jobs.
use super::*;
use gpui_component::{checkbox::Checkbox, switch::Switch};
use opsssh_sftp::{OperationReport, parse_mode, symbolic_mode};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum FileAction {
    Open,
    Terminal,
    NewTerminal,
    Download,
    Rename,
    Copy,
    Move,
    CopyPath,
    Permissions,
    Delete,
}
impl FileAction {
    fn label(self) -> &'static str {
        match self {
            Self::Open => "Open",
            Self::Terminal => "Open inside terminal",
            Self::NewTerminal => "Open in Terminal tab",
            Self::Download => "Download",
            Self::Rename => "Rename",
            Self::Copy => "Copy",
            Self::Move => "Move",
            Self::CopyPath => "Copy path",
            Self::Permissions => "Permissions",
            Self::Delete => "Delete",
        }
    }
    fn all() -> [Self; 10] {
        [
            Self::Open,
            Self::Terminal,
            Self::NewTerminal,
            Self::Download,
            Self::Rename,
            Self::Copy,
            Self::Move,
            Self::CopyPath,
            Self::Permissions,
            Self::Delete,
        ]
    }
}
pub(super) struct PermissionEditor {
    entry: Entry,
    serial: u64,
    attrs: Option<RemoteAttributes>,
    mode: Entity<InputState>,
    owner: Entity<InputState>,
    group: Entity<InputState>,
    original_owner: String,
    original_group: String,
    advanced: bool,
    recursive: bool,
    busy: bool,
    message: String,
    control: Option<TransferControl>,
}
pub(super) enum FileEditor {
    Actions(Entry),
    Transfer {
        entry: Entry,
        action: FileAction,
        destination: Entity<InputState>,
        name: Entity<InputState>,
        error: String,
    },
    Permissions(Box<PermissionEditor>),
}
impl FilePane {
    #[cfg(feature = "capture")]
    pub fn preview_permissions(
        &mut self,
        advanced: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let entry = self.entries[0].clone();
        self.file_action(entry, FileAction::Permissions, window, cx);
        let serial = self.editor_serial;
        self.receive_attributes(
            serial,
            Ok((
                RemoteAttributes {
                    mode: Some(0o755),
                    uid: Some(0),
                    gid: Some(0),
                    is_directory: true,
                    is_regular: false,
                    is_symlink: false,
                    size: None,
                    modified: None,
                },
                "root".into(),
                "root".into(),
            )),
            window,
            cx,
        );
        if let Some(FileEditor::Permissions(p)) = &mut self.editor {
            p.advanced = advanced;
        }
    }
    pub(super) fn action_menu(
        &self,
        entry: Entry,
        mut menu: PopupMenu,
        cx: &Context<Self>,
    ) -> PopupMenu {
        menu = menu.label(entry.name.clone()).separator();
        for action in FileAction::all() {
            if matches!(
                action,
                FileAction::Download
                    | FileAction::Rename
                    | FileAction::Permissions
                    | FileAction::Delete
            ) {
                menu = menu.separator();
            }
            let weak = cx.entity().downgrade();
            let target = entry.clone();
            let enabled = self.available || action == FileAction::CopyPath;
            let mut item = if action == FileAction::Delete {
                PopupMenuItem::element(|_, cx| div().text_color(cx.theme().danger).child("Delete"))
            } else {
                PopupMenuItem::new(action.label())
            };
            item = item.disabled(!enabled).on_click(move |_, window, cx| {
                let _ = weak.update(cx, |this, cx| {
                    this.file_action(target.clone(), action, window, cx)
                });
            });
            menu = menu.item(item);
        }
        menu
    }
    pub(super) fn file_action(
        &mut self,
        entry: Entry,
        action: FileAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.available && action != FileAction::CopyPath {
            return;
        }
        match action {
            FileAction::Open => {
                if entry.is_directory {
                    self.enqueue(Job::List(entry.path), cx);
                } else {
                    self.download_entry(entry, window, cx);
                }
            }
            FileAction::Download => self.download_entry(entry, window, cx),
            FileAction::CopyPath => {
                cx.write_to_clipboard(ClipboardItem::new_string(entry.path));
                self.message = "Remote path copied".into();
            }
            FileAction::Terminal | FileAction::NewTerminal => {
                let directory = if entry.is_directory {
                    entry.path
                } else {
                    parent(&entry.path)
                };
                cx.emit(FilePaneEvent::OpenTerminal {
                    directory,
                    new_tab: action == FileAction::NewTerminal,
                });
            }
            FileAction::Delete => {
                self.pending_delete = Some(entry.path);
            }
            FileAction::Permissions => {
                self.editor_serial = self.editor_serial.wrapping_add(1);
                let serial = self.editor_serial;
                let mode = cx.new(|cx| InputState::new(window, cx).placeholder("755"));
                let owner =
                    cx.new(|cx| InputState::new(window, cx).placeholder("Owner name or UID"));
                let group =
                    cx.new(|cx| InputState::new(window, cx).placeholder("Group name or GID"));
                for input in [&mode, &owner, &group] {
                    cx.subscribe(input, |_, _, _: &InputEvent, cx| cx.notify())
                        .detach();
                }
                self.editor = Some(FileEditor::Permissions(Box::new(PermissionEditor {
                    entry: entry.clone(),
                    serial,
                    attrs: None,
                    mode,
                    owner,
                    group,
                    original_owner: String::new(),
                    original_group: String::new(),
                    advanced: false,
                    recursive: false,
                    busy: true,
                    message: "Loading permissions…".into(),
                    control: None,
                })));
                self.enqueue(
                    Job::Attributes {
                        path: entry.path,
                        serial,
                    },
                    cx,
                );
                self.open_file_dialog(window, cx);
            }
            FileAction::Rename | FileAction::Copy | FileAction::Move => {
                let destination =
                    cx.new(|cx| InputState::new(window, cx).default_value(parent(&entry.path)));
                let name =
                    cx.new(|cx| InputState::new(window, cx).default_value(entry.name.clone()));
                self.editor = Some(FileEditor::Transfer {
                    entry,
                    action,
                    destination,
                    name,
                    error: String::new(),
                });
                self.open_file_dialog(window, cx);
            }
        }
        cx.notify();
    }
    pub(super) fn download_entry(
        &mut self,
        entry: Entry,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if entry.is_symlink {
            self.message = "Open the actual target to download a symbolic link".into();
            cx.notify();
            return;
        }
        let generation = self.worker_generation;
        let home = opsssh_platform::home_dir().unwrap_or_default();
        let picker = cx.prompt_for_new_path(&home, Some(&entry.name));
        cx.spawn_in(window, async move |this, cx| match picker.await {
            Ok(Ok(Some(local))) => {
                let _ = this.update(cx, |this, cx| {
                    if this.available && this.worker_generation == generation {
                        this.enqueue(
                            Job::Download {
                                remote: entry.path,
                                local,
                                control: TransferControl::default(),
                            },
                            cx,
                        );
                    }
                });
            }
            Ok(Err(e)) => {
                let _ = this.update(cx, |this, cx| {
                    this.message = e.to_string();
                    cx.notify();
                });
            }
            _ => {}
        })
        .detach();
    }
    pub(super) fn keyboard_actions(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(entry) = self.selected.and_then(|i| self.entries.get(i)).cloned() {
            self.editor = Some(FileEditor::Actions(entry));
            self.open_file_dialog(window, cx);
            cx.notify();
        }
    }
    fn open_file_dialog(&self, window: &mut Window, cx: &mut Context<Self>) {
        let weak = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, window, cx| {
            let Some(pane) = weak.upgrade() else {
                return dialog;
            };
            let state = pane.read(cx);
            let Some(editor) = state.editor.as_ref() else {
                return dialog;
            };
            let title = match editor {
                FileEditor::Permissions(_) => "Permissions",
                FileEditor::Transfer { action, .. } => action.label(),
                FileEditor::Actions(_) => "File actions",
            };
            let body = state.editor_body(window, cx, weak.clone());
            let footer = state.editor_footer(cx, weak.clone());
            let closed = weak.clone();
            dialog
                .title(title)
                .width(px(600.).min(window.viewport_size().width - px(48.)))
                .overlay_closable(false)
                .child(body)
                .footer(footer)
                .on_close(move |_, _, cx| {
                    let _ = closed.update(cx, |this, cx| {
                        this.close_editor(cx);
                    });
                })
        });
    }
    fn close_editor(&mut self, cx: &mut Context<Self>) {
        if let Some(FileEditor::Permissions(p)) = &self.editor
            && let Some(control) = &p.control
        {
            control.cancel();
        }
        self.editor = None;
        self.editor_serial = self.editor_serial.wrapping_add(1);
        cx.notify();
    }
    fn editor_body(
        &self,
        window: &Window,
        cx: &gpui::App,
        weak: gpui::WeakEntity<Self>,
    ) -> gpui::AnyElement {
        let mut body = div()
            .id("file-editor-body")
            .flex()
            .flex_col()
            .gap_3()
            .max_h((window.viewport_size().height - px(230.)).max(px(140.)))
            .overflow_y_scroll();
        match self.editor.as_ref().expect("editor") {
            FileEditor::Actions(entry) => {
                body = body.child(
                    div()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(entry.name.clone()),
                );
                for action in FileAction::all() {
                    let target = entry.clone();
                    let weak = weak.clone();
                    body = body.child(
                        Button::new(action.label())
                            .ghost()
                            .label(action.label())
                            .disabled(!self.available && action != FileAction::CopyPath)
                            .on_click(move |_, window, cx| {
                                window.close_dialog(cx);
                                let _ = weak.update(cx, |this, cx| {
                                    this.editor = None;
                                    this.file_action(target.clone(), action, window, cx);
                                });
                            }),
                    );
                }
            }
            FileEditor::Transfer {
                entry,
                action,
                destination,
                name,
                error,
            } => {
                body = body.child(div().text_sm().child(entry.path.clone()));
                if *action != FileAction::Rename {
                    body = body
                        .child("Destination folder on this server")
                        .child(Input::new(destination));
                }
                body = body.child("Name").child(Input::new(name)).child(div().text_sm().text_color(cx.theme().muted_foreground)
                    .child("Existing files require replacement confirmation. Folders are never merged."));
                if !error.is_empty() {
                    body = body.child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().danger)
                            .child(error.clone()),
                    );
                }
            }
            FileEditor::Permissions(p) => {
                let attrs = p.attrs.as_ref();
                let mode = parse_mode(&p.mode.read(cx).value()).ok();
                body = body
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child("Choose who can read, write, or run it."),
                    )
                    .child(
                        div()
                            .p_3()
                            .rounded_lg()
                            .border_1()
                            .border_color(cx.theme().border)
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(
                                Icon::new(if p.entry.is_directory {
                                    IconName::Folder
                                } else {
                                    IconName::File
                                })
                                .text_color(cx.theme().primary),
                            )
                            .child(div().flex_1().min_w_0().child(p.entry.name.clone()))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(if p.entry.is_directory {
                                        "FOLDER"
                                    } else {
                                        "FILE"
                                    }),
                            ),
                    )
                    .child(
                        div()
                            .p_3()
                            .rounded_lg()
                            .bg(cx.theme().muted)
                            .flex()
                            .justify_between()
                            .gap_3()
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .child(div().text_xs().child("OWNED BY"))
                                    .child(format!("{}:{}", p.original_owner, p.original_group)),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .child(div().text_xs().child("PRESET"))
                                    .child(
                                        presets(p.entry.is_directory)
                                            .into_iter()
                                            .find(|(v, _, _)| Some(*v) == mode)
                                            .map(|(_, title, _)| title)
                                            .unwrap_or("Custom"),
                                    ),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .child(div().text_xs().child("MODE"))
                                    .child(
                                        mode.map(|m| format!("{m:03o}  {}", symbolic_mode(m)))
                                            .unwrap_or_else(|| "Unavailable / invalid".into()),
                                    ),
                            ),
                    );
                let quick = weak.clone();
                let advanced = weak.clone();
                body = body.child(
                    div()
                        .flex()
                        .gap_2()
                        .child(
                            Button::new("permission-quick")
                                .label("Quick actions")
                                .selected(!p.advanced)
                                .on_click(move |_, _, cx| {
                                    let _ = quick.update(cx, |this, cx| {
                                        if let Some(FileEditor::Permissions(p)) = &mut this.editor {
                                            p.advanced = false;
                                        }
                                        cx.notify();
                                    });
                                }),
                        )
                        .child(
                            Button::new("permission-advanced")
                                .label("Advanced")
                                .selected(p.advanced)
                                .on_click(move |_, _, cx| {
                                    let _ = advanced.update(cx, |this, cx| {
                                        if let Some(FileEditor::Permissions(p)) = &mut this.editor {
                                            p.advanced = true;
                                        }
                                        cx.notify();
                                    });
                                }),
                        ),
                );
                let disabled = p.busy
                    || !self.available
                    || attrs.is_none_or(|a| a.mode.is_none() || a.is_symlink);
                if !p.advanced {
                    for (value, label, description) in presets(p.entry.is_directory) {
                        let weak = weak.clone();
                        let selected = mode == Some(value);
                        body = body.child(
                            Button::new(label)
                                .w_full()
                                .h(px(72.))
                                .selected(selected)
                                .disabled(disabled)
                                .accessibility_label(format!("{label}, {value:03o}, {description}"))
                                .child(
                                    div()
                                        .w_full()
                                        .flex()
                                        .items_center()
                                        .justify_between()
                                        .gap_3()
                                        .child(
                                            div()
                                                .flex_1()
                                                .min_w_0()
                                                .flex()
                                                .flex_col()
                                                .gap_2()
                                                .child(
                                                    div()
                                                        .text_sm()
                                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                                        .child(label),
                                                )
                                                .child(
                                                    div()
                                                        .text_xs()
                                                        .text_color(cx.theme().muted_foreground)
                                                        .child(description),
                                                ),
                                        )
                                        .child(
                                            div()
                                                .text_sm()
                                                .text_color(if selected {
                                                    cx.theme().primary
                                                } else {
                                                    cx.theme().muted_foreground
                                                })
                                                .child(format!("{value:03o}")),
                                        ),
                                )
                                .on_click(move |_, window, cx| {
                                    let _ = weak.update(cx, |this, cx| {
                                        if let Some(FileEditor::Permissions(p)) = &this.editor {
                                            p.mode.update(cx, |i, cx| {
                                                i.set_value(format!("{value:03o}"), window, cx)
                                            });
                                        }
                                        cx.notify();
                                    });
                                }),
                        );
                    }
                } else {
                    body = body
                        .child("Octal mode")
                        .child(Input::new(&p.mode).disabled(disabled));
                    for (row, shift) in [("Owner", 6), ("Group", 3), ("Others", 0)] {
                        let mut permissions = div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(div().w(px(70.)).child(row));
                        for (label, bit) in [("Read", 4), ("Write", 2), ("Execute", 1)] {
                            let bit = bit << shift;
                            let weak = weak.clone();
                            permissions = permissions.child(
                                Checkbox::new(format!("{row}-{label}"))
                                    .label(label)
                                    .checked(mode.is_some_and(|m| m & bit != 0))
                                    .disabled(disabled)
                                    .on_click(move |checked, window, cx| {
                                        let _ = weak.update(cx, |this, cx| {
                                            this.toggle_mode_bit(bit, *checked, window, cx)
                                        });
                                    }),
                            );
                        }
                        body = body.child(permissions);
                    }
                    for (label, bit) in [
                        ("Set user ID", 0o4000),
                        ("Set group ID", 0o2000),
                        ("Sticky bit", 0o1000),
                    ] {
                        let weak = weak.clone();
                        body = body.child(
                            Checkbox::new(label)
                                .label(label)
                                .checked(mode.is_some_and(|m| m & bit != 0))
                                .disabled(disabled)
                                .on_click(move |checked, window, cx| {
                                    let _ = weak.update(cx, |this, cx| {
                                        this.toggle_mode_bit(bit, *checked, window, cx)
                                    });
                                }),
                        );
                    }
                    let ownership_disabled = p.busy
                        || !self.available
                        || attrs.is_none_or(|a| a.uid.is_none() || a.gid.is_none() || a.is_symlink);
                    body = body.child("Owner (name or UID)").child(Input::new(&p.owner).disabled(ownership_disabled))
                        .child("Group (name or GID)").child(Input::new(&p.group).disabled(ownership_disabled))
                        .child(div().text_xs().text_color(cx.theme().muted_foreground).child("Uses your SSH login privileges. No automatic sudo. Name lookup requires getent; numeric IDs also work."));
                }
                if p.entry.is_directory {
                    let recursive = weak.clone();
                    body = body.child(
                        Switch::new("permission-recursive")
                            .label("Apply recursively to everything inside")
                            .checked(p.recursive)
                            .disabled(disabled)
                            .on_click(move |checked, _, cx| {
                                let _ = recursive.update(cx, |this, cx| {
                                    if let Some(FileEditor::Permissions(p)) = &mut this.editor {
                                        p.recursive = *checked;
                                    }
                                    cx.notify();
                                });
                            }),
                    );
                    if p.recursive {
                        body = body.child(div().text_sm().text_color(cx.theme().primary).child("The exact mode applies to files and folders, including executable bits. Symlinks are skipped. Only changed ownership fields propagate."));
                    }
                }
                if !p.message.is_empty() {
                    body = body.child(div().text_sm().child(p.message.clone()));
                }
                if attrs.is_none() && !p.busy {
                    let retry = weak.clone();
                    body = body.child(
                        Button::new("permission-retry")
                            .label("Retry")
                            .disabled(!self.available)
                            .on_click(move |_, _, cx| {
                                let _ = retry.update(cx, |this, cx| {
                                    if let Some(FileEditor::Permissions(p)) = &mut this.editor {
                                        p.busy = true;
                                        let job = Job::Attributes {
                                            path: p.entry.path.clone(),
                                            serial: p.serial,
                                        };
                                        this.enqueue(job, cx);
                                    }
                                });
                            }),
                    );
                }
            }
        }
        body.into_any_element()
    }
    fn editor_footer(&self, cx: &gpui::App, weak: gpui::WeakEntity<Self>) -> gpui::AnyElement {
        let cancel = weak.clone();
        let (label, enabled) = match &self.editor {
            Some(FileEditor::Permissions(p)) => (
                "Apply",
                self.available && !p.busy && permission_change(p, cx).is_ok_and(|c| c.is_some()),
            ),
            Some(FileEditor::Transfer { action, .. }) => (action.label(), self.available),
            _ => ("", false),
        };
        div()
            .flex()
            .justify_end()
            .gap_2()
            .child(Button::new("file-editor-cancel").label("Cancel").on_click(
                move |_, window, cx| {
                    let _ = cancel.update(cx, |this, cx| this.close_editor(cx));
                    window.close_dialog(cx);
                },
            ))
            .when(!label.is_empty(), |d| {
                d.child(
                    Button::new("file-editor-apply")
                        .primary()
                        .label(label)
                        .disabled(!enabled)
                        .on_click(move |_, window, cx| {
                            let _ = weak.update(cx, |this, cx| this.submit_editor(window, cx));
                        }),
                )
            })
            .into_any_element()
    }
    fn toggle_mode_bit(
        &mut self,
        bit: u32,
        checked: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(FileEditor::Permissions(p)) = &self.editor
            && let Ok(mode) = parse_mode(&p.mode.read(cx).value())
        {
            let mode = if checked { mode | bit } else { mode & !bit };
            p.mode
                .update(cx, |i, cx| i.set_value(format!("{mode:03o}"), window, cx));
        }
        cx.notify();
    }
    fn submit_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.available {
            return;
        }
        match &mut self.editor {
            Some(FileEditor::Transfer {
                entry,
                action,
                destination,
                name,
                error,
            }) => {
                let directory = if *action == FileAction::Rename {
                    parent(&entry.path)
                } else {
                    destination.read(cx).value().to_string()
                };
                let target = match opsssh_drop::join_remote(&directory, &name.read(cx).value()) {
                    Ok(path) if path.starts_with('/') && path != entry.path => path,
                    _ => {
                        *error = "Enter an absolute destination and a different valid name".into();
                        cx.notify();
                        return;
                    }
                };
                let job = Job::RemoteTransfer {
                    source: entry.path.clone(),
                    target,
                    copy: *action == FileAction::Copy,
                    control: TransferControl::default(),
                };
                self.editor = None;
                window.close_dialog(cx);
                self.enqueue(job, cx);
            }
            Some(FileEditor::Permissions(p)) if !p.busy => match permission_change(p, cx) {
                Ok(Some(change)) => {
                    let owner = (p.owner.read(cx).value().as_ref() != p.original_owner)
                        .then(|| p.owner.read(cx).value().to_string());
                    let group = (p.group.read(cx).value().as_ref() != p.original_group)
                        .then(|| p.group.read(cx).value().to_string());
                    let control = TransferControl::default();
                    p.control = Some(control.clone());
                    p.busy = true;
                    p.message = "Applying… completed changes are retained if cancelled.".into();
                    let job = Job::Permissions {
                        path: p.entry.path.clone(),
                        serial: p.serial,
                        expected: p.attrs.clone().expect("validated"),
                        change,
                        owner,
                        group,
                        control,
                    };
                    self.enqueue(job, cx);
                }
                Err(e) => {
                    p.message = e;
                    cx.notify();
                }
                _ => {}
            },
            _ => {}
        }
    }
    pub(super) fn receive_attributes(
        &mut self,
        serial: u64,
        result: Result<(RemoteAttributes, String, String), String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(FileEditor::Permissions(p)) = &mut self.editor else {
            return;
        };
        if p.serial != serial {
            return;
        }
        p.busy = false;
        match result {
            Ok((attrs, owner, group)) => {
                p.mode.update(cx, |i, cx| {
                    i.set_value(
                        attrs.mode.map(|m| format!("{m:03o}")).unwrap_or_default(),
                        window,
                        cx,
                    )
                });
                p.owner
                    .update(cx, |i, cx| i.set_value(owner.clone(), window, cx));
                p.group
                    .update(cx, |i, cx| i.set_value(group.clone(), window, cx));
                p.original_owner = owner;
                p.original_group = group;
                p.message = if attrs.is_symlink {
                    "Open the actual target to edit permissions.".into()
                } else if attrs.mode.is_none() {
                    "Server did not provide permissions.".into()
                } else if attrs.uid.is_none() || attrs.gid.is_none() {
                    "Server did not provide ownership; ownership editing is unavailable.".into()
                } else {
                    String::new()
                };
                p.attrs = Some(attrs);
            }
            Err(e) => p.message = e,
        }
        cx.notify();
    }
    pub(super) fn receive_permissions(
        &mut self,
        serial: u64,
        result: Result<(RemoteAttributes, String), String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(FileEditor::Permissions(p)) = &mut self.editor else {
            return;
        };
        if p.serial != serial {
            return;
        }
        p.busy = false;
        p.control = None;
        match result {
            Ok((attrs, report)) => {
                p.message = report;
                p.recursive = false;
                p.original_owner = attrs.uid.map(|id| id.to_string()).unwrap_or_default();
                p.original_group = attrs.gid.map(|id| id.to_string()).unwrap_or_default();
                p.owner.update(cx, |i, cx| {
                    i.set_value(p.original_owner.clone(), window, cx)
                });
                p.group.update(cx, |i, cx| {
                    i.set_value(p.original_group.clone(), window, cx)
                });
                p.mode.update(cx, |i, cx| {
                    i.set_value(
                        attrs.mode.map(|m| format!("{m:03o}")).unwrap_or_default(),
                        window,
                        cx,
                    )
                });
                p.attrs = Some(attrs);
            }
            Err(e) => {
                p.message = format!("{e}\nReopen Permissions to refresh any partial changes.")
            }
        }
        cx.notify();
    }
}
fn parent(path: &str) -> String {
    path.rsplit_once('/')
        .map(|(p, _)| if p.is_empty() { "/" } else { p })
        .unwrap_or("/")
        .to_owned()
}
fn presets(directory: bool) -> Vec<(u32, &'static str, &'static str)> {
    if directory {
        vec![
            (0o700, "Owner only", "Only the owner can access it"),
            (
                0o750,
                "Group read",
                "Group can read and enter; others cannot",
            ),
            (0o755, "Executable", "Everyone can read and enter"),
            (0o777, "For all", "Everyone can change it. Rarely correct."),
        ]
    } else {
        vec![
            (0o600, "Owner only", "Only the owner can read and write"),
            (0o640, "Group read", "Group can read; others cannot"),
            (
                0o644,
                "Readable",
                "Everyone can read; only the owner can write",
            ),
            (0o755, "Executable", "Everyone can read and run"),
            (
                0o666,
                "For all",
                "Everyone can read and write. Rarely correct.",
            ),
        ]
    }
}
fn permission_change(
    p: &PermissionEditor,
    cx: &gpui::App,
) -> Result<Option<PermissionChange>, String> {
    let attrs = p.attrs.as_ref().ok_or("Permissions are not loaded")?;
    if attrs.is_symlink {
        return Err("Symlink permissions cannot be edited".into());
    }
    let mode = parse_mode(&p.mode.read(cx).value()).map_err(|e| e.to_string())?;
    let owner = p.owner.read(cx).value();
    let group = p.group.read(cx).value();
    let ownership = owner.as_ref() != p.original_owner || group.as_ref() != p.original_group;
    if ownership && (attrs.uid.is_none() || attrs.gid.is_none()) {
        return Err("Server omitted ownership".into());
    }
    if ownership
        && (owner.is_empty()
            || group.is_empty()
            || owner.contains(['\n', '\r', '\0'])
            || group.contains(['\n', '\r', '\0']))
    {
        return Err("Enter an owner and group name or numeric ID".into());
    }
    let mode_changed = attrs.mode != Some(mode) || p.recursive;
    Ok((mode_changed || ownership).then_some(PermissionChange {
        mode: mode_changed.then_some(mode),
        recursive: p.recursive,
        ..Default::default()
    }))
}
pub(super) fn report_result(report: &OperationReport) -> opsssh_sftp::Result<()> {
    if report.cancelled || !report.failures.is_empty() {
        Err(report.to_string().into())
    } else {
        Ok(())
    }
}
pub(super) async fn atomic_renamer(
    commands: &Sender<SshCommand>,
    control: &TransferControl,
) -> opsssh_sftp::Result<opsssh_sftp::AtomicRenamer> {
    control.ensure_active()?;
    let (reply, receive) = async_channel::bounded(1);
    commands.send(SshCommand::OpenSftp { reply }).await?;
    let stream = tokio::time::timeout(Duration::from_secs(20), receive.recv())
        .await??
        .map_err(std::io::Error::other)?;
    control.ensure_active()?;
    let renamer = opsssh_sftp::AtomicRenamer::connect(stream).await?;
    control.ensure_active()?;
    Ok(renamer)
}
async fn lookup(
    commands: &Sender<SshCommand>,
    value: &str,
    group: bool,
) -> opsssh_sftp::Result<(String, u32)> {
    let quoted = opsssh_drop::quote(value, opsssh_drop::Shell::Posix)?;
    let mut request = ExecRequest::new(format!(
        "getent -- {} {quoted}",
        if group { "group" } else { "passwd" }
    ));
    request.output_limit = 8192;
    let (reply, receive) = async_channel::bounded(1);
    commands.send(SshCommand::Exec { request, reply }).await?;
    let output = tokio::time::timeout(Duration::from_secs(6), receive.recv())
        .await??
        .map_err(std::io::Error::other)?;
    if output.exit_status != Some(0) {
        return Err("Name lookup unavailable or unknown identity; use a numeric UID/GID".into());
    }
    let text = String::from_utf8(output.stdout)?;
    let fields: Vec<_> = text
        .lines()
        .next()
        .ok_or("Identity not found")?
        .split(':')
        .collect();
    Ok((
        fields.first().ok_or("Invalid identity")?.to_string(),
        fields.get(2).ok_or("Invalid identity")?.parse()?,
    ))
}
pub(super) async fn resolve_identity(
    commands: &Sender<SshCommand>,
    value: &str,
    group: bool,
) -> opsssh_sftp::Result<u32> {
    if let Ok(id) = value.parse() {
        return Ok(id);
    }
    Ok(lookup(commands, value, group).await?.1)
}
pub(super) async fn load_attributes(
    client: &SftpClient,
    commands: &Sender<SshCommand>,
    path: &str,
) -> opsssh_sftp::Result<(RemoteAttributes, String, String)> {
    let attrs = client.attributes(path).await?;
    let owner = if let Some(id) = attrs.uid {
        lookup(commands, &id.to_string(), false)
            .await
            .map(|r| r.0)
            .unwrap_or_else(|_| id.to_string())
    } else {
        "Unavailable".into()
    };
    let group = if let Some(id) = attrs.gid {
        lookup(commands, &id.to_string(), true)
            .await
            .map(|r| r.0)
            .unwrap_or_else(|_| id.to_string())
    } else {
        "Unavailable".into()
    };
    Ok((attrs, owner, group))
}

#[cfg(all(test, feature = "capture"))]
mod tests {
    use super::*;
    #[gpui::test]
    fn permission_editor_preserves_ownership_only_intent_and_checks_stale_results(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(gpui_component::init);
        let (pane, cx) = cx.add_window_view(FilePane::preview);
        cx.update(|window, cx| {
            pane.update(cx, |this, cx| {
                let mode = cx.new(|cx| InputState::new(window, cx).default_value("4755"));
                let owner = cx.new(|cx| InputState::new(window, cx).default_value("root"));
                let group = cx.new(|cx| InputState::new(window, cx).default_value("root"));
                this.editor = Some(FileEditor::Permissions(Box::new(PermissionEditor {
                    entry: this.entries[0].clone(),
                    serial: 9,
                    attrs: Some(RemoteAttributes {
                        mode: Some(0o4755),
                        uid: Some(0),
                        gid: Some(0),
                        is_directory: true,
                        is_regular: false,
                        is_symlink: false,
                        size: None,
                        modified: None,
                    }),
                    mode,
                    owner,
                    group,
                    original_owner: "root".into(),
                    original_group: "root".into(),
                    advanced: true,
                    recursive: false,
                    busy: false,
                    message: String::new(),
                    control: None,
                })));
                let Some(FileEditor::Permissions(p)) = &mut this.editor else {
                    panic!()
                };
                assert!(permission_change(p, cx).unwrap().is_none());
                p.owner
                    .update(cx, |i, cx| i.set_value("deploy", window, cx));
                assert!(permission_change(p, cx).unwrap().unwrap().mode.is_none());
                p.recursive = true;
                assert_eq!(
                    permission_change(p, cx).unwrap().unwrap().mode,
                    Some(0o4755)
                );
                p.mode.update(cx, |i, cx| i.set_value("888", window, cx));
                assert!(permission_change(p, cx).is_err());
                this.receive_attributes(8, Err("stale".into()), window, cx);
                let Some(FileEditor::Permissions(p)) = &this.editor else {
                    panic!()
                };
                assert!(p.message.is_empty());
                this.suspend(cx);
                this.receive_permissions(9, Err("stale after disconnect".into()), window, cx);
                assert!(this.editor.is_none());
            })
        });
    }
    #[gpui::test]
    fn copy_path_uses_captured_item_even_after_selection_changes(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        let (pane, cx) = cx.add_window_view(FilePane::preview);
        cx.update(|window, cx| {
            pane.update(cx, |this, cx| {
                let clicked = this.entries[0].clone();
                this.selected = Some(3);
                this.file_action(clicked.clone(), FileAction::CopyPath, window, cx);
                assert_eq!(
                    cx.read_from_clipboard().unwrap().text().unwrap(),
                    clicked.path
                );
            })
        });
    }
}
