//! Drag and drop inside the remote file manager, drops from the desktop, rubber-band
//! selection and spring-loaded folders.
use super::*;
use gpui::{
    AnimationExt, Bounds, DragMoveEvent, EntityId, ExternalPaths, MouseButton, Pixels, Point,
};
use selection::Band;
use std::time::Instant;

/// Remote items being dragged. They can only be dropped into the pane, connection and
/// listing generation they came from.
#[derive(Debug, Clone)]
pub(super) struct DraggedItems {
    pub pane: EntityId,
    pub generation: u64,
    pub paths: Vec<String>,
    /// Dragged real folders; nothing may be dropped inside one of them.
    pub folders: Vec<String>,
    pub first_name: String,
    pub first_is_folder: bool,
}

/// Whether `paths` may be dropped into `target`. A move into the folder that already
/// holds every item would change nothing, so it is refused; a copy there duplicates.
pub(super) fn drop_allowed(paths: &[String], folders: &[String], target: &str, copy: bool) -> bool {
    if paths.is_empty() || target.is_empty() {
        return false;
    }
    if paths.iter().any(|path| path == target) {
        return false;
    }
    if folders
        .iter()
        .any(|folder| target.starts_with(&format!("{}/", folder.trim_end_matches('/'))))
    {
        return false;
    }
    copy || paths
        .iter()
        .any(|path| browser::parent_of(path).as_deref() != Some(target))
}

fn wants_copy(window: &Window) -> bool {
    let modifiers = window.modifiers();
    let primary = if cfg!(target_os = "macos") {
        modifiers.alt || modifiers.platform
    } else {
        modifiers.control
    };
    primary && !modifiers.shift
}

/// The floating preview under the pointer while dragging.
pub(super) struct DragGhost {
    items: DraggedItems,
}
impl DragGhost {
    pub fn new(items: DraggedItems) -> Self {
        Self { items }
    }
}
impl Render for DragGhost {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let copy = wants_copy(window);
        let count = self.items.paths.len();
        let icon = if self.items.first_is_folder {
            IconName::Folder
        } else {
            IconName::File
        };
        let reduced = cx.reduce_motion();
        div()
            .flex()
            .items_center()
            .gap_2()
            .px_3()
            .py_2()
            .rounded_lg()
            .bg(theme.popover)
            .border_1()
            .border_color(theme.primary)
            .shadow_md()
            .text_sm()
            .text_color(theme.popover_foreground)
            .child(Icon::new(icon).text_color(theme.primary))
            .child(
                div()
                    .max_w(px(220.))
                    .truncate()
                    .child(self.items.first_name.clone()),
            )
            .when(count > 1, |d| {
                d.child(
                    div()
                        .px_2()
                        .rounded_full()
                        .bg(theme.primary)
                        .text_xs()
                        .text_color(theme.primary_foreground)
                        .child(format!("+{}", count - 1)),
                )
            })
            .child(
                div()
                    .px_2()
                    .rounded_md()
                    .border_1()
                    .border_color(theme.border)
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(tr(if copy {
                        "file-drop-copy"
                    } else {
                        "file-drop-move"
                    })),
            )
            .with_animation(
                "drag-ghost-enter",
                gpui::Animation::new(Duration::from_millis(if reduced { 0 } else { 160 }))
                    .with_easing(crate::design::spring_out),
                |d, t| d.opacity(0.6 + 0.4 * t),
            )
    }
}

/// A rubber band in progress: where it started and what was selected before it.
pub(super) struct BandDrag {
    pub band: Band,
    pub base: Vec<String>,
}

/// Folder hovered during a drag, opened when the pointer rests on it.
pub(super) const SPRING_DELAY: Duration = Duration::from_millis(900);

