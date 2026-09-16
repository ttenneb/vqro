use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct ExtensionListParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installed: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ExtensionConfirmedParams {
    pub repository: String,
    #[serde(default)]
    pub confirmed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ExtensionSetEnabledParams {
    pub repository: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ExtensionReorderParams {
    pub repository: String,
    pub index: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ExtensionRepositoryOverrideParams {
    pub repository: String,
    pub repository_id: String,
    /// `null` restores global enablement for this repository.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ExtensionUpdateParams {
    pub repository: String,
    pub version: String,
    /// Update discovery may stage a version, but package replacement requires
    /// an explicit approval from a host-owned confirmation UI.
    #[serde(default)]
    pub approved: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ExtensionInfo {
    pub id: String,
    pub repository: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_commit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest_path: Option<String>,
    pub name: String,
    pub summary: String,
    pub details: String,
    pub categories: Vec<String>,
    pub package_kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub integration_target: Option<String>,
    /// Deployment state of an agent adapter, distinct from package install state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub integration_status: Option<String>,
    pub available_version: String,
    pub active_version: String,
    pub installed: bool,
    pub enabled: bool,
    /// Whether this host can perform a fresh installation of the package.
    #[serde(default)]
    pub install_supported: bool,
    /// True only when the active retained source artifact re-verifies locally.
    #[serde(default)]
    pub artifact_verified: bool,
    #[serde(default)]
    pub repository_override_supported: bool,
    pub load_order: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rollback_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activation_error: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub repository_overrides: Vec<ExtensionRepositoryOverrideInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ExtensionRepositoryOverrideInfo {
    pub repository_id: String,
    pub enabled: bool,
}
