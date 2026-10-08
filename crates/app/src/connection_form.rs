//! Progressive connection setup; profiles remain the existing secret-free format.
use crate::localization::text as tr;
use gpui::{App, Context, Entity, EventEmitter, Render, Window, div, prelude::*, px};
use gpui_component::{
    ActiveTheme, Icon, Selectable,
    button::{Button, ButtonVariants},
    input::{Input, InputEvent, InputState},
    menu::{DropdownMenu, PopupMenuItem},
    switch::Switch,
};
use opsssh_store::{Auth, Profile};
use std::collections::BTreeMap;

pub enum FormEvent {
    Save(Box<Profile>, bool),
    Cancel,
}

#[cfg(all(test, feature = "capture"))]
mod tests {
    use super::*;
    use gpui::{TestAppContext, VisualTestContext};

    #[gpui::test]
    fn advanced_profile_round_trips_without_opening_sections(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let original = Profile {
            id: 42,
            name: "Bastion".into(),
            host: "server.example.test".into(),
            user: "deploy".into(),
            port: 2222,
            auth: Auth::Key,
            identity_file: "/keys/id_ed25519".into(),
            certificate_file: "/keys/id-cert.pub".into(),
            proxy_jump: "jump.example.test".into(),
            identity_agent: "/run/agent".into(),
            known_hosts: "/keys/known_hosts".into(),
            identities_only: true,
            strict_host_key: true,
            environment: "Production".into(),
            tags: vec!["api".into(), "critical".into()],
            tmux_session: Some("ops".into()),
            favorite: true,
            terminal_upload_directory: Some("/srv/uploads".into()),
            ..Profile::default()
        };
        let expected = original.clone();
        let (form, cx) =
            cx.add_window_view(|window, cx| ConnectionForm::new(original, vec![], window, cx));
        VisualTestContext::update(cx, |_, cx| {
            form.update(cx, |form, cx| {
                assert!(!form.details && !form.advanced);
                assert!(form.protect_work);
                assert_eq!(form.profile(cx), Some(expected));
            })
        });
    }

    #[gpui::test]
    fn invalid_port_keeps_values_without_opening_extra_sections(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let (form, cx) = cx.add_window_view(|window, cx| {
            ConnectionForm::new(
                Profile {
                    host: "server.example.test".into(),
                    user: "deploy".into(),
                    ..Profile::default()
                },
                vec![],
                window,
                cx,
            )
        });
        VisualTestContext::update(cx, |window, cx| {
            form.update(cx, |form, cx| {
                form.fields
                    .port
                    .update(cx, |input, cx| input.set_value("99999", window, cx));
                assert!(form.profile(cx).is_none());
                assert!(form.errors.contains_key("form-port") && !form.details);
                assert_eq!(form.fields.host.read(cx).value(), "server.example.test");
                assert_eq!(form.fields.port.read(cx).value(), "99999");
            })
        });
    }

    #[gpui::test]
    fn basic_connection_generates_name_and_requires_review(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let (form, cx) = cx.add_window_view(|window, cx| {
            ConnectionForm::new(
                Profile {
                    host: "server.example.test".into(),
                    user: "deploy".into(),
                    ..Profile::default()
                },
                vec![],
                window,
                cx,
            )
        });
        VisualTestContext::update(cx, |_, cx| {
            form.update(cx, |form, cx| {
                let profile = form.profile(cx).unwrap();
                assert_eq!(profile.port, 22);
                assert_eq!(profile.name, "deploy@server.example.test");
                form.warnings.push("Unsupported option".into());
                assert!(form.profile(cx).is_none());
                assert!(!form.message.is_empty());
            })
        });
    }

