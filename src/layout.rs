//! BSP tree layout for tiling panes within a workspace.

mod nested;

use std::cmp::Reverse;

use crate::terminal_container::TerminalContainerId;
use ratatui::{
    layout::{Direction, Rect},
    widgets::Borders,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct PaneId(u32);

/// Global atomic counter for unique PaneId generation across all workspaces.
static NEXT_PANE_ID: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);

impl PaneId {
    /// Allocate a globally unique PaneId.
    pub fn alloc() -> Self {
        Self(NEXT_PANE_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
    }

    pub fn raw(self) -> u32 {
        self.0
    }

    #[cfg(test)]
    pub(crate) fn next_for_test() -> u32 {
        NEXT_PANE_ID.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Reconstruct from a saved u32 (persistence only).
    pub fn from_raw(id: u32) -> Self {
        Self(id)
    }
}

/// Stable identity for a pane collection. Collection identity is independent
/// from pane identity and from the private slot used by the current BSP tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CollectionId(u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CollectionIdError {
    Invalid,
    Exhausted,
}

impl CollectionId {
    const SERIALIZED_PREFIX: &'static str = "collection_";

    pub(crate) fn alloc() -> Result<Self, CollectionIdError> {
        TerminalContainerId::alloc()
            .map(Into::into)
            .ok_or(CollectionIdError::Exhausted)
    }

    pub fn raw(self) -> u64 {
        self.0
    }

    /// Reconstruct an ID from persisted state while reserving it from allocation.
    pub fn from_raw(id: u64) -> Result<Self, CollectionIdError> {
        TerminalContainerId::from_persisted(id)
            .map(Into::into)
            .ok_or(CollectionIdError::Invalid)
    }

    /// Parse an untrusted public ID without changing allocator state.
    pub fn parse(raw: &str) -> Result<Self, CollectionIdError> {
        let id = raw
            .strip_prefix(Self::SERIALIZED_PREFIX)
            .ok_or(CollectionIdError::Invalid)?
            .parse::<u64>()
            .map_err(|_| CollectionIdError::Invalid)?;
        Self::parse_raw(id)
    }

    fn parse_raw(id: u64) -> Result<Self, CollectionIdError> {
        (id > 0 && id < u64::MAX)
            .then_some(Self(id))
            .ok_or(CollectionIdError::Invalid)
    }
}

impl From<TerminalContainerId> for CollectionId {
    fn from(id: TerminalContainerId) -> Self {
        Self(id.raw())
    }
}

impl From<CollectionId> for TerminalContainerId {
    fn from(id: CollectionId) -> Self {
        // CollectionId construction already validates the raw value.
        TerminalContainerId::from_validated(id.raw())
    }
}

impl serde::Serialize for CollectionId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&format!("{}{id}", Self::SERIALIZED_PREFIX, id = self.0))
    }
}

impl<'de> serde::Deserialize<'de> for CollectionId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = <String as serde::Deserialize>::deserialize(deserializer)?;
        let id =
            Self::parse(&value).map_err(|_| serde::de::Error::custom("invalid collection id"))?;
        Self::from_raw(id.raw()).map_err(|_| serde::de::Error::custom("invalid collection id"))
    }
}

/// Spatial focus target. Stack handles address boxes, never pane leaves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LayoutLeaf {
    Pane(PaneId),
    Stack(TerminalContainerId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanePlacement {
    Tiled,
    Stack(TerminalContainerId),
}

// The neutral container is intentionally private. Its PaneId members are
// legacy placement references, not stable identities for public host calls.

/// Snapshot of a pane's position and focus state after layout.
#[derive(Clone)]
pub struct PaneInfo {
    pub id: PaneId,
    /// Outer rect (including borders if present).
    pub rect: Rect,
    /// Inner rect (content area, excluding borders). Used for selection.
    pub inner_rect: Rect,
    /// Visible scrollbar lane, when scrollback is present. `inner_rect` may still
    /// exclude a stable hidden gutter when this is `None`.
    pub scrollbar_rect: Option<Rect>,
    /// Borders drawn around this pane after UI chrome is applied.
    pub borders: Borders,
    pub is_focused: bool,
}

/// Info about a split boundary, used for mouse drag resize.
#[derive(Clone)]
pub struct SplitBorder {
    /// Position of the divider line (x for horizontal split, y for vertical).
    pub pos: u16,
    /// Direction of the split that created this border.
    pub direction: Direction,
    /// Ratio assigned to the first child of this split.
    pub ratio: f32,
    /// Total area of the split node.
    pub area: Rect,
    /// Path from root to this split node (false=first, true=second).
    pub path: Vec<bool>,
}

/// Cardinal direction for pane navigation.
#[derive(Debug, Clone, Copy)]
pub enum NavDirection {
    Left,
    Right,
    Up,
    Down,
}

/// A node in the BSP tree. Public for serialization.
#[cfg(test)]
#[derive(Clone)]
pub enum Node {
    Pane(PaneId),
    Split {
        direction: Direction,
        ratio: f32,
        first: Box<Node>,
        second: Box<Node>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum TypedNode {
    /// A pane whose content is a terminal (a PTY), addressed by its stable pane ID.
    Leaf(PaneId),
    /// A pane whose content is an extension view. It keeps the same stable pane
    /// identity as a terminal pane and is a first-class pane in every structural
    /// walk; its content identity is the `(extension, view)` pair. The scope of a
    /// view is derived from its placement, never stored here.
    View {
        pane: PaneId,
        extension: String,
        view: String,
    },
    Stack {
        id: TerminalContainerId,
        selected: usize,
        children: Vec<TypedNode>,
    },
    Split {
        direction: Direction,
        ratio: f32,
        first: Box<TypedNode>,
        second: Box<TypedNode>,
    },
}

/// The content identity of a view pane: the extension that publishes it and the
/// view it publishes. Its scope is derived from placement and never stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ViewRef {
    pub(crate) pane: PaneId,
    pub(crate) extension: String,
    pub(crate) view: String,
}

impl ViewRef {
    /// Bounds mirror the host view contract's local-id and namespace rules.
    pub(crate) fn validate(&self) -> bool {
        fn ok(value: &str, max: usize) -> bool {
            !value.is_empty()
                && value.len() <= max
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        }
        ok(&self.extension, 120) && ok(&self.view, 64)
    }
}

/// Borrowed structural projection used by the legacy Collection boundary.
/// Membership and selection are read directly from the canonical stack.
#[derive(Clone, Copy)]
pub(crate) struct StackRef<'a> {
    pub(crate) id: TerminalContainerId,
    selected: usize,
    children: &'a [TypedNode],
}

impl StackRef<'_> {
    pub(crate) fn members(&self) -> Vec<PaneId> {
        let mut panes = Vec::new();
        for child in self.children {
            child.collect_all_panes(&mut panes);
        }
        panes
    }

    /// The terminals this box holds. View panes are excluded: the host terminal
    /// contract describes terminals, and a view has no terminal identity.
    pub(crate) fn terminal_members(&self) -> Vec<PaneId> {
        let mut panes = Vec::new();
        for child in self.children {
            child.collect_terminal_panes(&mut panes);
        }
        panes
    }

    pub(crate) fn selected(&self) -> Option<PaneId> {
        self.children.get(self.selected)?.selected_pane()
    }

    pub(crate) fn member_count(&self) -> usize {
        self.children.iter().map(TypedNode::pane_count).sum()
    }

    pub(crate) fn contains_member(&self, pane: PaneId) -> bool {
        self.children.iter().any(|child| child.contains_pane(pane))
    }
}

/// Canonical binary split/stack tree. Terminal execution stays in its registry.
#[derive(Clone)]
pub struct TileLayout {
    root: TypedNode,
    focus: LayoutLeaf,
    /// Pane focused before the current leaf/member. Targeted tree edits do not
    /// disturb this history, so closing a newly focused pane can return there.
    prev_focus: Option<PaneId>,
}

impl TileLayout {
    pub fn new() -> (Self, PaneId) {
        let root_id = PaneId::alloc();
        (
            Self {
                root: TypedNode::Leaf(root_id),
                focus: LayoutLeaf::Pane(root_id),
                prev_focus: None,
            },
            root_id,
        )
    }

    /// Compatibility pane focus. A focused non-empty collection resolves to
    /// its selected member; an empty collection falls back to another placed pane.
    pub fn focused(&self) -> PaneId {
        match self.focus {
            LayoutLeaf::Pane(pane_id) => pane_id,
            LayoutLeaf::Stack(collection_id) => self
                .collection(collection_id)
                .and_then(|stack| stack.selected())
                .or_else(|| self.pane_ids().into_iter().next())
                .unwrap_or_else(|| PaneId::from_raw(0)),
        }
    }

    pub fn focused_leaf(&self) -> LayoutLeaf {
        self.focus
    }

    /// All placed panes, including collection members.
    pub fn pane_count(&self) -> usize {
        self.root.pane_count()
    }

