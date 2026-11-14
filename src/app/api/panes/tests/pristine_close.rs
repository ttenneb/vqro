use super::*;

#[path = "pristine_relocation.rs"]
mod relocation;
use crate::{
    app::{AppPolicy, AppState},
    terminal::{TerminalId, TerminalRuntime},
};
use std::os::{fd::OwnedFd, unix::net::UnixStream};

fn fixture() -> (App, String, TerminalId, UnixStream) {
    fixture_with_state(AppState::test_with_adversarial_identity_state())
}

fn fixture_with_state(state: AppState) -> (App, String, TerminalId, UnixStream) {
    let (_send, receive) = tokio::sync::mpsc::unbounded_channel();
    let mut app = App::new(
        &crate::config::Config::default(),
        AppPolicy::TEST,
        None,
        receive,
        crate::api::EventHub::default(),
    );
    app.state = state;
    app.state.ensure_test_terminals();
    let pane = app.state.workspaces[0].tabs[0].root_pane;
    let terminal_id = app.state.workspaces[0].terminal_id(pane).unwrap().clone();
    let (socket, peer) = UnixStream::pair().unwrap();
    let actor = crate::pty::actor::PtyIoActor::spawn(crate::pty::actor::PtyIoActorConfig {
        pane_id: pane.raw(),
        master_fd: OwnedFd::from(socket),
        ownership: None,
        initially_quiesced: false,
        on_read: Box::new(|_| crate::pty::actor::PtyReadResult::empty()),
        on_reader_exit: None,
    })
    .unwrap();
    app.terminal_runtimes
        .insert(terminal_id.clone(), TerminalRuntime::test_with_actor(actor));
    let public = app.public_pane_id(0, pane).unwrap();
    (app, public, terminal_id, peer)
}

fn close(app: &mut App, pane: &str, terminal: &TerminalId) -> serde_json::Value {
    serde_json::from_str(&app.handle_pristine_terminal_close(
        "cleanup".into(),
        PaneCloseIfPristineTerminalParams {
            pane_id: pane.into(),
            expected_terminal_id: terminal.to_string(),
        },
    ))
    .unwrap()
}

#[tokio::test]
async fn pristine_close_requires_positive_freshness_and_exact_attachment() {
    let (mut app, pane, terminal, _peer) = fixture();
    assert_eq!(
        close(&mut app, &pane, &terminal)["error"]["code"],
        "pristine_close_rejected"
    );
    assert!(app
        .terminal_runtimes
        .get(&terminal)
        .unwrap()
        .arm_fresh_split_cleanup());
    let wrong = TerminalId::alloc().unwrap();
    assert_eq!(
        close(&mut app, &pane, &wrong)["error"]["code"],
        "pristine_close_rejected"
    );
    assert!(app.terminal_runtimes.get(&terminal).is_some());
    let result = close(&mut app, &pane, &terminal);
    assert!(result.get("error").is_none(), "{result}");
    assert!(app.terminal_runtimes.get(&terminal).is_none());
    app.state.assert_invariants_for_test();
}

#[tokio::test]
async fn pristine_close_retains_queued_work_and_invalidated_lifecycle() {
    for queued in [false, true] {
        let (mut app, pane, terminal, _peer) = fixture();
        let runtime = app.terminal_runtimes.get(&terminal).unwrap();
        assert!(runtime.arm_fresh_split_cleanup());
        if queued {
            let _completion = runtime
                .queue_user_input_submission(
                    bytes::Bytes::from_static(b"run"),
                    bytes::Bytes::from_static(b"\r"),
                    std::time::Duration::from_millis(100),
                    None,
                )
                .unwrap();
        } else {
            runtime.invalidate_cleanup();
        }
        assert_eq!(
            close(&mut app, &pane, &terminal)["error"]["code"],
            "pristine_close_rejected"
        );
        assert!(app
            .terminal_runtimes
            .get(&terminal)
            .unwrap()
            .try_send_bytes(bytes::Bytes::from_static(b"usable"))
            .is_ok());
        app.state.assert_invariants_for_test();
    }
}

