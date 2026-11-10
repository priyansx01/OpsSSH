//! Thunar-style browsing: multi-selection, keyboard shortcuts, history, clipboard and
//! inline rename. Remote work still goes through queued jobs.
use super::*;
use gpui::{KeyDownEvent, Modifiers};
use selection::Selection;

/// Back/forward history, bounded and free of consecutive duplicates.
#[derive(Debug, Default)]
pub(super) struct History {
    back: Vec<String>,
    forward: Vec<String>,
}
pub(super) const HISTORY_LIMIT: usize = 50;
impl History {
    pub fn can_go_back(&self) -> bool {
        !self.back.is_empty()
    }
    pub fn can_go_forward(&self) -> bool {
        !self.forward.is_empty()
    }
    /// Leaving `from` for a new place: forward history no longer applies.
    pub fn visit(&mut self, from: &str) {
        push_bounded(&mut self.back, from);
        self.forward.clear();
    }
    pub fn take_back(&mut self) -> Option<String> {
        self.back.pop()
    }
    pub fn take_forward(&mut self) -> Option<String> {
        self.forward.pop()
    }
    /// Arrived by Back: the page we left becomes Forward.
    pub fn went_back(&mut self, from: &str) {
        push_bounded(&mut self.forward, from);
    }
    /// Arrived by Forward: the page we left becomes Back.
    pub fn went_forward(&mut self, from: &str) {
        push_bounded(&mut self.back, from);
    }
}
fn push_bounded(stack: &mut Vec<String>, path: &str) {
    if path.is_empty() || stack.last().is_some_and(|last| last == path) {
        return;
    }
    stack.push(path.to_owned());
    if stack.len() > HISTORY_LIMIT {
        stack.remove(0);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Navigation {
    Visit,
    Back,
    Forward,
}

/// Items cut or copied inside OpsSSH, bound to the connection that listed them.
#[derive(Debug, Clone)]
pub(super) struct FileClipboard {
    pub generation: u64,
    pub paths: Vec<String>,
    pub mode: TransferMode,
}

/// The item being renamed in place.
pub(super) struct InlineRename {
    pub path: String,
    pub input: Entity<InputState>,
    pub error: Option<String>,
}

/// Why a new name cannot be used, or `None` when it is valid.
pub(super) fn invalid_name(name: &str) -> Option<&'static str> {
    if name.is_empty() || name.trim().is_empty() {
        Some("Enter a name")
    } else if name == "." || name == ".." {
        Some("This name is reserved")
    } else if name.contains('/') || name.contains('\0') {
        Some("Names cannot contain / or NUL")
    } else if name.chars().any(char::is_control) {
        Some("Names cannot contain control characters")
    } else if name.len() > opsssh_sftp::MAX_NAME_BYTES {
        Some("Name is longer than 255 bytes")
    } else {
        None
    }
}

pub(super) fn parent_of(path: &str) -> Option<String> {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return None;
    }
    match trimmed.rsplit_once('/') {
        Some(("", _)) => Some("/".into()),
        Some((parent, _)) => Some(parent.into()),
        None => None,
    }
}

fn primary(modifiers: &Modifiers) -> bool {
    if cfg!(target_os = "macos") {
        modifiers.platform
    } else {
        modifiers.control
    }
}

impl FilePane {
    /// Paths of the listed items in display order.
    pub(super) fn visible_paths(&self) -> Vec<String> {
        self.visible
            .iter()
            .filter_map(|&index| self.entries.get(index))
            .map(|entry| entry.path.clone())
            .collect()
    }
    pub(super) fn entry_for(&self, path: &str) -> Option<&Entry> {
        self.entries.iter().find(|entry| entry.path == path)
    }
    /// Selected entries in display order.
    pub(super) fn selected_entries(&self) -> Vec<Entry> {
        self.selection
            .ordered(&self.visible_paths())
            .iter()
            .filter_map(|path| self.entry_for(path).cloned())
            .collect()
    }
    /// The item keyboard actions apply to: the cursor if selected, else the only selection.
    pub(super) fn focused_entry(&self) -> Option<Entry> {
        self.selection
            .cursor()
            .filter(|cursor| self.selection.contains(cursor))
            .or_else(|| self.selection.single())
            .and_then(|path| self.entry_for(path).cloned())
    }
    pub(super) fn is_selected(&self, path: &str) -> bool {
        self.selection.contains(path)
    }

    /// Mouse selection with Thunar modifiers: Ctrl toggles, Shift selects a range.
    pub(super) fn click_entry(&mut self, path: &str, modifiers: &Modifiers) {
        if primary(modifiers) && modifiers.shift {
            self.selection.extend_to(path, &self.visible_paths(), true);
        } else if modifiers.shift {
            self.selection.extend_to(path, &self.visible_paths(), false);
        } else if primary(modifiers) {
            self.selection.toggle(path);
        } else {
            self.selection.select_only(path);
        }
    }
    /// Right-click or drag on an unselected item acts on that item alone.
    pub(super) fn ensure_selected(&mut self, path: &str) {
        if !self.selection.contains(path) {
            self.selection.select_only(path);
        }
    }

