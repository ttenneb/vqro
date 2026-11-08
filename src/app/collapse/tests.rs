use super::*;
use crate::{extension_contract::HostDocumentV1, layout::PaneId, ui::TabSurfaceTarget};
use ratatui::layout::{Direction, Rect};

fn setup() -> (App, PaneId, HostDocumentV1) {
    let (_send, receive) = tokio::sync::mpsc::unbounded_channel();
    let mut app = App::new(
        &crate::config::Config::default(),
        crate::app::AppPolicy::TEST,
        None,
        receive,
        crate::api::EventHub::default(),
    );
    let mut workspace = crate::workspace::Workspace::test_new("collapse fence");
    let terminal = workspace.tabs[0].root_pane;
    let (_, view) = workspace
        .split_pane_with_view(
            terminal,
            "sample.views",
            "tasks",
            Direction::Vertical,
            0.7,
            false,
        )
        .unwrap();
    app.state.workspaces = vec![workspace];
    app.state.active = Some(0);
    app.state.ensure_test_terminals();
    let public = app.public_pane_id(0, terminal).unwrap();
    let mut document: HostDocumentV1 = serde_json::from_value(serde_json::json!({"contract":"host.view.v1","namespace":"sample.views","generation":7,"revision":1,"surface":"navigation","target":{"kind":"session"},"items":[
        {"id":"root","role":"section","label":"Tasks","detail":"package summary","view":{"pane_id":public,"view_id":"tasks","state":"open"}}
    ]})).unwrap();
    publish(&mut app, &mut document, 1, 1, true);
    app.activate_auto_view(0, 0, view);
    resize(&mut app, Rect::new(0, 0, 100, 40));
    (app, view, document)
}
fn publish(app: &mut App, document: &mut HostDocumentV1, revision: u64, rows: usize, issue: bool) {
    document.revision = revision;
    document.items.truncate(1);
    for index in 0..rows {
        document.items.push(serde_json::from_value(serde_json::json!({"id":format!("row-{index}"),"parent_id":"root","role":"section","label":"row","view":{"state":"open"}})).unwrap());
    }
    let key = ("sample.views".into(), "session".into());
    if issue {
        app.host_document_request_watermarks
            .insert(key.clone(), (7, revision));
    }
    app.host_document_caches
        .entry(key)
        .or_default()
        .publish_for_namespace("sample.views", 7, 0, &Default::default(), document.clone())
        .unwrap();
    app.emit_host_document_updated("sample.views".into(), "session".into(), 7, revision, true);
}
fn resize(app: &mut App, area: Rect) {
    app.resize_auto_views(
        TabSurfaceTarget {
            workspace_index: 0,
            tab_index: 0,
        },
        area,
        crate::kitty_graphics::HostCellSize::default(),
    );
}
fn view_height(app: &App, view: PaneId) -> u16 {
    app.state.workspaces[0].tabs[0]
        .layout
        .geometry(Rect::new(0, 0, 100, 40))
        .panes
        .iter()
        .find(|pane| pane.id == view)
        .unwrap()
        .rect
        .height
}

#[tokio::test]
async fn expand_restores_before_queued_result_equal_metric_replay_and_a_genuinely_new_event() {
    let (mut app, view, mut document) = setup();
    let before = view_height(&app, view);
    assert!(app.set_node_collapsed(0, 0, &[1], true));
    // Host dispatch happened before expansion, but completion will arrive later.
    app.host_document_request_watermarks
        .insert(("sample.views".into(), "session".into()), (7, 2));
    assert!(app.set_node_collapsed(0, 0, &[1], false));
    assert!(app.state.view_sizing.fenced(view));
    resize(&mut app, Rect::new(0, 0, 100, 40));
    assert_eq!(view_height(&app, view), before);
    publish(&mut app, &mut document, 2, 8, false);
    resize(&mut app, Rect::new(0, 0, 100, 40));
    assert!(app.state.view_sizing.fenced(view));
    assert_eq!(view_height(&app, view), before);
    // Even a later host request with equal metrics is not a changed fact.
    publish(&mut app, &mut document, 3, 8, true);
    resize(&mut app, Rect::new(0, 0, 100, 40));
    assert!(app.state.view_sizing.fenced(view));
    assert_eq!(view_height(&app, view), before);
    publish(&mut app, &mut document, 4, 9, true);
    assert!(!app.state.view_sizing.fenced(view));
    resize(&mut app, Rect::new(0, 0, 100, 40));
    assert_ne!(view_height(&app, view), before);
    app.state.assert_invariants_for_test();
}

