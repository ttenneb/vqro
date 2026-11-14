use super::*;
use crate::{config::Config, workspace::Workspace};
use ratatui::layout::{Direction, Rect};

#[test]
fn collapsed_sibling_navigation_preserves_adversarial_stack_content_and_terminal_identity() {
    use crate::layout::{LayoutLeaf, TileLayout, TypedNode};
    let (_send, receive) = tokio::sync::mpsc::unbounded_channel();
    let mut app = App::new(
        &Config::default(),
        crate::app::AppPolicy::TEST,
        None,
        receive,
        crate::api::EventHub::default(),
    );
    app.state = crate::app::AppState::test_with_adversarial_identity_state();
    app.state.view.terminal_area = Rect::new(0, 0, 120, 40);
    let workspace = &mut app.state.workspaces[0];
    let index = workspace
        .tabs
        .iter()
        .position(|tab| tab.panes.len() >= 3)
        .unwrap();
    workspace.switch_tab(index);
    let panes = workspace.tabs[index].layout.pane_ids();
    let selected = panes[1];
    let sibling = panes[2];
    let content = TypedNode::Stack {
        id: crate::terminal_container::TerminalContainerId::parse(912_003).unwrap(),
        selected: 1,
        children: vec![TypedNode::Leaf(panes[0]), TypedNode::Leaf(selected)],
    };
    workspace.tabs[index].layout = TileLayout::from_tree(
        TypedNode::Split {
            direction: Direction::Horizontal,
            ratio: 0.6,
            first: Box::new(content.clone()),
            second: Box::new(TypedNode::Leaf(sibling)),
        },
        LayoutLeaf::Pane(selected),
    )
    .unwrap();
    app.state.assert_invariants_for_test();
    assert!(app.set_node_collapsed(0, index, &[0], true));
    assert!(app.set_node_collapsed(0, index, &[1], true));
    let fingerprint = app.state.workspaces[0].tabs[index]
        .layout
        .tree_fingerprint()
        .to_owned();
    for _ in 0..3 {
        assert_eq!(
            app.directional_pane_focus_target(0, index, selected, PaneDirection::Right),
            Some(sibling)
        );
        app.state.focus_pane_in_workspace(0, sibling);
        assert_eq!(
            app.directional_pane_focus_target(0, index, sibling, PaneDirection::Left),
            Some(selected)
        );
        app.state.focus_pane_in_workspace(0, selected);
        let layout = &app.state.workspaces[0].tabs[index].layout;
        assert_eq!(layout.typed_root().at(&[0]).unwrap().content(), &content);
        assert_eq!(layout.tree_fingerprint(), fingerprint);
        assert!(!layout.pane_is_expanded(selected));
        assert!(!layout.pane_is_expanded(sibling));
        app.state.assert_invariants_for_test();
    }
    assert!(app.set_node_collapsed(0, index, &[0], false));
    assert_eq!(
        app.state.workspaces[0].tabs[index]
            .layout
            .typed_root()
            .at(&[0]),
        Some(&content)
    );
    app.state.assert_invariants_for_test();
}

