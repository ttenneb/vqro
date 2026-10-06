use super::*;
use ratatui::layout::{Direction, Rect};

fn pane(raw: u32) -> TypedNode {
    TypedNode::Leaf(PaneId::from_raw(raw))
}
fn box_id(raw: u64) -> TerminalContainerId {
    TerminalContainerId::parse(raw).unwrap()
}
fn stack(raw: u64, selected: usize, children: Vec<TypedNode>) -> TypedNode {
    TypedNode::Stack {
        id: box_id(raw),
        selected,
        children,
    }
}
fn split(first: TypedNode, second: TypedNode) -> TypedNode {
    TypedNode::Split {
        direction: Direction::Horizontal,
        ratio: 0.5,
        first: Box::new(first),
        second: Box::new(second),
    }
}
fn layout(root: TypedNode, focus: u32) -> TileLayout {
    TileLayout::from_tree(root, LayoutLeaf::Pane(PaneId::from_raw(focus))).unwrap()
}
fn assert_same(left: &TileLayout, right: &TileLayout) {
    assert_eq!(left.root, right.root);
    assert_eq!(left.focus, right.focus);
    assert_eq!(left.prev_focus, right.prev_focus);
    assert_eq!(left.pane_ids(), right.pane_ids());
}
fn chain(levels: usize, node: TypedNode) -> TypedNode {
    (0..levels).fold(node, |node, level| stack(100 + level as u64, 0, vec![node]))
}

#[test]
fn nested_stack_reparent_preserves_subtree_identity_selection_and_focus() {
    let root = split(
        stack(1, 1, vec![pane(1), split(pane(2), pane(3))]),
        stack(2, 0, vec![pane(4)]),
    );
    let mut tree = layout(root, 2);
    let old_subtree = tree.root.at(&[0, 1]).unwrap().clone();
    assert!(tree.move_into_stack(&[0, 1], box_id(2), 1));
    assert_eq!(tree.root.at(&[1, 1]), Some(&old_subtree));
    assert_eq!(tree.focused(), PaneId::from_raw(2));
    assert_eq!(
        tree.prev_focus, None,
        "structural edits preserve focus history"
    );
    assert_eq!(
        tree.pane_ids(),
        vec![
            PaneId::from_raw(1),
            PaneId::from_raw(4),
            PaneId::from_raw(2),
            PaneId::from_raw(3)
        ]
    );
    assert!(tree.root.valid_structure());
}

#[test]
fn nested_reparent_overflow_without_allocating_any_box_is_atomic() {
    // Destination depth four. Moving an existing split beneath it makes depth
    // five; no box is allocated by this operation, so allocation-only checks miss it.
    let root = split(
        split(split(pane(1), pane(2)), pane(3)),
        chain(2, TypedNode::empty_stack(box_id(3))),
    );
    let mut tree = layout(root, 2);
    let before = tree.clone();
    let ids = tree.collection_ids();
    assert!(!tree.move_into_stack(&[0], box_id(3), 0));
    assert_same(&tree, &before);
    assert_eq!(tree.collection_ids(), ids);
}

#[test]
fn nested_stack_and_split_levels_both_count_excluding_tab_root() {
    let mut tree = layout(split(pane(1), chain(3, pane(2))), 1);
    let before = tree.clone();
    // Split + three stacks = four; another box at pane(2) would be five.
    assert!(!tree.new_stack_at(&[1, 0, 0, 0], box_id(4)));
    assert_same(&tree, &before);
    assert!(tree.new_stack_at(&[0], box_id(4)));
    assert!(tree.root.valid_structure());
}

#[test]
fn nested_insertion_overflow_and_duplicate_identity_are_atomic() {
    let mut tree = layout(chain(4, pane(1)), 1);
    let before = tree.clone();
    assert!(!tree.insert_into_stack(box_id(100), 1, split(pane(2), pane(3))));
    assert_same(&tree, &before);
    assert!(!tree.insert_into_stack(box_id(100), 1, pane(1)));
    assert_same(&tree, &before);
    assert!(!tree.new_stack_at(&[], box_id(100)));
    assert_same(&tree, &before);
    assert!(!tree.insert_into_stack(box_id(100), 9, pane(2)));
    assert_same(&tree, &before);
    assert!(tree.insert_into_stack(box_id(100), 1, pane(2)));
}