impl FilePane {
    /// Start dragging the entry under the pointer, together with the rest of the selection.
    pub(super) fn drag_value(&self, entry: &Entry, pane: EntityId) -> DraggedItems {
        let paths = if self.is_selected(&entry.path) {
            self.selected_entries()
                .into_iter()
                .map(|entry| entry.path)
                .collect()
        } else {
            vec![entry.path.clone()]
        };
        let folders = paths
            .iter()
            .filter(|path| {
                self.entry_for(path)
                    .is_some_and(|entry| entry.is_directory && !entry.is_symlink)
            })
            .cloned()
            .collect();
        DraggedItems {
            pane,
            generation: self.worker_generation,
            paths,
            folders,
            first_name: entry.name.clone(),
            first_is_folder: entry.is_directory,
        }
    }

    pub(super) fn accepts_items(&self, items: &DraggedItems, target: &str, copy: bool) -> bool {
        self.available
            && items.generation == self.worker_generation
            && drop_allowed(&items.paths, &items.folders, target, copy)
    }

    /// Internal drop: move by default, copy with Ctrl (Option on macOS).
    pub(super) fn drop_items(
        &mut self,
        items: &DraggedItems,
        target: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.spring = None;
        let copy = wants_copy(window);
        if items.pane != cx.entity_id() || !self.accepts_items(items, &target, copy) {
            return;
        }
        self.drop_pulse = Some((target.clone(), Instant::now()));
        self.drop_serial = self.drop_serial.wrapping_add(1);
        let mode = if copy {
            TransferMode::Copy
        } else {
            TransferMode::Move
        };
        self.transfer(items.paths.clone(), target, mode, cx);
        self.list_focus.focus(window, cx);
    }

    /// Files dropped from the desktop are uploaded into `target`.
    pub(super) fn drop_external(
        &mut self,
        paths: &ExternalPaths,
        target: String,
        cx: &mut Context<Self>,
    ) {
        self.spring = None;
        let paths = paths.paths().to_vec();
        if paths.is_empty() {
            return;
        }
        if !self.available {
            self.message = tr("file-reconnect-first");
            cx.notify();
            return;
        }
        self.drop_pulse = Some((target.clone(), Instant::now()));
        self.drop_serial = self.drop_serial.wrapping_add(1);
        self.upload_files(paths, Some(target), false, cx);
    }

