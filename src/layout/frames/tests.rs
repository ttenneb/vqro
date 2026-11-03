use super::*;
use crate::layout::TileLayout;
use crate::terminal_container::TerminalContainerId;
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
fn side_and_below_share_one_parent_frame_and_an_unframed_terminal_body() {
    let area = Rect::new(5, 7, 80, 20);
    let content = Rect::new(6, 8, 78, 18);
    for direction in [Direction::Horizontal, Direction::Vertical] {
        let layout =
            TileLayout::from_tree(view_group(direction), LayoutLeaf::Pane(pane(1))).unwrap();
        take_gathers();
        let geometry = layout.geometry(area);
        assert_eq!(take_gathers(), 1);
        assert_eq!(geometry.boxes.len(), 1);
        assert_eq!(geometry.boxes[0].rect, area);
        let (terminal_body, view) = crate::layout::split_rect(content, direction, 0.7);
        assert_eq!(geometry.panes[0].rect, area);
        assert_eq!(geometry.panes[0].inner_rect, terminal_body);
        assert_eq!(geometry.panes[0].frame_policy, PaneFramePolicy::Unframed);
        assert_eq!(geometry.panes[0].borders, Borders::NONE);
        assert_eq!(geometry.panes[1].rect, view);
        assert_eq!(geometry.panes[1].frame_policy, PaneFramePolicy::Full);
        assert_eq!(geometry.leaf_rects[&LayoutLeaf::Pane(pane(1))], area);
        assert_eq!(geometry.splits[0].area, content);
        assert!(geometry.splits[0].independent_borders);
    }
}
#[test]
fn maximal_single_terminal_group_does_not_repeat_outer_frames_for_more_views() {
    let root = TypedNode::Split {
        direction: Direction::Horizontal,
        ratio: 0.8,
        first: Box::new(view_group(Direction::Vertical)),
        second: Box::new(TypedNode::view(pane(3), "vqro.tasks", "messages")),
    };
    let layout = TileLayout::from_tree(root, LayoutLeaf::Pane(pane(1))).unwrap();
    let geometry = layout.geometry(Rect::new(0, 0, 100, 40));
    assert_eq!(geometry.boxes.len(), 1);
    assert_eq!(geometry.panes.len(), 3);
    assert_eq!(geometry.panes[0].frame_policy, PaneFramePolicy::Unframed);
}
#[test]
fn hidden_terminal_membership_prevents_a_false_single_terminal_parent() {
    let id = TerminalContainerId::parse(9_777).unwrap();
    let root = TypedNode::Split {
        direction: Direction::Horizontal,
        ratio: 0.5,
        first: Box::new(TypedNode::Stack {
            id,
            selected: 0,
            children: vec![TypedNode::Leaf(pane(1)), TypedNode::Leaf(pane(3))],
        }),
        second: Box::new(TypedNode::view(pane(2), "vqro.tasks", "tasks")),
    };
    let layout = TileLayout::from_tree(root, LayoutLeaf::Pane(pane(1))).unwrap();
    let geometry = layout.geometry(Rect::new(0, 0, 100, 40));
    assert!(geometry.boxes.is_empty());
    assert_eq!(geometry.panes[0].frame_policy, PaneFramePolicy::Normal);
}
#[test]
fn hidden_views_emit_no_frames_and_stacks_keep_normal_geometry() {
    let area = Rect::new(5, 7, 80, 20);
    let id = TerminalContainerId::parse(9_778).unwrap();
    let root = TypedNode::Stack {
        id,
        selected: 0,
        children: vec![TypedNode::Leaf(pane(3)), view_group(Direction::Vertical)],
    };
    let layout = TileLayout::from_tree(root, LayoutLeaf::Pane(pane(3))).unwrap();
    let geometry = layout.geometry(area);
    assert!(geometry.boxes.is_empty());
    assert_eq!(geometry.leaf_rects[&LayoutLeaf::Stack(id)], area);
    assert_eq!(geometry.panes.len(), 1);
    assert_eq!(geometry.panes[0].rect, area);
    assert!(!geometry.leaf_rects.contains_key(&LayoutLeaf::Pane(pane(2))));
}
#[test]
fn tiny_geometry_saturates_without_leaving_its_parent() {
    for direction in [Direction::Horizontal, Direction::Vertical] {
        let layout =
            TileLayout::from_tree(view_group(direction), LayoutLeaf::Pane(pane(1))).unwrap();
        for width in 0..6 {
            for height in 0..6 {
                let area = Rect::new(5, 7, width, height);
                for pane in layout.panes(area) {
                    for rect in [pane.rect, pane.inner_rect] {
                        assert!(rect.x >= area.x && rect.right() <= area.right());
                        assert!(rect.y >= area.y && rect.bottom() <= area.bottom());
                    }
                }
            }
        }
    }
}
#[test]
fn automatic_rows_are_capped_and_unchanged_integer_allocations_are_noops() {
    for direction in [Direction::Horizontal, Direction::Vertical] {
        let mut layout =
            TileLayout::from_tree(view_group(direction), LayoutLeaf::Pane(pane(1))).unwrap();
        let area = Rect::new(0, 0, 100, 40);
        let mut measures = HashMap::from([(
            pane(2),
            crate::view_sizing::ViewMeasure { rows: 2, width: 48 },
        )]);
        assert!(layout.auto_size_views(area, &measures));
        assert!(!layout.auto_size_views(area, &measures));
        let geometry = layout.geometry(area);
        let body = geometry.panes[0].inner_rect;
        assert!(body.width >= 20 && body.height >= 4);
        if direction == Direction::Vertical {
            assert!(geometry.panes[1].rect.height <= 12);
        }
        measures.get_mut(&pane(2)).unwrap().rows = 500;
        layout.auto_size_views(area, &measures);
        assert!(!layout.auto_size_views(area, &measures));
        if direction == Direction::Vertical {
            assert!(layout.geometry(area).panes[1].rect.height <= 12);
        }
    }
}
#[test]
fn multiple_automatic_views_reserve_the_terminal_body_minimum() {
    let root = TypedNode::Split {
        direction: Direction::Horizontal,
        ratio: 0.5,
        first: Box::new(view_group(Direction::Horizontal)),
        second: Box::new(TypedNode::view(pane(3), "sample.views", "other")),
    };
    let mut layout = TileLayout::from_tree(root, LayoutLeaf::Pane(pane(1))).unwrap();
    let measures = HashMap::from([
        (
            pane(2),
            crate::view_sizing::ViewMeasure {
                rows: 100,
                width: 200,
            },
        ),
        (
            pane(3),
            crate::view_sizing::ViewMeasure {
                rows: 100,
                width: 200,
            },
        ),
    ]);
    layout.auto_size_views(Rect::new(0, 0, 100, 40), &measures);
    let geometry = layout.geometry(Rect::new(0, 0, 100, 40));
    assert!(geometry.panes[0].inner_rect.width >= 20);
    assert!(!layout.auto_size_views(Rect::new(0, 0, 100, 40), &measures));
    for width in 0..8 {
        for height in 0..8 {
            layout.auto_size_views(Rect::new(0, 0, width, height), &measures);
            assert!(
                TileLayout::from_tree(layout.root.clone(), LayoutLeaf::Pane(pane(1))).is_some()
            );
        }
    }
}
#[test]
fn tall_automatic_views_fit_rows_and_keep_the_twelve_row_cap() {
    for height in [200, 202, 4000] {
        let mut layout =
            TileLayout::from_tree(view_group(Direction::Vertical), LayoutLeaf::Pane(pane(1)))
                .unwrap();
        let area = Rect::new(0, 0, 100, height);
        for rows in [1, 2, 500] {
            let measures =
                HashMap::from([(pane(2), crate::view_sizing::ViewMeasure { rows, width: 40 })]);
            layout.auto_size_views(area, &measures);
            let view = layout
                .geometry(area)
                .panes
                .into_iter()
                .find(|info| info.id == pane(2))
                .unwrap();
            assert_eq!(
                view.rect.height,
                rows.saturating_add(2).min(12),
                "height {height}, rows {rows}"
            );
            assert!(!layout.auto_size_views(area, &measures));
        }
    }
}
#[test]
fn standalone_view_is_full_framed_without_a_parent_group() {
    let layout = TileLayout::from_tree(
        TypedNode::view(pane(2), "vqro.tasks", "tasks"),
        LayoutLeaf::Pane(pane(2)),
    )
    .unwrap();
    let geometry = layout.geometry(Rect::new(0, 0, 80, 20));
    assert!(geometry.boxes.is_empty());
    assert_eq!(geometry.panes[0].frame_policy, PaneFramePolicy::Full);
}
