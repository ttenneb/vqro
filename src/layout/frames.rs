//! One transient membership/geometry pass; no session/tree or provider authority.
use super::{
    LayoutLeaf, LayoutPathStep, LayoutSplitChild, PaneFramePolicy, PaneId, PaneInfo, SplitBorder,
    TypedNode,
};
use ratatui::{
    layout::{Direction, Rect},
    widgets::Borders,
};
use std::collections::HashMap;

#[cfg(test)]
mod tests;
#[cfg(test)]
std::thread_local! { static GATHERS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }
#[cfg(test)]
std::thread_local! { static AUTO_GATHERS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }
#[cfg(test)]
pub(crate) fn take_auto_gathers() -> usize {
    AUTO_GATHERS.with(|count| count.replace(0))
}
#[cfg(test)]
pub(crate) fn take_gathers() -> usize {
    GATHERS.with(|count| count.replace(0))
}

/// Arrow, spacing and one readable title initial; height remains one header row.
pub(crate) const COLLAPSED_SIDE_WIDTH: u16 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BoxFrame {
    pub rect: Rect,
    pub title_pane: Option<PaneId>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CollapsedFrame {
    pub rect: Rect,
    pub path: Vec<LayoutPathStep>,
    pub direction: Direction,
    pub title_pane: Option<PaneId>,
    pub leaf_pane: Option<PaneId>,
    pub pane_count: usize,
    pub focused: bool,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ContainerGeometry {
    pub path: Vec<LayoutPathStep>,
    pub rect: Rect,
    pub framed: bool,
    pub stack: bool,
}
#[derive(Default)]
pub(crate) struct LayoutGeometry {
    pub panes: Vec<PaneInfo>,
    pub splits: Vec<SplitBorder>,
    pub boxes: Vec<BoxFrame>,
    pub collapsed: Vec<CollapsedFrame>,
    pub containers: Vec<ContainerGeometry>,
    pub pane_paths: HashMap<PaneId, Vec<LayoutPathStep>>,
    pub leaf_rects: HashMap<LayoutLeaf, Rect>,
}
#[derive(Clone, Copy, Default)]
struct Facts {
    panes: usize,
    terminals: u8,
    terminal: Option<PaneId>,
    visible_terminals: u8,
    visible_views: bool,
    views: bool,
    valid_scopes: bool,
    // Outside and inside an inherited box frame. A qualifying split adds its
    // own chrome only outside; walk suppresses nested frames inside a group.
    compact: Option<(u16, u16)>,
    compact_in_group: Option<(u16, u16)>,
    visible_collapse: bool,
}
impl Facts {
    fn compact(self, inside_group: bool) -> Option<(u16, u16)> {
        if inside_group {
            self.compact_in_group
        } else {
            self.compact
        }
    }

    fn group_terminal(self) -> Option<PaneId> {
        (self.terminals == 1
            && self.visible_terminals == 1
            && self.visible_views
            && self.valid_scopes)
            .then_some(self.terminal)
            .flatten()
    }
}
fn key(node: &TypedNode) -> usize {
    node as *const TypedNode as usize
}

// All membership (including hidden terminals) participates in scope validation.
// Only the selected stack branch contributes visibility. View-only containers
// inherit the nearest enclosing nonempty complete terminal membership.
fn gather(node: &TypedNode, facts: &mut HashMap<usize, Facts>) -> Facts {
    let mut result = match node {
        TypedNode::Collapsed { node } => {
            let mut result = gather(node, facts);
            result.compact = Some((COLLAPSED_SIDE_WIDTH, 1));
            result.compact_in_group = result.compact;
            result.visible_collapse = true;
            result
        }
        TypedNode::Leaf(pane) => Facts {
            panes: 1,
            terminals: 1,
            terminal: Some(*pane),
            visible_terminals: 1,
            valid_scopes: true,
            ..Facts::default()
        },
        TypedNode::View { .. } => Facts {
            panes: 1,
            views: true,
            visible_views: true,
            valid_scopes: true,
            ..Facts::default()
        },
        TypedNode::Split {
            direction,
            first,
            second,
            ..
        } => {
            let a = gather(first, facts);
            let b = gather(second, facts);
            let terminals = a.terminals.saturating_add(b.terminals).min(2);
            Facts {
                panes: a.panes.saturating_add(b.panes),
                terminals,
                terminal: if terminals == 1 {
                    a.terminal.or(b.terminal)
                } else {
                    None
                },
                visible_terminals: a
                    .visible_terminals
                    .saturating_add(b.visible_terminals)
                    .min(2),
                visible_views: a.visible_views || b.visible_views,
                views: a.views || b.views,
                valid_scopes: a.valid_scopes
                    && b.valid_scopes
                    && (!matches!(first.content(), TypedNode::View { .. }) || terminals <= 1)
                    && (!matches!(second.content(), TypedNode::View { .. }) || terminals <= 1),
                visible_collapse: a.visible_collapse || b.visible_collapse,
                compact: compact_split(*direction, a.compact, b.compact),
                compact_in_group: compact_split(*direction, a.compact_in_group, b.compact_in_group),
            }
        }
        TypedNode::Stack {
            selected, children, ..
        } => {
            let mut result = Facts {
                valid_scopes: true,
                ..Facts::default()
            };
            let mut direct_view = false;
            for (index, child) in children.iter().enumerate() {
                let child_facts = gather(child, facts);
                result.panes = result.panes.saturating_add(child_facts.panes);
                result.terminals = result
                    .terminals
                    .saturating_add(child_facts.terminals)
                    .min(2);
                result.terminal = result.terminal.or(child_facts.terminal);
                result.views |= child_facts.views;
                result.valid_scopes &= child_facts.valid_scopes;
                direct_view |= matches!(child, TypedNode::View { .. });
                if index == *selected {
                    result.visible_terminals = child_facts.visible_terminals;
                    result.visible_views = child_facts.visible_views;
                    let header = |(width, height): (u16, u16)| (width, height.saturating_add(1));
                    result.compact = child_facts.compact.map(header);
                    result.compact_in_group = child_facts.compact_in_group.map(header);
                    result.visible_collapse = child_facts.visible_collapse;
                }
            }
            if result.terminals != 1 {
                result.terminal = None;
            }
            result.valid_scopes &= !direct_view || result.terminals <= 1;
            result
        }
    };
    // Same qualification and inherited-frame boundary as walk. Collapsed
    // wrappers return before grouping, so they never acquire phantom chrome.
    if matches!(node, TypedNode::Split { .. }) && result.group_terminal().is_some() {
        result.compact = result
            .compact_in_group
            .map(|(width, height)| (width.saturating_add(2), height.saturating_add(2)));
    }
    facts.insert(key(node), result);
    result
}
fn compact_split(
    direction: Direction,
    a: Option<(u16, u16)>,
    b: Option<(u16, u16)>,
) -> Option<(u16, u16)> {
    a.zip(b).map(|(a, b)| {
        if direction == Direction::Horizontal {
            (a.0.saturating_add(b.0), a.1.max(b.1))
        } else {
            (a.0.max(b.0), a.1.saturating_add(b.1))
        }
    })
}
fn inset(area: Rect) -> Rect {
    Rect::new(
        area.x.saturating_add(area.width.min(1)),
        area.y.saturating_add(area.height.min(1)),
        area.width.saturating_sub(2),
        area.height.saturating_sub(2),
    )
}
fn below_stack_header(area: Rect) -> Rect {
    let row = area.height.min(1);
    Rect::new(
        area.x,
        area.y.saturating_add(row),
        area.width,
        area.height.saturating_sub(row),
    )
}
#[derive(Clone, Copy)]
struct Group {
    terminal: PaneId,
    outer: Rect,
}

pub(super) fn single_terminal_view_scopes(root: &TypedNode) -> HashMap<PaneId, PaneId> {
    fn visit(
        node: &TypedNode,
        inherited: Option<PaneId>,
        facts: &HashMap<usize, Facts>,
        out: &mut HashMap<PaneId, PaneId>,
    ) {
        let terminal = match facts.get(&key(node)) {
            Some(fact) if fact.terminals == 0 => inherited,
            Some(fact) if fact.terminals == 1 => fact.terminal,
            _ => None,
        };
        let mut child = |child: &TypedNode| {
            if let (TypedNode::View { pane, .. }, Some(terminal)) = (child.content(), terminal) {
                out.insert(*pane, terminal);
            }
            visit(child, terminal, facts, out);
        };
        match node {
            TypedNode::Collapsed { node } => visit(node, inherited, facts, out),
            TypedNode::Split { first, second, .. } => {
                child(first);
                child(second);
            }
            TypedNode::Stack { children, .. } => {
                for item in children {
                    child(item);
                }
            }
            _ => {}
        }
    }
    let mut facts = HashMap::new();
    gather(root, &mut facts);
    let mut scopes = HashMap::new();
    visit(root, None, &facts, &mut scopes);
    scopes
}

mod auto_size;
pub(super) use auto_size::auto_size;

fn effective_split(
    area: Rect,
    direction: Direction,
    ratio: f32,
    first: &TypedNode,
    second: &TypedNode,
    inside_group: bool,
    facts: &HashMap<usize, Facts>,
) -> (Rect, Rect) {
    let axis = |size: (u16, u16)| {
        if direction == Direction::Horizontal {
            size.0
        } else {
            size.1
        }
    };
    let a = facts
        .get(&key(first))
        .and_then(|fact| fact.compact(inside_group))
        .map(axis);
    let b = facts
        .get(&key(second))
        .and_then(|fact| fact.compact(inside_group))
        .map(axis);
    let extent = if direction == Direction::Horizontal {
        area.width
    } else {
        area.height
    };
    let (a, b) = match (a, b) {
        (None, None) => return super::split_rect(area, direction, ratio),
        (Some(a), None) => (a.min(extent), extent.saturating_sub(a)),
        (None, Some(b)) => (extent.saturating_sub(b), b.min(extent)),
        (Some(a), Some(b)) => (a.min(extent), b.min(extent.saturating_sub(a))),
    };
    if direction == Direction::Horizontal {
        (
            Rect::new(area.x, area.y, a, area.height),
            Rect::new(area.x.saturating_add(a), area.y, b, area.height),
        )
    } else {
        (
            Rect::new(area.x, area.y, area.width, a),
            Rect::new(area.x, area.y.saturating_add(a), area.width, b),
        )
    }
}

pub(super) fn geometry(root: &TypedNode, focus: LayoutLeaf, area: Rect) -> LayoutGeometry {
    #[cfg(test)]
    GATHERS.with(|count| count.set(count.get() + 1));
    let mut facts = HashMap::new();
    gather(root, &mut facts);
    let mut result = LayoutGeometry::default();
    let mut path = Vec::new();
    walk(
        root,
        focus,
        area,
        None,
        &facts,
        &mut path,
        &mut Vec::new(),
        Direction::Vertical,
        &mut result,
    );
    result
}

#[allow(clippy::too_many_arguments)] // One pass threads geometry, membership and the existing split path.
fn walk(
    node: &TypedNode,
    focus: LayoutLeaf,
    mut area: Rect,
    mut group: Option<Group>,
    facts: &HashMap<usize, Facts>,
    path: &mut Vec<bool>,
    address: &mut Vec<LayoutPathStep>,
    parent_direction: Direction,
    out: &mut LayoutGeometry,
) {
    if let TypedNode::Collapsed { node } = node {
        if parent_direction == Direction::Horizontal {
            area.width = area.width.min(COLLAPSED_SIDE_WIDTH);
        } else {
            area.height = area.height.min(1);
        }
        let focused = match focus {
            LayoutLeaf::Pane(pane) => node.contains_pane(pane),
            LayoutLeaf::Stack(id) => node.stack(id).is_some(),
        };
        out.collapsed.push(CollapsedFrame {
            rect: area,
            path: address.clone(),
            direction: parent_direction,
            title_pane: node.selected_pane(),
            leaf_pane: node.pane_leaf(),
            pane_count: facts.get(&key(node)).map_or(0, |facts| facts.panes),
            focused,
        });
        if matches!(
            node.content(),
            TypedNode::Split { .. } | TypedNode::Stack { .. }
        ) {
            out.containers.push(ContainerGeometry {
                path: address.clone(),
                rect: area,
                framed: false,
                stack: matches!(node.content(), TypedNode::Stack { .. }),
            });
        }
        return;
    }
    let frame_rect = area;
    let mut own_frame = false;
    if group.is_none() && matches!(node, TypedNode::Split { .. }) {
        if let Some(terminal) = facts
            .get(&key(node))
            .copied()
            .and_then(Facts::group_terminal)
        {
            out.boxes.push(BoxFrame {
                rect: area,
                title_pane: Some(terminal),
            });
            group = Some(Group {
                terminal,
                outer: area,
            });
            own_frame = true;
            area = inset(area);
        }
    }
    match node {
        TypedNode::Collapsed { .. } => {} // handled before grouping
        TypedNode::Leaf(pane) | TypedNode::View { pane, .. } => {
            let view = matches!(node, TypedNode::View { .. });
            let terminal_group = group.filter(|group| group.terminal == *pane && !view);
            let rect = terminal_group.map_or(area, |group| group.outer);
            out.leaf_rects.insert(LayoutLeaf::Pane(*pane), rect);
            out.pane_paths.insert(*pane, address.clone());
            out.panes.push(PaneInfo {
                id: *pane,
                rect,
                inner_rect: area,
                scrollbar_rect: None,
                borders: if view { Borders::ALL } else { Borders::NONE },
                frame_policy: if view {
                    PaneFramePolicy::Full
                } else if terminal_group.is_some() {
                    PaneFramePolicy::Unframed
                } else {
                    PaneFramePolicy::Normal
                },
                is_focused: focus == LayoutLeaf::Pane(*pane),
            });
        }
        TypedNode::Stack {
            id,
            selected,
            children,
        } => {
            out.leaf_rects.insert(LayoutLeaf::Stack(*id), area);
            out.containers.push(ContainerGeometry {
                path: address.clone(),
                rect: area,
                framed: true,
                stack: true,
            });
            if let Some(child) = children.get(*selected) {
                let focus = if focus == LayoutLeaf::Stack(*id) {
                    child.selected_pane().map(LayoutLeaf::Pane).unwrap_or(focus)
                } else {
                    focus
                };
                // Existing split paths cannot address stack children. Keep that
                // inherited interaction limit; geometry still projects correctly.
                let start = out.splits.len();
                address.push(LayoutPathStep::StackChild(*selected));
                let content = if facts
                    .get(&key(node))
                    .is_some_and(|facts| facts.visible_collapse)
                {
                    below_stack_header(area)
                } else {
                    area
                };
                walk(
                    child,
                    focus,
                    content,
                    group,
                    facts,
                    path,
                    address,
                    parent_direction,
                    out,
                );
                address.pop();
                out.splits.truncate(start);
            }
        }
        TypedNode::Split {
            direction,
            ratio,
            first,
            second,
        } => {
            out.containers.push(ContainerGeometry {
                path: address.clone(),
                rect: if own_frame { frame_rect } else { area },
                framed: own_frame,
                stack: false,
            });
            let (a, b) = effective_split(
                area,
                *direction,
                *ratio,
                first,
                second,
                group.is_some(),
                facts,
            );
            let compact = facts
                .get(&key(first))
                .and_then(|fact| fact.compact)
                .is_some()
                || facts
                    .get(&key(second))
                    .and_then(|fact| fact.compact)
                    .is_some();
            if !compact {
                out.splits.push(SplitBorder {
                    pos: if *direction == Direction::Horizontal {
                        b.x
                    } else {
                        b.y
                    },
                    direction: *direction,
                    ratio: *ratio,
                    area,
                    path: path.clone(),
                    independent_borders: group.is_some(),
                    view_before: group.is_some()
                        && facts
                            .get(&key(first))
                            .is_some_and(|facts| facts.terminals == 0),
                });
            }
            path.push(false);
            address.push(LayoutPathStep::Split(LayoutSplitChild::First));
            walk(
                first, focus, a, group, facts, path, address, *direction, out,
            );
            address.pop();
            path.pop();
            path.push(true);
            address.push(LayoutPathStep::Split(LayoutSplitChild::Second));
            walk(
                second, focus, b, group, facts, path, address, *direction, out,
            );
            address.pop();
            path.pop();
        }
    }
}
