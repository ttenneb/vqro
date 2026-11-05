//! One transient membership/geometry pass; no session/tree or provider authority.
use super::{LayoutLeaf, PaneFramePolicy, PaneId, PaneInfo, SplitBorder, TypedNode};
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BoxFrame {
    pub rect: Rect,
    pub title_pane: Option<PaneId>,
}
#[derive(Default)]
pub(crate) struct LayoutGeometry {
    pub panes: Vec<PaneInfo>,
    pub splits: Vec<SplitBorder>,
    pub boxes: Vec<BoxFrame>,
    pub leaf_rects: HashMap<LayoutLeaf, Rect>,
}
#[derive(Clone, Copy, Default)]
struct Facts {
    terminals: u8,
    terminal: Option<PaneId>,
    visible_terminals: u8,
    visible_views: bool,
    views: bool,
    valid_scopes: bool,
}
impl Facts {
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
    let result = match node {
        TypedNode::Leaf(pane) => Facts {
            terminals: 1,
            terminal: Some(*pane),
            visible_terminals: 1,
            valid_scopes: true,
            ..Facts::default()
        },
        TypedNode::View { .. } => Facts {
            views: true,
            visible_views: true,
            valid_scopes: true,
            ..Facts::default()
        },
        TypedNode::Split { first, second, .. } => {
            let a = gather(first, facts);
            let b = gather(second, facts);
            let terminals = a.terminals.saturating_add(b.terminals).min(2);
            Facts {
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
                    && (!matches!(**first, TypedNode::View { .. }) || terminals <= 1)
                    && (!matches!(**second, TypedNode::View { .. }) || terminals <= 1),
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
                }
            }
            if result.terminals != 1 {
                result.terminal = None;
            }
            result.valid_scopes &= !direct_view || result.terminals <= 1;
            result
        }
    };
    facts.insert(key(node), result);
    result
}
fn inset(area: Rect) -> Rect {
    Rect::new(
        area.x.saturating_add(area.width.min(1)),
        area.y.saturating_add(area.height.min(1)),
        area.width.saturating_sub(2),
        area.height.saturating_sub(2),
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
            if let (TypedNode::View { pane, .. }, Some(terminal)) = (child, terminal) {
                out.insert(*pane, terminal);
            }
            visit(child, terminal, facts, out);
        };
        match node {
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

pub(super) fn auto_size(
    root: &mut TypedNode,
    area: Rect,
    measures: &HashMap<PaneId, crate::view_sizing::ViewMeasure>,
    headers: &std::collections::HashSet<PaneId>,
) -> bool {
    let measures = measures
        .iter()
        .map(|(&pane, &measure)| {
            let mut measure = measure;
            measure.rows = measure
                .rows
                .saturating_add(u16::from(headers.contains(&pane)));
            (pane, measure)
        })
        .collect::<HashMap<_, _>>();
    fn minimum(
        node: &TypedNode,
        facts: &HashMap<usize, Facts>,
        measures: &HashMap<PaneId, crate::view_sizing::ViewMeasure>,
        inside: bool,
        headers: &std::collections::HashSet<PaneId>,
        out: &mut HashMap<usize, (u16, u16)>,
    ) -> (u16, u16) {
        let grouped = !inside
            && matches!(node, TypedNode::Split { .. })
            && facts
                .get(&key(node))
                .copied()
                .and_then(Facts::group_terminal)
                .is_some();
        let inside = inside || grouped;
        let mut size = match node {
            // Reserve twenty body columns plus the existing optional scrollbar
            // gutter. The conservative extra cell also works with scrollbars off.
            TypedNode::Leaf(pane) => (21, 4 + u16::from(headers.contains(pane))),
            TypedNode::View { pane, .. } => (8, 3 + u16::from(headers.contains(pane))),
            TypedNode::Stack {
                selected, children, ..
            } => children
                .get(*selected)
                .map(|child| minimum(child, facts, measures, inside, headers, out))
                .unwrap_or((0, 0)),
            TypedNode::Split {
                direction,
                ratio,
                first,
                second,
            } => {
                let a = minimum(first, facts, measures, inside, headers, out);
                let b = minimum(second, facts, measures, inside, headers, out);
                let automatic = [first.as_ref(), second.as_ref()].iter().any(|child| matches!(child, TypedNode::View { pane, .. } if measures.contains_key(pane)));
                let axis = |a: u16, b: u16| {
                    if automatic {
                        a.saturating_add(b)
                    } else {
                        ((a as f32 / ratio.max(f32::EPSILON))
                            .ceil()
                            .max((b as f32 / (1.0 - ratio).max(f32::EPSILON)).ceil()))
                        .min(u16::MAX as f32) as u16
                    }
                };
                if *direction == Direction::Horizontal {
                    (axis(a.0, b.0), a.1.max(b.1))
                } else {
                    (a.0.max(b.0), axis(a.1, b.1))
                }
            }
        };
        if grouped {
            size = (size.0.saturating_add(2), size.1.saturating_add(2));
        }
        out.insert(key(node), size);
        size
    }
    fn apply(
        node: &mut TypedNode,
        mut area: Rect,
        inside: bool,
        facts: &HashMap<usize, Facts>,
        minima: &HashMap<usize, (u16, u16)>,
        measures: &HashMap<PaneId, crate::view_sizing::ViewMeasure>,
    ) -> bool {
        let grouped = !inside
            && matches!(node, TypedNode::Split { .. })
            && facts
                .get(&key(node))
                .copied()
                .and_then(Facts::group_terminal)
                .is_some();
        let inside = inside || grouped;
        if grouped {
            area = inset(area);
        }
        match node {
            TypedNode::Stack {
                selected, children, ..
            } => children
                .get_mut(*selected)
                .is_some_and(|child| apply(child, area, inside, facts, minima, measures)),
            TypedNode::Split {
                direction,
                ratio,
                first,
                second,
            } => {
                let mut changed = false;
                let selected = match (&**first, &**second) {
                    (TypedNode::View { pane, .. }, other) => {
                        measures.get(pane).map(|measure| (true, *measure, other))
                    }
                    (other, TypedNode::View { pane, .. }) => {
                        measures.get(pane).map(|measure| (false, *measure, other))
                    }
                    _ => None,
                };
                if inside {
                    if let Some((before, measure, other)) = selected {
                        let other_min = minima.get(&key(other)).copied().unwrap_or((20, 4));
                        let (extent, reserve, wanted, low, high) =
                            if *direction == Direction::Horizontal {
                                (
                                    area.width,
                                    other_min.0,
                                    measure.width,
                                    area.width.div_ceil(4),
                                    (area.width as u32 * 60 / 100) as u16,
                                )
                            } else {
                                (
                                    area.height,
                                    other_min.1,
                                    measure.rows.saturating_add(2),
                                    3,
                                    ((area.height as u32 * 35 / 100) as u16).min(12),
                                )
                            };
                        if extent > 0 {
                            let cap = high.min(extent.saturating_sub(reserve));
                            let wanted = wanted.clamp(low.min(cap), cap);
                            let first_cells = if before {
                                wanted
                            } else {
                                extent.saturating_sub(wanted)
                            };
                            // Automatic cell bounds, unlike explicit manual ratios,
                            // may require a share below ten percent. Keep a valid
                            // interior saved ratio even for a zero-cell tiny view.
                            let next = (first_cells as f32 / extent as f32)
                                .clamp(f32::EPSILON, 1.0 - f32::EPSILON);
                            if (extent as f32 * next).round() as u16
                                != (extent as f32 * *ratio).round() as u16
                            {
                                *ratio = next;
                                changed = true;
                            }
                        }
                    }
                }
                let (a, b) = super::split_rect(area, *direction, *ratio);
                changed |= apply(first, a, inside, facts, minima, measures);
                changed |= apply(second, b, inside, facts, minima, measures);
                changed
            }
            _ => false,
        }
    }
    if measures.is_empty() {
        return false;
    }
    #[cfg(test)]
    AUTO_GATHERS.with(|count| count.set(count.get() + 1));
    let mut facts = HashMap::new();
    gather(root, &mut facts);
    let mut minima = HashMap::new();
    minimum(root, &facts, &measures, false, headers, &mut minima);
    apply(root, area, false, &facts, &minima, &measures)
}

pub(super) fn geometry(root: &TypedNode, focus: LayoutLeaf, area: Rect) -> LayoutGeometry {
    #[cfg(test)]
    GATHERS.with(|count| count.set(count.get() + 1));
    let mut facts = HashMap::new();
    gather(root, &mut facts);
    let mut result = LayoutGeometry::default();
    let mut path = Vec::new();
    walk(root, focus, area, None, &facts, &mut path, &mut result);
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
    out: &mut LayoutGeometry,
) {
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
            area = inset(area);
        }
    }
    match node {
        TypedNode::Leaf(pane) | TypedNode::View { pane, .. } => {
            let view = matches!(node, TypedNode::View { .. });
            let terminal_group = group.filter(|group| group.terminal == *pane && !view);
            let rect = terminal_group.map_or(area, |group| group.outer);
            out.leaf_rects.insert(LayoutLeaf::Pane(*pane), rect);
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
            if let Some(child) = children.get(*selected) {
                let focus = if focus == LayoutLeaf::Stack(*id) {
                    child.selected_pane().map(LayoutLeaf::Pane).unwrap_or(focus)
                } else {
                    focus
                };
                // Existing split paths cannot address stack children. Keep that
                // inherited interaction limit; geometry still projects correctly.
                let start = out.splits.len();
                walk(child, focus, area, group, facts, path, out);
                out.splits.truncate(start);
            }
        }
        TypedNode::Split {
            direction,
            ratio,
            first,
            second,
        } => {
            let (a, b) = super::split_rect(area, *direction, *ratio);
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
            path.push(false);
            walk(first, focus, a, group, facts, path, out);
            path.pop();
            path.push(true);
            walk(second, focus, b, group, facts, path, out);
            path.pop();
        }
    }
}
