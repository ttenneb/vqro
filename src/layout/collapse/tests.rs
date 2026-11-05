use super::*;
use crate::layout::{LayoutLeaf, TypedNode};
use ratatui::layout::{Direction, Rect};

fn pane(id: u32) -> TypedNode {
    TypedNode::Leaf(PaneId::from_raw(id))
}
fn split(direction: Direction, first: TypedNode, second: TypedNode) -> TypedNode {
    TypedNode::Split {
        direction,
        ratio: 0.7,
        first: Box::new(first),
        second: Box::new(second),
    }
}
fn layout(root: TypedNode) -> TileLayout {
    TileLayout::from_tree(root, LayoutLeaf::Pane(PaneId::from_raw(1))).unwrap()
}

#[test]
fn expanded_membership_rejects_leaf_and_ancestor_collapse_but_keeps_stack_selection_separate() {
    let mut tree = layout(TypedNode::Stack {
        id: crate::layout::CollectionId::from_raw(9).unwrap().into(),
        selected: 0,
        children: vec![pane(1), pane(2)],
    });
    assert!(tree.pane_is_expanded(PaneId::from_raw(2)));
    assert!(!tree.is_pane_visible(PaneId::from_raw(2)));
    assert!(tree.set_collapsed(&[1], true));
    assert!(!tree.pane_is_expanded(PaneId::from_raw(2)));
    assert!(tree.pane_is_expanded(PaneId::from_raw(1)));
    assert!(tree.set_collapsed(&[], true));
    assert!(!tree.pane_is_expanded(PaneId::from_raw(1)));
    assert!(!tree.pane_is_expanded(PaneId::from_raw(999)));
}

#[test]
fn collapse_is_idempotent_and_does_not_change_identity_membership_or_ratios() {
    let mut tree = layout(split(Direction::Vertical, pane(1), pane(2)));
    let before = tree.clone();
    assert!(tree.set_collapsed(&[1], true));
    assert!(!tree.set_collapsed(&[1], true));
    assert_eq!(tree.pane_ids(), before.pane_ids());
    assert_eq!(tree.focused_leaf(), before.focused_leaf());
    assert_eq!(
        crate::layout::typed_split_ratios(tree.typed_root()),
        crate::layout::typed_split_ratios(before.typed_root())
    );
    assert!(tree.typed_root().valid_structure());
    assert!(tree.set_collapsed(&[1], false));
    assert_eq!(tree.typed_root(), before.typed_root());
    assert!(!tree.set_collapsed(&[2], true));
}

#[test]
fn collapsed_body_is_not_visible_but_its_terminal_still_belongs_to_view_scope() {
    let mut tree = layout(split(
        Direction::Vertical,
        pane(1),
        TypedNode::view(PaneId::from_raw(2), "sample.views", "tasks"),
    ));
    assert!(tree.set_collapsed(&[0], true));
    assert!(!tree.is_pane_visible(PaneId::from_raw(1)));
    assert!(!tree.any_visible_pane(|pane| pane == PaneId::from_raw(1)));
    assert_eq!(
        tree.view_scope(PaneId::from_raw(2)),
        vec![PaneId::from_raw(1)]
    );
    let mut terminals = Vec::new();
    tree.typed_root().collect_terminal_panes(&mut terminals);
    assert_eq!(terminals, vec![PaneId::from_raw(1)]);
}

#[test]
fn collapsed_node_releases_axis_space_and_expand_restores_exact_geometry() {
    for direction in [Direction::Horizontal, Direction::Vertical] {
        let mut tree = layout(split(direction, pane(1), pane(2)));
        let area = Rect::new(3, 4, 100, 40);
        let before = tree.geometry(area).panes[0].inner_rect;
        assert!(tree.set_collapsed(&[1], true));
        let geometry = tree.geometry(area);
        assert_eq!(geometry.panes.len(), 1);
        assert_eq!(geometry.collapsed.len(), 1);
        let header = &geometry.collapsed[0];
        assert_eq!(
            header.path,
            vec![LayoutPathStep::Split(LayoutSplitChild::Second)]
        );
        assert_eq!(header.direction, direction);
        match direction {
            Direction::Horizontal => {
                assert_eq!(header.rect.width, 1);
                assert_eq!(geometry.panes[0].inner_rect.width, area.width - 1);
            }
            Direction::Vertical => {
                assert_eq!(header.rect.height, 1);
                assert_eq!(geometry.panes[0].inner_rect.height, area.height - 1);
            }
        }
        assert!(tree.set_collapsed(&[1], false));
        assert_eq!(tree.geometry(area).panes[0].inner_rect, before);
    }
}

