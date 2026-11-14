//! Negotiated atomic presentation. Existing named projection codecs stay byte-immutable.
use serde::{Deserialize, Serialize};

use super::PaneSurfaceFrameV2;

pub const SCENE_CODEC_V1: &str = "shell.scene.v1";
pub const ROUTE_REVOKED_KIND: &str = "shell.scene.route-revoked.v1";

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
