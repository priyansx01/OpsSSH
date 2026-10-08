//! Terminal drop destinations stay independent from file-browser visibility.
use super::*;
use gpui_component::{ActiveTheme, Disableable, Icon, IconName, checkbox::Checkbox};

pub(super) struct UploadDialog {
    session: SessionId,
    generation: u64,
    pub(super) request: u64,
    paths: Vec<PathBuf>,
    pub(super) pane: Entity<FilePane>,
    input: Entity<InputState>,
    remember: bool,
    pub(super) checking: bool,
    error: String,
}

impl Workspace {
    pub(super) fn terminal_drop(
        &mut self,
        terminal: &Entity<TerminalView>,
        paths: Vec<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(index) = self
            .sessions
            .iter()
            .position(|tab| &tab.terminal == terminal)
        else {
            return;
        };
        if paths.is_empty() || !terminal.read(cx).session_state().accepts_input() {
            return;
        }
        let Some(pane) = self.background_files(terminal, window, cx) else {
            return;
        };
        if let Some(directory) = self.sessions[index].upload_directory.clone() {
            pane.update(cx, |pane, cx| {
                pane.upload_files(paths, Some(directory), true, cx)
            });
        } else {
            self.choose_upload_destination(self.sessions[index].id, paths, window, cx);
        }
    }