#[test]
fn grandfathered_restore_close_and_non_growing_move_stay_allowed() {
    let root = split(
        chain(5, split(pane(1), pane(2))),
        TypedNode::empty_stack(box_id(3)),
    );
    // The pure constructor, like restore, does not enforce depth.
    let mut tree = layout(root, 1);
    assert!(tree.remove_node_at(&[0, 0, 0, 0, 0, 0, 1]));
    assert!(tree.move_into_stack(&[0, 0, 0, 0, 0, 0], box_id(3), 0));
    assert!(tree.root.valid_structure());
    assert_eq!(tree.focused(), PaneId::from_raw(1));
}

#[test]
fn grandfathered_unrelated_deep_branch_cannot_mask_new_overflow() {
    let destination = stack(
        200,
        0,
        vec![stack(
            201,
            0,
            vec![stack(202, 0, vec![TypedNode::empty_stack(box_id(3))])],
        )],
    );
    let root = split(chain(6, pane(1)), split(pane(2), destination));
    let mut tree = layout(root, 2);
    let before = tree.clone();
    // Destination is already five levels deep; grandfathering that box does not
    // allow a shallower pane to become over-cap merely because another branch is deep.
    assert!(!tree.move_into_stack(&[1, 0], box_id(3), 0));
    assert_same(&tree, &before);
}

#[test]
fn nested_reparent_cannot_move_parent_into_descendant() {
    let mut tree = layout(stack(1, 0, vec![stack(2, 0, vec![pane(1)]), pane(2)]), 1);
    let before = tree.clone();
    assert!(!tree.move_into_stack(&[0], box_id(2), 0));
    assert_same(&tree, &before);
    assert!(!tree.move_into_stack(&[], box_id(2), 0));
    assert_same(&tree, &before);
}

#[test]
fn selected_child_is_an_index_including_idless_split_and_repairs_next_then_previous() {
    let mut tree = layout(
        stack(1, 1, vec![pane(1), split(pane(2), pane(3)), pane(4)]),
        2,
    );
    assert!(tree.reorder_stack_child(box_id(1), 1, 2));
    assert!(matches!(tree.root, TypedNode::Stack { selected: 2, .. }));
    assert_eq!(tree.focused(), PaneId::from_raw(2));
    assert!(tree.remove_node_at(&[0]));
    assert!(matches!(tree.root, TypedNode::Stack { selected: 1, .. }));
    assert!(tree.remove_node_at(&[1]));
    assert!(matches!(tree.root, TypedNode::Stack { selected: 0, .. }));
    assert_eq!(tree.focused(), PaneId::from_raw(4));
    assert!(tree.remove_node_at(&[0]));
    assert!(
        matches!(tree.root, TypedNode::Stack { selected: 0, ref children, .. } if children.is_empty())
    );
    assert!(tree.root.valid_structure());
}

#[test]
fn selected_child_removal_prefers_next_over_previous() {
    let mut tree = layout(stack(1, 1, vec![pane(1), pane(2), pane(3)]), 2);
    assert!(tree.remove_node_at(&[1]));
    assert_eq!(tree.focused(), PaneId::from_raw(3));
    assert!(matches!(tree.root, TypedNode::Stack { selected: 1, .. }));
}

#[test]
fn same_stack_move_preserves_selected_child_and_rejects_bad_indices() {
    let mut tree = layout(
        stack(1, 1, vec![pane(1), split(pane(2), pane(3)), pane(4)]),
        2,
    );
    assert!(tree.move_into_stack(&[1], box_id(1), 0));
    assert!(matches!(tree.root, TypedNode::Stack { selected: 0, .. }));
    let before = tree.clone();
    assert!(!tree.move_into_stack(&[0], box_id(1), 3));
    assert_same(&tree, &before);
}

