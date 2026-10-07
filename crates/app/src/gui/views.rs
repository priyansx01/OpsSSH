//! Workspace presentation. SSH/session ownership stays in the parent controller.
use super::*;
use crate::design;
use gpui::{AnyElement, uniform_list};
use gpui_component::{
    ActiveTheme, Icon, IconName, Selectable, h_resizable,
    menu::{DropdownMenu, PopupMenuItem},
    resizable_panel,
    switch::Switch,
};
use std::{
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

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
        for tab in &self.tabs {
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
        let mut sidebar = div()
            .w(px(if collapsed { 64. } else { 220. }))
            .flex_shrink_0()
            .h_full()
            .p_3()
            .flex()
            .flex_col()
            .gap_2()
            .bg(rgb(p.sidebar))
            .border_r_1()
            .border_color(rgb(p.border))
            .child(
                div()
                    .h(px(48.))
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .rounded_md()
                            .p_2()
                            .bg(rgb(p.ruby_tint))
                            .text_color(rgb(p.text))
                            .text_lg()
                            .child(">_"),
                    )
                    .when(!collapsed, |d| {
                        d.child(
                            div()
                                .text_lg()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child("OpsSSH"),
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
            (
                "From SSH config",
                "nav-config",
                gpui_kit_assets::IconName::File,
            ),
        ]
        .into_iter()
        .enumerate()
        {
            let selected = !self.settings_page
                && !self.help_page
                && self.active.is_none()
                && self.filter == filter;
            sidebar = sidebar.child(
                Button::new(("nav", i))
                    .ghost()
                    .selected(selected)
                    .icon(icon)
                    .tooltip(tr(key))
                    .accessibility_label(tr(key))
                    .when(!collapsed, |b| b.label(tr(key)))
                    .w_full()
                    .on_click(cx.listener(move |this, _, w, cx| {
                        this.home(&GoHome, w, cx);
                        this.filter = filter.into();
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
                    .pt_4()
                    .pb_1()
                    .text_xs()
                    .text_color(rgb(p.muted))
                    .child(tr("nav-environments")),
            );
            for (i, environment) in environments.into_iter().enumerate() {
                sidebar = sidebar.child(
                    Button::new(("environment", i))
                        .ghost()
                        .selected(
                            self.filter == environment
                                && !self.settings_page
                                && !self.help_page
                                && self.active.is_none(),
                        )
                        .label(environment.clone())
                        .w_full()
                        .on_click(cx.listener(move |this, _, w, cx| {
                            this.home(&GoHome, w, cx);
                            this.filter = environment.clone();
                            cx.notify();
                        })),
                );
            }
        }
        sidebar
            .child(div().flex_1())
            .child(
                Button::new("settings-nav")
                    .ghost()
                    .selected(self.settings_page)
                    .icon(IconName::Settings)
                    .tooltip(tr("settings"))
                    .accessibility_label(tr("settings"))
                    .when(!collapsed, |b| b.label(tr("settings")))
                    .w_full()
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.settings_page = true;
                        this.help_page = false;
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
                    .when(!collapsed, |b| b.label(tr("help")))
                    .w_full()
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.help_page = true;
                        this.settings_page = false;
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
                    .when(!collapsed, |b| b.label(tr("sidebar-collapse")))
                    .w_full()
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
    ) -> gpui::Stateful<gpui::Div> {
        let p = design::palette(cx);
        let config = profile.id == 0;
        let connect = profile.clone();
        let favorite = profile.clone();
        let edit = profile.clone();
        let duplicate = profile.clone();
        let delete = profile.clone();
        let workspace = cx.entity().downgrade();
        let menu = Button::new(("server-menu", index))
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
                if config {
                    return menu;
                }
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
        let last = last_connected(timestamp);
        let metadata = div()
            .truncate()
            .flex()
            .gap_2()
            .text_xs()
            .text_color(rgb(p.muted))
            .child(auth)
            .when(!profile.proxy_jump.is_empty(), |d| {
                d.child(format!("{} {}", tr("via-label"), profile.proxy_jump))
            })
            .when(!profile.proxy_command.is_empty(), |d| {
                d.child(tr("proxy-label"))
            });
        let identity = div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(profile.name.clone()),
                    )
                    .when(!profile.environment.is_empty(), |d| {
                        d.child(
                            div()
                                .px_2()
                                .py_1()
                                .rounded_md()
                                .max_w(px(120.))
                                .flex_shrink_0()
                                .truncate()
                                .bg(rgb(p.raised))
                                .text_xs()
                                .child(profile.environment.clone()),
                        )
                    }),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(p.muted))
                    .truncate()
                    .font_family(cx.theme().mono_font_family.clone())
                    .child(format!(
                        "{}@{}:{}",
                        profile.user, profile.host, profile.port
                    )),
            )
            .child(metadata);
        let mut card = div()
            .id(("server-card", index))
            .h(px(if list { 120. } else { 208. }))
            .overflow_hidden()
            .p_4()
            .rounded(px(12.))
            .bg(rgb(p.surface))
            .border_1()
            .border_color(rgb(p.border))
            .flex()
            .gap_3()
            .when(!list, |d| d.flex_col())
            .when(list, |d| d.items_center())
            .child(identity);
        card = card.child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .when(!list, |d| d.pt_3().border_t_1().border_color(rgb(p.border)))
                .child(
                    Button::new(("connect", index))
                        .primary()
                        .label(tr("connect-server"))
                        .on_click(
                            cx.listener(move |this, _, w, cx| this.connect(connect.clone(), w, cx)),
                        ),
                )
                .child(div().flex_1())
                .when(!config, |d| {
                    d.child(
                        Button::new(("favorite", index))
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
        );
        card.child(
            div()
                .text_xs()
                .text_color(rgb(p.muted))
                .max_w(px(if list { 120. } else { 1000. }))
                .truncate()
                .child(last),
        )
    }
    fn servers(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = design::palette(cx);
        let query = self.search.read(cx).value();
        let profiles = Arc::new(self.visible_profiles(&query));
        let list = self.store.settings.server_view != "cards";
        let sidebar_width =
            if self.store.settings.sidebar_collapsed || window.viewport_size().width < px(1100.) {
                64.
            } else {
                220.
            };
        let available = f32::from(window.viewport_size().width) - sidebar_width - 64.;
        let columns = if list {
            1
        } else {
            ((available + 16.) / 316.).floor().clamp(1., 4.) as usize
        };
        let width = (available - 16. * (columns - 1) as f32) / columns as f32;
        let rows = profiles.len().div_ceil(columns);
        let title = match self.filter.as_str() {
            "All servers" => tr("nav-servers"),
            "Favourites" => tr("nav-favorites"),
            "From SSH config" => tr("nav-config"),
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
            .label(tr("filter-environments"))
            .dropdown_menu(move |menu, _, _| {
                let weak = workspace.clone();
                let mut menu = menu.item(PopupMenuItem::new(tr("all-environments")).on_click(
                    move |_, window, cx| {
                        let _ = weak.update(cx, |this, cx| {
                            this.home(&GoHome, window, cx);
                            this.filter = "All servers".into();
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
                                this.filter = value.clone();
                                cx.notify();
                            });
                        },
                    ));
                }
                menu
            });
        let mut page = div()
            .size_full()
            .p_6()
            .flex()
            .flex_col()
            .gap_4()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_2xl()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child(title),
                            )
                            .child(div().text_sm().text_color(rgb(p.muted)).child(format!(
                                "{} {}",
                                profiles.len(),
                                tr("servers-count")
                            ))),
                    )
                    .child(
                        Button::new("new-server")
                            .primary()
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
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(Input::new(&self.search).aria_label(tr("search-servers"))),
                    )
                    .child(environment_menu)
                    .child(
                        Button::new("view-cards")
                            .ghost()
                            .selected(!list)
                            .icon(IconName::LayoutDashboard)
                            .tooltip(tr("view-cards"))
                            .accessibility_label(tr("view-cards"))
                            .on_click(cx.listener(|this, _, w, cx| {
                                this.change_settings(|s| s.server_view = "cards".into(), w, cx)
                            })),
                    )
                    .child(
                        Button::new("view-list")
                            .ghost()
                            .selected(list)
                            .icon(gpui_kit_assets::IconName::List)
                            .tooltip(tr("view-list"))
                            .accessibility_label(tr("view-list"))
                            .on_click(cx.listener(|this, _, w, cx| {
                                this.change_settings(|s| s.server_view = "list".into(), w, cx)
                            })),
                    )
                    .child(
                        Button::new("sort")
                            .label(tr(if self.store.settings.sort == "last_connected" {
                                "sort-recent"
                            } else {
                                "sort-name"
                            }))
                            .on_click(cx.listener(|this, _, w, cx| {
                                this.change_settings(
                                    |s| {
                                        s.sort = if s.sort == "last_connected" {
                                            "name"
                                        } else {
                                            "last_connected"
                                        }
                                        .into()
                                    },
                                    w,
                                    cx,
                                )
                            })),
                    ),
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
                                Button::new("empty-new")
                                    .primary()
                                    .label(tr("new-server"))
                                    .on_click(cx.listener(|this, _, w, cx| {
                                        this.new_server(&NewServer, w, cx)
                                    })),
                            )
                            .child(
                                Button::new("empty-config")
                                    .label(tr("import-config"))
                                    .on_click(cx.listener(|this, _, w, cx| {
                                        this.home(&GoHome, w, cx);
                                        this.filter = "From SSH config".into();
                                        cx.notify();
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
                                    .h(px(if list { 136. } else { 224. }))
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
                    .p_5()
                    .rounded_lg()
                    .bg(rgb(p.surface))
                    .border_1()
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
                    .p_5()
                    .rounded_lg()
                    .bg(rgb(p.surface))
                    .border_1()
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
                    .p_5()
                    .rounded_lg()
                    .bg(rgb(p.surface))
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
            .p_5()
            .rounded_lg()
            .bg(rgb(p.surface))
            .border_1()
            .border_color(rgb(p.border))
            .flex()
            .flex_col()
            .gap_3()
            .child(div().text_lg().child(tr("settings-shortcuts")))
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
    fn session(&self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let p = design::palette(cx);
        let tab = &self.tabs[index];
        let terminal = tab.terminal.clone();
        let files = tab.files.clone();
        let visible = tab.files_visible;
        let follow = tab.follow;
        let is_ssh = terminal.read(cx).ssh_commands().is_some();
        div()
            .size_full()
            .flex()
            .flex_col()
            .min_h_0()
            .child(
                div()
                    .h(px(44.))
                    .px_4()
                    .flex()
                    .items_center()
                    .gap_2()
                    .border_b_1()
                    .border_color(rgb(p.border))
                    .child(div().w(px(6.)).h(px(6.)).rounded_full().bg(rgb(tab.color)))
                    .child(div().text_sm().child(tab.name.clone()))
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(p.muted))
                            .child(tab.environment.clone()),
                    )
                    .child(div().flex_1())
                    .when(is_ssh, |d| {
                        d.child(
                            Button::new("toggle-files")
                                .ghost()
                                .selected(visible)
                                .icon(IconName::Folder)
                                .label(tr("files"))
                                .on_click(cx.listener(move |this, _, w, cx| {
                                    if this.tabs[index].files_visible {
                                        this.tabs[index].files_visible = false;
                                    } else {
                                        let terminal = this.tabs[index].terminal.clone();
                                        this.ensure_files(&terminal, w, cx);
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
                                        this.tabs[index].follow = !this.tabs[index].follow;
                                        if this.tabs[index].follow {
                                            let directory = this.tabs[index]
                                                .terminal
                                                .read(cx)
                                                .current_directory()
                                                .map(str::to_owned);
                                            if let (Some(directory), Some(files)) =
                                                (directory, this.tabs[index].files.clone())
                                            {
                                                files.update(cx, |pane, cx| {
                                                    pane.follow_directory(directory.clone(), cx)
                                                });
                                                this.tabs[index].followed_directory =
                                                    Some(directory);
                                            }
                                        }
                                        cx.notify();
                                    })),
                            )
                        })
                    }),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .when(visible && files.is_some(), |d| {
                        d.child(
                            h_resizable(("session-split", index))
                                .child(
                                    resizable_panel()
                                        .size_range(px(200.)..px(10000.))
                                        .child(div().size_full().child(terminal.clone())),
                                )
                                .child(
                                    resizable_panel()
                                        .size(px(360.))
                                        .size_range(px(300.)..px(600.))
                                        .child(files.clone().unwrap()),
                                ),
                        )
                    })
                    .when(!visible || files.is_none(), |d| {
                        d.child(div().size_full().child(terminal))
                    }),
            )
            .into_any_element()
    }
    pub(super) fn render_workspace(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let p = design::palette(cx);
        let narrow = window.viewport_size().width < px(1100.);
        let home = self.active.is_none() && !self.settings_page && !self.help_page;
        let tabs = self
            .tabs
            .iter()
            .enumerate()
            .map(|(index, tab)| {
                Button::new(("tab", index))
                    .ghost()
                    .selected(self.active == Some(index) && !self.settings_page && !self.help_page)
                    .label(tab.name.clone())
                    .tooltip(format!(
                        "{} · {}",
                        tab.name,
                        tab.terminal.read(cx).connection_status()
                    ))
                    .max_w(px(220.))
                    .on_click(cx.listener(move |this, _, w, cx| {
                        this.active = Some(index);
                        this.settings_page = false;
                        this.help_page = false;
                        this.tabs[index].terminal.update(cx, |t, cx| t.focus(w, cx));
                        cx.notify();
                    }))
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
        } else if let Some(index) = self.active {
            self.session(index, cx)
        } else {
            self.servers(window, cx)
        };
        div()
            .id("opsssh-workspace")
            .key_context(if home { "OpsSSH OpsSSHHome" } else { "OpsSSH" })
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
            .child(
                div()
                    .id("workspace-tabs")
                    .h(px(52.))
                    .px_3()
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .gap_2()
                    .bg(rgb(p.sidebar))
                    .border_b_1()
                    .border_color(rgb(p.border))
                    .child(
                        Button::new("home-tab")
                            .ghost()
                            .selected(home)
                            .icon(gpui_kit_assets::IconName::House)
                            .label(tr("home-title"))
                            .on_click(cx.listener(|this, _, w, cx| this.home(&GoHome, w, cx))),
                    )
                    .child(
                        div()
                            .id("open-tabs")
                            .flex_1()
                            .min_w_0()
                            .overflow_x_scroll()
                            .flex()
                            .gap_1()
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
                    )
                    .when(
                        self.active.is_some() && !self.settings_page && !self.help_page,
                        |d| {
                            d.child(
                                Button::new("close-tab")
                                    .ghost()
                                    .icon(IconName::Close)
                                    .tooltip(tr("close-tab"))
                                    .accessibility_label(tr("close-tab"))
                                    .on_click(cx.listener(|this, _, w, cx| {
                                        this.close_tab(&CloseTab, w, cx)
                                    })),
                            )
                        },
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .child(self.sidebar(narrow, cx))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
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
                            .child(div().flex_1().min_h_0().child(content)),
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
