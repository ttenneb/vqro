use serde::{Deserialize, Serialize};

use crate::repository::CheckoutKind;

/// Optional first-class repository projection. Workspace APIs remain available
/// for clients that do not understand this model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RepositoryInfo {
    pub repository_id: String,
    pub label: String,
    pub git_common_dir: String,
    pub checkout_workspace_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_focused_workspace_id: Option<String>,
    pub focused: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CheckoutInfo {
    pub workspace_id: String,
    pub repository_id: String,
    pub checkout_path: String,
    pub kind: CheckoutKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum SpaceInfo {
    Repository(String),
    StandaloneWorkspace(String),
}
