use super::*;
use crate::api::schema::SuccessResponse;
use ratatui::layout::Direction;

fn setup() -> App {
    let (_send, receive) = tokio::sync::mpsc::unbounded_channel();
    let mut app = App::new(
        &crate::config::Config::default(),
        crate::app::AppPolicy::TEST,
        None,
        receive,
        crate::api::EventHub::default(),
    );
    let mut workspace = crate::workspace::Workspace::test_new("conditional collapse");
    workspace.test_split(Direction::Horizontal);
    app.state.workspaces = vec![workspace];
    app.state.active = Some(0);
    app.state.ensure_test_terminals();
    app
}
fn target(app: &mut App, path: Vec<LayoutPathStep>) -> LayoutCollapseTarget {
    let response = app.handle_layout_collapse_describe(
        "describe".into(),
        LayoutExportParams {
            tab_id: None,
            pane_id: None,
        },
    );
    let success: SuccessResponse = serde_json::from_str(&response).unwrap();
    let ResponseResult::LayoutCollapseDescription { collapse } = success.result else {
        panic!("describe")
    };
    LayoutCollapseTarget {
        boot_id: collapse.boot_id,
        tab_id: collapse.layout.tab_id,
        tree_fingerprint: collapse.tree_fingerprint,
        path,
    }
}
fn error(response: &str) -> String {
    serde_json::from_str::<serde_json::Value>(response).unwrap()["error"]["code"]
        .as_str()
        .unwrap()
        .into()
}
fn success(response: &str) {
    serde_json::from_str::<SuccessResponse>(response).unwrap();
}

#[tokio::test]
async fn racing_clients_cannot_retarget_a_stale_subtree_or_replay_across_incarnations() {
    let mut app = setup();
    let first = target(
        &mut app,
        vec![LayoutPathStep::Split(LayoutSplitChild::First)],
    );
    let second = target(
        &mut app,
        vec![LayoutPathStep::Split(LayoutSplitChild::Second)],
    );
    success(&app.handle_layout_collapse("first".into(), first, true));
    let after = app.state.workspaces[0].tabs[0].layout.typed_root().clone();
    assert_eq!(
        error(&app.handle_layout_collapse("second".into(), second, true)),
        "stale_layout_target"
    );
    assert_eq!(app.state.workspaces[0].tabs[0].layout.typed_root(), &after);
    let mut restart = target(&mut app, vec![]);
    restart.boot_id.push_str("-another-process");
    assert_eq!(
        error(&app.handle_layout_collapse("restart".into(), restart, true)),
        "stale_layout_target"
    );
    assert_eq!(app.state.workspaces[0].tabs[0].layout.typed_root(), &after);
    app.state.assert_invariants_for_test();
}

#[tokio::test]
async fn missed_projection_invalidation_cannot_authorize_a_different_tree() {
    let mut app = setup();
    let stale = target(
        &mut app,
        vec![LayoutPathStep::Split(LayoutSplitChild::Second)],
    );
    assert!(app.set_node_collapsed(0, 0, &[0], true));
    let layout = &mut app.state.workspaces[0].tabs[0].layout;
    let actual = layout.authoritative_fingerprint();
    layout.set_projection_fingerprint_for_test(stale.tree_fingerprint.clone());
    let before = layout.typed_root().clone();
    assert_eq!(
        error(&app.handle_layout_collapse("missed-invalidation".into(), stale, true)),
        "stale_layout_target"
    );
    assert_eq!(app.state.workspaces[0].tabs[0].layout.typed_root(), &before);
    app.state.workspaces[0].tabs[0]
        .layout
        .set_projection_fingerprint_for_test(actual);
    app.state.assert_invariants_for_test();
}

#[tokio::test]
async fn path_grammar_is_kind_checked_through_wrappers_and_selected_stack_children() {
    let mut app = setup();
    let id = crate::terminal_container::TerminalContainerId::parse(915_001).unwrap();
    assert!(app.state.workspaces[0].tabs[0].layout.new_stack_at(&[], id));
    let malformed = target(
        &mut app,
        vec![LayoutPathStep::Split(LayoutSplitChild::First)],
    );
    assert_eq!(
        error(&app.handle_layout_collapse("wrong-kind".into(), malformed, true)),
        "invalid_layout_path"
    );
    let missing = target(&mut app, vec![LayoutPathStep::StackChild(usize::MAX)]);
    assert_eq!(
        error(&app.handle_layout_collapse("missing".into(), missing, true)),
        "invalid_layout_path"
    );
    let root = target(&mut app, vec![]);
    success(&app.handle_layout_collapse("root".into(), root, true));
    let child = target(
        &mut app,
        vec![
            LayoutPathStep::StackChild(0),
            LayoutPathStep::Split(LayoutSplitChild::Second),
        ],
    );
    success(&app.handle_layout_collapse("hidden-child".into(), child, true));
    let root = target(&mut app, vec![]);
    success(&app.handle_layout_collapse("root-expand".into(), root, false));
    assert_eq!(
        app.state.workspaces[0].tabs[0]
            .layout
            .geometry(ratatui::layout::Rect::new(0, 0, 100, 40))
            .collapsed[0]
            .path,
        vec![
            LayoutPathStep::StackChild(0),
            LayoutPathStep::Split(LayoutSplitChild::Second)
        ]
    );
    app.state.assert_invariants_for_test();
}

#[tokio::test]
async fn stable_pane_desired_state_is_idempotent_and_persistence_fences_precede_mutation() {
    let mut app = setup();
    let pane = app.state.workspaces[0].tabs[0].root_pane;
    let public = app.public_pane_id(0, pane).unwrap();
    let target = PaneTarget { pane_id: public };
    app.session_persistence_blocked = true;
    let before = app.state.workspaces[0].tabs[0]
        .layout
        .tree_fingerprint()
        .to_owned();
    assert_eq!(
        error(&app.handle_pane_collapse("blocked".into(), target.clone(), true)),
        "layout_mutation_blocked"
    );
    assert_eq!(
        app.state.workspaces[0].tabs[0].layout.tree_fingerprint(),
        before
    );
    app.session_persistence_blocked = false;
    success(&app.handle_pane_collapse("collapse".into(), target.clone(), true));
    let collapsed = app.state.workspaces[0].tabs[0]
        .layout
        .tree_fingerprint()
        .to_owned();
    success(&app.handle_pane_collapse("again".into(), target.clone(), true));
    assert_eq!(
        app.state.workspaces[0].tabs[0].layout.tree_fingerprint(),
        collapsed
    );
    assert!(!app.state.workspaces[0].tabs[0].layout.is_pane_visible(pane));
    success(&app.handle_pane_collapse("expand".into(), target, false));
    assert_eq!(
        app.state.workspaces[0].tabs[0].layout.tree_fingerprint(),
        before
    );
    app.state.assert_invariants_for_test();
}
