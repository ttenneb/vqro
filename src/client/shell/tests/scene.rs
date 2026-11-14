use super::collapse::{feature_shell, projection};
use super::*;
use crate::client::shell::scene::DecodedScene;
use crate::protocol::scene::{
    ClientSceneV1, SceneProjectionV1, SceneRouteRevokedV1, SceneTargetV1,
};

fn scene(revision: u64, epoch: u64) -> ClientSceneV1 {
    let mut snapshot = snapshot();
    snapshot.revision = revision;
    let mut frame = surface();
    frame.projection_revision = revision;
    frame.surface_revision = revision;
    let collapse = projection(&frame);
    ClientSceneV1 {
        route_epoch: epoch,
        projections: vec![
            SceneProjectionV1 {
                kind: crate::protocol::endpoint::ENDPOINT_SNAPSHOT_KIND.into(),
                data: serde_json::to_string(&snapshot).unwrap(),
            },
            SceneProjectionV1 {
                kind: crate::protocol::collapse::LAYOUT_COLLAPSE_KIND.into(),
                data: serde_json::to_string(&collapse).unwrap(),
            },
        ],
        targets: vec![SceneTargetV1 {
            pane_id: Some("pane_1".into()),
            terminal_id: "term-1".into(),
            runtime_instance: Some(1),
        }],
        surface: crate::protocol::PaneSurfaceFrameV2 {
            frame,
            views: Vec::new(),
        },
    }
}
fn install(state: &mut ClientShellState, value: ClientSceneV1) -> bool {
    state.install_scene(
        &ClientEndpointId::Local,
        1,
        DecodedScene::decode(value).unwrap(),
        true,
    )
}
fn text(state: &mut ClientShellState) -> ClientShellInput {
    state.handle_raw_events(vec![RawInputEvent::Text(crate::input::TextCommit::new(
        "x",
    ))])
}
fn delivered(outcome: &ClientShellInput) -> bool {
    outcome.requests.iter().any(|request| matches!(request, ClientMessage::ClientShellPaneInput { pane_id, .. } if pane_id == "pane_1"))
}

#[test]
fn atomic_scene_new_fixture_has_its_own_wire_tag_and_round_trips_as_one_frame() {
    let value: ClientSceneV1 = serde_json::from_str(include_str!(
        "../../../../tests/fixtures/client-scene-v1.json"
    ))
    .unwrap();
    DecodedScene::decode(value.clone()).unwrap();
    let message = crate::protocol::ServerMessage::ClientSceneV1(Box::new(value.clone()));
    let mut bytes = Vec::new();
    crate::protocol::write_message(&mut bytes, &message).unwrap();
    assert_eq!(
        bytes[4], 22,
        "new tag, not a reinterpretation of surface v2 tag 21"
    );
    let mut cursor = std::io::Cursor::new(&bytes);
    let crate::protocol::ServerMessage::ClientSceneV1(decoded) =
        crate::protocol::read_message(&mut cursor, crate::protocol::MAX_FRAME_SIZE).unwrap()
    else {
        panic!("scene")
    };
    assert_eq!(*decoded, value);
    assert_eq!(cursor.position(), bytes.len() as u64);
}

#[test]
fn atomic_scene_status_update_keeps_old_route_until_one_complete_install() {
    let mut state = feature_shell();
    assert!(install(&mut state, scene(1, 1)));
    let mut next = scene(2, 1);
    let mut snapshot: ClientShellSnapshot =
        serde_json::from_str(&next.projections[0].data).unwrap();
    snapshot.tabs[0].agent_status = AgentStatus::Working;
    next.projections[0].data = serde_json::to_string(&snapshot).unwrap();
    // Preparation and a queued scene are not input events and cannot change the old pair.
    assert!(delivered(&text(&mut state)));
    assert_eq!(state.snapshot.as_ref().unwrap().revision, 1);
    assert!(install(&mut state, next));
    assert!(delivered(&text(&mut state)));
    assert_eq!(state.snapshot.as_ref().unwrap().revision, 2);
    assert_eq!(state.pane_surface.as_ref().unwrap().projection_revision, 2);
    assert!(state.collapse_interactions_current());
}

