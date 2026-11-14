use super::collapse::{feature_shell, projection};
use super::*;
use crossterm::event::{KeyCode, KeyEventKind, KeyModifiers};

fn coherent() -> ClientShellState {
    let mut state = feature_shell();
    let frame = surface();
    state.set_pane_surface_with_views_for_generation(
        &ClientEndpointId::Local,
        1,
        frame.clone(),
        Vec::new(),
    );
    state.set_endpoint_collapse_projection_for_generation(
        &ClientEndpointId::Local,
        1,
        Ok(projection(&frame)),
    );
    state
}

fn snapshot_ahead(state: &mut ClientShellState) {
    let mut next = snapshot();
    next.revision = 2;
    state.set_snapshot(Box::new(next));
    assert!(!state.collapse_interactions_current());
}

fn key(kind: KeyEventKind) -> RawInputEvent {
    RawInputEvent::Key(
        crate::input::TerminalKey::new(KeyCode::Char('x'), KeyModifiers::NONE).with_kind(kind),
    )
}

fn assert_refused(state: &ClientShellState, outcome: &ClientShellInput) {
    assert!(
        outcome.requests.is_empty(),
        "refused input must not reach a terminal"
    );
    let notice = state
        .visible_endpoint_notice
        .as_ref()
        .expect("input refusal notice");
    assert_eq!(notice.key.kind, ClientEndpointNoticeKind::Rejected);
    assert_eq!(notice.title, "Input not delivered");
    assert_eq!(notice.body, "Check the active pane before retyping.");
}

#[test]
fn input_refusal_coherent_keyboard_text_and_paste_are_unchanged() {
    for event in [
        key(KeyEventKind::Press),
        RawInputEvent::Text(crate::input::TextCommit::new("private text")),
        RawInputEvent::Paste("private paste".into()),
    ] {
        let mut state = coherent();
        let outcome = state.handle_raw_events(vec![event]);
        assert!(outcome.requests.iter().any(|request| matches!(request,
            ClientMessage::ClientShellPaneInput { pane_id, .. } if pane_id == "pane_1")));
        assert!(state.visible_endpoint_notice.is_none());
    }
}

#[test]
fn input_refusal_snapshot_skew_notifies_without_forwarding_or_echoing_content() {
    for event in [
        key(KeyEventKind::Press),
        RawInputEvent::Text(crate::input::TextCommit::new("SECRET typed text")),
        RawInputEvent::Paste("SECRET pasted text".into()),
    ] {
        let mut state = coherent();
        snapshot_ahead(&mut state);
        let outcome = state.handle_raw_events(vec![event]);
        assert_refused(&state, &outcome);
        assert!(
            outcome.repaint,
            "the refusal must redraw even with an incoherent surface"
        );
        let frame = state.compose(106, 24).expect("refusal frame");
        let text = frame
            .cells
            .iter()
            .map(|cell| cell.symbol.as_str())
            .collect::<String>();
        assert!(text.contains("Input not delivered"));
        assert!(!text.contains("SECRET"));
        assert!(
            !text.contains("LIVE"),
            "fallback does not republish stale pane pixels"
        );
        assert!(frame.cursor.is_none());
        assert!(state.hits.panes.is_empty());
        assert!(state.hits.pane_splits.is_empty());
        assert!(state.hits.popup.is_none());
        assert!(!state.collapse_interactions_current());
        assert_eq!(state.snapshot.as_ref().unwrap().revision, 2);
        assert_eq!(state.pane_surface.as_ref().unwrap().projection_revision, 1);
    }
}