    pub(super) fn choose_upload_destination(
        &mut self,
        session: SessionId,
        paths: Vec<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.upload_dialog.is_some() {
            return;
        }
        let Some(index) = self.session_index(session) else {
            return;
        };
        let terminal = self.sessions[index].terminal.clone();
        let Some(pane) = self.background_files(&terminal, window, cx) else {
            return;
        };
        let initial = self.sessions[index]
            .upload_directory
            .clone()
            .or_else(|| terminal.read(cx).current_directory().map(str::to_owned))
            .unwrap_or_else(|| pane.read(cx).directory().to_owned());
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(initial)
                .placeholder("/home/user/uploads")
        });
        self.transition = self.transition.wrapping_add(1);
        self.upload_dialog = Some(UploadDialog {
            session,
            generation: terminal.read(cx).connection_generation(),
            request: self.transition,
            paths,
            pane,
            input: input.clone(),
            remember: true,
            checking: false,
            error: String::new(),
        });
        let request = self.transition;
        cx.subscribe_in(&input, window, move |this, _, event, window, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.validate_upload_destination(request, window, cx);
            }
        })
        .detach();
        self.update_keyboard_capture(window, cx);
        let weak = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, window, cx| {
            let Some(workspace) = weak.upgrade() else {
                return dialog;
            };
            let Some(state) = workspace.read(cx).upload_dialog.as_ref() else {
                return dialog;
            };
            let request = state.request;
            let pane = state.pane.clone();
            let input = state.input.clone();
            let remember = state.remember;
            let checking = state.checking;
            let error = state.error.clone();
            let browse_directory = pane.read(cx).directory().to_owned();
            let parent = browse_directory
                .rsplit_once('/')
                .map(|(parent, _)| if parent.is_empty() { "/" } else { parent })
                .unwrap_or(".")
                .to_owned();
            let folders = pane.read(cx).folders();
            let cancel_button = weak.clone();
            let on_cancel = weak.clone();
            let on_remember = weak.clone();
            let on_save = weak.clone();
            let browse_pane = pane.clone();
            let browse_input = input.clone();
            let up_pane = pane.clone();
            let up_input = input.clone();
            dialog
                .title(tr("upload-destination-title"))
                .width(px(520.).min(window.viewport_size().width - px(48.)))
                .close_button(false)
                .overlay_closable(false)
                .on_close(move |_, window, cx| {
                    let _ = on_cancel.update(cx, |this, cx| {
                        this.upload_dialog = None;
                        this.restore_terminal_focus(window, cx);
                        cx.notify();
                    });
                })
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child(
                            div()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child(tr("upload-destination-help")),
                        )
                        .child(Input::new(&input).disabled(checking))
                        .child(
                            div()
                                .flex()
                                .gap_2()
                                .child(
                                    Button::new("browse-upload-folder")
                                        .ghost()
                                        .label(tr("upload-browse"))
                                        .disabled(checking)
                                        .on_click(move |_, _, cx| {
                                            let path = browse_input.read(cx).value().to_string();
                                            browse_pane.update(cx, |pane, cx| {
                                                pane.follow_directory(path, cx)
                                            });
                                        }),
                                )
                                .child(
                                    Button::new("upload-folder-up")
                                        .ghost()
                                        .label(tr("file-up"))
                                        .disabled(checking)
                                        .on_click(move |_, window, cx| {
                                            up_input.update(cx, |input, cx| {
                                                input.set_value(parent.clone(), window, cx)
                                            });
                                            up_pane.update(cx, |pane, cx| {
                                                pane.follow_directory(parent.clone(), cx)
                                            });
                                        }),
                                ),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(browse_directory),
                        )
                        .child(
                            div()
                                .id("upload-folders")
                                .max_h(px(160.))
                                .overflow_y_scroll()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .children(folders.into_iter().enumerate().map(
                                    |(index, folder)| {
                                        let pane = pane.clone();
                                        let input = input.clone();
                                        Button::new(("upload-folder", index))
                                            .ghost()
                                            .w_full()
                                            .accessibility_label(folder.name.clone())
                                            .child(
                                                div()
                                                    .w_full()
                                                    .min_w_0()
                                                    .flex()
                                                    .items_center()
                                                    .gap_2()
                                                    .child(Icon::new(IconName::Folder))
                                                    .child(div().truncate().child(folder.name)),
                                            )
                                            .disabled(checking)
                                            .on_click(move |_, window, cx| {
                                                input.update(cx, |input, cx| {
                                                    input.set_value(folder.path.clone(), window, cx)
                                                });
                                                pane.update(cx, |pane, cx| {
                                                    pane.follow_directory(folder.path.clone(), cx)
                                                });
                                            })
                                    },
                                )),
                        )
                        .child(
                            Checkbox::new("remember-upload-folder")
                                .checked(remember)
                                .label(tr("upload-remember"))
                                .disabled(checking)
                                .on_click(move |checked, _, cx| {
                                    let _ = on_remember.update(cx, |this, cx| {
                                        if let Some(dialog) = &mut this.upload_dialog {
                                            dialog.remember = *checked;
                                        }
                                        cx.notify();
                                    });
                                }),
                        )
                        .when(!error.is_empty(), |d| {
                            d.child(div().text_sm().text_color(cx.theme().danger).child(error))
                        })
                        .child(
                            div()
                                .flex()
                                .justify_end()
                                .gap_2()
                                .child(
                                    Button::new("cancel-upload-destination")
                                        .ghost()
                                        .label(tr("cancel"))
                                        .on_click(move |_, window, cx| {
                                            let _ = cancel_button.update(cx, |this, cx| {
                                                this.upload_dialog = None;
                                                window.close_dialog(cx);
                                                this.restore_terminal_focus(window, cx);
                                                cx.notify();
                                            });
                                        }),
                                )
                                .child(
                                    Button::new("save-upload-destination")
                                        .primary()
                                        .label(tr(if checking {
                                            "upload-checking"
                                        } else {
                                            "upload-use-folder"
                                        }))
                                        .disabled(checking)
                                        .on_click(move |_, window, cx| {
                                            let _ = on_save.update(cx, |this, cx| {
                                                this.validate_upload_destination(
                                                    request, window, cx,
                                                )
                                            });
                                        }),
                                ),
                        ),
                )
        });
        input.update(cx, |input, cx| input.focus(window, cx));
        cx.notify();
    }

    fn validate_upload_destination(
        &mut self,
        request: u64,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(dialog) = self
            .upload_dialog
            .as_mut()
            .filter(|d| d.request == request && !d.checking)
        else {
            return;
        };
        let path = dialog.input.read(cx).value().to_string();
        if path.is_empty() || path.len() > 4096 || path.chars().any(char::is_control) {
            dialog.error = tr("upload-invalid-directory");
            cx.notify();
            return;
        }
        dialog.checking = true;
        dialog.error.clear();
        dialog
            .pane
            .update(cx, |pane, cx| pane.validate_destination(request, path, cx));
        cx.notify();
    }

    pub(super) fn destination_failed(
        &mut self,
        pane: &Entity<FilePane>,
        request: u64,
        error: String,
        cx: &mut Context<Self>,
    ) {
        if let Some(dialog) = self
            .upload_dialog
            .as_mut()
            .filter(|d| &d.pane == pane && d.request == request)
        {
            dialog.checking = false;
            dialog.error = error;
            cx.notify();
        }
    }

    pub(super) fn destination_validated(
        &mut self,
        pane: &Entity<FilePane>,
        request: u64,
        path: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self
            .upload_dialog
            .as_ref()
            .is_some_and(|d| &d.pane == pane && d.request == request && d.checking)
        {
            return;
        }
        let Some(dialog) = self.upload_dialog.take() else {
            return;
        };
        let Some(index) = self.session_index(dialog.session) else {
            window.close_dialog(cx);
            return;
        };
        if self.sessions[index]
            .terminal
            .read(cx)
            .connection_generation()
            != dialog.generation
            || !self.sessions[index]
                .terminal
                .read(cx)
                .session_state()
                .accepts_input()
        {
            window.close_dialog(cx);
            self.message = tr("file-interrupted");
            cx.notify();
            return;
        }
        if dialog.remember {
            let old = self.store.clone();
            if let Some(id) = self.sessions[index].profile_id
                && let Some(profile) = self.store.servers.iter_mut().find(|p| p.id == id)
            {
                profile.terminal_upload_directory = Some(path.clone());
                if !self.persist() {
                    self.store = old;
                }
            }
            self.sessions[index].upload_directory = Some(path.clone());
        }
        pane.update(cx, |pane, cx| {
            pane.change_retry_destination(path.clone(), cx)
        });
        window.close_dialog(cx);
        self.restore_terminal_focus(window, cx);
        if !dialog.paths.is_empty() {
            pane.update(cx, |pane, cx| {
                pane.upload_files(dialog.paths, Some(path), true, cx)
            });
        }
        cx.notify();
    }
}