    /// Scalar visibility check for source dirtiness; never allocates or visits hidden children.
    /// Test a scalar fact over visible leaves only, without collecting geometry
    /// or visiting hidden stack members in pane-scaled presentation paths.
    pub(crate) fn any_visible_pane(&self, mut predicate: impl FnMut(PaneId) -> bool) -> bool {
        fn any(node: &TypedNode, predicate: &mut impl FnMut(PaneId) -> bool) -> bool {
            match node {
                TypedNode::Leaf(pane) | TypedNode::View { pane, .. } => predicate(*pane),
                TypedNode::Split { first, second, .. } => {
                    any(first, predicate) || any(second, predicate)
                }
                TypedNode::Stack {
                    selected, children, ..
                } => children
                    .get(*selected)
                    .is_some_and(|child| any(child, predicate)),
            }
        }
        any(&self.root, &mut predicate)
    }

    pub(crate) fn is_pane_visible(&self, pane: PaneId) -> bool {
        fn visible(node: &TypedNode, pane: PaneId) -> bool {
            match node {
                TypedNode::Leaf(id) => *id == pane,
                TypedNode::View {
                    pane: view_pane, ..
                } => *view_pane == pane,
                TypedNode::Split { first, second, .. } => {
                    visible(first, pane) || visible(second, pane)
                }
                TypedNode::Stack {
                    selected, children, ..
                } => children
                    .get(*selected)
                    .is_some_and(|child| visible(child, pane)),
            }
        }
        visible(&self.root, pane)
    }

    /// Ephemeral geometry projection, computed once before pane-scaled loops.
    /// Only selected stack branches are visited; no membership cache is retained.
    pub(crate) fn selected_stack_panes(&self) -> Vec<PaneId> {
        fn collect(node: &TypedNode, panes: &mut Vec<PaneId>) {
            match node {
                TypedNode::Leaf(_) | TypedNode::View { .. } => {}
                TypedNode::Split { first, second, .. } => {
                    collect(first, panes);
                    collect(second, panes);
                }
                TypedNode::Stack {
                    selected, children, ..
                } => {
                    if let Some(child) = children.get(*selected) {
                        if let Some(pane) = child.selected_pane() {
                            panes.push(pane);
                        }
                        collect(child, panes);
                    }
                }
            }
        }
        let mut panes = Vec::new();
        collect(&self.root, &mut panes);
        panes
    }

    #[cfg(test)]
    pub fn tiled_pane_count(&self) -> usize {
        self.tiled_pane_ids().len()
    }

    pub fn leaf_count(&self) -> usize {
        typed_count(&self.root)
    }

    pub fn leaves(&self) -> Vec<LayoutLeaf> {
        let mut leaves = Vec::new();
        typed_collect_leaves(&self.root, &mut leaves);
        leaves
    }

    pub(crate) fn collection_ids(&self) -> Vec<TerminalContainerId> {
        let mut ids = Vec::new();
        self.root.collect_stack_ids(&mut ids);
        ids
    }