#[test]
fn input_refusal_repeats_are_bounded_and_expire_without_passive_renewal() {
    let mut state = coherent();
    snapshot_ahead(&mut state);
    let first = state.handle_raw_events(vec![key(KeyEventKind::Press)]);
    assert_refused(&state, &first);
    let deadline = state.visible_endpoint_notice.as_ref().unwrap().deadline;
    state.compose(106, 24).expect("refusal-only chrome");
    let repeated = state.handle_raw_events(vec![key(KeyEventKind::Press)]);
    assert_refused(&state, &repeated);
    assert_eq!(
        state.visible_endpoint_notice.as_ref().unwrap().deadline,
        deadline
    );
    assert!(
        !state
            .tick_notifications(deadline - std::time::Duration::from_millis(1))
            .1
    );
    assert!(state.tick_notifications(deadline).1);
    assert!(state.visible_endpoint_notice.is_none());
    let cleared = state
        .compose(106, 24)
        .expect("one final chrome repaint clears expired notice");
    let text = cleared
        .cells
        .iter()
        .map(|cell| cell.symbol.as_str())
        .collect::<String>();
    assert!(!text.contains("Input not delivered"));
    assert!(state.hits.panes.is_empty());
    assert!(!state.collapse_interactions_current());
    assert!(
        state.compose(106, 24).is_none(),
        "restore normal suppressed composition without a notice"
    );
    assert!(
        !state
            .tick_notifications(deadline + std::time::Duration::from_secs(1))
            .1
    );
    assert!(
        state.visible_endpoint_notice.is_none(),
        "passive frames must not resurrect the notice"
    );
}

#[test]
fn input_refusal_does_not_notify_for_ui_keys_or_passive_projection_updates() {
    let mut state = coherent();
    snapshot_ahead(&mut state);
    assert!(state.visible_endpoint_notice.is_none());
    state.handle_input_bytes(b"\x02"); // Prefix mode is client UI, not terminal dispatch.
    assert!(state.visible_endpoint_notice.is_none());
    let outcome = state.handle_raw_events(vec![RawInputEvent::Text(
        crate::input::TextCommit::new("UI text"),
    )]);
    assert!(outcome.requests.is_empty());
    assert!(state.visible_endpoint_notice.is_none());
}

#[test]
fn input_refusal_ui_owned_collection_and_inspector_text_stay_silent() {
    for inspector in [false, true] {
        let mut state = coherent();
        snapshot_ahead(&mut state);
        if inspector {
            state.inspector = Some(Box::new(ClientTerminalInspectorPane {
                pane_id: "pane_1".into(),
                pane_label: "pane".into(),
                loading: false,
                focused: true,
                request_revision: 1,
                view_request_id: None,
                service_generation: None,
                pending_action: None,
                placement: crate::pane_contribution::PaneContributionPlacement::Accessory,
                contribution: None,
                telemetry: None,
                error: None,
            }));
        } else {
            state.structure = Some(Box::new(
                crate::protocol::endpoint::ClientShellStructureProjection {
                    session_id: "private".into(),
                    boot_id: "boot-1".into(),
                    revision: 2,
                    collections: vec![crate::protocol::endpoint::ClientShellCollection {
                        collection_id: "collection_1".into(),
                        workspace_id: "ws_1".into(),
                        tab_id: "tab_1".into(),
                        label: None,
                        selected_pane_id: Some("pane_1".into()),
                        members: vec![crate::protocol::endpoint::ClientShellCollectionMember {
                            pane_id: "pane_1".into(),
                            index: 0,
                            archived: false,
                        }],
                    }],
                    delegations: Vec::new(),
                    repositories: Vec::new(),
                    space_order: Vec::new(),
                },
            ));
            state.collection_list_views.insert("collection_1".into());
        }
        let outcome = state.handle_raw_events(vec![
            RawInputEvent::Text(crate::input::TextCommit::new("UI text")),
            RawInputEvent::Paste("UI paste".into()),
        ]);
        assert!(outcome.requests.is_empty());
        assert!(
            state.visible_endpoint_notice.is_none(),
            "UI consumption is not refused terminal dispatch"
        );
    }
}

