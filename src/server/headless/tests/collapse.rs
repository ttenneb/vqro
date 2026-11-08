use super::*;
use crate::protocol::collapse::{CollapseProjectionV1, LAYOUT_COLLAPSE_KIND};

#[tokio::test]
async fn collapsed_terminal_title_schedules_only_its_visible_header_consumer() {
    let (mut server, _control) = window_title_test_server();
    server.app.configure_window_title("fixed");
    server.app.state.sidebar_agents.rows.clear();
    server.app.state.sidebar_agents.rows_by_agent.clear();
    server.app.state.ensure_test_terminals();
    let pane = server.app.state.workspaces[0].tabs[0].root_pane;
    let terminal = server.app.state.workspaces[0]
        .terminal_id(pane)
        .unwrap()
        .clone();
    assert!(server.app.state.workspaces[0].tabs[0]
        .layout
        .set_collapsed(&[], true));
    let runtime = crate::terminal::TerminalRuntime::test_with_screen_bytes(
        80,
        24,
        b"\x1b]0;changed collapsed title\x07",
    );
    server
        .app
        .terminal_runtimes
        .insert(terminal.clone(), runtime);
    let _ = server.app.render_dirty.take();
    server.app.render_dirty.request_terminal_title(pane);
    // An API metadata read may synchronize the title before the render loop.
    server.app.sync_pending_terminal_titles();
    let request = server.app.render_dirty.take();
    let (presentation, _) = server.sync_terminal_title_sources(&request.terminal_title_sources);
    assert!(
        presentation,
        "visible header title must schedule presentation without sidebar tokens"
    );
    assert!(
        !server.sync_terminal_title_sources(&HashSet::new()).0,
        "without a new title event there is no header repaint"
    );
    for client in server.clients.values_mut() {
        client.shell_surface_active = false;
    }
    server
        .app
        .terminal_runtimes
        .get(&terminal)
        .unwrap()
        .test_process_pty_bytes(b"\x1b]0;hidden title\x07");
    assert!(
        !server.sync_terminal_title_sources(&HashSet::from([pane])).0,
        "a header with no active surface does not repaint"
    );
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn zoom_focus_or_restored_flag_never_exposes_collapsed_bodies_or_latent_zoom_on_expand() {
    for containing_box in [false, true] {
        for restored_flag in [false, true] {
            let mut server = test_headless_server();
            let mut hidden_input = install_focused_test_runtime(&mut server, b"live");
            let root = server.app.state.workspaces[0].tabs[0].root_pane;
            let pane_id = server.app.public_pane_id(0, root).unwrap();
            let sibling =
                server.app.state.workspaces[0].test_split(ratatui::layout::Direction::Horizontal);
            let (runtime, mut sibling_input) =
                crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
                    80,
                    24,
                    0,
                    b"visible sibling\x1b[?1000h\x1b[?1006h\x1b[?1016h",
                    4,
                );
            server.app.state.workspaces[0].insert_test_runtime(sibling, runtime);
            if containing_box {
                server.app.state.workspaces[0]
                    .split_pane_with_view(
                        root,
                        "example.views",
                        "summary",
                        ratatui::layout::Direction::Vertical,
                        0.5,
                        false,
                    )
                    .unwrap();
            }
            server.app.state.ensure_test_terminals();
            server.app.state.focus_pane_in_workspace(0, root);
            let (_control, render) = add_client(&mut server, 1, true);
            server.render_and_stream();
            let (initial, _) = decode_pair(render.try_recv().unwrap());
            let original = initial
                .panes
                .iter()
                .find(|pane| pane.pane_id == pane_id)
                .unwrap()
                .rect;
            let size = server
                .app
                .state
                .runtime_for_pane_in_workspace(&server.app.terminal_runtimes, 0, root)
                .unwrap()
                .current_size();
            assert!(server.app.set_node_collapsed(0, 0, &[0], true));
            if restored_flag {
                server.app.state.workspaces[0].tabs[0].zoomed = true;
            } else {
                server
                    .app
                    .state
                    .apply_pane_zoom(0, sibling, crate::app::actions::PaneZoomCommand::On)
                    .unwrap();
                server.app.state.focus_pane_in_workspace(0, root);
            }
            server.clients.get_mut(&1).unwrap().request_repaint();
            server.render_and_stream();
            let (hidden, _) = decode_pair(render.try_recv().unwrap());
            assert!(
                hidden.panes.iter().all(|pane| pane.pane_id != pane_id),
                "focus/restore must not bypass collapsed body visibility"
            );
            assert!(
                server.any_shell_surface_contains_pane(sibling),
                "displayed sibling output must remain visible"
            );
            server.sync_immediate_pty_sources();
            let _ = server.app.render_dirty.take();
            server.app.render_dirty.request_pty(sibling);
            assert!(
                server.app.render_dirty.has_immediate_work(),
                "displayed sibling output is immediate"
            );
            let _ = server.app.render_dirty.take();
            server.app.render_dirty.request_pty(root);
            assert!(
                !server.app.render_dirty.has_immediate_work(),
                "hidden body output is not immediate"
            );
            let _ = server.app.render_dirty.take();
            assert!(
                server.shell_visible_pane_wants_sgr_pixels(1),
                "visible sibling pixel mouse must remain eligible"
            );
            let projected = crate::server::client_shell::snapshot(
                &server.app,
                &server.client_shell_boot_id,
                1,
                None,
                None,
            );
            assert!(
                !projected.tabs[0].zoomed,
                "ZOOM status must describe effective geometry"
            );
            let sibling_id = server.app.public_pane_id(0, sibling).unwrap();
            server.handle_server_event(ServerEvent::ClientShellPaneInput {
                client_id: 1,
                pane_id: sibling_id,
                events: vec![crate::protocol::ClientPaneInputEvent::TextCommit(
                    "visible-sibling".into(),
                )],
            });
            assert_eq!(
                sibling_input
                    .try_recv()
                    .expect("visible sibling gets input")
                    .as_ref(),
                b"visible-sibling"
            );
            server.handle_server_event(ServerEvent::ClientShellPaneInput {
                client_id: 1,
                pane_id: pane_id.clone(),
                events: vec![crate::protocol::ClientPaneInputEvent::TextCommit(
                    "hidden".into(),
                )],
            });
            assert!(hidden_input.try_recv().is_err());
            assert_eq!(
                server
                    .app
                    .state
                    .runtime_for_pane_in_workspace(&server.app.terminal_runtimes, 0, root)
                    .unwrap()
                    .current_size(),
                size
            );
            assert!(server.app.set_node_collapsed(0, 0, &[0], false));
            server.clients.get_mut(&1).unwrap().request_repaint();
            server.render_and_stream();
            let (expanded, _) = decode_pair(render.try_recv().unwrap());
            assert_eq!(
                expanded
                    .panes
                    .iter()
                    .find(|pane| pane.pane_id == pane_id)
                    .unwrap()
                    .rect,
                original,
                "expand restores shares, not a latent zoom mode"
            );
            shutdown_test_runtimes(&mut server);
        }
    }
}