#[tokio::test]
async fn fence_survives_unrelated_collapse_and_same_size_replay_but_not_changed_viewport() {
    let (mut app, view, _) = setup();
    assert!(app.set_node_collapsed(0, 0, &[1], true));
    assert!(app.set_node_collapsed(0, 0, &[1], false));
    assert!(app.state.workspaces[0].tabs[0]
        .layout
        .set_collapsed(&[0], true));
    app.prune_auto_view_sizing();
    assert!(app.state.view_sizing.fenced(view));
    resize(&mut app, Rect::new(0, 0, 100, 40));
    assert!(app.state.view_sizing.fenced(view));
    resize(&mut app, Rect::new(0, 0, 100, 41));
    assert!(!app.state.view_sizing.fenced(view));
}

#[tokio::test]
async fn withdrawal_clears_header_without_resuming_old_work_and_fresh_recovery_can_resume() {
    let (mut app, view, mut document) = setup();
    let before = view_height(&app, view);
    assert!(app.set_node_collapsed(0, 0, &[1], true));
    assert!(app.set_node_collapsed(0, 0, &[1], false));
    app.host_document_caches
        .get_mut(&("sample.views".into(), "session".into()))
        .unwrap()
        .disable();
    app.emit_host_document_updated("sample.views".into(), "session".into(), 7, 1, false);
    assert_eq!(app.state.view_headers.panes[&view].0, "tasks");
    resize(&mut app, Rect::new(0, 0, 100, 40));
    assert!(app.state.view_sizing.fenced(view));
    assert_eq!(view_height(&app, view), before);
    publish(&mut app, &mut document, 2, 10, true);
    assert!(!app.state.view_sizing.fenced(view));
    resize(&mut app, Rect::new(0, 0, 100, 40));
    assert_ne!(view_height(&app, view), before);
}

fn setup_box() -> (App, PaneId, Vec<PaneId>) {
    let (mut app, _, _) = setup();
    let mut workspace = crate::workspace::Workspace::test_new("designated box");
    let terminal = workspace.tabs[0].root_pane;
    let (_, views) = workspace
        .split_pane_with_view_box(
            terminal,
            &[
                ("sample.views".into(), "one".into()),
                ("sample.views".into(), "two".into()),
            ],
            Direction::Vertical,
            0.5,
        )
        .unwrap();
    app.state.workspaces = vec![workspace];
    app.state.terminals.clear();
    app.state.ensure_test_terminals();
    app.prune_auto_view_sizing();
    for view in &views {
        app.activate_auto_view(0, 0, *view);
    }
    resize(&mut app, Rect::new(0, 0, 100, 40));
    (app, terminal, views)
}

#[tokio::test]
async fn designated_box_child_expansion_refits_without_rearming_manual_or_latent_zoom() {
    let (mut app, _, views) = setup_box();
    let before = view_height(&app, views[0]);
    assert!(app.set_node_collapsed(0, 0, &[1, 1], true));
    resize(&mut app, Rect::new(0, 0, 100, 40));
    assert_eq!(view_height(&app, views[0]), before);
    // A restored policy flag on a collapsed child must not prevent its sibling
    // from being sized, and expansion must clear that latent zoom policy.
    app.state.workspaces[0].tabs[0].layout.focus_pane(views[1]);
    app.state.workspaces[0].tabs[0].zoomed = true;
    resize(&mut app, Rect::new(0, 0, 100, 40));
    assert!(app.set_node_collapsed(0, 0, &[1, 1], false));
    assert!(!app.state.workspaces[0].tabs[0].zoomed);
    assert!(!app.state.view_sizing.fenced(views[0]));
    assert!(!app.state.view_sizing.fenced(views[1]));
    resize(&mut app, Rect::new(0, 0, 100, 40));
    assert_eq!(view_height(&app, views[0]), before);
    app.manual_view_sizing_for_split(0, 0, &[]);
    assert!(
        app.state.view_sizing.active.is_empty(),
        "outer box drag disarms every child"
    );
    let shares = app.state.workspaces[0].tabs[0].layout.typed_root().clone();
    assert!(app.set_node_collapsed(0, 0, &[1, 1], true));
    assert!(app.set_node_collapsed(0, 0, &[1, 1], false));
    resize(&mut app, Rect::new(0, 0, 100, 40));
    assert!(app.state.view_sizing.active.is_empty());
    assert_eq!(app.state.workspaces[0].tabs[0].layout.typed_root(), &shares);
    app.state.assert_invariants_for_test();
}