    #[gpui::test]
    fn quick_command_fills_fields_without_losing_edit_metadata(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let original = Profile {
            id: 42,
            name: "My server".into(),
            environment: "dev".into(),
            host: "old.example.test".into(),
            user: "old-user".into(),
            favorite: true,
            strict_host_key: true,
            keepalive_seconds: 75,
            tags: vec!["backend".into()],
            tmux_session: Some("work".into()),
            terminal_upload_directory: Some("/opt/uploads".into()),
            ..Profile::default()
        };
        let (form, cx) =
            cx.add_window_view(|window, cx| ConnectionForm::new(original, vec![], window, cx));
        VisualTestContext::update(cx, |window, cx| {
            form.update(cx, |form, cx| {
                form.quick.update(cx, |input, cx| {
                    input.set_value(
                        "ssh -i 'my key.pem' deploy@new.example.test -p 2222",
                        window,
                        cx,
                    )
                });
                form.apply_quick(window, cx, true);
                let profile = form.profile(cx).unwrap();
                assert_eq!(
                    (profile.host.as_str(), profile.user.as_str(), profile.port),
                    ("new.example.test", "deploy", 2222)
                );
                assert_eq!(profile.identity_file, "my key.pem");
                assert_eq!(profile.auth, Auth::Key);
                assert_eq!(profile.id, 42);
                assert_eq!(profile.name, "My server");
                assert_eq!(profile.environment, "dev");
                assert_eq!(profile.tags, vec!["backend"]);
                assert!(profile.favorite && profile.strict_host_key);
                assert_eq!(profile.keepalive_seconds, 75);
                assert_eq!(profile.tmux_session.as_deref(), Some("work"));
                assert_eq!(
                    profile.terminal_upload_directory.as_deref(),
                    Some("/opt/uploads")
                );
                form.fields
                    .environment
                    .update(cx, |input, cx| input.set_value("Production", window, cx));
                assert_eq!(form.profile(cx).unwrap().environment, "Production");
                form.quick.update(cx, |input, cx| {
                    input.set_value("ssh -i 'unclosed", window, cx)
                });
                form.apply_quick(window, cx, true);
                assert!(!form.quick_error.is_empty());
                assert_eq!(form.fields.host.read(cx).value(), "new.example.test");
                assert_eq!(form.fields.environment.read(cx).value(), "Production");
                form.quick
                    .update(cx, |input, cx| input.set_value("", window, cx));
                form.apply_quick(window, cx, true);
                assert!(form.quick_error.is_empty());
                assert_eq!(form.profile(cx).unwrap().port, 2222);
            });
        });
    }

