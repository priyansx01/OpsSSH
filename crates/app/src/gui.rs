use crate::connection_form::{ConnectionForm, FormEvent};
use crate::files::{FilePane, FilePaneEvent};
mod views;
use crate::localization::text as tr;
use gpui::{
    App, Bounds, Context, Entity, FocusHandle, KeyBinding, Render, Window, WindowBounds,
    WindowOptions, actions, div, prelude::*, px, rgb, size,
};
use gpui_component::{
    Root, Theme, ThemeMode, WindowExt,
    button::{Button, ButtonVariants},
    input::{Input, InputEvent, InputState},
};
use opsssh_store::{Profile, Store};
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
    environment: String,
    files: Option<Entity<FilePane>>,
    files_visible: bool,
    follow: bool,
    followed_directory: Option<String>,
    file_generation: u64,
}
struct Workspace {
    home_focus: FocusHandle,
    search: Entity<InputState>,
    store: Store,
    path: PathBuf,
    tabs: Vec<Tab>,
    active: Option<usize>,
    editor: Option<Entity<ConnectionForm>>,
    settings_page: bool,
    help_page: bool,
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
pub fn capture(screen: &str, path: PathBuf) {
    run_internal(screen == "snapshot-local", Some((screen.into(), path)));
}
fn run_internal(open_terminal: bool, capture_path: Option<(String, PathBuf)>) {
    let launched = Instant::now();
    opsssh_platform::application().with_assets(gpui_kit_assets::AllAssets).run(move|cx:&mut App|{
    gpui_component::init(cx);Theme::change(ThemeMode::Dark,None,cx);
    cx.bind_keys([KeyBinding::new("ctrl-enter",OpenLocalTerminal,Some("OpsSSHHome")),KeyBinding::new("cmd-enter",OpenLocalTerminal,Some("OpsSSHHome")),KeyBinding::new("ctrl-k",GoHome,Some("OpsSSH")),KeyBinding::new("cmd-k",GoHome,Some("OpsSSH")),KeyBinding::new("ctrl-n",NewServer,Some("OpsSSH")),KeyBinding::new("cmd-n",NewServer,Some("OpsSSH")),KeyBinding::new("ctrl-w",CloseTab,Some("OpsSSH")),KeyBinding::new("cmd-w",CloseTab,Some("OpsSSH")),KeyBinding::new("ctrl-shift-h",GoHome,Some("OpsSSH")),KeyBinding::new("cmd-shift-h",GoHome,Some("OpsSSH")),KeyBinding::new("ctrl-shift-q",Quit,Some("OpsSSH")),KeyBinding::new("cmd-q",Quit,Some("OpsSSH"))]);
    cx.on_window_closed(|cx,_|{if cx.windows().is_empty(){cx.quit();}}).detach();
    #[allow(unused_mut)]
    let mut dimensions=size(px(1280.),px(820.));
    #[cfg(feature="capture")]
    if capture_path.is_some() {
        let dimension=|key:&str,default:f32|std::env::var(key).ok().and_then(|v|v.parse::<f32>().ok()).filter(|v|v.is_finite()).unwrap_or(default).clamp(640.,2560.);
        dimensions=size(px(dimension("OPSSSH_CAPTURE_WIDTH",1280.)),px(dimension("OPSSSH_CAPTURE_HEIGHT",820.)));
    }
    let bounds=Bounds::centered(None,dimensions,cx);
    let result=cx.open_window(WindowOptions{window_bounds:Some(WindowBounds::Windowed(bounds)),..WindowOptions::default()},move|window,cx|{
        window.set_window_title("OpsSSH");
        let workspace=cx.new(|cx: &mut Context<Workspace>|{
            let home_focus=cx.focus_handle();let search=cx.new(|cx|InputState::new(window,cx).placeholder("Search name, host, user, environment or tags"));
            cx.subscribe_in(&search,window,|this,_,event,window,cx|{match event {InputEvent::PressEnter{..}=>{let query=this.search.read(cx).value();if let Some(p)=this.visible_profiles(&query).first().cloned(){this.connect(p,window,cx);}},InputEvent::Change=>cx.notify(),_=>{}}}).detach();
            let (path,mut message)=match opsssh_platform::app_data_dir(){Ok(dir)=>(dir.join("servers.toml"),String::new()),Err(error)=>(PathBuf::new(),format!("Cannot locate settings folder: {error}"))};
            let mut load_failed=false;
            let store=if path.as_os_str().is_empty(){load_failed=true;Store::default()}else{match Store::load(&path){Ok(store)=>store,Err(error)=>{message=format!("Cannot load servers. Fix the settings file before saving: {error}");load_failed=true;Store::default()}}};
            let mut workspace=Workspace{home_focus,search,store,path,tabs:vec![],active:None,editor:None,settings_page:false,help_page:false,message,filter:"All servers".into(),config:vec![],launched,first_frame_recorded:false,load_failed};
            crate::design::apply(&workspace.store.settings.theme,window,cx);
            crate::design::set_reduced_motion(workspace.store.settings.reduced_motion,cx);
            crate::design::observe_system(window).detach();
            workspace.reload_config();
            if open_terminal{workspace.open_local(&OpenLocalTerminal,window,cx);}else{workspace.search.update(cx,|state,cx|state.focus(window,cx));}
            #[cfg(feature="capture")]
            if let Some((screen,path))=capture_path{
                // In-memory fixtures only; snapshot processes cannot save user data.
                workspace.load_failed=true;workspace.message.clear();workspace.config.clear();workspace.store=Store::default();
                if screen.contains("servers") || screen.contains("list") || screen.contains("many") {
                    let count=if screen.contains("many"){500}else{6};
                    for i in 0..count {let _=workspace.store.upsert(Profile{name:if i==5 {"Research / a deliberately long server name to verify truncation".into()}else{format!("{} {:02}",["Atlas gateway","Build runner","Payments API","Staging database","Personal lab"][i%5],i+1)},host:format!("host-{}.example.test",i+1),user:"deploy".into(),environment:["Production","Staging","Development"][i%3].into(),auth:[opsssh_store::Auth::Agent,opsssh_store::Auth::Password,opsssh_store::Auth::Key][i%3].clone(),favorite:i%2==0,..Profile::default()});}
                }
                if screen.contains("list") {workspace.store.settings.server_view="list".into();}
                if screen.contains("light") {crate::design::apply("light",window,cx);}
                if screen.contains("files") {
                    workspace.open_local(&OpenLocalTerminal,window,cx);
                    let files=cx.new(|cx|FilePane::preview(window,cx));
                    let tab=workspace.tabs.last_mut().unwrap();tab.files=Some(files);tab.files_visible=true;
                }
                if screen.contains("settings") {workspace.settings_page=true;}
                if screen.contains("connection") {let this=cx.entity().downgrade();window.on_next_frame(move|window,cx|{let _=this.update(cx,|this,cx|this.new_server(&NewServer,window,cx));});}
                if screen.contains("key") {let this=cx.entity().downgrade();window.on_next_frame(move|window,cx|{let _=this.update(cx,|this,cx|this.edit(Profile{host:"host.example.test".into(),user:"deploy".into(),auth:opsssh_store::Auth::Key,..Profile::default()},window,cx));});}
                let timer=cx.background_executor().timer(std::time::Duration::from_secs(2));cx.spawn_in(window,async move|this,cx|{timer.await;let _=this.update_in(cx,|_,window,cx|{match window.render_to_image().and_then(|image|image.save(&path).map_err(Into::into)){Ok(())=>eprintln!("Saved rendered frame to {}",path.display()),Err(error)=>eprintln!("Frame export failed: {error}")};cx.quit();});}).detach();}
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
        self.settings_page = false;
        self.help_page = false;
        self.tabs.push(Tab {
            name: "Local".into(),
            terminal: cx.new(|cx| {
                let mut terminal = TerminalView::new(window, cx);
                terminal.set_font_size(self.store.settings.font_size as f32, cx);
                terminal
            }),
            color: 0x8abdaf,
            environment: tr("local-session"),
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
        self.settings_page = false;
        self.help_page = false;
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
        if self.settings_page || self.help_page || self.editor.is_some() {
            return;
        }
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
        let warnings = if profile.id == 0 {
            self.config
                .iter()
                .find(|(p, _)| p.name == profile.name)
                .map(|(_, w)| w.clone())
                .unwrap_or_default()
        } else {
            vec![]
        };
        let title = tr(if profile.id == 0 {
            "form-title"
        } else {
            "form-edit-title"
        });
        let form = cx.new(|cx| ConnectionForm::new(profile, warnings, window, cx));
        cx.subscribe_in(&form, window, |this, form, event, window, cx| {
            match event {
                FormEvent::Cancel => {
                    this.editor = None;
                    window.close_dialog(cx);
                }
                FormEvent::Save(profile, connect) => {
                    if *connect
                        && let Err(error) = crate::connections::options(profile, &this.store)
                    {
                        form.update(cx, |f, cx| f.error(error, cx));
                        cx.notify();
                        return;
                    }
                    let old = this.store.clone();
                    let mut profile = profile.as_ref().clone();
                    match this.store.upsert(profile.clone()) {
                        Ok(id) => {
                            profile.id = id;
                            if this.persist() {
                                this.editor = None;
                                window.close_dialog(cx);
                                if *connect {
                                    this.connect(profile, window, cx);
                                } else {
                                    this.message = tr("server-saved");
                                }
                            } else {
                                this.store = old;
                                form.update(cx, |f, cx| f.error(this.message.clone(), cx));
                            }
                        }
                        Err(error) => form.update(cx, |f, cx| f.error(error.to_string(), cx)),
                    }
                }
            }
            cx.notify();
        })
        .detach();
        let child = form.clone();
        let workspace = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, window, _cx| {
            let workspace = workspace.clone();
            dialog
                .title(title.clone())
                .width(px(660.).min(window.viewport_size().width - px(48.)))
                .overlay_closable(false)
                .child(child.clone())
                .on_close(move |_, _, cx| {
                    let _ = workspace.update(cx, |this, cx| {
                        this.editor = None;
                        cx.notify();
                    });
                })
        });
        form.update(cx, |form, cx| form.focus(window, cx));
        self.editor = Some(form);
        self.message.clear();
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
                let terminal = cx.new(|cx| {
                    let mut terminal = TerminalView::connect(options, window, cx);
                    terminal.set_font_size(self.store.settings.font_size as f32, cx);
                    terminal
                });
                self.settings_page = false;
                self.help_page = false;
                let profile_id = profile.id;
                let mut recorded = false;
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
                cx.observe_in(&terminal, window, move |this, terminal, _, cx| {
                    if !recorded
                        && terminal.read(cx).session_state()
                            == opsssh_term_core::SessionState::Connected
                    {
                        recorded = true;
                        let old = this.store.clone();
                        let timestamp = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs();
                        if this.store.record_connection(profile_id, timestamp).is_ok()
                            && !this.persist()
                        {
                            this.store = old;
                        }
                    }
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
                    environment: profile.environment.clone(),
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
        let mut profiles: Vec<_> = profiles
            .into_iter()
            .filter(|p| {
                p.matches(query)
                    && match self.filter.as_str() {
                        "All servers" | "From SSH config" => true,
                        "Favourites" => p.favorite,
                        environment => p.environment == environment,
                    }
            })
            .collect();
        if self.store.settings.sort == "last_connected" {
            profiles.sort_by_key(|p| {
                std::cmp::Reverse(
                    self.store
                        .settings
                        .last_connected
                        .get(&p.id)
                        .copied()
                        .unwrap_or_default(),
                )
            });
        } else {
            profiles.sort_by_key(|p| p.name.to_lowercase());
        }
        profiles
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
        self.render_workspace(window, cx)
    }
}
