use super::*;
use crate::{client::shell::collapse::CollapsePairState, protocol::collapse::*};

pub(super) fn projection(frame: &PaneSurfaceFrame) -> Box<CollapseProjectionV1> {
    Box::new(CollapseProjectionV1 {
        boot_id: frame.boot_id.clone(),
        projection_revision: frame.projection_revision,
        base_surface_revision: frame.surface_revision,
        body: CollapseBodyV1 {
            tab_id: Some("tab_1".into()),
            tree_fingerprint: Some("a".repeat(64)),
            width: frame.frame.width,
            height: frame.frame.height,
            enabled: true,
            unavailable: None,
            headers: Vec::new(),
            anchors: vec![CollapseAnchorV1 {
                pane_id: "pane_1".into(),
                path: Vec::new(),
            }],
            containers: Vec::new(),
        },
    })
}
pub(super) fn feature_shell() -> ClientShellState {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_endpoint_capabilities_for(
        &ClientEndpointId::Local,
        vec![
            LAYOUT_COLLAPSE_CAPABILITY.into(),
            crate::native_api::NATIVE_API_CAPABILITY_V1.into(),
        ],
    );
    state.set_endpoint_methods(Some(vec!["layout.collapse".into(), "layout.expand".into()]));
    state.set_snapshot(Box::new(snapshot()));
    state
}

#[test]
fn either_queue_order_requires_exact_base_and_keeps_only_the_coherent_pair() {
    for companion_first in [false, true] {
        let frame = surface();
        let mut pair = CollapsePairState::default();
        if companion_first {
            pair.offer_projection(1, projection(&frame)).unwrap();
        } else {
            pair.offer_surface(1, frame.clone(), Vec::new());
        }
        assert!(pair.take_ready(&snapshot()).is_none());
        assert!(!pair.coherent());
        if companion_first {
            pair.offer_surface(1, frame, Vec::new());
        } else {
            pair.offer_projection(1, projection(&frame)).unwrap();
        }
        let (ready, _) = pair.take_ready(&snapshot()).unwrap();
        assert_eq!(ready.surface_revision, 1);
        assert!(pair.coherent());
    }
}

#[test]
fn newer_metadata_invalidates_input_even_without_a_projection_counter_change() {
    let frame = surface();
    let mut state = feature_shell();
    state.set_endpoint_collapse_projection_for_generation(
        &ClientEndpointId::Local,
        1,
        Ok(projection(&frame)),
    );
    state.set_pane_surface_with_views_for_generation(
        &ClientEndpointId::Local,
        1,
        frame.clone(),
        Vec::new(),
    );
    assert!(state.collapse_interactions_current());
    let mut next = frame.clone();
    next.surface_revision = 2;
    let mut metadata = projection(&next);
    metadata.body.tree_fingerprint = Some("b".repeat(64));
    state.set_endpoint_collapse_projection_for_generation(
        &ClientEndpointId::Local,
        1,
        Ok(metadata),
    );
    assert!(!state.collapse_interactions_current());
    assert_eq!(
        state.pane_surface.as_ref().unwrap().surface_revision,
        1,
        "keep last coherent raster until the full mate arrives"
    );
    state.set_pane_surface_with_views_for_generation(&ClientEndpointId::Local, 1, next, Vec::new());
    assert!(state.collapse_interactions_current());
    assert_eq!(state.pane_surface.as_ref().unwrap().surface_revision, 2);
}

#[test]
fn mismatched_geometry_tab_and_stale_connection_cannot_install_or_retarget() {
    let frame = surface();
    let mut pair = CollapsePairState::default();
    pair.offer_surface(2, frame.clone(), Vec::new());
    let mut wrong = projection(&frame);
    wrong.body.tab_id = Some("another_tab".into());
    pair.offer_projection(2, wrong).unwrap();
    assert!(pair.take_ready(&snapshot()).is_none());
    pair.offer_projection(2, projection(&frame)).unwrap();
    assert!(pair.take_ready(&snapshot()).is_some());
    let mut newer = frame.clone();
    newer.surface_revision = 3;
    pair.offer_surface(1, newer.clone(), Vec::new());
    pair.offer_projection(1, projection(&newer)).unwrap();
    assert!(pair.coherent());
    assert_eq!(pair.current_surface_revision, 1);
    // A new connection generation may legitimately restart the surface counter.
    pair.offer_surface(3, frame.clone(), Vec::new());
    pair.offer_projection(3, projection(&frame)).unwrap();
    assert!(pair.take_ready(&snapshot()).is_some());
}

#[test]
fn patches_inherit_only_the_verified_base_and_do_not_wait_for_another_companion() {
    let mut state = feature_shell();
    let frame = surface();
    let patch = crate::protocol::PaneSurfacePatch {
        boot_id: frame.boot_id.clone(),
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
    state.set_pane_surface_with_views_for_generation(
        &ClientEndpointId::Local,
        1,
        frame.clone(),
        Vec::new(),
    );
    assert!(!state.has_presented_surface());
    state.set_endpoint_collapse_projection_for_generation(
        &ClientEndpointId::Local,
        1,
        Ok(projection(&frame)),
    );
    assert!(matches!(
        state.apply_pane_surface_patch(patch),
        ClientPaneSurfacePatchOutcome::Applied(_)
    ));
    assert!(state.collapse_projection().is_some());
    assert_eq!(state.pane_surface.as_ref().unwrap().surface_revision, 2);
}

#[test]
fn legacy_peer_never_waits_for_optional_metadata_and_invalid_feature_does_not_disconnect() {
    let mut legacy = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    legacy.set_snapshot(Box::new(snapshot()));
    legacy.set_pane_surface(surface());
    assert!(legacy.has_presented_surface());
    let mut modern = feature_shell();
    modern.set_pane_surface_with_views_for_generation(
        &ClientEndpointId::Local,
        1,
        surface(),
        Vec::new(),
    );
    modern.set_endpoint_collapse_projection_for_generation(
        &ClientEndpointId::Local,
        1,
        Err("malformed optional metadata".into()),
    );
    assert!(modern.has_presented_surface());
    assert!(modern.collapse_interactions_current());
    assert!(modern.collapse_projection().is_none());
}

#[test]
fn cold_collapsed_root_establishes_header_focus_without_inventing_a_body_target() {
    let mut state = feature_shell();
    let mut frame = surface();
    frame.panes.clear();
    let mut metadata = projection(&frame);
    metadata.body.anchors.clear();
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
    let endpoint = state
        .endpoints
        .iter()
        .find(|endpoint| endpoint.endpoint_id.is_local())
        .unwrap();
    assert_eq!(endpoint.collapse.active_header, Some(Vec::new()));
    assert!(state.pane_surface.as_ref().unwrap().panes.is_empty());
    state.mark_endpoint_disconnected(&ClientEndpointId::Local);
    assert!(state.collapse_projection().is_none());
}