    /// Open a folder, recording where we came from.
    pub(super) fn navigate(&mut self, path: String, how: Navigation, cx: &mut Context<Self>) {
        if !self.available || path.is_empty() {
            return;
        }
        self.navigation = Some((self.next_id, how));
        self.enqueue(Job::List(path), cx);
    }
    /// A listing arrived for a different folder: update history for the pending navigation.
    pub(super) fn record_navigation(&mut self, from: &str, to: &str) {
        let how = self.navigation.take().map(|(_, how)| how);
        if from.is_empty() || from == to {
            return;
        }
        match how {
            Some(Navigation::Back) => self.history.went_back(from),
            Some(Navigation::Forward) => self.history.went_forward(from),
            Some(Navigation::Visit) | None => self.history.visit(from),
        }
    }
    pub(super) fn go_back(&mut self, cx: &mut Context<Self>) {
        if let Some(path) = self.history.take_back() {
            self.navigate(path, Navigation::Back, cx);
        }
    }
    pub(super) fn go_forward(&mut self, cx: &mut Context<Self>) {
        if let Some(path) = self.history.take_forward() {
            self.navigate(path, Navigation::Forward, cx);
        }
    }
    pub(super) fn go_up(&mut self, cx: &mut Context<Self>) {
        if let Some(parent) = parent_of(&self.directory) {
            self.navigate(parent, Navigation::Visit, cx);
        }
    }
    pub(super) fn open_entry(&mut self, entry: Entry, window: &mut Window, cx: &mut Context<Self>) {
        if entry.is_directory {
            self.navigate(entry.path, Navigation::Visit, cx);
        } else {
            self.download_entry(entry, window, cx);
        }
    }

    /// Ask before deleting everything selected; nothing is removed until confirmed.
    pub(super) fn request_delete_selection(&mut self, cx: &mut Context<Self>) {
        let paths = self
            .selected_entries()
            .into_iter()
            .map(|entry| entry.path)
            .collect::<Vec<_>>();
        if !paths.is_empty() && self.available {
            self.pending_delete = Some(paths);
            cx.notify();
        }
    }

