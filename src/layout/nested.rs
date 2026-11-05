//! Pure nested-box edits. No runtime, persistence, or product authority lives here.
use std::collections::{HashMap, HashSet};

use super::{LayoutLeaf, PaneId, TileLayout, TypedNode, ViewRef};

#[cfg(test)]
mod tests;
use crate::terminal_container::TerminalContainerId;

/// The tab root is not a box; every split and stack beneath it counts.
const MAX_BOX_DEPTH: usize = 4;

impl TypedNode {
    /// The pane identity of a pane leaf (terminal or view). Containers are `None`.
    pub(crate) fn pane_leaf(&self) -> Option<PaneId> {
        match self {
            Self::Collapsed { node } => node.pane_leaf(),
            Self::Leaf(pane) => Some(*pane),
            Self::View { pane, .. } => Some(*pane),
            Self::Stack { .. } | Self::Split { .. } => None,
        }
    }

    /// The content identity when this node is a view pane.
    pub(crate) fn view_ref(&self) -> Option<ViewRef> {
        match self {
            Self::Collapsed { node } => node.view_ref(),
            Self::View {
                pane,
                extension,
                view,
            } => Some(ViewRef {
                pane: *pane,
                extension: extension.clone(),
                view: view.clone(),
            }),
            _ => None,
        }
    }

    pub(crate) fn view(
        pane: PaneId,
        extension: impl Into<String>,
        view: impl Into<String>,
    ) -> Self {
        Self::View {
            pane,
            extension: extension.into(),
            view: view.into(),
        }
    }

    #[cfg(test)]
    pub(crate) fn empty_stack(id: TerminalContainerId) -> Self {
        Self::Stack {
            id,
            selected: 0,
            children: Vec::new(),
        }
    }

    /// Structural child addresses: binary splits use 0/1, stacks use child indices.
    pub(crate) fn at(&self, path: &[usize]) -> Option<&Self> {
        let Some((&index, rest)) = path.split_first() else {
            return Some(self);
        };
        match self {
            Self::Collapsed { node } => node.at(path),
            Self::Split { first, second, .. } => match index {
                0 => first.at(rest),
                1 => second.at(rest),
                _ => None,
            },
            Self::Stack { children, .. } => children.get(index)?.at(rest),
            Self::Leaf(_) | Self::View { .. } => None,
        }
    }

    pub(crate) fn at_mut(&mut self, path: &[usize]) -> Option<&mut Self> {
        let Some((&index, rest)) = path.split_first() else {
            return Some(self);
        };
        match self {
            Self::Collapsed { node } => node.at_mut(path),
            Self::Split { first, second, .. } => match index {
                0 => first.at_mut(rest),
                1 => second.at_mut(rest),
                _ => None,
            },
            Self::Stack { children, .. } => children.get_mut(index)?.at_mut(rest),
            Self::Leaf(_) | Self::View { .. } => None,
        }
    }

    pub(crate) fn stack(&self, id: TerminalContainerId) -> Option<&Self> {
        match self {
            Self::Collapsed { node } => node.stack(id),
            Self::Stack { id: own, .. } if *own == id => Some(self),
            Self::Stack { children, .. } => children.iter().find_map(|node| node.stack(id)),
            Self::Split { first, second, .. } => first.stack(id).or_else(|| second.stack(id)),
            Self::Leaf(_) | Self::View { .. } => None,
        }
    }

    pub(crate) fn stack_mut(&mut self, id: TerminalContainerId) -> Option<&mut Self> {
        match self {
            Self::Collapsed { node } => node.stack_mut(id),
            Self::Stack { id: own, .. } if *own == id => Some(self),
            Self::Stack { children, .. } => children.iter_mut().find_map(|node| node.stack_mut(id)),
            Self::Split { first, second, .. } => {
                first.stack_mut(id).or_else(|| second.stack_mut(id))
            }
            Self::Leaf(_) | Self::View { .. } => None,
        }
    }

    /// Every pane and stable box must occur exactly once; empty stacks select index zero.
    pub(crate) fn valid_structure(&self) -> bool {
        fn visit(
            node: &TypedNode,
            panes: &mut HashSet<PaneId>,
            boxes: &mut HashSet<TerminalContainerId>,
        ) -> bool {
            match node {
                TypedNode::Collapsed { node } => {
                    !matches!(**node, TypedNode::Collapsed { .. }) && visit(node, panes, boxes)
                }
                TypedNode::Leaf(id) => panes.insert(*id),
                TypedNode::View { pane, .. } => panes.insert(*pane),
                TypedNode::Split { first, second, .. } => {
                    visit(first, panes, boxes) && visit(second, panes, boxes)
                }
                TypedNode::Stack {
                    id,
                    selected,
                    children,
                } => {
                    boxes.insert(*id)
                        && (if children.is_empty() {
                            *selected == 0
                        } else {
                            *selected < children.len()
                        })
                        && children.iter().all(|child| visit(child, panes, boxes))
                }
            }
        }
        visit(self, &mut HashSet::new(), &mut HashSet::new())
    }

