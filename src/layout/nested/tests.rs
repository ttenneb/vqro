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
    let target_tab = state.workspaces[0]
        .tabs
        .iter()
        .position(|tab| tab.panes.len() > 1)
        .expect("adversarial state must include a multi-pane tab");
    let mut panes: Vec<_> = state.workspaces[0].tabs[target_tab]
        .panes
        .keys()
        .copied()
        .collect();
    panes.sort_by_key(|pane| pane.raw());
    assert!(panes.len() >= 2, "move/reorder fence requires two panes");
    let identities: Vec<_> = panes
        .iter()
        .map(|pane| {
            (
                *pane,
                state.workspaces[0].public_pane_number(*pane),
                state.workspaces[0].tabs[target_tab].panes[pane]
                    .attached_terminal_id
                    .clone(),
            )
        })
        .collect();
    let assert_identities = |state: &crate::app::state::AppState| {
        state.assert_invariants_for_test();
        let mut placed = state.workspaces[0].tabs[target_tab].layout.pane_ids();
        placed.sort_by_key(|pane| pane.raw());
        assert_eq!(
            placed, panes,
            "raw pane identities must survive every mutation"
        );
        for (pane, number, terminal) in &identities {
            assert_eq!(state.workspaces[0].public_pane_number(*pane), *number);
            assert_eq!(
                &state.workspaces[0].tabs[target_tab].panes[pane].attached_terminal_id,
                terminal
            );
        }
    };
    let children = panes.iter().copied().map(TypedNode::Leaf).collect();
    state.workspaces[0].tabs[target_tab].layout =
        TileLayout::from_tree(stack(700_001, 0, children), LayoutLeaf::Pane(panes[0])).unwrap();
    assert_identities(&state);
    assert!(state.workspaces[0].tabs[target_tab]
        .layout
        .new_stack_at(&[0], box_id(700_002)));
    assert_identities(&state);
    assert!(state.workspaces[0].tabs[target_tab]
        .layout
        .move_into_stack(&[1], box_id(700_002), 1));
    assert_identities(&state);
    assert!(state.workspaces[0].tabs[target_tab]
        .layout
        .reorder_stack_child(box_id(700_002), 0, 1));
    assert_identities(&state);
    state.workspaces[0].tabs[target_tab]
        .layout
        .focus_pane(panes[1]);
    assert_identities(&state);
}

fn view_node(raw: u32, extension: &str, id: &str) -> TypedNode {
    TypedNode::view(PaneId::from_raw(raw), extension, id)
}

#[test]
fn without_view_panes_promotes_the_covered_terminal_into_the_view_box() {
    // split[ view(9), pane(1) ]: dropping the view promotes pane(1) to fill the
    // whole tab, exactly its pre-view box. Focus lands on the surviving terminal.
    let tree = layout(split(view_node(9, "vqro.tasks", "tasks"), pane(1)), 9);
    let projection = tree.without_view_panes();
    assert!(projection.view_panes().is_empty());
    assert_eq!(projection.pane_ids(), vec![PaneId::from_raw(1)]);
    assert_eq!(projection.focused(), PaneId::from_raw(1));
    let rects = projection.panes(Rect::new(0, 0, 80, 24));
    assert_eq!(rects.len(), 1);
    assert_eq!(
        rects[0].rect,
        Rect::new(0, 0, 80, 24),
        "the covered terminal fills the view's box with no hole"
    );

    // split[ split[ view(9), pane(1) ], pane(2) ]: only the view's own subtree is
    // promoted; the unrelated sibling keeps its geometry.
    let tree = layout(
        split(split(view_node(9, "vqro.tasks", "tasks"), pane(1)), pane(2)),
        9,
    );
    let projection = tree.without_view_panes();
    assert!(projection.view_panes().is_empty());
    assert_eq!(
        projection.pane_ids(),
        vec![PaneId::from_raw(1), PaneId::from_raw(2)]
    );
    let rects = projection.panes(Rect::new(0, 0, 80, 24));
    assert_eq!(rects[0].rect, Rect::new(0, 0, 40, 24));
    assert_eq!(rects[1].rect, Rect::new(40, 0, 40, 24));
}

#[test]
fn without_view_panes_on_a_view_free_layout_is_the_same_layout() {
    let tree = layout(split(pane(1), pane(2)), 1);
    assert_same(&tree.without_view_panes(), &tree);
}

#[test]
fn view_scope_covers_every_terminal_in_its_parent_box() {
    // split[ view(9), stack[1,2,3] ]: the view's parent is the split, so its
    // scope is all three terminals, including the two hidden stack members.
    let root = split(
        view_node(9, "vqro.tasks", "tasks"),
        stack(1, 0, vec![pane(1), pane(2), pane(3)]),
    );
    let tree = layout(root, 9);
    assert_eq!(
        tree.view_scope(PaneId::from_raw(9)),
        vec![
            PaneId::from_raw(1),
            PaneId::from_raw(2),
            PaneId::from_raw(3)
        ]
    );
}

