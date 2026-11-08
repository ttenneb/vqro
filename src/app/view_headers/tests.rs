use super::*;

#[tokio::test]
async fn collapsed_manual_view_gets_and_withdraws_summary_without_auto_consumers() {
    let (_send, receive) = tokio::sync::mpsc::unbounded_channel();
    let mut app = App::new(
        &crate::config::Config::default(),
        crate::app::AppPolicy::TEST,
        None,
        receive,
        crate::api::EventHub::default(),
    );
    let mut workspace = crate::workspace::Workspace::test_new("header cache");
    let root = workspace.tabs[0].root_pane;
    let (_, view) = workspace
        .split_pane_with_view(
            root,
            "sample.views",
            "tasks",
            ratatui::layout::Direction::Vertical,
            0.7,
            false,
        )
        .unwrap();
    assert!(workspace.tabs[0].layout.set_collapsed(&[1], true));
    app.state.workspaces = vec![workspace];
    app.state.active = Some(0);
    app.state.ensure_test_terminals();
    let public = app.public_pane_id(0, root).unwrap();
    let document: crate::extension_contract::HostDocumentV1 = serde_json::from_value(serde_json::json!({"contract":"host.view.v1","namespace":"sample.views","generation":7,"revision":1,"surface":"navigation","target":{"kind":"session"},"items":[
        {"id":"root","role":"section","label":"Tasks","detail":"3/4 done · 1 waiting","view":{"pane_id":public,"view_id":"tasks","state":"open"}}
    ]})).unwrap();
    let key = ("sample.views".into(), "session".into());
    app.host_document_caches
        .entry(key.clone())
        .or_default()
        .publish_for_namespace("sample.views", 7, 0, &Default::default(), document)
        .unwrap();
    app.emit_host_document_updated("sample.views".into(), "session".into(), 7, 1, true);
    assert!(app.state.view_sizing.active.is_empty());
    assert_eq!(
        app.state.view_headers.panes[&view].0,
        "Tasks · 3/4 done · 1 waiting"
    );
    assert!(app.state.view_sizing.pending_geometry.is_empty());
    app.host_document_caches.get_mut(&key).unwrap().disable();
    app.emit_host_document_updated("sample.views".into(), "session".into(), 7, 1, false);
    assert_eq!(app.state.view_headers.panes[&view].0, "tasks");
    assert!(app.state.view_headers.sessions.is_empty());
    assert!(!app.state.workspaces[0].close_pane(view));
    assert!(app.state.workspaces[0].tabs[0]
        .layout
        .view_at(view)
        .is_none());
    app.sync_view_headers();
    assert!(app.state.view_headers.panes.is_empty());
    assert!(app.state.view_headers.tabs.is_empty());
}
