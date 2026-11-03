use super::*;
use crate::layout::{LayoutLeaf, TileLayout};
use ratatui::{layout::Direction, widgets::Borders};

fn pane(id: u32) -> PaneId {
    PaneId::from_raw(id)
}

fn view_group(direction: Direction) -> TypedNode {
    TypedNode::Split {
        direction,
        ratio: 0.7,
        first: Box::new(TypedNode::Leaf(pane(1))),
        second: Box::new(TypedNode::view(pane(2), "vqro.tasks", "tasks")),
    }
}

#[test]
fn side_and_below_frames_share_the_same_inset_geometry() {
    let area = Rect::new(5, 7, 80, 20);
    let content = Rect::new(6, 8, 78, 18);
    for direction in [Direction::Horizontal, Direction::Vertical] {
        let layout =
            TileLayout::from_tree(view_group(direction), LayoutLeaf::Pane(pane(1))).unwrap();
        let panes = layout.panes(area);
        assert_eq!(panes.len(), 2);
        assert!(panes.iter().all(|pane| pane.borders == Borders::ALL));
        let (first, second) = crate::layout::split_rect(content, direction, 0.7);
        assert_eq!(panes[0].rect, first);
        assert_eq!(panes[1].rect, second);
        assert_eq!(
            layout.leaf_rect(LayoutLeaf::Pane(pane(2)), area),
            Some(second)
        );
        let splits = layout.splits(area);
        assert_eq!(splits[0].area, content);
        assert!(splits[0].independent_borders);
        assert_eq!(
            layout.box_frames(area, &[]),
            vec![BoxFrame {
                rect: area,
                title_pane: Some(pane(1)),
                stack: None,
            }]
        );
    }
}

#[test]
fn terminal_only_ancestors_do_not_acquire_redundant_frames() {
    let area = Rect::new(0, 0, 100, 40);
    let root = TypedNode::Split {
        direction: Direction::Horizontal,
        ratio: 0.5,
        first: Box::new(TypedNode::Leaf(pane(3))),
        second: Box::new(view_group(Direction::Vertical)),
    };
    let layout = TileLayout::from_tree(root, LayoutLeaf::Pane(pane(1))).unwrap();
    let frames = layout.box_frames(area, &[]);
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0].rect, Rect::new(50, 0, 50, 40));
    let panes = layout.panes(area);
    assert_eq!(panes[0].rect, Rect::new(0, 0, 50, 40));
    assert_eq!(panes[0].borders, Borders::NONE);
    assert_eq!(panes[1].rect.x, 51);
    assert_eq!(layout.splits(area)[1].area, Rect::new(51, 1, 48, 38));
}

#[test]
fn named_stack_lookup_keeps_outer_rect_and_hidden_branches_emit_nothing() {
    let area = Rect::new(5, 7, 80, 20);
    let id = TerminalContainerId::parse(9_777).unwrap();
    let root = TypedNode::Stack {
        id,
        selected: 0,
        children: vec![TypedNode::Leaf(pane(3)), view_group(Direction::Vertical)],
    };
    let layout = TileLayout::from_tree(root, LayoutLeaf::Pane(pane(3))).unwrap();
    assert!(
        layout.box_frames(area, &[]).is_empty(),
        "hidden group does no frame work"
    );
    let named = [id];
    assert_eq!(layout.box_frames(area, &named).len(), 1);
    assert_eq!(
        layout.leaf_rect_with_box_frames(LayoutLeaf::Stack(id), area, &named),
        Some(area)
    );
    let panes = layout.panes_with_box_frames(area, &named);
    assert_eq!(panes.len(), 1);
    assert_eq!(panes[0].id, pane(3));
    assert_eq!(panes[0].rect, Rect::new(6, 8, 78, 18));
    assert_eq!(panes[0].borders, Borders::ALL);
    assert_eq!(
        layout.leaf_rect_with_box_frames(LayoutLeaf::Pane(pane(2)), area, &named),
        None
    );
}

#[test]
fn tiny_box_geometry_stays_within_parent_even_with_zero_content() {
    for direction in [Direction::Horizontal, Direction::Vertical] {
        let layout =
            TileLayout::from_tree(view_group(direction), LayoutLeaf::Pane(pane(1))).unwrap();
        for width in 0..6 {
            for height in 0..6 {
                let area = Rect::new(5, 7, width, height);
                for pane in layout.panes(area) {
                    assert!(pane.rect.x >= area.x && pane.rect.right() <= area.right());
                    assert!(pane.rect.y >= area.y && pane.rect.bottom() <= area.bottom());
                }
            }
        }
    }
}

#[test]
fn standalone_view_has_its_own_full_frame_without_a_group() {
    let layout = TileLayout::from_tree(
        TypedNode::view(pane(2), "vqro.tasks", "tasks"),
        LayoutLeaf::Pane(pane(2)),
    )
    .unwrap();
    let area = Rect::new(0, 0, 80, 20);
    assert!(layout.box_frames(area, &[]).is_empty());
    assert_eq!(layout.panes(area)[0].borders, Borders::ALL);
    assert_eq!(layout.panes(area)[0].rect, area);
}