#[test]
fn nested_collapse_and_close_promotion_coalesce_without_losing_the_survivor() {
    let mut tree = layout(split(
        Direction::Vertical,
        pane(1),
        split(Direction::Vertical, pane(2), pane(3)),
    ));
    assert!(tree.set_collapsed(&[1, 1], true));
    assert!(tree.set_collapsed(&[1], true));
    assert_eq!(tree.geometry(Rect::new(0, 0, 100, 40)).collapsed.len(), 1);
    assert!(tree.remove_node_at(&[1, 0]));
    assert_eq!(
        tree.pane_ids(),
        vec![PaneId::from_raw(1), PaneId::from_raw(3)]
    );
    assert!(tree.typed_root().valid_structure());
    assert!(tree.set_collapsed(&[1], false));
    assert!(tree.is_pane_visible(PaneId::from_raw(3)));
}

#[test]
fn collapsed_stack_paths_keep_selection_and_hidden_members() {
    let id = crate::terminal_container::TerminalContainerId::parse(912_001).unwrap();
    let mut tree = layout(TypedNode::Stack {
        id,
        selected: 0,
        children: vec![pane(1), split(Direction::Vertical, pane(2), pane(3))],
    });
    assert!(tree.set_collapsed(&[1, 0], true));
    assert_eq!(tree.pane_count(), 3);
    assert!(tree.select_stack_child(id, 1));
    assert!(!tree.is_pane_visible(PaneId::from_raw(2)));
    assert!(tree.is_pane_visible(PaneId::from_raw(3)));
    assert_eq!(
        tree.geometry(Rect::new(0, 0, 100, 40)).collapsed[0].path,
        vec![
            LayoutPathStep::StackChild(1),
            LayoutPathStep::Split(LayoutSplitChild::First)
        ]
    );
    assert!(tree.typed_root().valid_structure());
}

#[test]
fn collapsed_root_and_tiny_both_collapsed_children_never_escape_the_viewport() {
    for direction in [Direction::Horizontal, Direction::Vertical] {
        let mut tree = layout(split(direction, pane(1), pane(2)));
        assert!(tree.set_collapsed(&[0], true));
        assert!(tree.set_collapsed(&[1], true));
        for width in 0..5 {
            for height in 0..5 {
                let area = Rect::new(3, 4, width, height);
                let geometry = tree.geometry(area);
                assert!(geometry.panes.is_empty());
                for header in geometry.collapsed {
                    assert!(header.rect.x >= area.x && header.rect.right() <= area.right());
                    assert!(header.rect.y >= area.y && header.rect.bottom() <= area.bottom());
                }
            }
        }
        assert!(tree.set_collapsed(&[], true));
        let geometry = tree.geometry(Rect::new(0, 0, 100, 40));
        assert!(geometry.panes.is_empty());
        assert_eq!(geometry.collapsed.len(), 1);
        assert_eq!(geometry.collapsed[0].rect.height, 1);
    }
}

#[test]
fn compact_children_do_not_overlap_existing_stack_headers() {
    for direction in [Direction::Horizontal, Direction::Vertical] {
        for selected_child in [0, 1] {
            let id = crate::terminal_container::TerminalContainerId::parse(916_001).unwrap();
            let mut tree = layout(TypedNode::Stack {
                id,
                selected: 0,
                children: vec![split(direction, pane(1), pane(2))],
            });
            assert!(tree.set_collapsed(&[0, selected_child], true));
            let area = Rect::new(0, 0, 100, 40);
            let geometry = tree.geometry(area);
            assert_eq!(geometry.leaf_rects[&LayoutLeaf::Stack(id)], area);
            assert!(
                geometry.collapsed[0].rect.y > area.y,
                "compact child must not cover stack controls"
            );
            assert!(geometry.panes.iter().all(|pane| pane.inner_rect.y > area.y));
            assert!(
                tree.selected_stack_panes().is_empty(),
                "already reserved structural header must not be charged twice"
            );
        }
    }
}

