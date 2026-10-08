//! Native per-session key registry. All remote operations use independent exec channels.
use crate::localization::text as tr;
use async_channel::Sender;
use gpui::{ClipboardItem, Context, Entity, Render, Window, div, prelude::*, px, uniform_list};
use gpui_component::{
    ActiveTheme, Disableable, IconName, Selectable, WindowExt,
    button::{Button, ButtonVariants},
    input::{Input, InputEvent, InputState},
};
use opsssh_ssh_core::{
    AuthenticationInfo, AuthenticationMethod, ExecControl, ExecRequest, SshCommand,
};
use opsssh_ssh_management::{self as management, KeyEntry, Registry};
use serde_json::{Value, json};
use std::sync::Arc;

#[derive(Clone)]
enum Editor {
    Add,
    Comment(KeyEntry),
    Remove(KeyEntry),
    Details(KeyEntry),
    Create,
    Repair,
}
struct Fields {
    key: Entity<InputState>,
    comment: Entity<InputState>,
    username: Entity<InputState>,
    password: Entity<InputState>,
}
pub struct SshManagementView {
    window: gpui::AnyWindowHandle,
    commands: Sender<SshCommand>,
    generation: u64,
    authentication: Option<AuthenticationInfo>,
    snapshot: Option<Arc<Registry>>,
    search: Entity<InputState>,
    fields: Fields,
    visible: bool,
    available: bool,
    busy: bool,
    serial: u64,
    operation: Option<ExecControl>,
    message: String,
    editor: Option<Editor>,
    editor_error: String,
    editor_serial: u64,
    partial: Option<Value>,
    shell: String,
    shells: Vec<String>,
    preview: bool,
}
impl SshManagementView {
    pub fn new(
        commands: Sender<SshCommand>,
        generation: u64,
        authentication: Option<AuthenticationInfo>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let handle = window.window_handle();
        let mut input = |cx: &mut Context<Self>, placeholder| {
            cx.new(|cx| InputState::new(window, cx).placeholder(placeholder))
        };
        let search = input(cx, "Search names, fingerprints or key types");
        cx.subscribe(&search, |_, _, event, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        })
        .detach();
        Self {
            window: handle,
            commands,
            generation,
            authentication,
            snapshot: None,
            search,
            fields: Fields {
                key: input(cx, "ssh-ed25519 AAAA… name@device"),
                comment: input(cx, "Name or description"),
                username: input(cx, "deploy"),
                password: cx.new(|cx| {
                    InputState::new(window, cx)
                        .masked(true)
                        .placeholder("Leave empty for passwordless sudo")
                }),
            },
            visible: false,
            available: true,
            busy: false,
            serial: 0,
            operation: None,
            message: String::new(),
            editor: None,
            editor_error: String::new(),
            editor_serial: 0,
            partial: None,
            shell: "/bin/sh".into(),
            shells: vec![],
            preview: false,
        }
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
    pub fn suspend(&mut self, cx: &mut Context<Self>) {
        self.available = false;
        self.serial = self.serial.wrapping_add(1);
        if let Some(control) = self.operation.take() {
            control.cancel();
        }
        self.busy = false;
        self.close_editor(cx);
        self.message = tr("sshmgmt-disconnected");
        cx.notify();
    }
    pub fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        if self.visible == visible {
            return;
        }
        self.visible = visible;
        if visible && self.snapshot.is_none() && self.available && !self.preview {
            self.refresh(cx);
        }
        cx.notify();
    }
    fn refresh(&mut self, cx: &mut Context<Self>) {
        let Some(auth) = &self.authentication else {
            self.message = tr("sshmgmt-auth-unavailable");
            cx.notify();
            return;
        };
        if self.busy || !self.available || self.preview {
            return;
        }
        match management::request(json!({"op":"read","account":auth.username}), false, None) {
            Ok(request) => self.execute(request, false, cx),
            Err(error) => {
                self.message = error;
                cx.notify();
            }
        }
    }
    fn execute(&mut self, request: ExecRequest, creating: bool, cx: &mut Context<Self>) {
        if self.busy || !self.available || self.preview {
            return;
        }
        let (reply, receive) = async_channel::bounded(1);
        self.operation = Some(request.control.clone());
        self.serial = self.serial.wrapping_add(1);
        let serial = self.serial;
        if self
            .commands
            .try_send(SshCommand::Exec { request, reply })
            .is_err()
        {
            self.operation = None;
            self.message = tr("sshmgmt-disconnected");
            cx.notify();
            return;
        }
        self.busy = true;
        self.editor_error.clear();
        cx.spawn(async move |this, cx| {
            let result = receive
                .recv()
                .await
                .unwrap_or_else(|_| Err(tr("sshmgmt-result-unknown")));
            let _ = this.update(cx, |this, cx| {
                if this.serial != serial || !this.available {
                    return;
                }
                this.busy = false;
                this.operation = None;
                let result = result.and_then(|output| {
                    if creating {
                        let value = management::response(&output)?;
                        if value["ok"] == true && output.exit_status == Some(0) {
                            this.partial = None;
                            this.message = format!(
                                "{} {}",
                                tr("sshmgmt-user-created"),
                                value["created"].as_str().unwrap_or("")
                            );
                            Ok(None)
                        } else {
                            if value["created"].is_string() && value["uid"].is_u64() {
                                this.partial = Some(value.clone());
                                this.editor = Some(Editor::Repair);
                            }
                            Err(value["error"]
                                .as_str()
                                .unwrap_or("Account creation did not complete")
                                .to_owned())
                        }
                    } else {
                        let value = management::reply(output)?;
                        this.shells = value["shells"]
                            .as_array()
                            .map(|items| {
                                items
                                    .iter()
                                    .filter_map(|v| v.as_str().map(str::to_owned))
                                    .collect()
                            })
                            .unwrap_or_default();
                        this.shell = this
                            .shells
                            .first()
                            .cloned()
                            .unwrap_or_else(|| "/bin/sh".into());
                        Registry::from_reply(&value).map(|registry| Some(Arc::new(registry)))
                    }
                });
                match result {
                    Ok(snapshot) => {
                        if let Some(snapshot) = snapshot {
                            this.snapshot = Some(snapshot);
                            this.message = tr("sshmgmt-refreshed");
                        }
                        this.close_editor(cx);
                    }
                    Err(error) => {
                        this.message = error.clone();
                        this.editor_error = error;
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
    fn close_editor(&mut self, cx: &mut Context<Self>) {
        if self.editor.take().is_none() {
            return;
        }
        let window = self.window;
        let password = self.fields.password.clone();
        cx.defer(move |cx| {
            let _ = window.update(cx, |_, window, cx| {
                password.update(cx, |input, cx| input.set_value("", window, cx));
                window.close_dialog(cx);
            });
        });
    }
    fn active_key(&self) -> Option<&str> {
        self.authentication
            .as_ref()
            .and_then(|info| info.fingerprint.as_deref())
    }
    fn open_editor(&mut self, editor: Editor, window: &mut Window, cx: &mut Context<Self>) {
        if self.editor.is_some() || self.busy {
            return;
        }
        self.editor_error.clear();
        self.fields
            .password
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.fields
            .key
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.fields.comment.update(cx, |input, cx| {
            input.set_value(
                match &editor {
                    Editor::Comment(key) => key.comment.clone(),
                    _ => String::new(),
                },
                window,
                cx,
            )
        });
        self.editor_serial = self.editor_serial.wrapping_add(1);
        self.editor = Some(editor);
        let weak = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, window, cx| {
            let Some(view) = weak.upgrade() else {
                return dialog;
            };
            let state = view.read(cx);
            let Some(editor) = state.editor.clone() else {
                return dialog;
            };
            let submit = weak.clone();
            let cancel = weak.clone();
            let closed = weak.clone();
            let fields = (
                &state.fields.key,
                &state.fields.comment,
                &state.fields.username,
                &state.fields.password,
            );
            let busy = state.busy;
            let creating = matches!(editor, Editor::Create | Editor::Repair);
            let mut body = div()
                .id("ssh-management-editor-body")
                .flex()
                .flex_col()
                .gap_3()
                .max_h((window.viewport_size().height - px(220.)).max(px(160.)))
                .overflow_y_scroll();
            if let Some(snapshot) = &state.snapshot {
                body = body.child(
                    div()
                        .text_sm()
                        .child(format!("{} · {}", snapshot.account, snapshot.path)),
                );
            }
            body = match &editor {
                Editor::Add => body
                    .child(div().text_sm().child(tr("sshmgmt-public-key")))
                    .child(
                        Input::new(fields.0)
                            .aria_label(tr("sshmgmt-public-key"))
                            .w_full()
                            .disabled(busy),
                    )
                    .child(
                        Button::new("import-public-key")
                            .label(tr("sshmgmt-import"))
                            .disabled(busy)
                            .on_click({
                                let weak = weak.clone();
                                move |_, window, cx| {
                                    let _ = weak.update(cx, |this, cx| this.import_key(window, cx));
                                }
                            }),
                    ),
                Editor::Comment(key) => body
                    .child(div().text_sm().child(key.fingerprint.clone()))
                    .child(
                        Input::new(fields.1)
                            .aria_label(tr("sshmgmt-comment"))
                            .w_full()
                            .disabled(busy),
                    ),
                Editor::Remove(key) => body
                    .child(div().child(tr("sshmgmt-remove-help")))
                    .child(div().text_sm().child(key.fingerprint.clone()))
                    .child(div().text_sm().child(key.comment.clone())),
                Editor::Details(key) => body
                    .child(div().text_sm().child(key.algorithm.clone()))
                    .child(div().text_sm().child(key.fingerprint.clone()))
                    .child(div().text_sm().child(key.public_key.clone()))
                    .child(div().text_sm().child(format!(
                        "{} {}",
                        tr("sshmgmt-restrictions"),
                        if key.restrictions.is_empty() {
                            tr("sshmgmt-none")
                        } else {
                            key.restrictions.clone()
                        }
                    ))),
                Editor::Create | Editor::Repair => body
                    .child(div().text_sm().child(tr("sshmgmt-create-help")))
                    .child(div().text_sm().child(tr("sshmgmt-username")))
                    .child(
                        Input::new(fields.2)
                            .aria_label(tr("sshmgmt-username"))
                            .w_full()
                            .disabled(busy || matches!(editor, Editor::Repair)),
                    )
                    .child(div().text_sm().child(tr("sshmgmt-display-name")))
                    .child(
                        Input::new(fields.1)
                            .aria_label(tr("sshmgmt-display-name"))
                            .w_full()
                            .disabled(busy),
                    )
                    .child(div().text_sm().child(tr("sshmgmt-public-key")))
                    .child(
                        Input::new(fields.0)
                            .aria_label(tr("sshmgmt-public-key"))
                            .w_full()
                            .disabled(busy),
                    )
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .children(state.shells.iter().enumerate().map(|(index, shell)| {
                                let shell = shell.clone();
                                let chosen = state.shell == shell;
                                let change = weak.clone();
                                Button::new(("ssh-user-shell", index))
                                    .label(shell.clone())
                                    .selected(chosen)
                                    .disabled(busy)
                                    .on_click(move |_, _, cx| {
                                        let _ = change.update(cx, |this, cx| {
                                            this.shell = shell.clone();
                                            cx.notify();
                                        });
                                    })
                            })),
                    )
                    .when(
                        state
                            .snapshot
                            .as_ref()
                            .is_some_and(|snapshot| snapshot.uid != 0),
                        |body| {
                            body.child(div().text_sm().child(tr("sshmgmt-sudo-help")))
                                .child(
                                    Input::new(fields.3)
                                        .aria_label(tr("sshmgmt-sudo-password"))
                                        .w_full()
                                        .disabled(busy),
                                )
                        },
                    ),
            };
            if !state.editor_error.is_empty() {
                body = body.child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().danger)
                        .child(state.editor_error.clone()),
                );
            }
            if state.partial.is_some() && creating {
                body = body.child(div().text_sm().child(tr("sshmgmt-partial-help")));
            }
            let editable = !matches!(editor, Editor::Details(_));
            let title = tr(match editor {
                Editor::Add => "sshmgmt-add",
                Editor::Comment(_) => "sshmgmt-edit-comment",
                Editor::Remove(_) => "sshmgmt-remove",
                Editor::Details(_) => "sshmgmt-details",
                Editor::Create => "sshmgmt-create-user",
                Editor::Repair => "sshmgmt-repair",
            });
            dialog
                .title(title)
                .width(px(600.).min(window.viewport_size().width - px(48.)))
                .close_button(false)
                .overlay_closable(false)
                .keyboard(!busy)
                .on_close(move |_, window, cx| {
                    let _ = closed.update(cx, |this, cx| {
                        this.editor = None;
                        this.fields
                            .password
                            .update(cx, |input, cx| input.set_value("", window, cx));
                        cx.notify();
                    });
                })
                .child(body)
                .footer({
                    div()
                        .flex()
                        .gap_2()
                        .when(editable, |body| {
                            body.child(
                                crate::design::PrimaryAction::new("ssh-management-submit")
                                    .label(tr(if busy {
                                        "sshmgmt-working"
                                    } else if matches!(editor, Editor::Repair) {
                                        "sshmgmt-repair"
                                    } else if creating {
                                        "sshmgmt-create-confirm"
                                    } else {
                                        "sshmgmt-confirm"
                                    }))
                                    .disabled(busy)
                                    .on_click({
                                        let submit = submit.clone();
                                        move |_, window, cx| {
                                            let _ = submit
                                                .update(cx, |this, cx| this.submit(window, cx));
                                        }
                                    }),
                            )
                        })
                        .child(
                            Button::new("ssh-management-cancel")
                                .label(tr("cancel"))
                                .disabled(busy)
                                .on_click({
                                    let cancel = cancel.clone();
                                    move |_, window, cx| {
                                        let _ = cancel.update(cx, |this, cx| {
                                            this.editor = None;
                                            this.fields.password.update(cx, |input, cx| {
                                                input.set_value("", window, cx)
                                            });
                                            cx.notify();
                                        });
                                        window.close_dialog(cx);
                                    }
                                }),
                        )
                        .into_any_element()
                })
        });
        match self.editor.as_ref() {
            Some(Editor::Comment(_)) => self
                .fields
                .comment
                .update(cx, |input, cx| input.focus(window, cx)),
            Some(Editor::Create) => self
                .fields
                .username
                .update(cx, |input, cx| input.focus(window, cx)),
            Some(Editor::Add | Editor::Repair) => self
                .fields
                .key
                .update(cx, |input, cx| input.focus(window, cx)),
            _ => {}
        }
        cx.notify();
    }
    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || !self.available {
            return;
        }
        let Some(snapshot) = self.snapshot.clone() else {
            return;
        };
        let Some(editor) = self.editor.clone() else {
            return;
        };
        let comment = self.fields.comment.read(cx).value().to_string();
        let key = self.fields.key.read(cx).value().to_string();
        let creating = matches!(editor, Editor::Create | Editor::Repair);
        let result = (|| {
            let payload = match editor {
                Editor::Add => snapshot.replacement(snapshot.add(&key)?),
                Editor::Comment(entry) => snapshot.replacement(snapshot.change(
                    &entry,
                    Some(&comment),
                    self.active_key(),
                )?),
                Editor::Remove(entry) => {
                    snapshot.replacement(snapshot.change(&entry, None, self.active_key())?)
                }
                Editor::Details(_) => return Err("This view is read only".into()),
                Editor::Create | Editor::Repair => {
                    let name = self.fields.username.read(cx).value().trim().to_owned();
                    if !management::username(&name) {
                        return Err(tr("sshmgmt-invalid-user"));
                    }
                    let key = management::public_key(&key)?;
                    if comment.len() > 128
                        || comment.contains(':')
                        || comment.chars().any(char::is_control)
                    {
                        return Err("Use a display name of up to 128 characters without colons or control characters".into());
                    }
                    let repairing = matches!(editor, Editor::Repair);
                    if repairing
                        && self.partial.as_ref().is_none_or(|partial| {
                            partial["created"].as_str() != Some(name.as_str())
                        })
                    {
                        return Err(
                            "Partial account identity is unavailable; review it on the server"
                                .into(),
                        );
                    }
                    json!({"op":if repairing {"repair"} else {"create"}, "username":name,"display":comment,"key":key.public_key,"shell":self.shell,"uid":self.partial.as_ref().map(|value|value["uid"].clone())})
                }
            };
            let password = self.fields.password.read(cx).value();
            management::request(
                payload,
                creating && snapshot.uid != 0,
                Some(password.as_str()),
            )
        })();
        self.fields
            .password
            .update(cx, |input, cx| input.set_value("", window, cx));
        match result {
            Ok(request) => self.execute(request, creating, cx),
            Err(error) => {
                self.editor_error = error;
                cx.notify();
            }
        }
    }
    fn import_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let serial = self.editor_serial;
        let picker = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(tr("sshmgmt-import").into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(paths))) = picker.await
                && let Some(path) = paths.first().cloned()
            {
                let result = cx
                    .background_executor()
                    .spawn(async move {
                        use std::io::Read;
                        let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
                        let mut bytes = Vec::new();
                        file.take(8193)
                            .read_to_end(&mut bytes)
                            .map_err(|e| e.to_string())?;
                        let value = String::from_utf8(bytes)
                            .map_err(|_| "Public key must be UTF-8".to_owned())?;
                        management::public_key(&value).map(|key| key.public_key)
                    })
                    .await;
                let _ = this.update_in(cx, |this, window, cx| {
                    if !this.available
                        || this.editor.is_none()
                        || this.editor_serial != serial
                        || this.busy
                    {
                        return;
                    }
                    match result {
                        Ok(key) => this
                            .fields
                            .key
                            .update(cx, |input, cx| input.set_value(key, window, cx)),
                        Err(error) => this.editor_error = error,
                    };
                    cx.notify();
                });
            }
        })
        .detach();
    }
    fn export_key(&self, entry: KeyEntry, window: &mut Window, cx: &mut Context<Self>) {
        let directory = opsssh_platform::home_dir().unwrap_or_default();
        let picker = cx.prompt_for_new_path(&directory, Some("authorized-key.pub"));
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(path))) = picker.await {
                let result = cx
                    .background_executor()
                    .spawn(async move {
                        std::fs::write(path, format!("{}\n", entry.public_key))
                            .map_err(|e| e.to_string())
                    })
                    .await;
                let _ = this.update(cx, |this, cx| {
                    this.message = result.err().unwrap_or_else(|| tr("sshmgmt-exported"));
                    cx.notify();
                });
            }
        })
        .detach();
    }
    #[cfg(feature = "capture")]
    pub fn preview(window: &mut Window, cx: &mut Context<Self>, screen: &str) -> Self {
        let (commands, _) = async_channel::bounded(1);
        let auth = AuthenticationInfo {
            username: "deploy".into(),
            method: AuthenticationMethod::Password,
            fingerprint: None,
        };
        let mut view = Self::new(commands, 0, Some(auth), window, cx);
        view.preview = true;
        view.visible = true;
        let key =
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEB";
        let text = if screen.contains("empty") {
            String::new()
        } else {
            format!(
                "# Existing keys and restrictions are preserved\n{key} build-runner\nrestrict {key} deployment-pipeline\n{key} workstation\n"
            )
        };
        view.snapshot = Some(Arc::new(
            Registry::from_document(
                "deploy",
                "/home/deploy/.ssh/authorized_keys",
                1000,
                text.as_bytes(),
            )
            .unwrap(),
        ));
        view.shells = vec!["/bin/bash".into(), "/bin/sh".into()];
        view.shell = "/bin/bash".into();
        if screen.contains("denied") {
            view.snapshot = None;
            view.message = tr("sshmgmt-permission-denied");
        }
        view
    }
    #[cfg(feature = "capture")]
    pub fn preview_dialog(&mut self, create: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.open_editor(
            if create { Editor::Create } else { Editor::Add },
            window,
            cx,
        );
    }
}
impl Drop for SshManagementView {
    fn drop(&mut self) {
        if let Some(control) = self.operation.take() {
            control.cancel();
        }
    }
}
impl Render for SshManagementView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = crate::design::palette(cx);
        let mutable = self.available
            && !self.busy
            && self
                .snapshot
                .as_ref()
                .is_some_and(|snapshot| snapshot.writable);
        let query = self.search.read(cx).value().to_lowercase();
        let entries: Arc<Vec<KeyEntry>> = Arc::new(
            self.snapshot
                .as_ref()
                .map(|snapshot| {
                    snapshot
                        .entries
                        .iter()
                        .filter(|key| {
                            format!("{} {} {}", key.comment, key.algorithm, key.fingerprint)
                                .to_lowercase()
                                .contains(&query)
                        })
                        .cloned()
                        .collect()
                })
                .unwrap_or_default(),
        );
        let all = self
            .snapshot
            .as_ref()
            .map(|snapshot| snapshot.entries.as_slice())
            .unwrap_or_default();
        let counts = [
            all.len(),
            all.iter()
                .filter(|entry| entry.algorithm == "ssh-ed25519")
                .count(),
            all.iter()
                .filter(|entry| entry.algorithm == "ssh-rsa")
                .count(),
            all.iter()
                .filter(|entry| entry.algorithm != "ssh-ed25519" && entry.algorithm != "ssh-rsa")
                .count(),
        ];
        let auth = self
            .authentication
            .as_ref()
            .map(|info| match info.method {
                AuthenticationMethod::Password => tr("sshmgmt-password-auth"),
                AuthenticationMethod::PublicKey => tr("sshmgmt-key-auth"),
                AuthenticationMethod::Agent => tr("sshmgmt-agent-auth"),
                AuthenticationMethod::Certificate => tr("sshmgmt-certificate-auth"),
                AuthenticationMethod::KeyboardInteractive => tr("sshmgmt-interactive-auth"),
                AuthenticationMethod::None => tr("sshmgmt-no-auth"),
            })
            .unwrap_or_else(|| tr("sshmgmt-auth-unavailable"));
        let mut page = div()
            .size_full()
            .min_h_0()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_4()
            .p_5()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_xl()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(tr("sshmgmt-title")),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(gpui::rgb(colors.muted))
                            .child(auth),
                    ),
            )
            .child(
                div().flex().flex_wrap().gap_3().children(
                    counts
                        .into_iter()
                        .zip([
                            "sshmgmt-total",
                            "sshmgmt-ed25519",
                            "sshmgmt-rsa",
                            "sshmgmt-other",
                        ])
                        .map(|(count, label)| {
                            div()
                                .flex_1()
                                .min_w(px(100.))
                                .p_4()
                                .rounded_lg()
                                .bg(gpui::rgb(colors.raised))
                                .border_1()
                                .border_color(gpui::rgb(colors.border))
                                .child(div().text_2xl().child(count.to_string()))
                                .child(div().text_sm().child(tr(label)))
                        }),
                ),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_2()
                    .child(
                        div().flex_1().min_w(px(160.)).child(
                            Input::new(&self.search)
                                .aria_label(tr("sshmgmt-search"))
                                .w_full()
                                .disabled(self.busy),
                        ),
                    )
                    .child(
                        Button::new("ssh-refresh")
                            .icon(IconName::RefreshCw)
                            .tooltip(tr("sshmgmt-refresh"))
                            .accessibility_label(tr("sshmgmt-refresh"))
                            .disabled(self.busy || !self.available)
                            .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
                    )
                    .child(
                        crate::design::PrimaryAction::new("ssh-add-key")
                            .label(tr("sshmgmt-add"))
                            .disabled(!mutable)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.open_editor(Editor::Add, window, cx)
                            })),
                    )
                    .child(
                        Button::new("ssh-create-user")
                            .label(tr("sshmgmt-create-user"))
                            .disabled(
                                self.busy
                                    || !self.available
                                    || !self
                                        .snapshot
                                        .as_ref()
                                        .is_some_and(|snapshot| snapshot.can_create),
                            )
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.open_editor(Editor::Create, window, cx)
                            })),
                    ),
            )
            .when_some(self.snapshot.as_ref(), |page, snapshot| {
                page.child(
                    div()
                        .text_xs()
                        .text_color(gpui::rgb(colors.muted))
                        .child(format!("{} · {}", snapshot.account, snapshot.path)),
                )
                .when(snapshot.unrecognized > 0, |page| {
                    page.child(div().text_xs().child(format!(
                        "{} {}",
                        snapshot.unrecognized,
                        tr("sshmgmt-preserved-lines")
                    )))
                })
            })
            .when(!self.message.is_empty(), |page| {
                page.child(
                    div()
                        .p_3()
                        .rounded_md()
                        .bg(gpui::rgb(colors.raised))
                        .text_sm()
                        .child(self.message.clone()),
                )
            })
            .when(self.partial.is_some(), |page| {
                page.child(
                    Button::new("ssh-repair-user")
                        .label(tr("sshmgmt-repair"))
                        .disabled(self.busy || !self.available)
                        .on_click(cx.listener(|this, _, window, cx| {
                            if let Some(name) = this
                                .partial
                                .as_ref()
                                .and_then(|value| value["created"].as_str())
                                .map(str::to_owned)
                            {
                                this.fields
                                    .username
                                    .update(cx, |input, cx| input.set_value(name, window, cx));
                            }
                            this.open_editor(Editor::Repair, window, cx);
                        })),
                )
            });
        if entries.is_empty() {
            page = page.child(
                div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_color(gpui::rgb(colors.muted))
                    .child(tr(if self.busy {
                        "sshmgmt-working"
                    } else if self.snapshot.is_some() {
                        "sshmgmt-empty"
                    } else {
                        "sshmgmt-unavailable"
                    })),
            );
        } else {
            page = page.child(
                div()
                    .h(px(32.))
                    .px_3()
                    .flex()
                    .items_center()
                    .gap_3()
                    .bg(gpui::rgb(colors.raised))
                    .text_xs()
                    .text_color(gpui::rgb(colors.muted))
                    .child(div().flex_1().min_w_0().child(tr("sshmgmt-name-type")))
                    .when(window.viewport_size().width > px(900.), |row| {
                        row.child(div().flex_1().min_w_0().child(tr("sshmgmt-fingerprint")))
                    })
                    .when(window.viewport_size().width > px(1100.), |row| {
                        row.child(div().w(px(70.)).child(tr("sshmgmt-account")))
                    })
                    .child(div().w(px(212.)).child(tr("sshmgmt-actions"))),
            );
            page = page.child(
                uniform_list(
                    "ssh-key-registry",
                    entries.len(),
                    cx.processor(move |this, range: std::ops::Range<usize>, window, cx| {
                        range
                            .filter_map(|index| entries.get(index).cloned().map(|key| (index, key)))
                            .map(|(index, key)| {
                                let details = key.clone();
                                let copy = key.public_key.clone();
                                let export = key.clone();
                                let comment = key.clone();
                                let remove = key.clone();
                                let protected = this.active_key() == Some(key.fingerprint.as_str());
                                div()
                                    .id(("ssh-key", index))
                                    .h(px(76.))
                                    .px_3()
                                    .w_full()
                                    .flex()
                                    .items_center()
                                    .gap_3()
                                    .border_b_1()
                                    .border_color(gpui::rgb(colors.border))
                                    .hover(|style| style.bg(gpui::rgb(colors.hover)))
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .flex()
                                            .flex_col()
                                            .gap_1()
                                            .child(div().text_sm().truncate().child(
                                                if key.comment.is_empty() {
                                                    tr("sshmgmt-unnamed")
                                                } else {
                                                    key.comment
                                                },
                                            ))
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .text_color(gpui::rgb(colors.muted))
                                                    .child(format!(
                                                        "{}{}{}",
                                                        key.algorithm,
                                                        if key.restrictions.is_empty() {
                                                            ""
                                                        } else {
                                                            " · restricted"
                                                        },
                                                        if protected {
                                                            " · current login"
                                                        } else {
                                                            ""
                                                        }
                                                    )),
                                            ),
                                    )
                                    .when(window.viewport_size().width > px(900.), |row| {
                                        row.child(
                                            div()
                                                .flex_1()
                                                .min_w_0()
                                                .text_xs()
                                                .truncate()
                                                .child(key.fingerprint),
                                        )
                                    })
                                    .when(window.viewport_size().width > px(1100.), |row| {
                                        row.child(
                                            div().w(px(70.)).text_xs().truncate().child(
                                                this.snapshot
                                                    .as_ref()
                                                    .map(|snapshot| snapshot.account.clone())
                                                    .unwrap_or_default(),
                                            ),
                                        )
                                    })
                                    .child(
                                        Button::new(("ssh-key-details", index))
                                            .ghost()
                                            .icon(IconName::Info)
                                            .tooltip(tr("sshmgmt-details"))
                                            .accessibility_label(tr("sshmgmt-details"))
                                            .on_click(cx.listener(move |this, _, window, cx| {
                                                this.open_editor(
                                                    Editor::Details(details.clone()),
                                                    window,
                                                    cx,
                                                )
                                            })),
                                    )
                                    .child(
                                        Button::new(("ssh-key-copy", index))
                                            .ghost()
                                            .icon(IconName::Copy)
                                            .tooltip(tr("sshmgmt-copy"))
                                            .accessibility_label(tr("sshmgmt-copy"))
                                            .on_click(move |_, _, cx| {
                                                cx.write_to_clipboard(ClipboardItem::new_string(
                                                    copy.clone(),
                                                ))
                                            }),
                                    )
                                    .child(
                                        Button::new(("ssh-key-export", index))
                                            .ghost()
                                            .icon(gpui_kit_assets::IconName::Download)
                                            .tooltip(tr("sshmgmt-export"))
                                            .accessibility_label(tr("sshmgmt-export"))
                                            .on_click(cx.listener(move |this, _, window, cx| {
                                                this.export_key(export.clone(), window, cx)
                                            })),
                                    )
                                    .child(
                                        Button::new(("ssh-key-comment", index))
                                            .ghost()
                                            .icon(gpui_kit_assets::IconName::Pencil)
                                            .tooltip(tr("sshmgmt-edit-comment"))
                                            .accessibility_label(tr("sshmgmt-edit-comment"))
                                            .disabled(!mutable || protected)
                                            .on_click(cx.listener(move |this, _, window, cx| {
                                                this.open_editor(
                                                    Editor::Comment(comment.clone()),
                                                    window,
                                                    cx,
                                                )
                                            })),
                                    )
                                    .child(
                                        Button::new(("ssh-key-remove", index))
                                            .ghost()
                                            .icon(gpui_kit_assets::IconName::Trash)
                                            .tooltip(tr(if protected {
                                                "sshmgmt-current-key"
                                            } else {
                                                "sshmgmt-remove"
                                            }))
                                            .accessibility_label(tr("sshmgmt-remove"))
                                            .disabled(!mutable || protected)
                                            .on_click(cx.listener(move |this, _, window, cx| {
                                                this.open_editor(
                                                    Editor::Remove(remove.clone()),
                                                    window,
                                                    cx,
                                                )
                                            })),
                                    )
                            })
                            .collect::<Vec<_>>()
                    }),
                )
                .flex_1()
                .min_h_0(),
            );
        }
        page
    }
}

