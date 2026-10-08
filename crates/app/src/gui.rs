use crate::connection_form::{ConnectionForm, FormEvent};
use crate::files::{FilePane, FilePaneEvent};
mod session_tabs;
mod terminal_uploads;
mod views;
use crate::infrastructure::InfrastructureView;
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
use session_tabs::{SessionId, SessionTabs};
use std::{path::PathBuf, time::Instant};
actions!(
    opsssh,
    [OpenLocalTerminal, GoHome, NewServer, CloseTab, Quit]
);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SessionPage {
    Terminal,
    Files,
    Infrastructure,
}

struct Tab {
    id: SessionId,
    endpoint: Option<String>,
    profile_id: Option<u64>,
    upload_directory: Option<String>,
    keyboard_capture: bool,
    persistent_name: Option<String>,
    protected: bool,
    connected_at: Option<Instant>,
    page: SessionPage,
    infrastructure: Option<Entity<InfrastructureView>>,
    name: String,
    terminal: Entity<TerminalView>,
    color: u32,
    environment: String,
    files: Option<Entity<FilePane>>,
    files_visible: bool,
    follow: bool,
    followed_directory: Option<String>,
    file_generation: u64,
    files_needs_rebind: bool,
}
struct Workspace {
    home_focus: FocusHandle,
    capture: Option<opsssh_platform::keyboard_capture::KeyboardCapture>,
    upload_dialog: Option<terminal_uploads::UploadDialog>,
    _terminal_focus: Vec<gpui::Subscription>,
    search: Entity<InputState>,
    store: Store,
    path: PathBuf,
    sessions: Vec<Tab>,
    navigation: SessionTabs,
    transition: u64,
    pending_close: Option<SessionId>,
    quit_dialog: bool,
    uptime_timer: Option<gpui::Task<()>>,
    editor: Option<Entity<ConnectionForm>>,
    settings_page: bool,
    help_page: bool,
    message: String,
    filter: String,
    hovered_card: Option<usize>,
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
fn bind_workspace_keys(cx: &mut App) {
    TerminalView::bind_keys(cx);
    // Workspace actions must never consume keys from a focused VM terminal.
    const WORKSPACE: &str = "OpsSSH && !OpsSSHTerminal && !OpsSSHTerminalOpen";
    const HOME: &str = "OpsSSHHome && !OpsSSHTerminal && !OpsSSHTerminalOpen";
    cx.bind_keys([
        KeyBinding::new("ctrl-enter", OpenLocalTerminal, Some(HOME)),
        KeyBinding::new("cmd-enter", OpenLocalTerminal, Some(HOME)),
        KeyBinding::new("ctrl-k", GoHome, Some(WORKSPACE)),
        KeyBinding::new("cmd-k", GoHome, Some(WORKSPACE)),
        KeyBinding::new("ctrl-n", NewServer, Some(WORKSPACE)),
        KeyBinding::new("cmd-n", NewServer, Some(WORKSPACE)),
        KeyBinding::new("ctrl-w", CloseTab, Some(WORKSPACE)),
        KeyBinding::new("cmd-w", CloseTab, Some(WORKSPACE)),
        KeyBinding::new("ctrl-shift-h", GoHome, Some(WORKSPACE)),
        KeyBinding::new("cmd-shift-h", GoHome, Some(WORKSPACE)),
        KeyBinding::new("ctrl-shift-q", Quit, Some(WORKSPACE)),
        KeyBinding::new("cmd-q", Quit, Some(WORKSPACE)),
    ]);
}

fn run_internal(open_terminal: bool, capture_path: Option<(String, PathBuf)>) {
    let launched = Instant::now();
    let enable_native_capture = capture_path.is_none();
    opsssh_platform::application().with_assets(gpui_kit_assets::AllAssets).run(move|cx:&mut App|{
    gpui_component::init(cx);Theme::change(ThemeMode::Dark,None,cx);
    bind_workspace_keys(cx);
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
            let home_focus=cx.focus_handle();let terminal_focus=Workspace::terminal_focus_subscriptions(&home_focus,window,cx);let search=cx.new(|cx|InputState::new(window,cx).placeholder("Search name, host, user, environment or tags"));
            cx.subscribe_in(&search,window,|this,_,event,window,cx|{match event {InputEvent::PressEnter{..}=>{let query=this.search.read(cx).value();if let Some(p)=this.visible_profiles(&query).first().cloned(){this.connect(p,window,cx);}},InputEvent::Change=>cx.notify(),_=>{}}}).detach();
            let (path,mut message)=match opsssh_platform::app_data_dir(){Ok(dir)=>(dir.join("servers.toml"),String::new()),Err(error)=>(PathBuf::new(),format!("Cannot locate settings folder: {error}"))};
            let mut load_failed=false;
            let store=if path.as_os_str().is_empty(){load_failed=true;Store::default()}else{match Store::load(&path){Ok(store)=>store,Err(error)=>{message=format!("Cannot load servers. Fix the settings file before saving: {error}");load_failed=true;Store::default()}}};
            let mut workspace=Workspace{home_focus,capture:None,upload_dialog:None,_terminal_focus:terminal_focus,search,store,path,sessions:vec![],navigation:SessionTabs::default(),transition:0,pending_close:None,quit_dialog:false,uptime_timer:None,editor:None,settings_page:false,help_page:false,message,filter:"All servers".into(),hovered_card:None,config:vec![],launched,first_frame_recorded:false,load_failed};
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
                if screen.contains("files") || screen.contains("upload") {
                    workspace.open_local(&OpenLocalTerminal,window,cx);
                    let files=cx.new(|cx|if screen.contains("files-list") {FilePane::preview_list(window,cx)}else{FilePane::preview(window,cx)});
                    let tab=workspace.sessions.last_mut().unwrap();tab.files=Some(files);tab.files_visible=screen.contains("files");
                    if screen.contains("upload-progress") || screen.contains("upload-failed") {tab.files.as_ref().unwrap().update(cx,|files,_|files.preview_transfer(screen.contains("failed")));}
                    if screen.contains("session") && screen.contains("files") {tab.page=SessionPage::Files;tab.files.as_ref().unwrap().update(cx,|f,cx|f.set_full_page(true,cx));}
                }
                if screen.contains("session") || screen.contains("close") || screen.contains("background") {
                    if workspace.sessions.is_empty(){workspace.open_local(&OpenLocalTerminal,window,cx);}
                    let tab=workspace.sessions.last_mut().unwrap();
                    tab.keyboard_capture=true;tab.name="Atlas development VM".into();tab.endpoint=Some("deploy@atlas.example.test:22".into());tab.environment="Development".into();tab.protected=true;tab.persistent_name=Some("harness".into());
                    if screen.contains("clipboard") {
                        let terminal=tab.terminal.clone();
                        let timer=cx.background_executor().timer(std::time::Duration::from_millis(900));
                        cx.spawn_in(window,async move|_,cx|{timer.await;let _=terminal.update_in(cx,|view,window,cx|view.preview_clipboard_menu(window,cx));}).detach();
                    }
                    if screen.contains("infra") {tab.page=SessionPage::Infrastructure;tab.infrastructure=Some(cx.new(|cx|InfrastructureView::preview(window,cx)));}
                    if screen.contains("background") {workspace.navigation.hide(tab.id);}
                    if screen.contains("close") {let id=tab.id;let this=cx.entity().downgrade();window.on_next_frame(move|window,cx|{let _=this.update(cx,|this,cx|this.request_close(id,window,cx));});}
                }
                if screen.contains("upload-destination") {
                    let id=workspace.sessions.last().unwrap().id;let this=cx.entity().downgrade();
                    window.on_next_frame(move|window,cx|{let _=this.update(cx,|this,cx|this.choose_upload_destination(id,vec![],window,cx));});
                }
                if screen.contains("settings") {workspace.settings_page=true;}
                if screen.contains("connection") {let this=cx.entity().downgrade();window.on_next_frame(move|window,cx|{let _=this.update(cx,|this,cx|this.new_server(&NewServer,window,cx));});}
                if screen.contains("key") {let this=cx.entity().downgrade();window.on_next_frame(move|window,cx|{let _=this.update(cx,|this,cx|this.edit(Profile{host:"host.example.test".into(),user:"deploy".into(),auth:opsssh_store::Auth::Key,..Profile::default()},window,cx));});}
                let timer=cx.background_executor().timer(std::time::Duration::from_secs(2));cx.spawn_in(window,async move|this,cx|{timer.await;let _=this.update_in(cx,|_,window,cx|{match window.render_to_image().and_then(|image|image.save(&path).map_err(Into::into)){Ok(())=>eprintln!("Saved rendered frame to {}",path.display()),Err(error)=>eprintln!("Frame export failed: {error}")};cx.quit();});}).detach();}
            #[cfg(not(feature="capture"))]let _=capture_path;
            workspace
        });
        if enable_native_capture {workspace.update(cx,|this,cx|this.install_keyboard_capture(window,cx));}
        let weak=workspace.downgrade();
        window.on_window_should_close(cx,move|window,cx| {weak.update(cx,|this,cx| this.quit(&Quit,window,cx)).is_err()});
        cx.new(|cx|Root::new(workspace,window,cx))
    });if let Err(error)=result{eprintln!("Could not open OpsSSH: {error}");cx.quit();return;}cx.activate(true);
});
}
impl Workspace {
    fn install_keyboard_capture(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if opsssh_platform::info().os != "windows" {
            return;
        }
        match opsssh_platform::keyboard_capture::KeyboardCapture::new(window) {
            Ok(capture) => {
                let receive = capture.events();
                self.capture = Some(capture);
                self._terminal_focus.push(
                    cx.observe_window_activation(window, |this, window, cx| {
                        this.update_keyboard_capture(window, cx)
                    }),
                );
                cx.spawn_in(window, async move |this, cx| {
                    while let Ok(event) = receive.recv().await {
                        if this
                            .update_in(cx, |this, window, cx| {
                                let Some(index) = this.session_index(SessionId(event.target))
                                else {
                                    return;
                                };
                                if event.release_capture {
                                    this.sessions[index].keyboard_capture = false;
                                    this.update_keyboard_capture(window, cx);
                                    cx.notify();
                                } else if this.navigation.active == Some(SessionId(event.target))
                                    && this.terminal_visible()
                                    && !window.has_active_dialog(cx)
                                    && this.sessions[index]
                                        .terminal
                                        .read(cx)
                                        .accepts_terminal_keys(window)
                                {
                                    this.sessions[index].terminal.update(cx, |terminal, cx| {
                                        terminal.captured_key(event, cx)
                                    });
                                }
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                })
                .detach();
            }
            Err(error) => {
                self.message = format!(
                    "Keyboard capture unavailable: {error}. Terminal shortcuts still pass through; use Send Alt+Tab from the menu."
                )
            }
        }
    }

    fn update_keyboard_capture(&self, window: &mut Window, cx: &mut Context<Self>) {
        let target = if self.terminal_visible()
            && self.editor.is_none()
            && self.pending_close.is_none()
            && !self.quit_dialog
            && self.upload_dialog.is_none()
            && !window.has_active_dialog(cx)
        {
            self.navigation
                .active
                .and_then(|id| self.session_index(id))
                .and_then(|index| {
                    let tab = &self.sessions[index];
                    let terminal = tab.terminal.read(cx);
                    (tab.keyboard_capture
                        && tab.endpoint.is_some()
                        && terminal.accepts_terminal_keys(window))
                    .then_some((tab.id.0, terminal.connection_generation()))
                })
        } else {
            None
        };
        if let Some(capture) = &self.capture {
            capture.set_target(target);
        }
    }
    fn terminal_focus_subscriptions(
        home: &FocusHandle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<gpui::Subscription> {
        vec![
            cx.on_focus(home, window, |this, window, cx| {
                this.restore_terminal_focus(window, cx)
            }),
            cx.on_focus_lost(window, |this, window, cx| {
                this.restore_terminal_focus(window, cx)
            }),
        ]
    }

    fn terminal_visible(&self) -> bool {
        !self.settings_page
            && !self.help_page
            && self
                .navigation
                .active
                .and_then(|id| self.session_index(id))
                .is_some_and(|index| self.sessions[index].page == SessionPage::Terminal)
    }

    fn restore_terminal_focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.terminal_visible()
            || self.editor.is_some()
            || self.pending_close.is_some()
            || self.quit_dialog
            || self.upload_dialog.is_some()
            || window.has_active_dialog(cx)
        {
            return;
        }
        if let Some(index) = self.navigation.active.and_then(|id| self.session_index(id)) {
            self.sessions[index]
                .terminal
                .update(cx, |terminal, cx| terminal.focus(window, cx));
        }
    }

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
        let id = self.navigation.allocate();
        self.sessions.push(Tab {
            id,
            endpoint: None,
            profile_id: None,
            upload_directory: None,
            keyboard_capture: false,
            persistent_name: None,
            protected: false,
            connected_at: Some(Instant::now()),
            page: SessionPage::Terminal,
            infrastructure: None,
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
            files_needs_rebind: false,
        });
        self.transition = self.transition.wrapping_add(1);
        self.sync_visibility(cx);
        self.editor = None;
        cx.notify();
    }
    fn home(&mut self, _: &GoHome, window: &mut Window, cx: &mut Context<Self>) {
        self.settings_page = false;
        self.help_page = false;
        self.navigation.active = None;
        self.transition = self.transition.wrapping_add(1);
        self.sync_visibility(cx);
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
    fn session_index(&self, id: SessionId) -> Option<usize> {
        self.sessions.iter().position(|session| session.id == id)
    }
    fn sync_visibility(&mut self, cx: &mut Context<Self>) {
        if self.navigation.active.is_none() || self.settings_page || self.help_page {
            self.uptime_timer = None;
        } else if self.uptime_timer.is_none() {
            self.uptime_timer = Some(cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor()
                        .timer(std::time::Duration::from_secs(30))
                        .await;
                    if !this
                        .update(cx, |this, cx| {
                            cx.notify();
                            this.navigation.active.is_some()
                                && !this.settings_page
                                && !this.help_page
                        })
                        .unwrap_or(false)
                    {
                        break;
                    }
                }
            }));
        }
        for session in &self.sessions {
            if let Some(infra) = &session.infrastructure {
                let visible = self.navigation.active == Some(session.id)
                    && !self.settings_page
                    && !self.help_page
                    && session.page == SessionPage::Infrastructure
                    && session.terminal.read(cx).session_state()
                        == opsssh_term_core::SessionState::Connected;
                infra.update(cx, |view, cx| view.set_visible(visible, cx));
            }
            if let Some(files) = &session.files {
                files.update(cx, |view, cx| {
                    view.set_full_page(session.page == SessionPage::Files, cx)
                });
            }
        }
    }
    fn activate_session(&mut self, id: SessionId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.session_index(id) else {
            return;
        };
        self.navigation.show(id);
        self.settings_page = false;
        self.help_page = false;
        self.transition = self.transition.wrapping_add(1);
        self.sync_visibility(cx);
        if self.sessions[index].page == SessionPage::Terminal {
            self.sessions[index]
                .terminal
                .update(cx, |t, cx| t.focus(window, cx));
        } else {
            self.home_focus.focus(window, cx);
        }
        cx.notify();
    }
    fn select_page(
        &mut self,
        id: SessionId,
        page: SessionPage,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(index) = self.session_index(id) else {
            return;
        };
        if page == SessionPage::Files {
            let terminal = self.sessions[index].terminal.clone();
            if self.ensure_files(&terminal, window, cx).is_none() {
                return;
            }
        }
        if page == SessionPage::Infrastructure && self.sessions[index].infrastructure.is_none() {
            let terminal = self.sessions[index].terminal.read(cx);
            if terminal.session_state() != opsssh_term_core::SessionState::Connected {
                self.message = tr("infra-connect-first");
                cx.notify();
                return;
            }
            let Some(commands) = terminal.ssh_commands() else {
                return;
            };
            let generation = terminal.connection_generation();
            self.sessions[index].infrastructure =
                Some(cx.new(|cx| InfrastructureView::new(commands, generation, window, cx)));
        }
        self.sessions[index].page = page;
        self.activate_session(id, window, cx);
    }
    fn finish_close(
        &mut self,
        id: SessionId,
        disconnect: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.pending_close = None;
        self.navigation.hide(id);
        if disconnect && let Some(index) = self.session_index(id) {
            let session = self.sessions.remove(index);
            if let Some(files) = session.files {
                files.update(cx, |files, cx| files.cancel_transfers(cx));
            }
            session
                .terminal
                .update(cx, |terminal, cx| terminal.disconnect(cx));
        }
        self.transition = self.transition.wrapping_add(1);
        self.sync_visibility(cx);
        if let Some(active) = self.navigation.active {
            self.activate_session(active, window, cx);
        } else {
            self.search.update(cx, |s, cx| s.focus(window, cx));
        }
        cx.notify();
    }
    fn request_close(&mut self, id: SessionId, window: &mut Window, cx: &mut Context<Self>) {
        if self.pending_close.is_some() || self.editor.is_some() || self.quit_dialog {
            return;
        }
        let Some(index) = self.session_index(id) else {
            return;
        };
        let session = &self.sessions[index];
        if matches!(
            session.terminal.read(cx).session_state(),
            opsssh_term_core::SessionState::Closed | opsssh_term_core::SessionState::Disconnected
        ) {
            self.finish_close(id, true, window, cx);
            return;
        }
        let remote = session.endpoint.is_some();
        let name = session.name.clone();
        let protected = session.protected;
        let transfers = session
            .files
            .as_ref()
            .map(|f| f.read(cx).active_transfers())
            .unwrap_or(0);
        self.pending_close = Some(id);
        let weak = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, _, _| {
            let stay = weak.clone();
            let exit = weak.clone();
            let cancel = weak.clone();
            let cancel_button = weak.clone();
            dialog
                .title(format!("{} {}", tr("close-session"), name))
                .width(px(490.))
                .overlay_closable(false)
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_4()
                        .child(tr(if remote {
                            "close-session-help"
                        } else {
                            "close-local-help"
                        }))
                        .when(protected, |d| d.child(tr("close-tmux-help")))
                        .when(transfers > 0, |d| {
                            d.child(format!("{transfers} {}", tr("close-active-transfers")))
                        })
                        .child(
                            div()
                                .flex()
                                .flex_wrap()
                                .gap_2()
                                .when(remote, |d| {
                                    d.child(
                                        Button::new("stay-connected")
                                            .primary()
                                            .label(tr("stay-connected"))
                                            .on_click(move |_, window, cx| {
                                                let _ = stay.update(cx, |this, cx| {
                                                    this.finish_close(id, false, window, cx)
                                                });
                                                window.close_dialog(cx);
                                            }),
                                    )
                                })
                                .child(
                                    Button::new("disconnect-session")
                                        .danger()
                                        .label(tr(if remote {
                                            "disconnect"
                                        } else {
                                            "exit-terminal"
                                        }))
                                        .on_click(move |_, window, cx| {
                                            let _ = exit.update(cx, |this, cx| {
                                                this.finish_close(id, true, window, cx)
                                            });
                                            window.close_dialog(cx);
                                        }),
                                )
                                .child(Button::new("cancel-close").label(tr("cancel")).on_click(
                                    move |_, window, cx| {
                                        let _ = cancel_button.update(cx, |this, cx| {
                                            this.pending_close = None;
                                            cx.notify();
                                        });
                                        window.close_dialog(cx);
                                    },
                                )),
                        ),
                )
                .on_close(move |_, _, cx| {
                    let _ = cancel.update(cx, |this, cx| {
                        this.pending_close = None;
                        cx.notify();
                    });
                })
        });
    }
    fn close_tab(&mut self, _: &CloseTab, window: &mut Window, cx: &mut Context<Self>) {
        if !self.settings_page
            && !self.help_page
            && let Some(id) = self.navigation.active
        {
            self.request_close(id, window, cx);
        }
    }
    fn quit(&mut self, _: &Quit, window: &mut Window, cx: &mut Context<Self>) {
        if self.quit_dialog || self.pending_close.is_some() || self.editor.is_some() {
            return;
        }
        let active = self
            .sessions
            .iter()
            .filter(|s| {
                !matches!(
                    s.terminal.read(cx).session_state(),
                    opsssh_term_core::SessionState::Closed
                        | opsssh_term_core::SessionState::Disconnected
                )
            })
            .count();
        let transfers: usize = self
            .sessions
            .iter()
            .filter_map(|s| s.files.as_ref())
            .map(|f| f.read(cx).active_transfers())
            .sum();
        if active == 0 && transfers == 0 {
            cx.quit();
            return;
        }
        let protected = self.sessions.iter().filter(|s| s.protected).count();
        self.quit_dialog = true;
        let weak = cx.entity().downgrade();
        let close = weak.clone();
        window.open_dialog(cx, move |dialog, _, _| {
            let exit = weak.clone();
            let cancel = weak.clone();
            dialog
                .title(tr("quit-title"))
                .width(px(480.))
                .overlay_closable(false)
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_4()
                        .child(format!(
                            "{active} {} · {transfers} {}",
                            tr("quit-sessions"),
                            tr("close-active-transfers")
                        ))
                        .child(format!("{protected} {}", tr("quit-protected")))
                        .child(tr("quit-help"))
                        .child(
                            div()
                                .flex()
                                .gap_2()
                                .child(
                                    Button::new("quit-confirmed")
                                        .danger()
                                        .label(tr("quit"))
                                        .on_click(move |_, _, cx| {
                                            let _ = exit.update(cx, |this, cx| {
                                                for s in &this.sessions {
                                                    if let Some(f) = &s.files {
                                                        f.update(cx, |f, cx| {
                                                            f.cancel_transfers(cx)
                                                        });
                                                    }
                                                    s.terminal.update(cx, |t, cx| t.disconnect(cx));
                                                }
                                                cx.quit();
                                            });
                                        }),
                                )
                                .child(Button::new("quit-cancel").label(tr("cancel")).on_click(
                                    move |_, w, cx| {
                                        let _ = cancel.update(cx, |this, cx| {
                                            this.quit_dialog = false;
                                            cx.notify();
                                        });
                                        w.close_dialog(cx);
                                    },
                                )),
                        ),
                )
                .on_close({
                    let close = close.clone();
                    move |_, _, cx| {
                        let _ = close.update(cx, |this, cx| {
                            this.quit_dialog = false;
                            cx.notify();
                        });
                    }
                })
        });
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
                            profile.terminal_upload_directory = this
                                .store
                                .servers
                                .iter()
                                .find(|p| p.id == id)
                                .and_then(|p| p.terminal_upload_directory.clone());
                            if this.persist() {
                                let endpoint =
                                    format!("{}@{}:{}", profile.user, profile.host, profile.port);
                                for tab in &mut this.sessions {
                                    if tab.profile_id == Some(id) {
                                        if tab.endpoint.as_deref() == Some(&endpoint) {
                                            tab.upload_directory =
                                                profile.terminal_upload_directory.clone();
                                        } else {
                                            tab.profile_id = None;
                                            tab.upload_directory = None;
                                        }
                                    }
                                }
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
        let endpoint = format!("{}@{}:{}", profile.user, profile.host, profile.port);
        if profile.tmux_session.is_some()
            && let Some(id) = self
                .sessions
                .iter()
                .find(|s| {
                    s.endpoint.as_deref() == Some(&endpoint)
                        && s.persistent_name == profile.tmux_session
                        && !matches!(
                            s.terminal.read(cx).session_state(),
                            opsssh_term_core::SessionState::Closed
                                | opsssh_term_core::SessionState::Disconnected
                        )
                })
                .map(|s| s.id)
        {
            self.activate_session(id, window, cx);
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
                cx.subscribe_in(
                    &terminal,
                    window,
                    |this, terminal, event, window, cx| match event {
                        TerminalViewEvent::UploadFiles(paths) => {
                            this.terminal_drop(terminal, paths.clone(), window, cx)
                        }
                        TerminalViewEvent::UploadClipboard(item) => {
                            if let Some(pane) = this.background_files(terminal, window, cx) {
                                pane.update(cx, |pane, cx| pane.upload_clipboard(item.clone(), cx));
                            }
                        }
                    },
                )
                .detach();
                cx.observe_in(&terminal, window, move |this, terminal, window, cx| {
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
                    if let Some(tab) = this
                        .sessions
                        .iter_mut()
                        .find(|tab| tab.terminal == terminal)
                    {
                        if tab.connected_at.is_none()
                            && terminal.read(cx).session_state()
                                == opsssh_term_core::SessionState::Connected
                        {
                            tab.connected_at = Some(Instant::now());
                        }
                        tab.protected = terminal.read(cx).is_tmux_protected();
                        if tab
                            .infrastructure
                            .as_ref()
                            .is_some_and(|view| view.read(cx).generation() != generation)
                        {
                            tab.infrastructure = None;
                        }
                        if tab.files.is_some() && tab.file_generation != generation {
                            if let Some(files) = &tab.files {
                                files.update(cx, |pane, cx| pane.suspend(cx));
                            }
                            tab.file_generation = generation;
                            tab.files_needs_rebind = true;
                            tab.followed_directory = None;
                        }
                        if tab.files_needs_rebind
                            && terminal.read(cx).session_state()
                                == opsssh_term_core::SessionState::Connected
                            && let (Some(files), Some(commands), Some(fingerprint)) = (
                                &tab.files,
                                terminal.read(cx).ssh_commands(),
                                terminal.read(cx).verified_host_key().map(str::to_owned),
                            )
                        {
                            let folder = if tab.follow {
                                directory.clone().unwrap_or_default()
                            } else {
                                String::new()
                            };
                            files.update(cx, |pane, cx| {
                                pane.rebind(commands, fingerprint, folder, window, cx)
                            });
                            tab.files_needs_rebind = false;
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
                    if terminal.read(cx).session_state()
                        == opsssh_term_core::SessionState::Connected
                        && let Some((id, page)) = this
                            .sessions
                            .iter()
                            .find(|s| {
                                s.terminal == terminal
                                    && this.navigation.active == Some(s.id)
                                    && ((s.page == SessionPage::Infrastructure
                                        && s.infrastructure.is_none())
                                        || (s.page == SessionPage::Files && s.files.is_none()))
                            })
                            .map(|s| (s.id, s.page))
                    {
                        this.select_page(id, page, window, cx);
                    }
                    this.sync_visibility(cx);
                    cx.notify();
                })
                .detach();
                let id = self.navigation.allocate();
                self.sessions.push(Tab {
                    id,
                    profile_id: (profile.id != 0).then_some(profile.id),
                    upload_directory: profile.terminal_upload_directory.clone(),
                    keyboard_capture: true,
                    endpoint: Some(format!(
                        "{}@{}:{}",
                        profile.user, profile.host, profile.port
                    )),
                    persistent_name: profile.tmux_session.clone(),
                    protected: profile.tmux_session.is_some(),
                    connected_at: None,
                    page: SessionPage::Terminal,
                    infrastructure: None,
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
                    files_needs_rebind: false,
                });
                self.transition = self.transition.wrapping_add(1);
                self.sync_visibility(cx);
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
        let pane = self.background_files(terminal, window, cx)?;
        if let Some(tab) = self
            .sessions
            .iter_mut()
            .find(|tab| &tab.terminal == terminal)
        {
            tab.files_visible = true;
        }
        cx.notify();
        Some(pane)
    }
    fn background_files(
        &mut self,
        terminal: &Entity<TerminalView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<FilePane>> {
        let index = self
            .sessions
            .iter()
            .position(|tab| &tab.terminal == terminal)?;
        if let Some(files) = &self.sessions[index].files {
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
        cx.observe(&files, |_, _, cx| cx.notify()).detach();
        cx.subscribe_in(&files, window, move |this, pane, event, window, cx| {
            let (id, text) = match event {
                FilePaneEvent::ChooseUploadDestination => {
                    if let Some(id) = this
                        .sessions
                        .iter()
                        .find(|tab| {
                            tab.files.as_ref() == Some(pane)
                                && this.navigation.active == Some(tab.id)
                        })
                        .map(|tab| tab.id)
                    {
                        this.choose_upload_destination(id, vec![], window, cx);
                    }
                    return;
                }
                FilePaneEvent::InsertText(id, text) => (id, text),
                FilePaneEvent::FocusTerminal => {
                    if this.sessions.iter().any(|tab| {
                        tab.files.as_ref() == Some(pane) && this.navigation.active == Some(tab.id)
                    }) {
                        this.restore_terminal_focus(window, cx);
                    }
                    return;
                }
                FilePaneEvent::DestinationValidated(request, path) => {
                    this.destination_validated(pane, *request, path.clone(), window, cx);
                    return;
                }
                FilePaneEvent::DestinationFailed(request, error) => {
                    this.destination_failed(pane, *request, error.clone(), cx);
                    return;
                }
            };
            // FilePane discards old worker updates; also require this pane's current live transport.
            if let Some(tab) = this
                .sessions
                .iter()
                .find(|s| s.files.as_ref() == Some(pane))
                && !tab.files_needs_rebind
                && tab.terminal.read(cx).connection_generation() == tab.file_generation
                && tab.terminal.read(cx).session_state()
                    == opsssh_term_core::SessionState::Connected
            {
                if this.navigation.active == Some(tab.id)
                    && this.terminal_visible()
                    && this.editor.is_none()
                    && this.pending_close.is_none()
                    && !this.quit_dialog
                    && this.upload_dialog.is_none()
                    && !window.has_active_dialog(cx)
                    && tab.terminal.read(cx).accepts_terminal_keys(window)
                {
                    tab.terminal
                        .update(cx, |terminal, cx| terminal.insert_text(text, cx));
                } else {
                    pane.update(cx, |pane, cx| pane.defer_insert(*id, cx));
                }
            }
        })
        .detach();
        self.sessions[index].files = Some(files.clone());
        self.sessions[index].file_generation = generation;
        self.sessions[index].followed_directory = Some(directory);
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

#[cfg(all(test, feature = "capture"))]
mod tests {
    use super::*;
    use gpui::{Focusable, TestAppContext, VisualTestContext};

    #[gpui::test]
    fn terminal_drops_keep_files_closed_and_remember_the_chosen_destination(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_component::init);
        let mut workspace = None;
        let (_root, cx) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| fixture(window, cx));
            workspace = Some(view.clone());
            Root::new(view, window, cx)
        });
        let workspace = workspace.unwrap();
        let terminal = VisualTestContext::update(cx, |window, cx| {
            workspace.update(cx, |this, cx| {
                let index = this.session_index(this.navigation.active.unwrap()).unwrap();
                let terminal = cx.new(|cx| TerminalView::preview_connected_local(window, cx));
                let pane = cx.new(|cx| FilePane::preview(window, cx));
                this.sessions[index].terminal = terminal.clone();
                this.sessions[index].files = Some(pane);
                terminal.update(cx, |terminal, cx| terminal.focus(window, cx));
                terminal
            })
        });
        VisualTestContext::update(cx, |window, cx| {
            workspace.update(cx, |this, cx| {
                let index = this.session_index(this.navigation.active.unwrap()).unwrap();
                this.terminal_drop(&terminal, vec![PathBuf::from("first.txt")], window, cx);
                assert!(!this.sessions[index].files_visible);
                assert_eq!(this.sessions[index].page, SessionPage::Terminal);
                assert!(this.upload_dialog.is_some());
            })
        });
        cx.run_until_parked();
        cx.simulate_keystrokes("escape");
        VisualTestContext::update(cx, |window, cx| {
            workspace.update(cx, |this, cx| {
                assert!(this.upload_dialog.is_none());
                let index = this.session_index(this.navigation.active.unwrap()).unwrap();
                let terminal = this.sessions[index].terminal.clone();
                this.terminal_drop(&terminal, vec![PathBuf::from("second.txt")], window, cx);
                let dialog = this.upload_dialog.as_mut().unwrap();
                dialog.checking = true;
                let pane = dialog.pane.clone();
                let request = dialog.request;
                this.destination_failed(&pane, request, "Permission denied".into(), cx);
                assert!(this.upload_dialog.as_ref().is_some_and(|d| !d.checking));
                assert!(this.sessions[index].upload_directory.is_none());
                assert_eq!(pane.read(cx).active_transfers(), 0);
                this.upload_dialog.as_mut().unwrap().checking = true;
                this.destination_validated(&pane, request + 1, "/wrong".into(), window, cx);
                assert!(this.upload_dialog.is_some());
                this.destination_validated(&pane, request, "/srv/uploads".into(), window, cx);
                assert!(this.upload_dialog.is_none());
                assert_eq!(
                    this.sessions[index].upload_directory.as_deref(),
                    Some("/srv/uploads")
                );
                this.terminal_drop(&terminal, vec![PathBuf::from("third.txt")], window, cx);
                assert!(this.upload_dialog.is_none());
                assert!(!this.sessions[index].files_visible);
                assert_eq!(pane.read(cx).active_transfers(), 2);
                terminal.update(cx, |terminal, cx| terminal.disconnect(cx));
            })
        });
    }

    fn fixture(window: &mut Window, cx: &mut Context<Workspace>) -> Workspace {
        let home_focus = cx.focus_handle();
        let terminal_focus = Workspace::terminal_focus_subscriptions(&home_focus, window, cx);
        let mut navigation = SessionTabs::default();
        let sessions = (0..2)
            .map(|_| {
                let id = navigation.allocate();
                // Invalid options fail before a worker can open a network connection.
                let terminal = cx.new(|cx| {
                    TerminalView::connect(
                        opsssh_ssh_core::ConnectionOptions::new("", "", PathBuf::new()),
                        window,
                        cx,
                    )
                });
                Tab {
                    id,
                    endpoint: Some("deploy@fixture.invalid:22".into()),
                    profile_id: None,
                    upload_directory: None,
                    keyboard_capture: true,
                    persistent_name: None,
                    protected: false,
                    connected_at: None,
                    page: SessionPage::Terminal,
                    infrastructure: None,
                    name: format!("Session {}", id.0),
                    terminal,
                    color: 0xbc3150,
                    environment: String::new(),
                    files: None,
                    files_visible: false,
                    follow: true,
                    followed_directory: None,
                    file_generation: 0,
                    files_needs_rebind: false,
                }
            })
            .collect();
        Workspace {
            home_focus,
            capture: None,
            upload_dialog: None,
            _terminal_focus: terminal_focus,
            search: cx.new(|cx| InputState::new(window, cx)),
            store: Store::default(),
            path: PathBuf::new(),
            sessions,
            navigation,
            transition: 0,
            pending_close: None,
            quit_dialog: false,
            uptime_timer: None,
            editor: None,
            settings_page: false,
            help_page: false,
            message: String::new(),
            filter: "All servers".into(),
            hovered_card: None,
            config: vec![],
            launched: Instant::now(),
            first_frame_recorded: false,
            load_failed: true,
        }
    }

    #[gpui::test]
    fn hiding_and_reopening_retains_the_terminal_entity(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let mut workspace = None;
        let (_root, cx) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| fixture(window, cx));
            workspace = Some(view.clone());
            Root::new(view, window, cx)
        });
        let workspace = workspace.unwrap();
        VisualTestContext::update(cx, |window, cx| {
            workspace.update(cx, |this, cx| {
                let id = this.sessions[0].id;
                let terminal = this.sessions[0].terminal.clone();
                let other = this.sessions[1].id;
                this.finish_close(id, false, window, cx);
                assert_eq!(this.sessions.len(), 2);
                assert_eq!(this.navigation.active, Some(other));
                assert!(!this.navigation.open.contains(&id));
                this.activate_session(id, window, cx);
                assert_eq!(
                    this.sessions[this.session_index(id).unwrap()].terminal,
                    terminal
                );
                assert_eq!(this.navigation.active, Some(id));
            })
        });
    }

    #[gpui::test]
    fn disconnect_and_stale_close_target_cannot_remove_another_session(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let mut workspace = None;
        let (_root, cx) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| fixture(window, cx));
            workspace = Some(view.clone());
            Root::new(view, window, cx)
        });
        let workspace = workspace.unwrap();
        VisualTestContext::update(cx, |window, cx| {
            workspace.update(cx, |this, cx| {
                let first = this.sessions[0].id;
                let second = this.sessions[1].id;
                let terminal = this.sessions[1].terminal.clone();
                this.finish_close(first, true, window, cx);
                this.finish_close(first, true, window, cx);
                assert_eq!(this.sessions.len(), 1);
                assert_eq!(this.sessions[0].id, second);
                assert_eq!(this.sessions[0].terminal, terminal);
                assert_eq!(this.navigation.active, Some(second));
            })
        });
    }

    #[gpui::test]
    fn terminal_focus_preserves_harness_shortcuts(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_workspace_keys(cx);
        });
        let mut workspace = None;
        let (_root, cx) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| fixture(window, cx));
            workspace = Some(view.clone());
            Root::new(view, window, cx)
        });
        let workspace = workspace.unwrap();
        let active = VisualTestContext::update(cx, |window, cx| {
            workspace.update(cx, |this, cx| {
                let id = this.sessions[1].id;
                this.activate_session(id, window, cx);
                id
            })
        });
        cx.run_until_parked();
        VisualTestContext::update(cx, |window, cx| {
            let _ = window.draw(cx);
        });
        for keys in [
            "ctrl-k",
            "ctrl-n",
            "ctrl-w",
            "ctrl-shift-h",
            "ctrl-shift-q",
            "cmd-k",
            "cmd-n",
            "cmd-w",
            "cmd-shift-h",
            "cmd-q",
            "ctrl-enter",
            "cmd-enter",
        ] {
            cx.simulate_keystrokes(keys);
            VisualTestContext::update(cx, |window, cx| {
                let this = workspace.read(cx);
                assert_eq!(
                    this.navigation.active,
                    Some(active),
                    "{keys} navigated away from the VM"
                );
                assert_eq!(
                    this.sessions.len(),
                    2,
                    "{keys} changed the session registry"
                );
                assert!(
                    this.pending_close.is_none(),
                    "{keys} opened the close dialog"
                );
                assert!(!this.quit_dialog, "{keys} opened the quit dialog");
                assert!(this.editor.is_none(), "{keys} opened the connection editor");
                assert_eq!(
                    window.focused(cx),
                    Some(this.sessions[1].terminal.read(cx).focus_handle(cx))
                );
            });
        }
    }

    #[gpui::test]
    fn terminal_page_reclaims_workspace_and_lost_focus_before_typing(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_workspace_keys(cx);
        });
        let mut workspace = None;
        let (_root, cx) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| fixture(window, cx));
            workspace = Some(view.clone());
            Root::new(view, window, cx)
        });
        let workspace = workspace.unwrap();
        for index in [0, 1, 0] {
            VisualTestContext::update(cx, |window, cx| {
                workspace.update(cx, |this, cx| {
                    this.activate_session(this.sessions[index].id, window, cx);
                });
                let _ = window.draw(cx);
            });
            for empty in [false, true] {
                VisualTestContext::update(cx, |window, cx| {
                    if empty {
                        window.blur(cx);
                    } else {
                        let handle = workspace.read(cx).home_focus.clone();
                        handle.focus(window, cx);
                    }
                });
                cx.run_until_parked();
                VisualTestContext::update(cx, |window, cx| {
                    let _ = window.draw(cx);
                    assert_eq!(
                        window.focused(cx),
                        Some(
                            workspace.read(cx).sessions[index]
                                .terminal
                                .read(cx)
                                .focus_handle(cx)
                        )
                    );
                });
                cx.simulate_keystrokes("ctrl-w");
                assert!(workspace.read_with(cx, |this, _| this.pending_close.is_none()));
            }
        }
    }

    #[gpui::test]
    fn visible_terminal_page_blocks_workspace_shortcuts_even_without_terminal_focus(
        cx: &mut TestAppContext,
    ) {
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_workspace_keys(cx);
        });
        let mut workspace = None;
        let (_root, cx) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| fixture(window, cx));
            workspace = Some(view.clone());
            Root::new(view, window, cx)
        });
        let workspace = workspace.unwrap();
        let active = VisualTestContext::update(cx, |window, cx| {
            workspace.update(cx, |this, cx| {
                let id = this.sessions[1].id;
                this.activate_session(id, window, cx);
                id
            })
        });
        cx.run_until_parked();
        VisualTestContext::update(cx, |window, cx| {
            let _ = window.draw(cx);
            // Deliberate keyboard focus on chrome must still not enable app shortcuts.
            window.focus_next(cx);
            assert_ne!(
                window.focused(cx),
                Some(
                    workspace.read(cx).sessions[1]
                        .terminal
                        .read(cx)
                        .focus_handle(cx)
                )
            );
        });
        cx.simulate_keystrokes("ctrl-k ctrl-n ctrl-w ctrl-shift-h ctrl-shift-q");
        VisualTestContext::update(cx, |_, cx| {
            let this = workspace.read(cx);
            assert_eq!(this.navigation.active, Some(active));
            assert!(this.editor.is_none() && this.pending_close.is_none() && !this.quit_dialog);
            assert_eq!(this.sessions.len(), 2);
        });
    }

    #[gpui::test]
    fn workspace_shortcuts_still_work_on_home(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            bind_workspace_keys(cx);
        });
        let mut workspace = None;
        let (_root, cx) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| fixture(window, cx));
            workspace = Some(view.clone());
            Root::new(view, window, cx)
        });
        let workspace = workspace.unwrap();
        VisualTestContext::update(cx, |window, cx| {
            workspace.update(cx, |this, cx| this.home(&GoHome, window, cx));
        });
        cx.run_until_parked();
        VisualTestContext::update(cx, |window, cx| {
            let _ = window.draw(cx);
        });
        cx.simulate_keystrokes("ctrl-n");
        assert!(workspace.read_with(cx, |this, _| this.editor.is_some()));
    }

    #[gpui::test]
    fn escape_cancels_close_without_hiding_or_disconnecting(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let mut workspace = None;
        let (_root, cx) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| fixture(window, cx));
            workspace = Some(view.clone());
            Root::new(view, window, cx)
        });
        let workspace = workspace.unwrap();
        VisualTestContext::update(cx, |window, cx| {
            workspace.update(cx, |this, cx| {
                let id = this.sessions[0].id;
                this.request_close(id, window, cx);
                assert_eq!(this.pending_close, Some(id));
                assert_eq!(this.navigation.open.len(), 2);
            })
        });
        cx.run_until_parked();
        cx.simulate_keystrokes("escape");
        VisualTestContext::update(cx, |_, cx| {
            workspace.update(cx, |this, _| {
                assert_eq!(this.pending_close, None);
                assert_eq!(this.sessions.len(), 2);
                assert_eq!(this.navigation.open.len(), 2);
                assert_eq!(this.navigation.active, Some(this.sessions[1].id));
            })
        });
    }
}
