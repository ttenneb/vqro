use super::*;

fn pane_text_commits(outcome: &ClientShellInput) -> Vec<String> {
    outcome
        .requests
        .iter()
        .filter_map(|request| match request {
            ClientMessage::ClientShellPaneInput { events, .. } => Some(events),
            _ => None,
        })
        .flatten()
        .filter_map(|event| match event {
            ClientPaneInputEvent::TextCommit(text) => Some(text.clone()),
            _ => None,
        })
        .collect()
}

// Upstream #5277: report-all hosts can still produce plain text (Tern, IME,
// compose and Ghostty text bindings). Such text remains modal shortcut input.
fn report_all_kitty_host_state(snapshot: crate::protocol::ClientShellSnapshot) -> ClientShellState {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_host_reports_key_releases(true);
    state.set_host_reports_all_keys(true);
    state.set_snapshot(Box::new(snapshot));
    state.set_pane_surface(surface());
    state
}

#[test]
fn plain_text_after_the_prefix_runs_the_prefix_binding_on_a_report_all_host() {
    let mut state = report_all_kitty_host_state(snapshot());
    let _ = state.handle_input_bytes(b"\x1b[98;5u");
    assert_eq!(state.mode, ClientShellMode::Prefix);
    let detach = state.handle_input_bytes(b"q");
    assert!(detach.detach);
    assert!(pane_text_commits(&detach).is_empty());
    let mut state = report_all_kitty_host_state(snapshot());
    let detach = state.handle_input_bytes(b"\x02q");
    assert!(detach.detach);
    assert!(pane_text_commits(&detach).is_empty());
}

#[test]
fn plain_text_in_navigate_mode_runs_the_navigate_binding_on_a_report_all_host() {
    let mut snapshot = snapshot();
    let mut second = snapshot.workspaces[0].clone();
    second.workspace_id = "ws_2".into();
    second.number = 2;
    second.label = "second".into();
    second.focused = false;
    snapshot.workspaces.push(second);
    let mut state = report_all_kitty_host_state(snapshot);
    let _ = state.handle_input_bytes(b"\x1b[98;5u");
    let _ = state.handle_input_bytes(b"\x1b[119u");
    assert_eq!(state.mode, ClientShellMode::Navigate);
    let jump = state.handle_input_bytes(b"2");
    assert!(jump.actions.iter().any(|action| matches!(action,
        ClientShellAction::Endpoint { request, .. }
            if matches!(&request.method, crate::api::schema::Method::WorkspaceFocus(target)
                if target.workspace_id == "ws_2"))));
}

#[test]
fn committed_text_still_reaches_a_report_all_pane_as_text() {
    let mut state = report_all_kitty_host_state(snapshot());
    let cjk = state.handle_input_bytes("你好".as_bytes());
    assert_eq!(pane_text_commits(&cjk).concat(), "你好");
    let ascii = state.handle_input_bytes(b"n");
    assert_eq!(pane_text_commits(&ascii), ["n"]);
    let _ = state.handle_input_bytes(b"\x1b[98;5u");
    let _ = state.handle_input_bytes(b"\x1b");
    assert_eq!(state.mode, ClientShellMode::Terminal);
    let after = state.handle_input_bytes("あ".as_bytes());
    assert_eq!(pane_text_commits(&after).concat(), "あ");
}
