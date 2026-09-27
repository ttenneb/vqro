use serde::{Deserialize, Serialize};

use super::agents::AgentInfo;
use super::collections::CollectionInfo;
use super::delegations::DelegationTreeEntry;
use super::panes::{PaneInfo, PaneLayoutSnapshot};
use super::repositories::{RepositoryInfo, SpaceInfo};
use super::tabs::TabInfo;
use super::workspaces::WorkspaceInfo;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SessionSnapshot {
    pub version: String,
    pub protocol: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focused_workspace_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focused_tab_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focused_pane_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub repositories: Vec<RepositoryInfo>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub space_order: Vec<SpaceInfo>,
    pub workspaces: Vec<WorkspaceInfo>,
    pub tabs: Vec<TabInfo>,
    pub panes: Vec<PaneInfo>,
    pub layouts: Vec<PaneLayoutSnapshot>,
    pub agents: Vec<AgentInfo>,
    /// Authoritative collection organization, omitted when the session has none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub collections: Vec<CollectionInfo>,
    /// Authoritative delegation forest in deterministic preorder, omitted when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub delegations: Vec<DelegationTreeEntry>,
}
