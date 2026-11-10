//! Name conflicts from any transfer, decided in one Thunar-style dialog.
use super::*;
use gpui_component::checkbox::Checkbox;
use opsssh_sftp::{Conflict, ConflictDecision, ConflictKind, ConflictReply};

/// A worker waiting on the user's decision for one conflict.
pub(super) struct PendingConflict {
    pub conflict: Conflict,
    pub reply: Sender<ConflictReply>,
}

impl FilePane {
    /// Record a worker's question and show it unless another conflict is already open.
    pub(super) fn receive_conflict(
        &mut self,
        id: u64,
        conflict: Conflict,
        reply: Sender<ConflictReply>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(task) = self.transfers.iter_mut().find(|task| task.id == id) else {
            return;
        };
        if task.state == TransferState::Cancelling || task.control.ensure_active().is_err() {
            return;
        }
        task.state = TransferState::AwaitingReplace;
        task.conflict = Some(PendingConflict { conflict, reply });
        // A dismissed active transfer must become visible when it requires a decision.
        task.dismissed = false;
        self.show_next_conflict(window, cx);
        cx.notify();
    }

    /// Open the dialog for the oldest unanswered conflict, one at a time.
    pub(super) fn show_next_conflict(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.conflict_dialog.is_some() || window.has_active_dialog(cx) {
            return;
        }
        let Some(id) = self
            .transfers
            .iter()
            .find(|task| task.conflict.is_some())
            .map(|task| task.id)
        else {
            return;
        };
        self.conflict_dialog = Some(id);
        self.conflict_apply_all = false;
        let weak = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, window, cx| {
            let Some(pane) = weak.upgrade() else {
                return dialog;
            };
            let state = pane.read(cx);
            let Some(pending) = state
                .transfers
                .iter()
                .find(|task| Some(task.id) == state.conflict_dialog)
                .and_then(|task| task.conflict.as_ref())
            else {
                return dialog;
            };
            let conflict = &pending.conflict;
            let kind = conflict.kind();
            let name = conflict
                .destination
                .rsplit(['/', '\\'])
                .next()
                .unwrap_or(&conflict.destination)
                .to_owned();
            let title = match kind {
                ConflictKind::Folders => format!("{} “{name}”?", tr("conflict-merge-title")),
                ConflictKind::Files => format!("{} “{name}”?", tr("conflict-replace-title")),
                ConflictKind::Mismatch => format!("“{name}” {}", tr("conflict-exists-title")),
            };
            let body = conflict_body(conflict, state.conflict_apply_all, weak.clone(), cx);
            let decide = |decision: ConflictDecision| {
                let weak = weak.clone();
                move |_: &gpui::ClickEvent, window: &mut Window, cx: &mut gpui::App| {
                    let _ = weak.update(cx, |this, cx| {
                        let apply_to_all = this.conflict_apply_all;
                        this.answer_open_conflict(
                            ConflictReply {
                                decision,
                                apply_to_all,
                            },
                            cx,
                        );
                    });
                    window.close_dialog(cx);
                    show_next_after_close(weak.clone(), window, cx);
                }
            };
            let footer = div()
                .flex()
                .flex_wrap()
                .justify_end()
                .gap_2()
                .child(
                    Button::new("conflict-cancel")
                        .ghost()
                        .label(tr("conflict-cancel"))
                        .on_click(decide(ConflictDecision::Cancel)),
                )
                .child(
                    Button::new("conflict-skip")
                        .label(tr("conflict-skip"))
                        .on_click(decide(ConflictDecision::Skip)),
                )
                .child(
                    Button::new("conflict-keep-both")
                        .label(tr("conflict-keep-both"))
                        .on_click(decide(ConflictDecision::KeepBoth)),
                )
                .when(kind != ConflictKind::Mismatch, |d| {
                    d.child(
                        crate::design::PrimaryAction::new("conflict-replace")
                            .label(tr(if kind == ConflictKind::Folders {
                                "conflict-merge"
                            } else {
                                "conflict-replace"
                            }))
                            .on_click(decide(ConflictDecision::Replace)),
                    )
                });
            let closed = weak.clone();
            dialog
                .title(title)
                .width(px(520.).min(window.viewport_size().width - px(48.)))
                .child(body)
                .footer(footer)
                .on_close(move |_, window, cx| {
                    // Esc or the close button never replaces anything: skip this item.
                    let _ = closed.update(cx, |this, cx| {
                        this.answer_open_conflict(ConflictReply::once(ConflictDecision::Skip), cx);
                    });
                    show_next_after_close(closed.clone(), window, cx);
                })
        });
    }

    /// Send a decision for the conflict shown in the dialog, at most once.
    fn answer_open_conflict(&mut self, reply: ConflictReply, cx: &mut Context<Self>) {
        if let Some(id) = self.conflict_dialog.take() {
            self.resolve_conflict(id, reply, cx);
        }
    }

    /// Called while rendering: schedule the next question when no dialog is open.
    pub(super) fn schedule_conflict(&self, window: &mut Window, cx: &mut Context<Self>) {
        if self.conflict_dialog.is_none()
            && self.transfers.iter().any(|task| task.conflict.is_some())
            && !window.has_active_dialog(cx)
        {
            cx.defer_in(window, |this, window, cx| {
                this.show_next_conflict(window, cx)
            });
        }
    }

    /// Answer a transfer's pending conflict. Returns whether the worker received it.
    pub(super) fn resolve_conflict(
        &mut self,
        id: u64,
        reply: ConflictReply,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(task) = self.transfers.iter_mut().find(|task| task.id == id) else {
            return false;
        };
        let Some(pending) = task.conflict.take() else {
            return false;
        };
        let sent = task.control.ensure_active().is_ok() && pending.reply.try_send(reply).is_ok();
        if sent {
            task.state = if reply.decision == ConflictDecision::Cancel {
                TransferState::Cancelling
            } else {
                TransferState::Running
            };
        }
        cx.notify();
        sent
    }

    /// Reopen the decision dialog for a transfer whose question is still waiting.
    pub(super) fn review_conflict(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        if self.conflict_dialog.is_none()
            && self
                .transfers
                .iter()
                .any(|task| task.id == id && task.conflict.is_some())
        {
            // Show this transfer first by moving it ahead of other waiting ones.
            if let Some(index) = self.transfers.iter().position(|task| task.id == id) {
                let task = self.transfers.remove(index);
                let first = self
                    .transfers
                    .iter()
                    .position(|task| task.conflict.is_some())
                    .unwrap_or(self.transfers.len());
                self.transfers.insert(first.min(index), task);
            }
            self.show_next_conflict(window, cx);
        }
    }
}

