//! Visible tabs do not own SSH sessions. Closing a tab can leave its session alive.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) struct SessionId(pub u64);

#[derive(Default, Debug)]
pub(super) struct SessionTabs {
    pub open: Vec<SessionId>,
    pub active: Option<SessionId>,
    next_id: u64,
}

impl SessionTabs {
    pub fn allocate(&mut self) -> SessionId {
        self.next_id += 1;
        let id = SessionId(self.next_id);
        self.show(id);
        id
    }

    pub fn show(&mut self, id: SessionId) {
        if !self.open.contains(&id) {
            self.open.push(id);
        }
        self.active = Some(id);
    }

    pub fn hide(&mut self, id: SessionId) {
        let Some(index) = self.open.iter().position(|item| *item == id) else {
            return;
        };
        self.open.remove(index);
        if self.active == Some(id) {
            self.active = self
                .open
                .get(index.min(self.open.len().saturating_sub(1)))
                .copied();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closing_inactive_tab_does_not_change_active_identity() {
        let mut tabs = SessionTabs::default();
        let first = tabs.allocate();
        let second = tabs.allocate();
        tabs.hide(first);
        assert_eq!(tabs.active, Some(second));
        assert_eq!(tabs.open, vec![second]);
    }

    #[test]
    fn hidden_session_reopens_with_same_identity_without_duplicates() {
        let mut tabs = SessionTabs::default();
        let id = tabs.allocate();
        tabs.hide(id);
        assert_eq!(tabs.active, None);
        tabs.show(id);
        tabs.show(id);
        assert_eq!(tabs.open, vec![id]);
        assert_ne!(tabs.allocate(), id);
    }

    #[test]
    fn closing_last_visible_tab_selects_previous() {
        let mut tabs = SessionTabs::default();
        let first = tabs.allocate();
        let last = tabs.allocate();
        tabs.hide(last);
        assert_eq!(tabs.active, Some(first));
        tabs.hide(last);
        assert_eq!(tabs.active, Some(first));
    }
}
