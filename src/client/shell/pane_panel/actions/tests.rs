use super::*;
use crate::api::schema::{Method, ResponseResult};
use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use serde_json::json;

fn fixture(choice: bool, reversible: bool) -> ClientShellState {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(
        &crate::config::Config::default(),
    ));
    let snapshot = crate::client::shell::tests::snapshot();
    state.set_snapshot(Box::new(snapshot));
    state.set_endpoint_methods(Some(vec![
        "host.view.action.prepare.v1".into(),
        "host.view.action.submit.v1".into(),
    ]));
    let affordance = if choice {
        json!({"kind":"choice","label":"Answer","detail":"The full structured question?","subject":{"id":"opaque"},"options":[{"key":"go","label":"Proceed"},{"key":"wait","label":"Wait"}],"default":"wait","reversible":reversible})
    } else {
        json!({"kind":"action","label":"Accept","detail":"Accept reported work","subject":{"id":"opaque"}})
    };
    let documents:HostDocumentsProjectionV1=serde_json::from_value(json!({"boot_id":"boot-1","revision":1,"documents":[{"provider_priority":1,"document":{"contract":"host.document.v1","namespace":"example.docs","generation":7,"revision":8,"surface":"pane","target":{"kind":"session"},"items":[{"id":"root","role":"section","label":"Work","view":{"pane_id":"pane_1","view_id":"work"}},{"id":"row","parent_id":"root","role":"section","label":"Reported work","view":{"kind":"row","affordance":affordance}}]}}]})).unwrap();
    state.endpoints[0].host_documents = Some(Box::new(documents));
    let rect = Rect::new(0, 0, 80, 24);
    let inner = Rect::new(1, 1, 78, 22);
    state.pane_surface = Some(crate::protocol::PaneSurfaceFrame {
        boot_id: "boot-1".into(),
        projection_revision: 1,
        surface_revision: 1,
        frame: crate::protocol::FrameData::from_ratatui_buffer_with_hyperlinks(
            &Buffer::empty(rect),
            None,
            &[],
        ),
        panes: Vec::new(),
        collections: Vec::new(),
        splits: Vec::new(),
        popup: None,
        graphics: Default::default(),
    });
    state.pane_surface_views = vec![crate::protocol::PaneSurfaceViewEntryV2 {
        pane_id: "view".into(),
        extension: "example.docs".into(),
        view: "work".into(),
        scope: vec!["pane_1".into(), "hidden_terminal".into()],
    }];
    state.hits.panes = vec![PaneHit {
        rect,
        inner_rect: inner,
        projected_inner_rect: inner,
        scrollbar_rect: None,
        scroll: None,
        pane_id: "view".into(),
        popup: false,
        mouse_reporting: false,
        sgr_pixel_mouse: false,
        pixel_width: 0,
        pixel_height: 0,
    }];
    state.view_action_composed = Some(("boot-1".into(), 1, 1));
    state
}
fn click(x: u16, y: u16) -> MouseEvent {
    MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: x,
        row: y,
        modifiers: KeyModifiers::NONE,
    }
}
fn open(state: &mut ClientShellState) -> ClientShellInput {
    let mut outcome = ClientShellInput::default();
    assert!(state.handle_view_action_mouse(click(77, 1), &mut outcome));
    outcome
}
fn paint(state: &mut ClientShellState) -> Vec<(Rect, usize)> {
    let Some(ClientShellOverlay::ViewAction(value)) = state.overlay.as_ref() else {
        panic!("dialog")
    };
    let mut buffer = Buffer::empty(Rect::new(0, 0, 100, 32));
    let rendered = render_dialog(&mut buffer, value, &state.config.palette).unwrap();
    state.hits.view_action_popup = rendered.area;
    state.hits.overlay_primary = rendered.primary;
    state.hits.overlay_cancel = rendered.cancel;
    dialog::layout(value, rendered.area).options
}
fn choose(state: &mut ClientShellState, index: usize) -> ClientShellInput {
    let rows = paint(state);
    let rect = rows.iter().find(|(_, i)| *i == index).unwrap().0;
    let mut outcome = ClientShellInput::default();
    assert!(state.handle_view_action_dialog_mouse(click(rect.x, rect.y), &mut outcome));
    outcome
}
fn pending(state: &ClientShellState) -> (String, Intent, bool) {
    state
        .pending_requests
        .iter()
        .find_map(|(id, pending)| match &pending.kind {
            PendingEndpointKind::ViewAction { intent, submitting } => {
                Some((id.clone(), *intent.clone(), *submitting))
            }
            _ => None,
        })
        .unwrap()
}
fn prepared(operation_id: String, irreversible: bool) -> ResponseResult {
    ResponseResult::HostViewActionPrepared {
        token: "prepared-token".into(),
        confirmation_token: irreversible.then(|| "confirmation-token".into()),
        requires_confirmation: irreversible,
        expires_in_ms: 60000,
        operation_id,
    }
}
fn is_submit(action: &ClientShellAction, cancel: bool) -> bool {
    matches!(action,ClientShellAction::Endpoint{request,..} if matches!(&request.method,Method::HostViewActionSubmit(params) if params.cancel==cancel))
}
#[test]
fn choosing_is_explicit_and_never_applies_the_default() {
    let mut state = fixture(true, false);
    assert!(open(&mut state).actions.is_empty());
    paint(&mut state);
    let mut outcome = ClientShellInput::default();
    state.handle_view_action_dialog_key(
        crossterm::event::KeyEvent::new(crossterm::event::KeyCode::Enter, KeyModifiers::NONE),
        &mut outcome,
    );
    assert!(outcome.actions.is_empty());
    assert!(state.pending_requests.is_empty());
    let actions = choose(&mut state, 0);
    assert!(!actions.actions.is_empty());
    let (_, intent, submitting) = pending(&state);
    assert!(!submitting);
    assert_eq!(intent.selection.option.as_deref(), Some("go"));
    assert_eq!(
        (intent.selection.generation, intent.selection.revision),
        (7, 8)
    );
    assert_eq!(
        intent.selection.terminal_scope,
        vec!["pane_1", "hidden_terminal"]
    );
}
#[test]
fn irreversible_choice_requires_preparation_paint_and_explicit_confirmation() {
    let mut state = fixture(true, false);
    open(&mut state);
    choose(&mut state, 0);
    let (id, intent, _) = pending(&state);
    let (_, actions) = state.handle_endpoint_result(
        "boot-1",
        &id,
        Ok(prepared(intent.selection.operation_id.clone(), true)),
    );
    assert!(actions.is_empty());
    let mut outcome = ClientShellInput::default();
    state.activate_view_action_dialog(None, &mut outcome);
    assert!(
        outcome.actions.is_empty(),
        "no confirmation before the dialog is composed"
    );
    paint(&mut state);
    let rect = state.hits.overlay_primary;
    state.handle_view_action_dialog_mouse(click(rect.x, rect.y), &mut outcome);
    assert!(outcome
        .actions
        .iter()
        .any(|action| is_submit(action, false)));
    let (_, submitted, submitting) = pending(&state);
    assert!(submitting);
    assert_eq!(submitted.selection, intent.selection);
}
#[test]
fn accept_and_reversible_answers_submit_only_after_matching_prepare() {
    for choice in [false, true] {
        let mut state = fixture(choice, true);
        open(&mut state);
        if choice {
            choose(&mut state, 1);
        }
        let (id, intent, _) = pending(&state);
        let (_, actions) = state.handle_endpoint_result(
            "boot-1",
            &id,
            Ok(prepared(intent.selection.operation_id.clone(), false)),
        );
        assert!(actions.iter().any(|action| is_submit(action, false)));
        let (_, submitted, submitting) = pending(&state);
        assert!(submitting);
        assert_eq!(submitted.selection, intent.selection);
    }
}
#[test]
fn cancel_before_prepare_reply_discards_the_token_without_submitting() {
    let mut state = fixture(false, true);
    open(&mut state);
    let (id, intent, _) = pending(&state);
    state.cancel_view_action_dialog(&mut ClientShellInput::default());
    let (_, actions) = state.handle_endpoint_result(
        "boot-1",
        &id,
        Ok(prepared(intent.selection.operation_id, false)),
    );
    assert!(actions.iter().any(|action| is_submit(action, true)));
    assert!(!actions.iter().any(|action| is_submit(action, false)));
    assert!(state.overlay.is_none());
}
#[test]
fn irreversible_cancel_uses_cancel_not_write() {
    let mut state = fixture(true, false);
    open(&mut state);
    choose(&mut state, 1);
    let (id, intent, _) = pending(&state);
    state.handle_endpoint_result(
        "boot-1",
        &id,
        Ok(prepared(intent.selection.operation_id, true)),
    );
    let mut outcome = ClientShellInput::default();
    state.cancel_view_action_dialog(&mut outcome);
    assert!(outcome.actions.iter().any(|action| is_submit(action, true)));
    assert!(!outcome
        .actions
        .iter()
        .any(|action| is_submit(action, false)));
}
#[test]
fn scope_change_after_prepare_does_not_submit_or_replace_intent() {
    let mut state = fixture(false, true);
    open(&mut state);
    let (id, intent, _) = pending(&state);
    state.pane_surface_views[0].scope.push("new-member".into());
    let (_, actions) = state.handle_endpoint_result(
        "boot-1",
        &id,
        Ok(prepared(intent.selection.operation_id, false)),
    );
    assert!(!actions.iter().any(|action| is_submit(action, false)));
    assert!(actions.iter().any(|action| is_submit(action, true)));
}
#[test]
fn publication_before_repaint_rejects_the_old_body_click() {
    let mut state = fixture(false, true);
    state.view_action_composed.as_mut().unwrap().1 = 0;
    assert!(open(&mut state).actions.is_empty());
    assert!(state.pending_requests.is_empty());
    assert!(state.overlay.is_none());
}
#[test]
fn terminal_content_patch_does_not_invalidate_unchanged_view_meaning() {
    let mut state = fixture(true, false);
    state.pane_surface.as_mut().unwrap().surface_revision = 2;
    assert!(open(&mut state).actions.is_empty());
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::ViewAction(_))
    ));
}
#[test]
fn document_change_after_prepare_cancels_without_upgrading_revision() {
    let mut state = fixture(false, true);
    open(&mut state);
    let (id, intent, _) = pending(&state);
    state.endpoints[0]
        .host_documents
        .as_mut()
        .unwrap()
        .documents[0]
        .document
        .revision += 1;
    let (_, actions) = state.handle_endpoint_result(
        "boot-1",
        &id,
        Ok(prepared(intent.selection.operation_id, false)),
    );
    assert!(!actions.iter().any(|action| is_submit(action, false)));
    assert!(actions.iter().any(|action| is_submit(action, true)));
}
#[test]
fn released_or_repeated_input_cannot_choose_or_confirm() {
    let mut state = fixture(true, false);
    open(&mut state);
    paint(&mut state);
    let mut outcome = ClientShellInput::default();
    for kind in [
        crossterm::event::KeyEventKind::Release,
        crossterm::event::KeyEventKind::Repeat,
    ] {
        state.handle_view_action_dialog_key(
            crossterm::event::KeyEvent::new_with_kind(
                crossterm::event::KeyCode::Enter,
                KeyModifiers::NONE,
                kind,
            ),
            &mut outcome,
        );
    }
    assert!(outcome.actions.is_empty());
    assert!(state.pending_requests.is_empty());
}
#[test]
fn unadvertised_methods_never_use_legacy_actions() {
    let mut state = fixture(false, true);
    state.set_endpoint_methods(Some(vec!["host.document.action".into()]));
    assert!(open(&mut state).actions.is_empty());
    assert!(state.pending_requests.is_empty());
}
#[test]
fn provider_error_and_timeout_remain_inspectable_without_retry() {
    for provider in [true, false] {
        let mut state = fixture(false, true);
        open(&mut state);
        let (id, intent, _) = pending(&state);
        state.handle_endpoint_result(
            "boot-1",
            &id,
            Ok(prepared(intent.selection.operation_id.clone(), false)),
        );
        let (id, _, submitting) = pending(&state);
        assert!(submitting);
        let result = if provider {
            Ok(ResponseResult::HostViewActionSubmitted {
                operation_id: intent.selection.operation_id.clone(),
                caller: json!({}),
                provider_response: json!({"error":{"code":"provider_error","message":"explicit policy rejection"}}),
                audit_warning: Some("audit warning".into()),
            })
        } else {
            Err(ClientShellEndpointError {
                code: Some("endpoint_timeout".into()),
                message: "timeout".into(),
            })
        };
        let (_, actions) = state.handle_endpoint_result("boot-1", &id, result);
        assert!(actions.is_empty());
        assert!(state.pending_requests.is_empty());
        let Some(ClientShellOverlay::ViewAction(Dialog {
            phase: Phase::Outcome(message),
            ..
        })) = &state.overlay
        else {
            panic!("inspectable outcome")
        };
        assert!(message.contains(&intent.selection.operation_id));
        assert!(message.contains("retry"));
    }
}
#[test]
fn action_buttons_change_only_the_expanded_body_not_the_frame() {
    let state = fixture(false, true);
    let documents = state.endpoints[0].current_host_documents().unwrap();
    let mut readonly = documents.clone();
    readonly.documents[0].document.items[1]
        .view
        .as_mut()
        .unwrap()
        .affordance = None;
    let pane = &state.hits.panes[0];
    let scope = vec!["pane_1".into(), "hidden_terminal".into()];
    let projection = ViewPaneProjection {
        extension: "example.docs",
        view: "work",
        scope: &scope,
    };
    let render = |documents: &HostDocumentsProjectionV1| {
        let mut buffer = Buffer::empty(pane.rect);
        render_view_pane(
            &mut buffer,
            &PanelIndex::with_rows(documents, true),
            documents,
            state.snapshot.as_ref().unwrap(),
            pane,
            &projection,
            &state.config.palette,
        );
        buffer
    };
    let baseline = render(&readonly);
    let action = render(documents);
    for y in pane.rect.y..pane.rect.bottom() {
        for x in pane.rect.x..pane.rect.right() {
            if !super::super::super::contains(pane.inner_rect, (x, y)) {
                assert_eq!(baseline[(x, y)], action[(x, y)]);
            }
        }
    }
    let text = action
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(text.contains("[Accept]"));
}
#[test]
fn full_question_and_options_are_scrollable_not_silently_truncated() {
    let mut state = fixture(true, false);
    open(&mut state);
    let Some(ClientShellOverlay::ViewAction(dialog)) = &mut state.overlay else {
        panic!("dialog")
    };
    if let Affordance::Choice {
        detail, options, ..
    } = &mut dialog.intent.affordance
    {
        *detail = "long bounded question ".repeat(40);
        options[1].label = "last option text ".repeat(12);
    }
    let layout = dialog::layout(dialog, Rect::new(0, 0, 50, 14));
    assert!(layout.max_scroll > 0);
    let all = layout
        .lines
        .iter()
        .map(|(line, _)| line.as_str())
        .collect::<String>();
    assert!(all.contains("irreversible"));
    assert!(all.contains("(default)"));
    assert!(all.contains("last option text"));
}
