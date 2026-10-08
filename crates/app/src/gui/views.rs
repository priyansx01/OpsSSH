//! Workspace presentation. SSH/session ownership stays in the parent controller.
use super::*;
use crate::design;
use gpui::{Animation, AnimationExt, AnyElement, SpringAnimation, SpringConfig, uniform_list};
use gpui_component::{
    ActiveTheme, Disableable, Icon, IconName, Selectable, Sizable, TitleBar,
    menu::{DropdownMenu, PopupMenuItem},
    switch::Switch,
};
use std::{
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

fn card_hover<E: gpui::Styled>(element: E, p: design::Palette) -> E {
    let ruby: gpui::Hsla = rgb(p.ruby).into();
    let glow = if p.canvas == design::LIGHT.canvas {
        0xffedf1
    } else {
        0x2b151c
    };
    element
        .bg(gpui::linear_gradient(
            135.,
            gpui::linear_color_stop(rgb(glow), 0.),
            gpui::linear_color_stop(rgb(p.surface), 1.),
        ))
        .border_color(ruby.opacity(0.6))
        .shadow(vec![gpui::BoxShadow {
            color: ruby.opacity(0.08),
            offset: gpui::point(px(0.), px(3.)),
            blur_radius: px(16.),
            spread_radius: px(0.),
            inset: false,
        }])
}

fn sidebar_label(key: &'static str, reduced: bool) -> AnyElement {
    let label = div().flex_1().min_w_0().truncate().child(tr(key));
    if reduced {
        return label.into_any_element();
    }
    label
        .with_animation(
            gpui::SharedString::from(format!("sidebar-label-{key}")),
            Animation::new(std::time::Duration::from_millis(300)),
            |d, t| d.opacity(((t - 0.15) / 0.85).clamp(0., 1.)),
        )
        .into_any_element()
}

impl Workspace {
    fn change_settings(
        &mut self,
        change: impl FnOnce(&mut opsssh_store::Settings),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let old = self.store.clone();
        change(&mut self.store.settings);
        if !self.persist() {
            self.store = old;
        }
        design::apply(&self.store.settings.theme, window, cx);
        design::set_reduced_motion(self.store.settings.reduced_motion, cx);
        let font = self.store.settings.font_size as f32;
        for tab in &self.sessions {
            tab.terminal.update(cx, |t, cx| t.set_font_size(font, cx));
        }
        cx.notify();
    }
    fn confirm_delete(&mut self, profile: Profile, window: &mut Window, cx: &mut Context<Self>) {
        let workspace = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, _, cx| {
            let workspace = workspace.clone();
            let id = profile.id;
            dialog
                .title(tr("delete-title"))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_4()
                        .child(profile.name.clone())
                        .child(tr("delete-description"))
                        .child(
                            div()
                                .flex()
                                .gap_2()
                                .child(
                                    Button::new("confirm-delete")
                                        .danger()
                                        .label(tr("delete-server"))
                                        .on_click(move |_, window, cx| {
                                            let _ = workspace.update(cx, |this, cx| {
                                                let old = this.store.clone();
                                                this.store.servers.retain(|p| p.id != id);
                                                this.store.settings.last_connected.remove(&id);
                                                if !this.persist() {
                                                    this.store = old;
                                                }
                                                cx.notify();
                                            });
                                            window.close_dialog(cx);
                                        }),
                                )
                                .child(
                                    Button::new("cancel-delete")
                                        .label(tr("cancel"))
                                        .on_click(|_, window, cx| window.close_dialog(cx)),
                                ),
                        ),
                )
                .width(px(440.))
                .overlay_closable(false)
                .border_color(cx.theme().border)
        });
    }
    fn sidebar(&self, narrow: bool, cx: &mut Context<Self>) -> gpui::Div {
        let p = design::palette(cx);
        let collapsed = self.store.settings.sidebar_collapsed || narrow;
        if !self.settings_page
            && !self.help_page
            && let Some(id) = self.navigation.active
            && let Some(index) = self.session_index(id)
        {
            let tab = &self.sessions[index];
            let mut side = div()
                .w(px(if collapsed { 68. } else { 228. }))
                .h_full()
                .flex_shrink_0()
                .p_3()
                .flex()
                .flex_col()
                .gap_2()
                .bg(rgb(p.sidebar))
                .border_r_1()
                .border_color(rgb(p.border))
                .child(
                    Button::new("back-servers")
                        .ghost()
                        .icon(IconName::ArrowLeft)
                        .tooltip(tr("back-servers"))
                        .accessibility_label(tr("back-servers"))
                        .when(!collapsed, |b| {
                            b.child(sidebar_label(
                                "back-servers",
                                self.store.settings.reduced_motion,
                            ))
                        })
                        .w_full()
                        .h(px(36.))
                        .px_3()
                        .justify_start()
                        .when(collapsed, |b| b.justify_center())
                        .on_click(cx.listener(|this, _, w, cx| this.home(&GoHome, w, cx))),
                )
                .when(!collapsed, |d| {
                    d.child(
                        div()
                            .mt_4()
                            .mb_2()
                            .text_xs()
                            .text_color(rgb(p.muted))
                            .child(tr("server-workspace")),
                    )
                });
            for (i, (page, key, icon)) in [
                (
                    SessionPage::Terminal,
                    "nav-terminal",
                    gpui_kit_assets::IconName::SquareTerminal,
                ),
                (
                    SessionPage::Files,
                    "nav-files",
                    gpui_kit_assets::IconName::Folder,
                ),
                (
                    SessionPage::SshManagement,
                    "sshmgmt-title",
                    gpui_kit_assets::IconName::Shield,
                ),
                (
                    SessionPage::Infrastructure,
                    "nav-infrastructure",
                    gpui_kit_assets::IconName::LayoutDashboard,
                ),
            ]
            .into_iter()
            .enumerate()
            {
                if tab.endpoint.is_none() && page != SessionPage::Terminal {
                    continue;
                }
                side = side.child(
                    Button::new(("session-nav", i))
                        .ghost()
                        .selected(tab.page == page)
                        .icon(icon)
                        .tooltip(tr(key))
                        .accessibility_label(tr(key))
                        .when(!collapsed, |b| {
                            b.child(sidebar_label(key, self.store.settings.reduced_motion))
                        })
                        .w_full()
                        .h(px(36.))
                        .px_3()
                        .justify_start()
                        .when(collapsed, |b| b.justify_center())
                        .on_click(
                            cx.listener(move |this, _, w, cx| this.select_page(id, page, w, cx)),
                        ),
                );
            }
            return side
                .child(div().flex_1())
                .when(!collapsed, |d| {
                    d.child(
                        div()
                            .text_xs()
                            .text_color(rgb(p.muted))
                            .child(tr(if tab.protected {
                                "tmux-protected"
                            } else {
                                "session-unprotected"
                            })),
                    )
                })
                .child(
                    Button::new("collapse-session-nav")
                        .ghost()
                        .icon(if collapsed {
                            IconName::ChevronRight
                        } else {
                            IconName::ChevronLeft
                        })
                        .tooltip(tr("sidebar-toggle"))
                        .accessibility_label(tr("sidebar-toggle"))
                        .on_click(cx.listener(|this, _, w, cx| {
                            this.change_settings(
                                |s| s.sidebar_collapsed = !s.sidebar_collapsed,
                                w,
                                cx,
                            )
                        })),
                );
        }
        let mut sidebar = div()
            .w(px(if collapsed { 68. } else { 228. }))
            .flex_shrink_0()
            .h_full()
            .p_3()
            .flex()
            .flex_col()
            .gap_1()
            .bg(rgb(p.sidebar))
            .border_r_1()
            .border_color(rgb(p.border))
            .child(
                div()
                    .h(px(54.))
                    .flex_shrink_0()
                    .mx(-px(12.))
                    .mt(-px(12.))
                    .mb_2()
                    .px(px(if collapsed { 0. } else { 24. }))
                    .border_b_1()
                    .border_color(rgb(p.border))
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .when(collapsed, |d| d.justify_center())
                    .child(
                        div()
                            .size(px(32.))
                            .flex_shrink_0()
                            .rounded(px(8.))
                            .bg(design::action_gradient(false))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(
                                Icon::new(gpui_kit_assets::IconName::Terminal)
                                    .size(px(19.))
                                    .text_color(rgb(p.on_ruby)),
                            ),
                    )
                    .when(!collapsed, |d| {
                        d.child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .gap(px(2.))
                                .child(
                                    div()
                                        .text_size(px(15.))
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .child("OpsSSH"),
                                )
                                .child(
                                    div()
                                        .text_size(px(10.))
                                        .truncate()
                                        .text_color(rgb(p.muted))
                                        .child(tr("workspace-subtitle")),
                                ),
                        )
                    }),
            );
        for (i, (filter, key, icon)) in [
            (
                "All servers",
                "nav-servers",
                gpui_kit_assets::IconName::Server,
            ),
            (
                "Favourites",
                "nav-favorites",
                gpui_kit_assets::IconName::Star,
            ),
        ]
        .into_iter()
        .enumerate()
        {
            let selected = !self.settings_page
                && !self.help_page
                && self.navigation.active.is_none()
                && self.filter == filter;
            sidebar = sidebar.child(
                Button::new(("nav", i))
                    .ghost()
                    .selected(selected)
                    .icon(icon)
                    .tooltip(tr(key))
                    .accessibility_label(tr(key))
                    .when(!collapsed, |b| {
                        b.child(sidebar_label(key, self.store.settings.reduced_motion))
                    })
                    .w_full()
                    .h(px(36.))
                    .px_3()
                    .justify_start()
                    .when(collapsed, |b| b.justify_center())
                    .on_click(cx.listener(move |this, _, w, cx| {
                        this.home(&GoHome, w, cx);
                        this.filter = filter.into();
                        this.environment_filter.clear();
                        cx.notify();
                    })),
            );
        }
        if !collapsed {
            let environments = self
                .store
                .servers
                .iter()
                .map(|p| p.environment.clone())
                .filter(|s| !s.is_empty())
                .collect::<std::collections::BTreeSet<_>>();
            sidebar = sidebar.child(
                div()
                    .pt_5()
                    .pb_1()
                    .px_3()
                    .text_xs()
                    .text_color(rgb(p.muted))
                    .child(tr("nav-environments")),
            );
            for (i, environment) in environments.into_iter().enumerate() {
                sidebar = sidebar.child(
                    Button::new(("environment", i))
                        .ghost()
                        .icon(gpui_kit_assets::IconName::Box)
                        .selected(
                            self.filter == environment
                                && !self.settings_page
                                && !self.help_page
                                && self.navigation.active.is_none(),
                        )
                        .accessibility_label(environment.clone())
                        .child(div().flex_1().min_w_0().child(environment.clone()))
                        .w_full()
                        .h(px(36.))
                        .px_3()
                        .justify_start()
                        .when(collapsed, |b| b.justify_center())
                        .on_click(cx.listener(move |this, _, w, cx| {
                            this.home(&GoHome, w, cx);
                            this.filter = environment.clone();
                            this.environment_filter.clear();
                            cx.notify();
                        })),
                );
            }
        }
        sidebar
            .child(div().flex_1().min_h_0())
            .child(div().h(px(1.)).flex_shrink_0().bg(rgb(p.border)).mb_2())
            .child(
                Button::new("settings-nav")
                    .ghost()
                    .selected(self.settings_page)
                    .icon(IconName::Settings)
                    .tooltip(tr("settings"))
                    .accessibility_label(tr("settings"))
                    .when(!collapsed, |b| {
                        b.child(sidebar_label(
                            "settings",
                            self.store.settings.reduced_motion,
                        ))
                    })
                    .w_full()
                    .h(px(36.))
                    .px_3()
                    .justify_start()
                    .when(collapsed, |b| b.justify_center())
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.settings_page = true;
                        this.help_page = false;
                        this.transition = this.transition.wrapping_add(1);
                        this.sync_visibility(cx);
                        cx.notify();
                    })),
            )
            .child(
                Button::new("help-nav")
                    .ghost()
                    .selected(self.help_page)
                    .icon(IconName::Info)
                    .tooltip(tr("help"))
                    .accessibility_label(tr("help"))
                    .when(!collapsed, |b| {
                        b.child(sidebar_label("help", self.store.settings.reduced_motion))
                    })
                    .w_full()
                    .h(px(36.))
                    .px_3()
                    .justify_start()
                    .when(collapsed, |b| b.justify_center())
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.help_page = true;
                        this.settings_page = false;
                        this.transition = this.transition.wrapping_add(1);
                        this.sync_visibility(cx);
                        cx.notify();
                    })),
            )
            .child(
                Button::new("collapse-nav")
                    .ghost()
                    .icon(if collapsed {
                        IconName::ChevronRight
                    } else {
                        IconName::ChevronLeft
                    })
                    .tooltip(tr("sidebar-toggle"))
                    .accessibility_label(tr("sidebar-toggle"))
                    .when(!collapsed, |b| {
                        b.child(sidebar_label(
                            "sidebar-collapse",
                            self.store.settings.reduced_motion,
                        ))
                    })
                    .w_full()
                    .h(px(36.))
                    .px_3()
                    .justify_start()
                    .when(collapsed, |b| b.justify_center())
                    .on_click(cx.listener(|this, _, w, cx| {
                        this.change_settings(|s| s.sidebar_collapsed = !s.sidebar_collapsed, w, cx)
                    })),
            )
    }
    fn server_card(
        &self,
        profile: Profile,
        list: bool,
        index: usize,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = design::palette(cx);
        let active = self.profile_sessions(&profile, cx);
        let status = active
            .iter()
            .filter_map(|id| self.session_index(*id))
            .map(|i| self.sessions[i].terminal.read(cx).session_state())
            .find(|state| *state == opsssh_term_core::SessionState::Connected)
            .or_else(|| {
                active
                    .first()
                    .and_then(|id| self.session_index(*id))
                    .map(|i| self.sessions[i].terminal.read(cx).session_state())
            });
        let status_text = active
            .iter()
            .filter_map(|id| self.session_index(*id))
            .find(|i| Some(self.sessions[*i].terminal.read(cx).session_state()) == status)
            .map(|i| self.sessions[i].terminal.read(cx).connection_status())
            .unwrap_or_else(|| tr("connection-disconnected"));
        let connected = !active.is_empty();
        let config = profile.id == 0;
        let connect = profile.clone();
        let favorite = profile.clone();
        let edit = profile.clone();
        let duplicate = profile.clone();
        let delete = profile.clone();
        let workspace = cx.entity().downgrade();
        let menu = Button::new(("server-menu", index))
            .small()
            .ghost()
            .icon(IconName::Ellipsis)
            .tooltip(tr("server-actions"))
            .accessibility_label(tr("server-actions"))
            .dropdown_menu(move |menu, _, _| {
                let weak = workspace.clone();
                let profile = edit.clone();
                let menu = menu.item(
                    PopupMenuItem::new(tr(if config {
                        "customize-server"
                    } else {
                        "edit-server"
                    }))
                    .on_click(move |_, w, cx| {
                        let _ = weak.update(cx, |this, cx| this.edit(profile.clone(), w, cx));
                    }),
                );
                let copied = edit.clone();
                let menu = menu.item(PopupMenuItem::new(tr("copy-ssh-command")).on_click(
                    move |_, window, cx| match crate::connections::ssh_command(&copied) {
                        Ok(command) => {
                            cx.write_to_clipboard(gpui::ClipboardItem::new_string(command));
                            window.push_notification(tr("ssh-command-copied"), cx);
                        }
                        Err(error) => window.push_notification(error, cx),
                    },
                ));
                let weak = workspace.clone();
                let profile = duplicate.clone();
                let menu = menu.item(PopupMenuItem::new(tr("duplicate-server")).on_click(
                    move |_, w, cx| {
                        let _ = weak.update(cx, |this, cx| {
                            let mut p = profile.clone();
                            p.id = 0;
                            p.name = format!("{} ({})", p.name, tr("copy-label"));
                            this.edit(p, w, cx);
                        });
                    },
                ));
                if config {
                    return menu;
                }
                let weak = workspace.clone();
                let profile = delete.clone();
                menu.separator()
                    .item(
                        PopupMenuItem::new(tr("delete-server")).on_click(move |_, w, cx| {
                            let _ = weak
                                .update(cx, |this, cx| this.confirm_delete(profile.clone(), w, cx));
                        }),
                    )
            });
        let auth = tr(match profile.auth {
            opsssh_store::Auth::Agent => "auth-agent",
            opsssh_store::Auth::Password => "auth-password",
            opsssh_store::Auth::Key => "auth-key",
        });
        let timestamp = self.store.settings.last_connected.get(&profile.id).copied();
        let last = if config {
            tr("config-alias")
        } else {
            last_connected(timestamp)
        };
        let key_path = if profile.identity_file.is_empty() {
            auth.clone()
        } else {
            profile.identity_file.clone()
        };
        let key_tooltip = key_path.clone();
        let address_tooltip = format!("{}@{}:{}", profile.user, profile.host, profile.port);
        let name_tooltip = profile.name.clone();
        let state_color = if status == Some(opsssh_term_core::SessionState::Connected) {
            cx.theme().success
        } else {
            rgb(p.muted).into()
        };
        let action = if connected {
            Button::new(("connect", index))
                .w(px(112.))
                .label(tr("disconnect"))
                .on_click(cx.listener(move |this, _, w, cx| {
                    this.disconnect_profile(&connect, w, cx);
                }))
                .into_any_element()
        } else {
            crate::design::PrimaryAction::new(("connect", index))
                .w(px(112.))
                .label(tr("connect-server"))
                .on_click(cx.listener(move |this, _, w, cx| {
                    this.connect(connect.clone(), w, cx);
                }))
                .into_any_element()
        };
        let identity = div()
            .when(list, |d| d.flex_1())
            .min_w_0()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .size(px(8.))
                            .flex_shrink_0()
                            .rounded_full()
                            .bg(state_color),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .id(("profile-name", index))
                            .tooltip(move |window, cx| {
                                gpui_component::tooltip::Tooltip::new(format!(
                                    "{} \u{b7} {}",
                                    name_tooltip, last
                                ))
                                .build(window, cx)
                            })
                            .child(profile.name.clone()),
                    )
                    .when(!config, |d| {
                        d.child(
                            Button::new(("favorite", index))
                                .small()
                                .ghost()
                                .icon(IconName::Star)
                                .selected(profile.favorite)
                                .tooltip(tr(if profile.favorite {
                                    "unfavorite-server"
                                } else {
                                    "favorite-server"
                                }))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    let old = this.store.clone();
                                    if let Some(p) =
                                        this.store.servers.iter_mut().find(|p| p.id == favorite.id)
                                    {
                                        p.favorite = !p.favorite;
                                    }
                                    if !this.persist() {
                                        this.store = old;
                                    }
                                    cx.notify();
                                })),
                        )
                    })
                    .child(menu),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(p.muted))
                    .truncate()
                    .font_family(cx.theme().mono_font_family.clone())
                    .id(("profile-address", index))
                    .tooltip(move |window, cx| {
                        gpui_component::tooltip::Tooltip::new(address_tooltip.clone())
                            .build(window, cx)
                    })
                    .child(format!(
                        "{}@{}:{}",
                        profile.user, profile.host, profile.port
                    )),
            );
        let details = div()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_3()
            .text_sm()
            .when(!list, |d| d.pt_3().border_t_1().border_color(rgb(p.border)))
            .child(
                div()
                    .flex()
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .w(px(92.))
                            .flex_shrink_0()
                            .text_color(rgb(p.muted))
                            .child(tr("card-environment")),
                    )
                    .child(div().flex_1().min_w_0().truncate().child(
                        if profile.environment.is_empty() {
                            tr("card-unassigned")
                        } else {
                            profile.environment.clone()
                        },
                    )),
            )
            .child(
                div()
                    .flex()
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .w(px(92.))
                            .flex_shrink_0()
                            .text_color(rgb(p.muted))
                            .child(tr(if profile.identity_file.is_empty() {
                                "card-authentication"
                            } else {
                                "card-key"
                            })),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .font_family(cx.theme().mono_font_family.clone())
                            .id(("key-path", index))
                            .tooltip(move |window, cx| {
                                gpui_component::tooltip::Tooltip::new(key_tooltip.clone())
                                    .build(window, cx)
                            })
                            .child(key_path),
                    ),
            )
            .when(!profile.proxy_jump.is_empty(), |d| {
                d.child(
                    div()
                        .truncate()
                        .text_xs()
                        .text_color(rgb(p.muted))
                        .child(format!("{} {}", tr("via-label"), profile.proxy_jump)),
                )
            })
            .when(!profile.proxy_command.is_empty(), |d| {
                d.child(
                    div()
                        .text_xs()
                        .text_color(rgb(p.muted))
                        .child(tr("proxy-label")),
                )
            });
        div()
            .id(("server-card", index))
            .h(px(if list { 124. } else { 240. }))
            .overflow_hidden()
            .p_5()
            .rounded(px(7.))
            .bg(rgb(p.surface))
            .border_1()
            .border_color(rgb(p.border))
            .hover(|d| card_hover(d, p))
            .when(self.hovered_card == Some(index), |d| card_hover(d, p))
            .flex()
            .gap_3()
            .when(!list, |d| d.flex_col())
            .when(list, |d| d.items_center())
            .child(identity)
            .child(div().when(list, |d| d.w(px(270.))).child(details))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .when(!list, |d| d.mt_auto())
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_sm()
                            .text_color(rgb(p.muted))
                            .child(div().size(px(6.)).rounded_full().bg(state_color))
                            .child(div().truncate().child(status_text)),
                    )
                    .child(action),
            )
            .on_hover(cx.listener(move |this, hovered, _, cx| {
                this.hovered_card = if *hovered { Some(index) } else { None };
                cx.notify();
            }))
            .with_spring(
                ("server-motion", index),
                SpringAnimation::new(SpringConfig::new(180., 27., 1.))
                    .to(if self.hovered_card == Some(index) {
                        -2.
                    } else {
                        0.
                    })
                    .from(0.),
                |d, offset| d.relative().top(px(offset)),
            )
            .into_any_element()
    }
    fn servers(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = design::palette(cx);
        let query = self.search.read(cx).value();
        let profiles = Arc::new(self.visible_profiles(&query));
        let list = self.store.settings.server_view != "cards";
        let sidebar_width =
            if self.store.settings.sidebar_collapsed || window.viewport_size().width < px(1100.) {
                68.
            } else {
                228.
            };
        let available = f32::from(window.viewport_size().width) - sidebar_width - 80.;
        let columns = if list {
            1
        } else {
            ((available + 16.) / 316.).floor().clamp(1., 3.) as usize
        };
        let width = (available - 16. * (columns - 1) as f32) / columns as f32;
        let rows = profiles.len().div_ceil(columns);
        let title = match self.filter.as_str() {
            "All servers" => tr("nav-servers"),
            "Favourites" => tr("nav-favorites"),
            other => other.into(),
        };
        let environments = self
            .store
            .servers
            .iter()
            .map(|p| p.environment.clone())
            .filter(|e| !e.is_empty())
            .collect::<std::collections::BTreeSet<_>>();
        let workspace = cx.entity().downgrade();
        let environment_menu = Button::new("environment-filter")
            .icon(gpui_kit_assets::IconName::ListFilter)
            .label(if self.environment_filter.is_empty() {
                tr("all-environments")
            } else {
                self.environment_filter.clone()
            })
            .dropdown_menu(move |menu, _, _| {
                let weak = workspace.clone();
                let mut menu = menu.item(PopupMenuItem::new(tr("all-environments")).on_click(
                    move |_, window, cx| {
                        let _ = weak.update(cx, |this, cx| {
                            this.home(&GoHome, window, cx);
                            this.environment_filter.clear();
                            cx.notify();
                        });
                    },
                ));
                for environment in &environments {
                    let weak = workspace.clone();
                    let value = environment.clone();
                    menu = menu.item(PopupMenuItem::new(environment.clone()).on_click(
                        move |_, window, cx| {
                            let _ = weak.update(cx, |this, cx| {
                                this.home(&GoHome, window, cx);
                                this.environment_filter = value.clone();
                                cx.notify();
                            });
                        },
                    ));
                }
                menu
            });
        let mut page = div()
            .size_full()
            .p_8()
            .flex()
            .flex_col()
            .gap_4()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .pb_4()
                    .gap_3()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(p.muted))
                                    .child(format!("Workspace / {}", title)),
                            )
                            .child(
                                div()
                                    .text_2xl()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child(title),
                            )
                            .child(div().text_sm().text_color(rgb(p.muted)).child(format!(
                                    "{} {} \u{b7} {} {}",
                                    profiles.len(),
                                    tr("servers-count"),
                                    profiles
                                        .iter()
                                        .filter(|profile| self
                                            .profile_sessions(profile, cx)
                                            .iter()
                                            .any(|id| self.session_index(*id).is_some_and(
                                                |i| self.sessions[i]
                                                    .terminal
                                                    .read(cx)
                                                    .session_state()
                                                    == opsssh_term_core::SessionState::Connected
                                            )))
                                        .count(),
                                    tr("active-connections")
                                ))),
                    )
                    .child(
                        crate::design::PrimaryAction::new("new-server")
                            .icon(IconName::Plus)
                            .label(tr("new-server"))
                            .on_click(
                                cx.listener(|this, _, w, cx| this.new_server(&NewServer, w, cx)),
                            ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .flex_wrap()
                    .py_3()
                    .border_t_1()
                    .border_b_1()
                    .border_color(rgb(p.border))
                    .child(
                        div().flex_1().min_w(px(220.)).child(
                            Input::new(&self.search)
                                .prefix(Icon::new(IconName::Search).text_color(rgb(p.muted)))
                                .cleanable(true)
                                .aria_label(tr("search-servers")),
                        ),
                    )
                    .child(environment_menu)
                    .child(
                        div()
                            .flex()
                            .gap_1()
                            .p_1()
                            .rounded(px(6.))
                            .border_1()
                            .border_color(rgb(p.border))
                            .child(
                                Button::new("view-cards")
                                    .ghost()
                                    .selected(!list)
                                    .toggled(!list)
                                    .icon(IconName::LayoutDashboard)
                                    .tooltip(tr("view-cards"))
                                    .accessibility_label(tr("view-cards"))
                                    .on_click(cx.listener(|this, _, w, cx| {
                                        this.change_settings(
                                            |s| s.server_view = "cards".into(),
                                            w,
                                            cx,
                                        )
                                    })),
                            )
                            .child(
                                Button::new("view-list")
                                    .ghost()
                                    .selected(list)
                                    .toggled(list)
                                    .icon(gpui_kit_assets::IconName::List)
                                    .tooltip(tr("view-list"))
                                    .accessibility_label(tr("view-list"))
                                    .on_click(cx.listener(|this, _, w, cx| {
                                        this.change_settings(
                                            |s| s.server_view = "list".into(),
                                            w,
                                            cx,
                                        )
                                    })),
                            ),
                    )
                    .child(
                        Button::new("sort")
                            .label(tr(if self.store.settings.sort == "last_connected" {
                                "sort-recent"
                            } else {
                                "sort-name"
                            }))
                            .dropdown_menu({
                                let weak = cx.entity().downgrade();
                                move |mut menu, _, _| {
                                    for (value, label) in
                                        [("name", "sort-name"), ("last_connected", "sort-recent")]
                                    {
                                        let weak = weak.clone();
                                        menu = menu.item(PopupMenuItem::new(tr(label)).on_click(
                                            move |_, window, cx| {
                                                let _ = weak.update(cx, |this, cx| {
                                                    this.change_settings(
                                                        |settings| settings.sort = value.into(),
                                                        window,
                                                        cx,
                                                    );
                                                });
                                            },
                                        ));
                                    }
                                    menu
                                }
                            }),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .text_xs()
                    .text_color(rgb(p.muted))
                    .child(tr("card-collection"))
                    .child(format!("{} {}", profiles.len(), tr("card-shown"))),
            );
        if rows == 0 {
            let first =
                self.store.servers.is_empty() && query.is_empty() && self.filter == "All servers";
            page = page.child(
                div().flex_1().flex().items_center().justify_center().child(
                    div()
                        .max_w(px(500.))
                        .p_6()
                        .flex()
                        .flex_col()
                        .gap_4()
                        .child(
                            Icon::new(gpui_kit_assets::IconName::Server).text_color(rgb(p.muted)),
                        )
                        .child(div().text_xl().child(tr(if first {
                            "empty-title"
                        } else {
                            "no-results-title"
                        })))
                        .child(div().text_sm().text_color(rgb(p.muted)).child(tr(if first {
                            "empty-description"
                        } else {
                            "no-results-description"
                        })))
                        .when(first, |d| {
                            d.child(
                                crate::design::PrimaryAction::new("empty-new")
                                    .label(tr("new-server"))
                                    .on_click(cx.listener(|this, _, w, cx| {
                                        this.new_server(&NewServer, w, cx)
                                    })),
                            )
                            .child(
                                Button::new("empty-local")
                                    .ghost()
                                    .label(tr("open-local"))
                                    .on_click(cx.listener(|this, _, w, cx| {
                                        this.open_local(&OpenLocalTerminal, w, cx)
                                    })),
                            )
                        }),
                ),
            );
        } else {
            page = page.child(
                uniform_list(
                    "server-list",
                    rows,
                    cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                        range
                            .map(|row| {
                                div()
                                    // Leave room inside the scroll clip for the hover lift and glow.
                                    .h(px(if list { 148. } else { 264. }))
                                    .pt_2()
                                    .pb_4()
                                    .flex()
                                    .gap_4()
                                    .children(
                                        (row * columns..((row + 1) * columns).min(profiles.len()))
                                            .map(|index| {
                                                div().w(px(width.max(200.))).child(
                                                    this.server_card(
                                                        profiles[index].clone(),
                                                        list,
                                                        index,
                                                        cx,
                                                    ),
                                                )
                                            }),
                                    )
                            })
                            .collect()
                    }),
                )
                .flex_1()
                .min_h_0()
                .w_full(),
            );
        }
        page.into_any_element()
    }
    fn settings(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = design::palette(cx);
        div()
            .id("settings-page")
            .size_full()
            .overflow_y_scroll()
            .p_6()
            .flex()
            .flex_col()
            .gap_6()
            .child(
                div()
                    .text_2xl()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(tr("settings")),
            )
            .child(
                div()
                    .max_w(px(680.))
                    .py_4()
                    .border_b_1()
                    .border_color(rgb(p.border))
                    .flex()
                    .flex_col()
                    .gap_4()
                    .child(div().text_lg().child(tr("settings-appearance")))
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgb(p.muted))
                            .child(tr("settings-theme-help")),
                    )
                    .child(
                        div().flex().gap_2().children(
                            [
                                ("dark", "theme-dark"),
                                ("light", "theme-light"),
                                ("system", "theme-system"),
                            ]
                            .into_iter()
                            .map(|(value, key)| {
                                Button::new(key)
                                    .selected(self.store.settings.theme == value)
                                    .label(tr(key))
                                    .on_click(cx.listener(move |this, _, w, cx| {
                                        this.change_settings(|s| s.theme = value.into(), w, cx)
                                    }))
                            }),
                        ),
                    )
                    .child(
                        Switch::new("motion-setting")
                            .checked(self.store.settings.reduced_motion)
                            .label(tr("settings-reduced-motion"))
                            .on_click(cx.listener(|this, checked, w, cx| {
                                this.change_settings(|s| s.reduced_motion = *checked, w, cx)
                            })),
                    ),
            )
            .child(
                div()
                    .max_w(px(680.))
                    .py_4()
                    .border_b_1()
                    .border_color(rgb(p.border))
                    .flex()
                    .flex_col()
                    .gap_4()
                    .child(div().text_lg().child(tr("settings-terminal")))
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgb(p.muted))
                            .child(tr("settings-font-help")),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(
                                Button::new("font-down")
                                    .label("−")
                                    .tooltip(tr("font-decrease"))
                                    .accessibility_label(tr("font-decrease"))
                                    .on_click(cx.listener(|this, _, w, cx| {
                                        this.change_settings(
                                            |s| s.font_size = s.font_size.saturating_sub(1).max(8),
                                            w,
                                            cx,
                                        )
                                    })),
                            )
                            .child(format!("{} px", self.store.settings.font_size))
                            .child(
                                Button::new("font-up")
                                    .label("+")
                                    .tooltip(tr("font-increase"))
                                    .accessibility_label(tr("font-increase"))
                                    .on_click(cx.listener(|this, _, w, cx| {
                                        this.change_settings(
                                            |s| s.font_size = (s.font_size + 1).min(48),
                                            w,
                                            cx,
                                        )
                                    })),
                            ),
                    ),
            )
            .child(self.shortcuts(cx))
            .child(
                div()
                    .max_w(px(680.))
                    .py_4()
                    .border_b_1()
                    .border_color(rgb(p.border))
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(div().text_lg().child(tr("settings-data")))
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgb(p.muted))
                            .child(tr("settings-data-help")),
                    )
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .child(
                                Button::new("import")
                                    .label(tr("import-servers"))
                                    .on_click(cx.listener(|this, _, w, cx| this.import(w, cx))),
                            )
                            .child(
                                Button::new("export")
                                    .label(tr("export-servers"))
                                    .on_click(cx.listener(|this, _, w, cx| this.export(w, cx))),
                            ),
                    ),
            )
            .into_any_element()
    }
    fn shortcuts(&self, cx: &App) -> gpui::Div {
        let p = design::palette(cx);
        let modifier = if cfg!(target_os = "macos") {
            "Cmd"
        } else {
            "Ctrl"
        };
        div()
            .max_w(px(680.))
            .py_4()
            .border_b_1()
            .border_color(rgb(p.border))
            .flex()
            .flex_col()
            .gap_3()
            .child(div().text_lg().child(tr("settings-shortcuts")))
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(p.muted))
                    .child(tr("settings-terminal-shortcuts")),
            )
            .children(
                [
                    ("N", "new-server"),
                    ("K", "search-servers"),
                    ("Enter", "open-local"),
                    ("W", "close-tab"),
                    ("Shift+H", "home-title"),
                ]
                .into_iter()
                .map(|(key, label)| {
                    div().flex().justify_between().child(tr(label)).child(
                        div()
                            .font_family(cx.theme().mono_font_family.clone())
                            .text_color(rgb(p.muted))
                            .child(format!("{modifier}+{key}")),
                    )
                }),
            )
    }
    fn session(&self, index: usize, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = design::palette(cx);
        let tab = &self.sessions[index];
        let id = tab.id;
        let terminal = tab.terminal.clone();
        let files = tab.files.clone();
        let visible = tab.files_visible;
        let follow = tab.follow;
        let status = terminal.read(cx).connection_status();
        let state = terminal.read(cx).session_state();
        let live = state == opsssh_term_core::SessionState::Connected;
        let endpoint = tab.endpoint.clone();
        let copy = endpoint.clone().unwrap_or_default();
        let header = div()
            .min_h(px(76.))
            .flex_shrink_0()
            .px_5()
            .py_3()
            .flex()
            .items_center()
            .gap_3()
            .border_b_1()
            .border_color(rgb(p.border))
            .bg(rgb(p.sidebar))
            .child(
                div()
                    .size(px(40.))
                    .rounded(px(7.))
                    .bg(design::action_gradient(false))
                    .text_color(rgb(0xffffff))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_lg()
                    .child(">_"),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_lg()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(tab.name.clone()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(p.muted))
                            .font_family(cx.theme().mono_font_family.clone())
                            .truncate()
                            .child(endpoint.unwrap_or_else(|| tr("local-session"))),
                    ),
            )
            .when(tab.endpoint.is_some(), |d| {
                d.child(
                    Button::new("copy-endpoint")
                        .ghost()
                        .icon(IconName::Copy)
                        .tooltip(tr("copy-endpoint"))
                        .accessibility_label(tr("copy-endpoint"))
                        .on_click(move |_, window, cx| {
                            cx.write_to_clipboard(gpui::ClipboardItem::new_string(copy.clone()));
                            window.push_notification(tr("address-copied"), cx);
                        }),
                )
            })
            .when(!tab.environment.is_empty(), |d| {
                d.child(
                    div()
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .border_1()
                        .border_color(rgb(tab.color))
                        .text_xs()
                        .text_color(rgb(p.muted))
                        .child(tab.environment.clone()),
                )
            })
            .when(tab.endpoint.is_some(), |d| {
                d.child(
                    div()
                        .text_xs()
                        .text_color(rgb(p.muted))
                        .child(tr(if tab.protected {
                            "tmux-protected"
                        } else {
                            "session-unprotected"
                        })),
                )
            })
            .when(tab.connected_at.is_some(), |d| {
                d.child(div().text_xs().text_color(rgb(p.muted)).child(format!(
                    "{}m",
                    tab.connected_at.unwrap().elapsed().as_secs() / 60
                )))
            })
            .child(
                div()
                    .px_3()
                    .py_2()
                    .rounded(px(6.))
                    .bg(rgb(p.surface))
                    .border_1()
                    .border_color(rgb(p.border))
                    .text_xs()
                    .text_color(if live {
                        cx.theme().success
                    } else {
                        cx.theme().warning
                    })
                    .child(format!(
                        "{} {}",
                        if live { "\u{25cf}" } else { "\u{25cb}" },
                        status
                    )),
            )
            .when(
                matches!(
                    state,
                    opsssh_term_core::SessionState::Disconnected
                        | opsssh_term_core::SessionState::Closed
                ) && tab.endpoint.is_some(),
                |d| {
                    d.child(
                        crate::design::PrimaryAction::new("reconnect-server")
                            .label(tr("reconnect-server"))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                if let Some(i) = this.session_index(id) {
                                    this.sessions[i]
                                        .terminal
                                        .update(cx, |t, cx| t.reconnect(window, cx));
                                    this.sessions[i].files_needs_rebind = true;
                                    this.terminal_focus_released = false;
                                    this.sync_visibility(cx);
                                    this.update_keyboard_capture(window, cx);
                                    cx.notify();
                                }
                            })),
                    )
                },
            )
            .when(state == opsssh_term_core::SessionState::Reconnecting, |d| {
                d.child(
                    Button::new("stop-recovery")
                        .ghost()
                        .label(tr("stop-reconnecting"))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(i) = this.session_index(id) {
                                this.sessions[i]
                                    .terminal
                                    .update(cx, |t, cx| t.disconnect(cx));
                            }
                            this.sync_visibility(cx);
                            cx.notify();
                        })),
                )
            });
        let body = match tab.page {
            SessionPage::Files => files
                .clone()
                .map(|f| div().size_full().child(f).into_any_element())
                .unwrap_or_else(|| {
                    div()
                        .p_6()
                        .child(tr("infra-connect-first"))
                        .into_any_element()
                }),
            SessionPage::Infrastructure => tab
                .infrastructure
                .clone()
                .map(|view| div().size_full().child(view).into_any_element())
                .unwrap_or_else(|| {
                    div()
                        .p_6()
                        .child(tr("infra-connect-first"))
                        .into_any_element()
                }),
            SessionPage::SshManagement => tab
                .ssh_management
                .clone()
                .map(|view| div().size_full().child(view).into_any_element())
                .unwrap_or_else(|| {
                    div()
                        .p_6()
                        .child(tr("sshmgmt-connect-first"))
                        .into_any_element()
                }),
            SessionPage::Terminal => div()
                .size_full()
                .flex()
                .flex_col()
                .min_h_0()
                .p_4()
                .gap_3()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .text_sm()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child(tr("nav-terminal")),
                        )
                        .child(div().flex_1())
                        .when(tab.endpoint.is_some(), |d| {
                            d.when(
                                self.capture.is_some()
                                    || (cfg!(feature = "capture")
                                        && self.load_failed
                                        && opsssh_platform::info().os == "windows"),
                                |d| {
                                    d.child(
                                        Button::new("keyboard-capture")
                                            .ghost()
                                            .selected(tab.keyboard_capture)
                                            .label(tr(if !tab.keyboard_capture {
                                                "keyboard-released"
                                            } else if !live {
                                                "keyboard-paused"
                                            } else {
                                                "keyboard-captured"
                                            }))
                                            .tooltip(tr("keyboard-capture-help"))
                                            .on_click(cx.listener(move |this, _, window, cx| {
                                                if let Some(index) = this.session_index(id) {
                                                    this.terminal_focus_released = false;
                                    this.sessions[index].keyboard_capture =
                                                        !this.sessions[index].keyboard_capture;
                                                    this.sessions[index]
                                                        .terminal
                                                        .update(cx, |terminal, cx| {
                                                            terminal.focus(window, cx)
                                                        });
                                                }
                                                this.update_keyboard_capture(window, cx);
                                                cx.notify();
                                            })),
                                    )
                                },
                            )
                            .child(
                                Button::new("terminal-upload-destination")
                                    .ghost()
                                    .label(tr("upload-destination"))
                                    .tooltip(
                                        tab.upload_directory
                                            .clone()
                                            .unwrap_or_else(|| tr("upload-destination-help")),
                                    )
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.choose_upload_destination(id, vec![], window, cx)
                                    })),
                            )
                        })
                        .when(tab.endpoint.is_some(), |d| {
                            d.child(
                                Button::new("toggle-files")
                                    .ghost()
                                    .selected(visible)
                                    .icon(IconName::Folder)
                                    .label(tr("files"))
                                    .tooltip(tr("files"))
                                    .accessibility_label(tr("files"))
                                    .on_click(cx.listener(move |this, _, w, cx| {
                                        let Some(i) = this.session_index(id) else {
                                            return;
                                        };
                                        if this.sessions[i].files_visible {
                                            this.close_files(id,w,cx);
                                        } else {
                                            let t = this.sessions[i].terminal.clone();
                                            this.ensure_files(&t, w, cx);
                                        }
                                        cx.notify();
                                    })),
                            )
                            .when(visible, |d| {
                                d.child(
                                    Button::new("follow-folder")
                                        .ghost()
                                        .selected(follow)
                                        .label(tr("follow-folder"))
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            let Some(i) = this.session_index(id) else {
                                                return;
                                            };
                                            let tab = &mut this.sessions[i];
                                            tab.follow = !tab.follow;
                                            if tab.follow
                                                && let (Some(path), Some(files)) = (
                                                    tab.terminal
                                                        .read(cx)
                                                        .current_directory()
                                                        .map(str::to_owned),
                                                    tab.files.clone(),
                                                )
                                            {
                                                files.update(cx, |f, cx| {
                                                    f.follow_directory(path.clone(), cx)
                                                });
                                                tab.followed_directory = Some(path);
                                            }
                                            cx.notify();
                                        })),
                                )
                            })
                        }),
                )
                .when_some(files.clone(), |d, files| {
                    d.child(files.update(cx, |files, cx| {
                        files.terminal_tray(self.store.settings.reduced_motion, cx)
                    }))
                })
                .child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .rounded(px(7.))
                        .overflow_hidden()
                        .border_1()
                        .border_color(rgb(p.border))
                        .flex()
                        .child(div().flex_1().min_w(px(480.)).min_h_0().child(terminal))
                        .when_some(files.clone(),|d,files| {
                            let available=f32::from(window.viewport_size().width)-if self.store.settings.sidebar_collapsed || window.viewport_size().width<px(1100.){68.}else{228.}-32.;
                            let dragging=self.panel_drag.is_some();
                            let desired=if tab.files_width>0. {tab.files_width}else{available/3.};
                            let width=desired.clamp(280.,(available-480.).max(280.));
                            d.child(div().id(("files-panel",id.0 as usize)).flex_shrink_0().min_h_0().overflow_hidden().flex()
                                .with_spring(("files-panel-width",id.0 as usize),SpringAnimation::new(SpringConfig::new(250.,32.,1.)).to(if visible{width}else{0.}).from(if visible && tab.files_width>0. {width} else {0.}),move|d,value|d.w(px(if dragging && visible {width} else {value.max(0.)})))
                                .child(Button::new(("files-divider",id.0 as usize)).ghost().w(px(6.)).h_full().px_0().disabled(!visible).cursor(gpui::CursorStyle::ResizeLeftRight).tooltip(tr("resize-remote-files")).accessibility_label(tr("resize-remote-files"))
                                    .on_mouse_down(gpui::MouseButton::Left,cx.listener(move|this,event:&gpui::MouseDownEvent,_,cx|{this.panel_drag=Some((id,f32::from(event.position.x),width));cx.stop_propagation();}))
                                    .on_key_down(cx.listener(move|this,event:&gpui::KeyDownEvent,window,cx|{let delta=match event.keystroke.key.as_str(){"left"=>16.,"right"=>-16.,_=>return};if let Some(i)=this.session_index(id){this.sessions[i].files_width=(width+delta).clamp(280.,(available-480.).max(280.));this.remember_files(id,cx);cx.notify();}window.prevent_default();cx.stop_propagation();}))
                                    .child(div().w(px(1.)).h_full().bg(rgb(p.border))))
                                .child(div().flex_1().min_w_0().h_full().when(visible,|d|d.child(files))))
                        }),
                )
                .into_any_element(),
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .min_h_0()
            .child(header)
            .child(div().flex_1().min_h_0().child(body))
            .into_any_element()
    }
    fn background_sessions(&self, cx: &mut Context<Self>) -> gpui::Div {
        let p = design::palette(cx);
        let hidden = self
            .sessions
            .iter()
            .filter(|s| !self.navigation.open.contains(&s.id))
            .collect::<Vec<_>>();
        div().when(!hidden.is_empty(), |d| {
            d.p_3()
                .rounded(px(7.))
                .border_1()
                .border_color(rgb(p.border))
                .bg(rgb(p.surface))
                .flex()
                .flex_col()
                .gap_2()
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(p.muted))
                        .child(tr("background-sessions")),
                )
                .child(
                    div()
                        .id("background-session-strip")
                        .flex()
                        .gap_2()
                        .overflow_x_scroll()
                        .children(hidden.into_iter().map(|s| {
                            let id = s.id;
                            Button::new(("reopen", id.0 as usize))
                                .ghost()
                                .icon(IconName::SquareTerminal)
                                .label(format!(
                                    "{} · {}",
                                    s.name,
                                    s.terminal.read(cx).connection_status()
                                ))
                                .tooltip(tr("reopen-session"))
                                .on_click(cx.listener(move |this, _, w, cx| {
                                    this.activate_session(id, w, cx)
                                }))
                        })),
                )
        })
    }
    pub(super) fn render_workspace(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let p = design::palette(cx);
        let narrow = window.viewport_size().width < px(1100.);
        let home = self.navigation.active.is_none() && !self.settings_page && !self.help_page;
        let terminal_open = self.terminal_visible();
        if terminal_open
            && !self.terminal_focus_released
            && (window.focused(cx).is_none() || self.home_focus.is_focused(window))
        {
            self.restore_terminal_focus(window, cx);
        }
        self.flush_uploaded_paths(window, cx);
        if self
            .capture
            .as_ref()
            .is_some_and(|capture| capture.has_failed())
        {
            self.message = tr("keyboard-capture-failed");
            for tab in &mut self.sessions {
                tab.keyboard_capture = false;
            }
            self.capture = None;
        }
        self.update_keyboard_capture(window, cx);
        let tabs = self
            .navigation
            .open
            .iter()
            .filter_map(|id| self.session_index(*id))
            .map(|index| {
                let tab = &self.sessions[index];
                let id = tab.id;
                let selected =
                    self.navigation.active == Some(id) && !self.settings_page && !self.help_page;
                div()
                    .id(("tab-shell", id.0 as usize))
                    .flex()
                    .items_center()
                    .gap_1()
                    .h_full()
                    .min_w(px(180.))
                    .max_w(px(260.))
                    .px_1()
                    .bg(rgb(if selected { p.surface } else { p.sidebar }))
                    .border_r_1()
                    .border_color(rgb(p.border))
                    .relative()
                    .when(selected, |d| {
                        d.child(
                            div()
                                .absolute()
                                .bottom_0()
                                .left_0()
                                .h(px(2.))
                                .bg(rgb(p.ruby))
                                .w_full(),
                        )
                    })
                    .child(
                        Button::new(("tab", id.0 as usize))
                            .ghost()
                            .h_full()
                            .rounded_none()
                            .icon(IconName::SquareTerminal)
                            .accessibility_label(tab.name.clone())
                            .child(
                                div()
                                    .flex()
                                    .min_w_0()
                                    .items_center()
                                    .gap_1()
                                    .child(
                                        div()
                                            .text_color(
                                                if tab.terminal.read(cx).session_state()
                                                    == opsssh_term_core::SessionState::Connected
                                                {
                                                    cx.theme().success
                                                } else {
                                                    rgb(p.muted).into()
                                                },
                                            )
                                            .child("\u{25cf}"),
                                    )
                                    .child(div().min_w_0().truncate().child(tab.name.clone())),
                            )
                            .tooltip(format!(
                                "{} · {}",
                                tab.name,
                                tab.terminal.read(cx).connection_status()
                            ))
                            .max_w(px(200.))
                            .on_click(
                                cx.listener(move |this, _, w, cx| this.activate_session(id, w, cx)),
                            ),
                    )
                    .child(
                        Button::new(("close-tab", id.0 as usize))
                            .ghost()
                            .icon(IconName::Close)
                            .tooltip(tr("close-tab"))
                            .accessibility_label(format!("{} {}", tr("close-tab"), tab.name))
                            .on_click(
                                cx.listener(move |this, _, w, cx| this.request_close(id, w, cx)),
                            ),
                    )
            })
            .collect::<Vec<_>>();
        let content = if self.settings_page {
            self.settings(cx)
        } else if self.help_page {
            div()
                .id("help-page")
                .size_full()
                .overflow_y_scroll()
                .p_6()
                .flex()
                .flex_col()
                .gap_6()
                .child(div().text_2xl().child(tr("help")))
                .child(
                    div()
                        .text_sm()
                        .text_color(rgb(p.muted))
                        .max_w(px(680.))
                        .child(tr("help-description")),
                )
                .child(self.shortcuts(cx))
                .into_any_element()
        } else if let Some(index) = self.navigation.active.and_then(|id| self.session_index(id)) {
            self.session(index, window, cx)
        } else {
            self.servers(window, cx)
        };
        div()
            .id("opsssh-workspace")
            .font_family(cx.theme().font_family.clone())
            .key_context(if terminal_open {
                "OpsSSH OpsSSHTerminalOpen"
            } else if home {
                "OpsSSH OpsSSHHome"
            } else {
                "OpsSSH"
            })
            .track_focus(&self.home_focus)
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(p.canvas))
            .text_color(rgb(p.text))
            .on_action(cx.listener(Self::open_local))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::new_server))
            .on_action(cx.listener(Self::close_tab))
            .on_action(cx.listener(Self::quit))
            .on_mouse_move(
                cx.listener(|this, event: &gpui::MouseMoveEvent, window, cx| {
                    if let Some((id, start, start_width)) = this.panel_drag
                        && let Some(i) = this.session_index(id)
                    {
                        let available = f32::from(window.viewport_size().width)
                            - if this.store.settings.sidebar_collapsed
                                || window.viewport_size().width < px(1100.)
                            {
                                68.
                            } else {
                                228.
                            }
                            - 32.;
                        this.sessions[i].files_width = (start_width + start
                            - f32::from(event.position.x))
                        .clamp(280., (available - 480.).max(280.));
                        cx.notify();
                    }
                }),
            )
            .on_mouse_up(
                gpui::MouseButton::Left,
                cx.listener(|this, _, _, cx| this.finish_panel_drag(cx)),
            )
            .on_mouse_up_out(
                gpui::MouseButton::Left,
                cx.listener(|this, _, _, cx| this.finish_panel_drag(cx)),
            )
            .child(
                TitleBar::new()
                    .h(px(32.))
                    .bg(rgb(p.sidebar))
                    .on_close_window(
                        cx.listener(|this, _, window, cx| this.quit(&Quit, window, cx)),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_xs()
                            .child(
                                Icon::new(gpui_kit_assets::IconName::Terminal)
                                    .size(px(14.))
                                    .text_color(rgb(p.ruby)),
                            )
                            .child("OpsSSH"),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .child(
                        self.sidebar(narrow, cx).overflow_hidden().with_spring(
                            "sidebar-width",
                            SpringAnimation::new(SpringConfig::new(180., 27., 1.))
                                .to(if self.store.settings.sidebar_collapsed || narrow {
                                    68.
                                } else {
                                    228.
                                })
                                .from(if self.store.settings.sidebar_collapsed || narrow {
                                    68.
                                } else {
                                    228.
                                }),
                            |d, width| d.w(px(width)),
                        ),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .id("workspace-tabs")
                                    .h(px(54.))
                                    .px_2()
                                    .flex_shrink_0()
                                    .flex()
                                    .items_center()
                                    .gap_0()
                                    .bg(rgb(p.sidebar))
                                    .border_b_1()
                                    .border_color(rgb(p.border))
                                    .child(
                                        Button::new("home-tab")
                                            .h_full()
                                            .w(px(140.))
                                            .rounded_none()
                                            .border_r_1()
                                            .border_color(rgb(p.border))
                                            .ghost()
                                            .selected(home)
                                            .icon(gpui_kit_assets::IconName::Server)
                                            .label(tr("nav-servers"))
                                            .on_click(cx.listener(|this, _, w, cx| {
                                                this.home(&GoHome, w, cx)
                                            })),
                                    )
                                    .child(
                                        div()
                                            .id("open-tabs")
                                            .h_full()
                                            .flex_1()
                                            .min_w_0()
                                            .overflow_x_scroll()
                                            .flex()
                                            .gap_0()
                                            .children(tabs),
                                    )
                                    .child(
                                        Button::new("new-local")
                                            .ghost()
                                            .icon(IconName::Plus)
                                            .tooltip(tr("open-local"))
                                            .accessibility_label(tr("open-local"))
                                            .on_click(cx.listener(|this, _, w, cx| {
                                                this.open_local(&OpenLocalTerminal, w, cx)
                                            })),
                                    ),
                            )
                            .when(!self.message.is_empty(), |d| {
                                d.child(
                                    div()
                                        .px_4()
                                        .py_2()
                                        .bg(rgb(p.raised))
                                        .border_b_1()
                                        .border_color(rgb(p.border))
                                        .flex()
                                        .items_center()
                                        .gap_3()
                                        .child(div().flex_1().text_sm().child(self.message.clone()))
                                        .child(
                                            Button::new("dismiss-message")
                                                .ghost()
                                                .icon(IconName::Close)
                                                .tooltip(tr("dismiss-message"))
                                                .accessibility_label(tr("dismiss-message"))
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.message.clear();
                                                    cx.notify();
                                                })),
                                        ),
                                )
                            })
                            .when(home, |d| d.child(self.background_sessions(cx)))
                            .child(
                                div().flex_1().min_h_0().child(content).with_animation(
                                    ("page-enter", self.transition as usize),
                                    Animation::new(std::time::Duration::from_millis(
                                        if self.store.settings.reduced_motion || terminal_open {
                                            0
                                        } else {
                                            140
                                        },
                                    ))
                                    .with_easing(design::spring_out),
                                    |d, t| d.opacity((0.7 + 0.3 * t).clamp(0., 1.)),
                                ),
                            ),
                    ),
            )
    }
}
fn last_connected(timestamp: Option<u64>) -> String {
    let Some(timestamp) = timestamp else {
        return tr("never-connected");
    };
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .saturating_sub(timestamp);
    if elapsed < 60 {
        tr("connected-now")
    } else if elapsed < 3600 {
        format!("{} {}", elapsed / 60, tr("connected-minutes"))
    } else if elapsed < 86400 {
        format!("{} {}", elapsed / 3600, tr("connected-hours"))
    } else {
        format!("{} {}", elapsed / 86400, tr("connected-days"))
    }
}