    /// One regular file opens a save dialog; anything else is saved into a chosen folder.
    pub(super) fn download_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let entries = self.selected_entries();
        match entries.as_slice() {
            [] => {}
            [entry] => self.download_entry(entry.clone(), window, cx),
            _ => self.download_entries_to_folder(entries, window, cx),
        }
    }
    fn download_entries_to_folder(
        &mut self,
        entries: Vec<Entry>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let links = entries.iter().filter(|entry| entry.is_symlink).count();
        let entries = entries
            .into_iter()
            .filter(|entry| !entry.is_symlink)
            .collect::<Vec<_>>();
        if entries.is_empty() {
            self.message = "Open the actual target to download a symbolic link".into();
            cx.notify();
            return;
        }
        let generation = self.worker_generation;
        let picker = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some(tr("file-download-here").into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(mut folders))) = picker.await else {
                return;
            };
            let Some(folder) = folders.pop() else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                if !this.available || this.worker_generation != generation {
                    return;
                }
                for entry in entries {
                    // Conflicts with existing local files are asked one by one.
                    this.enqueue(
                        Job::Download {
                            local: folder.join(&entry.name),
                            remote: entry.path,
                            confirmed_existing: false,
                            control: TransferControl::default(),
                        },
                        cx,
                    );
                }
                if links > 0 {
                    this.message = format!("{links} symbolic link(s) skipped");
                }
            });
        })
        .detach();
    }

    pub(super) fn copy_selection(&mut self, mode: TransferMode, cx: &mut Context<Self>) {
        let paths = self
            .selected_entries()
            .into_iter()
            .map(|entry| entry.path)
            .collect::<Vec<_>>();
        if paths.is_empty() {
            return;
        }
        self.message = format!(
            "{} {} {}",
            paths.len(),
            tr("file-items"),
            tr(if mode == TransferMode::Move {
                "file-cut-ready"
            } else {
                "file-copy-ready"
            })
        );
        self.clipboard = Some(FileClipboard {
            generation: self.worker_generation,
            paths,
            mode,
        });
        cx.notify();
    }
    /// Cut items are drawn faded until they are pasted.
    pub(super) fn is_cut(&self, path: &str) -> bool {
        self.clipboard.as_ref().is_some_and(|clipboard| {
            clipboard.mode == TransferMode::Move
                && clipboard.generation == self.worker_generation
                && clipboard.paths.iter().any(|item| item == path)
        })
    }
    /// Where Paste puts items: the one selected folder, otherwise the open folder.
    pub(super) fn paste_target(&self) -> String {
        self.selection
            .single()
            .and_then(|path| self.entry_for(path))
            .filter(|entry| entry.is_directory && !entry.is_symlink)
            .map(|entry| entry.path.clone())
            .unwrap_or_else(|| self.directory.clone())
    }
    pub(super) fn paste(&mut self, destination: String, cx: &mut Context<Self>) {
        // Files copied in the operating system are uploaded.
        if let Some(item) = cx.read_from_clipboard()
            && let Some(paths) = item.entries().iter().find_map(|entry| match entry {
                ClipboardEntry::ExternalPaths(paths) => {
                    Some(paths.0.iter().cloned().collect::<Vec<_>>())
                }
                _ => None,
            })
            && !paths.is_empty()
        {
            self.upload_files(paths, Some(destination), false, cx);
            return;
        }
        let Some(clipboard) = self.clipboard.clone() else {
            return;
        };
        if clipboard.generation != self.worker_generation {
            self.clipboard = None;
            self.message = tr("file-clipboard-stale");
            cx.notify();
            return;
        }
        self.transfer(clipboard.paths.clone(), destination, clipboard.mode, cx);
        if clipboard.mode == TransferMode::Move {
            // Cut items move once; pasting again would find nothing to move.
            self.clipboard = None;
        }
    }
    /// Queue a copy or move of `paths` into `destination`.
    pub(super) fn transfer(
        &mut self,
        paths: Vec<String>,
        destination: String,
        mode: TransferMode,
        cx: &mut Context<Self>,
    ) {
        if !self.available {
            self.message = tr("file-reconnect-first");
            cx.notify();
            return;
        }
        let items = paths
            .iter()
            .filter_map(|path| TransferItem::named_as_source(path))
            .collect::<Vec<_>>();
        if items.is_empty() {
            return;
        }
        self.enqueue(
            Job::Transfer {
                items,
                destination,
                mode,
                control: TransferControl::default(),
            },
            cx,
        );
    }

    pub(super) fn begin_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(entry) = self.focused_entry() else {
            return;
        };
        if !self.available {
            return;
        }
        self.selection.select_only(&entry.path);
        let input = cx.new(|cx| InputState::new(window, cx).default_value(entry.name.clone()));
        cx.subscribe_in(&input, window, |this, _, event, window, cx| match event {
            InputEvent::PressEnter { .. } => this.commit_rename(window, cx),
            InputEvent::Blur => this.cancel_rename(window, cx),
            InputEvent::Change => {
                if let Some(rename) = &mut this.rename {
                    rename.error = None;
                }
                cx.notify();
            }
            _ => {}
        })
        .detach();
        input.update(cx, |input, cx| input.focus(window, cx));
        self.rename = Some(InlineRename {
            path: entry.path,
            input,
            error: None,
        });
        cx.notify();
    }
    pub(super) fn commit_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(rename) = &mut self.rename else {
            return;
        };
        let name = rename.input.read(cx).value().to_string();
        let current = rename
            .path
            .rsplit('/')
            .next()
            .unwrap_or_default()
            .to_owned();
        if name == current {
            return self.cancel_rename(window, cx);
        }
        if let Some(error) = invalid_name(&name) {
            rename.error = Some(error.into());
            cx.notify();
            return;
        }
        let Some(parent) = parent_of(&rename.path) else {
            return self.cancel_rename(window, cx);
        };
        let source = rename.path.clone();
        self.rename = None;
        self.enqueue(
            Job::Transfer {
                items: vec![TransferItem { source, name }],
                destination: parent,
                mode: TransferMode::Move,
                control: TransferControl::default(),
            },
            cx,
        );
        self.list_focus.focus(window, cx);
    }
    pub(super) fn cancel_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.rename.take().is_some() {
            self.list_focus.focus(window, cx);
            cx.notify();
        }
    }

    /// Keyboard shortcuts while the listing has focus. Returns whether the key was used.
    pub(super) fn handle_list_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let modifiers = event.keystroke.modifiers;
        let key = event.keystroke.key.as_str();
        let ctrl = primary(&modifiers);
        let order = self.visible_paths();
        let position = self.selection.cursor_index(&order);
        let last = order.len().saturating_sub(1);
        let step = self.columns.max(1);
        let mut moved = None;
        match key {
            "left" if modifiers.alt => self.go_back(cx),
            "right" if modifiers.alt => self.go_forward(cx),
            "up" if modifiers.alt => self.go_up(cx),
            // The server resolves "." to the login user's home folder.
            "home" if modifiers.alt => self.navigate(".".into(), Navigation::Visit, cx),
            "backspace" => self.go_up(cx),
            "down" => moved = Some(position.map_or(0, |i| (i + step).min(last))),
            "up" => moved = Some(position.map_or(0, |i| i.saturating_sub(step))),
            "right" => moved = Some(position.map_or(0, |i| (i + 1).min(last))),
            "left" => moved = Some(position.map_or(0, |i| i.saturating_sub(1))),
            "pagedown" => moved = Some(position.map_or(0, |i| (i + step * 8).min(last))),
            "pageup" => moved = Some(position.map_or(0, |i| i.saturating_sub(step * 8))),
            "home" => moved = Some(0),
            "end" => moved = Some(last),
            "a" if ctrl => self.selection.select_all(&order),
            "escape" => {
                if self.selection.is_empty() && self.clipboard.is_some() {
                    self.clipboard = None;
                } else {
                    self.selection.clear();
                }
            }
            "enter" => {
                let entries = self.selected_entries();
                match entries.as_slice() {
                    [entry] => self.open_entry(entry.clone(), window, cx),
                    [] => {}
                    _ => self.download_selection(window, cx),
                }
            }
            "delete" => self.request_delete_selection(cx),
            "f2" => self.begin_rename(window, cx),
            "f5" => self.refresh(cx),
            "r" if ctrl => self.refresh(cx),
            "h" if ctrl => {
                self.show_hidden = !self.show_hidden;
                self.browser_dirty = true;
            }
            "l" if ctrl => self.path.update(cx, |input, cx| input.focus(window, cx)),
            "f" if ctrl => self.search.update(cx, |input, cx| input.focus(window, cx)),
            "d" if ctrl => {
                let path = self.directory.clone();
                self.toggle_bookmark(path, cx);
            }
            "n" if ctrl && modifiers.shift => {
                self.folder.update(cx, |input, cx| input.focus(window, cx))
            }
            "c" if ctrl && modifiers.shift => {
                let paths = self
                    .selected_entries()
                    .into_iter()
                    .map(|entry| entry.path)
                    .collect::<Vec<_>>();
                if !paths.is_empty() {
                    cx.write_to_clipboard(ClipboardItem::new_string(paths.join("\n")));
                    self.message = tr("file-paths-copied");
                }
            }
            "c" if ctrl => self.copy_selection(TransferMode::Copy, cx),
            "x" if ctrl => self.copy_selection(TransferMode::Move, cx),
            "v" if ctrl => {
                let target = self.paste_target();
                self.paste(target, cx);
            }
            "f10" if modifiers.shift => self.keyboard_actions(window, cx),
            "menu" => self.keyboard_actions(window, cx),
            _ => return false,
        }
        if let Some(index) = moved
            && !order.is_empty()
        {
            self.selection.move_to(index, &order, modifiers.shift);
            self.list_scroll
                .scroll_to_item(index / step, gpui::ScrollStrategy::Nearest);
        }
        cx.notify();
        true
    }

    /// Selection that survives refreshes, filters and hidden-file toggles.
    pub(super) fn prune_selection(&mut self) {
        let order = self.visible_paths();
        self.selection.retain_visible(&order);
    }
    pub(super) fn reset_selection(&mut self) {
        self.selection = Selection::default();
        self.rename = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_is_bounded_and_ignores_repeats() {
        let mut history = History::default();
        for index in 0..60 {
            history.visit(&format!("/{index}"));
            history.visit(&format!("/{index}"));
        }
        assert_eq!(history.back.len(), HISTORY_LIMIT);
        assert_eq!(history.take_back().as_deref(), Some("/59"));
        history.went_back("/now");
        assert!(history.can_go_forward());
        history.visit("/elsewhere");
        assert!(!history.can_go_forward(), "a new visit clears forward");
        history.visit("");
        assert_eq!(history.back.last().map(String::as_str), Some("/elsewhere"));
    }

    #[test]
    fn names_are_validated_like_a_posix_filesystem() {
        assert!(invalid_name("notes.txt").is_none());
        assert!(invalid_name(".env").is_none());
        assert!(invalid_name("雪 report").is_none());
        for bad in ["", "   ", ".", "..", "a/b", "a\0b", "line\nbreak"] {
            assert!(invalid_name(bad).is_some(), "{bad:?}");
        }
        assert!(invalid_name(&"x".repeat(256)).is_some());
        assert!(invalid_name(&"x".repeat(255)).is_none());
    }

    #[test]
    fn parents_stop_at_the_root() {
        assert_eq!(parent_of("/srv/app").as_deref(), Some("/srv"));
        assert_eq!(parent_of("/srv").as_deref(), Some("/"));
        assert_eq!(parent_of("/srv/app/").as_deref(), Some("/srv"));
        assert_eq!(parent_of("/"), None);
    }
}