    #[gpui::test]
    fn tmux_protection_is_opt_in_and_session_name_is_validated(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let (form, cx) = cx.add_window_view(|window, cx| {
            ConnectionForm::new(
                Profile {
                    host: "server.example.test".into(),
                    user: "deploy".into(),
                    ..Profile::default()
                },
                vec![],
                window,
                cx,
            )
        });
        VisualTestContext::update(cx, |window, cx| {
            form.update(cx, |form, cx| {
                assert!(!form.protect_work);
                assert_eq!(form.profile(cx).unwrap().tmux_session, None);
                form.protect_work = true;
                assert_eq!(
                    form.profile(cx).unwrap().tmux_session.as_deref(),
                    Some("ops")
                );
                form.fields
                    .tmux
                    .update(cx, |input, cx| input.set_value("", window, cx));
                assert!(form.profile(cx).is_none());
                assert!(form.errors.contains_key("form-tmux-name"));
                form.fields
                    .tmux
                    .update(cx, |input, cx| input.set_value("a".repeat(101), window, cx));
                assert!(form.profile(cx).is_none());
                form.fields
                    .tmux
                    .update(cx, |input, cx| input.set_value("work;unsafe", window, cx));
                assert!(form.profile(cx).is_none());
                assert!(form.errors.contains_key("form-tmux-name"));
                assert!(!form.advanced);
                form.protect_work = false;
                assert_eq!(form.profile(cx).unwrap().tmux_session, None);
                assert_eq!(form.fields.tmux.read(cx).value(), "work;unsafe");
                form.protect_work = true;
                form.fields
                    .tmux
                    .update(cx, |input, cx| input.set_value("release-42", window, cx));
                assert_eq!(
                    form.profile(cx).unwrap().tmux_session.as_deref(),
                    Some("release-42")
                );
            })
        });
    }
}
struct Fields {
    name: Entity<InputState>,
    host: Entity<InputState>,
    port: Entity<InputState>,
    user: Entity<InputState>,
    environment: Entity<InputState>,
    key: Entity<InputState>,
    certificate: Entity<InputState>,
    jump: Entity<InputState>,
    proxy: Entity<InputState>,
    tmux: Entity<InputState>,
    keepalive: Entity<InputState>,
    upload_directory: Entity<InputState>,
    known_hosts: Entity<InputState>,
    tags: Entity<InputState>,
    agent: Entity<InputState>,
}
impl Fields {
    fn new(p: &Profile, window: &mut Window, cx: &mut Context<ConnectionForm>) -> Self {
        let mut input = |value: String, key: &str| {
            cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(tr(key))
                    .default_value(value)
            })
        };
        Self {
            name: input(p.name.clone(), "form-name"),
            host: input(p.host.clone(), "form-host-placeholder"),
            port: input(p.port.to_string(), "form-port"),
            user: input(p.user.clone(), "form-user"),
            environment: input(p.environment.clone(), "form-environment"),
            key: input(p.identity_file.clone(), "form-key"),
            certificate: input(p.certificate_file.clone(), "form-certificate"),
            jump: input(p.proxy_jump.clone(), "form-jump"),
            proxy: input(p.proxy_command.clone(), "form-proxy"),
            tmux: input(
                p.tmux_session.clone().unwrap_or_else(|| "ops".into()),
                "form-tmux-name",
            ),
            keepalive: input(p.keepalive_seconds.to_string(), "form-keepalive"),
            upload_directory: input(
                p.terminal_upload_directory.clone().unwrap_or_default(),
                "form-upload-directory",
            ),
            known_hosts: input(p.known_hosts.clone(), "form-known-hosts"),
            tags: input(p.tags.join(", "), "form-tags"),
            agent: input(p.identity_agent.clone(), "form-agent-socket"),
        }
    }
}
pub struct ConnectionForm {
    profile: Profile,
    fields: Fields,
    quick: Entity<InputState>,
    warnings: Vec<String>,
    advanced: bool,
    details: bool,
    routing: bool,
    security: bool,
    session: bool,
    protect_work: bool,
    quick_generation: u64,
    quick_error: String,
    quick_applied: String,
    custom_environment: bool,
    errors: BTreeMap<&'static str, String>,
    message: String,
}
impl EventEmitter<FormEvent> for ConnectionForm {}
impl ConnectionForm {
    pub fn new(
        profile: Profile,
        warnings: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let fields = Fields::new(&profile, window, cx);
        cx.subscribe(&fields.environment, |_, _, event, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        })
        .detach();
        let quick = cx.new(|cx| InputState::new(window, cx).placeholder("ssh user@host"));
        cx.subscribe_in(&quick, window, |this, _, event, window, cx| match event {
            InputEvent::Change => {
                this.quick_generation += 1;
                let generation = this.quick_generation;
                this.quick_error.clear();
                let timer = cx
                    .background_executor()
                    .timer(std::time::Duration::from_millis(450));
                cx.spawn_in(window, async move |this, cx| {
                    timer.await;
                    let _ = this.update_in(cx, |this, window, cx| {
                        if this.quick_generation == generation {
                            this.apply_quick(window, cx, false);
                        }
                    });
                })
                .detach();
            }
            InputEvent::PressEnter { .. } => this.apply_quick(window, cx, true),
            _ => {}
        })
        .detach();
        let protect_work = profile.tmux_session.is_some();
        Self {
            profile,
            fields,
            quick,
            warnings,
            advanced: false,
            details: false,
            routing: false,
            security: false,
            session: false,
            protect_work,
            quick_generation: 0,
            quick_error: String::new(),
            quick_applied: String::new(),
            custom_environment: false,
            errors: BTreeMap::new(),
            message: String::new(),
        }
    }
    #[cfg(feature = "capture")]
    pub fn preview_quick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.quick.update(cx, |input, cx| {
            input.set_value(
                "ssh -i ~/.ssh/mykey.pem user@192.168.1.1 -p 2222",
                window,
                cx,
            );
            cx.emit(InputEvent::Change);
        });
    }
    pub fn footer(form: &Entity<Self>, cx: &App) -> gpui::Div {
        let connect = form.downgrade();
        let save = form.downgrade();
        let cancel = form.downgrade();
        div()
            .pt_3()
            .mt_2()
            .border_t_1()
            .border_color(cx.theme().border)
            .flex()
            .gap_2()
            .child(
                crate::design::PrimaryAction::new("save-connect")
                    .label(tr("save-connect"))
                    .on_click(move |_, window, cx| {
                        let _ = connect.update(cx, |form, cx| form.submit(true, window, cx));
                    }),
            )
            .child(
                Button::new("save")
                    .label(tr("save-server"))
                    .on_click(move |_, window, cx| {
                        let _ = save.update(cx, |form, cx| form.submit(false, window, cx));
                    }),
            )
            .child(
                Button::new("cancel")
                    .ghost()
                    .label(tr("cancel"))
                    .on_click(move |_, _, cx| {
                        let _ = cancel.update(cx, |_, cx| cx.emit(FormEvent::Cancel));
                    }),
            )
    }
    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.fields.host.update(cx, |s, cx| s.focus(window, cx));
    }
    pub fn error(&mut self, error: String, cx: &mut Context<Self>) {
        self.message = error;
        cx.notify();
    }
    fn profile(&mut self, cx: &App) -> Option<Profile> {
        self.errors.clear();
        self.message.clear();
        let value = |input: &Entity<InputState>| input.read(cx).value().to_string();
        let mut p = self.profile.clone();
        p.host = value(&self.fields.host).trim().into();
        p.user = value(&self.fields.user).trim().into();
        p.name = value(&self.fields.name).trim().into();
        if p.name.is_empty() {
            p.name = format!("{}@{}", p.user, p.host);
        }
        if p.host.is_empty()
            || p.host.starts_with('-')
            || p.host.chars().any(|c| c.is_control() || c.is_whitespace())
        {
            self.errors.insert("form-host", tr("form-invalid-host"));
        }
        if p.user.is_empty() || p.user.chars().any(char::is_control) {
            self.errors.insert("form-user", tr("form-invalid-user"));
        }
        match value(&self.fields.port).parse::<u16>() {
            Ok(port) if port > 0 => p.port = port,
            _ => {
                self.errors.insert("form-port", tr("form-invalid-port"));
            }
        }
        p.terminal_upload_directory =
            Some(value(&self.fields.upload_directory)).filter(|path| !path.is_empty());
        p.environment = value(&self.fields.environment);
        p.identity_file = value(&self.fields.key);
        if p.auth == Auth::Key && p.identity_file.trim().is_empty() {
            self.errors.insert("form-key", tr("form-invalid-key"));
        }
        p.certificate_file = value(&self.fields.certificate);
        p.proxy_jump = value(&self.fields.jump);
        let proxy = value(&self.fields.proxy);
        if proxy != p.proxy_command {
            p.proxy_review_required = true;
        }
        p.proxy_command = proxy;
        let tmux = value(&self.fields.tmux).trim().to_owned();
        p.tmux_session = self.protect_work.then_some(tmux.clone());
        if self.protect_work
            && (tmux.is_empty()
                || tmux.len() > 100
                || !tmux
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'))
        {
            self.errors
                .insert("form-tmux-name", tr("form-invalid-tmux"));
        }
        match value(&self.fields.keepalive).parse() {
            Ok(seconds) => p.keepalive_seconds = seconds,
            _ => {
                self.errors
                    .insert("form-keepalive", tr("form-invalid-keepalive"));
            }
        }
        p.known_hosts = value(&self.fields.known_hosts);
        p.identity_agent = value(&self.fields.agent);
        p.tags = value(&self.fields.tags)
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect();
        if !self.warnings.is_empty() {
            self.message = tr("form-review-options");
        }
        if let Err(error) = p.validate()
            && self.errors.is_empty()
        {
            self.message = error.to_string();
        }
        if !self.errors.is_empty() {
            self.session |= self.errors.contains_key("form-keepalive");
            self.advanced |= self.session;
        }
        (self.errors.is_empty() && self.message.is_empty()).then_some(p)
    }
    fn submit(&mut self, connect: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.apply_quick(window, cx, false);
        if !self.quick_error.is_empty() {
            return;
        }
        if let Some(p) = self.profile(cx) {
            cx.emit(FormEvent::Save(Box::new(p), connect));
        }
        cx.notify();
    }
    fn apply_quick(&mut self, window: &mut Window, cx: &mut Context<Self>, force: bool) {
        self.quick_generation += 1;
        let command = self.quick.read(cx).value().trim().to_owned();
        if command.is_empty() {
            self.quick_error.clear();
            self.quick_applied.clear();
            cx.notify();
            return;
        }
        if !force && command == self.quick_applied {
            return;
        }
        let parsed = match opsssh_ssh_config::parse_command(&command) {
            Ok(parsed) => parsed,
            Err(error) => {
                self.quick_error = error;
                cx.notify();
                return;
            }
        };
        let p = parsed.profile;
        self.fields
            .host
            .update(cx, |input, cx| input.set_value(p.host, window, cx));
        self.fields.port.update(cx, |input, cx| {
            input.set_value(p.port.to_string(), window, cx)
        });
        if !p.user.is_empty() {
            self.fields
                .user
                .update(cx, |input, cx| input.set_value(p.user, window, cx));
        }
        for (option, field, value) in [
            ("identityfile", &self.fields.key, p.identity_file),
            (
                "certificatefile",
                &self.fields.certificate,
                p.certificate_file,
            ),
            ("proxyjump", &self.fields.jump, p.proxy_jump),
            ("proxycommand", &self.fields.proxy, p.proxy_command),
            (
                "serveraliveinterval",
                &self.fields.keepalive,
                p.keepalive_seconds.to_string(),
            ),
            (
                "userknownhostsfile",
                &self.fields.known_hosts,
                p.known_hosts,
            ),
            ("identityagent", &self.fields.agent, p.identity_agent),
        ] {
            if parsed.options.contains(option) {
                field.update(cx, |input, cx| input.set_value(value, window, cx));
            }
        }
        if parsed.options.contains("identityfile") {
            self.profile.auth = Auth::Key;
        }
        if parsed.options.contains("proxycommand") {
            self.profile.proxy_review_required = true;
        }
        if parsed.options.contains("identitiesonly") {
            self.profile.identities_only = p.identities_only;
        }
        if parsed.options.contains("stricthostkeychecking") {
            self.profile.strict_host_key = p.strict_host_key;
        }
        if parsed.options.contains("forwardagent") {
            self.profile.forward_agent = p.forward_agent;
        }
        self.warnings = parsed.warnings;
        self.quick_applied = command;
        self.quick_error.clear();
        self.message.clear();
        self.errors.clear();
        cx.notify();
    }
    fn environment(&self, cx: &mut Context<Self>) -> gpui::Div {
        let current = self.fields.environment.read(cx).value().to_string();
        let weak = cx.entity().downgrade();
        let selected = current.clone();
        div()
            .flex()
            .flex_col()
            .gap_1()
            .w_full()
            .child(div().text_sm().child(tr("form-environment")))
            .child(
                Button::new("environment-select")
                    .w_full()
                    .icon(gpui_kit_assets::IconName::Box)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(if current.is_empty() {
                                tr("form-select-environment")
                            } else {
                                current
                            }),
                    )
                    .child(Icon::new(gpui_kit_assets::IconName::ChevronDown))
                    .accessibility_label(tr("form-environment"))
                    .dropdown_menu(move |mut menu, window, _| {
                        menu = menu.min_w(px(580.).min(window.viewport_size().width - px(112.)));
                        let mut values = vec![
                            String::new(),
                            "Production".into(),
                            "Staging".into(),
                            "Development".into(),
                        ];
                        if !values.contains(&selected) {
                            values.push(selected.clone());
                        }
                        for value in values {
                            let weak = weak.clone();
                            let checked = value == selected;
                            menu = menu.item(
                                PopupMenuItem::new(if value.is_empty() {
                                    tr("form-no-environment")
                                } else {
                                    value.clone()
                                })
                                .checked(checked)
                                .on_click(move |_, window, cx| {
                                    let _ = weak.update(cx, |this, cx| {
                                        this.fields.environment.update(cx, |input, cx| {
                                            input.set_value(value.clone(), window, cx)
                                        });
                                        this.custom_environment = false;
                                        cx.notify();
                                    });
                                }),
                            );
                        }
                        let weak = weak.clone();
                        menu.item(PopupMenuItem::new(tr("form-custom-environment")).on_click(
                            move |_, window, cx| {
                                let _ = weak.update(cx, |this, cx| {
                                    this.custom_environment = true;
                                    this.fields
                                        .environment
                                        .update(cx, |input, cx| input.focus(window, cx));
                                    cx.notify();
                                });
                            },
                        ))
                    }),
            )
            .when(self.custom_environment, |d| {
                d.child(
                    Input::new(&self.fields.environment).aria_label(tr("form-custom-environment")),
                )
            })
    }
    fn row(&self, key: &'static str, field: &Entity<InputState>, cx: &App) -> gpui::Div {
        div()
            .flex()
            .flex_col()
            .gap_1()
            .w_full()
            .child(div().text_sm().child(tr(key)))
            .child(Input::new(field).aria_label(tr(key)).w_full())
            .when_some(self.errors.get(key), |d, error| {
                d.child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().danger)
                        .child(error.clone()),
                )
            })
    }
    fn heading(&self, id: &'static str, open: bool, cx: &mut Context<Self>) -> Button {
        Button::new(id)
            .ghost()
            .accessibility_label(tr(id))
            .w_full()
            .child(
                div()
                    .w_full()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(tr(id))
                    .child(gpui_component::Icon::new(if open {
                        gpui_component::IconName::ChevronDown
                    } else {
                        gpui_component::IconName::ChevronRight
                    })),
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                match id {
                    "form-advanced" => this.advanced = !this.advanced,
                    "form-details" => this.details = !this.details,
                    "form-routing" => this.routing = !this.routing,
                    "form-security" => this.security = !this.security,
                    "form-session" => this.session = !this.session,
                    _ => {}
                }
                cx.notify();
            }))
    }
    fn browse(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let picker = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(tr("form-browse-key").into()),
        });
        cx.spawn_in(window, async move |this, cx| match picker.await {
            Ok(Ok(Some(paths))) => {
                if let Some(path) = paths.first() {
                    let _ = this.update_in(cx, |this, window, cx| {
                        this.fields.key.update(cx, |s, cx| {
                            s.set_value(path.to_string_lossy().to_string(), window, cx)
                        });
                        cx.notify();
                    });
                }
            }
            Ok(Err(error)) => {
                let _ = this.update(cx, |this, cx| this.error(error.to_string(), cx));
            }
            _ => {}
        })
        .detach();
    }
}
impl Render for ConnectionForm {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let auth = self.profile.auth.clone();
        div()
            .flex()
            .flex_col()
            .gap_4()
            .min_w_0()
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(tr("form-intro")),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(div().text_sm().child(tr("form-quick-connect")))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(div().flex_1().min_w_0().child(
                                Input::new(&self.quick).aria_label(tr("form-quick-connect")),
                            ))
                            .child(
                                Button::new("parse-command")
                                    .label(tr("form-fill-command"))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.apply_quick(window, cx, true)
                                    })),
                            ),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(tr("form-quick-help")),
                    )
                    .when(!self.quick_error.is_empty(), |d| {
                        d.child(
                            div()
                                .text_sm()
                                .text_color(cx.theme().danger)
                                .child(self.quick_error.clone()),
                        )
                    }),
            )
            .when(!self.message.is_empty(), |d| {
                d.child(
                    div()
                        .p_3()
                        .rounded_md()
                        .bg(cx.theme().muted)
                        .text_color(cx.theme().danger)
                        .child(self.message.clone()),
                )
            })
            .child(
                div()
                    .flex()
                    .items_start()
                    .gap_3()
                    .w_full()
                    .child(div().flex_1().min_w_0().child(self.row(
                        "form-host",
                        &self.fields.host,
                        cx,
                    )))
                    .child(div().w(px(112.)).flex_shrink_0().child(self.row(
                        "form-port",
                        &self.fields.port,
                        cx,
                    ))),
            )
            .child(self.row("form-user", &self.fields.user, cx))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(tr("form-auth"))
                    .child(
                        div().flex().gap_2().children(
                            [
                                (Auth::Agent, "auth-agent"),
                                (Auth::Password, "auth-password"),
                                (Auth::Key, "auth-key"),
                            ]
                            .into_iter()
                            .enumerate()
                            .map(|(i, (method, key))| {
                                Button::new(("auth", i))
                                    .selected(auth == method)
                                    .label(tr(key))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.profile.auth = method.clone();
                                        cx.notify();
                                    }))
                            }),
                        ),
                    ),
            )
            .when(auth == Auth::Key, |d| {
                d.child(self.row("form-key", &self.fields.key, cx)).child(
                    Button::new("browse-key")
                        .label(tr("form-browse-key"))
                        .on_click(cx.listener(|this, _, window, cx| this.browse(window, cx))),
                )
            })
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(tr(match auth {
                        Auth::Agent => "auth-agent-help",
                        Auth::Password => "auth-password-help",
                        Auth::Key => "auth-key-help",
                    })),
            )
            .child(self.environment(cx))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        Switch::new("protect-work")
                            .checked(self.protect_work)
                            .label(tr("form-protect-work"))
                            .on_click(cx.listener(|this, checked, _, cx| {
                                this.protect_work = *checked;
                                this.errors.remove("form-tmux-name");
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(tr("form-protect-work-help")),
                    )
                    .when(self.protect_work, |d| {
                        d.child(self.row("form-tmux-name", &self.fields.tmux, cx))
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(tr("form-tmux-name-help")),
                            )
                    }),
            )
            .child(self.heading("form-details", self.details, cx))
            .when(self.details, |d| {
                d.child(self.row("form-name", &self.fields.name, cx))
                    .child(self.row("form-tags", &self.fields.tags, cx))
            })
            .when(!self.warnings.is_empty(), |d| {
                d.child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().warning)
                        .child(self.warnings.join("; ")),
                )
                .child(
                    Button::new("omit-unsupported")
                        .label(tr("form-omit-options"))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.warnings.clear();
                            this.profile.forward_agent = false;
                            this.profile.legacy = false;
                            cx.notify();
                        })),
                )
            })
            .child(self.heading("form-advanced", self.advanced, cx))
            .when(self.advanced, |d| {
                d.child(self.heading("form-routing", self.routing, cx))
                    .when(self.routing, |d| {
                        d.child(self.row("form-jump", &self.fields.jump, cx))
                            .child(self.row("form-proxy", &self.fields.proxy, cx))
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(tr("form-proxy-help")),
                            )
                            .child(
                                Button::new("approve-proxy")
                                    .label(tr("form-approve-proxy"))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.profile.proxy_command =
                                            this.fields.proxy.read(cx).value().to_string();
                                        this.profile.proxy_review_required = false;
                                        cx.notify();
                                    })),
                            )
                    })
                    .child(self.heading("form-security", self.security, cx))
                    .when(self.security, |d| {
                        d.child(self.row("form-certificate", &self.fields.certificate, cx))
                            .child(self.row("form-known-hosts", &self.fields.known_hosts, cx))
                            .child(self.row("form-agent-socket", &self.fields.agent, cx))
                            .child(
                                Button::new("identities-only")
                                    .selected(self.profile.identities_only)
                                    .label(tr("form-identities-only"))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.profile.identities_only =
                                            !this.profile.identities_only;
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("strict-hosts")
                                    .selected(self.profile.strict_host_key)
                                    .label(tr("form-strict-hosts"))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.profile.strict_host_key =
                                            !this.profile.strict_host_key;
                                        cx.notify();
                                    })),
                            )
                    })
                    .child(self.heading("form-session", self.session, cx))
                    .when(self.session, |d| {
                        d.child(self.row("form-keepalive", &self.fields.keepalive, cx))
                            .child(self.row(
                                "form-upload-directory",
                                &self.fields.upload_directory,
                                cx,
                            ))
                    })
            })
            .max_w(px(620.))
    }
}