    pub(crate) fn select_pane(&mut self, pane: PaneId) -> bool {
        match self {
            Self::Collapsed { node } => node.select_pane(pane),
            Self::Leaf(id) => *id == pane,
            Self::View {
                pane: view_pane, ..
            } => *view_pane == pane,
            Self::Split { first, second, .. } => {
                first.select_pane(pane) || second.select_pane(pane)
            }
            Self::Stack {
                selected, children, ..
            } => {
                for (index, child) in children.iter_mut().enumerate() {
                    if child.select_pane(pane) {
                        *selected = index;
                        return true;
                    }
                }
                false
            }
        }
    }

    pub(crate) fn selected_pane(&self) -> Option<PaneId> {
        match self {
            Self::Collapsed { node } => node.selected_pane(),
            Self::Leaf(id) => Some(*id),
            Self::View { pane, .. } => Some(*pane),
            Self::Split { first, second, .. } => {
                first.selected_pane().or_else(|| second.selected_pane())
            }
            Self::Stack {
                selected, children, ..
            } => children.get(*selected)?.selected_pane(),
        }
    }

    pub(crate) fn contains_pane(&self, pane: PaneId) -> bool {
        match self {
            Self::Collapsed { node } => node.contains_pane(pane),
            Self::Leaf(id) => *id == pane,
            Self::View {
                pane: view_pane, ..
            } => *view_pane == pane,
            Self::Split { first, second, .. } => {
                first.contains_pane(pane) || second.contains_pane(pane)
            }
            Self::Stack { children, .. } => children.iter().any(|child| child.contains_pane(pane)),
        }
    }

    pub(crate) fn pane_count(&self) -> usize {
        match self {
            Self::Collapsed { node } => node.pane_count(),
            Self::Leaf(_) | Self::View { .. } => 1,
            Self::Split { first, second, .. } => first.pane_count() + second.pane_count(),
            Self::Stack { children, .. } => children.iter().map(Self::pane_count).sum(),
        }
    }

    pub(crate) fn collect_stack_ids(&self, ids: &mut Vec<TerminalContainerId>) {
        match self {
            Self::Collapsed { node } => node.collect_stack_ids(ids),
            Self::Leaf(_) | Self::View { .. } => {}
            Self::Split { first, second, .. } => {
                first.collect_stack_ids(ids);
                second.collect_stack_ids(ids);
            }
            Self::Stack { id, children, .. } => {
                ids.push(*id);
                for child in children {
                    child.collect_stack_ids(ids);
                }
            }
        }
    }

    pub(crate) fn collect_all_panes(&self, panes: &mut Vec<PaneId>) {
        match self {
            Self::Collapsed { node } => node.collect_all_panes(panes),
            Self::Leaf(id) => panes.push(*id),
            Self::View { pane, .. } => panes.push(*pane),
            Self::Split { first, second, .. } => {
                first.collect_all_panes(panes);
                second.collect_all_panes(panes);
            }
            Self::Stack { children, .. } => {
                for child in children {
                    child.collect_all_panes(panes);
                }
            }
        }
    }

    /// Every terminal pane inside this subtree, in tree order. View panes are not
    /// terminals and are excluded: a view covers terminals, not other views.
    pub(crate) fn collect_terminal_panes(&self, panes: &mut Vec<PaneId>) {
        match self {
            Self::Collapsed { node } => node.collect_terminal_panes(panes),
            Self::Leaf(id) => panes.push(*id),
            Self::View { .. } => {}
            Self::Split { first, second, .. } => {
                first.collect_terminal_panes(panes);
                second.collect_terminal_panes(panes);
            }
            Self::Stack { children, .. } => {
                for child in children {
                    child.collect_terminal_panes(panes);
                }
            }
        }
    }

    /// Every terminal in the nearest enclosing container with nonempty terminal
    /// membership, in tree order. View-only containers inherit that scope; hidden
    /// stack members still count. A standalone view or unknown pane has no scope.
    pub(crate) fn view_scope(&self, view_pane: PaneId) -> Vec<PaneId> {
        fn find(node: &TypedNode, pane: PaneId) -> Option<Vec<PaneId>> {
            let found = match node {
                TypedNode::Collapsed { node } => return find(node, pane),
                TypedNode::View { pane: own, .. } => return (*own == pane).then(Vec::new),
                TypedNode::Leaf(_) => return None,
                TypedNode::Split { first, second, .. } => {
                    find(first, pane).or_else(|| find(second, pane))
                }
                TypedNode::Stack { children, .. } => {
                    children.iter().find_map(|child| find(child, pane))
                }
            };
            found.map(|mut scope| {
                if scope.is_empty() {
                    node.collect_terminal_panes(&mut scope);
                }
                scope
            })
        }
        find(self, view_pane).unwrap_or_default()
    }
}