    /// Track the folder under a drag; open it after a short rest, as file managers do.
    pub(super) fn spring_hover(
        &mut self,
        path: &str,
        bounds: Bounds<Pixels>,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let inside = bounds.contains(&position);
        let current = self.spring.as_ref().is_some_and(|(p, _)| p == path);
        if !inside {
            if current {
                self.spring = None;
                cx.notify();
            }
            return;
        }
        if current {
            return;
        }
        let started = Instant::now();
        self.spring = Some((path.to_owned(), started));
        cx.notify();
        let path = path.to_owned();
        cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(SPRING_DELAY).await;
            let _ = this.update(cx, |this, cx| {
                if this.spring.as_ref() == Some(&(path.clone(), started)) && cx.has_active_drag() {
                    this.spring = None;
                    this.navigate(path, Navigation::Visit, cx);
                }
            });
        })
        .detach();
    }

    /// Scroll the listing while a drag or band rests near its top or bottom edge.
    pub(super) fn edge_scroll(&self, position: Point<Pixels>) -> bool {
        let handle = &self.list_scroll.0.borrow().base_handle;
        let bounds = handle.bounds();
        let edge = px(40.);
        let delta = if position.y < bounds.top() + edge && position.y >= bounds.top() - edge {
            px(18.)
        } else if position.y > bounds.bottom() - edge && position.y <= bounds.bottom() + edge {
            px(-18.)
        } else {
            return false;
        };
        let mut offset = handle.offset();
        let max = handle.max_offset();
        offset.y = (offset.y + delta).min(px(0.)).max(-max.y);
        handle.set_offset(offset);
        true
    }

    /// Geometry of the listing: content-space point, cell pitch and row height.
    fn layout_metrics(&self, position: Point<Pixels>) -> ((f32, f32), f32, f32) {
        let handle = &self.list_scroll.0.borrow().base_handle;
        let bounds = handle.bounds();
        let offset = handle.offset();
        let grid = self.full_page && self.grid;
        // Grid rows have 12px side padding and 12px gaps between cells.
        let inset = if grid { 12. } else { 0. };
        let width = f32::from(bounds.size.width) - inset * 2.;
        let columns = self.columns.max(1) as f32;
        let pitch = if grid { (width + 12.) / columns } else { width };
        let row = if grid { 164. } else { 56. };
        let point = (
            f32::from(position.x - bounds.origin.x) - inset,
            f32::from(position.y - bounds.origin.y - offset.y),
        );
        (point, pitch, row)
    }

    /// Mouse down in the listing: start a rubber band when it is not on an item.
    pub(super) fn begin_band(
        &mut self,
        position: Point<Pixels>,
        add: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (point, pitch, row) = self.layout_metrics(position);
        let probe = Band {
            start: point,
            end: point,
        };
        let on_item = !probe
            .hits(self.visible.len(), self.columns.max(1), pitch, row)
            .is_empty()
            && (self.columns <= 1 || point.0.rem_euclid(pitch) < pitch - 12.);
        if on_item || point.0 < 0. {
            return;
        }
        self.list_focus.focus(window, cx);
        let base = if add {
            self.selection.ordered(&self.visible_paths())
        } else {
            vec![]
        };
        self.band = Some(BandDrag { band: probe, base });
        cx.notify();
    }
    pub(super) fn update_band(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(mut drag) = self.band.take() else {
            return;
        };
        self.edge_scroll(position);
        let (point, pitch, row) = self.layout_metrics(position);
        drag.band.end = point;
        if drag.band.is_meaningful() {
            let order = self.visible_paths();
            let hits = drag
                .band
                .hits(order.len(), self.columns.max(1), pitch, row)
                .into_iter()
                .filter_map(|index| order.get(index).cloned())
                .collect();
            self.selection.set_band(hits, &drag.base);
        }
        self.band = Some(drag);
        cx.notify();
    }
    pub(super) fn end_band(&mut self, cx: &mut Context<Self>) {
        if let Some(drag) = self.band.take() {
            if !drag.band.is_meaningful() && drag.base.is_empty() {
                // A plain click on empty space clears the selection.
                self.selection.clear();
            }
            cx.notify();
        }
    }
    /// The band rectangle in window coordinates, for drawing.
    pub(super) fn band_rect(&self) -> Option<Bounds<Pixels>> {
        let drag = self.band.as_ref()?;
        if !drag.band.is_meaningful() {
            return None;
        }
        let handle = &self.list_scroll.0.borrow().base_handle;
        let bounds = handle.bounds();
        let offset = handle.offset();
        let inset = if self.full_page && self.grid { 12. } else { 0. };
        let (a, b) = (drag.band.start, drag.band.end);
        let left = a.0.min(b.0) + inset;
        let top = a.1.min(b.1) + f32::from(offset.y);
        let width = (a.0 - b.0).abs();
        let height = (a.1 - b.1).abs();
        Some(Bounds::new(
            gpui::point(px(left), px(top)),
            gpui::size(px(width), px(height)),
        ))
        .map(|rect| {
            // Clip to the visible listing.
            let visible = Bounds::new(gpui::point(px(0.), px(0.)), bounds.size);
            rect.intersect(&visible)
        })
    }

    /// Attach drop targets for internal and desktop drags to a folder element.
    pub(super) fn folder_drop_target<E>(
        &self,
        element: E,
        target: String,
        cx: &mut Context<Self>,
    ) -> E
    where
        E: InteractiveElement + StatefulInteractiveElement + Styled,
    {
        let theme = cx.theme();
        let (primary, tint, danger) = (theme.primary, theme.accent, theme.danger);
        let weak = cx.entity().downgrade();
        let allowed_target = target.clone();
        let style_target = target.clone();
        let style_weak = weak.clone();
        let internal_target = target.clone();
        let external_target = target.clone();
        element
            .can_drop(move |value, window, cx| {
                if let Some(items) = value.downcast_ref::<DraggedItems>() {
                    weak.upgrade().is_some_and(|pane| {
                        items.pane == pane.entity_id()
                            && pane.read(cx).accepts_items(
                                items,
                                &allowed_target,
                                wants_copy(window),
                            )
                    })
                } else {
                    value.downcast_ref::<ExternalPaths>().is_some()
                }
            })
            .drag_over::<DraggedItems>(move |style, items, window, cx| {
                let allowed = style_weak.upgrade().is_some_and(|pane| {
                    items.pane == pane.entity_id()
                        && pane
                            .read(cx)
                            .accepts_items(items, &style_target, wants_copy(window))
                });
                if allowed {
                    style.bg(tint).border_color(primary)
                } else {
                    style.border_color(danger).opacity(0.7)
                }
            })
            .drag_over::<ExternalPaths>(move |style, _, _, _| style.bg(tint).border_color(primary))
            .on_drop(cx.listener(move |this, items: &DraggedItems, window, cx| {
                this.drop_items(items, internal_target.clone(), window, cx);
                cx.stop_propagation();
            }))
            .on_drop(cx.listener(move |this, paths: &ExternalPaths, _, cx| {
                this.drop_external(paths, external_target.clone(), cx);
                cx.stop_propagation();
            }))
    }

    /// The listing itself accepts drops into the open folder and starts rubber bands.
    pub(super) fn listing_interactions(
        &self,
        element: gpui::Stateful<gpui::Div>,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let directory = self.directory.clone();
        let element = self.folder_drop_target(element, directory, cx);
        element
            .on_drag_move(
                cx.listener(|this, event: &DragMoveEvent<DraggedItems>, _, cx| {
                    if this.edge_scroll(event.event.position) {
                        cx.notify();
                    }
                }),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &gpui::MouseDownEvent, window, cx| {
                    let add = browser_primary(&event.modifiers) || event.modifiers.shift;
                    this.begin_band(event.position, add, window, cx);
                }),
            )
            .on_mouse_move(cx.listener(|this, event: &gpui::MouseMoveEvent, _, cx| {
                if this.band.is_some() {
                    if event.pressed_button == Some(MouseButton::Left) {
                        this.update_band(event.position, cx);
                    } else {
                        this.end_band(cx);
                    }
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| this.end_band(cx)),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| this.end_band(cx)),
            )
    }
}