    pub(crate) fn collection(&self, id: TerminalContainerId) -> Option<StackRef<'_>> {
        let TypedNode::Stack {
            id,
            selected,
            children,
        } = self.root.stack(id)?
        else {
            return None;
        };
        Some(StackRef {
            id: *id,
            selected: *selected,
            children,
        })
    }

    pub(crate) fn collections(&self) -> impl Iterator<Item = StackRef<'_>> {
        fn collect<'a>(node: &'a TypedNode, stacks: &mut Vec<StackRef<'a>>) {
            match node {
                TypedNode::Leaf(_) | TypedNode::View { .. } => {}
                TypedNode::Split { first, second, .. } => {
                    collect(first, stacks);
                    collect(second, stacks);
                }
                TypedNode::Stack {
                    id,
                    selected,
                    children,
                } => {
                    stacks.push(StackRef {
                        id: *id,
                        selected: *selected,
                        children,
                    });
                    for child in children {
                        collect(child, stacks);
                    }
                }
            }
        }
        let mut stacks = Vec::new();
        collect(&self.root, &mut stacks);
        stacks.into_iter()
    }

    #[cfg(test)]
    pub(crate) fn replace_typed_root_for_test(&mut self, root: TypedNode) {
        self.root = root;
    }

    pub fn placement(&self, pane_id: PaneId) -> Option<PanePlacement> {
        fn find(
            node: &TypedNode,
            pane: PaneId,
            stack: Option<TerminalContainerId>,
        ) -> Option<PanePlacement> {
            match node {
                TypedNode::Leaf(id) => (*id == pane).then_some(
                    stack
                        .map(PanePlacement::Stack)
                        .unwrap_or(PanePlacement::Tiled),
                ),
                TypedNode::View {
                    pane: view_pane, ..
                } => (*view_pane == pane).then_some(
                    stack
                        .map(PanePlacement::Stack)
                        .unwrap_or(PanePlacement::Tiled),
                ),
                TypedNode::Split { first, second, .. } => {
                    find(first, pane, stack).or_else(|| find(second, pane, stack))
                }
                TypedNode::Stack { id, children, .. } => children
                    .iter()
                    .find_map(|child| find(child, pane, stack.or(Some(*id)))),
            }
        }
        find(&self.root, pane_id, None)
    }

    pub fn panes(&self, area: Rect) -> Vec<PaneInfo> {
        let mut result = Vec::new();
        typed_collect_panes(&self.root, area, self.focus, &mut result);
        result
    }

    pub(crate) fn leaf_rect(&self, leaf: LayoutLeaf, area: Rect) -> Option<Rect> {
        typed_leaf_rect(&self.root, leaf, area)
    }

    pub fn splits(&self, area: Rect) -> Vec<SplitBorder> {
        let mut result = Vec::new();
        typed_collect_splits(&self.root, area, &mut Vec::new(), &mut result);
        result
    }

    #[cfg(test)]
    pub fn split_focused(&mut self, direction: Direction) -> PaneId {
        self.split_focused_with_ratio(direction, 0.5)
    }

    #[cfg(test)]
    pub fn split_focused_with_ratio(&mut self, direction: Direction, ratio: f32) -> PaneId {
        let target = match self.focused_leaf() {
            LayoutLeaf::Pane(pane_id) => pane_id,
            LayoutLeaf::Stack(_) => self
                .prev_focus
                .filter(|pane_id| matches!(self.placement(*pane_id), Some(PanePlacement::Tiled)))
                .or_else(|| self.tiled_pane_ids().into_iter().next())
                .expect("layout has a tiled pane"),
        };
        let new_id = self
            .split_pane(target, direction, ratio)
            .expect("target pane is in the tiled layout");
        self.focus_pane(new_id);
        new_id
    }

    /// Split a tiled target without moving focus. Collection members do not
    /// occupy BSP leaves and therefore cannot be split directly.
    pub fn split_pane(
        &mut self,
        target: PaneId,
        direction: Direction,
        ratio: f32,
    ) -> Option<PaneId> {
        let target_leaf = match self.placement(target)? {
            PanePlacement::Tiled => LayoutLeaf::Pane(target),
            PanePlacement::Stack(collection_id) => LayoutLeaf::Stack(collection_id),
        };
        let new_id = PaneId::alloc();
        // Split the target leaf in place: rebuilding from the root recreated
        // every node on the path and on every split of a deep layout.
        typed_split_leaf_in_place(
            &mut self.root,
            target_leaf,
            direction,
            TypedNode::Leaf(new_id),
            valid_split_ratio(ratio),
        )
        .then_some(new_id)
    }

    /// Split `target` with a view pane. The view pane carries its own content
    /// identity and has no terminal or runtime; it is a pane like any other for
    /// layout, frame, focus and close. Returns the new pane identity.
    pub(crate) fn split_pane_with_view(
        &mut self,
        target: PaneId,
        direction: Direction,
        ratio: f32,
        extension: &str,
        view: &str,
    ) -> Option<(PaneId, ViewRef)> {
        let target_leaf = match self.placement(target)? {
            PanePlacement::Tiled => LayoutLeaf::Pane(target),
            PanePlacement::Stack(collection_id) => LayoutLeaf::Stack(collection_id),
        };
        let new_id = PaneId::alloc();
        let content = ViewRef {
            pane: new_id,
            extension: extension.to_owned(),
            view: view.to_owned(),
        };
        if !content.validate() {
            return None;
        }
        typed_split_leaf_in_place(
            &mut self.root,
            target_leaf,
            direction,
            TypedNode::view(new_id, &content.extension, &content.view),
            valid_split_ratio(ratio),
        )
        .then_some((new_id, content))
    }

    /// The content identity of a placed view pane.
    pub(crate) fn view_at(&self, pane: PaneId) -> Option<ViewRef> {
        fn find(node: &TypedNode, pane: PaneId) -> Option<ViewRef> {
            match node {
                TypedNode::View { .. } => node.view_ref().filter(|view| view.pane == pane),
                TypedNode::Leaf(_) => None,
                TypedNode::Split { first, second, .. } => {
                    find(first, pane).or_else(|| find(second, pane))
                }
                TypedNode::Stack { children, .. } => {
                    // Every child, not just the selected one: placement, not
                    // visibility, decides where a pane is, and `view_panes` and
                    // `view_scope` walk all children too.
                    children.iter().find_map(|child| find(child, pane))
                }
            }
        }
        find(&self.root, pane)
    }

    /// The terminal panes a placed view pane covers, derived from its placement.
    pub(crate) fn view_scope(&self, view_pane: PaneId) -> Vec<PaneId> {
        self.root.view_scope(view_pane)
    }

    /// Every placed view pane, in tree order.
    pub(crate) fn view_panes(&self) -> Vec<PaneId> {
        fn collect(node: &TypedNode, out: &mut Vec<PaneId>) {
            match node {
                TypedNode::View { pane, .. } => out.push(*pane),
                TypedNode::Leaf(_) => {}
                TypedNode::Split { first, second, .. } => {
                    collect(first, out);
                    collect(second, out);
                }
                TypedNode::Stack { children, .. } => {
                    for child in children {
                        collect(child, out);
                    }
                }
            }
        }
        let mut panes = Vec::new();
        collect(&self.root, &mut panes);
        panes
    }

    pub fn insert_pane_near(
        &mut self,
        target: PaneId,
        moved: PaneId,
        direction: Direction,
        ratio: f32,
        focus: bool,
    ) -> bool {
        if target == moved
            || self.placement(target) != Some(PanePlacement::Tiled)
            || self.placement(moved).is_some()
        {
            return false;
        }
        if !typed_split_leaf_in_place(
            &mut self.root,
            LayoutLeaf::Pane(target),
            direction,
            TypedNode::Leaf(moved),
            valid_split_ratio(ratio),
        ) {
            return false;
        }
        if focus {
            self.focus_pane(moved);
        }
        true
    }

    pub(crate) fn can_insert_collection_near(&self, target: LayoutLeaf) -> bool {
        self.leaves().contains(&target)
    }

    pub(crate) fn insert_collection_near(
        &mut self,
        target: LayoutLeaf,
        collection_id: TerminalContainerId,
        direction: Direction,
        ratio: f32,
    ) -> bool {
        if self.root.stack(collection_id).is_some() || !self.can_insert_collection_near(target) {
            return false;
        }
        typed_split_leaf_in_place(
            &mut self.root,
            target,
            direction,
            TypedNode::Stack {
                id: collection_id,
                selected: 0,
                children: Vec::new(),
            },
            valid_split_ratio(ratio),
        )
    }

    pub fn focus_leaf(&mut self, leaf: LayoutLeaf) -> bool {
        if !self.leaves().contains(&leaf) {
            return false;
        }
        if self.focus != leaf {
            self.prev_focus = Some(self.focused());
            self.focus = leaf;
        }
        true
    }

    pub(crate) fn remove_collection(&mut self, collection_id: TerminalContainerId) -> bool {
        if self.leaf_count() <= 1 || !self.leaves().contains(&LayoutLeaf::Stack(collection_id)) {
            return false;
        }
        if !self.remove_leaf(LayoutLeaf::Stack(collection_id)) {
            return false;
        }
        true
    }

    pub(crate) fn add_collection_member(&mut self, id: TerminalContainerId, pane: PaneId) -> bool {
        if self.placement(pane).is_some() {
            return false;
        }
        let Some(TypedNode::Stack { children, .. }) = self.root.stack_mut(id) else {
            return false;
        };
        children.push(TypedNode::Leaf(pane));
        true
    }

    pub(crate) fn remove_collection_member(
        &mut self,
        id: TerminalContainerId,
        pane: PaneId,
    ) -> bool {
        let Some(stack) = self.root.stack_mut(id) else {
            return false;
        };
        if !stack.contains_pane(pane) {
            return false;
        }
        let Some(root) = typed_remove(stack.clone(), LayoutLeaf::Pane(pane)) else {
            return false;
        };
        *stack = root;
        true
    }

    pub(crate) fn select_collection_member(
        &mut self,
        id: TerminalContainerId,
        pane: PaneId,
    ) -> bool {
        let Some(stack) = self.root.stack_mut(id) else {
            return false;
        };
        stack.select_pane(pane)
    }

    pub(crate) fn reorder_collection_member(
        &mut self,
        id: TerminalContainerId,
        pane: PaneId,
        index: usize,
    ) -> bool {
        let Some(TypedNode::Stack {
            children, selected, ..
        }) = self.root.stack_mut(id)
        else {
            return false;
        };
        let Some(from) = children
            .iter()
            .position(|child| child.pane_leaf() == Some(pane))
        else {
            return false;
        };
        nested::reorder_child(children, selected, from, index)
    }

    // Retained as the pure layout primitive exercised by collection invariant tests.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn remove_tiled_pane_for_collection(&mut self, pane: PaneId) -> bool {
        self.placement(pane) == Some(PanePlacement::Tiled)
            && self.remove_leaf(LayoutLeaf::Pane(pane))
    }

    pub fn close_focused(&mut self) -> bool {
        let LayoutLeaf::Pane(pane) = self.focus else {
            return false;
        };
        let previous = self
            .prev_focus
            .filter(|previous| *previous != pane && self.placement(*previous).is_some());
        if !self.remove_leaf(LayoutLeaf::Pane(pane)) {
            return false;
        }
        if let Some(previous) = previous {
            self.focus_pane(previous);
        }
        self.prev_focus = None;
        true
    }

    /// Close any placed pane without changing focus unless it is focused.
    pub fn close_pane(&mut self, id: PaneId) -> bool {
        match self.placement(id) {
            Some(PanePlacement::Tiled) if self.focus == LayoutLeaf::Pane(id) => {
                self.close_focused()
            }
            Some(PanePlacement::Tiled) => {
                let removed = self.remove_leaf(LayoutLeaf::Pane(id));
                if removed && self.prev_focus == Some(id) {
                    self.prev_focus = None;
                }
                removed
            }
            Some(PanePlacement::Stack(collection_id)) => {
                let removed = self.remove_collection_member(collection_id, id);
                if removed && self.prev_focus == Some(id) {
                    self.prev_focus = None;
                }
                removed
            }
            None => false,
        }
    }

    pub fn focus_pane(&mut self, id: PaneId) {
        let previous = self.focused();
        let focused = match self.placement(id) {
            Some(PanePlacement::Tiled) => {
                self.focus = LayoutLeaf::Pane(id);
                true
            }
            Some(PanePlacement::Stack(collection_id)) => {
                if !self.select_collection_member(collection_id, id) {
                    return;
                }
                self.focus = match self.root.stack(collection_id) {
                    Some(TypedNode::Stack {
                        selected, children, ..
                    }) if children
                        .get(*selected)
                        .is_some_and(|child| child.pane_leaf().is_some()) =>
                    {
                        LayoutLeaf::Stack(collection_id)
                    }
                    _ => LayoutLeaf::Pane(id),
                };
                true
            }
            None => false,
        };
        if focused && previous != id {
            self.prev_focus = Some(previous);
        }
    }

    pub fn swap_panes(&mut self, first: PaneId, second: PaneId) -> bool {
        if first == second
            || self.placement(first) != Some(PanePlacement::Tiled)
            || self.placement(second) != Some(PanePlacement::Tiled)
        {
            return false;
        }
        typed_swap_panes(&mut self.root, first, second);
        true
    }

    pub fn set_ratio_at(&mut self, path: &[bool], ratio: f32) -> bool {
        typed_set_ratio(&mut self.root, path, ratio.clamp(0.1, 0.9))
    }

    pub fn resize_focused(&mut self, nav: NavDirection, delta: f32, area: Rect) {
        let Some(focused_rect) = typed_leaf_rect(&self.root, self.focus, area) else {
            return;
        };
        let target_dir = match nav {
            NavDirection::Left | NavDirection::Right => Direction::Horizontal,
            NavDirection::Up | NavDirection::Down => Direction::Vertical,
        };
        let grows = matches!(nav, NavDirection::Right | NavDirection::Down);
        let splits = self.splits(area);
        let best = nearest_resize_split(&splits, target_dir, focused_rect, nav).or_else(|| {
            nearest_resize_split(&splits, target_dir, focused_rect, opposite_direction(nav))
        });
        if let Some(split) = best {
            let current = typed_get_ratio(&self.root, &split.path).unwrap_or(0.5);
            let adjustment = if grows { delta } else { -delta };
            self.set_ratio_at(&split.path, current + adjustment);
        }
    }

    pub fn resize_pane(
        &mut self,
        pane_id: PaneId,
        nav: NavDirection,
        delta: f32,
        area: Rect,
    ) -> bool {
        let Some(placement) = self.placement(pane_id) else {
            return false;
        };
        let target = match placement {
            PanePlacement::Tiled => LayoutLeaf::Pane(pane_id),
            PanePlacement::Stack(collection_id) => LayoutLeaf::Stack(collection_id),
        };
        let before = typed_split_ratios(&self.root);
        let previous = self.focus;
        self.focus = target;
        self.resize_focused(nav, delta, area);
        self.focus = previous;
        typed_split_ratios(&self.root) != before
    }

    /// All placed panes in deterministic top-level/member order.
    pub fn pane_ids(&self) -> Vec<PaneId> {
        self.placed_pane_ids()
    }

    /// Panes that occupy ordinary top-level BSP leaves.
    #[cfg(test)]
    pub fn tiled_pane_ids(&self) -> Vec<PaneId> {
        self.leaves()
            .into_iter()
            .filter_map(|leaf| match leaf {
                LayoutLeaf::Pane(id) => Some(id),
                LayoutLeaf::Stack(_) => None,
            })
            .collect()
    }

    pub fn placed_pane_ids(&self) -> Vec<PaneId> {
        let mut panes = Vec::new();
        self.root.collect_all_panes(&mut panes);
        panes
    }

    pub(crate) fn typed_root(&self) -> &TypedNode {
        &self.root
    }

    pub(crate) fn for_pane(pane: PaneId) -> Self {
        Self {
            root: TypedNode::Leaf(pane),
            focus: LayoutLeaf::Pane(pane),
            prev_focus: None,
        }
    }

    #[cfg(test)]
    pub fn from_saved(root: Node, focus: PaneId) -> Self {
        Self {
            root: typed_from_legacy(&root),
            focus: LayoutLeaf::Pane(focus),
            prev_focus: None,
        }
    }

    fn remove_leaf(&mut self, target: LayoutLeaf) -> bool {
        if self.leaf_count() <= 1 {
            return false;
        }
        let leaves = self.leaves();
        let Some(position) = leaves.iter().position(|leaf| *leaf == target) else {
            return false;
        };
        let replacement = leaves
            .get(position + 1)
            .or_else(|| position.checked_sub(1).and_then(|i| leaves.get(i)))
            .copied();
        let old = std::mem::replace(&mut self.root, TypedNode::Leaf(PaneId::from_raw(0)));
        let Some(root) = typed_remove(old, target) else {
            return false;
        };
        self.root = root;
        if self.focus == target {
            if let Some(replacement) = replacement {
                self.focus = replacement;
            }
        }
        true
    }
}

