use crate::files::{FilePane, FilePaneEvent};
use crate::localization::text as tr;
use gpui::{
    App, Bounds, Context, Entity, FocusHandle, KeyBinding, Render, Window, WindowBounds,
    WindowOptions, actions, div, prelude::*, px, rgb, size,
};
use gpui_component::{
    Root, Theme, ThemeMode,
    button::{Button, ButtonVariants},
    input::{Input, InputEvent, InputState},
};
use opsssh_store::{Auth, Profile, Store};
use opsssh_term_view::{TerminalView, TerminalViewEvent};
use std::{path::PathBuf, time::Instant};
actions!(
    opsssh,
    [OpenLocalTerminal, GoHome, NewServer, CloseTab, Quit]
);
struct Tab {
    name: String,
    terminal: Entity<TerminalView>,
    color: u32,
    files: Option<Entity<FilePane>>,
    files_visible: bool,
    follow: bool,
    followed_directory: Option<String>,
    file_generation: u64,
}
struct Editor {
    profile: Profile,
    fields: Vec<Entity<InputState>>,
    quick: Entity<InputState>,
    warnings: Vec<String>,
}
const LABELS: [&str; 16] = [
    "Name",
    "Host / IP",
    "Port",
    "Username",
    "Environment",
    "Key file",
    "Certificate file",
    "Jump hosts (comma separated)",
    "Proxy command (program and arguments)",
    "tmux session (empty for plain shell)",
    "Keepalive seconds",
    "Known hosts file (empty for OpenSSH default)",
    "Tags (comma separated)",
    "Identity agent socket (empty for default)",
    "Identities only (yes / no)",
    "Strict host keys (yes / ask)",
];
struct Workspace {
    home_focus: FocusHandle,
    search: Entity<InputState>,
    store: Store,
    path: PathBuf,
    tabs: Vec<Tab>,
    active: Option<usize>,
    editor: Option<Editor>,
    message: String,
    filter: String,
    config: Vec<(Profile, Vec<String>)>,
    launched: Instant,
    first_frame_recorded: bool,
    load_failed: bool,
}
pub fn run(open_terminal: bool) {
    run_internal(open_terminal, None);
}
#[cfg(feature = "capture")]
pub fn capture(open_terminal: bool, path: PathBuf) {
    run_internal(open_terminal, Some(path));
}
fn run_internal(open_terminal: bool, capture_path: Option<PathBuf>) {
    let launched = Instant::now();
    opsssh_platform::application().run(move|cx:&mut App|{
    gpui_component::init(cx);Theme::change(ThemeMode::Dark,None,cx);
    cx.bind_keys([KeyBinding::new("ctrl-enter",OpenLocalTerminal,Some("OpsSSHHome")),KeyBinding::new("cmd-enter",OpenLocalTerminal,Some("OpsSSHHome")),KeyBinding::new("ctrl-k",GoHome,Some("OpsSSH")),KeyBinding::new("cmd-k",GoHome,Some("OpsSSH")),KeyBinding::new("ctrl-n",NewServer,Some("OpsSSH")),KeyBinding::new("cmd-n",NewServer,Some("OpsSSH")),KeyBinding::new("ctrl-w",CloseTab,Some("OpsSSH")),KeyBinding::new("cmd-w",CloseTab,Some("OpsSSH")),KeyBinding::new("ctrl-shift-h",GoHome,Some("OpsSSH")),KeyBinding::new("cmd-shift-h",GoHome,Some("OpsSSH")),KeyBinding::new("ctrl-shift-q",Quit,Some("OpsSSH")),KeyBinding::new("cmd-q",Quit,Some("OpsSSH"))]);
    cx.on_window_closed(|cx,_|{if cx.windows().is_empty(){cx.quit();}}).detach();
    let bounds=Bounds::centered(None,size(px(1200.),px(820.)),cx);
    let result=cx.open_window(WindowOptions{window_bounds:Some(WindowBounds::Windowed(bounds)),..WindowOptions::default()},move|window,cx|{
        window.set_window_title("OpsSSH");
        let workspace=cx.new(|cx: &mut Context<Workspace>|{
            let home_focus=cx.focus_handle();let search=cx.new(|cx|InputState::new(window,cx).placeholder("Search name, host, user, environment or tags"));
            cx.subscribe_in(&search,window,|this,_,event,window,cx|{match event {InputEvent::PressEnter{..}=>{let query=this.search.read(cx).value();if let Some(p)=this.visible_profiles(&query).first().cloned(){this.connect(p,window,cx);}},InputEvent::Change=>cx.notify(),_=>{}}}).detach();
            let (path,mut message)=match opsssh_platform::app_data_dir(){Ok(dir)=>(dir.join("servers.toml"),String::new()),Err(error)=>(PathBuf::new(),format!("Cannot locate settings folder: {error}"))};
            let mut load_failed=false;
            let store=if path.as_os_str().is_empty(){load_failed=true;Store::default()}else{match Store::load(&path){Ok(store)=>store,Err(error)=>{message=format!("Cannot load servers. Fix the settings file before saving: {error}");load_failed=true;Store::default()}}};
            let mut workspace=Workspace{home_focus,search,store,path,tabs:vec![],active:None,editor:None,message,filter:"All servers".into(),config:vec![],launched,first_frame_recorded:false,load_failed};
            workspace.reload_config();
            if open_terminal{workspace.open_local(&OpenLocalTerminal,window,cx);}else{workspace.search.update(cx,|state,cx|state.focus(window,cx));}
            #[cfg(feature="capture")]
            if let Some(path)=capture_path{let timer=cx.background_executor().timer(std::time::Duration::from_secs(2));cx.spawn_in(window,async move|this,cx|{timer.await;let _=this.update_in(cx,|_,window,cx|{match window.render_to_image().and_then(|image|image.save(&path).map_err(Into::into)){Ok(())=>eprintln!("Saved rendered frame to {}",path.display()),Err(error)=>eprintln!("Frame export failed: {error}")};cx.quit();});}).detach();}
            #[cfg(not(feature="capture"))]let _=capture_path;
            workspace
        });
        cx.new(|cx|Root::new(workspace,window,cx))
    });if let Err(error)=result{eprintln!("Could not open OpsSSH: {error}");cx.quit();return;}cx.activate(true);
});
}
impl Workspace {
    fn reload_config(&mut self) {
        self.config.clear();
        if let Ok(home) = opsssh_platform::home_dir() {
            let path = home.join(".ssh/config");
            if path.exists() {
                match opsssh_ssh_config::ParsedConfig::read(&path, &home) {
                    Ok(config) => {
                        self.config = config
                            .aliases()
                            .iter()
                            .map(|alias| {
                                let mut resolved = config.resolve(alias);
                                if resolved.profile.user.is_empty() {
                                    resolved.profile.user =
                                        opsssh_platform::default_username().unwrap_or_default();
                                }
                                (resolved.profile, resolved.warnings)
                            })
                            .collect();
                    }
                    Err(error) => self.message = format!("Could not read SSH config: {error}"),
                }
            }
        }
    }
    fn persist(&mut self) -> bool {
        if self.load_failed {
            self.message =
                "Settings could not be loaded; fix the file and restart before saving".into();
            return false;
        }
        match self.store.save(&self.path) {
            Ok(()) => true,
            Err(error) => {
                self.message = format!("Could not save servers: {error}");
                false
            }
        }
    }
    fn open_local(&mut self, _: &OpenLocalTerminal, window: &mut Window, cx: &mut Context<Self>) {
        self.tabs.push(Tab {
            name: "Local".into(),
            terminal: cx.new(|cx| TerminalView::new(window, cx)),
            color: 0x8abdaf,
            files: None,
            files_visible: false,
            follow: true,
            followed_directory: None,
            file_generation: 0,
        });
        self.active = Some(self.tabs.len() - 1);
        self.editor = None;
        cx.notify();
    }
    fn home(&mut self, _: &GoHome, window: &mut Window, cx: &mut Context<Self>) {
        self.active = None;
        self.editor = None;
        self.reload_config();
        self.search.update(cx, |state, cx| state.focus(window, cx));
        cx.notify();
    }
    fn new_server(&mut self, _: &NewServer, window: &mut Window, cx: &mut Context<Self>) {
        self.edit(
            Profile {
                user: opsssh_platform::default_username().unwrap_or_default(),
                ..Profile::default()
            },
            window,
            cx,
        );
    }
    fn close_tab(&mut self, _: &CloseTab, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(index) = self.active {
            self.tabs.remove(index);
            self.active = if self.tabs.is_empty() {
                None
            } else {
                Some(index.min(self.tabs.len() - 1))
            };
        }
        if let Some(index) = self.active {
            self.tabs[index]
                .terminal
                .update(cx, |terminal, cx| terminal.focus(window, cx));
        } else {
            self.search.update(cx, |s, cx| s.focus(window, cx));
        }
        cx.notify();
    }
    fn quit(&mut self, _: &Quit, _: &mut Window, cx: &mut Context<Self>) {
        cx.quit();
    }
    fn edit(&mut self, profile: Profile, window: &mut Window, cx: &mut Context<Self>) {
        let values = [
            profile.name.clone(),
            profile.host.clone(),
            profile.port.to_string(),
            profile.user.clone(),
            profile.environment.clone(),
            profile.identity_file.clone(),
            profile.certificate_file.clone(),
            profile.proxy_jump.clone(),
            profile.proxy_command.clone(),
            profile.tmux_session.clone().unwrap_or_default(),
            profile.keepalive_seconds.to_string(),
            profile.known_hosts.clone(),
            profile.tags.join(", "),
            profile.identity_agent.clone(),
            if profile.identities_only { "yes" } else { "no" }.into(),
            if profile.strict_host_key {
                "yes"
            } else {
                "ask"
            }
            .into(),
        ];
        let fields = values
            .into_iter()
            .zip(LABELS)
            .map(|(value, label)| {
                cx.new(|cx| {
                    InputState::new(window, cx)
                        .placeholder(label)
                        .default_value(value)
                })
            })
            .collect();
        let quick = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("ssh -i ~/.ssh/key -p 2222 -J bastion user@host")
        });
        let warnings = if profile.id == 0 {
            self.config
                .iter()
                .find(|(p, _)| p.name == profile.name)
                .map(|(_, w)| w.clone())
                .unwrap_or_default()
        } else {
            vec![]
        };
        self.editor = Some(Editor {
            profile,
            fields,
            quick,
            warnings,
        });
        self.active = None;
        self.message.clear();
        cx.notify();
    }
    fn read_editor(&self, cx: &App) -> Result<Profile, String> {
        let editor = self.editor.as_ref().ok_or("No server form")?;
        if !editor.warnings.is_empty() {
            return Err(
                "Review unsupported options and explicitly choose whether to omit them".into(),
            );
        }
        let values: Vec<String> = editor
            .fields
            .iter()
            .map(|field| field.read(cx).value().to_string())
            .collect();
        let mut p = editor.profile.clone();
        p.name = values[0].clone();
        p.host = values[1].trim().into();
        p.port = values[2].parse().map_err(|_| "Port must be 1-65535")?;
        p.user = values[3].trim().into();
        p.environment = values[4].clone();
        p.identity_file = values[5].clone();
        p.certificate_file = values[6].clone();
        p.proxy_jump = values[7].clone();
        if p.proxy_command != values[8] {
            p.proxy_review_required = true;
        }
        p.proxy_command = values[8].clone();
        p.tmux_session = (!values[9].is_empty()).then(|| values[9].clone());
        p.keepalive_seconds = values[10]
            .parse()
            .map_err(|_| "Keepalive must be a number")?;
        p.known_hosts = values[11].clone();
        p.identity_agent = values[13].clone();
        p.identities_only = match values[14].trim() {
            "yes" => true,
            "no" => false,
            _ => return Err("Identities only must be yes or no".into()),
        };
        p.strict_host_key = match values[15].trim() {
            "yes" => true,
            "ask" => false,
            _ => return Err("Strict host keys must be yes or ask".into()),
        };
        p.tags = values[12]
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect();
        p.validate().map_err(|e| e.to_string())?;
        Ok(p)
    }
    fn save_editor(&mut self, connect: bool, window: &mut Window, cx: &mut Context<Self>) {
        match self.read_editor(cx) {
            Ok(mut profile) => {
                let old = self.store.clone();
                match self.store.upsert(profile.clone()) {
                    Ok(id) => {
                        profile.id = id;
                        if self.persist() {
                            self.editor = None;
                            if connect {
                                self.connect(profile, window, cx);
                            } else {
                                self.message = "Server saved".into();
                            }
                        } else {
                            self.store = old;
                        }
                    }
                    Err(error) => self.message = error.to_string(),
                }
            }
            Err(error) => self.message = error,
        }
        cx.notify();
    }
    fn connect(&mut self, profile: Profile, window: &mut Window, cx: &mut Context<Self>) {
        if profile.id == 0
            && let Some((_, warnings)) = self.config.iter().find(|(p, _)| p.name == profile.name)
            && !warnings.is_empty()
        {
            self.message = format!(
                "Review unsupported config options using Customize: {}",
                warnings.join("; ")
            );
            cx.notify();
            return;
        }
        match crate::connections::options(&profile, &self.store) {
            Ok(options) => {
                let terminal = cx.new(|cx| TerminalView::connect(options, window, cx));
                cx.subscribe_in(&terminal, window, |this, terminal, event, window, cx| {
                    let Some(pane) = this.ensure_files(terminal, window, cx) else {
                        return;
                    };
                    match event {
                        TerminalViewEvent::UploadFiles(paths) => {
                            let view = terminal.read(cx);
                            let target = if view.at_shell_prompt() {
                                view.current_directory().map(str::to_owned)
                            } else {
                                None
                            };
                            let insert = target.is_none();
                            pane.update(cx, |pane, cx| {
                                pane.upload_files(paths.clone(), target, insert, cx)
                            });
                        }
                        TerminalViewEvent::UploadClipboard(item) => {
                            pane.update(cx, |pane, cx| pane.upload_clipboard(item.clone(), cx))
                        }
                    }
                })
                .detach();
                cx.observe_in(&terminal, window, |this, terminal, _, cx| {
                    let directory = terminal.read(cx).current_directory().map(str::to_owned);
                    let generation = terminal.read(cx).connection_generation();
                    if let Some(tab) = this.tabs.iter_mut().find(|tab| tab.terminal == terminal) {
                        if tab.files.is_some() && tab.file_generation != generation {
                            tab.files = None;
                            tab.files_visible = false;
                            tab.followed_directory = None;
                        }
                        if tab.follow
                            && tab.followed_directory != directory
                            && let (Some(directory), Some(files)) = (&directory, &tab.files)
                        {
                            files.update(cx, |pane, cx| {
                                pane.follow_directory(directory.clone(), cx)
                            });
                            tab.followed_directory = Some(directory.clone());
                        }
                    }
                    cx.notify();
                })
                .detach();
                self.tabs.push(Tab {
                    name: if profile.name.is_empty() {
                        profile.host.clone()
                    } else {
                        profile.name.clone()
                    },
                    terminal,
                    color: environment_color(&profile.environment),
                    files: None,
                    files_visible: false,
                    follow: true,
                    followed_directory: None,
                    file_generation: 0,
                });
                self.active = Some(self.tabs.len() - 1);
                self.editor = None;
                self.message.clear();
            }
            Err(error) => self.message = error,
        }
        cx.notify();
    }
    fn ensure_files(
        &mut self,
        terminal: &Entity<TerminalView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<FilePane>> {
        let index = self.tabs.iter().position(|tab| &tab.terminal == terminal)?;
        self.tabs[index].files_visible = true;
        if let Some(files) = &self.tabs[index].files {
            return Some(files.clone());
        }
        let view = terminal.read(cx);
        let (Some(commands), Some(fingerprint)) = (
            view.ssh_commands(),
            view.verified_host_key().map(str::to_owned),
        ) else {
            self.message = "Connect and authenticate before opening files".into();
            cx.notify();
            return None;
        };
        let directory = view.current_directory().unwrap_or(".").to_owned();
        let generation = view.connection_generation();
        let files =
            cx.new(|cx| FilePane::new(commands, fingerprint, directory.clone(), window, cx));
        let terminal = terminal.downgrade();
        cx.subscribe(&files, move |_, _, event, cx| {
            let FilePaneEvent::InsertText(text) = event;
            let _ = terminal.update(cx, |terminal, cx| {
                if terminal.connection_generation() == generation {
                    terminal.insert_text(text, cx);
                }
            });
        })
        .detach();
        self.tabs[index].files = Some(files.clone());
        self.tabs[index].file_generation = generation;
        self.tabs[index].followed_directory = Some(directory);
        cx.notify();
        Some(files)
    }
    fn visible_profiles(&self, query: &str) -> Vec<Profile> {
        let profiles: Vec<Profile> = if self.filter == "From SSH config" {
            self.config.iter().map(|(p, _)| p.clone()).collect()
        } else {
            self.store.servers.clone()
        };
        profiles
            .into_iter()
            .filter(|p| {
                p.matches(query)
                    && match self.filter.as_str() {
                        "All servers" | "From SSH config" => true,
                        "Favourites" => p.favorite,
                        environment => p.environment == environment,
                    }
            })
            .collect()
    }
    fn import(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let picker = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: None,
        });
        cx.spawn_in(window,async move|this,cx|{match picker.await{Ok(Ok(Some(paths)))=>{if let Some(path)=paths.first(){let content=std::fs::read_to_string(path);let _=this.update_in(cx,|this,_,cx|{match content{Ok(text)=>{let old=this.store.clone();match this.store.import(&text){Ok(count)=>{if this.persist(){this.message=format!("Imported {count} servers; proxy commands require review");}else{this.store=old;}},Err(error)=>this.message=error.to_string()}},Err(error)=>this.message=error.to_string()}cx.notify();});}},Ok(Err(error))=>{let _=this.update(cx,|this,cx|{this.message=error.to_string();cx.notify();});},_=>{}}}).detach();
    }
    fn export(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = match self.store.export() {
            Ok(text) => text,
            Err(error) => {
                self.message = error.to_string();
                cx.notify();
                return;
            }
        };
        let directory = opsssh_platform::home_dir().unwrap_or_default();
        let picker = cx.prompt_for_new_path(&directory, Some("opsssh-servers.toml"));
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(path))) = picker.await {
                let result = std::fs::write(path, text);
                let _ = this.update(cx, |this, cx| {
                    this.message = match result {
                        Ok(()) => "Exported server profiles without secrets".into(),
                        Err(error) => error.to_string(),
                    };
                    cx.notify();
                });
            }
        })
        .detach();
    }
}
fn environment_color(environment: &str) -> u32 {
    match environment.to_lowercase().as_str() {
        "production" | "prod" => 0xdb8d94,
        "staging" | "stage" => 0xe4bc77,
        _ => 0x8acabb,
    }
}
impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.first_frame_recorded {
            self.first_frame_recorded = true;
            let launched = self.launched;
            window.on_next_frame(move |_, _| {
                eprintln!(
                    "OpsSSH first-frame callback: {:.2} ms (presentation unverified)",
                    launched.elapsed().as_secs_f64() * 1000.
                )
            });
        }
        let tabs = self
            .tabs
            .iter()
            .enumerate()
            .map(|(index, tab)| {
                Button::new(("tab", index))
                    .label(tab.name.clone())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.active = Some(index);
                        this.editor = None;
                        this.tabs[index]
                            .terminal
                            .update(cx, |terminal, cx| terminal.focus(window, cx));
                        cx.notify();
                    }))
            })
            .collect::<Vec<_>>();
        let root = div()
            .id("opsssh-workspace")
            .key_context("OpsSSH")
            .track_focus(&self.home_focus)
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(0x10141b))
            .text_color(rgb(0xe7ecf4))
            .on_action(cx.listener(Self::open_local))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::new_server))
            .on_action(cx.listener(Self::close_tab))
            .on_action(cx.listener(Self::quit))
            .child(
                div()
                    .h(px(52.))
                    .px_4()
                    .flex()
                    .items_center()
                    .gap_2()
                    .border_b_1()
                    .border_color(rgb(0x29303c))
                    .child(
                        Button::new("home")
                            .label("OpsSSH / Servers")
                            .on_click(cx.listener(|this, _, w, cx| this.home(&GoHome, w, cx))),
                    )
                    .children(tabs)
                    .child(Button::new("new-local").label("+ Local").on_click(
                        cx.listener(|this, _, w, cx| this.open_local(&OpenLocalTerminal, w, cx)),
                    ))
                    .when(self.active.is_some(), |bar| {
                        bar.child(Button::new("close-tab").label(tr("close-tab")).on_click(
                            cx.listener(|this, _, w, cx| this.close_tab(&CloseTab, w, cx)),
                        ))
                    }),
            );
        if let Some(index) = self.active {
            let tab = &self.tabs[index];
            let terminal = tab.terminal.clone();
            let files = tab.files.clone();
            let visible = tab.files_visible;
            let follow = tab.follow;
            let color = tab.color;
            let is_ssh = terminal.read(cx).ssh_commands().is_some();
            return root.child(
                div()
                    .flex_1()
                    .min_h_0()
                    .border_t_2()
                    .border_color(rgb(color))
                    .flex()
                    .flex_col()
                    .when(is_ssh, |d| {
                        d.child(
                            div()
                                .flex()
                                .gap_2()
                                .px_3()
                                .py_1()
                                .child(
                                    Button::new("toggle-files")
                                        .label(if visible { "Hide files" } else { "Files" })
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            if this.tabs[index].files_visible {
                                                this.tabs[index].files_visible = false;
                                            } else {
                                                let terminal = this.tabs[index].terminal.clone();
                                                this.ensure_files(&terminal, window, cx);
                                            }
                                            cx.notify();
                                        })),
                                )
                                .when(visible, |d| {
                                    d.child(
                                        Button::new("follow-directory")
                                            .label(if follow {
                                                "Following terminal folder"
                                            } else {
                                                "Follow terminal folder"
                                            })
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.tabs[index].follow = !this.tabs[index].follow;
                                                cx.notify();
                                            })),
                                    )
                                }),
                        )
                    })
                    .when(!self.message.is_empty(), |d| {
                        d.child(
                            div()
                                .px_3()
                                .text_color(rgb(0xe4bc77))
                                .child(self.message.clone()),
                        )
                    })
                    .child(
                        div()
                            .flex_1()
                            .min_h_0()
                            .flex()
                            .child(div().flex_1().min_w_0().child(terminal))
                            .when(visible, |d| {
                                d.when_some(files, |d, files| {
                                    d.child(
                                        div()
                                            .w(px(440.))
                                            .min_h_0()
                                            .border_l_1()
                                            .border_color(rgb(0x29303c))
                                            .child(files),
                                    )
                                })
                            }),
                    ),
            );
        }
        let mut content = div()
            .id("home-content")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .p_6()
            .flex()
            .flex_col()
            .gap_4();
        if !self.message.is_empty() {
            content = content.child(
                div()
                    .p_3()
                    .rounded_md()
                    .bg(rgb(0x322b20))
                    .text_color(rgb(0xf0ce9c))
                    .child(self.message.clone()),
            );
        }
        if let Some(editor) = &self.editor {
            let fields = editor
                .fields
                .iter()
                .enumerate()
                .map(|(index, field)| {
                    div()
                        .flex()
                        .items_center()
                        .gap_4()
                        .child(div().w(px(250.)).text_sm().child(LABELS[index]))
                        .child(Input::new(field).aria_label(LABELS[index]).w_full())
                })
                .collect::<Vec<_>>();
            let auth = editor.profile.auth.clone();
            let approved = !editor.profile.proxy_review_required;
            let warnings = editor.warnings.join("; ");
            return root.child(content.child(div().text_2xl().child("Connect to a server"))
            .child(div().flex().gap_2().child(Input::new(&editor.quick).aria_label("Paste an SSH command").w_full()).child(Button::new("parse-command").label("Fill from command").on_click(cx.listener(|this,_,window,cx|{let command=this.editor.as_ref().unwrap().quick.read(cx).value();match opsssh_ssh_config::parse_command(&command){Ok(parsed)=>{this.edit(parsed.profile,window,cx);this.editor.as_mut().unwrap().warnings=parsed.warnings;},Err(error)=>this.message=error}cx.notify();}))))
            .when(!warnings.is_empty(),|d|d.child(div().text_color(rgb(0xe4bc77)).child(warnings)).child(Button::new("omit-unsupported").label("Use only the supported fields shown below").on_click(cx.listener(|this,_,_,cx|{let editor=this.editor.as_mut().unwrap();editor.warnings.clear();editor.profile.forward_agent=false;editor.profile.legacy=false;cx.notify();})))).children(fields)
            .child(div().flex().gap_2().child("Sign in with").children([(Auth::Agent,"SSH agent"),(Auth::Password,"Password"),(Auth::Key,"Key file")].into_iter().enumerate().map(|(index,(method,label))|Button::new(("auth",index)).label(if auth==method{format!("Selected: {label}")}else{label.into()}).on_click(cx.listener(move|this,_,_,cx|{this.editor.as_mut().unwrap().profile.auth=method.clone();cx.notify();})))))
            .child(div().text_sm().text_color(rgb(0x9aacc0)).child("Passwords and one-time codes are requested securely when connecting. tmux requires tmux on the server."))
            .child(Button::new("approve-proxy").label(if approved{"Proxy command reviewed"}else{"Approve the exact proxy command shown above"}).on_click(cx.listener(|this,_,_,cx|{if let Ok(profile)=this.read_editor(cx){let editor=this.editor.as_mut().unwrap();editor.profile.proxy_command=profile.proxy_command;editor.profile.proxy_review_required=false;}cx.notify();})))
            .child(div().flex().gap_3().child(Button::new("save-connect").primary().label(tr("save-connect")).on_click(cx.listener(|this,_,w,cx|this.save_editor(true,w,cx)))).child(Button::new("save").label(tr("save-server")).on_click(cx.listener(|this,_,w,cx|this.save_editor(false,w,cx)))).child(Button::new("test").label("Connect without saving").on_click(cx.listener(|this,_,w,cx|{match this.read_editor(cx){Ok(p)=>this.connect(p,w,cx),Err(e)=>{this.message=e;cx.notify();}}}))).child(Button::new("cancel").label(tr("cancel")).on_click(cx.listener(|this,_,w,cx|this.home(&GoHome,w,cx))))));
        }
        let query = self.search.read(cx).value();
        let profiles = self.visible_profiles(&query);
        let filters = std::iter::once("All servers".to_string())
            .chain(["Favourites".into(), "From SSH config".into()])
            .chain(
                self.store
                    .servers
                    .iter()
                    .map(|p| p.environment.clone())
                    .filter(|e| !e.is_empty())
                    .collect::<std::collections::BTreeSet<_>>(),
            )
            .collect::<Vec<_>>();
        let sidebar = div()
            .w(px(200.))
            .p_4()
            .flex()
            .flex_col()
            .gap_2()
            .bg(rgb(0x151a23))
            .children(filters.into_iter().enumerate().map(|(index, filter)| {
                Button::new(("filter", index))
                    .label(filter.clone())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.filter = filter.clone();
                        cx.notify();
                    }))
            }))
            .child(div().flex_1())
            .child(
                Button::new("import")
                    .label(tr("import-servers"))
                    .on_click(cx.listener(|this, _, w, cx| this.import(w, cx))),
            )
            .child(
                Button::new("export")
                    .label(tr("export-servers"))
                    .on_click(cx.listener(|this, _, w, cx| this.export(w, cx))),
            );
        content = content
            .child(
                div()
                    .flex()
                    .justify_between()
                    .items_center()
                    .child(div().text_2xl().child(self.filter.clone()))
                    .child(
                        Button::new("add-server")
                            .primary()
                            .label(tr("new-server"))
                            .on_click(
                                cx.listener(|this, _, w, cx| this.new_server(&NewServer, w, cx)),
                            ),
                    ),
            )
            .child(Input::new(&self.search).aria_label("Search servers"))
            .when(profiles.is_empty(), |d| {
                d.child(
                    div()
                        .p_6()
                        .text_color(rgb(0x94a2b7))
                        .child("No matching servers. Add a server or use your SSH config."),
                )
            })
            .children(profiles.into_iter().enumerate().map(|(index, profile)| {
                let connect = profile.clone();
                let edit = profile.clone();
                let favorite = profile.clone();
                let duplicate = profile.clone();
                let delete = profile.clone();
                let config = profile.id == 0;
                div()
                    .p_4()
                    .rounded_md()
                    .border_1()
                    .border_color(rgb(0x2c3441))
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(
                        div()
                            .flex()
                            .justify_between()
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .child(profile.name.clone())
                                    .child(div().text_sm().text_color(rgb(0x8797ad)).child(
                                        format!(
                                            "{}@{}:{}",
                                            profile.user, profile.host, profile.port
                                        ),
                                    )),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(rgb(environment_color(&profile.environment)))
                                    .child(profile.environment.clone()),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .child(
                                Button::new(("connect", index))
                                    .primary()
                                    .label(tr("connect-server"))
                                    .on_click(cx.listener(move |this, _, w, cx| {
                                        this.connect(connect.clone(), w, cx)
                                    })),
                            )
                            .child(
                                Button::new(("edit", index))
                                    .label(if config { "Customize in app" } else { "Edit" })
                                    .on_click(cx.listener(move |this, _, w, cx| {
                                        this.edit(edit.clone(), w, cx)
                                    })),
                            )
                            .when(!config, |d| {
                                d.child(
                                    Button::new(("fav", index))
                                        .label(if profile.favorite {
                                            "Unfavourite"
                                        } else {
                                            "Favourite"
                                        })
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            let old = this.store.clone();
                                            if let Some(p) = this
                                                .store
                                                .servers
                                                .iter_mut()
                                                .find(|p| p.id == favorite.id)
                                            {
                                                p.favorite = !p.favorite;
                                            }
                                            if !this.persist() {
                                                this.store = old;
                                            }
                                            cx.notify();
                                        })),
                                )
                                .child(
                                    Button::new(("duplicate", index))
                                        .label("Duplicate")
                                        .on_click(cx.listener(move |this, _, w, cx| {
                                            let mut p = duplicate.clone();
                                            p.id = 0;
                                            p.name.push_str(" copy");
                                            this.edit(p, w, cx);
                                        })),
                                )
                                .child(
                                    Button::new(("delete", index)).label("Delete").on_click(
                                        cx.listener(move |this, _, _, cx| {
                                            let old = this.store.clone();
                                            this.store.servers.retain(|p| p.id != delete.id);
                                            if !this.persist() {
                                                this.store = old;
                                            }
                                            cx.notify();
                                        }),
                                    ),
                                )
                            }),
                    )
            }));
        root.key_context("OpsSSH OpsSSHHome").child(
            div()
                .flex_1()
                .min_h_0()
                .flex()
                .child(sidebar)
                .child(content),
        )
    }
}