/// Preserve the selected child's identity across removal, choosing next then previous
/// when removing that child (the legacy TerminalContainer rule).
pub(super) fn remove_child(
    children: &mut Vec<TypedNode>,
    selected: &mut usize,
    index: usize,
) -> TypedNode {
    let node = children.remove(index);
    if index < *selected {
        *selected -= 1;
    } else if index == *selected {
        *selected = index.min(children.len().saturating_sub(1));
    }
    node
}

pub(super) fn reorder_child(
    children: &mut Vec<TypedNode>,
    selected: &mut usize,
    from: usize,
    to: usize,
) -> bool {
    if from >= children.len() || to >= children.len() {
        return false;
    }
    let previous = *selected;
    let node = children.remove(from);
    children.insert(to, node);
    *selected = if previous == from {
        to
    } else if from < previous && previous <= to {
        previous - 1
    } else if to <= previous && previous < from {
        previous + 1
    } else {
        previous
    };
    true
}

fn depths(node: &TypedNode) -> HashMap<LayoutLeaf, usize> {
    fn visit(node: &TypedNode, depth: usize, result: &mut HashMap<LayoutLeaf, usize>) {
        match node {
            TypedNode::Collapsed { node } => visit(node, depth, result),
            TypedNode::Leaf(pane) => {
                result.insert(LayoutLeaf::Pane(*pane), depth);
            }
            TypedNode::View { pane, .. } => {
                result.insert(LayoutLeaf::Pane(*pane), depth);
            }
            TypedNode::Split { first, second, .. } => {
                visit(first, depth + 1, result);
                visit(second, depth + 1, result);
            }
            TypedNode::Stack { id, children, .. } => {
                result.insert(LayoutLeaf::Stack(*id), depth + 1);
                for child in children {
                    visit(child, depth + 1, result);
                }
            }
        }
    }
    let mut result = HashMap::new();
    visit(node, 0, &mut result);
    result
}

/// Grandfathering is per identity, not a global maximum: an unrelated deep branch
/// must never mask growth on the branch being inserted/reparented.
fn allowed_growth(before: &TypedNode, after: &TypedNode) -> bool {
    let old = depths(before);
    depths(after).into_iter().all(|(id, depth)| {
        depth <= MAX_BOX_DEPTH || old.get(&id).is_some_and(|previous| depth <= *previous)
    })
}

fn take_at(node: TypedNode, path: &[usize]) -> Option<(Option<TypedNode>, TypedNode)> {
    let Some((&index, rest)) = path.split_first() else {
        return Some((None, node));
    };
    match node {
        TypedNode::Collapsed { node } => {
            let (remaining, removed) = take_at(*node, path)?;
            Some((remaining.map(TypedNode::collapsed), removed))
        }
        TypedNode::Split {
            direction,
            ratio,
            first,
            second,
        } => {
            let (chosen, sibling) = match index {
                0 => (*first, *second),
                1 => (*second, *first),
                _ => return None,
            };
            let (remaining, removed) = take_at(chosen, rest)?;
            let remaining = match remaining {
                None => sibling,
                Some(chosen) => {
                    let (first, second) = if index == 0 {
                        (chosen, sibling)
                    } else {
                        (sibling, chosen)
                    };
                    TypedNode::Split {
                        direction,
                        ratio,
                        first: Box::new(first),
                        second: Box::new(second),
                    }
                }
            };
            Some((Some(remaining), removed))
        }
        TypedNode::Stack {
            id,
            mut selected,
            mut children,
        } => {
            if index >= children.len() {
                return None;
            }
            if rest.is_empty() {
                let removed = remove_child(&mut children, &mut selected, index);
                Some((
                    Some(TypedNode::Stack {
                        id,
                        selected,
                        children,
                    }),
                    removed,
                ))
            } else {
                let child =
                    std::mem::replace(&mut children[index], TypedNode::Leaf(PaneId::from_raw(0)));
                let (remaining, removed) = take_at(child, rest)?;
                match remaining {
                    Some(remaining) => children[index] = remaining,
                    None => {
                        remove_child(&mut children, &mut selected, index);
                    }
                }
                Some((
                    Some(TypedNode::Stack {
                        id,
                        selected,
                        children,
                    }),
                    removed,
                ))
            }
        }
        TypedNode::Leaf(_) | TypedNode::View { .. } => None,
    }
}