#[tokio::test]
async fn later_zoom_on_or_toggle_cannot_reveal_a_collapsed_leaf_or_containing_box() {
    for containing_box in [false, true] {
        let mut server = test_headless_server();
        let _input = install_focused_test_runtime(&mut server, b"live");
        let root = server.app.state.workspaces[0].tabs[0].root_pane;
        let pane_id = server.app.public_pane_id(0, root).unwrap();
        server.app.state.workspaces[0]
            .split_pane_with_view(
                root,
                "example.views",
                "summary",
                ratatui::layout::Direction::Horizontal,
                0.5,
                false,
            )
            .unwrap();
        let (_control, render) = add_client(&mut server, 1, true);
        server.render_and_stream();
        let _ = render.try_recv().unwrap();
        let size = server
            .app
            .state
            .runtime_for_pane_in_workspace(&server.app.terminal_runtimes, 0, root)
            .unwrap()
            .current_size();
        let method = if containing_box {
            api::schema::Method::LayoutCollapse(api::schema::LayoutCollapseTarget {
                boot_id: server.app.runtime_incarnation.clone(),
                tab_id: server.app.public_tab_id(0, 0).unwrap(),
                tree_fingerprint: server.app.state.workspaces[0].tabs[0]
                    .layout
                    .tree_fingerprint()
                    .into(),
                path: Vec::new(),
            })
        } else {
            api::schema::Method::PaneCollapse(api::schema::PaneTarget {
                pane_id: pane_id.clone(),
            })
        };
        let response = server.app.handle_api_request(api::schema::Request {
            id: "collapse-before-zoom".into(),
            method,
        });
        assert!(serde_json::from_str::<serde_json::Value>(&response)
            .unwrap()
            .get("error")
            .is_none());
        for mode in [
            api::schema::PaneZoomMode::On,
            api::schema::PaneZoomMode::Toggle,
        ] {
            let response = server.app.handle_api_request(api::schema::Request {
                id: "zoom-collapsed".into(),
                method: api::schema::Method::PaneZoom(api::schema::PaneZoomParams {
                    pane_id: Some(pane_id.clone()),
                    mode,
                }),
            });
            assert!(
                serde_json::from_str::<serde_json::Value>(&response)
                    .unwrap()
                    .get("error")
                    .is_some(),
                "zoom-on must not resurrect a collapsed body"
            );
            assert!(!server.app.state.workspaces[0].tabs[0].zoomed);
            assert!(!server.app.state.workspaces[0].tabs[0]
                .layout
                .pane_is_expanded(root));
            server.clients.get_mut(&1).unwrap().request_repaint();
            server.render_and_stream();
            let (surface, _) = decode_pair(render.try_recv().unwrap());
            assert!(surface.panes.iter().all(|pane| pane.pane_id != pane_id));
            assert_eq!(
                server
                    .app
                    .state
                    .runtime_for_pane_in_workspace(&server.app.terminal_runtimes, 0, root)
                    .unwrap()
                    .current_size(),
                size
            );
        }
        assert!(server
            .app
            .state
            .apply_pane_zoom(0, root, crate::app::actions::PaneZoomCommand::Toggle)
            .is_none());
        shutdown_test_runtimes(&mut server);
    }
}