#[cfg(test)]
fn typed_from_legacy(node: &Node) -> TypedNode {
    match node {
        Node::Pane(id) => TypedNode::Leaf(*id),
        Node::Split {
            direction,
            ratio,
            first,
            second,
        } => TypedNode::Split {
            direction: *direction,
            ratio: *ratio,
            first: Box::new(typed_from_legacy(first)),
            second: Box::new(typed_from_legacy(second)),
        },
    }
}

fn typed_count(node: &TypedNode) -> usize {
    match node {
        TypedNode::Leaf(_) | TypedNode::View { .. } | TypedNode::Stack { .. } => 1,
        TypedNode::Split { first, second, .. } => typed_count(first) + typed_count(second),
    }
}

fn typed_collect_leaves(node: &TypedNode, result: &mut Vec<LayoutLeaf>) {
    match node {
        TypedNode::Leaf(pane) => result.push(LayoutLeaf::Pane(*pane)),
        TypedNode::View { pane, .. } => result.push(LayoutLeaf::Pane(*pane)),
        TypedNode::Stack { id, .. } => result.push(LayoutLeaf::Stack(*id)),
        TypedNode::Split { first, second, .. } => {
            typed_collect_leaves(first, result);
            typed_collect_leaves(second, result);
        }
    }
}

fn typed_leaf_rect(node: &TypedNode, target: LayoutLeaf, area: Rect) -> Option<Rect> {
    match node {
        TypedNode::Leaf(pane) => (LayoutLeaf::Pane(*pane) == target).then_some(area),
        TypedNode::View { pane, .. } => (LayoutLeaf::Pane(*pane) == target).then_some(area),
        TypedNode::Stack {
            id,
            selected,
            children,
        } => {
            if target == LayoutLeaf::Stack(*id) {
                Some(area)
            } else {
                children
                    .get(*selected)
                    .and_then(|child| typed_leaf_rect(child, target, area))
            }
        }
        TypedNode::Split {
            direction,
            ratio,
            first,
            second,
        } => {
            let (first_area, second_area) = split_rect(area, *direction, *ratio);
            typed_leaf_rect(first, target, first_area)
                .or_else(|| typed_leaf_rect(second, target, second_area))
        }
    }
}

fn typed_collect_panes(
    node: &TypedNode,
    area: Rect,
    focus: LayoutLeaf,
    result: &mut Vec<PaneInfo>,
) {
    let pane = match node {
        TypedNode::Leaf(id) => Some((*id, focus == LayoutLeaf::Pane(*id))),
        TypedNode::View { pane, .. } => Some((*pane, focus == LayoutLeaf::Pane(*pane))),
        TypedNode::Stack {
            id,
            selected,
            children,
        } => {
            if let Some(child) = children.get(*selected) {
                let focus = if focus == LayoutLeaf::Stack(*id) {
                    child.selected_pane().map(LayoutLeaf::Pane).unwrap_or(focus)
                } else {
                    focus
                };
                typed_collect_panes(child, area, focus, result);
            }
            None
        }
        TypedNode::Split {
            direction,
            ratio,
            first,
            second,
        } => {
            let (first_area, second_area) = split_rect(area, *direction, *ratio);
            typed_collect_panes(first, first_area, focus, result);
            typed_collect_panes(second, second_area, focus, result);
            None
        }
    };
    if let Some((id, is_focused)) = pane {
        result.push(PaneInfo {
            id,
            rect: area,
            inner_rect: area,
            scrollbar_rect: None,
            borders: Borders::NONE,
            is_focused,
        });
    }
}

fn typed_collect_splits(
    node: &TypedNode,
    area: Rect,
    path: &mut Vec<bool>,
    result: &mut Vec<SplitBorder>,
) {
    if let TypedNode::Split {
        direction,
        ratio,
        first,
        second,
    } = node
    {
        let (first_area, second_area) = split_rect(area, *direction, *ratio);
        let pos = match direction {
            Direction::Horizontal => first_area.x + first_area.width,
            Direction::Vertical => first_area.y + first_area.height,
        };
        result.push(SplitBorder {
            pos,
            direction: *direction,
            ratio: *ratio,
            area,
            path: path.clone(),
        });
        path.push(false);
        typed_collect_splits(first, first_area, path, result);
        path.pop();
        path.push(true);
        typed_collect_splits(second, second_area, path, result);
        path.pop();
    }
}

/// Replaces the target leaf with a two-child split, leaving every other node in
/// place. Returns whether the target was found.
fn typed_split_leaf_in_place(
    node: &mut TypedNode,
    target: LayoutLeaf,
    direction: Direction,
    added: TypedNode,
    ratio: f32,
) -> bool {
    fn insert(
        node: &mut TypedNode,
        target: LayoutLeaf,
        direction: Direction,
        added: &mut Option<TypedNode>,
        ratio: f32,
    ) -> bool {
        let identity = match &*node {
            TypedNode::Leaf(pane) | TypedNode::View { pane, .. } => Some(LayoutLeaf::Pane(*pane)),
            TypedNode::Stack { id, .. } => Some(LayoutLeaf::Stack(*id)),
            TypedNode::Split { .. } => None,
        };
        if identity == Some(target) {
            let Some(added) = added.take() else {
                return false;
            };
            let existing = std::mem::replace(node, TypedNode::Leaf(PaneId::from_raw(0)));
            *node = TypedNode::Split {
                direction,
                ratio,
                first: Box::new(existing),
                second: Box::new(added),
            };
            return true;
        }
        match node {
            TypedNode::Split { first, second, .. } => {
                insert(first, target, direction, added, ratio)
                    || insert(second, target, direction, added, ratio)
            }
            TypedNode::Leaf(_) | TypedNode::View { .. } | TypedNode::Stack { .. } => false,
        }
    }
    insert(node, target, direction, &mut Some(added), ratio)
}

fn typed_remove(node: TypedNode, target: LayoutLeaf) -> Option<TypedNode> {
    match node {
        TypedNode::Leaf(pane) if LayoutLeaf::Pane(pane) == target => None,
        TypedNode::View { pane, .. } if LayoutLeaf::Pane(pane) == target => None,
        TypedNode::Stack { id, .. } if target == LayoutLeaf::Stack(id) => None,
        TypedNode::Leaf(_) | TypedNode::View { .. } => Some(node),
        TypedNode::Stack {
            id,
            mut selected,
            mut children,
        } => {
            let mut index = 0;
            while index < children.len() {
                if let Some(child) = typed_remove(children[index].clone(), target) {
                    children[index] = child;
                    index += 1;
                } else {
                    nested::remove_child(&mut children, &mut selected, index);
                }
            }
            Some(TypedNode::Stack {
                id,
                selected,
                children,
            })
        }
        TypedNode::Split {
            direction,
            ratio,
            first,
            second,
        } => match (typed_remove(*first, target), typed_remove(*second, target)) {
            (None, Some(node)) | (Some(node), None) => Some(node),
            (Some(first), Some(second)) => Some(TypedNode::Split {
                direction,
                ratio,
                first: Box::new(first),
                second: Box::new(second),
            }),
            (None, None) => None,
        },
    }
}

