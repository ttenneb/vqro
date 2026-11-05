use super::*;
use crate::protocol::collapse::{CollapseProjectionV1, LAYOUT_COLLAPSE_KIND};

#[tokio::test]
async fn two_clients_cannot_type_from_a_stale_legacy_surface_after_remote_collapse() {
    let mut server = test_headless_server();
    let mut input = install_focused_test_runtime(&mut server, b"\x1b[>3u");
    let pane_id = server.app.session_snapshot().focused_pane_id.unwrap();
    let (_control1, render1) = add_client(&mut server, 1, false);
    let (_control2, _render2) = add_client(&mut server, 2, true);
    server.render_and_stream();
    let ServerMessage::PaneSurfaceV2(stale) = read_server_message(render1.recv().unwrap()) else {
        panic!("legacy full surface")
    };
    assert!(stale.frame.panes.iter().any(|pane| pane.pane_id == pane_id));
    let key = |kind| crate::protocol::ClientPaneInputEvent::Key {
        code: crate::protocol::ClientKeyCode::Char('x'),
        modifiers: 0,
        kind,
        repeat_count: 1,
        shifted_codepoint: None,
        generated_text: (kind == crate::protocol::ClientKeyKind::Press).then(|| "x".to_owned()),
        tracks_release: true,
        physical_key_id: Some(0x2d),
        windows_record: None,
    };
    server.handle_server_event(ServerEvent::ClientShellPaneInput {
        client_id: 1,
        pane_id: pane_id.clone(),
        events: vec![key(crate::protocol::ClientKeyKind::Press)],
    });
    assert!(!input.recv().await.unwrap().is_empty());
    server.handle_server_event(ServerEvent::NativeEndpointRequest {
        client_id: 2,
        boot_id: server.client_shell_boot_id.clone(),
        request: Box::new(api::schema::Request {
            id: "collapse-from-other-client".into(),
            method: api::schema::Method::PaneCollapse(api::schema::PaneTarget {
                pane_id: pane_id.clone(),
            }),
        }),
    });
    assert!(server.app.state.workspaces[0].tabs[0]
        .layout
        .typed_root()
        .has_visible_collapse());
    server.promote_client_to_foreground(2);
    // Client 1 has not received/applied a replacement surface. Its previously
    // valid target must still be rejected from current canonical host state.
    assert!(
        !server.handle_server_event(ServerEvent::ClientShellPaneInput {
            client_id: 1,
            pane_id: pane_id.clone(),
            events: vec![
                key(crate::protocol::ClientKeyKind::Press),
                crate::protocol::ClientPaneInputEvent::TextCommit("stale-text".into()),
                crate::protocol::ClientPaneInputEvent::Paste("stale-paste".into()),
            ],
        }),
        "collapsed body input cannot claim geometry or foreground"
    );
    assert!(
        input.try_recv().is_err(),
        "stale client wrote into a collapsed PTY"
    );
    assert_eq!(server.foreground_client_id, Some(2));
    assert!(
        !server.handle_server_event(ServerEvent::ClientShellPaneInput {
            client_id: 1,
            pane_id: pane_id.clone(),
            events: vec![key(crate::protocol::ClientKeyKind::Release)]
        })
    );
    assert!(
        !input.recv().await.unwrap().is_empty(),
        "held release must survive remote collapse"
    );
    assert_eq!(server.foreground_client_id, Some(2));
    let response = server.app.handle_api_request(api::schema::Request {
        id: "authorized-explicit-write".into(),
        method: api::schema::Method::PaneSendText(api::schema::PaneSendTextParams {
            pane_id,
            text: "authorized".into(),
        }),
    });
    assert!(serde_json::from_str::<serde_json::Value>(&response)
        .unwrap()
        .get("error")
        .is_none());
    assert_eq!(input.recv().await.unwrap().as_ref(), b"authorized");
    assert!(server.app.state.workspaces[0].tabs[0]
        .layout
        .typed_root()
        .has_visible_collapse());
    shutdown_test_runtimes(&mut server);
}