#[tokio::test]
async fn two_clients_cannot_type_from_a_stale_legacy_surface_after_remote_collapse() {
    for containing_box in [false, true] {
        check_stale_client_input_after_collapse(containing_box).await;
    }
}

async fn check_stale_client_input_after_collapse(containing_box: bool) {
    let mut server = test_headless_server();
    let mut input = install_focused_test_runtime(&mut server, b"\x1b[>3u");
    let pane_id = server.app.session_snapshot().focused_pane_id.unwrap();
    if containing_box {
        let root = server.app.state.workspaces[0].tabs[0].root_pane;
        server.app.state.workspaces[0]
            .split_pane_with_view(
                root,
                "example.views",
                "summary",
                ratatui::layout::Direction::Horizontal,
                0.5,
                false,
            )
            .unwrap();
    }
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
            method: if containing_box {
                api::schema::Method::LayoutCollapse(api::schema::LayoutCollapseTarget {
                    boot_id: server.app.runtime_incarnation.clone(),
                    tab_id: server.app.public_tab_id(0, 0).unwrap(),
                    tree_fingerprint: server.app.state.workspaces[0].tabs[0]
                        .layout
                        .tree_fingerprint()
                        .into(),
                    path: Vec::new(),
                })
            } else {
                api::schema::Method::PaneCollapse(api::schema::PaneTarget {
                    pane_id: pane_id.clone(),
                })
            },
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
