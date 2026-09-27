use serde::{Deserialize, Serialize};

fn default_package_path() -> String {
    "package.vqrox".into()
}

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
pub struct ExtensionPackageSource {
    pub url: String,
    pub commit: String,
    #[serde(default = "default_package_path")]
    pub package_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ExtensionPackageInspectParams {
    pub source: ExtensionPackageSource,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ExtensionPackageApprovalParams {
    pub source: ExtensionPackageSource,
    pub expected_package_sha256: String,
    pub expected_manifest_sha256: String,
    #[serde(default)]
    pub approved: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ExtensionPackageMutationParams {
    pub package_id: String,
    #[serde(default)]
    pub confirmed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ExtensionContractMethodInspection {
    pub id: String,
    pub request_information: Vec<String>,
    pub response_information: Vec<String>,
    pub max_request_bytes: u64,
    pub max_response_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ExtensionContractInspection {
    pub service_id: String,
    pub endpoint_id: String,
    pub contract_id: String,
    pub contract_version: String,
    pub descriptor_sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required: Option<bool>,
    pub methods: Vec<ExtensionContractMethodInspection>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ExtensionServiceEndpointParams {
    pub package_id: String,
    pub service_id: String,
    pub endpoint_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ExtensionServiceBindingInspectParams {
    pub consumer: ExtensionServiceEndpointParams,
    pub provider: ExtensionServiceEndpointParams,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ExtensionServiceBindingApproveParams {
    pub consumer: ExtensionServiceEndpointParams,
    pub provider: ExtensionServiceEndpointParams,
    pub expected_registry_revision: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_binding_revision: Option<u64>,
    pub expected_consent_sha256: String,
    #[serde(default)]
    pub confirmed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ExtensionServiceBindingRevokeParams {
    pub binding_id: String,
    pub expected_registry_revision: u64,
    pub expected_binding_revision: u64,
    #[serde(default)]
    pub confirmed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
#[serde(deny_unknown_fields)]
pub struct ExtensionDoctorParams {}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ExtensionServiceBindingInvokeParams {
    pub binding_id: String,
    pub expected_binding_revision: u64,
    pub consumer_method: String,
    pub params: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ExtensionServiceBindingInspection {
    pub registry_revision: u64,
    pub consent_sha256: String,
    pub consent: crate::service_fabric::ServiceBindingConsentV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub existing_binding: Option<crate::service_fabric::ServiceBindingV1>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ExtensionServiceBindingInfo {
    pub binding: crate::service_fabric::ServiceBindingV1,
    pub effective_status: crate::service_fabric::EffectiveServiceBindingStatusV1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ExtensionPackageInspection {
    pub source: ExtensionPackageSource,
    pub package_id: String,
    pub version: String,
    pub package_sha256: String,
    pub manifest_sha256: String,
    pub namespaces: Vec<String>,
    pub services: Vec<String>,
    pub requested_capabilities: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub provided_contracts: Vec<ExtensionContractInspection>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_contracts: Vec<ExtensionContractInspection>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct InstalledExtensionPackageInfo {
    pub package_id: String,
    pub version: String,
    pub source: ExtensionPackageSource,
    pub package_sha256: String,
    pub manifest_sha256: String,
    pub enabled: bool,
    pub removed_retaining_data: bool,
    pub load_order: usize,
    pub requested_capabilities: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub provided_contracts: Vec<ExtensionContractInspection>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_contracts: Vec<ExtensionContractInspection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rollback_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activation_error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionPackageKind {
    Bundled,
    AgentIntegration,
    SandboxedPackage,
    GithubPlugin,
}

impl std::fmt::Display for ExtensionPackageKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Bundled => "bundled provider",
            Self::AgentIntegration => "agent integration",
            Self::SandboxedPackage => "sandboxed package",
            Self::GithubPlugin => "GitHub plugin",
        })
    }
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
    /// Capabilities requested by executable package services. An empty list means
    /// that this catalog item has no downloaded executable service.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requested_capabilities: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub provided_contracts: Vec<ExtensionContractInspection>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_contracts: Vec<ExtensionContractInspection>,
    pub package_kind: ExtensionPackageKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub integration_target: Option<super::IntegrationTarget>,
    /// Deployment state of an agent adapter, distinct from package install state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub integration_status: Option<super::IntegrationState>,
    pub available_version: String,
    pub active_version: String,
    pub installed: bool,
    pub enabled: bool,
    /// Whether this host can perform a fresh installation of the package.
    #[serde(default)]
    pub install_supported: bool,
    /// True when an artifact verified at installation remains in managed storage.
    /// Activation and rollback re-verify its digests before use.
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
