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
