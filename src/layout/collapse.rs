//! Generic collapse state belongs to the canonical tree, never to pane runtimes.
use super::{PaneId, TileLayout, TypedNode};

#[cfg(test)]
mod tests;

impl TypedNode {
    pub(crate) fn content(&self) -> &Self {
        match self {
            Self::Collapsed { node } => node.content(),
            _ => self,
        }
    }

    pub(crate) fn collapsed(self) -> Self {
        match self {
            Self::Collapsed { .. } => self,
            node => Self::Collapsed {
                node: Box::new(node),
            },
        }
    }
}

impl TileLayout {
    pub(crate) fn pane_path(&self, pane: PaneId) -> Option<Vec<usize>> {
        super::leaf_path(&self.root, pane)
    }

    pub(crate) fn node_members(&self, path: &[usize]) -> Option<Vec<PaneId>> {
        let mut members = Vec::new();
        self.root.at(path)?.collect_all_panes(&mut members);
        members.sort_by_key(|pane| pane.raw());
        Some(members)
    }

    pub(crate) fn has_ancestor_members(&self, anchor: PaneId, members: &[PaneId]) -> bool {
        let Some(path) = self.pane_path(anchor) else {
            return false;
        };
        (0..=path.len()).any(|depth| self.node_members(&path[..depth]).as_deref() == Some(members))
    }

    /// Desired-state edit. Paths address structural children, never wrappers.
    /// Public callers must validate their incarnation/tree fence before this edit.
    pub(crate) fn set_collapsed(&mut self, path: &[usize], collapsed: bool) -> bool {
        let Some(slot) = self.root.at_mut(path) else {
            return false;
        };
        if matches!(slot, TypedNode::Collapsed { .. }) == collapsed {
            return false;
        }
        let old = std::mem::replace(slot, TypedNode::Leaf(PaneId::from_raw(0)));
        *slot = if collapsed {
            old.collapsed()
        } else if let TypedNode::Collapsed { node } = old {
            *node
        } else {
            old
        };
        true
    }
}