// These APIs deliberately have no production interaction entry point yet.
// Mutations are infrequent layout events, O(nodes) on a candidate tree, never PTY/frame work.
#[allow(dead_code)] // New box operations are consumed by the later nested interaction slice.
impl TileLayout {
    pub(crate) fn from_tree(root: TypedNode, focus: LayoutLeaf) -> Option<Self> {
        if !root.valid_structure() {
            return None;
        }
        let mut layout = Self {
            tree_fingerprint: super::collapse::fingerprint(&root),
            root,
            focus,
            prev_focus: None,
        };
        match focus {
            LayoutLeaf::Pane(pane) if layout.placement(pane).is_some() => layout.focus_pane(pane),
            LayoutLeaf::Stack(id) if layout.root.stack(id).is_some() => {}
            _ => return None,
        }
        Some(layout)
    }

    pub(crate) fn new_stack_at(&mut self, path: &[usize], id: TerminalContainerId) -> bool {
        if self.root.stack(id).is_some() {
            return false;
        }
        let mut root = self.root.clone();
        let Some(node) = root.at_mut(path) else {
            return false;
        };
        *node = TypedNode::Stack {
            id,
            selected: 0,
            children: vec![node.clone()],
        };
        self.commit_nested(root, true)
    }

    pub(crate) fn insert_into_stack(
        &mut self,
        id: TerminalContainerId,
        index: usize,
        node: TypedNode,
    ) -> bool {
        let mut root = self.root.clone();
        if !insert(&mut root, id, index, node) {
            return false;
        }
        self.commit_nested(root, true)
    }

    /// Address source by path and destination by stable stack ID. Detach first,
    /// so sibling promotion and index shifts cannot retarget the destination.
    pub(crate) fn move_into_stack(
        &mut self,
        source: &[usize],
        id: TerminalContainerId,
        index: usize,
    ) -> bool {
        if let Some((&from, parent)) = source.split_last() {
            if matches!(self.root.at(parent), Some(TypedNode::Stack { id: own, .. }) if *own == id)
            {
                return self.reorder_stack_child(id, from, index);
            }
        }
        let Some((Some(mut root), node)) = take_at(self.root.clone(), source) else {
            return false;
        };
        if node.stack(id).is_some() || !insert(&mut root, id, index, node) {
            return false;
        }
        self.commit_nested(root, true)
    }

    pub(crate) fn remove_node_at(&mut self, path: &[usize]) -> bool {
        let Some((Some(root), _)) = take_at(self.root.clone(), path) else {
            return false;
        };
        self.commit_nested(root, false)
    }

    pub(crate) fn select_stack_child(&mut self, id: TerminalContainerId, index: usize) -> bool {
        let focused = self.focused();
        let Some(stack) = self.root.stack_mut(id) else {
            return false;
        };
        let contains_focus = stack.contains_pane(focused);
        let TypedNode::Stack {
            selected, children, ..
        } = stack
        else {
            return false;
        };
        if index >= children.len() {
            return false;
        }
        *selected = index;
        let replacement = children[index].selected_pane();
        if contains_focus {
            if let Some(pane) = replacement {
                self.focus_pane(pane);
            } else {
                self.focus = LayoutLeaf::Stack(id);
            }
        }
        self.refresh_tree_fingerprint();
        true
    }

    pub(crate) fn reorder_stack_child(
        &mut self,
        id: TerminalContainerId,
        from: usize,
        to: usize,
    ) -> bool {
        let Some(TypedNode::Stack {
            selected, children, ..
        }) = self.root.stack_mut(id)
        else {
            return false;
        };
        let changed = reorder_child(children, selected, from, to);
        if changed {
            self.refresh_tree_fingerprint();
        }
        changed
    }

    fn commit_nested(&mut self, root: TypedNode, check_depth: bool) -> bool {
        if !root.valid_structure() || (check_depth && !allowed_growth(&self.root, &root)) {
            return false;
        }
        let previous = self.focused();
        let previous_history = self.prev_focus;
        self.root = root;
        // Focus repair remains structural; geometric navigation still uses pane rectangles.
        let leaves = self.leaves();
        if !leaves.contains(&self.focus) {
            // A valid root is a pane, split, or retained (possibly empty) stack,
            // so it always has a spatial target. No fallible step follows commit.
            if let Some(focus) = leaves.first().copied() {
                self.focus = focus;
            }
        }
        if self.placement(previous).is_some() {
            self.focus_pane(previous);
        }
        self.prev_focus = previous_history.filter(|id| self.placement(*id).is_some());
        self.refresh_tree_fingerprint();
        true
    }
}

fn insert(root: &mut TypedNode, id: TerminalContainerId, index: usize, node: TypedNode) -> bool {
    let Some(TypedNode::Stack {
        selected, children, ..
    }) = root.stack_mut(id)
    else {
        return false;
    };
    if index > children.len() {
        return false;
    }
    if !children.is_empty() && index <= *selected {
        *selected += 1;
    }
    children.insert(index, node);
    true
}
