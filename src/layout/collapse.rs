//! Generic collapse state belongs to the canonical tree, never to pane runtimes.
use super::{PaneId, TileLayout, TypedNode};
use sha2::{Digest, Sha256};

/// Structural request-local addresses; wrappers consume no step or identity.
#[derive(
    Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum LayoutPathStep {
    Split(LayoutSplitChild),
    StackChild(usize),
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum LayoutSplitChild {
    First,
    Second,
}

pub(super) fn fingerprint(node: &TypedNode) -> String {
    fn text(hash: &mut Sha256, value: &str) {
        hash.update((value.len() as u64).to_le_bytes());
        hash.update(value.as_bytes());
    }
    fn visit(hash: &mut Sha256, node: &TypedNode) {
        match node {
            TypedNode::Leaf(pane) => {
                hash.update([0]);
                hash.update(pane.raw().to_le_bytes());
            }
            TypedNode::View {
                pane,
                extension,
                view,
            } => {
                hash.update([1]);
                hash.update(pane.raw().to_le_bytes());
                text(hash, extension);
                text(hash, view);
            }
            TypedNode::Stack {
                id,
                selected,
                children,
            } => {
                hash.update([2]);
                hash.update(id.raw().to_le_bytes());
                hash.update((*selected as u64).to_le_bytes());
                hash.update((children.len() as u64).to_le_bytes());
                for child in children {
                    visit(hash, child);
                }
            }
            TypedNode::Split {
                direction,
                ratio,
                first,
                second,
            } => {
                hash.update([
                    3,
                    u8::from(*direction == ratatui::layout::Direction::Horizontal),
                ]);
                hash.update(ratio.to_bits().to_le_bytes());
                visit(hash, first);
                visit(hash, second);
            }
            TypedNode::Collapsed { node } => {
                hash.update([4]);
                visit(hash, node);
            }
        }
    }
    let mut hash = Sha256::new();
    hash.update(b"vqro.layout.tree-fingerprint.v1\0");
    visit(&mut hash, node);
    format!("{:x}", hash.finalize())
}

#[cfg(test)]
mod tests;

impl TypedNode {
    pub(crate) fn has_visible_collapse(&self) -> bool {
        match self {
            Self::Collapsed { .. } => true,
            Self::Split { first, second, .. } => {
                first.has_visible_collapse() || second.has_visible_collapse()
            }
            Self::Stack {
                selected, children, ..
            } => children
                .get(*selected)
                .is_some_and(Self::has_visible_collapse),
            Self::Leaf(_) | Self::View { .. } => false,
        }
    }

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
    /// Cached at canonical layout edits, never hashed per byte/frame/client.
    pub(crate) fn tree_fingerprint(&self) -> &str {
        &self.tree_fingerprint
    }

    #[cfg(test)]
    pub(crate) fn set_projection_fingerprint_for_test(&mut self, fingerprint: String) {
        self.tree_fingerprint = fingerprint;
    }

    /// Independent request-time verification fences missed projection invalidation.
    pub(crate) fn authoritative_fingerprint(&self) -> String {
        fingerprint(&self.root)
    }

    pub(super) fn refresh_tree_fingerprint(&mut self) {
        self.tree_fingerprint = fingerprint(&self.root);
    }

    pub(crate) fn pane_path(&self, pane: PaneId) -> Option<Vec<usize>> {
        super::leaf_path(&self.root, pane)
    }

    pub(crate) fn tagged_path(&self, path: &[usize]) -> Option<Vec<LayoutPathStep>> {
        let mut node = &self.root;
        let mut tagged = Vec::with_capacity(path.len());
        for &index in path {
            let (next, step) = match node.content() {
                TypedNode::Split { first, .. } if index == 0 => (
                    first.as_ref(),
                    LayoutPathStep::Split(LayoutSplitChild::First),
                ),
                TypedNode::Split { second, .. } if index == 1 => (
                    second.as_ref(),
                    LayoutPathStep::Split(LayoutSplitChild::Second),
                ),
                TypedNode::Stack { children, .. } => {
                    (children.get(index)?, LayoutPathStep::StackChild(index))
                }
                _ => return None,
            };
            tagged.push(step);
            node = next;
        }
        Some(tagged)
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
        self.refresh_tree_fingerprint();
        true
    }
}
