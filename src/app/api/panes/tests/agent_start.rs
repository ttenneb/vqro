use super::*;
use crate::layout::{PaneFramePolicy, TypedNode};
use ratatui::layout::Direction;

fn start(config: &Config) -> (App, PaneId) {
    let (_send, receive) = tokio::sync::mpsc::unbounded_channel();
    let mut app = App::new(
        config,
        crate::app::AppPolicy::TEST,
        None,
        receive,
        crate::api::EventHub::default(),
    );
    app.state.workspaces = vec![Workspace::test_new("start-box")];
    app.state.active = Some(0);
    app.state.selected = 0;
    app.state.ensure_test_terminals();
    let pane = app.state.workspaces[0].tabs[0].root_pane;
    (app, pane)
}

#[test]
fn default_box_respects_depth_per_identity_and_never_partially_commits() {
    for (depth, admitted) in [(2, true), (3, false)] {
        let (mut app, mut target) = start(&Config::default());
        for _ in 0..depth {
            target = app.state.workspaces[0].test_split(Direction::Horizontal);
        }
        app.state.ensure_test_terminals();
        let before = app.state.workspaces[0].tabs[0].layout.typed_root().clone();
        let next = app.state.workspaces[0].next_public_pane_number;
        app.maybe_add_agent_start_view(target);
        assert_eq!(
            app.state.workspaces[0].tabs[0].layout.view_panes().len(),
            if admitted { 2 } else { 0 }
        );
        assert_eq!(
            app.state.workspaces[0]
                .pane_state(target)
                .unwrap()
                .agent_start_view_applied,
            admitted
        );
        if !admitted {
            assert_eq!(app.state.workspaces[0].tabs[0].layout.typed_root(), &before);
            assert_eq!(app.state.workspaces[0].next_public_pane_number, next);
        }
        app.state.assert_invariants_for_test();
    }
}

#[test]
fn singular_input_remains_an_explicit_override_and_invalid_lists_are_atomic() {
    for (text, count) in [
        ("[ui]\nagent_start_view='example.one:summary'", 1),
        ("[ui]\nagent_start_view=''", 0),
        (
            "[ui]\nagent_start_views=['example.one:summary',':invalid']",
            0,
        ),
    ] {
        let config: Config = toml::from_str(text).unwrap();
        let (mut app, root) = start(&config);
        app.maybe_add_agent_start_view(root);
        assert_eq!(
            app.state.workspaces[0].tabs[0].layout.view_panes().len(),
            count
        );
        if count == 1 {
            assert!(matches!(
                app.state.workspaces[0].tabs[0].layout.typed_root(),
                TypedNode::Split {
                    direction: Direction::Vertical,
                    ..
                }
            ));
        }
        app.state.assert_invariants_for_test();
    }
}

#[test]
fn default_agent_start_creates_one_ordinary_terminal_free_views_box_below() {
    let (mut app, root) = start(&Config::default());
    app.maybe_add_agent_start_view(root);
    let layout = &app.state.workspaces[0].tabs[0].layout;
    let views = layout.view_panes();
    assert_eq!(views.len(), 2, "default-on Tasks and Messages");
    assert_eq!(layout.view_at(views[0]).unwrap().view, "tasks");
    assert_eq!(layout.view_at(views[1]).unwrap().view, "messages");
    assert!(views
        .iter()
        .all(|pane| layout.view_at(*pane).unwrap().extension == "vqro.tasks"));
    let TypedNode::Split {
        direction: Direction::Vertical,
        first,
        second,
        ..
    } = layout.typed_root()
    else {
        panic!("agent group below")
    };
    assert!(matches!(first.content(), TypedNode::Leaf(pane) if *pane == root));
    assert!(matches!(
        second.content(),
        TypedNode::Split {
            direction: Direction::Vertical,
            ..
        }
    ));
    let mut terminals = Vec::new();
    second.collect_terminal_panes(&mut terminals);
    assert!(terminals.is_empty());
    for view in &views {
        assert_eq!(layout.view_scope(*view), vec![root]);
    }
    let geometry = layout.geometry(Rect::new(0, 0, 120, 40));
    assert_eq!(geometry.boxes.len(), 1);
    assert_eq!(
        geometry
            .panes
            .iter()
            .find(|pane| pane.id == root)
            .unwrap()
            .frame_policy,
        PaneFramePolicy::Unframed
    );
    assert!(geometry
        .panes
        .iter()
        .filter(|pane| views.contains(&pane.id))
        .all(|pane| pane.frame_policy != PaneFramePolicy::Unframed));
    assert_eq!(app.state.terminals.len(), 1);
    assert_eq!(app.state.workspaces[0].tabs[0].panes.len(), 1);
    app.state.assert_invariants_for_test();
}