fn typed_swap_panes(node: &mut TypedNode, first: PaneId, second: PaneId) {
    // Address both nodes by path before touching the tree: after the first
    // replacement two nodes can carry the same pane id, so a second lookup by id
    // would find the wrong one.
    let Some(first_path) = leaf_path(node, first) else {
        return;
    };
    let Some(second_path) = leaf_path(node, second) else {
        return;
    };
    let (Some(first_node), Some(second_node)) = (
        node.at(&first_path).cloned(),
        node.at(&second_path).cloned(),
    ) else {
        return;
    };
    // Swap the whole content-bearing leaves, so the view keeps its
    // (extension, view) and the terminal keeps its terminal identity. Swapping
    // only the numeric pane ids turns the terminal into a view and the view into
    // a terminal-shaped leaf with no terminal record.
    if let Some(slot) = node.at_mut(&first_path) {
        *slot = second_node;
    }
    if let Some(slot) = node.at_mut(&second_path) {
        *slot = first_node;
    }
}

fn leaf_path(node: &TypedNode, pane: PaneId) -> Option<Vec<usize>> {
    match node {
        TypedNode::Leaf(id) if *id == pane => Some(Vec::new()),
        TypedNode::View {
            pane: view_pane, ..
        } if *view_pane == pane => Some(Vec::new()),
        TypedNode::Split { first, second, .. } => {
            if let Some(mut path) = leaf_path(first, pane) {
                path.insert(0, 0);
                Some(path)
            } else {
                leaf_path(second, pane).map(|mut path| {
                    path.insert(0, 1);
                    path
                })
            }
        }
        TypedNode::Stack { children, .. } => {
            children.iter().enumerate().find_map(|(index, child)| {
                leaf_path(child, pane).map(|mut path| {
                    path.insert(0, index);
                    path
                })
            })
        }
        _ => None,
    }
}

fn typed_split_ratios(node: &TypedNode) -> Vec<(Vec<bool>, f32)> {
    fn collect(node: &TypedNode, path: &mut Vec<bool>, result: &mut Vec<(Vec<bool>, f32)>) {
        match node {
            TypedNode::Leaf(_) | TypedNode::View { .. } | TypedNode::Stack { .. } => {}
            TypedNode::Split {
                ratio,
                first,
                second,
                ..
            } => {
                result.push((path.clone(), *ratio));
                path.push(false);
                collect(first, path, result);
                path.pop();
                path.push(true);
                collect(second, path, result);
                path.pop();
            }
        }
    }
    let mut result = Vec::new();
    collect(node, &mut Vec::new(), &mut result);
    result
}

fn typed_set_ratio(node: &mut TypedNode, path: &[bool], ratio: f32) -> bool {
    if let TypedNode::Split {
        first,
        second,
        ratio: current,
        ..
    } = node
    {
        if path.is_empty() {
            *current = ratio;
            true
        } else if path[0] {
            typed_set_ratio(second, &path[1..], ratio)
        } else {
            typed_set_ratio(first, &path[1..], ratio)
        }
    } else {
        false
    }
}

fn typed_get_ratio(node: &TypedNode, path: &[bool]) -> Option<f32> {
    if let TypedNode::Split {
        first,
        second,
        ratio,
        ..
    } = node
    {
        if path.is_empty() {
            Some(*ratio)
        } else if path[0] {
            typed_get_ratio(second, &path[1..])
        } else {
            typed_get_ratio(first, &path[1..])
        }
    } else {
        None
    }
}

// --- Directional pane navigation ---

/// Find the nearest pane in the given direction from `focused`.
pub fn find_in_direction(
    focused: &PaneInfo,
    direction: NavDirection,
    panes: &[PaneInfo],
) -> Option<PaneId> {
    let fr = focused.rect;

    panes
        .iter()
        .enumerate()
        .filter(|(_, p)| p.id != focused.id)
        .filter(|(_, p)| {
            let r = p.rect;
            match direction {
                NavDirection::Left => {
                    r.x + r.width <= fr.x && ranges_overlap(r.y, r.height, fr.y, fr.height)
                }
                NavDirection::Right => {
                    r.x >= fr.x + fr.width && ranges_overlap(r.y, r.height, fr.y, fr.height)
                }
                NavDirection::Up => {
                    r.y + r.height <= fr.y && ranges_overlap(r.x, r.width, fr.x, fr.width)
                }
                NavDirection::Down => {
                    r.y >= fr.y + fr.height && ranges_overlap(r.x, r.width, fr.x, fr.width)
                }
            }
        })
        .min_by_key(|(index, p)| {
            let r = p.rect;
            let edge_distance = match direction {
                NavDirection::Left => fr.x.saturating_sub(r.x + r.width),
                NavDirection::Right => r.x.saturating_sub(fr.x + fr.width),
                NavDirection::Up => fr.y.saturating_sub(r.y + r.height),
                NavDirection::Down => r.y.saturating_sub(fr.y + fr.height),
            };
            let overlap = match direction {
                NavDirection::Left | NavDirection::Right => {
                    range_overlap_amount(r.y, r.height, fr.y, fr.height)
                }
                NavDirection::Up | NavDirection::Down => {
                    range_overlap_amount(r.x, r.width, fr.x, fr.width)
                }
            };
            let center_distance = match direction {
                NavDirection::Left | NavDirection::Right => {
                    range_center_distance(r.y, r.height, fr.y, fr.height)
                }
                NavDirection::Up | NavDirection::Down => {
                    range_center_distance(r.x, r.width, fr.x, fr.width)
                }
            };
            (edge_distance, Reverse(overlap), center_distance, *index)
        })
        .map(|(_, p)| p.id)
}

fn ranges_overlap(a_start: u16, a_len: u16, b_start: u16, b_len: u16) -> bool {
    a_start < b_start + b_len && a_start + a_len > b_start
}

fn split_on_requested_edge(split: &SplitBorder, focused: Rect, nav: NavDirection) -> bool {
    split_edge_distance(split, focused, nav) <= 1
}

fn split_area_overlaps_focused_pane(split: &SplitBorder, focused: Rect, nav: NavDirection) -> bool {
    match nav {
        NavDirection::Left | NavDirection::Right => {
            ranges_overlap(split.area.y, split.area.height, focused.y, focused.height)
        }
        NavDirection::Up | NavDirection::Down => {
            ranges_overlap(split.area.x, split.area.width, focused.x, focused.width)
        }
    }
}

fn nearest_resize_split(
    splits: &[SplitBorder],
    target_dir: Direction,
    focused: Rect,
    nav: NavDirection,
) -> Option<&SplitBorder> {
    splits
        .iter()
        .filter(|s| s.direction == target_dir)
        .filter(|s| split_area_overlaps_focused_pane(s, focused, nav))
        .filter(|s| split_on_requested_edge(s, focused, nav))
        .min_by_key(|s| split_edge_distance(s, focused, nav))
}

fn opposite_direction(nav: NavDirection) -> NavDirection {
    match nav {
        NavDirection::Left => NavDirection::Right,
        NavDirection::Right => NavDirection::Left,
        NavDirection::Up => NavDirection::Down,
        NavDirection::Down => NavDirection::Up,
    }
}

fn split_edge_distance(split: &SplitBorder, focused: Rect, nav: NavDirection) -> u32 {
    match nav {
        NavDirection::Left => (split.pos as i32 - focused.x as i32).unsigned_abs(),
        NavDirection::Right => {
            (split.pos as i32 - (focused.x + focused.width) as i32).unsigned_abs()
        }
        NavDirection::Up => (split.pos as i32 - focused.y as i32).unsigned_abs(),
        NavDirection::Down => {
            (split.pos as i32 - (focused.y + focused.height) as i32).unsigned_abs()
        }
    }
}

fn range_overlap_amount(a_start: u16, a_len: u16, b_start: u16, b_len: u16) -> u16 {
    let a_end = a_start.saturating_add(a_len);
    let b_end = b_start.saturating_add(b_len);
    a_end.min(b_end).saturating_sub(a_start.max(b_start))
}

fn range_center_distance(a_start: u16, a_len: u16, b_start: u16, b_len: u16) -> u16 {
    let a_center = a_start.saturating_mul(2).saturating_add(a_len);
    let b_center = b_start.saturating_mul(2).saturating_add(b_len);
    a_center.abs_diff(b_center)
}

// --- Tree operations ---

fn valid_split_ratio(ratio: f32) -> f32 {
    if ratio.is_finite() {
        ratio.clamp(0.1, 0.9)
    } else {
        0.5
    }
}

