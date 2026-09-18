//! Public, Collections-specific contracts layered beside `host.document.v2`.
//!
//! These DTOs intentionally do not add actions or effects to the generic document contract.
//! The host validates a Collections profile and typed action response before adapting either to
//! legacy clients or committing one package-state compare-and-swap transaction.

use std::collections::{BTreeSet, HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::extension_contract::{
    HostDocumentItem, HostDocumentItemFlags, HostDocumentV1, HostItemRole, HostSurface, HostTarget,
    HostTerminalPlacement, HostTerminalRef, HOST_DOCUMENT_CONTRACT_V1,
};
use crate::host_document_v2::{
    DocumentDependencyV2, DocumentNodeV2, DocumentProducerFenceV2, HostDocumentV2,
    TerminalScopePolicy, HOST_STATE_DEPENDENCY_CONTRACT_V1, HOST_TERMINALS_DEPENDENCY_CONTRACT_V1,
};
use crate::terminal::TerminalId;

pub const COLLECTIONS_DOCUMENT_PROFILE_CONTRACT_V1: &str = "vqro.collections.document-profile.v1";
pub const COLLECTIONS_ACTIONS_CONTRACT_V1: &str = "vqro.collections.actions.v1";
pub const COLLECTIONS_ACTION_INVOCATION_CONTRACT_V1: &str = "vqro.collections.action-invocation.v1";
pub const COLLECTIONS_EFFECT_PLAN_CONTRACT_V1: &str = "vqro.collections.effect-plan.v1";
pub const COLLECTIONS_PRODUCTION_PIN_CONTRACT_V1: &str = "vqro.collections.production-pin.v1";
pub const COLLECTIONS_AUTHORITY_CONTRACT_V1: &str = "collections_authority.v1";

pub(crate) fn embedded_collections_production_pin() -> Result<CollectionsProductionPinV1, String> {
    let pin: CollectionsProductionPinV1 = serde_json::from_str(include_str!(
        "../extensions/first-party/collections/production-pin-v1.json"
    ))
    .map_err(|error| format!("invalid embedded Collections production pin: {error}"))?;
    pin.validate().map_err(|error| error.to_string())?;
    Ok(pin)
}
pub const COLLECTIONS_PACKAGE_AUTHORITY_SNAPSHOT_VERSION: u32 = 7;
pub const COLLECTIONS_PACKAGE_AUTHORITY_MINIMUM_READER_VERSION: u32 = 7;
pub const COLLECTIONS_NAMESPACE: &str = "vqro.collections";
pub const COLLECTIONS_PACKAGE_ID: &str = "vqro.collections";
pub const COLLECTIONS_SCOPE_CONTRACT_V1: &str = "host.tab.v1";
pub const COLLECTIONS_PROFILE_RENDER_METHOD_V1: &str =
    "vqro.collections.document-profile.render.v1";
pub const COLLECTIONS_ACTIONS_LIST_METHOD_V1: &str = "vqro.collections.actions.list.v1";
pub const COLLECTIONS_ACTION_PLAN_METHOD_V1: &str = "vqro.collections.action.plan.v1";
pub const MAX_COLLECTIONS_ACTIONS: usize = crate::host_document_v2::MAX_DOCUMENT_V2_NODES * 2;
pub const MAX_COLLECTIONS_IDENTIFIER_BYTES: usize = 256;
pub const MAX_COLLECTIONS_LABEL_BYTES: usize = 4 * 1024;
pub const MAX_COLLECTIONS_ACTION_LABEL_BYTES: usize = 512;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CollectionsProductionPinV1 {
    pub contract: String,
    pub package_id: String,
    pub service_id: String,
    pub source_url: String,
    pub source_commit: String,
    pub package_path: String,
    pub package_sha256: String,
    pub manifest_sha256: String,
    pub component_sha256: String,
    pub world: String,
    pub capabilities: Vec<String>,
    pub profile_contract: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CollectionsAuthorityV1 {
    pub contract: String,
    pub phase: CollectionsAuthorityPhaseV1,
    pub authority_epoch: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package: Option<CollectionsAuthorityPackageV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<CollectionsAuthorityStateV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub migration_id: Option<String>,
    pub minimum_reader_version: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CollectionsAuthorityPhaseV1 {
    Legacy,
    Package,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CollectionsAuthorityPackageV1 {
    pub package_id: String,
    pub service_id: String,
    pub package_sha256: String,
    pub component_sha256: String,
    pub profile_contract: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CollectionsAuthorityStateV1 {
    pub store_generation: u64,
    pub namespace_revision: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CollectionsDocumentProfileV1 {
    pub contract: String,
    pub document: HostDocumentV2,
    pub collections: Vec<CollectionsDocumentCollectionV1>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CollectionsDocumentCollectionV1 {
    pub container_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// Host-authored neutral topology used to validate and project a package profile.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CollectionsProfileTopologyV1 {
    pub workspace_id: String,
    pub tab_id: String,
    pub containers: Vec<CollectionsProfileContainerV1>,
    pub placements: Vec<CollectionsTerminalPlacementV1>,
    pub producer: DocumentProducerFenceV2,
    pub state_dependency: DocumentDependencyV2,
    pub terminals_dependency: DocumentDependencyV2,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CollectionsProfileContainerV1 {
    pub container_id: String,
    pub terminal_ids: Vec<String>,
    pub selected_terminal_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CollectionsTerminalPlacementV1 {
    pub terminal_id: String,
    pub pane_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CollectionsActionDeclarationsV1 {
    pub contract: String,
    pub document_revision: u64,
    pub revision: u64,
    pub actions: Vec<CollectionsActionDeclarationV1>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CollectionsActionDeclarationV1 {
    pub id: String,
    pub subject_node_id: String,
    pub label: String,
    pub action: CollectionsActionKindV1,
}

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum CollectionsActionKindV1 {
    SetLabel,
    SetArchived,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CollectionsActionInvocationV1 {
    pub contract: String,
    pub ticket: String,
    pub idempotency_key: String,
    pub document_revision: u64,
    pub actions_revision: u64,
    pub action_id: String,
    pub expected_store_generation: u64,
    pub expected_namespace_revision: u64,
    pub dependency: CollectionsActionDependencyV1,
    pub input: CollectionsActionInputV1,
}

/// The minimum host-owned dependency needed by each typed action.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CollectionsActionDependencyV1 {
    Container {
        container_id: String,
    },
    TerminalMembership {
        container_id: String,
        terminal_id: String,
        terminal_snapshot_fingerprint_sha256: String,
        provider_generation: u64,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CollectionsActionInputV1 {
    SetLabel {
        container_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
    SetArchived {
        container_id: String,
        terminal_id: String,
        archived: bool,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CollectionsEffectPlanV1 {
    pub contract: String,
    pub ticket: String,
    pub idempotency_key: String,
    pub expected_store_generation: u64,
    pub expected_namespace_revision: u64,
    pub state: CollectionsStateCasV1,
}

/// Exactly one opaque write or delete in the package's host-bound namespace.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CollectionsStateCasV1 {
    pub key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<serde_json::Value>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CollectionsContractError {
    InvalidContract,
    InvalidIdentifier,
    InvalidLabel,
    InvalidTopology,
    InvalidDocument,
    InvalidProfile,
    InvalidActions,
    ActionMismatch,
    StaleRevision,
    InvalidEffectPlan,
}

impl std::fmt::Display for CollectionsContractError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::InvalidContract => "invalid Collections contract",
            Self::InvalidIdentifier => "invalid Collections identifier",
            Self::InvalidLabel => "invalid Collections label",
            Self::InvalidTopology => "invalid Collections topology",
            Self::InvalidDocument => "invalid Collections document",
            Self::InvalidProfile => "invalid Collections document profile",
            Self::InvalidActions => "invalid Collections action declarations",
            Self::ActionMismatch => "Collections action does not match its declaration",
            Self::StaleRevision => "stale Collections revision",
            Self::InvalidEffectPlan => "invalid Collections effect plan",
        })
    }
}

impl std::error::Error for CollectionsContractError {}

impl CollectionsProductionPinV1 {
    pub fn validate(&self) -> Result<(), CollectionsContractError> {
        if self.contract != COLLECTIONS_PRODUCTION_PIN_CONTRACT_V1
            || self.package_id != COLLECTIONS_PACKAGE_ID
            || self.service_id != "collections"
            || self.profile_contract != COLLECTIONS_DOCUMENT_PROFILE_CONTRACT_V1
            || self.world != "vqro:extension/service@1.0.0"
            || !self.source_url.starts_with("https://")
            || self.source_url.contains('@')
            || !valid_lower_hex(&self.source_commit, 40)
            || !valid_sha256(&self.package_sha256)
            || !valid_sha256(&self.manifest_sha256)
            || !valid_sha256(&self.component_sha256)
            || !valid_relative_package_path(&self.package_path)
            || self.capabilities
                != [
                    "host.state.read".to_string(),
                    "host.terminals.read".to_string(),
                ]
        {
            return Err(CollectionsContractError::InvalidProfile);
        }
        Ok(())
    }
}

impl Default for CollectionsAuthorityV1 {
    fn default() -> Self {
        Self {
            contract: COLLECTIONS_AUTHORITY_CONTRACT_V1.into(),
            phase: CollectionsAuthorityPhaseV1::Legacy,
            authority_epoch: 1,
            package: None,
            state: None,
            migration_id: None,
            minimum_reader_version: COLLECTIONS_PACKAGE_AUTHORITY_MINIMUM_READER_VERSION,
        }
    }
}

impl CollectionsAuthorityV1 {
    pub(crate) fn is_default_legacy(&self) -> bool {
        self.phase == CollectionsAuthorityPhaseV1::Legacy
            && self.contract == COLLECTIONS_AUTHORITY_CONTRACT_V1
            && self.authority_epoch == 1
            && self.package.is_none()
            && self.state.is_none()
            && self.migration_id.is_none()
            && self.minimum_reader_version == COLLECTIONS_PACKAGE_AUTHORITY_MINIMUM_READER_VERSION
    }

    pub fn validate(&self) -> Result<(), CollectionsContractError> {
        if self.contract != COLLECTIONS_AUTHORITY_CONTRACT_V1
            || self.authority_epoch == 0
            || self.minimum_reader_version != COLLECTIONS_PACKAGE_AUTHORITY_MINIMUM_READER_VERSION
        {
            return Err(CollectionsContractError::InvalidProfile);
        }
        match self.phase {
            CollectionsAuthorityPhaseV1::Legacy => {
                if self.package.is_some() || self.state.is_some() || self.migration_id.is_some() {
                    return Err(CollectionsContractError::InvalidProfile);
                }
            }
            CollectionsAuthorityPhaseV1::Package => {
                let package = self
                    .package
                    .as_ref()
                    .ok_or(CollectionsContractError::InvalidProfile)?;
                let state = self
                    .state
                    .as_ref()
                    .ok_or(CollectionsContractError::InvalidProfile)?;
                let migration_id = self
                    .migration_id
                    .as_deref()
                    .ok_or(CollectionsContractError::InvalidProfile)?;
                if package.package_id != COLLECTIONS_PACKAGE_ID
                    || package.service_id != "collections"
                    || package.profile_contract != COLLECTIONS_DOCUMENT_PROFILE_CONTRACT_V1
                    || !valid_sha256(&package.package_sha256)
                    || !valid_sha256(&package.component_sha256)
                    || state.store_generation == 0
                {
                    return Err(CollectionsContractError::InvalidProfile);
                }
                validate_identifier(migration_id)?;
            }
        }
        Ok(())
    }
}

impl CollectionsDocumentProfileV1 {
    /// Validates the exact Collections shape against one host-authored topology snapshot.
    pub fn validate(
        &self,
        topology: &CollectionsProfileTopologyV1,
    ) -> Result<(), CollectionsContractError> {
        if self.contract != COLLECTIONS_DOCUMENT_PROFILE_CONTRACT_V1 {
            return Err(CollectionsContractError::InvalidContract);
        }
        validate_topology(topology)?;
        if self.document.producer != topology.producer
            || self.document.producer.package_id != COLLECTIONS_PACKAGE_ID
            || self.document.producer.service_id != "collections"
            || self.document.scope.contract != COLLECTIONS_SCOPE_CONTRACT_V1
            || self.document.scope.scope_id != topology.tab_id
        {
            return Err(CollectionsContractError::InvalidDocument);
        }
        if self.collections.len() != topology.containers.len()
            || self.document.roots.len() != topology.containers.len()
            || self.document.dependencies
                != [
                    topology.state_dependency.clone(),
                    topology.terminals_dependency.clone(),
                ]
        {
            return Err(CollectionsContractError::InvalidProfile);
        }

        let mut allowed = HashSet::new();
        for container in &topology.containers {
            for terminal_id in &container.terminal_ids {
                allowed.insert(
                    TerminalId::parse(terminal_id)
                        .map_err(|_| CollectionsContractError::InvalidTopology)?,
                );
            }
        }
        self.document
            .validate(&allowed, TerminalScopePolicy::ExactCoverage)
            .map_err(|_| CollectionsContractError::InvalidDocument)?;

        let nodes = self
            .document
            .nodes
            .iter()
            .map(|node| (node.id(), node))
            .collect::<HashMap<_, _>>();
        let mut profile_containers = BTreeSet::new();
        for ((profile, topology_container), root_id) in self
            .collections
            .iter()
            .zip(&topology.containers)
            .zip(&self.document.roots)
        {
            validate_container_id(&profile.container_id)?;
            validate_label(profile.label.as_deref())?;
            if profile.container_id != topology_container.container_id
                || !profile_containers.insert(profile.container_id.as_str())
                || root_id != &collection_root_id(&profile.container_id)
            {
                return Err(CollectionsContractError::InvalidProfile);
            }
            validate_collection_tree(
                topology_container,
                profile.label.as_deref(),
                root_id,
                &nodes,
            )?;
        }
        Ok(())
    }

    /// Produces both generation-1 compatibility outputs from the same validated profile.
    pub fn project_compatibility(
        &self,
        topology: &CollectionsProfileTopologyV1,
    ) -> Result<CollectionsCompatibilityProjectionV1, CollectionsContractError> {
        self.validate(topology)?;
        let placements = topology
            .placements
            .iter()
            .map(|placement| (placement.terminal_id.as_str(), placement.pane_id.as_str()))
            .collect::<HashMap<_, _>>();
        let selected = topology
            .containers
            .iter()
            .filter_map(|container| container.selected_terminal_id.as_deref())
            .collect::<HashSet<_>>();
        let parent_ids = document_parent_ids(&self.document)?;
        let mut items = Vec::with_capacity(self.document.nodes.len());
        for node in depth_first_nodes(&self.document)? {
            let (role, label, detail, terminal) = match node {
                DocumentNodeV2::Group { accessibility, .. } => (
                    if parent_ids.contains_key(node.id()) {
                        HostItemRole::Section
                    } else {
                        HostItemRole::Group
                    },
                    accessibility.name.clone(),
                    accessibility.description.clone(),
                    None,
                ),
                DocumentNodeV2::Text {
                    accessibility,
                    text,
                    ..
                } => (
                    HostItemRole::Status,
                    text.clone(),
                    accessibility.description.clone(),
                    None,
                ),
                DocumentNodeV2::TerminalSlot {
                    accessibility,
                    terminal_id,
                    ..
                } => {
                    let pane_id = placements
                        .get(terminal_id.as_str())
                        .ok_or(CollectionsContractError::InvalidTopology)?;
                    (
                        HostItemRole::Terminal,
                        accessibility.name.clone(),
                        accessibility.description.clone(),
                        Some(HostTerminalRef {
                            terminal_id: terminal_id.clone(),
                            placement: Some(HostTerminalPlacement {
                                pane_id: (*pane_id).to_string(),
                                workspace_id: topology.workspace_id.clone(),
                                tab_id: topology.tab_id.clone(),
                            }),
                        }),
                    )
                }
            };
            let terminal_selected = terminal
                .as_ref()
                .is_some_and(|terminal| selected.contains(terminal.terminal_id.as_str()));
            items.push(HostDocumentItem {
                id: node.id().to_string(),
                parent_id: parent_ids.get(node.id()).cloned(),
                role,
                label,
                detail,
                terminal,
                action_ids: Vec::new(),
                flags: HostDocumentItemFlags {
                    selected: terminal_selected,
                    expanded: matches!(node, DocumentNodeV2::Group { .. }),
                    disabled: false,
                    attention: false,
                },
            });
        }
        let document = HostDocumentV1 {
            contract: HOST_DOCUMENT_CONTRACT_V1.into(),
            namespace: COLLECTIONS_NAMESPACE.into(),
            generation: self.document.producer.provider_generation,
            revision: self.document.revision,
            surface: HostSurface::Navigation,
            target: HostTarget::Tab {
                tab_id: topology.tab_id.clone(),
            },
            items,
            actions: Vec::new(),
            required_capabilities: Vec::new(),
        };
        document
            .validate()
            .map_err(|_| CollectionsContractError::InvalidProfile)?;

        let mut collections = Vec::with_capacity(topology.containers.len());
        for (profile, container) in self.collections.iter().zip(&topology.containers) {
            let archived = archived_terminal_ids(&self.document, &profile.container_id)?;
            let members = container
                .terminal_ids
                .iter()
                .enumerate()
                .map(|(index, terminal_id)| {
                    let pane_id = placements
                        .get(terminal_id.as_str())
                        .ok_or(CollectionsContractError::InvalidTopology)?;
                    Ok(crate::protocol::endpoint::ClientShellCollectionMember {
                        pane_id: (*pane_id).to_string(),
                        index,
                        archived: archived.contains(terminal_id.as_str()),
                    })
                })
                .collect::<Result<Vec<_>, CollectionsContractError>>()?;
            let selected_pane_id = container
                .selected_terminal_id
                .as_deref()
                .and_then(|terminal_id| placements.get(terminal_id).copied())
                .map(str::to_string);
            let raw = parse_container_raw(&profile.container_id)?;
            collections.push(crate::protocol::endpoint::ClientShellCollection {
                collection_id: format!("collection_{raw}"),
                workspace_id: topology.workspace_id.clone(),
                tab_id: topology.tab_id.clone(),
                label: profile.label.clone(),
                selected_pane_id,
                members,
            });
        }
        Ok(CollectionsCompatibilityProjectionV1 {
            document,
            collections,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CollectionsCompatibilityProjectionV1 {
    pub document: HostDocumentV1,
    pub collections: Vec<crate::protocol::endpoint::ClientShellCollection>,
}

impl CollectionsActionDeclarationsV1 {
    pub fn validate_for(
        &self,
        profile: &CollectionsDocumentProfileV1,
        topology: &CollectionsProfileTopologyV1,
    ) -> Result<(), CollectionsContractError> {
        profile.validate(topology)?;
        if self.contract != COLLECTIONS_ACTIONS_CONTRACT_V1 {
            return Err(CollectionsContractError::InvalidContract);
        }
        if self.document_revision != profile.document.revision || self.revision == 0 {
            return Err(CollectionsContractError::StaleRevision);
        }
        if self.actions.len() > MAX_COLLECTIONS_ACTIONS {
            return Err(CollectionsContractError::InvalidActions);
        }
        let nodes = profile
            .document
            .nodes
            .iter()
            .map(|node| (node.id(), node))
            .collect::<HashMap<_, _>>();
        let root_ids = profile
            .document
            .roots
            .iter()
            .map(String::as_str)
            .collect::<HashSet<_>>();
        let mut expected_subjects = BTreeSet::new();
        for collection in &profile.collections {
            expected_subjects.insert((
                CollectionsActionKindV1::SetLabel,
                collection_root_id(&collection.container_id),
            ));
        }
        for node in &profile.document.nodes {
            if matches!(node, DocumentNodeV2::TerminalSlot { .. }) {
                expected_subjects
                    .insert((CollectionsActionKindV1::SetArchived, node.id().to_string()));
            }
        }
        let mut ids = BTreeSet::new();
        let mut subjects = BTreeSet::new();
        for action in &self.actions {
            validate_identifier(&action.id)?;
            validate_identifier(&action.subject_node_id)?;
            validate_action_label(&action.label)?;
            if !ids.insert(action.id.as_str())
                || !subjects.insert((action.action, action.subject_node_id.as_str()))
            {
                return Err(CollectionsContractError::InvalidActions);
            }
            let Some(node) = nodes.get(action.subject_node_id.as_str()) else {
                return Err(CollectionsContractError::InvalidActions);
            };
            match action.action {
                CollectionsActionKindV1::SetLabel
                    if root_ids.contains(action.subject_node_id.as_str())
                        && matches!(node, DocumentNodeV2::Group { .. }) => {}
                CollectionsActionKindV1::SetArchived
                    if matches!(node, DocumentNodeV2::TerminalSlot { .. }) => {}
                _ => return Err(CollectionsContractError::InvalidActions),
            }
        }
        let actual_subjects = subjects
            .iter()
            .map(|(kind, subject)| (*kind, (*subject).to_string()))
            .collect::<BTreeSet<_>>();
        if actual_subjects != expected_subjects {
            return Err(CollectionsContractError::InvalidActions);
        }
        Ok(())
    }

    pub fn validate_invocation(
        &self,
        invocation: &CollectionsActionInvocationV1,
    ) -> Result<&CollectionsActionDeclarationV1, CollectionsContractError> {
        invocation.validate()?;
        if invocation.document_revision != self.document_revision
            || invocation.actions_revision != self.revision
        {
            return Err(CollectionsContractError::StaleRevision);
        }
        let declaration = self
            .actions
            .iter()
            .find(|action| action.id == invocation.action_id)
            .ok_or(CollectionsContractError::ActionMismatch)?;
        if declaration.action != invocation.input.kind()
            || declaration.subject_node_id != invocation.input.subject_node_id()
        {
            return Err(CollectionsContractError::ActionMismatch);
        }
        Ok(declaration)
    }
}

impl CollectionsActionInputV1 {
    pub fn kind(&self) -> CollectionsActionKindV1 {
        match self {
            Self::SetLabel { .. } => CollectionsActionKindV1::SetLabel,
            Self::SetArchived { .. } => CollectionsActionKindV1::SetArchived,
        }
    }

    pub fn container_id(&self) -> &str {
        match self {
            Self::SetLabel { container_id, .. } | Self::SetArchived { container_id, .. } => {
                container_id
            }
        }
    }

    pub fn subject_node_id(&self) -> String {
        match self {
            Self::SetLabel { container_id, .. } => collection_root_id(container_id),
            Self::SetArchived { terminal_id, .. } => terminal_slot_id(terminal_id),
        }
    }
}

impl CollectionsActionInvocationV1 {
    pub fn validate(&self) -> Result<(), CollectionsContractError> {
        if self.contract != COLLECTIONS_ACTION_INVOCATION_CONTRACT_V1 {
            return Err(CollectionsContractError::InvalidContract);
        }
        for identifier in [&self.ticket, &self.idempotency_key, &self.action_id] {
            validate_identifier(identifier)?;
        }
        if self.document_revision == 0
            || self.actions_revision == 0
            || self.expected_store_generation == 0
        {
            return Err(CollectionsContractError::StaleRevision);
        }
        validate_container_id(self.input.container_id())?;
        match (&self.input, &self.dependency) {
            (
                CollectionsActionInputV1::SetLabel {
                    container_id,
                    label,
                },
                CollectionsActionDependencyV1::Container {
                    container_id: dependency_container,
                },
            ) if container_id == dependency_container => validate_label(label.as_deref())?,
            (
                CollectionsActionInputV1::SetArchived {
                    container_id,
                    terminal_id,
                    ..
                },
                CollectionsActionDependencyV1::TerminalMembership {
                    container_id: dependency_container,
                    terminal_id: dependency_terminal,
                    terminal_snapshot_fingerprint_sha256,
                    provider_generation,
                },
            ) if container_id == dependency_container
                && terminal_id == dependency_terminal
                && *provider_generation > 0
                && valid_sha256(terminal_snapshot_fingerprint_sha256) =>
            {
                validate_terminal_id(terminal_id)?
            }
            _ => return Err(CollectionsContractError::ActionMismatch),
        }
        Ok(())
    }
}

impl CollectionsEffectPlanV1 {
    /// Validates one package-state CAS plan against the one-shot invocation that authorized it.
    pub fn validate_for(
        &self,
        invocation: &CollectionsActionInvocationV1,
    ) -> Result<(), CollectionsContractError> {
        invocation.validate()?;
        if self.contract != COLLECTIONS_EFFECT_PLAN_CONTRACT_V1 {
            return Err(CollectionsContractError::InvalidContract);
        }
        if self.ticket != invocation.ticket
            || self.idempotency_key != invocation.idempotency_key
            || self.expected_store_generation != invocation.expected_store_generation
            || self.expected_namespace_revision != invocation.expected_namespace_revision
            || self.state.key != invocation.input.container_id()
        {
            return Err(CollectionsContractError::InvalidEffectPlan);
        }
        validate_container_id(&self.state.key)?;
        if let Some(value) = &self.state.value {
            let size = serde_json::to_vec(value)
                .map_err(|_| CollectionsContractError::InvalidEffectPlan)?
                .len();
            if size > crate::extension_state::MAX_STATE_VALUE_BYTES {
                return Err(CollectionsContractError::InvalidEffectPlan);
            }
        }
        Ok(())
    }
}

fn validate_collection_tree(
    topology: &CollectionsProfileContainerV1,
    label: Option<&str>,
    root_id: &str,
    nodes: &HashMap<&str, &DocumentNodeV2>,
) -> Result<(), CollectionsContractError> {
    let Some(DocumentNodeV2::Group {
        accessibility,
        children,
        ..
    }) = nodes.get(root_id).copied()
    else {
        return Err(CollectionsContractError::InvalidProfile);
    };
    let rendered_label = label
        .filter(|label| !label.is_empty())
        .unwrap_or("Collection");
    if accessibility.name != rendered_label {
        return Err(CollectionsContractError::InvalidProfile);
    }
    let expected = topology
        .terminal_ids
        .iter()
        .map(String::as_str)
        .collect::<HashSet<_>>();
    let mut observed = HashSet::new();
    let mut active_order = Vec::new();
    let mut archived_order = Vec::new();
    let archive_id = archive_group_id(&topology.container_id);
    let mut saw_archive = false;
    for (index, child_id) in children.iter().enumerate() {
        match nodes.get(child_id.as_str()).copied() {
            Some(DocumentNodeV2::Text { .. }) if !saw_archive => {}
            Some(DocumentNodeV2::TerminalSlot { terminal_id, .. }) if !saw_archive => {
                validate_canonical_terminal_node(child_id, terminal_id)?;
                if !expected.contains(terminal_id.as_str())
                    || !observed.insert(terminal_id.as_str())
                {
                    return Err(CollectionsContractError::InvalidProfile);
                }
                active_order.push(terminal_id.as_str());
            }
            Some(DocumentNodeV2::Group {
                id,
                children: archive_children,
                ..
            }) if id == &archive_id && index + 1 == children.len() && !saw_archive => {
                saw_archive = true;
                for archive_child_id in archive_children {
                    let Some(DocumentNodeV2::TerminalSlot { terminal_id, .. }) =
                        nodes.get(archive_child_id.as_str()).copied()
                    else {
                        return Err(CollectionsContractError::InvalidProfile);
                    };
                    validate_canonical_terminal_node(archive_child_id, terminal_id)?;
                    if !expected.contains(terminal_id.as_str())
                        || !observed.insert(terminal_id.as_str())
                    {
                        return Err(CollectionsContractError::InvalidProfile);
                    }
                    archived_order.push(terminal_id.as_str());
                }
            }
            _ => return Err(CollectionsContractError::InvalidProfile),
        }
    }
    if observed != expected
        || !preserves_relative_order(&topology.terminal_ids, &active_order)
        || !preserves_relative_order(&topology.terminal_ids, &archived_order)
    {
        return Err(CollectionsContractError::InvalidProfile);
    }
    Ok(())
}

fn validate_topology(
    topology: &CollectionsProfileTopologyV1,
) -> Result<(), CollectionsContractError> {
    validate_identifier(&topology.workspace_id)?;
    validate_identifier(&topology.tab_id)?;
    if topology.producer.package_id != COLLECTIONS_PACKAGE_ID
        || topology.producer.service_id != "collections"
        || topology.state_dependency.contract != HOST_STATE_DEPENDENCY_CONTRACT_V1
        || topology.terminals_dependency.contract != HOST_TERMINALS_DEPENDENCY_CONTRACT_V1
        || topology.state_dependency.revision == 0
        || topology.state_dependency.generation == 0
        || topology.terminals_dependency.revision == 0
        || topology.terminals_dependency.generation == 0
    {
        return Err(CollectionsContractError::InvalidTopology);
    }
    if topology.containers.len() > crate::admission::MAX_GROUPS_PER_TAB
        || topology.placements.len() > crate::admission::MAX_TAB_PANES
    {
        return Err(CollectionsContractError::InvalidTopology);
    }
    let mut containers = BTreeSet::new();
    let mut terminals = BTreeSet::new();
    for container in &topology.containers {
        validate_container_id(&container.container_id)?;
        if !containers.insert(container.container_id.as_str())
            || container.terminal_ids.len() > crate::admission::MAX_GROUP_MEMBERS
        {
            return Err(CollectionsContractError::InvalidTopology);
        }
        for terminal_id in &container.terminal_ids {
            validate_terminal_id(terminal_id)?;
            if !terminals.insert(terminal_id.as_str()) {
                return Err(CollectionsContractError::InvalidTopology);
            }
        }
        if container
            .selected_terminal_id
            .as_ref()
            .is_some_and(|selected| !container.terminal_ids.contains(selected))
        {
            return Err(CollectionsContractError::InvalidTopology);
        }
    }
    let mut placements = BTreeSet::new();
    let mut pane_ids = BTreeSet::new();
    for placement in &topology.placements {
        validate_terminal_id(&placement.terminal_id)?;
        validate_identifier(&placement.pane_id)?;
        if !terminals.contains(placement.terminal_id.as_str())
            || !placements.insert(placement.terminal_id.as_str())
            || !pane_ids.insert(placement.pane_id.as_str())
        {
            return Err(CollectionsContractError::InvalidTopology);
        }
    }
    if placements != terminals {
        return Err(CollectionsContractError::InvalidTopology);
    }
    Ok(())
}

fn document_parent_ids(
    document: &HostDocumentV2,
) -> Result<HashMap<String, String>, CollectionsContractError> {
    let mut parents = HashMap::new();
    for node in &document.nodes {
        for child in node_children(node) {
            if parents
                .insert(child.clone(), node.id().to_string())
                .is_some()
            {
                return Err(CollectionsContractError::InvalidDocument);
            }
        }
    }
    Ok(parents)
}

fn depth_first_nodes(
    document: &HostDocumentV2,
) -> Result<Vec<&DocumentNodeV2>, CollectionsContractError> {
    let nodes = document
        .nodes
        .iter()
        .map(|node| (node.id(), node))
        .collect::<HashMap<_, _>>();
    fn visit<'a>(
        id: &str,
        nodes: &HashMap<&str, &'a DocumentNodeV2>,
        output: &mut Vec<&'a DocumentNodeV2>,
    ) -> Result<(), CollectionsContractError> {
        let node = nodes
            .get(id)
            .copied()
            .ok_or(CollectionsContractError::InvalidDocument)?;
        output.push(node);
        for child in node_children(node) {
            visit(child, nodes, output)?;
        }
        Ok(())
    }
    let mut output = Vec::with_capacity(document.nodes.len());
    for root in &document.roots {
        visit(root, &nodes, &mut output)?;
    }
    Ok(output)
}

fn node_children(node: &DocumentNodeV2) -> &[String] {
    match node {
        DocumentNodeV2::Group { children, .. } => children,
        DocumentNodeV2::Text { .. } | DocumentNodeV2::TerminalSlot { .. } => &[],
    }
}

fn archived_terminal_ids<'a>(
    document: &'a HostDocumentV2,
    container_id: &str,
) -> Result<HashSet<&'a str>, CollectionsContractError> {
    let nodes = document
        .nodes
        .iter()
        .map(|node| (node.id(), node))
        .collect::<HashMap<_, _>>();
    let Some(DocumentNodeV2::Group { children, .. }) =
        nodes.get(archive_group_id(container_id).as_str()).copied()
    else {
        return Ok(HashSet::new());
    };
    children
        .iter()
        .map(|child| match nodes.get(child.as_str()).copied() {
            Some(DocumentNodeV2::TerminalSlot { terminal_id, .. }) => Ok(terminal_id.as_str()),
            _ => Err(CollectionsContractError::InvalidProfile),
        })
        .collect()
}

fn preserves_relative_order(expected: &[String], observed: &[&str]) -> bool {
    let mut expected = expected.iter().map(String::as_str);
    observed
        .iter()
        .all(|observed| expected.any(|expected| expected == *observed))
}

fn validate_canonical_terminal_node(
    node_id: &str,
    terminal_id: &str,
) -> Result<(), CollectionsContractError> {
    validate_terminal_id(terminal_id)?;
    if node_id != terminal_slot_id(terminal_id) {
        return Err(CollectionsContractError::InvalidProfile);
    }
    Ok(())
}

fn validate_identifier(value: &str) -> Result<(), CollectionsContractError> {
    if value.is_empty()
        || value.len() > MAX_COLLECTIONS_IDENTIFIER_BYTES
        || !value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'/' | b'-')
        })
    {
        return Err(CollectionsContractError::InvalidIdentifier);
    }
    Ok(())
}

fn validate_action_label(value: &str) -> Result<(), CollectionsContractError> {
    if value.is_empty() {
        return Err(CollectionsContractError::InvalidActions);
    }
    validate_text(value, MAX_COLLECTIONS_ACTION_LABEL_BYTES)
        .map_err(|_| CollectionsContractError::InvalidActions)
}

fn validate_label(value: Option<&str>) -> Result<(), CollectionsContractError> {
    if let Some(value) = value {
        validate_text(value, MAX_COLLECTIONS_LABEL_BYTES)?;
    }
    Ok(())
}

fn validate_text(value: &str, max_bytes: usize) -> Result<(), CollectionsContractError> {
    if value.len() > max_bytes || value.chars().any(char::is_control) {
        return Err(CollectionsContractError::InvalidLabel);
    }
    Ok(())
}

fn validate_container_id(value: &str) -> Result<(), CollectionsContractError> {
    parse_container_raw(value).map(|_| ())
}

fn parse_container_raw(value: &str) -> Result<u64, CollectionsContractError> {
    let hex = value
        .strip_prefix("container_")
        .filter(|hex| hex.len() == 16)
        .filter(|hex| {
            hex.bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
        .ok_or(CollectionsContractError::InvalidIdentifier)?;
    let raw =
        u64::from_str_radix(hex, 16).map_err(|_| CollectionsContractError::InvalidIdentifier)?;
    if raw == 0 || raw == u64::MAX {
        return Err(CollectionsContractError::InvalidIdentifier);
    }
    Ok(raw)
}

fn valid_lower_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn valid_sha256(value: &str) -> bool {
    valid_lower_hex(value, 64)
}

fn valid_relative_package_path(value: &str) -> bool {
    !value.is_empty()
        && value.ends_with(".vqrox")
        && !value.starts_with('/')
        && !value.starts_with('\\')
        && value
            .split(['/', '\\'])
            .all(|segment| !segment.is_empty() && segment != "." && segment != "..")
}

fn validate_terminal_id(value: &str) -> Result<(), CollectionsContractError> {
    TerminalId::parse(value)
        .map(|_| ())
        .map_err(|_| CollectionsContractError::InvalidIdentifier)
}

fn collection_root_id(container_id: &str) -> String {
    format!("collection:{container_id}")
}

fn archive_group_id(container_id: &str) -> String {
    format!("archive:{container_id}")
}

fn terminal_slot_id(terminal_id: &str) -> String {
    format!("terminal:{terminal_id}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (CollectionsDocumentProfileV1, CollectionsProfileTopologyV1) {
        let profile: CollectionsDocumentProfileV1 = serde_json::from_slice(include_bytes!(
            "../docs/next/api/vqro-collections-v1.fixtures/document-profile-valid.json"
        ))
        .unwrap();
        let topology = CollectionsProfileTopologyV1 {
            workspace_id: "w1".into(),
            tab_id: "tab_11111111111111111111111111111111".into(),
            containers: vec![CollectionsProfileContainerV1 {
                container_id: "container_000000000000002a".into(),
                terminal_ids: vec![
                    "term_11111111111111111111111111111111".into(),
                    "term_22222222222222222222222222222222".into(),
                ],
                selected_terminal_id: Some("term_11111111111111111111111111111111".into()),
            }],
            placements: vec![
                CollectionsTerminalPlacementV1 {
                    terminal_id: "term_11111111111111111111111111111111".into(),
                    pane_id: "pane_1".into(),
                },
                CollectionsTerminalPlacementV1 {
                    terminal_id: "term_22222222222222222222222222222222".into(),
                    pane_id: "pane_2".into(),
                },
            ],
            producer: profile.document.producer.clone(),
            state_dependency: profile.document.dependencies[0].clone(),
            terminals_dependency: profile.document.dependencies[1].clone(),
        };
        (profile, topology)
    }

    #[test]
    fn r0_operation_baseline_has_bounded_cardinality() {
        let baseline: serde_json::Value = serde_json::from_slice(include_bytes!(
            "../docs/next/api/collections-r0-operation-baseline.json"
        ))
        .unwrap();
        assert_eq!(
            baseline["contract"],
            "vqro.collections.operation-baseline.v1"
        );
        let scenarios = baseline["scenarios"].as_array().unwrap();
        assert_eq!(scenarios.len(), 6);
        let mut service_counts = BTreeSet::new();
        let mut pane_counts = BTreeSet::new();
        let mut client_counts = BTreeSet::new();
        for scenario in scenarios {
            let services = scenario["enabled_services"].as_u64().unwrap();
            let changed_scopes = scenario["changed_scopes"].as_u64().unwrap();
            let panes = scenario["panes"].as_u64().unwrap();
            let clients = scenario["clients"].as_u64().unwrap();
            let counts = &scenario["counts"];
            service_counts.insert(services);
            pane_counts.insert(panes);
            client_counts.insert(clients);
            assert_eq!(counts["component_factories"].as_u64(), Some(services));
            assert_eq!(counts["wasmtime_engines"].as_u64(), Some(services));
            assert_eq!(counts["compiled_components"].as_u64(), Some(services));
            assert_eq!(
                counts["provider_invocations"].as_u64(),
                Some(changed_scopes)
            );
            assert_eq!(
                counts["extension_calls_during_frame_composition"].as_u64(),
                Some(0)
            );
            assert_eq!(counts["cached_projection_fanout"].as_u64(), Some(clients));
            if scenario["id"] == "one-hundred-service-cold-capacity" {
                assert_eq!(counts["active_stores"].as_u64(), Some(0));
                assert_eq!(counts["active_instances"].as_u64(), Some(0));
            } else {
                assert_eq!(counts["active_stores"].as_u64(), Some(services));
                assert_eq!(counts["active_instances"].as_u64(), Some(services));
            }
        }
        assert!(service_counts.is_superset(&BTreeSet::from([1, 15, 100])));
        assert!(pane_counts.is_superset(&BTreeSet::from([1, 15])));
        assert!(client_counts.is_superset(&BTreeSet::from([1, 3])));
        assert_eq!(
            baseline["bounded_completion_cases"]["stale_result_publications"],
            0
        );
        assert_eq!(
            baseline["bounded_completion_cases"]["canceled_result_publications"],
            0
        );
    }

    #[test]
    fn production_pin_binds_the_public_exact_git_artifact() {
        let pin: CollectionsProductionPinV1 = serde_json::from_slice(include_bytes!(
            "../extensions/first-party/collections/production-pin-v1.json"
        ))
        .unwrap();
        pin.validate().unwrap();
        assert_eq!(
            pin.source_commit,
            "f3f4d2f8ce1dce8c319bf4c4d0e7224850afa868"
        );
        assert_eq!(
            pin.package_sha256,
            "067726c3ab83b44485a28926a1fd485a9ab05d1bd807382e6388fb5457299676"
        );
        assert_eq!(pin.package_path, "dist/vqro-collections.vqrox");
    }

    #[test]
    fn package_authority_record_requires_snapshot_reader_v7() {
        let authority: CollectionsAuthorityV1 = serde_json::from_slice(include_bytes!(
            "../docs/next/api/vqro-collections-v1.fixtures/package-authority-valid.json"
        ))
        .unwrap();
        authority.validate().unwrap();
        assert_eq!(
            COLLECTIONS_PACKAGE_AUTHORITY_SNAPSHOT_VERSION,
            COLLECTIONS_PACKAGE_AUTHORITY_MINIMUM_READER_VERSION
        );
        let mut invalid_legacy = authority;
        invalid_legacy.phase = CollectionsAuthorityPhaseV1::Legacy;
        assert!(invalid_legacy.validate().is_err());
    }

    #[test]
    fn valid_profile_produces_synchronized_compatibility_outputs() {
        let (profile, topology) = fixture();
        let projected = profile.project_compatibility(&topology).unwrap();
        assert_eq!(projected.document.namespace, COLLECTIONS_NAMESPACE);
        assert_eq!(projected.document.revision, 7);
        assert_eq!(projected.collections.len(), 1);
        let collection = &projected.collections[0];
        assert_eq!(collection.collection_id, "collection_42");
        assert_eq!(collection.label.as_deref(), Some("Helpers"));
        assert_eq!(collection.selected_pane_id.as_deref(), Some("pane_1"));
        assert_eq!(
            collection
                .members
                .iter()
                .map(|member| member.pane_id.as_str())
                .collect::<Vec<_>>(),
            ["pane_1", "pane_2"]
        );
        assert!(!collection.members[0].archived);
        assert!(collection.members[1].archived);
        let terminal_items = projected
            .document
            .items
            .iter()
            .filter(|item| item.role == HostItemRole::Terminal)
            .collect::<Vec<_>>();
        assert_eq!(terminal_items.len(), 2);
        assert!(terminal_items[0].flags.selected);
        assert_eq!(
            terminal_items[1].parent_id.as_deref(),
            Some("archive:container_000000000000002a")
        );

        let fixture_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("docs/next/api/vqro-collections-v1.fixtures");
        for (name, value) in [
            (
                "projected-host-document-v1.json",
                serde_json::to_value(&projected.document).unwrap(),
            ),
            (
                "projected-collections-structure-v1.json",
                serde_json::to_value(&projected.collections).unwrap(),
            ),
        ] {
            let actual = format!("{}\n", serde_json::to_string_pretty(&value).unwrap());
            let path = fixture_dir.join(name);
            if std::env::var_os("HERDR_UPDATE_COLLECTIONS_FIXTURES").is_some() {
                std::fs::write(path, actual).unwrap();
            } else {
                assert_eq!(std::fs::read_to_string(path).unwrap(), actual);
            }
        }
    }

    #[test]
    fn provisional_empty_container_profile_is_valid_for_atomic_create() {
        let (mut profile, mut topology) = fixture();
        topology.containers[0].terminal_ids.clear();
        topology.containers[0].selected_terminal_id = None;
        topology.placements.clear();
        profile.document.nodes.retain(|node| {
            matches!(node, DocumentNodeV2::Group { id, .. } if id == "collection:container_000000000000002a")
                || matches!(node, DocumentNodeV2::Text { .. })
        });
        let DocumentNodeV2::Group { children, .. } = &mut profile.document.nodes[0] else {
            panic!("fixture begins with collection root")
        };
        children.retain(|id| id.starts_with("text:"));
        profile.validate(&topology).unwrap();
    }

    #[test]
    fn profile_rejects_archive_membership_outside_topology() {
        let (mut profile, topology) = fixture();
        let DocumentNodeV2::TerminalSlot {
            terminal_id, id, ..
        } = profile.document.nodes.last_mut().unwrap()
        else {
            panic!("fixture ends in archived slot")
        };
        *terminal_id = "term_33333333333333333333333333333333".into();
        *id = terminal_slot_id(terminal_id);
        assert_eq!(
            profile.validate(&topology),
            Err(CollectionsContractError::InvalidDocument)
        );
    }

    #[test]
    fn typed_actions_and_one_state_cas_are_bound_end_to_end() {
        let (profile, topology) = fixture();
        let declarations: CollectionsActionDeclarationsV1 = serde_json::from_slice(include_bytes!(
            "../docs/next/api/vqro-collections-v1.fixtures/actions-valid.json"
        ))
        .unwrap();
        declarations.validate_for(&profile, &topology).unwrap();
        let invocation: CollectionsActionInvocationV1 = serde_json::from_slice(include_bytes!(
            "../docs/next/api/vqro-collections-v1.fixtures/invocation-set-label-valid.json"
        ))
        .unwrap();
        declarations.validate_invocation(&invocation).unwrap();
        assert!(matches!(
            &invocation.dependency,
            CollectionsActionDependencyV1::Container { .. }
        ));
        let archive_invocation: CollectionsActionInvocationV1 =
            serde_json::from_slice(include_bytes!(
                "../docs/next/api/vqro-collections-v1.fixtures/invocation-set-archived-valid.json"
            ))
            .unwrap();
        declarations
            .validate_invocation(&archive_invocation)
            .unwrap();
        assert!(matches!(
            &archive_invocation.dependency,
            CollectionsActionDependencyV1::TerminalMembership { .. }
        ));
        let plan: CollectionsEffectPlanV1 = serde_json::from_slice(include_bytes!(
            "../docs/next/api/vqro-collections-v1.fixtures/effect-plan-valid.json"
        ))
        .unwrap();
        plan.validate_for(&invocation).unwrap();
    }

    #[test]
    fn frozen_invalid_fixtures_fail_normative_validation() {
        let (profile, topology) = fixture();
        let invalid_profile: CollectionsDocumentProfileV1 = serde_json::from_slice(include_bytes!(
            "../docs/next/api/vqro-collections-v1.fixtures/document-profile-invalid-root-label.json"
        ))
        .unwrap();
        assert_eq!(
            invalid_profile.validate(&topology),
            Err(CollectionsContractError::InvalidProfile)
        );
        let invalid_invocation: CollectionsActionInvocationV1 = serde_json::from_slice(
            include_bytes!(
                "../docs/next/api/vqro-collections-v1.fixtures/invocation-set-label-invalid-dependency.json"
            ),
        )
        .unwrap();
        assert_eq!(
            invalid_invocation.validate(),
            Err(CollectionsContractError::ActionMismatch)
        );
        let valid_invocation: CollectionsActionInvocationV1 =
            serde_json::from_slice(include_bytes!(
                "../docs/next/api/vqro-collections-v1.fixtures/invocation-set-label-valid.json"
            ))
            .unwrap();
        let invalid_plan: CollectionsEffectPlanV1 = serde_json::from_slice(include_bytes!(
            "../docs/next/api/vqro-collections-v1.fixtures/effect-plan-invalid-ticket.json"
        ))
        .unwrap();
        assert_eq!(
            invalid_plan.validate_for(&valid_invocation),
            Err(CollectionsContractError::InvalidEffectPlan)
        );
        profile.validate(&topology).unwrap();
    }

    #[test]
    fn cancellation_and_labeled_creation_vectors_are_atomic() {
        let cancellation: serde_json::Value = serde_json::from_slice(include_bytes!(
            "../docs/next/api/vqro-collections-v1.fixtures/cancellation-before-commit.json"
        ))
        .unwrap();
        assert_eq!(cancellation["expected"]["state_transactions"], 0);
        assert_eq!(cancellation["expected"]["visible_projection_changes"], 0);
        let creation: serde_json::Value = serde_json::from_slice(include_bytes!(
            "../docs/next/api/vqro-collections-v1.fixtures/labeled-create-atomic.json"
        ))
        .unwrap();
        assert_eq!(
            creation["expected_success"]["unlabeled_intermediate_publications"],
            0
        );
        assert_eq!(
            creation["expected_failure_before_two_image_commit"]["new_visible_containers"],
            0
        );
    }

    #[test]
    fn action_replay_fences_are_not_interchangeable() {
        let invocation: CollectionsActionInvocationV1 = serde_json::from_slice(include_bytes!(
            "../docs/next/api/vqro-collections-v1.fixtures/invocation-set-label-valid.json"
        ))
        .unwrap();
        let mut plan: CollectionsEffectPlanV1 = serde_json::from_slice(include_bytes!(
            "../docs/next/api/vqro-collections-v1.fixtures/effect-plan-valid.json"
        ))
        .unwrap();
        plan.ticket = "ticket:other".into();
        assert_eq!(
            plan.validate_for(&invocation),
            Err(CollectionsContractError::InvalidEffectPlan)
        );
    }
}
