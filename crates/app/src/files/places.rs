//! Places sidebar: standard locations, the terminal's folder and per-server bookmarks.
use super::*;
use opsssh_store::MAX_BOOKMARKS;

/// Fixed places offered on every Linux server.
const STANDARD: [(&str, &str); 3] = [
    ("file-place-home", "."),
    ("file-place-root", "/"),
    ("file-place-tmp", "/tmp"),
];

impl FilePane {
    /// Bookmarks come from the saved workspace preference for this server.
    pub fn set_bookmarks(&mut self, bookmarks: Vec<String>, cx: &mut Context<Self>) {
        self.bookmarks = bookmarks
            .into_iter()
            .filter(|path| path.starts_with('/'))
            .take(MAX_BOOKMARKS)
            .collect();
        cx.notify();
    }
    pub(super) fn toggle_bookmark(&mut self, path: String, cx: &mut Context<Self>) {
        if let Some(index) = self.bookmarks.iter().position(|item| *item == path) {
            self.bookmarks.remove(index);
            self.message = tr("file-bookmark-removed");
        } else if !path.starts_with('/') {
            return;
        } else if self.bookmarks.len() >= MAX_BOOKMARKS {
            self.message = tr("file-bookmark-limit");
            cx.notify();
            return;
        } else {
            self.bookmarks.push(path);
            self.message = tr("file-bookmark-added");
        }
        cx.emit(FilePaneEvent::BookmarksChanged(self.bookmarks.clone()));
        cx.notify();
    }

    pub(super) fn places_view(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let mut column = div()
            .id("file-places")
            .w(px(200.))
            .flex_shrink_0()
            .h_full()
            .overflow_y_scroll()
            .p_2()
            .flex()
            .flex_col()
            .gap_1()
            .border_r_1()
            .border_color(theme.border)
            .child(
                div()
                    .px_2()
                    .pb_1()
                    .text_xs()
                    .text_color(muted)
                    .child(tr("file-places")),
            );
        let mut places: Vec<(String, String, IconName)> = STANDARD
            .iter()
            .map(|(label, path)| (tr(label), (*path).to_owned(), IconName::Folder))
            .collect();
        if let Some(terminal) = &self.terminal_directory {
            places.push((
                tr("file-place-terminal"),
                terminal.clone(),
                IconName::SquareTerminal,
            ));
        }
        for (index, (label, path, icon)) in places.into_iter().enumerate() {
            column =
                column.child(self.place_row(("file-place", index), label, path, icon, false, cx));
        }
        column = column.child(
            div()
                .px_2()
                .pt_3()
                .pb_1()
                .flex()
                .items_center()
                .justify_between()
                .text_xs()
                .text_color(muted)
                .child(tr("file-bookmarks"))
                .child(
                    Button::new("file-bookmark-current")
                        .xsmall()
                        .ghost()
                        .icon(if self.bookmarks.contains(&self.directory) {
                            IconName::Star
                        } else {
                            IconName::Plus
                        })
                        .tooltip(tr("file-bookmark-toggle"))
                        .accessibility_label(tr("file-bookmark-toggle"))
                        .disabled(!self.directory.starts_with('/'))
                        .on_click(cx.listener(|this, _, _, cx| {
                            let path = this.directory.clone();
                            this.toggle_bookmark(path, cx);
                        })),
                ),
        );
        if self.bookmarks.is_empty() {
            column = column.child(
                div()
                    .px_2()
                    .text_xs()
                    .text_color(muted)
                    .child(tr("file-bookmarks-empty")),
            );
        }
        for (index, path) in self.bookmarks.iter().enumerate() {
            let label = path
                .trim_end_matches('/')
                .rsplit('/')
                .next()
                .filter(|name| !name.is_empty())
                .unwrap_or("/")
                .to_owned();
            column = column.child(self.place_row(
                ("file-bookmark", index),
                label,
                path.clone(),
                IconName::Star,
                true,
                cx,
            ));
        }
        column.into_any_element()
    }

    fn place_row(
        &self,
        id: (&'static str, usize),
        label: String,
        path: String,
        icon: IconName,
        bookmark: bool,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let theme = cx.theme();
        let current = path == self.directory || (path == "." && self.directory == self.home_hint);
        let row = div()
            .id(id)
            .px_2()
            .py_1()
            .rounded_md()
            .border_1()
            .border_color(gpui::transparent_black())
            .flex()
            .items_center()
            .gap_2()
            .text_sm()
            .cursor_pointer()
            .when(current, |d| d.bg(theme.accent))
            .hover(|style| style.bg(theme.muted))
            .tooltip({
                let path = path.clone();
                move |window, cx| {
                    gpui_component::tooltip::Tooltip::new(path.clone()).build(window, cx)
                }
            })
            .child(Icon::new(icon).small().text_color(theme.primary))
            .child(div().flex_1().min_w_0().truncate().child(label));
        let navigate = path.clone();
        let row = row.on_click(cx.listener(move |this, _, _, cx| {
            this.navigate(navigate.clone(), Navigation::Visit, cx);
        }));
        // "." resolves on the server; only real paths accept drops.
        let row = if path.starts_with('/') {
            self.folder_drop_target(row, path.clone(), cx)
        } else {
            row
        };
        if bookmark {
            let weak = cx.entity().downgrade();
            row.context_menu(move |menu, _, _| {
                let weak = weak.clone();
                let path = path.clone();
                menu.item(PopupMenuItem::new(tr("file-bookmark-remove")).on_click(
                    move |_, _, cx| {
                        let _ = weak.update(cx, |this, cx| this.toggle_bookmark(path.clone(), cx));
                    },
                ))
            })
            .into_any_element()
        } else {
            row.into_any_element()
        }
    }
}
