use super::*;
use crate::protocol::collapse::{CollapseProjectionV1, LAYOUT_COLLAPSE_KIND};

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
