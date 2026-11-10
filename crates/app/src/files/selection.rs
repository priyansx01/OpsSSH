//! Multi-item selection keyed by remote path, so a refresh never selects the wrong item.

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Selection {
    /// Selected paths in the order they were added.
    items: Vec<String>,
    /// Where a Shift range starts.
    anchor: Option<String>,
    /// The item keyboard navigation moves from; it may be unselected after Ctrl+click.
    cursor: Option<String>,
}

impl Selection {
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.items.len()
    }
    pub fn contains(&self, path: &str) -> bool {
        self.items.iter().any(|item| item == path)
    }
    pub fn cursor(&self) -> Option<&str> {
        self.cursor.as_deref()
    }
    /// Selected paths in listing order.
    pub fn ordered(&self, order: &[String]) -> Vec<String> {
        order
            .iter()
            .filter(|path| self.contains(path))
            .cloned()
            .collect()
    }
    /// The single selected path, if exactly one item is selected.
    pub fn single(&self) -> Option<&str> {
        match self.items.as_slice() {
            [only] => Some(only),
            _ => None,
        }
    }
    pub fn clear(&mut self) {
        self.items.clear();
        self.anchor = None;
        self.cursor = None;
    }
    /// Plain click: this item only.
    pub fn select_only(&mut self, path: &str) {
        self.items = vec![path.to_owned()];
        self.anchor = Some(path.to_owned());
        self.cursor = Some(path.to_owned());
    }
    /// Ctrl/Cmd+click: add or remove one item, keeping the rest.
    pub fn toggle(&mut self, path: &str) {
        if let Some(index) = self.items.iter().position(|item| item == path) {
            self.items.remove(index);
        } else {
            self.items.push(path.to_owned());
        }
        self.anchor = Some(path.to_owned());
        self.cursor = Some(path.to_owned());
    }
    /// Shift+click: the range from the anchor to `path` in listing order.
    /// With `add`, the range joins the existing selection (Ctrl+Shift).
    pub fn extend_to(&mut self, path: &str, order: &[String], add: bool) {
        // Without a usable anchor the range starts at the keyboard cursor.
        let anchor = [&self.anchor, &self.cursor]
            .into_iter()
            .flatten()
            .find(|candidate| order.contains(candidate))
            .cloned()
            .unwrap_or_else(|| path.to_owned());
        let (Some(start), Some(end)) = (
            order.iter().position(|item| *item == anchor),
            order.iter().position(|item| item == path),
        ) else {
            return self.select_only(path);
        };
        let range = &order[start.min(end)..=start.max(end)];
        if !add {
            self.items.clear();
        }
        for item in range {
            if !self.contains(item) {
                self.items.push(item.clone());
            }
        }
        self.anchor = Some(anchor);
        self.cursor = Some(path.to_owned());
    }
    pub fn select_all(&mut self, order: &[String]) {
        self.items = order.to_vec();
        if self.cursor.as_ref().is_none_or(|c| !order.contains(c)) {
            self.cursor = order.first().cloned();
        }
        self.anchor = self.cursor.clone();
    }
    /// Rubber-band result: `paths`, optionally added to what was selected before the band.
    pub fn set_band(&mut self, paths: Vec<String>, base: &[String]) {
        let mut items = base.to_vec();
        for path in paths {
            if !items.contains(&path) {
                items.push(path);
            }
        }
        self.cursor = items.last().cloned();
        self.anchor = self.cursor.clone();
        self.items = items;
    }
    /// Keyboard step to the item at `index` of `order`; Shift extends from the anchor.
    pub fn move_to(&mut self, index: usize, order: &[String], extend: bool) {
        let Some(path) = order.get(index) else {
            return;
        };
        if extend {
            self.extend_to(&path.clone(), order, false);
        } else {
            self.select_only(&path.clone());
        }
    }
    /// Index of the cursor within `order`.
    pub fn cursor_index(&self, order: &[String]) -> Option<usize> {
        let cursor = self.cursor.as_ref()?;
        order.iter().position(|item| item == cursor)
    }
    /// Drop paths that are no longer listed, hidden or filtered out.
    pub fn retain_visible(&mut self, order: &[String]) {
        self.items.retain(|item| order.contains(item));
        if self.anchor.as_ref().is_some_and(|a| !order.contains(a)) {
            self.anchor = None;
        }
        if self.cursor.as_ref().is_some_and(|c| !order.contains(c)) {
            self.cursor = self.items.last().cloned();
        }
    }
}

/// A rubber-band rectangle in list content coordinates (scroll offset included).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Band {
    pub start: (f32, f32),
    pub end: (f32, f32),
}