#[test]
fn collapse_does_not_buy_box_depth_and_duplicate_wrappers_are_invalid() {
    let root = (0..4).fold(pane(1), |node, index| TypedNode::Stack {
        id: crate::terminal_container::TerminalContainerId::parse(913_001 + index).unwrap(),
        selected: 0,
        children: vec![node],
    });
    let mut tree = layout(root);
    assert!(tree.set_collapsed(&[], true));
    let before = tree.typed_root().clone();
    assert!(!tree.new_stack_at(
        &[0, 0, 0, 0],
        crate::terminal_container::TerminalContainerId::parse(913_099).unwrap()
    ));
    assert_eq!(tree.typed_root(), &before);
    let duplicate = TypedNode::Collapsed {
        node: Box::new(TypedNode::Collapsed {
            node: Box::new(pane(1)),
        }),
    };
    assert!(!duplicate.valid_structure());
}

#[test]
fn collapsed_view_content_changes_do_not_rewrite_expanded_share_memory() {
    let mut tree = layout(split(
        Direction::Vertical,
        pane(1),
        TypedNode::view(PaneId::from_raw(2), "sample.views", "tasks"),
    ));
    let area = Rect::new(0, 0, 100, 40);
    let mut measures = std::collections::HashMap::from([(
        PaneId::from_raw(2),
        crate::view_sizing::ViewMeasure { rows: 2, width: 32 },
    )]);
    tree.auto_size_views(area, &measures);
    let before = crate::layout::typed_split_ratios(tree.typed_root());
    assert!(tree.set_collapsed(&[1], true));
    measures.get_mut(&PaneId::from_raw(2)).unwrap().rows = 200;
    assert!(!tree.auto_size_views(area, &measures));
    assert_eq!(crate::layout::typed_split_ratios(tree.typed_root()), before);
    assert!(tree.set_collapsed(&[1], false));
    assert_eq!(crate::layout::typed_split_ratios(tree.typed_root()), before);
}

#[test]
fn tree_fingerprint_covers_hidden_structure_selection_shares_and_collapse() {
    let id = crate::terminal_container::TerminalContainerId::parse(914_001).unwrap();
    let mut tree = layout(split(
        Direction::Vertical,
        pane(1),
        TypedNode::Stack {
            id,
            selected: 0,
            children: vec![pane(2), pane(3)],
        },
    ));
    let original = tree.tree_fingerprint().to_owned();
    assert_eq!(original, tree.authoritative_fingerprint());
    assert!(tree.select_stack_child(id, 1));
    assert_ne!(tree.tree_fingerprint(), original);
    assert_eq!(tree.tree_fingerprint(), tree.authoritative_fingerprint());
    let selected = tree.tree_fingerprint().to_owned();
    assert!(tree.set_collapsed(&[1, 0], true));
    assert_ne!(tree.tree_fingerprint(), selected);
    assert_eq!(tree.tree_fingerprint(), tree.authoritative_fingerprint());
    assert!(tree.set_collapsed(&[1, 0], false));
    assert_eq!(tree.tree_fingerprint(), selected);
    assert!(tree.set_ratio_at(&[], 0.6));
    assert_ne!(tree.tree_fingerprint(), selected);
    assert_eq!(tree.tree_fingerprint(), tree.authoritative_fingerprint());
}

#[test]
fn adversarial_collapse_keeps_public_and_terminal_identity_authority() {
    let mut state = crate::app::state::AppState::test_with_adversarial_identity_state();
    state.assert_invariants_for_test();
    for workspace in &mut state.workspaces {
        for tab in &mut workspace.tabs {
            let before = tab.layout.pane_ids();
            assert!(tab.layout.set_collapsed(&[], true));
            assert_eq!(tab.layout.pane_ids(), before);
        }
    }
    state.assert_invariants_for_test();
    for workspace in &mut state.workspaces {
        for tab in &mut workspace.tabs {
            assert!(tab.layout.set_collapsed(&[], false));
        }
    }
    state.assert_invariants_for_test();
}