fn split_rect(area: Rect, direction: Direction, ratio: f32) -> (Rect, Rect) {
    match direction {
        Direction::Horizontal => {
            let first_w = ((area.width as f32) * ratio).round() as u16;
            let second_w = area.width.saturating_sub(first_w);
            (
                Rect::new(area.x, area.y, first_w, area.height),
                Rect::new(area.x + first_w, area.y, second_w, area.height),
            )
        }
        Direction::Vertical => {
            let first_h = ((area.height as f32) * ratio).round() as u16;
            let second_h = area.height.saturating_sub(first_h);
            (
                Rect::new(area.x, area.y, area.width, first_h),
                Rect::new(area.x, area.y + first_h, area.width, second_h),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane(id: u32) -> PaneId {
        PaneId::from_raw(id)
    }

    fn sample_layout() -> TileLayout {
        TileLayout::from_saved(
            Node::Split {
                direction: Direction::Horizontal,
                ratio: 0.3,
                first: Box::new(Node::Pane(pane(1))),
                second: Box::new(Node::Split {
                    direction: Direction::Vertical,
                    ratio: 0.6,
                    first: Box::new(Node::Pane(pane(2))),
                    second: Box::new(Node::Split {
                        direction: Direction::Horizontal,
                        ratio: 0.4,
                        first: Box::new(Node::Pane(pane(3))),
                        second: Box::new(Node::Pane(pane(4))),
                    }),
                }),
            },
            pane(2),
        )
    }

    fn pane_rects(layout: &TileLayout) -> Vec<(PaneId, Rect)> {
        layout
            .panes(Rect::new(0, 0, 100, 40))
            .into_iter()
            .map(|info| (info.id, info.rect))
            .collect()
    }

    fn pane_rect(layout: &TileLayout, pane_id: PaneId) -> Rect {
        pane_rects(layout)
            .into_iter()
            .find_map(|(id, rect)| (id == pane_id).then_some(rect))
            .expect("pane should exist")
    }

    fn split_snapshot(layout: &TileLayout) -> Vec<(Direction, f32)> {
        fn collect(node: &TypedNode, out: &mut Vec<(Direction, f32)>) {
            match node {
                TypedNode::Leaf(_) | TypedNode::View { .. } => {}
                TypedNode::Stack {
                    selected, children, ..
                } => {
                    if let Some(child) = children.get(*selected) {
                        collect(child, out);
                    }
                }
                TypedNode::Split {
                    direction,
                    ratio,
                    first,
                    second,
                } => {
                    out.push((*direction, *ratio));
                    collect(first, out);
                    collect(second, out);
                }
            }
        }

        let mut out = Vec::new();
        collect(layout.typed_root(), &mut out);
        out
    }

    #[test]
    fn pane_only_layout_characterization_preserves_leaf_order_geometry_and_focus() {
        let layout = sample_layout();

        assert_eq!(
            layout.leaves(),
            vec![
                LayoutLeaf::Pane(pane(1)),
                LayoutLeaf::Pane(pane(2)),
                LayoutLeaf::Pane(pane(3)),
                LayoutLeaf::Pane(pane(4)),
            ]
        );
        assert_eq!(layout.focused_leaf(), LayoutLeaf::Pane(pane(2)));
        assert_eq!(layout.focused(), pane(2));
        assert_eq!(pane_rect(&layout, pane(1)), Rect::new(0, 0, 30, 40));
        assert_eq!(pane_rect(&layout, pane(2)), Rect::new(30, 0, 70, 24));
    }

    #[test]
    fn legacy_split_pane_keeps_growing_a_deep_binary_branch() {
        let (mut layout, root) = TileLayout::new();
        let mut expected = vec![root];
        for _ in 0..6 {
            expected.push(layout.split_pane(root, Direction::Horizontal, 0.8).unwrap());
        }
        let depth = layout
            .splits(Rect::new(0, 0, 512, 128))
            .iter()
            .map(|split| split.path.len() + 1)
            .max()
            .unwrap();
        assert_eq!(depth, 6);
        assert_eq!(layout.focused(), root);
        let placed = layout.pane_ids();
        assert_eq!(placed.len(), expected.len());
        assert!(expected.iter().all(|id| placed.contains(id)));
        assert!(placed
            .iter()
            .all(|id| layout.placement(*id) == Some(PanePlacement::Tiled)));
    }

    #[test]
    fn split_borders_keep_distinct_paths_and_in_place_splits_are_local() {
        let (mut layout, root) = TileLayout::new();
        layout.split_pane(root, Direction::Horizontal, 0.5).unwrap();
        layout.split_pane(root, Direction::Vertical, 0.5).unwrap();
        let area = Rect::new(0, 0, 80, 24);
        let splits = layout.splits(area);
        assert_eq!(splits.len(), 2);
        // One shared path buffer must still yield one distinct path per border.
        let paths = splits
            .iter()
            .map(|split| split.path.clone())
            .collect::<Vec<_>>();
        assert!(paths.contains(&vec![]), "{paths:?}");
        assert!(paths.contains(&vec![false]), "{paths:?}");
        // Splitting a leaf in place leaves the unrelated branch geometry alone.
        let sibling = layout
            .pane_ids()
            .into_iter()
            .find(|pane_id| *pane_id != root)
            .expect("sibling pane");
        let sibling_before = pane_rect(&layout, sibling);
        let added = layout
            .split_pane(root, Direction::Horizontal, 0.5)
            .expect("root is still splittable");
        assert_eq!(pane_rect(&layout, sibling), sibling_before);
        assert_eq!(layout.splits(area).len(), 3);
        assert!(layout.pane_ids().contains(&added));
    }

    #[test]
    fn legacy_insert_pane_near_preserves_ids_beyond_four_split_levels() {
        let (mut layout, root) = TileLayout::new();
        for _ in 0..6 {
            layout.split_pane(root, Direction::Horizontal, 0.8).unwrap();
        }
        let before = layout.pane_ids();
        let moved = PaneId::alloc();
        assert!(layout.insert_pane_near(root, moved, Direction::Vertical, 0.8, true));
        assert_eq!(layout.focused(), moved);
        assert_eq!(layout.pane_ids().len(), before.len() + 1);
        assert!(before.iter().all(|id| layout.pane_ids().contains(id)));
        assert_eq!(layout.placement(moved), Some(PanePlacement::Tiled));
        assert_eq!(
            layout
                .splits(Rect::new(0, 0, 512, 128))
                .iter()
                .map(|split| split.path.len() + 1)
                .max(),
            Some(7)
        );

        let tree = layout.typed_root().clone();
        let focus = layout.focused_leaf();
        assert!(!layout.insert_pane_near(root, moved, Direction::Horizontal, 0.5, false));
        assert_eq!(layout.typed_root(), &tree);
        assert_eq!(layout.focused_leaf(), focus);
    }

    #[test]
    fn creating_collection_does_not_allocate_or_change_pane_ids() {
        let (mut layout, root) = TileLayout::new();
        let collection =
            TerminalContainerId::from_persisted(9_000_001).expect("valid container id");
        let pane_ids_before = layout.pane_ids();

        assert!(layout.insert_collection_near(
            LayoutLeaf::Pane(root),
            collection,
            Direction::Horizontal,
            0.5,
        ));

        assert_eq!(layout.pane_ids(), pane_ids_before);
        assert_eq!(
            layout.leaves(),
            vec![LayoutLeaf::Pane(root), LayoutLeaf::Stack(collection)]
        );
        assert_eq!(layout.focused_leaf(), LayoutLeaf::Pane(root));
        assert_eq!(layout.panes(Rect::new(0, 0, 80, 20)).len(), 1);
    }

    #[test]
    fn collection_ids_are_distinct_and_restored_ids_reserve_allocator_space() {
        let restored = CollectionId::from_raw(9_100_001).expect("valid restored id");
        let allocated = CollectionId::alloc().expect("collection id available");
        assert_ne!(restored, allocated);
        assert!(allocated.raw() > restored.raw());
    }

    #[test]
    fn untrusted_collection_id_parsing_does_not_reserve_allocator_space() {
        let before = CollectionId::alloc().expect("collection ID available");
        let parsed = CollectionId::parse("collection_18446744073709551614")
            .expect("near-max public ID parses");
        let after = CollectionId::alloc().expect("untrusted lookup must not exhaust allocation");

        assert!(parsed.raw() > after.raw());
        assert!(after.raw() > before.raw());
    }

    #[test]
    fn collection_id_serde_is_prefixed_string_and_rejects_invalid_values() {
        let id = CollectionId::from_raw(42).expect("valid collection id");
        assert_eq!(serde_json::to_string(&id).unwrap(), "\"collection_42\"");
        assert_eq!(
            serde_json::from_str::<CollectionId>("\"collection_42\"").unwrap(),
            id
        );
        assert!(serde_json::from_str::<CollectionId>("42").is_err());
        assert!(serde_json::from_str::<CollectionId>("\"collection_0\"").is_err());
        assert!(CollectionId::from_raw(0).is_err());
        assert!(CollectionId::from_raw(u64::MAX).is_err());
    }

    #[test]
    fn grouped_focus_and_enumeration_resolve_selected_member() {
        let (mut layout, root) = TileLayout::new();
        let child = layout.split_focused(Direction::Horizontal);
        let collection =
            TerminalContainerId::from_persisted(9_200_001).expect("valid container id");
        assert!(layout.insert_collection_near(
            LayoutLeaf::Pane(root),
            collection,
            Direction::Vertical,
            0.5,
        ));
        assert!(layout.remove_tiled_pane_for_collection(child));
        assert!(layout.add_collection_member(collection, child));

        layout.focus_pane(child);

        assert_eq!(layout.focused_leaf(), LayoutLeaf::Stack(collection));
        assert_eq!(layout.focused(), child);
        assert_eq!(layout.pane_ids(), vec![root, child]);
        assert_eq!(layout.pane_count(), 2);
        assert_eq!(layout.tiled_pane_ids(), vec![root]);
        assert_eq!(layout.tiled_pane_count(), 1);
        let previews = layout.panes(Rect::new(0, 0, 80, 20));
        assert_eq!(
            previews.len(),
            2,
            "selected collection member gets a dogfood preview"
        );
        assert!(previews
            .iter()
            .any(|pane| pane.id == child && pane.is_focused));
        assert_eq!(
            layout.collection(collection).unwrap().selected(),
            Some(child)
        );
    }

    #[test]
    fn collection_only_canonical_root_keeps_members_without_stale_tiled_leaf() {
        let (mut layout, root) = TileLayout::new();
        let collection =
            TerminalContainerId::from_persisted(9_300_001).expect("valid container id");
        assert!(layout.insert_collection_near(
            LayoutLeaf::Pane(root),
            collection,
            Direction::Horizontal,
            0.5,
        ));
        assert!(layout.remove_tiled_pane_for_collection(root));
        assert!(layout.add_collection_member(collection, root));
        assert!(layout.focus_leaf(LayoutLeaf::Stack(collection)));

        assert!(layout.tiled_pane_ids().is_empty());
        assert_eq!(layout.pane_ids(), vec![root]);
        assert_eq!(layout.focused(), root);
        assert!(matches!(layout.typed_root(), TypedNode::Stack { id, .. } if *id == collection));
        assert_eq!(layout.typed_root().selected_pane(), Some(root));
    }

    #[test]
    fn splitting_selected_collection_member_splits_the_collection_leaf() {
        let (mut layout, root) = TileLayout::new();
        let collection =
            TerminalContainerId::from_persisted(9_350_001).expect("valid container id");
        assert!(layout.insert_collection_near(
            LayoutLeaf::Pane(root),
            collection,
            Direction::Horizontal,
            0.5,
        ));
        assert!(layout.remove_tiled_pane_for_collection(root));
        assert!(layout.add_collection_member(collection, root));

        let new_pane = layout
            .split_pane(root, Direction::Vertical, 0.4)
            .expect("collection member remains a valid split target");

        assert_eq!(
            layout.placement(root),
            Some(PanePlacement::Stack(collection))
        );
        assert_eq!(layout.placement(new_pane), Some(PanePlacement::Tiled));
        assert_eq!(layout.leaf_count(), 2);
        assert_eq!(layout.pane_count(), 2);
    }

    #[test]
    fn focus_pane_selects_requested_member_inside_focused_collection() {
        let (mut layout, root) = TileLayout::new();
        let second = layout.split_focused(Direction::Horizontal);
        let collection =
            TerminalContainerId::from_persisted(9_400_001).expect("valid container id");
        assert!(layout.insert_collection_near(
            LayoutLeaf::Pane(root),
            collection,
            Direction::Vertical,
            0.5,
        ));
        assert!(layout.remove_tiled_pane_for_collection(root));
        assert!(layout.add_collection_member(collection, root));
        assert!(layout.remove_tiled_pane_for_collection(second));
        assert!(layout.add_collection_member(collection, second));

        layout.focus_pane(second);

        assert_eq!(layout.focused_leaf(), LayoutLeaf::Stack(collection));
        assert_eq!(layout.focused(), second);
        assert_eq!(
            layout.collection(collection).unwrap().selected(),
            Some(second)
        );
        assert!(matches!(layout.typed_root(), TypedNode::Stack { id, .. } if *id == collection));
        assert_eq!(layout.typed_root().selected_pane(), Some(second));
    }

    #[test]
    fn resizing_focused_collection_resizes_its_top_level_bsp_leaf() {
        let (mut layout, root) = TileLayout::new();
        let collection =
            TerminalContainerId::from_persisted(9_500_001).expect("valid container id");
        assert!(layout.insert_collection_near(
            LayoutLeaf::Pane(root),
            collection,
            Direction::Horizontal,
            0.5,
        ));
        assert!(layout.focus_leaf(LayoutLeaf::Stack(collection)));

        layout.resize_focused(NavDirection::Left, 0.05, Rect::new(0, 0, 100, 40));

        let splits = layout.splits(Rect::new(0, 0, 100, 40));
        assert_eq!(splits.len(), 1);
        assert!((splits[0].ratio - 0.45).abs() < f32::EPSILON);
    }

    #[test]
    fn collection_selected_member_focus_and_resize_target_same_terminal() {
        let (mut layout, root) = TileLayout::new();
        let member = layout.split_focused(Direction::Vertical);
        let collection =
            TerminalContainerId::from_persisted(9_600_001).expect("valid container id");
        assert!(layout.insert_collection_near(
            LayoutLeaf::Pane(root),
            collection,
            Direction::Horizontal,
            0.5,
        ));
        assert!(layout.remove_tiled_pane_for_collection(member));
        assert!(layout.add_collection_member(collection, member));
        assert!(layout.select_collection_member(collection, member));
        layout.focus_pane(member);
        let previous_focus = layout.focused_leaf();

        assert_eq!(layout.focused(), member);
        assert_eq!(
            layout.collection(collection).unwrap().selected(),
            Some(member)
        );
        assert!(layout.resize_pane(member, NavDirection::Left, 0.05, Rect::new(0, 0, 100, 40),));

        assert_eq!(layout.focused_leaf(), previous_focus);
        assert_eq!(layout.focused(), member);
        let splits = layout.splits(Rect::new(0, 0, 100, 40));
        assert!((splits[0].ratio - 0.45).abs() < f32::EPSILON);
    }

    #[test]
    fn collection_member_has_exactly_one_placement_after_every_mutation() {
        let (mut layout, root) = TileLayout::new();
        let first = layout.split_focused(Direction::Horizontal);
        let second = layout.split_focused(Direction::Vertical);
        let assert_unique = |layout: &TileLayout| {
            for pane in [root, first, second] {
                assert_eq!(
                    layout
                        .pane_ids()
                        .iter()
                        .filter(|candidate| **candidate == pane)
                        .count(),
                    1,
                    "pane {pane:?} must have exactly one placement"
                );
                assert!(layout.placement(pane).is_some());
            }
        };
        assert_unique(&layout);
        let collection = TerminalContainerId::from_persisted(9_700_001).expect("container id");
        assert!(layout.insert_collection_near(
            LayoutLeaf::Pane(root),
            collection,
            Direction::Vertical,
            0.5
        ));
        assert_unique(&layout);
        for pane in [first, second] {
            assert!(layout.remove_tiled_pane_for_collection(pane));
            assert!(layout.add_collection_member(collection, pane));
            assert_unique(&layout);
            assert_eq!(
                layout.placement(pane),
                Some(PanePlacement::Stack(collection))
            );
        }
        assert!(layout.reorder_collection_member(collection, second, 0));
        assert_unique(&layout);
        assert!(layout.select_collection_member(collection, second));
        assert_unique(&layout);
        let group = layout.collection(collection).expect("collection");
        assert_eq!(group.members(), &[second, first]);
        assert_eq!(group.selected(), Some(second));
        assert_unique(&layout);
        assert!(layout.remove_collection_member(collection, second));
        assert!(layout.insert_pane_near(root, second, Direction::Horizontal, 0.5, true));
        assert_unique(&layout);
        assert_eq!(layout.placement(second), Some(PanePlacement::Tiled));
    }

    #[test]
    fn swap_panes_exchanges_leaf_ids_without_changing_cells() {
        let mut layout = sample_layout();
        let before_rects = pane_rects(&layout);
        let before_splits = split_snapshot(&layout);

        assert!(layout.swap_panes(pane(2), pane(4)));

        assert_eq!(layout.pane_count(), 4);
        assert_eq!(split_snapshot(&layout), before_splits);
        assert_eq!(layout.focused(), pane(2));

        let after_rects = pane_rects(&layout);
        assert_eq!(after_rects[0], before_rects[0]);
        assert_eq!(after_rects[1], (pane(4), before_rects[1].1));
        assert_eq!(after_rects[2], before_rects[2]);
        assert_eq!(after_rects[3], (pane(2), before_rects[3].1));
    }

    #[test]
    fn swap_panes_is_noop_for_same_or_missing_pane() {
        let mut layout = sample_layout();
        let before_rects = pane_rects(&layout);
        let before_splits = split_snapshot(&layout);
        let before_focus = layout.focused();

        assert!(!layout.swap_panes(pane(2), pane(2)));
        assert!(!layout.swap_panes(pane(2), pane(99)));
        assert!(!layout.swap_panes(pane(99), pane(2)));

        assert_eq!(pane_rects(&layout), before_rects);
        assert_eq!(split_snapshot(&layout), before_splits);
        assert_eq!(layout.focused(), before_focus);
    }

    #[test]
    fn insert_existing_pane_near_target_preserves_existing_ids_and_focuses_moved_pane() {
        let (mut layout, root) = TileLayout::new();
        let moved = pane(99);

        assert!(layout.insert_pane_near(root, moved, Direction::Horizontal, 0.25, true));

        assert_eq!(layout.pane_count(), 2);
        assert_eq!(layout.pane_ids(), vec![root, moved]);
        assert_eq!(layout.focused(), moved);
        let splits = split_snapshot(&layout);
        assert_eq!(splits, vec![(Direction::Horizontal, 0.25)]);
        assert_eq!(pane_rect(&layout, root), Rect::new(0, 0, 25, 40));
        assert_eq!(pane_rect(&layout, moved), Rect::new(25, 0, 75, 40));
    }

    #[test]
    fn split_focused_with_ratio_sets_new_split_ratio() {
        let (mut layout, root) = TileLayout::new();
        layout.focus_pane(root);

        layout.split_focused_with_ratio(Direction::Horizontal, 0.333);

        let splits = split_snapshot(&layout);
        assert_eq!(splits.len(), 1);
        assert_eq!(splits[0].0, Direction::Horizontal);
        assert!((splits[0].1 - 0.333).abs() < f32::EPSILON);
    }

    #[test]
    fn resize_pane_preserves_focus_and_reports_change() {
        let mut layout = sample_layout();
        let original_focus = layout.focused();

        assert!(layout.resize_pane(pane(1), NavDirection::Right, 0.05, Rect::new(0, 0, 100, 40),));

        assert_eq!(layout.focused(), original_focus);
        let split = split_snapshot(&layout)[0];
        assert_eq!(split.0, Direction::Horizontal);
        assert!((split.1 - 0.35).abs() < f32::EPSILON);
    }

    #[test]
    fn resize_second_child_toward_split_decreases_ratio() {
        let (mut layout, root) = TileLayout::new();
        let right = layout.split_focused(Direction::Horizontal);
        layout.focus_pane(root);

        assert!(layout.resize_pane(right, NavDirection::Left, 0.05, Rect::new(0, 0, 100, 40),));

        let split = split_snapshot(&layout)[0];
        assert_eq!(split.0, Direction::Horizontal);
        assert!((split.1 - 0.45).abs() < f32::EPSILON);
        assert_eq!(layout.focused(), root);
    }

    #[test]
    fn resize_outer_edges_shrink_focused_pane() {
        let (mut horizontal, left) = TileLayout::new();
        horizontal.split_focused(Direction::Horizontal);

        assert!(horizontal.resize_pane(left, NavDirection::Left, 0.05, Rect::new(0, 0, 100, 40),));
        let split = split_snapshot(&horizontal)[0];
        assert_eq!(split.0, Direction::Horizontal);
        assert!((split.1 - 0.45).abs() < f32::EPSILON);

        let (mut horizontal, _left) = TileLayout::new();
        let right = horizontal.split_focused(Direction::Horizontal);

        assert!(horizontal.resize_pane(right, NavDirection::Right, 0.05, Rect::new(0, 0, 100, 40),));
        let split = split_snapshot(&horizontal)[0];
        assert_eq!(split.0, Direction::Horizontal);
        assert!((split.1 - 0.55).abs() < f32::EPSILON);

        let (mut vertical, top) = TileLayout::new();
        vertical.split_focused(Direction::Vertical);

        assert!(vertical.resize_pane(top, NavDirection::Up, 0.05, Rect::new(0, 0, 100, 40),));
        let split = split_snapshot(&vertical)[0];
        assert_eq!(split.0, Direction::Vertical);
        assert!((split.1 - 0.45).abs() < f32::EPSILON);

        let (mut vertical, _top) = TileLayout::new();
        let bottom = vertical.split_focused(Direction::Vertical);

        assert!(vertical.resize_pane(bottom, NavDirection::Down, 0.05, Rect::new(0, 0, 100, 40),));
        let split = split_snapshot(&vertical)[0];
        assert_eq!(split.0, Direction::Vertical);
        assert!((split.1 - 0.55).abs() < f32::EPSILON);
    }

    #[test]
    fn resize_outer_edge_falls_back_to_horizontal_ancestor_split() {
        let mut layout = TileLayout::from_saved(
            Node::Split {
                direction: Direction::Horizontal,
                ratio: 0.6,
                first: Box::new(Node::Split {
                    direction: Direction::Vertical,
                    ratio: 0.5,
                    first: Box::new(Node::Pane(pane(1))),
                    second: Box::new(Node::Pane(pane(2))),
                }),
                second: Box::new(Node::Pane(pane(3))),
            },
            pane(1),
        );
        let before = pane_rect(&layout, pane(1));

        assert!(layout.resize_pane(pane(1), NavDirection::Left, 0.05, Rect::new(0, 0, 100, 40),));

        let after = pane_rect(&layout, pane(1));
        assert_eq!(after.height, before.height);
        assert!(after.width < before.width);
        let splits = split_snapshot(&layout);
        assert_eq!(splits[0].0, Direction::Horizontal);
        assert!((splits[0].1 - 0.55).abs() < f32::EPSILON);
        assert_eq!(splits[1], (Direction::Vertical, 0.5));
    }

    #[test]
    fn resize_outer_edge_falls_back_to_vertical_ancestor_split() {
        let mut layout = TileLayout::from_saved(
            Node::Split {
                direction: Direction::Vertical,
                ratio: 0.6,
                first: Box::new(Node::Split {
                    direction: Direction::Horizontal,
                    ratio: 0.5,
                    first: Box::new(Node::Pane(pane(1))),
                    second: Box::new(Node::Pane(pane(2))),
                }),
                second: Box::new(Node::Pane(pane(3))),
            },
            pane(1),
        );
        let before = pane_rect(&layout, pane(1));

        assert!(layout.resize_pane(pane(1), NavDirection::Up, 0.05, Rect::new(0, 0, 100, 40),));

        let after = pane_rect(&layout, pane(1));
        assert_eq!(after.width, before.width);
        assert!(after.height < before.height);
        let splits = split_snapshot(&layout);
        assert_eq!(splits[0].0, Direction::Vertical);
        assert!((splits[0].1 - 0.55).abs() < f32::EPSILON);
        assert_eq!(splits[1], (Direction::Horizontal, 0.5));
    }

    #[test]
    fn resize_uses_split_in_same_branch_when_borders_share_coordinate() {
        let mut layout = TileLayout::from_saved(
            Node::Split {
                direction: Direction::Vertical,
                ratio: 0.5,
                first: Box::new(Node::Split {
                    direction: Direction::Horizontal,
                    ratio: 0.5,
                    first: Box::new(Node::Pane(pane(1))),
                    second: Box::new(Node::Pane(pane(2))),
                }),
                second: Box::new(Node::Split {
                    direction: Direction::Horizontal,
                    ratio: 0.5,
                    first: Box::new(Node::Pane(pane(3))),
                    second: Box::new(Node::Pane(pane(4))),
                }),
            },
            pane(3),
        );

        assert!(layout.resize_pane(pane(3), NavDirection::Right, 0.05, Rect::new(0, 0, 100, 40),));

        let splits = split_snapshot(&layout);
        assert_eq!(splits[0], (Direction::Vertical, 0.5));
        assert_eq!(splits[1], (Direction::Horizontal, 0.5));
        assert_eq!(splits[2].0, Direction::Horizontal);
        assert!((splits[2].1 - 0.55).abs() < f32::EPSILON);
    }

    #[test]
    fn find_in_direction_tiebreaks_by_larger_overlap_before_layout_order() {
        let focused = PaneInfo {
            id: pane(1),
            rect: Rect::new(10, 10, 10, 10),
            inner_rect: Rect::new(10, 10, 10, 10),
            scrollbar_rect: None,
            borders: Borders::NONE,
            is_focused: true,
        };
        let small_overlap_first = PaneInfo {
            id: pane(2),
            rect: Rect::new(0, 10, 10, 2),
            inner_rect: Rect::new(0, 10, 10, 2),
            scrollbar_rect: None,
            borders: Borders::NONE,
            is_focused: false,
        };
        let larger_overlap_second = PaneInfo {
            id: pane(3),
            rect: Rect::new(0, 10, 10, 8),
            inner_rect: Rect::new(0, 10, 10, 8),
            scrollbar_rect: None,
            borders: Borders::NONE,
            is_focused: false,
        };
        let panes = vec![focused.clone(), small_overlap_first, larger_overlap_second];

        assert_eq!(
            find_in_direction(&focused, NavDirection::Left, &panes),
            Some(pane(3))
        );
    }
}