#[test]
fn atomic_scene_revocation_overtakes_queued_scene_and_patch_without_reopening() {
    let mut state = feature_shell();
    assert!(install(&mut state, scene(1, 1)));
    assert_eq!(
        state.clipboard_image_target(),
        Some(crate::protocol::ClientClipboardImageTarget::Pane(
            "pane_1".into()
        ))
    );
    state.revoke_scene_route(
        &ClientEndpointId::Local,
        1,
        SceneRouteRevokedV1 {
            boot_id: "boot-1".into(),
            route_epoch: 2,
        },
    );
    assert!(!delivered(&text(&mut state)));
    assert_eq!(state.clipboard_image_target(), None);
    assert!(!install(&mut state, scene(2, 1)));
    assert_eq!(state.snapshot.as_ref().unwrap().revision, 1);
    assert!(!delivered(&text(&mut state)));
    assert!(matches!(
        state.apply_pane_surface_patch(crate::protocol::PaneSurfacePatch {
            boot_id: "boot-1".into(),
            projection_revision: 1,
            base_surface_revision: 1,
            surface_revision: 2,
            rows: Vec::new(),
            panes: Vec::new(),
            cursor: None,
        }),
        ClientPaneSurfacePatchOutcome::Rejected
    ));
    assert!(install(&mut state, scene(3, 2)));
    assert!(delivered(&text(&mut state)));
}

#[test]
fn atomic_scene_malformed_members_or_old_base_leave_all_installed_facts_unchanged() {
    let mut state = feature_shell();
    assert!(install(&mut state, scene(2, 2)));
    let before = state.snapshot.clone();
    let raster = state.pane_surface.clone();
    let targets = state.endpoints[0].scene_targets.clone();
    for fault in 0..5 {
        let mut value = scene(3, 3);
        match fault {
            0 => value.projections[1].data = "bad JSON".into(),
            1 => value.projections.push(value.projections[0].clone()),
            2 => value.surface.frame.boot_id = "wrong boot".into(),
            3 => value.surface.frame.surface_revision = 999,
            _ => value.targets.clear(),
        }
        assert!(DecodedScene::decode(value).is_err());
        assert_eq!(state.snapshot, before);
        assert_eq!(state.pane_surface, raster);
        assert_eq!(state.endpoints[0].scene_targets, targets);
    }
    assert!(!install(&mut state, scene(1, 1)));
    assert_eq!(state.snapshot, before);
    assert_eq!(state.pane_surface, raster);
}

#[test]
fn atomic_scene_popup_key_text_and_paste_refuse_revoked_route_but_release_cleans_up() {
    use crossterm::event::{KeyCode, KeyEventKind, KeyModifiers};
    let key = |kind| {
        RawInputEvent::Key(
            crate::input::TerminalKey::new(KeyCode::Char('x'), KeyModifiers::NONE).with_kind(kind),
        )
    };
    for kind in 0..3 {
        let event = || match kind {
            0 => key(KeyEventKind::Press),
            1 => RawInputEvent::Text(crate::input::TextCommit::new("x")),
            _ => RawInputEvent::Paste("x".into()),
        };
        let mut state = feature_shell();
        let mut value = scene(1, 1);
        value.surface.frame.popup = surface_with_popup().popup;
        value.targets.push(SceneTargetV1 {
            pane_id: None,
            terminal_id: "terminal-popup".into(),
            runtime_instance: Some(2),
        });
        assert!(install(&mut state, value));
        state.overlay = None;
        state.set_host_reports_key_releases(true);
        assert_eq!(
            state.clipboard_image_target(),
            Some(crate::protocol::ClientClipboardImageTarget::Popup(
                "terminal-popup".into()
            ))
        );
        let admitted = state.handle_raw_events(vec![event()]);
        assert!(admitted
            .requests
            .iter()
            .any(|request| matches!(request, ClientMessage::ClientShellPopupInput { .. })));
        state.revoke_scene_route(
            &ClientEndpointId::Local,
            1,
            SceneRouteRevokedV1 {
                boot_id: "boot-1".into(),
                route_epoch: 2,
            },
        );
        assert_eq!(state.clipboard_image_target(), None);
        let refused = state.handle_raw_events(vec![if kind == 0 {
            key(KeyEventKind::Repeat)
        } else {
            event()
        }]);
        assert!(!refused
            .requests
            .iter()
            .any(|request| matches!(request, ClientMessage::ClientShellPopupInput { .. })));
        assert!(state.visible_endpoint_notice.is_some());
        if kind == 0 {
            let release = state.handle_raw_events(vec![key(KeyEventKind::Release)]);
            assert!(release
                .requests
                .iter()
                .any(|request| matches!(request, ClientMessage::ClientShellPopupInput { .. })));
        }
    }
}

