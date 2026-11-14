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
    state.revoke_scene_route(
        &ClientEndpointId::Local,
        1,
        SceneRouteRevokedV1 {
            boot_id: "boot-1".into(),
            route_epoch: 2,
        },
    );
    assert!(!delivered(&text(&mut state)));
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