#[test]
fn closing_either_default_child_normalizes_and_never_reopens_it() {
    for first_index in [0, 1] {
        let (mut app, root) = start(&Config::default());
        app.maybe_add_agent_start_view(root);
        let views = app.state.workspaces[0].tabs[0].layout.view_panes();
        assert_eq!(views.len(), 2);
        let public = app.public_pane_id(0, views[first_index]).unwrap();
        assert!(app
            .close_pane("close-child".into(), &PaneTarget { pane_id: public })
            .is_ok());
        assert_eq!(
            app.state.workspaces[0].tabs[0].layout.view_panes(),
            vec![views[1 - first_index]]
        );
        app.maybe_add_agent_start_view(root);
        assert_eq!(app.state.workspaces[0].tabs[0].layout.view_panes().len(), 1);
        let public = app.public_pane_id(0, views[1 - first_index]).unwrap();
        assert!(app
            .close_pane("close-last".into(), &PaneTarget { pane_id: public })
            .is_ok());
        assert!(
            matches!(app.state.workspaces[0].tabs[0].layout.typed_root(), TypedNode::Leaf(pane) if *pane == root)
        );
        app.maybe_add_agent_start_view(root);
        assert_eq!(app.state.workspaces[0].tabs[0].layout.pane_count(), 1);
        app.state.assert_invariants_for_test();
    }
}

#[test]
fn default_box_needs_all_slots_or_leaves_tree_numbers_and_marker_unchanged() {
    let (mut app, root) = start(&Config::default());
    for _ in 1..crate::admission::MAX_TAB_PANES - 1 {
        app.state.workspaces[0].test_split(Direction::Horizontal);
    }
    app.state.ensure_test_terminals();
    let before = app.state.workspaces[0].tabs[0].layout.typed_root().clone();
    let numbers = app.state.workspaces[0].public_pane_numbers.clone();
    let next_number = app.state.workspaces[0].next_public_pane_number;
    app.maybe_add_agent_start_view(root);
    assert_eq!(app.state.workspaces[0].tabs[0].layout.typed_root(), &before);
    assert_eq!(app.state.workspaces[0].public_pane_numbers, numbers);
    assert_eq!(app.state.workspaces[0].next_public_pane_number, next_number);
    assert!(
        !app.state.workspaces[0]
            .pane_state(root)
            .unwrap()
            .agent_start_view_applied
    );
    app.state.assert_invariants_for_test();
}

#[test]
fn configurable_box_can_be_disabled_or_choose_ordered_views_without_product_literals() {
    for (text, expected) in [
        ("[ui]\nagent_start_views=[]", vec![]),
        (
            "[ui]\nagent_start_views=['example.first:summary','example.second:activity']",
            vec!["summary", "activity"],
        ),
    ] {
        let config: Config = toml::from_str(text).unwrap();
        let (mut app, root) = start(&config);
        app.maybe_add_agent_start_view(root);
        let layout = &app.state.workspaces[0].tabs[0].layout;
        let actual = layout
            .view_panes()
            .iter()
            .map(|pane| layout.view_at(*pane).unwrap().view)
            .collect::<Vec<_>>();
        assert_eq!(actual, expected);
        app.state.assert_invariants_for_test();
    }
}