#[test]
fn directional_focus_leaves_a_collapsed_leaf_or_box_via_its_header_geometry() {
    for (direction, navigation) in [
        (Direction::Horizontal, PaneDirection::Right),
        (Direction::Vertical, PaneDirection::Down),
    ] {
        for containing_box in [false, true] {
            let (_send, receive) = tokio::sync::mpsc::unbounded_channel();
            let mut app = App::new(
                &Config::default(),
                crate::app::AppPolicy::TEST,
                None,
                receive,
                crate::api::EventHub::default(),
            );
            app.state.workspaces = vec![Workspace::test_new("header-navigation")];
            app.state.active = Some(0);
            app.state.selected = 0;
            app.state.view.terminal_area = Rect::new(0, 0, 120, 40);
            let source = app.state.workspaces[0].tabs[0].root_pane;
            let sibling = app.state.workspaces[0].test_split(direction);
            if containing_box {
                app.state.workspaces[0]
                    .split_pane_with_view(
                        source,
                        "example.views",
                        "summary",
                        Direction::Vertical,
                        0.5,
                        false,
                    )
                    .unwrap();
            }
            app.state.ensure_test_terminals();
            let terminal_id = app.state.workspaces[0].terminal_id(source).unwrap().clone();
            let resume_plan = crate::agent_resume::AgentResumePlan {
                agent: "pi".into(),
                argv: vec!["pi".into()],
                dedupe_key: "focus-must-not-resume".into(),
            };
            app.state
                .terminals
                .get_mut(&terminal_id)
                .unwrap()
                .pending_agent_resume_plan = Some(resume_plan.clone());
            app.state.focus_pane_in_workspace(0, source);
            assert!(app.set_node_collapsed(0, 0, &[0], true));
            assert_eq!(
                app.directional_pane_target(0, 0, source, navigation),
                Some(sibling),
                "header/strip must be the navigation origin"
            );
            let source_public = app.public_pane_id(0, source).unwrap();
            let response = app.handle_pane_focus_direction(
                "navigate-header".into(),
                PaneFocusDirectionParams {
                    pane_id: Some(source_public.clone()),
                    direction: navigation,
                },
            );
            assert!(serde_json::from_str::<serde_json::Value>(&response)
                .unwrap()
                .get("error")
                .is_none());
            assert_eq!(app.state.workspaces[0].tabs[0].layout.focused(), sibling);
            let projected = app.pane_layout_snapshot(0, 0).unwrap();
            assert!(
                projected
                    .panes
                    .iter()
                    .all(|pane| pane.pane_id != source_public),
                "navigation must not manufacture a body descriptor"
            );
            assert!(!app.state.workspaces[0].tabs[0]
                .layout
                .pane_is_expanded(source));
            let reverse = if direction == Direction::Horizontal {
                PaneDirection::Left
            } else {
                PaneDirection::Up
            };
            let sibling_public = app.public_pane_id(0, sibling).unwrap();
            let before = app.state.workspaces[0].tabs[0].layout.typed_root().clone();
            let neighbor = app.handle_pane_neighbor(
                "neighbor-hidden".into(),
                PaneNeighborParams {
                    pane_id: Some(sibling_public.clone()),
                    direction: reverse,
                },
            );
            let neighbor: serde_json::Value = serde_json::from_str(&neighbor).unwrap();
            assert!(neighbor["result"]["neighbor"]["neighbor_pane_id"].is_null());
            let swapped = app.handle_pane_swap(
                "swap-hidden".into(),
                PaneSwapParams {
                    pane_id: Some(sibling_public.clone()),
                    direction: Some(reverse),
                    ..Default::default()
                },
            );
            let swapped: serde_json::Value = serde_json::from_str(&swapped).unwrap();
            assert_eq!(swapped["result"]["swap"]["reason"], "no_neighbor");
            assert_eq!(app.state.workspaces[0].tabs[0].layout.typed_root(), &before);
            let response = app.handle_pane_focus_direction(
                "refocus-header".into(),
                PaneFocusDirectionParams {
                    pane_id: Some(sibling_public),
                    direction: reverse,
                },
            );
            assert!(serde_json::from_str::<serde_json::Value>(&response)
                .unwrap()
                .get("error")
                .is_none());
            assert_eq!(
                app.state.workspaces[0].tabs[0].layout.focused(),
                source,
                "a collapsed strip must be reachable for its full focus label"
            );
            assert!(!app.state.workspaces[0].tabs[0]
                .layout
                .pane_is_expanded(source));
            assert!(
                app.state.workspaces[0].tabs[0]
                    .layout
                    .geometry(app.state.view.terminal_area)
                    .collapsed[0]
                    .focused
            );
            assert_eq!(
                app.state
                    .terminals
                    .get(&terminal_id)
                    .unwrap()
                    .pending_agent_resume_plan,
                Some(resume_plan)
            );
            assert!(
                app.terminal_runtimes.get(&terminal_id).is_none(),
                "header focus must not create a live terminal"
            );
            app.state.assert_invariants_for_test();
        }
    }
}
