use super::*;

fn scene_client(
    server: &mut HeadlessServer,
) -> (
    std::sync::mpsc::Receiver<Vec<u8>>,
    std::sync::mpsc::Receiver<Vec<u8>>,
) {
    let (writer, control, render) = test_client_writer();
    let mut client = ClientConnection::new(
        (100, 40),
        Default::default(),
        1,
        RenderEncoding::SemanticFrame,
        Some(writer),
    );
    client.shell_scene_enabled = true;
    client.shell_collapse_enabled = true;
    client.shell_structure_enabled = true;
    client.shell_projection_revision = 1;
    server.clients.insert(1, client);
    (control, render)
}
fn decode(bytes: Vec<u8>) -> Box<crate::protocol::scene::ClientSceneV1> {
    let mut cursor = std::io::Cursor::new(bytes);
    let ServerMessage::ClientSceneV1(scene) =
        protocol::read_message(&mut cursor, MAX_FRAME_SIZE).unwrap()
    else {
        panic!("single scene frame")
    };
    assert_eq!(
        cursor.position(),
        cursor.get_ref().len() as u64,
        "no companion frame after scene"
    );
    scene
}
fn revocations(control: &std::sync::mpsc::Receiver<Vec<u8>>) -> Vec<u64> {
    control
        .try_iter()
        .filter_map(|bytes| match read_server_message(bytes) {
            ServerMessage::EndpointControl { kind, data }
                if kind == crate::protocol::scene::ROUTE_REVOKED_KIND =>
            {
                Some(
                    serde_json::from_str::<crate::protocol::scene::SceneRouteRevokedV1>(&data)
                        .unwrap()
                        .route_epoch,
                )
            }
            ServerMessage::EndpointControl { kind, .. }
                if kind == crate::protocol::endpoint::ENDPOINT_SNAPSHOT_KIND
                    || kind == crate::protocol::collapse::LAYOUT_COLLAPSE_KIND
                    || kind == crate::protocol::endpoint::SESSION_STRUCTURE_KIND =>
            {
                panic!("active facts escaped atomic scene")
            }
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn atomic_scene_route_inventory_excludes_status_cells_and_scroll() {
    let mut server = test_headless_server();
    let _input = install_focused_test_runtime(&mut server, b"raster");
    server.app.state.ensure_test_terminals();
    let (_control, render) = scene_client(&mut server);
    server.render_and_stream();
    let first = decode(render.try_recv().unwrap());
    let snapshot: crate::protocol::ClientShellSnapshot = serde_json::from_str(
        &first
            .projections
            .iter()
            .find(|value| value.kind == crate::protocol::endpoint::ENDPOINT_SNAPSHOT_KIND)
            .unwrap()
            .data,
    )
    .unwrap();
    let witness = |snapshot: &crate::protocol::ClientShellSnapshot,
                   surface: &crate::protocol::PaneSurfaceFrame| {
        super::super::scene::route_witness(super::super::scene::SceneRoutingFacts {
            snapshot,
            structure: None,
            surface,
            views: &[],
            collapse: None,
            targets: &first.targets,
            previews: None,
            tree_fingerprint: None,
        })
        .unwrap()
    };
    let before = witness(&snapshot, &first.surface.frame);
    let mut harmless = snapshot.clone();
    harmless.revision += 1;
    harmless.tabs[0].agent_status = crate::api::schema::AgentStatus::Working;
    harmless.panes[0].cwd = Some("/changed-cwd".into());
    harmless
        .tab_bar_right
        .push(crate::protocol::ClientShellTabStatusSegment {
            text: "task status".into(),
            accent: true,
        });
    let mut raster = first.surface.frame.clone();
    raster.surface_revision += 1;
    raster.panes[0].content_revision += 1;
    raster.panes[0].scroll = Some(crate::protocol::PaneSurfaceScrollMetrics {
        offset_from_bottom: 1,
        max_offset_from_bottom: 10,
        viewport_rows: 24,
    });
    assert_eq!(witness(&harmless, &raster), before);
    raster.panes[0].mouse_reporting = !raster.panes[0].mouse_reporting;
    assert_ne!(witness(&harmless, &raster), before);
    raster = first.surface.frame.clone();
    raster.panes[0].pixel_width += 1;
    assert_ne!(
        witness(&snapshot, &raster),
        before,
        "same cells but changed pixel geometry revokes"
    );
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn atomic_scene_full_render_slot_keeps_harmless_pending_publication_on_old_route() {
    let mut server = test_headless_server();
    let _input = install_focused_test_runtime(&mut server, b"stable raster");
    server.app.state.ensure_test_terminals();
    let (control, render) = scene_client(&mut server);
    server.render_and_stream();
    let first_epoch = server.clients[&1].shell_route_epoch;
    revocations(&control);
    // Keep the first full scene occupying the bounded render slot.
    server.app.state.workspaces[0].custom_name = Some("harmless label churn".into());
    server.render_and_stream();
    assert_eq!(server.clients[&1].shell_route_epoch, first_epoch);
    assert!(revocations(&control).is_empty());
    let first = decode(render.try_recv().unwrap());
    assert_eq!(first.route_epoch, first_epoch);
    server.render_and_stream();
    let second = decode(render.try_recv().unwrap());
    assert_eq!(second.route_epoch, first_epoch);
    assert!(second.surface.frame.projection_revision > first.surface.frame.projection_revision);
    server.app.state.assert_invariants_for_test();
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn atomic_scene_runtime_replacement_and_geometry_revoke_even_if_cells_do_not_change() {
    let mut server = test_headless_server();
    let _input = install_focused_test_runtime(&mut server, b"same");
    server.app.state.ensure_test_terminals();
    let (control, render) = scene_client(&mut server);
    server.render_and_stream();
    let first = decode(render.try_recv().unwrap());
    revocations(&control);
    let pane = server.app.state.workspaces[0].tabs[0].root_pane;
    let (replacement, _rx) =
        crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
            80, 24, 0, b"same", 4,
        );
    server.app.state.workspaces[0].insert_test_runtime(pane, replacement);
    server.render_and_stream();
    let second = decode(render.try_recv().unwrap());
    assert!(second.route_epoch > first.route_epoch);
    assert_eq!(first.targets[0].terminal_id, second.targets[0].terminal_id);
    assert_ne!(
        first.targets[0].runtime_instance,
        second.targets[0].runtime_instance
    );
    assert_eq!(revocations(&control), vec![second.route_epoch]);
    server.clients.get_mut(&1).unwrap().terminal_size = (100, 41);
    server.render_and_stream();
    let third = decode(render.try_recv().unwrap());
    assert!(third.route_epoch > second.route_epoch);
    assert_eq!(revocations(&control), vec![third.route_epoch]);
    server.app.state.assert_invariants_for_test();
    shutdown_test_runtimes(&mut server);
}
