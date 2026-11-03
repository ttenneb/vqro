//! Ephemeral box-frame geometry. Names stay with the caller, never in the tree.
use ratatui::layout::Rect;

use super::{PaneId, TypedNode};
use crate::terminal_container::TerminalContainerId;

#[cfg(test)]
mod tests;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BoxFrame {
    pub rect: Rect,
    pub title_pane: Option<PaneId>,
    pub stack: Option<TerminalContainerId>,
}

/// Prototype depth economy; the human chooses at screenshot review. Returning
/// `true` here instead makes every container framed, without changing geometry
/// consumers or storing presentation policy in the session tree.
fn frame_box(direct_view: bool, named: bool) -> bool {
    direct_view || named
}

pub(super) fn content_rect(
    node: &TypedNode,
    area: Rect,
    named: &[TerminalContainerId],
) -> (Rect, bool) {
    let (direct_view, is_named) = match node {
        TypedNode::Split { first, second, .. } => (
            matches!(**first, TypedNode::View { .. }) || matches!(**second, TypedNode::View { .. }),
            false,
        ),
        TypedNode::Stack { id, children, .. } => (
            children
                .iter()
                .any(|child| matches!(child, TypedNode::View { .. })),
            named.contains(id),
        ),
        TypedNode::Leaf(_) | TypedNode::View { .. } => return (area, false),
    };
    let framed = frame_box(direct_view, is_named);
    (
        if framed {
            Rect::new(
                area.x.saturating_add(area.width.min(1)),
                area.y.saturating_add(area.height.min(1)),
                area.width.saturating_sub(2),
                area.height.saturating_sub(2),
            )
        } else {
            area
        },
        framed,
    )
}

pub(super) fn collect(
    node: &TypedNode,
    area: Rect,
    named: &[TerminalContainerId],
    result: &mut Vec<BoxFrame>,
) {
    let (content, framed) = content_rect(node, area, named);
    if framed {
        let title_pane = match node {
            TypedNode::Split { first, second, .. } => match (&**first, &**second) {
                (TypedNode::Leaf(pane), TypedNode::View { .. })
                | (TypedNode::View { .. }, TypedNode::Leaf(pane)) => Some(*pane),
                _ => None,
            },
            _ => None,
        };
        let stack = match node {
            TypedNode::Stack { id, .. } => Some(*id),
            _ => None,
        };
        result.push(BoxFrame {
            rect: area,
            title_pane,
            stack,
        });
    }
    match node {
        TypedNode::Split {
            direction,
            ratio,
            first,
            second,
        } => {
            let (first_area, second_area) = super::split_rect(content, *direction, *ratio);
            collect(first, first_area, named, result);
            collect(second, second_area, named, result);
        }
        TypedNode::Stack {
            selected, children, ..
        } => {
            if let Some(child) = children.get(*selected) {
                collect(child, content, named, result);
            }
        }
        TypedNode::Leaf(_) | TypedNode::View { .. } => {}
    }
}