#[test]
fn atomic_scene_preview_member_rebases_existing_interest_and_rejects_bad_geometry_before_mutation()
{
    use crate::protocol::endpoint::*;
    let mut state = feature_shell();
    assert!(install(&mut state, scene(1, 1)));
    state.collection_list_views.insert("collection_1".into());
    state.collection_entered_pane = Some("pane_1".into());
    state.collection_expanded_members.insert(
        "collection_1".into(),
        std::collections::HashSet::from(["pane_1".into()]),
    );
    state.last_terminal_views_interest = Some(ClientTerminalViewsInterest {
        boot_id: "boot-1".into(),
        projection_revision: 1,
        views: vec![ClientTerminalViewInterest {
            pane_id: "pane_1".into(),
            size: crate::protocol::ClientSurfaceSize { cols: 8, rows: 2 },
        }],
    });
    let value = |revision: u64, cols: u16| {
        let mut value = scene(revision, 1);
        let structure = ClientShellStructureProjection {
            session_id: "session".into(),
            boot_id: "boot-1".into(),
            revision,
            collections: vec![ClientShellCollection {
                collection_id: "collection_1".into(),
                workspace_id: "ws_1".into(),
                tab_id: "tab_1".into(),
                label: None,
                selected_pane_id: Some("pane_1".into()),
                members: vec![ClientShellCollectionMember {
                    pane_id: "pane_1".into(),
                    index: 0,
                    archived: false,
                }],
            }],
            delegations: Vec::new(),
            repositories: Vec::new(),
            space_order: Vec::new(),
        };
        let preview = ServerTerminalViewsSurface {
            boot_id: "boot-1".into(),
            projection_revision: revision,
            replace: true,
            views: vec![ServerTerminalViewSurface {
                pane_id: "pane_1".into(),
                frame: FrameData::from_ratatui_buffer_with_hyperlinks(
                    &Buffer::empty(Rect::new(0, 0, cols, 2)),
                    None,
                    &[],
                ),
                scroll: None,
                mouse_reporting: false,
                sgr_pixel_mouse: false,
                pixel_width: 0,
                pixel_height: 0,
            }],
        };
        value.projections.push(SceneProjectionV1 {
            kind: SESSION_STRUCTURE_KIND.into(),
            data: serde_json::to_string(&structure).unwrap(),
        });
        value.projections.push(SceneProjectionV1 {
            kind: TERMINAL_VIEWS_SURFACE_KIND.into(),
            data: serde_json::to_string(&preview).unwrap(),
        });
        value
    };
    assert!(install(&mut state, value(2, 8)));
    assert_eq!(state.terminal_view_surfaces["pane_1"].frame.width, 8);
    assert_eq!(
        state
            .last_terminal_views_interest
            .as_ref()
            .unwrap()
            .projection_revision,
        2
    );
    assert_eq!(state.collection_entered_pane.as_deref(), Some("pane_1"));
    assert!(delivered(&text(&mut state)));
    let snapshot = state.snapshot.clone();
    let surface = state.pane_surface.clone();
    let previews = state.terminal_view_surfaces.clone();
    assert!(
        !install(&mut state, value(3, 9)),
        "well-formed but unclaimed geometry cannot partially install"
    );
    assert_eq!(state.snapshot, snapshot);
    assert_eq!(state.pane_surface, surface);
    assert_eq!(state.terminal_view_surfaces, previews);
    assert_eq!(
        state
            .last_terminal_views_interest
            .as_ref()
            .unwrap()
            .projection_revision,
        2
    );
    assert!(state.collapse_interactions_current());
}