#[test]
fn input_refusal_preserves_release_cleanup_and_never_leases_a_refused_press() {
    let mut state = coherent();
    state.set_host_reports_key_releases(true);
    let pressed = state.handle_raw_events(vec![key(KeyEventKind::Press)]);
    assert!(!pressed.requests.is_empty());
    snapshot_ahead(&mut state);
    let repeat = state.handle_raw_events(vec![key(KeyEventKind::Repeat)]);
    assert_refused(&state, &repeat);
    state.visible_endpoint_notice = None;
    let release = state.handle_raw_events(vec![key(KeyEventKind::Release)]);
    assert!(
        !release.requests.is_empty(),
        "a forwarded press retains release cleanup"
    );
    assert!(state.visible_endpoint_notice.is_none());
    let refused = state.handle_raw_events(vec![key(KeyEventKind::Press)]);
    assert_refused(&state, &refused);
    let release = state.handle_raw_events(vec![key(KeyEventKind::Release)]);
    assert!(
        release.requests.is_empty(),
        "a refused press cannot acquire a forwarded lease"
    );
}

#[test]
fn input_refusal_newer_topology_and_cold_pairs_remain_inert() {
    let mut pending = coherent();
    let mut next = surface();
    next.surface_revision = 2;
    let mut metadata = projection(&next);
    metadata.body.tree_fingerprint = Some("b".repeat(64));
    pending.set_endpoint_collapse_projection_for_generation(
        &ClientEndpointId::Local,
        1,
        Ok(metadata),
    );
    assert!(!pending.collapse_interactions_current());
    let rejected = pending.handle_raw_events(vec![key(KeyEventKind::Press)]);
    assert_refused(&pending, &rejected);
    let chrome = pending
        .compose(106, 24)
        .expect("pending-companion refusal chrome");
    assert!(chrome.cursor.is_none());
    assert!(pending.hits.panes.is_empty());
    assert!(!pending.collapse_interactions_current());
    assert_eq!(pending.pane_surface.as_ref().unwrap().surface_revision, 1);

    let mut cold = feature_shell();
    let rejected = cold.handle_raw_events(vec![RawInputEvent::Paste("private paste".into())]);
    assert_refused(&cold, &rejected);
    let chrome = cold.compose(106, 24).expect("cold refusal chrome");
    let text = chrome
        .cells
        .iter()
        .map(|cell| cell.symbol.as_str())
        .collect::<String>();
    assert!(text.contains("Input not delivered"));
    assert!(chrome.cursor.is_none());
    assert!(cold.hits.panes.is_empty());
    assert!(!cold.collapse_interactions_current());
}

#[test]
fn input_refusal_notice_does_not_block_recovery_or_survive_a_boot_change() {
    let mut state = coherent();
    snapshot_ahead(&mut state);
    let rejected = state.handle_raw_events(vec![key(KeyEventKind::Press)]);
    assert_refused(&state, &rejected);
    let deadline = state.visible_endpoint_notice.as_ref().unwrap().deadline;
    let mut next = surface();
    next.projection_revision = 2;
    next.surface_revision = 2;
    state.set_pane_surface_with_views_for_generation(
        &ClientEndpointId::Local,
        1,
        next.clone(),
        Vec::new(),
    );
    state.set_endpoint_collapse_projection_for_generation(
        &ClientEndpointId::Local,
        1,
        Ok(projection(&next)),
    );
    let accepted = state.handle_raw_events(vec![RawInputEvent::Text(
        crate::input::TextCommit::new("after recovery"),
    )]);
    assert!(!accepted.requests.is_empty());
    let restored = state.compose(106, 24).expect("coherent body restored");
    let text = restored
        .cells
        .iter()
        .map(|cell| cell.symbol.as_str())
        .collect::<String>();
    assert!(text.contains("LIVE"));
    assert!(!state.hits.panes.is_empty());
    assert_eq!(
        state.visible_endpoint_notice.as_ref().unwrap().deadline,
        deadline
    );
    let mut reboot = snapshot();
    reboot.boot_id = "new-boot".into();
    state.set_snapshot(Box::new(reboot));
    assert!(state.visible_endpoint_notice.is_none());
}