fn decode_pair(bytes: Vec<u8>) -> (crate::protocol::PaneSurfaceFrame, CollapseProjectionV1) {
    let mut cursor = std::io::Cursor::new(bytes);
    let ServerMessage::PaneSurfaceV2(surface) =
        protocol::read_message(&mut cursor, MAX_FRAME_SIZE).unwrap()
    else {
        panic!("full v2 surface")
    };
    let ServerMessage::EndpointControl { kind, data } =
        protocol::read_message(&mut cursor, MAX_FRAME_SIZE).unwrap()
    else {
        panic!("collapse companion")
    };
    assert_eq!(kind, LAYOUT_COLLAPSE_KIND);
    let projection: CollapseProjectionV1 = serde_json::from_str(&data).unwrap();
    projection.validate().unwrap();
    assert!(projection.matches_surface(&surface.frame, projection.body.tab_id.as_deref()));
    assert_eq!(cursor.position(), cursor.get_ref().len() as u64);
    (surface.frame, projection)
}
fn add_client(
    server: &mut HeadlessServer,
    id: u64,
    enabled: bool,
) -> (
    std::sync::mpsc::Receiver<Vec<u8>>,
    std::sync::mpsc::Receiver<Vec<u8>>,
) {
    let (writer, control, render) = test_client_writer();
    let mut client = ClientConnection::new(
        (100, 40),
        crate::kitty_graphics::HostCellSize::default(),
        id,
        RenderEncoding::SemanticFrame,
        Some(writer),
    );
    client.shell_collapse_enabled = enabled;
    client.shell_projection_revision = 1;
    server.clients.insert(id, client);
    (control, render)
}

#[tokio::test]
async fn collapsed_root_initial_surface_has_header_target_and_no_body_descriptors() {
    let mut server = test_headless_server();
    let mut workspace = crate::workspace::Workspace::test_new("collapsed root");
    let pane = workspace.tabs[0].root_pane;
    workspace.insert_test_runtime(
        pane,
        crate::terminal::TerminalRuntime::test_with_screen_bytes(80, 24, b"hidden body"),
    );
    assert!(workspace.tabs[0].layout.set_collapsed(&[], true));
    server.app.state.workspaces = vec![workspace];
    server.app.state.active = Some(0);
    server.app.state.ensure_test_terminals();
    let (_control, render) = add_client(&mut server, 1, true);
    server.render_and_stream();
    let (surface, projection) = decode_pair(render.recv().unwrap());
    assert!(surface.panes.is_empty());
    assert!(surface.graphics.placements.is_empty());
    assert_eq!(projection.body.headers.len(), 1);
    assert!(projection.body.headers[0].collapsed);
    assert!(projection.body.headers[0].focused);
    assert!(projection.body.headers[0].path.is_empty());
    assert!(!frame_text(&surface.frame).contains("hidden body"));
    server.app.state.assert_invariants_for_test();
}

#[tokio::test]
async fn hidden_metadata_change_forces_new_pair_but_legacy_peer_keeps_its_identical_baseline() {
    let mut server = test_headless_server();
    let mut workspace = crate::workspace::Workspace::test_new("metadata only");
    let root = workspace.tabs[0].root_pane;
    let other = workspace.test_split(ratatui::layout::Direction::Horizontal);
    let stack = crate::terminal_container::TerminalContainerId::parse(917_001).unwrap();
    assert!(workspace.tabs[0].layout.new_stack_at(&[], stack));
    assert!(workspace.tabs[0].layout.move_into_stack(&[0, 1], stack, 1));
    assert!(workspace.tabs[0].layout.select_stack_child(stack, 0));
    for pane in [root, other] {
        workspace.insert_test_runtime(
            pane,
            crate::terminal::TerminalRuntime::test_with_screen_bytes(80, 24, b"stable raster"),
        );
    }
    server.app.state.workspaces = vec![workspace];
    server.app.state.active = Some(0);
    server.app.state.ensure_test_terminals();
    let (_modern_control, modern) = add_client(&mut server, 1, true);
    let (_legacy_control, legacy) = add_client(&mut server, 2, false);
    server.render_and_stream();
    let (before, old_projection) = decode_pair(modern.recv().unwrap());
    let bytes = legacy.recv().unwrap();
    let mut legacy_cursor = std::io::Cursor::new(bytes);
    assert!(matches!(
        protocol::read_message::<_, ServerMessage>(&mut legacy_cursor, MAX_FRAME_SIZE).unwrap(),
        ServerMessage::PaneSurfaceV2(_)
    ));
    assert_eq!(
        legacy_cursor.position(),
        legacy_cursor.get_ref().len() as u64
    );
    assert!(server.app.state.workspaces[0].tabs[0]
        .layout
        .set_collapsed(&[1], true));
    server.render_and_stream();
    let (after, new_projection) = decode_pair(modern.recv().unwrap());
    assert_eq!(after.frame, before.frame);
    assert_eq!(after.projection_revision, before.projection_revision);
    assert!(after.surface_revision > before.surface_revision);
    assert_ne!(
        new_projection.body.tree_fingerprint,
        old_projection.body.tree_fingerprint
    );
    assert!(
        legacy.try_recv().is_err(),
        "legacy peer does not need an optional companion or metadata-only full frame"
    );
    server.app.state.assert_invariants_for_test();
}