#[test]
fn view_scope_uses_the_nearest_parent_container_not_the_root() {
    // split[ split[ view(9), pane(1) ], pane(2) ]: the view's own box holds only
    // pane(1), so pane(2) in the outer box is out of scope.
    let root = split(split(view_node(9, "vqro.tasks", "tasks"), pane(1)), pane(2));
    let tree = layout(root, 9);
    assert_eq!(
        tree.view_scope(PaneId::from_raw(9)),
        vec![PaneId::from_raw(1)]
    );
}

#[test]
fn lone_view_has_no_scope_and_is_still_one_pane() {
    let tree = layout(view_node(9, "vqro.tasks", "tasks"), 9);
    assert!(tree.view_scope(PaneId::from_raw(9)).is_empty());
    assert_eq!(tree.pane_count(), 1);
    assert_eq!(tree.focused(), PaneId::from_raw(9));
    assert!(tree.typed_root().valid_structure());
}

#[test]
fn split_pane_with_view_inserts_a_content_bearing_leaf_with_no_terminal() {
    let target = PaneId::alloc();
    let mut tree = layout(pane(target.raw()), target.raw());
    let (view_pane, content) = tree
        .split_pane_with_view(target, Direction::Horizontal, 0.5, "vqro.tasks", "tasks")
        .expect("valid view identity is admitted");
    assert_ne!(view_pane, target);
    assert_eq!(content.extension, "vqro.tasks");
    assert_eq!(content.view, "tasks");
    assert_eq!(
        tree.view_at(view_pane).map(|view| view.view),
        Some("tasks".to_owned())
    );
    assert!(tree.root.contains_pane(view_pane));
    assert_eq!(tree.pane_count(), 2);
    assert_eq!(
        tree.view_scope(view_pane),
        vec![target],
        "the new view covers the pane it was split beside"
    );
    assert!(tree.typed_root().valid_structure());
    assert_eq!(tree.focused(), target);
}

#[test]
fn split_pane_with_view_rejects_an_invalid_identity_and_changes_nothing() {
    let mut tree = layout(pane(1), 1);
    let before = tree.root.clone();
    assert!(tree
        .split_pane_with_view(
            PaneId::from_raw(1),
            Direction::Horizontal,
            0.5,
            "bad extension",
            "tasks"
        )
        .is_none());
    assert!(tree
        .split_pane_with_view(
            PaneId::from_raw(1),
            Direction::Horizontal,
            0.5,
            "vqro.tasks",
            ""
        )
        .is_none());
    assert_eq!(tree.root, before);
    assert_eq!(tree.pane_count(), 1);
}

#[test]
fn view_pane_participates_in_focus_close_and_structure_uniqueness() {
    let mut tree = layout(split(pane(1), view_node(9, "vqro.tasks", "tasks")), 1);
    let view_pane = PaneId::from_raw(9);
    tree.focus_pane(view_pane);
    assert_eq!(tree.focused(), view_pane);
    assert!(tree.typed_root().valid_structure());
    assert!(tree.close_pane(view_pane));
    assert_eq!(tree.pane_count(), 1);
    assert!(!tree.root.contains_pane(view_pane));
}

#[test]
fn swap_moves_a_views_content_and_identity_with_it() {
    let terminal = PaneId::alloc();
    let view_pane = PaneId::alloc();
    let mut tree = layout(
        split(
            pane(terminal.raw()),
            view_node(view_pane.raw(), "vqro.tasks", "tasks"),
        ),
        terminal.raw(),
    );
    assert!(tree.swap_panes(terminal, view_pane));
    let root = tree.typed_root();
    let TypedNode::Split { first, second, .. } = root else {
        panic!("expected the split");
    };
    // The view travels with its identity; the terminal keeps its own.
    assert!(matches!(**first, TypedNode::View { pane, .. } if pane == view_pane));
    assert!(matches!(**second, TypedNode::Leaf(pane) if pane == terminal));
    assert_eq!(
        tree.view_at(view_pane).map(|content| content.view),
        Some("tasks".to_owned()),
        "the view still has its content"
    );
    assert!(tree.view_at(terminal).is_none());
    assert_eq!(
        tree.view_scope(view_pane),
        vec![terminal],
        "the view now covers the terminal beside it"
    );
    assert_eq!(tree.pane_count(), 2);
    assert!(tree.typed_root().valid_structure());
}

#[test]
fn swap_moves_two_differently_configured_views_with_their_contents() {
    let first_view = PaneId::alloc();
    let second_view = PaneId::alloc();
    let mut tree = layout(
        split(
            view_node(first_view.raw(), "vqro.tasks", "tasks"),
            view_node(second_view.raw(), "vqro.tasks", "messages"),
        ),
        first_view.raw(),
    );
    assert!(tree.swap_panes(first_view, second_view));
    let TypedNode::Split { first, second, .. } = tree.typed_root() else {
        panic!("expected the split");
    };
    // The contents moved with their identities, not into each other.
    assert!(matches!(**first, TypedNode::View { pane, .. } if pane == second_view));
    assert!(matches!(**second, TypedNode::View { pane, .. } if pane == first_view));
    assert_eq!(
        tree.view_at(first_view).map(|content| content.view),
        Some("tasks".to_owned())
    );
    assert_eq!(
        tree.view_at(second_view).map(|content| content.view),
        Some("messages".to_owned())
    );
    assert!(tree.typed_root().valid_structure());
}
