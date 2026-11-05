use super::collapse::{feature_shell, projection};
use super::*;
use crate::{
    api::schema::{LayoutCollapseTarget, Method},
    layout::{LayoutPathStep, LayoutSplitChild},
    protocol::collapse::*,
};
use crossterm::event::{KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind};

fn header_state(collapsed: bool) -> ClientShellState {
    let mut state = feature_shell();
    let mut frame = surface();
    let mut metadata = projection(&frame);
    if collapsed {
        frame.panes.clear();
        metadata.body.anchors.clear();
    }
    metadata.body.headers.push(CollapseHeaderV1 {
        rect: SurfaceRect {
            x: 0,
            y: 0,
            width: 4,
            height: 1,
        },
        action_rect: SurfaceRect {
            x: 1,
            y: 0,
            width: if collapsed { 3 } else { 1 },
            height: 1,
        },
        path: Vec::new(),
        collapsed,
        focused: collapsed,
    });
    state.set_pane_surface_with_views_for_generation(
        &ClientEndpointId::Local,
        1,
        frame,
        Vec::new(),
    );
    state.set_endpoint_collapse_projection_for_generation(
        &ClientEndpointId::Local,
        1,
        Ok(metadata),
    );
    state.compose(80, 24).unwrap();
    state
}
fn method(input: &ClientShellInput) -> Option<&Method> {
    input.actions.iter().find_map(|action| match action {
        ClientShellAction::Endpoint { request, .. } => Some(&request.method),
        _ => None,
    })
}
fn mouse(rect: Rect, kind: MouseEventKind) -> MouseEvent {
    MouseEvent {
        kind,
        column: rect.x,
        row: rect.y,
        modifiers: KeyModifiers::NONE,
    }
}

#[test]
fn header_mouse_and_default_key_use_the_same_fenced_desired_state() {
    let mut state = header_state(false);
    let hit = state.hits.collapse_headers[0].rect;
    let clicked = state.handle_raw_events(vec![RawInputEvent::Mouse(mouse(
        hit,
        MouseEventKind::Down(MouseButton::Left),
    ))]);
    let keyed = state.handle_input_bytes(b"\x02m");
    assert!(matches!(method(&clicked), Some(Method::LayoutCollapse(_))));
    assert_eq!(method(&clicked), method(&keyed));
    assert!(clicked.requests.is_empty());
    assert!(keyed.requests.is_empty());
}

#[test]
fn cold_header_consumes_typing_text_and_paste_but_the_key_expands_it() {
    let mut state = header_state(true);
    let typing = state.handle_input_bytes(b"do-not-send");
    assert!(typing.requests.is_empty());
    let committed = state.handle_raw_events(vec![
        RawInputEvent::Text(crate::input::TextCommit::new("text")),
        RawInputEvent::Paste("paste".into()),
    ]);
    assert!(committed.requests.is_empty());
    let keyed = state.handle_input_bytes(b"\x02m");
    assert!(matches!(method(&keyed), Some(Method::LayoutExpand(target)) if target.path.is_empty()));
    assert!(keyed.requests.is_empty());
}

#[test]
fn remote_collapse_suppresses_new_presses_without_losing_held_key_release_cleanup() {
    let mut state = header_state(false);
    state.set_host_reports_key_releases(true);
    let key = crate::input::TerminalKey::new(KeyCode::Char('a'), KeyModifiers::NONE);
    let pressed = state.handle_raw_events(vec![RawInputEvent::Key(key.clone())]);
    assert!(pressed.requests.iter().any(|request| matches!(request, ClientMessage::ClientShellPaneInput { events, .. } if events.iter().any(|event| matches!(event, ClientPaneInputEvent::Key { kind: crate::protocol::ClientKeyKind::Press, .. })))));
    let mut frame = surface();
    frame.surface_revision = 2;
    frame.panes.clear();
    let mut metadata = projection(&frame);
    metadata.body.anchors.clear();
    metadata.body.tree_fingerprint = Some("b".repeat(64));
    metadata.body.headers.push(CollapseHeaderV1 {
        rect: SurfaceRect {
            x: 0,
            y: 0,
            width: 4,
            height: 1,
        },
        action_rect: SurfaceRect {
            x: 0,
            y: 0,
            width: 4,
            height: 1,
        },
        path: Vec::new(),
        collapsed: true,
        focused: true,
    });
    state.set_endpoint_collapse_projection_for_generation(
        &ClientEndpointId::Local,
        1,
        Ok(metadata),
    );
    state.set_pane_surface_with_views_for_generation(
        &ClientEndpointId::Local,
        1,
        frame,
        Vec::new(),
    );
    let repeat = state.handle_raw_events(vec![RawInputEvent::Key(
        key.clone().with_kind(KeyEventKind::Repeat),
    )]);
    assert!(repeat.requests.is_empty());
    let released = state.handle_raw_events(vec![RawInputEvent::Key(
        key.with_kind(KeyEventKind::Release),
    )]);
    assert!(released.requests.iter().any(|request| matches!(request, ClientMessage::ClientShellPaneInput { events, .. } if events.iter().any(|event| matches!(event, ClientPaneInputEvent::Key { kind: crate::protocol::ClientKeyKind::Release, .. })))));
    let hidden_key = crate::input::TerminalKey::new(KeyCode::Char('b'), KeyModifiers::NONE);
    assert!(state
        .handle_raw_events(vec![RawInputEvent::Key(hidden_key.clone())])
        .requests
        .is_empty());
    assert!(
        state
            .handle_raw_events(vec![RawInputEvent::Key(
                hidden_key.with_kind(KeyEventKind::Release)
            )])
            .requests
            .is_empty(),
        "an unforwarded press must not acquire a release lease"
    );
}