#[test]
fn atomic_scene_same_route_preserves_local_inspector_ownership_without_rebinding_actions() {
    let mut state = feature_shell();
    assert!(install(&mut state, scene(1, 1)));
    state.inspector = Some(Box::new(ClientTerminalInspectorPane {
        pane_id: "pane_1".into(),
        pane_label: "pane".into(),
        loading: false,
        focused: true,
        request_revision: 1,
        view_request_id: Some("old-view".into()),
        service_generation: Some(1),
        pending_action: None,
        placement: crate::pane_contribution::PaneContributionPlacement::Accessory,
        contribution: None,
        telemetry: None,
        error: None,
    }));
    assert!(install(&mut state, scene(2, 1)));
    let inspector = state
        .inspector
        .as_ref()
        .expect("status projection must not remove local input owner");
    assert!(inspector.focused);
    assert!(inspector.view_request_id.is_none());
    assert!(inspector.service_generation.is_none());
    assert!(
        !delivered(&text(&mut state)),
        "focused inspector still consumes text as local UI"
    );
    let mut geometry = scene(2, 2);
    geometry.surface.frame.surface_revision = 3;
    geometry.projections[1].data =
        serde_json::to_string(&projection(&geometry.surface.frame)).unwrap();
    state.revoke_scene_route(
        &ClientEndpointId::Local,
        1,
        SceneRouteRevokedV1 {
            boot_id: "boot-1".into(),
            route_epoch: 2,
        },
    );
    assert!(install(&mut state, geometry));
    assert!(
        state.inspector.is_some(),
        "same-projection own geometry preserves the client owner"
    );
    let mut replaced = scene(2, 3);
    replaced.surface.frame.surface_revision = 4;
    replaced.targets[0].runtime_instance = Some(2);
    replaced.projections[1].data =
        serde_json::to_string(&projection(&replaced.surface.frame)).unwrap();
    assert!(install(&mut state, replaced));
    assert!(
        state.inspector.is_none(),
        "runtime identity loss closes owner even at same projection"
    );
}

#[test]
fn atomic_scene_patch_before_base_and_mode_change_do_not_create_authority() {
    let mut state = feature_shell();
    let patch = crate::protocol::PaneSurfacePatch {
        boot_id: "boot-1".into(),
        projection_revision: 1,
        base_surface_revision: 1,
        surface_revision: 2,
        rows: Vec::new(),
        panes: Vec::new(),
        cursor: None,
    };
    assert!(matches!(
        state.apply_pane_surface_patch(patch.clone()),
        ClientPaneSurfacePatchOutcome::Rejected
    ));
    assert!(install(&mut state, scene(1, 1)));
    let mut mode_patch = patch.clone();
    let mut pane = state.pane_surface.as_ref().unwrap().panes[0].clone();
    pane.mouse_reporting = !pane.mouse_reporting;
    mode_patch.panes.push(pane);
    assert!(matches!(
        state.apply_pane_surface_patch(mode_patch),
        ClientPaneSurfacePatchOutcome::Rejected
    ));
    assert!(matches!(
        state.apply_pane_surface_patch(patch),
        ClientPaneSurfacePatchOutcome::Applied(_)
    ));
    assert!(state.collapse_interactions_current());
}