#[tokio::test]
async fn designated_child_refit_never_clears_whole_box_or_enclosing_fences() {
    for path in [vec![1], vec![]] {
        let (mut app, _, views) = setup_box();
        assert!(app.set_node_collapsed(0, 0, &path, true));
        assert!(app.set_node_collapsed(0, 0, &path, false));
        assert!(views.iter().all(|view| app.state.view_sizing.fenced(*view)));
        assert!(app.set_node_collapsed(0, 0, &[1, 1], true));
        assert!(app.set_node_collapsed(0, 0, &[1, 1], false));
        resize(&mut app, Rect::new(0, 0, 100, 40));
        assert!(views.iter().all(|view| app.state.view_sizing.fenced(*view)));
    }
}

#[tokio::test]
async fn restored_box_document_arrival_and_child_expansion_never_rearm_auto_fit() {
    let (mut app, terminal, _) = setup_box();
    let tab = &mut app.state.workspaces[0].tabs[0];
    tab.layout = crate::layout::TileLayout::from_tree(
        tab.layout.typed_root().clone(),
        tab.layout.focused_leaf(),
    )
    .unwrap();
    app.state.view_sizing = Default::default();
    let before = app.state.workspaces[0].tabs[0].layout.typed_root().clone();
    let public = app.public_pane_id(0, terminal).unwrap();
    let mut document: HostDocumentV1 = serde_json::from_value(serde_json::json!({"contract":"host.view.v1","namespace":"sample.views","generation":7,"revision":1,"surface":"navigation","target":{"kind":"session"},"items":[
        {"id":"root","role":"section","label":"Rows","view":{"pane_id":public,"view_id":"one"}}
    ]})).unwrap();
    publish(&mut app, &mut document, 2, 200, true);
    assert!(app.set_node_collapsed(0, 0, &[1, 1], true));
    assert!(app.set_node_collapsed(0, 0, &[1, 1], false));
    resize(&mut app, Rect::new(0, 0, 100, 40));
    assert!(app.state.view_sizing.active.is_empty());
    assert_eq!(app.state.workspaces[0].tabs[0].layout.typed_root(), &before);
    app.state.assert_invariants_for_test();
}

#[tokio::test]
async fn manual_opt_out_clears_an_existing_resume_fence() {
    let (mut app, view, _) = setup();
    assert!(app.set_node_collapsed(0, 0, &[1], true));
    assert!(app.set_node_collapsed(0, 0, &[1], false));
    assert!(app.state.view_sizing.fenced(view));
    app.state.view_sizing.manual([view]);
    assert!(!app.state.view_sizing.fenced(view));
    assert!(!app.state.view_sizing.active.contains_key(&view));
}

#[tokio::test]
async fn manual_opt_out_is_never_rearmed_by_collapse_expand_or_new_documents() {
    let (mut app, view, mut document) = setup();
    app.state.view_sizing.manual([view]);
    let before = view_height(&app, view);
    assert!(app.set_node_collapsed(0, 0, &[1], true));
    assert!(app.set_node_collapsed(0, 0, &[1], false));
    assert!(app.state.view_sizing.resume_fences.is_empty());
    publish(&mut app, &mut document, 2, 10, true);
    resize(&mut app, Rect::new(0, 0, 100, 40));
    assert!(app.state.view_sizing.active.is_empty());
    assert_eq!(view_height(&app, view), before);
}
