//! Nested terminal splits within one tab. The primary pane owns the tab's connection
//! and can only leave by closing the tab.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) struct PaneId(pub u64);

impl PaneId {
    pub const PRIMARY: Self = Self(0);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Axis {
    /// Children sit side by side.
    Horizontal,
    /// Children are stacked top to bottom.
    Vertical,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) enum PaneNode {
    Leaf(PaneId),
    Split {
        id: u64,
        axis: Axis,
        /// Share of the space given to `first`.
        ratio: f32,
        first: Box<PaneNode>,
        second: Box<PaneNode>,
    },
}

pub(super) const MIN_RATIO: f32 = 0.15;
pub(super) const MAX_RATIO: f32 = 0.85;

#[derive(Debug)]
pub(super) struct PaneLayout {
    pub root: PaneNode,
    next_pane: u64,
    next_split: u64,
}

impl Default for PaneLayout {
    fn default() -> Self {
        Self {
            root: PaneNode::Leaf(PaneId::PRIMARY),
            next_pane: 0,
            next_split: 0,
        }
    }
}

impl PaneLayout {
    pub fn is_split(&self) -> bool {
        matches!(self.root, PaneNode::Split { .. })
    }

    pub fn contains(&self, pane: PaneId) -> bool {
        self.leaves().contains(&pane)
    }

    /// Panes in reading order: left to right, top to bottom.
    pub fn leaves(&self) -> Vec<PaneId> {
        fn walk(node: &PaneNode, out: &mut Vec<PaneId>) {
            match node {
                PaneNode::Leaf(id) => out.push(*id),
                PaneNode::Split { first, second, .. } => {
                    walk(first, out);
                    walk(second, out);
                }
            }
        }
        let mut out = Vec::new();
        walk(&self.root, &mut out);
        out
    }

    /// Divide `target` in two along `axis`; the new pane follows it.
    pub fn split(&mut self, target: PaneId, axis: Axis) -> Option<PaneId> {
        fn find(node: &mut PaneNode, target: PaneId) -> Option<&mut PaneNode> {
            match node {
                PaneNode::Leaf(id) if *id == target => Some(node),
                PaneNode::Leaf(_) => None,
                PaneNode::Split { first, second, .. } => {
                    find(first, target).or_else(|| find(second, target))
                }
            }
        }
        let leaf = find(&mut self.root, target)?;
        self.next_pane += 1;
        self.next_split += 1;
        let pane = PaneId(self.next_pane);
        *leaf = PaneNode::Split {
            id: self.next_split,
            axis,
            ratio: 0.5,
            first: Box::new(PaneNode::Leaf(target)),
            second: Box::new(PaneNode::Leaf(pane)),
        };
        Some(pane)
    }

    /// Remove a pane; its sibling takes the parent's space. Returns the pane that
    /// should receive focus next, or `None` when nothing was removed.
    pub fn remove(&mut self, target: PaneId) -> Option<PaneId> {
        fn first_leaf(node: &PaneNode) -> PaneId {
            match node {
                PaneNode::Leaf(id) => *id,
                PaneNode::Split { first, .. } => first_leaf(first),
            }
        }
        fn take(node: &mut PaneNode, target: PaneId) -> Option<PaneId> {
            let PaneNode::Split { first, second, .. } = node else {
                return None;
            };
            let sibling = if **first == PaneNode::Leaf(target) {
                std::mem::replace(second.as_mut(), PaneNode::Leaf(target))
            } else if **second == PaneNode::Leaf(target) {
                std::mem::replace(first.as_mut(), PaneNode::Leaf(target))
            } else {
                return take(first, target).or_else(|| take(second, target));
            };
            let focus = first_leaf(&sibling);
            *node = sibling;
            Some(focus)
        }
        if target == PaneId::PRIMARY {
            return None;
        }
        take(&mut self.root, target)
    }

    pub fn set_ratio(&mut self, split: u64, ratio: f32) {
        fn walk(node: &mut PaneNode, split: u64, value: f32) {
            if let PaneNode::Split {
                id,
                ratio,
                first,
                second,
                ..
            } = node
            {
                if *id == split {
                    *ratio = value;
                } else {
                    walk(first, split, value);
                    walk(second, split, value);
                }
            }
        }
        if ratio.is_finite() {
            walk(&mut self.root, split, ratio.clamp(MIN_RATIO, MAX_RATIO));
        }
    }

    pub fn ratio(&self, split: u64) -> Option<f32> {
        fn walk(node: &PaneNode, split: u64) -> Option<f32> {
            match node {
                PaneNode::Leaf(_) => None,
                PaneNode::Split {
                    id,
                    ratio,
                    first,
                    second,
                    ..
                } => (*id == split)
                    .then_some(*ratio)
                    .or_else(|| walk(first, split))
                    .or_else(|| walk(second, split)),
            }
        }
        walk(&self.root, split)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splitting_inserts_the_new_pane_after_its_target() {
        let mut layout = PaneLayout::default();
        let right = layout.split(PaneId::PRIMARY, Axis::Horizontal).unwrap();
        let below = layout.split(PaneId::PRIMARY, Axis::Vertical).unwrap();
        assert_eq!(layout.leaves(), vec![PaneId::PRIMARY, below, right]);
        assert!(layout.is_split());
        assert_eq!(layout.split(PaneId(99), Axis::Vertical), None);
    }

    #[test]
    fn removing_a_pane_collapses_its_split_and_focuses_the_sibling() {
        let mut layout = PaneLayout::default();
        let right = layout.split(PaneId::PRIMARY, Axis::Horizontal).unwrap();
        let below_right = layout.split(right, Axis::Vertical).unwrap();
        assert_eq!(layout.remove(right), Some(below_right));
        assert_eq!(layout.leaves(), vec![PaneId::PRIMARY, below_right]);
        assert_eq!(layout.remove(below_right), Some(PaneId::PRIMARY));
        assert_eq!(layout.root, PaneNode::Leaf(PaneId::PRIMARY));
        assert!(!layout.is_split());
    }

    #[test]
    fn primary_and_unknown_panes_cannot_be_removed() {
        let mut layout = PaneLayout::default();
        let other = layout.split(PaneId::PRIMARY, Axis::Horizontal).unwrap();
        assert_eq!(layout.remove(PaneId::PRIMARY), None);
        assert_eq!(layout.remove(PaneId(42)), None);
        assert_eq!(layout.leaves(), vec![PaneId::PRIMARY, other]);
    }

    #[test]
    fn identities_are_never_reused_after_removal() {
        let mut layout = PaneLayout::default();
        let first = layout.split(PaneId::PRIMARY, Axis::Horizontal).unwrap();
        layout.remove(first);
        let second = layout.split(PaneId::PRIMARY, Axis::Horizontal).unwrap();
        assert_ne!(first, second);
        assert!(layout.contains(second) && !layout.contains(first));
    }

    #[test]
    fn ratios_are_clamped_and_target_one_split() {
        let mut layout = PaneLayout::default();
        let right = layout.split(PaneId::PRIMARY, Axis::Horizontal).unwrap();
        layout.split(right, Axis::Vertical);
        layout.set_ratio(1, 0.99);
        layout.set_ratio(2, 0.01);
        layout.set_ratio(2, f32::NAN);
        assert_eq!(layout.ratio(1), Some(MAX_RATIO));
        assert_eq!(layout.ratio(2), Some(MIN_RATIO));
        assert_eq!(layout.ratio(3), None);
    }
}
