use super::*;

/// Test-only real negotiated publisher seam for the independent scaling profile.
pub(crate) struct SceneProfileHarness {
    server: HeadlessServer,
    control: std::sync::mpsc::Receiver<Vec<u8>>,
    render: std::sync::mpsc::Receiver<Vec<u8>>,
    stream: Option<crate::ipc::LocalStream>,
    worker: Option<std::thread::JoinHandle<std::io::Result<()>>>,
}

impl SceneProfileHarness {
    pub(crate) fn new(workspaces: Vec<crate::workspace::Workspace>, size: (u16, u16)) -> Self {
        let path = std::env::temp_dir().join(format!(
            "scene-profile-{}-{}.sock",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let listener = crate::ipc::bind_private_local_listener(&path).unwrap();
        let mut stream = crate::ipc::connect_local_stream(&path).unwrap();
        let accepted = listener.accept().unwrap();
        std::fs::remove_file(&path).unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        let quit = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let worker = std::thread::spawn(move || {
            crate::server::client_transport::handle_client_handshake(accepted, 1, &tx, &quit)
        });
        let hello = serde_json::json!({
            "generation": 2, "cell_width_px": 1, "cell_height_px": 1,
            "surface_size": {"cols": size.0, "rows": size.1}, "pixel_mouse": false,
            "direct_graphics": false, "endpoint_keybindings": false, "mouse_capture": false,
            "surface_active": true, "snapshot_codecs": ["shell.snapshot.v1"],
            "surface_codecs": ["shell.scene.v1"], "input_codecs": ["shell.input.semantic.v1"],
            "blob_codecs": ["shell.blob.v1"], "control_kinds": ["session.structure.v1", "layout.collapse.v1"]
        });
        protocol::write_message(
            &mut stream,
            &crate::protocol::ClientMessage::EndpointControl {
                kind: crate::protocol::endpoint::ENDPOINT_HELLO_KIND.into(),
                data: hello.to_string(),
            },
        )
        .unwrap();
        let ServerMessage::EndpointControl { kind, data } =
            protocol::read_message(&mut stream, MAX_FRAME_SIZE).unwrap()
        else {
            panic!("scene welcome")
        };
        assert_eq!(kind, crate::protocol::endpoint::ENDPOINT_WELCOME_KIND);
        let welcome: crate::protocol::endpoint::EndpointServerWelcome =
            serde_json::from_str(&data).unwrap();
        assert!(welcome.error.is_none());
        assert_eq!(
            welcome.surface_codec,
            crate::protocol::scene::SCENE_CODEC_V1
        );
        let mut event = std::thread::spawn(move || rx.blocking_recv().unwrap())
            .join()
            .unwrap();
        let (writer, control, render) = test_client_writer();
        let ServerEvent::ClientShellConnected { writer: output, .. } = &mut event else {
            panic!("negotiated shell connection")
        };
        *output = writer;
        let mut server = test_headless_server();
        server.app.state.workspaces = workspaces;
        server.app.state.active = Some(0);
        server.app.state.ensure_test_terminals();
        assert!(server.handle_server_event(event));
        assert!(server.clients[&1].shell_scene_enabled);
        Self {
            server,
            control,
            render,
            stream: Some(stream),
            worker: Some(worker),
        }
    }
    pub(crate) fn feed_output(&mut self) {
        for workspace in &self.server.app.state.workspaces {
            for runtime in workspace.test_runtimes.values() {
                runtime.test_process_pty_bytes(b"> atomic scene sample\r\n");
            }
        }
    }
    pub(crate) fn publish(&mut self) -> (Vec<Vec<u8>>, Vec<u8>) {
        self.server.clients.get_mut(&1).unwrap().request_repaint();
        self.server.render_and_stream();
        (
            self.control.try_iter().collect(),
            self.render.try_recv().unwrap(),
        )
    }
}
impl Drop for SceneProfileHarness {
    fn drop(&mut self) {
        self.stream.take();
        if let Some(worker) = self.worker.take() {
            worker.join().unwrap().unwrap();
        }
        shutdown_test_runtimes(&mut self.server);
    }
}

#[tokio::test]
async fn atomic_scene_profile_hook_negotiates_real_codec_and_emits_complete_transactions() {
    let mut workspace = crate::workspace::Workspace::test_new("profile hook");
    let pane = workspace.tabs[0].root_pane;
    workspace.insert_test_runtime(
        pane,
        crate::terminal::TerminalRuntime::test_with_screen_bytes(80, 24, b"populated"),
    );
    let mut harness = crate::server::headless::SceneProfileHarness::new(vec![workspace], (100, 40));
    let (_, bytes) = harness.publish();
    let first = decode(bytes);
    crate::client::DecodedScene::decode(*first.clone()).unwrap();
    harness.feed_output();
    let (_, bytes) = harness.publish();
    let second = decode(bytes);
    assert_eq!(first.route_epoch, second.route_epoch);
    assert!(second.surface.frame.surface_revision > first.surface.frame.surface_revision);
}

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
async fn atomic_scene_retained_graphics_refresh_stays_atomic_and_reuses_assets() {
    let mut server = test_headless_server();
    let _input = install_focused_test_runtime(&mut server, b"before image");
    server.app.state.ensure_test_terminals();
    let pane = server.app.state.workspaces[0].tabs[0].root_pane;
    let (_control, render) = scene_client(&mut server);
    server.clients.get_mut(&1).unwrap().cell_size = crate::kitty_graphics::HostCellSize {
        width_px: 10,
        height_px: 20,
    };
    server.foreground_client_id = Some(1);
    server.sync_foreground_client_state();
    assert!(server.claim_unowned_shell_tab_geometry(1, true));
    server.render_and_stream();
    let initial = decode(render.try_recv().unwrap());
    let mut shell = crate::client::ClientShellState::new(
        crate::client::ClientShellConfig::from_config(&crate::config::Config::default()),
    );
    shell.set_endpoint_capabilities_for(
        &crate::client::endpoint::ClientEndpointId::Local,
        vec![
            crate::protocol::collapse::LAYOUT_COLLAPSE_CAPABILITY.into(),
            crate::protocol::endpoint::SESSION_STRUCTURE_CAPABILITY.into(),
        ],
    );
    let install = |shell: &mut crate::client::ClientShellState,
                   scene: crate::protocol::scene::ClientSceneV1| {
        assert!(shell.install_scene(
            &crate::client::endpoint::ClientEndpointId::Local,
            1,
            crate::client::DecodedScene::decode(scene).unwrap(),
            true
        ));
    };
    install(&mut shell, *initial.clone());
    let image = b"\x1b_Ga=T,f=32,t=d,i=7,p=3,s=1,v=1,c=1,r=1,q=2;/wAA/w==\x1b\\";
    write_shared_test_pane(&mut server, pane, image);
    assert!(server.render_retained_pane_surface_and_stream(&HashSet::from([pane])));
    let with_image = decode(render.try_recv().unwrap());
    assert_eq!(with_image.route_epoch, initial.route_epoch);
    assert_eq!(with_image.targets, initial.targets);
    assert_eq!(
        with_image.surface.frame.graphics.assets[0].data,
        [255, 0, 0, 255]
    );
    install(&mut shell, *with_image.clone());
    write_shared_test_pane(&mut server, pane, b"\rchanged text");
    assert!(server.render_retained_pane_surface_and_stream(&HashSet::from([pane])));
    let reused = decode(render.try_recv().unwrap());
    assert_eq!(reused.route_epoch, initial.route_epoch);
    assert!(reused.surface.frame.graphics.assets.is_empty());
    assert_eq!(
        reused.surface.frame.graphics.placements,
        with_image.surface.frame.graphics.placements
    );
    install(&mut shell, *reused);
    write_shared_test_pane(&mut server, pane, b"\x1b_Ga=d,d=A\x1b\\");
    assert!(server.render_retained_pane_surface_and_stream(&HashSet::from([pane])));
    let deleted = decode(render.try_recv().unwrap());
    assert!(deleted.surface.frame.graphics.placements.is_empty());
    install(&mut shell, *deleted);
    let dummy = HeadlessServer::frame_server_message(&ServerMessage::ReloadSoundConfig).unwrap();
    server.clients[&1]
        .writer
        .as_ref()
        .unwrap()
        .test_fill_render(dummy);
    write_shared_test_pane(&mut server, pane, image);
    assert!(server.render_retained_pane_surface_and_stream(&HashSet::from([pane])));
    assert!(server.clients[&1]
        .render_state
        .last_pane_surface()
        .unwrap()
        .graphics
        .placements
        .is_empty());
    render.try_recv().unwrap(); // The dummy occupying the bounded render slot.
    server.render_and_stream();
    let recovered = decode(render.try_recv().unwrap());
    assert_eq!(
        recovered.surface.frame.graphics.assets[0].data,
        [255, 0, 0, 255]
    );
    install(&mut shell, *recovered);
    server.app.state.assert_invariants_for_test();
    shutdown_test_runtimes(&mut server);
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
async fn atomic_scene_status_churn_rebases_existing_viewport_without_provider_refresh() {
    let mut server = test_headless_server();
    let _input = install_focused_test_runtime(&mut server, b"stable");
    server.app.state.ensure_test_terminals();
    let (control, render) = scene_client(&mut server);
    server.render_and_stream();
    let initial = decode(render.try_recv().unwrap());
    let pane_id = initial.targets[0].pane_id.clone().unwrap();
    server.clients.get_mut(&1).unwrap().shell_pane_viewport =
        Some(crate::protocol::endpoint::ClientPaneViewportRequest {
            boot_id: initial.surface.frame.boot_id.clone(),
            projection_revision: initial.surface.frame.projection_revision,
            pane_id,
            size: Some(crate::protocol::ClientSurfaceSize { cols: 50, rows: 20 }),
        });
    server.render_and_stream();
    let _ = render.try_recv();
    let before = server.clients[&1].shell_route_epoch;
    revocations(&control);
    server.app.state.workspaces[0].custom_name = Some("only a status label".into());
    server.render_and_stream();
    let updated = decode(render.try_recv().unwrap());
    let reservation = server.clients[&1]
        .shell_pane_viewport
        .as_ref()
        .expect("do not restore/shrink PTY on presentation churn");
    assert_eq!(
        reservation.projection_revision,
        updated.surface.frame.projection_revision
    );
    assert_eq!(
        reservation.size,
        Some(crate::protocol::ClientSurfaceSize { cols: 50, rows: 20 })
    );
    assert_eq!(updated.route_epoch, before);
    assert!(revocations(&control).is_empty());
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn atomic_scene_retained_update_cannot_reopen_a_newer_uncommitted_route() {
    let mut server = test_headless_server();
    let _input = install_focused_test_runtime(&mut server, b"old base");
    server.app.state.ensure_test_terminals();
    let (_control, render) = scene_client(&mut server);
    server.render_and_stream();
    let first = decode(render.try_recv().unwrap());
    let pane = server.app.state.workspaces[0].tabs[0].root_pane;
    let client = server.clients.get_mut(&1).unwrap();
    client.shell_route_epoch += 1; // A newer known mutation, not a successfully queued replacement.
    assert_eq!(client.shell_committed_scene_epoch, first.route_epoch);
    write_shared_test_pane(&mut server, pane, b"\rchanged old raster");
    assert!(!server.render_retained_pane_surface_and_stream(&HashSet::from([pane])));
    assert!(
        render.try_recv().is_err(),
        "no old geometry stamped with newer route epoch"
    );
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