#[test]
fn nested_invalid_selection_or_duplicate_boxes_are_rejected() {
    assert!(
        TileLayout::from_tree(stack(1, 2, vec![pane(1)]), LayoutLeaf::Stack(box_id(1))).is_none()
    );
    assert!(TileLayout::from_tree(stack(1, 1, vec![]), LayoutLeaf::Stack(box_id(1))).is_none());
    assert!(TileLayout::from_tree(
        stack(1, 0, vec![stack(1, 0, vec![pane(1)])]),
        LayoutLeaf::Stack(box_id(1))
    )
    .is_none());
    assert!(TileLayout::from_tree(
        stack(1, 0, vec![pane(1), pane(1)]),
        LayoutLeaf::Stack(box_id(1))
    )
    .is_none());
}

#[test]
fn nested_geometry_and_selected_visibility_preserve_geometric_leaf_focus() {
    let mut tree = layout(
        split(stack(1, 0, vec![split(pane(1), pane(2)), pane(3)]), pane(4)),
        2,
    );
    let panes = tree.panes(Rect::new(0, 0, 120, 40));
    assert_eq!(tree.selected_stack_panes(), vec![PaneId::from_raw(1)]);
    assert_eq!(
        panes.iter().map(|pane| pane.id.raw()).collect::<Vec<_>>(),
        vec![1, 2, 4]
    );
    assert_eq!(panes[0].rect, Rect::new(0, 0, 30, 40));
    assert_eq!(panes[1].rect, Rect::new(30, 0, 30, 40));
    assert!(panes[1].is_focused);
    assert_eq!(
        super::super::find_in_direction(&panes[1], super::super::NavDirection::Right, &panes),
        Some(PaneId::from_raw(4))
    );
    assert!(tree.select_stack_child(box_id(1), 1));
    assert_eq!(tree.selected_stack_panes(), vec![PaneId::from_raw(3)]);
    assert_eq!(tree.focused(), PaneId::from_raw(3));
    assert!(tree.panes(Rect::new(0, 0, 120, 40))[0].is_focused);
    assert_eq!(
        tree.panes(Rect::new(0, 0, 120, 40))
            .iter()
            .map(|pane| pane.id.raw())
            .collect::<Vec<_>>(),
        vec![3, 4]
    );
}

#[test]
fn nested_adversarial_app_mutations_keep_identity_and_terminal_authority_without_ptys() {
    let mut state = crate::app::state::AppState::test_with_adversarial_identity_state();
    state.assert_invariants_for_test();
    let active = state.workspaces[0].active_tab;
    let mut panes: Vec<_> = state.workspaces[0].tabs[active]
        .panes
        .keys()
        .copied()
        .collect();
    panes.sort_by_key(|pane| pane.raw());
    let identities: Vec<_> = panes
        .iter()
        .map(|pane| {
            (
                *pane,
                state.workspaces[0].public_pane_number(*pane),
                state.workspaces[0].tabs[active].panes[pane]
                    .attached_terminal_id
                    .clone(),
            )
        })
        .collect();
    let children = panes.iter().copied().map(TypedNode::Leaf).collect();
    state.workspaces[0].tabs[active].layout =
        TileLayout::from_tree(stack(700_001, 0, children), LayoutLeaf::Pane(panes[0])).unwrap();
    state.assert_invariants_for_test();
    assert!(state.workspaces[0].tabs[active]
        .layout
        .new_stack_at(&[0], box_id(700_002)));
    state.assert_invariants_for_test();
    if panes.len() > 1 {
        assert!(state.workspaces[0].tabs[active]
            .layout
            .move_into_stack(&[1], box_id(700_002), 1));
        state.assert_invariants_for_test();
        assert!(state.workspaces[0].tabs[active]
            .layout
            .reorder_stack_child(box_id(700_002), 0, 1));
        state.assert_invariants_for_test();
        state.workspaces[0].tabs[active].layout.focus_pane(panes[1]);
        state.assert_invariants_for_test();
    }
    for (pane, number, terminal) in identities {
        assert_eq!(state.workspaces[0].public_pane_number(pane), number);
        assert_eq!(
            state.workspaces[0].tabs[active].panes[&pane].attached_terminal_id,
            terminal
        );
    }
}