#[test]
fn ancestor_picker_has_only_conditional_collapse_actions_and_rejects_stale_menu_targets() {
    let mut state = feature_shell();
    let frame = surface();
    let mut metadata = projection(&frame);
    metadata.body.anchors[0].path = vec![
        LayoutPathStep::StackChild(0),
        LayoutPathStep::Split(LayoutSplitChild::First),
    ];
    metadata.body.containers = vec![
        CollapseContainerV1 {
            path: Vec::new(),
            stack: true,
        },
        CollapseContainerV1 {
            path: vec![LayoutPathStep::StackChild(0)],
            stack: false,
        },
    ];
    state.set_endpoint_collapse_projection_for_generation(
        &ClientEndpointId::Local,
        1,
        Ok(metadata),
    );
    state.set_pane_surface_with_views_for_generation(
        &ClientEndpointId::Local,
        1,
        frame.clone(),
        Vec::new(),
    );
    let mut outcome = ClientShellInput::default();
    state.open_collapse_box_picker(Some("pane_1"), 1, 1, &mut outcome);
    let Some(ClientShellOverlay::ContextMenu(menu)) = state.overlay.as_ref() else {
        panic!("ancestor picker")
    };
    let items = menu.available_items(true);
    assert_eq!(items.len(), 2);
    assert!(items
        .iter()
        .all(|item| matches!(item.action, ClientContextMenuAction::CollapseAncestor(_))));
    let ClientContextMenuTarget::CollapseBoxes { targets } = &menu.target else {
        panic!("box targets")
    };
    assert_eq!(targets[0].path, vec![LayoutPathStep::StackChild(0)]);
    let mut next = frame;
    next.surface_revision = 2;
    let mut changed = projection(&next);
    changed.body.tree_fingerprint = Some("b".repeat(64));
    state.set_endpoint_collapse_projection_for_generation(&ClientEndpointId::Local, 1, Ok(changed));
    state.set_pane_surface_with_views_for_generation(&ClientEndpointId::Local, 1, next, Vec::new());
    let mut stale = ClientShellInput::default();
    state.activate_context_menu_item(0, &mut stale);
    assert!(stale.actions.is_empty());
}

#[test]
fn deep_ancestor_picker_scrolls_to_the_highlighted_target_without_new_box_selection() {
    let targets = (0..32)
        .map(|depth| LayoutCollapseTarget {
            boot_id: "boot-1".into(),
            tab_id: "tab_1".into(),
            tree_fingerprint: "a".repeat(64),
            path: vec![LayoutPathStep::StackChild(0); depth],
        })
        .collect();
    let menu = ClientContextMenuOverlay {
        target: ClientContextMenuTarget::CollapseBoxes { targets },
        x: 0,
        y: 0,
        highlighted: 31,
    };
    let mut buffer = Buffer::empty(Rect::new(0, 0, 40, 6));
    let config = ClientShellConfig::from_config(&Config::default());
    let rendered = render::render_context_menu(&mut buffer, &menu, &config.palette, true).unwrap();
    assert!(rendered.menu_rows.iter().any(|(_, index)| *index == 31));
}
