//! Progressive connection setup; profiles remain the existing secret-free format.
use crate::localization::text as tr;
use gpui::{App, Context, Entity, EventEmitter, Render, Window, div, prelude::*, px};
use gpui_component::{
    ActiveTheme, Selectable,
    button::{Button, ButtonVariants},
    input::{Input, InputState},
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
    fn invalid_port_keeps_values_and_opens_its_section(cx: &mut TestAppContext) {
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
                assert!(form.errors.contains_key("form-port") && form.details);
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
    command: bool,
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
        let quick = cx.new(|cx| InputState::new(window, cx).placeholder("ssh user@host"));
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
            command: false,
            errors: BTreeMap::new(),
            message: String::new(),
        }
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
            self.details |= self.errors.contains_key("form-port");
            self.session |= self.errors.contains_key("form-keepalive");
            self.advanced |= self.session;
        }
        (self.errors.is_empty() && self.message.is_empty()).then_some(p)
    }
    fn submit(&mut self, connect: bool, cx: &mut Context<Self>) {
        if let Some(p) = self.profile(cx) {
            cx.emit(FormEvent::Save(Box::new(p), connect));
        }
        cx.notify();
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
                    _ => this.command = !this.command,
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
            .child(self.row("form-host", &self.fields.host, cx))
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
                    .child(self.row("form-port", &self.fields.port, cx))
                    .child(self.row("form-environment", &self.fields.environment, cx))
                    .child(self.row("form-tags", &self.fields.tags, cx))
            })
            .child(self.heading("form-command", self.command, cx))
            .when(self.command, |d| {
                d.child(Input::new(&self.quick).aria_label(tr("form-command")))
                    .child(
                        Button::new("parse-command")
                            .label(tr("form-fill-command"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                let command = this.quick.read(cx).value();
                                match opsssh_ssh_config::parse_command(&command) {
                                    Ok(parsed) => {
                                        let mut profile = parsed.profile;
                                        profile.id = this.profile.id;
                                        profile.favorite = this.profile.favorite;
                                        profile.name =
                                            this.fields.name.read(cx).value().to_string();
                                        profile.environment =
                                            this.fields.environment.read(cx).value().to_string();
                                        profile.tags = this
                                            .fields
                                            .tags
                                            .read(cx)
                                            .value()
                                            .split(',')
                                            .map(str::trim)
                                            .filter(|s| !s.is_empty())
                                            .map(str::to_owned)
                                            .collect();
                                        profile.tmux_session = this
                                            .protect_work
                                            .then(|| this.fields.tmux.read(cx).value().to_string());
                                        this.fields = Fields::new(&profile, window, cx);
                                        this.profile = profile;
                                        this.warnings = parsed.warnings;
                                        this.message.clear();
                                        this.errors.clear();
                                        this.details = true;
                                    }
                                    Err(error) => this.message = error,
                                }
                                cx.notify();
                            })),
                    )
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
            .child(
                div()
                    .pt_3()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .flex()
                    .gap_2()
                    .child(
                        Button::new("save-connect")
                            .primary()
                            .label(tr("save-connect"))
                            .on_click(cx.listener(|this, _, _, cx| this.submit(true, cx))),
                    )
                    .child(
                        Button::new("save")
                            .label(tr("save-server"))
                            .on_click(cx.listener(|this, _, _, cx| this.submit(false, cx))),
                    )
                    .child(
                        Button::new("cancel")
                            .ghost()
                            .label(tr("cancel"))
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(FormEvent::Cancel))),
                    ),
            )
            .max_w(px(620.))
    }
}