#[cfg(all(test, feature = "capture"))]
mod tests {
    use super::*;
    use gpui::{AppContext, TestAppContext, VisualTestContext};
    use gpui_component::Root;
    const KEY: &str =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEB";

    #[gpui::test]
    fn create_uses_exec_and_clears_password_while_disconnect_cancels(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let (commands, receive) = async_channel::bounded(4);
        let mut entity = None;
        let (_root, cx) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| SshManagementView::new(commands, 7, None, window, cx));
            entity = Some(view.clone());
            Root::new(view, window, cx)
        });
        let view = entity.unwrap();
        VisualTestContext::update(cx, |window, cx| {
            view.update(cx, |view, cx| {
                view.snapshot = Some(Arc::new(
                    Registry::from_document(
                        "deploy",
                        "/home/deploy/.ssh/authorized_keys",
                        1000,
                        b"",
                    )
                    .unwrap(),
                ));
                view.editor = Some(Editor::Create);
                view.fields
                    .username
                    .update(cx, |input, cx| input.set_value("new-user", window, cx));
                view.fields
                    .key
                    .update(cx, |input, cx| input.set_value(KEY, window, cx));
                view.fields
                    .password
                    .update(cx, |input, cx| input.set_value("secret-marker", window, cx));
                view.submit(window, cx);
                assert!(view.busy);
                assert_eq!(view.fields.password.read(cx).value(), "");
                let SshCommand::Exec { request, .. } = receive.try_recv().unwrap() else {
                    panic!("Management must use exec rather than terminal input");
                };
                assert!(request.command.starts_with("sudo -S"));
                assert!(!format!("{request:?}").contains("secret-marker"));
                let control = request.control.clone();
                view.suspend(cx);
                assert!(control.is_cancelled());
                assert!(!view.available && !view.busy && view.editor.is_none());
                view.submit(window, cx);
                assert!(receive.try_recv().is_err());
            });
        });
    }

    #[gpui::test]
    fn protected_login_key_never_sends_a_mutation(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let (commands, receive) = async_channel::bounded(4);
        let (view, cx) =
            cx.add_window_view(|window, cx| SshManagementView::new(commands, 1, None, window, cx));
        VisualTestContext::update(cx, |window, cx| {
            view.update(cx, |view, cx| {
                let snapshot =
                    Registry::from_document("deploy", "/unused", 1000, KEY.as_bytes()).unwrap();
                let key = snapshot.entries[0].clone();
                view.authentication = Some(AuthenticationInfo {
                    username: "deploy".into(),
                    method: AuthenticationMethod::PublicKey,
                    fingerprint: Some(key.fingerprint.clone()),
                });
                view.snapshot = Some(Arc::new(snapshot));
                view.editor = Some(Editor::Remove(key));
                view.submit(window, cx);
                assert!(view.editor_error.contains("current login key"));
                assert!(!view.busy);
                assert!(receive.try_recv().is_err());
            });
        });
    }
}