/// Ask the next waiting question once the closing dialog is gone. The pane may be
/// hidden (terminal drops), so this cannot rely on the pane rendering again.
fn show_next_after_close(
    weak: gpui::WeakEntity<FilePane>,
    window: &mut Window,
    cx: &mut gpui::App,
) {
    window.defer(cx, move |window, cx| {
        let _ = weak.update(cx, |this, cx| this.show_next_conflict(window, cx));
    });
}

fn conflict_body(
    conflict: &Conflict,
    apply_to_all: bool,
    weak: gpui::WeakEntity<FilePane>,
    cx: &gpui::App,
) -> gpui::AnyElement {
    let muted = cx.theme().muted_foreground;
    let border = cx.theme().border;
    let describe = |label: String, attrs: &RemoteAttributes| {
        let kind = if attrs.is_symlink {
            tr("file-type-link")
        } else if attrs.is_directory {
            tr("file-type-folder")
        } else {
            attrs
                .size
                .map(format_bytes)
                .unwrap_or_else(|| tr("file-type-file"))
        };
        let modified = attrs
            .modified
            .map(format_modified)
            .unwrap_or_else(|| "—".into());
        div()
            .flex_1()
            .min_w_0()
            .p_3()
            .rounded_md()
            .border_1()
            .border_color(border)
            .flex()
            .flex_col()
            .gap_1()
            .child(div().text_xs().text_color(muted).child(label))
            .child(div().text_sm().child(kind))
            .child(div().text_xs().text_color(muted).child(modified))
    };
    let help = match conflict.kind() {
        ConflictKind::Files => tr("conflict-files-help"),
        ConflictKind::Folders => tr("conflict-folders-help"),
        ConflictKind::Mismatch => tr("conflict-mismatch-help"),
    };
    div()
        .flex()
        .flex_col()
        .gap_3()
        .child(div().text_sm().child(help))
        .child(
            div()
                .text_xs()
                .text_color(muted)
                .truncate()
                .child(conflict.destination.clone()),
        )
        .child(
            div()
                .flex()
                .gap_3()
                .child(describe(tr("conflict-existing"), &conflict.existing))
                .child(describe(tr("conflict-incoming"), &conflict.incoming)),
        )
        .child(
            Checkbox::new("conflict-apply-all")
                .label(tr("conflict-apply-all"))
                .checked(apply_to_all)
                .on_click(move |checked, _, cx| {
                    let _ = weak.update(cx, |this, cx| {
                        this.conflict_apply_all = *checked;
                        cx.notify();
                    });
                }),
        )
        .into_any_element()
}

/// A file-vs-file conflict for development previews and tests.
#[cfg(any(test, feature = "capture"))]
pub(super) fn sample_conflict(destination: &str) -> Conflict {
    let file = |size: u64, modified: u32| RemoteAttributes {
        mode: Some(0o644),
        uid: None,
        gid: None,
        is_directory: false,
        is_symlink: false,
        is_regular: true,
        size: Some(size),
        modified: Some(modified),
    };
    Conflict {
        source: "/home/user/test.txt".into(),
        destination: destination.into(),
        incoming: file(2048, 1_767_225_600),
        existing: file(512, 1_735_689_600),
    }
}

/// Ask the user about one conflict and wait, honoring cancellation and a closed pane.
pub(super) async fn request_conflict(
    updates: &Sender<Update>,
    id: u64,
    conflict: Conflict,
    control: &TransferControl,
) -> opsssh_sftp::Result<ConflictReply> {
    control.ensure_active()?;
    let (reply, receive) = async_channel::bounded(1);
    updates.send(Update::Conflict(id, conflict, reply)).await?;
    loop {
        tokio::select! {
            decision = receive.recv() => { control.ensure_active()?; return Ok(decision?); },
            _ = tokio::time::sleep(Duration::from_millis(100)) => {
                control.ensure_active()?;
                if updates.is_closed() {
                    return Err("File pane closed; existing item kept".into());
                }
            }
        }
    }
}