fn browser_primary(modifiers: &gpui::Modifiers) -> bool {
    if cfg!(target_os = "macos") {
        modifiers.platform
    } else {
        modifiers.control
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(items: &[&str]) -> Vec<String> {
        items.iter().map(|p| (*p).to_owned()).collect()
    }

    #[test]
    fn drops_refuse_self_descendants_and_no_op_moves() {
        let items = paths(&["/srv/app", "/srv/readme.md"]);
        let folders = paths(&["/srv/app"]);
        assert!(drop_allowed(&items, &folders, "/srv/other", false));
        assert!(
            !drop_allowed(&items, &folders, "/srv/app", false),
            "onto itself"
        );
        assert!(
            !drop_allowed(&items, &folders, "/srv/app/nested", true),
            "inside itself"
        );
        // A folder with a shared prefix is a different folder.
        assert!(drop_allowed(&items, &folders, "/srv/application", false));
        assert!(
            !drop_allowed(&items, &folders, "/srv", false),
            "move to same parent"
        );
        assert!(
            drop_allowed(&items, &folders, "/srv", true),
            "copy duplicates"
        );
        assert!(!drop_allowed(&[], &[], "/srv", true));
        assert!(!drop_allowed(&paths(&["/a.txt"]), &[], "/", false));
        assert!(drop_allowed(&paths(&["/srv/a.txt"]), &[], "/", false));
    }
}