#[tokio::test]
async fn pristine_close_after_same_id_runtime_replacement_is_rejected() {
    let (mut app, pane, terminal, _peer) = fixture();
    assert!(app
        .terminal_runtimes
        .get(&terminal)
        .unwrap()
        .arm_fresh_split_cleanup());
    let (socket, replacement_peer) = UnixStream::pair().unwrap();
    let actor = crate::pty::actor::PtyIoActor::spawn(crate::pty::actor::PtyIoActorConfig {
        pane_id: 1,
        master_fd: OwnedFd::from(socket),
        ownership: None,
        initially_quiesced: false,
        on_read: Box::new(|_| crate::pty::actor::PtyReadResult::empty()),
        on_reader_exit: None,
    })
    .unwrap();
    let _replacement_peer = replacement_peer;
    app.terminal_runtimes
        .insert(terminal.clone(), TerminalRuntime::test_with_actor(actor));
    assert_eq!(
        close(&mut app, &pane, &terminal)["error"]["code"],
        "pristine_close_rejected"
    );
    assert!(app
        .terminal_runtimes
        .get(&terminal)
        .unwrap()
        .try_send_bytes(bytes::Bytes::from_static(b"replacement usable"))
        .is_ok());
}

#[tokio::test]
async fn pristine_close_move_disqualifies_even_with_unchanged_public_attachment() {
    let (mut app, pane, terminal, _peer) = fixture();
    assert!(app
        .terminal_runtimes
        .get(&terminal)
        .unwrap()
        .arm_fresh_split_cleanup());
    for _ in 0..2 {
        let moved = app.handle_pane_move(
            "move".into(),
            crate::api::schema::PaneMoveParams {
                pane_id: pane.clone(),
                destination: crate::api::schema::PaneMoveDestination::NewTab {
                    workspace_id: None,
                    label: None,
                },
                focus: false,
            },
        );
        let moved: serde_json::Value = serde_json::from_str(&moved).unwrap();
        assert!(moved.get("error").is_none(), "{moved}");
    }
    assert_eq!(
        close(&mut app, &pane, &terminal)["error"]["code"],
        "pristine_close_rejected"
    );
    app.state.assert_invariants_for_test();
}

#[tokio::test]
async fn pristine_close_confirmation_failure_rolls_back_only_its_claim() {
    let mut state = AppState::test_new();
    state.workspaces = vec![
        crate::workspace::Workspace::test_new("parent"),
        crate::workspace::Workspace::test_new("child"),
    ];
    state.confirm_close = true;
    for (index, workspace) in state.workspaces.iter_mut().enumerate() {
        workspace.worktree_space = Some(crate::workspace::WorktreeSpaceMembership {
            key: "cleanup-test".into(),
            label: "test".into(),
            repo_root: "/private-cleanup-test".into(),
            checkout_path: if index == 0 {
                "/private-cleanup-test".into()
            } else {
                "/private-cleanup-test-child".into()
            },
            is_linked_worktree: index != 0,
        });
    }
    state.active = Some(0);
    let (mut app, pane, terminal, _peer) = fixture_with_state(state);
    assert!(app
        .terminal_runtimes
        .get(&terminal)
        .unwrap()
        .arm_fresh_split_cleanup());
    let result = close(&mut app, &pane, &terminal);
    assert!(result.get("error").is_some(), "{result}");
    assert_ne!(
        result["error"]["code"], "pristine_close_rejected",
        "must reach ordinary close preconditions"
    );
    assert!(app
        .terminal_runtimes
        .get(&terminal)
        .unwrap()
        .try_send_bytes(bytes::Bytes::from_static(b"retained usable"))
        .is_ok());
}

#[tokio::test]
async fn pristine_close_retains_pending_resume_without_disabling_input() {
    let (mut app, pane, terminal, _peer) = fixture();
    assert!(app
        .terminal_runtimes
        .get(&terminal)
        .unwrap()
        .arm_fresh_split_cleanup());
    app.state
        .terminals
        .get_mut(&terminal)
        .unwrap()
        .pending_agent_resume_plan = Some(crate::agent_resume::AgentResumePlan {
        agent: "pi".into(),
        argv: vec!["pi".into()],
        dedupe_key: "pending".into(),
    });
    assert_eq!(
        close(&mut app, &pane, &terminal)["error"]["code"],
        "pristine_close_rejected"
    );
    assert!(app
        .terminal_runtimes
        .get(&terminal)
        .unwrap()
        .try_send_bytes(bytes::Bytes::from_static(b"usable"))
        .is_ok());
}
