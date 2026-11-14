//! Negotiated atomic presentation. Existing named projection codecs stay byte-immutable.
use serde::{Deserialize, Serialize};

use super::PaneSurfaceFrameV2;

pub const SCENE_CODEC_V1: &str = "shell.scene.v1";
pub const ROUTE_REVOKED_KIND: &str = "shell.scene.route-revoked.v1";

/// Shared host/client owner identity, deliberately excluding geometry and observation churn.
pub(crate) fn inspector_identity(
    snapshot: &super::ClientShellSnapshot,
    targets: &[SceneTargetV1],
    tree: Option<&str>,
) -> Option<[u8; 32]> {
    use sha2::{Digest, Sha256};
    let panes = snapshot
        .panes
        .iter()
        .filter(|pane| Some(&pane.tab_id) == snapshot.focused_tab_id.as_ref())
        .map(|pane| {
            (
                &pane.pane_id,
                &pane.workspace_id,
                &pane.tab_id,
                pane.focused,
                pane.right_click_passthrough,
            )
        })
        .collect::<Vec<_>>();
    let bytes = serde_json::to_vec(&(
        &snapshot.boot_id,
        &snapshot.focused_workspace_id,
        &snapshot.focused_tab_id,
        &snapshot.focused_pane_id,
        panes,
        targets,
        tree,
    ))
    .ok()?;
    Some(Sha256::digest(bytes).into())
}

/// One owner-continuity condition for host reservations and client inspectors.
/// A live owner's own geometry request keeps ownership, while keyboard geometry
/// authority still advances. A new projection plus a new route closes ownership.
pub(crate) fn preserves_inspector_owner(
    same_identity: bool,
    same_projection: bool,
    same_route: bool,
) -> bool {
    same_identity && (same_projection || same_route)
}

/// JSON projections retain their published validation and extensible data shapes.
/// They are carried inside the binary render transaction, never separately queued.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SceneProjectionV1 {
    pub kind: String,
    pub data: String,
}

/// One full base and all classification facts, delivered as one client-loop event.
/// Raster revisions may advance without advancing route authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientSceneV1 {
    pub route_epoch: u64,
    pub projections: Vec<SceneProjectionV1>,
    pub targets: Vec<SceneTargetV1>,
    pub surface: PaneSurfaceFrameV2,
}

/// Host observation of an attachment/runtime, not an input admission token yet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SceneTargetV1 {
    pub pane_id: Option<String>,
    pub terminal_id: String,
    pub runtime_instance: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SceneRouteRevokedV1 {
    pub boot_id: String,
    pub route_epoch: u64,
}

#[cfg(test)]
mod tests {
    #[test]
    fn inspector_owner_continuity_is_shared_for_host_claim_and_client_ui() {
        use super::preserves_inspector_owner as keep;
        assert!(
            keep(true, true, false),
            "owned geometry request, keyboard route advances"
        );
        assert!(keep(true, false, true), "status-only projection advance");
        assert!(
            !keep(false, true, false),
            "runtime/identity loss even at same projection"
        );
        assert!(!keep(false, false, false), "external focus/topology loss");
        assert!(
            !keep(true, false, false),
            "new geometry plus new projection closes owner"
        );
    }
}