impl Band {
    fn bounds(&self) -> (f32, f32, f32, f32) {
        (
            self.start.0.min(self.end.0),
            self.start.1.min(self.end.1),
            self.start.0.max(self.end.0),
            self.start.1.max(self.end.1),
        )
    }
    /// Indices into `count` items laid out in rows of `columns` cells, each `cell_width`
    /// wide and `row_height` tall, that the band touches.
    pub fn hits(
        &self,
        count: usize,
        columns: usize,
        cell_width: f32,
        row_height: f32,
    ) -> Vec<usize> {
        let (left, top, right, bottom) = self.bounds();
        if count == 0 || columns == 0 || cell_width <= 0. || row_height <= 0. {
            return vec![];
        }
        let rows = count.div_ceil(columns);
        let first_row = (top / row_height).floor().max(0.) as usize;
        let last_row = ((bottom / row_height).floor().max(0.) as usize).min(rows - 1);
        let first_column = (left / cell_width).floor().max(0.) as usize;
        let last_column = ((right / cell_width).floor().max(0.) as usize).min(columns - 1);
        if first_row >= rows || first_column >= columns {
            return vec![];
        }
        let mut hits = vec![];
        for row in first_row..=last_row {
            for column in first_column..=last_column {
                let index = row * columns + column;
                if index < count {
                    hits.push(index);
                }
            }
        }
        hits
    }
    /// Ignore tiny pointer movement so a click on empty space still just clears.
    pub fn is_meaningful(&self) -> bool {
        let (left, top, right, bottom) = self.bounds();
        right - left > 4. || bottom - top > 4.
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn order() -> Vec<String> {
        ["/a", "/b", "/c", "/d", "/e"].map(String::from).to_vec()
    }

    #[test]
    fn click_ctrl_and_shift_follow_file_manager_rules() {
        let order = order();
        let mut s = Selection::default();
        s.select_only("/b");
        s.extend_to("/d", &order, false);
        assert_eq!(s.ordered(&order), ["/b", "/c", "/d"]);
        // A new Shift range replaces the previous one, from the same anchor.
        s.extend_to("/a", &order, false);
        assert_eq!(s.ordered(&order), ["/a", "/b"]);
        s.toggle("/e");
        assert_eq!(s.ordered(&order), ["/a", "/b", "/e"]);
        s.toggle("/a");
        assert_eq!(s.ordered(&order), ["/b", "/e"]);
        // Ctrl+Shift adds the range from the last Ctrl+click.
        s.extend_to("/c", &order, true);
        assert_eq!(s.ordered(&order), ["/a", "/b", "/c", "/e"]);
        assert_eq!(s.cursor(), Some("/c"));
        s.select_all(&order);
        assert_eq!(s.len(), 5);
        s.clear();
        assert!(s.is_empty() && s.cursor().is_none());
    }

    #[test]
    fn refresh_prunes_missing_items_and_keeps_the_rest() {
        let mut s = Selection::default();
        s.select_only("/b");
        s.toggle("/d");
        s.retain_visible(&["/a", "/b", "/c"].map(String::from));
        assert_eq!(s.single(), Some("/b"));
        assert_eq!(s.cursor(), Some("/b"));
        // A pruned anchor falls back to the cursor that survived the refresh.
        s.extend_to("/c", &["/b", "/c"].map(String::from), false);
        assert_eq!(s.len(), 2);
    }

    #[test]
    fn keyboard_moves_and_extends_from_the_cursor() {
        let order = order();
        let mut s = Selection::default();
        s.move_to(1, &order, false);
        s.move_to(3, &order, true);
        assert_eq!(s.ordered(&order), ["/b", "/c", "/d"]);
        assert_eq!(s.cursor_index(&order), Some(3));
        s.move_to(99, &order, false);
        assert_eq!(s.cursor_index(&order), Some(3));
    }

    #[test]
    fn bands_hit_grid_cells_and_list_rows() {
        let band = Band {
            start: (250., 10.),
            end: (90., 180.),
        };
        // Three columns of 100px, rows of 164px: columns 0..=2, rows 0..=1.
        assert_eq!(band.hits(5, 3, 100., 164.), [0, 1, 2, 3, 4]);
        let list = Band {
            start: (10., 60.),
            end: (20., 120.),
        };
        assert_eq!(list.hits(10, 1, 400., 56.), [1, 2]);
        assert!(list.hits(0, 1, 400., 56.).is_empty());
        let outside = Band {
            start: (10., 900.),
            end: (20., 950.),
        };
        assert!(outside.hits(3, 1, 400., 56.).is_empty());
        assert!(
            !Band {
                start: (1., 1.),
                end: (3., 2.)
            }
            .is_meaningful()
        );
        let set = {
            let mut s = Selection::default();
            s.set_band(vec!["/c".into()], &["/a".into()]);
            s
        };
        assert_eq!(set.ordered(&order()), ["/a", "/c"]);
    }
}
