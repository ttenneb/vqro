use super::*;
use crate::api::schema::{Method, PaneViewAction, PaneViewParams, Request};

fn snapshot(server: &HeadlessServer) -> serde_json::Value {
    serde_json::to_value(crate::persist::capture(
        &server.app.state.workspaces,
        &server.app.state.repositories,
        &server.app.state.space_order,
        &server.app.state.terminals,
        &server.app.terminal_runtimes,
        server.app.state.active,
        server.app.state.selected,
    ))
    .unwrap()
}

#[tokio::test]
async fn checkpoint_busy_view_add_and_close_are_unchanged_state_dispatch_errors() {
    let (mut server, _render, root) = retained_test_server(b"terminal");
    let (_, view) = server.app.state.workspaces[0]
        .split_pane_with_view(
            root,
            "vqro.tasks",
            "tasks",
            ratatui::layout::Direction::Horizontal,
            0.5,
            false,
        )
        .unwrap();
    server.app.state.ensure_test_terminals();
    let root_public = server.app.public_pane_id(0, root).unwrap();
    let view_public = server.app.public_pane_id(0, view).unwrap();
    server.app.collections_mutations_blocked = true;
    let before = snapshot(&server);
    for (action, pane) in [
        (PaneViewAction::Add, root_public),
        (PaneViewAction::Close, view_public),
    ] {
        let request = Request {
            id: "checkpoint-view".into(),
            method: Method::PaneView(PaneViewParams {
                pane_id: Some(pane),
                action,
                extension: Some("vqro.tasks".into()),
                view: Some("tasks".into()),
                direction: None,
                ratio: Some(0.3),
                focus: false,
            }),
        };
        assert!(
            crate::api::request_changes_ui(&request),
            "views use the existing mutation/dirty classifier"
        );
        let (send, receive) = std::sync::mpsc::channel();
        server.handle_api_request_with_shutdown_check(api::ApiRequestMessage {
            origin: api::ApiOrigin::LocalSocket,
            request,
            respond_to: send,
            response_write_complete: None,
        });
        let response: serde_json::Value = serde_json::from_str(&receive.recv().unwrap()).unwrap();
        assert_eq!(response["id"], "checkpoint-view");
        assert_eq!(response["error"]["code"], "collections_checkpoint_busy");
        assert_eq!(
            snapshot(&server),
            before,
            "blocked {action:?} may not mutate session state"
        );
        server.app.state.workspaces[0].assert_invariants_for_test();
        server.app.state.assert_invariants_for_test();
    }
}
