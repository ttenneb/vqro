//! Public, versioned declarative contracts shared by bundled and third-party extensions.
//!
//! These values contain semantics and stable references only. The host remains
//! responsible for geometry, styling, focus, input, accessibility, PTYs, and
//! effect execution.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};

pub const HOST_DOCUMENT_CONTRACT_V1: &str = "host.document.v1";
/// One new, separately named, versioned view format. It travels the live host
/// document path with the same envelope, scope and action contract as
/// `host.document.v1` and adds the view blocks Vqro draws in panels. An older
/// client ignores the added item/action fields, so this stays append-only.
pub const HOST_VIEW_CONTRACT_V1: &str = "host.view.v1";
pub const MAX_VIEW_DEPTH: usize = 8;
pub const MAX_VIEW_ACTION_CHARS: usize = 24;
pub const MAX_DOCUMENT_PROVIDERS: usize = 16;
pub const MAX_DOCUMENT_ITEMS: usize = 512;
const MAX_SPACES_ITEMS: usize = MAX_DOCUMENT_ITEMS;
pub const MAX_EXTENSION_FRAME_BYTES: usize = 256 * 1024;
pub const MAX_RELATIONSHIP_SNAPSHOT_BYTES: usize = MAX_EXTENSION_FRAME_BYTES / 2;
pub const MAX_AGENT_ACTIVITY_SNAPSHOT_BYTES: usize = MAX_EXTENSION_FRAME_BYTES / 2;
pub const MAX_DOCUMENT_ACTIONS: usize = 256;
pub const MAX_DOCUMENT_TEXT_CHARS: usize = 4096;
pub const MAX_SPACES_REPOSITORIES: usize = 256;
pub const MAX_SPACES_WORKSPACES: usize = 512;
pub const MAX_RELATIONSHIPS: usize = 4096;
pub const MAX_RELATIONSHIP_DEPTH: usize = 32;
pub const MAX_RELATIONSHIP_FAN_OUT: usize = 128;
pub const MAX_RELATIONSHIP_METADATA_ENTRIES: usize = 32;
pub const MAX_RELATIONSHIP_METADATA_BYTES: usize = 16 * 1024;
pub const MAX_RELATIONSHIP_METADATA_DEPTH: usize = 8;
pub const MAX_AGENT_ACTIVITIES: usize = 256;
pub const MAX_AGENT_ACTIVITY_LABELS: usize = 16;

pub const HOST_RELATIONSHIPS_CONTRACT_V1: &str = "host.relationships.v1";
pub const HOST_AGENT_ACTIVITY_CONTRACT_V1: &str = "host.agent-activity.v1";

/// A host-owned session projection supplied to Spaces document providers.
/// Repository membership and labels are semantic state; paths and Git internals
/// are intentionally excluded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct HostSpacesV1 {
    pub contract: String,
    pub generation: u64,
    pub revision: u64,
    pub scope_id: String,
    #[serde(default)]
    pub repositories: Vec<HostSpaceRepository>,
    #[serde(default)]
    pub standalone: Vec<HostSpaceWorkspaceRef>,
    #[serde(default)]
    pub order: Vec<HostSpaceOrderRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct HostSpaceRepository {
    pub repository_id: String,
    pub label: String,
    #[serde(default)]
    pub workspaces: Vec<HostSpaceWorkspaceRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct HostSpaceWorkspaceRef {
    pub workspace_id: String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HostSpaceOrderRef {
    Repository { repository_id: String },
    StandaloneWorkspace { workspace_id: String },
}

/// A session-scoped, host-owned relationship forest. Relationship kinds and
/// metadata keys are namespaces so consumers can safely ignore semantics they
/// do not understand while retaining the records unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct HostRelationshipsV1 {
    pub contract: String,
    pub generation: u64,
    pub revision: u64,
    pub scope_id: String,
    #[serde(default)]
    pub relationships: Vec<HostRelationship>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct HostRelationship {
    pub relationship_id: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal: Option<HostTerminalRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    pub sibling_rank: u64,
    pub lifecycle: HostRelationshipLifecycle,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum HostRelationshipLifecycle {
    Active,
    Retained,
}

/// Session-scoped, attributed agent activity after host arbitration. This is
/// intentionally smaller than any agent-specific state model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct HostAgentActivityV1 {
    pub contract: String,
    pub generation: u64,
    pub revision: u64,
    pub scope_id: String,
    #[serde(default)]
    pub activities: Vec<HostAgentActivityRecord>,
    /// Complete current placements, including non-agent terminals. Absence is unknown.
    /// Placement never proves an execution is current or grants task authority.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminals: Option<Vec<HostTerminalRef>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct HostAgentActivityRecord {
    pub terminal: HostTerminalRef,
    pub activity: HostAgentActivityState,
    /// Stable identity of the evidence publisher.
    pub source: String,
    /// Evidence kind. Unknown kinds remain round-trippable strings.
    pub evidence_kind: String,
    pub state_change_seq: u64,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,
}

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    PartialOrd,
    Ord,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum HostAgentActivityState {
    Idle,
    Working,
    Blocked,
    Unknown,
}

/// A host-owned terminal-group projection supplied to document providers.
/// It contains stable identities and semantic state, never PTYs or geometry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct HostTerminalGroupsV1 {
    pub contract: String,
    pub generation: u64,
    pub revision: u64,
    pub scope_id: String,
    pub groups: Vec<HostTerminalGroup>,
    #[serde(default)]
    pub ungrouped: Vec<HostTerminalGroupMember>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct HostTerminalGroup {
    pub group_id: String,
    pub label: Option<String>,
    pub members: Vec<HostTerminalGroupMember>,
    pub selected_terminal_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct HostTerminalGroupMember {
    pub terminal: HostTerminalRef,
    pub label: String,
    pub lifecycle: HostTerminalLifecycle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum HostTerminalLifecycle {
    Live,
    Exited,
    Archived,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct HostDocumentV1 {
    pub contract: String,
    pub namespace: String,
    pub generation: u64,
    pub revision: u64,
    pub surface: HostSurface,
    pub target: HostTarget,
    #[serde(default)]
    pub items: Vec<HostDocumentItem>,
    #[serde(default)]
    pub actions: Vec<HostDocumentAction>,
    #[serde(default)]
    pub required_capabilities: Vec<HostCapability>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum HostSurface {
    Navigation,
    Pane,
}

#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HostTarget {
    Session,
    Workspace { workspace_id: String },
    Tab { tab_id: String },
    Pane { pane_id: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct HostDocumentItem {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    pub role: HostItemRole,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal: Option<HostTerminalRef>,
    #[serde(default)]
    pub action_ids: Vec<String>,
    #[serde(default)]
    pub flags: HostDocumentItemFlags,
    /// View only: this item's view blocks. A legacy document never carries it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view: Option<HostViewItem>,
}

/// The view blocks of one item. Every field is optional: an item with no view
/// block is a plain group, and an unknown future block degrades to
/// "unsupported" instead of rejecting the view.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct HostViewItem {
    /// What this block is, as a free string so the set can grow: `section` and
    /// `row` are known, and anything else is drawn as "unsupported" in place
    /// rather than dropped. Never a closed enum: an unknown variant would make
    /// an older client reject the whole view.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// A panel root: the pane whose panel this item's subtree fills. Only a
    /// root item (no parent) may set it. Vqro draws the pane's bottom edge slot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<String>,
    /// A panel root: the stable id of this view inside the pane's panel stack,
    /// as the user names it to `vqro pane panel`. Only a root may set it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view_id: Option<String>,
    /// A row state, or a panel root's aggregate state for its header glyph.
    /// Vqro draws the fixed Pi-style glyph and its tone; the extension owns
    /// only which state the row or panel is in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<HostViewState>,
    /// What the row is waiting for, in one short word ("unblock", "report").
    /// Vqro composes the sentence around it, so the row never spells out
    /// bookkeeping of its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    /// Whose move the row waits for. Vqro says "you" or names the terminal
    /// link, which is the only party it can speak about live.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waiting: Option<HostViewWaiting>,
    /// Optional neutral presentation metadata, never executable by itself.
    /// Only JSON host-document controls carry this type; no frozen binary lane.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub affordance: Option<serde_json::Value>,
    /// Unix epoch seconds whose age Vqro draws. Only the extension knows what
    /// the instant means.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<u64>,
}

/// Whose move a row waits for.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum HostViewWaiting {
    /// The user of this client owes the action.
    #[default]
    You,
    /// The row's terminal link owes the action; Vqro names it.
    Them,
}

/// The fixed state vocabulary Vqro draws a glyph and tone for: the same set the
/// Pi todo widget uses, so panels read like the rest of the UI.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum HostViewState {
    /// Not started or unblocked: `○`.
    #[default]
    Open,
    /// Being worked on: `◐`.
    InProgress,
    /// Cannot proceed: `⊘`.
    Blocked,
    /// Waiting for someone to accept it: `◎`.
    Reported,
    /// Finished: `●`.
    Done,
}

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum HostViewTone {
    #[default]
    Default,
    Muted,
    Accent,
    Success,
    Warning,
    Danger,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum HostItemRole {
    Group,
    Terminal,
    Section,
    Command,
    Status,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct HostTerminalRef {
    /// Stable runtime identity. It does not change when a pane moves, is
    /// promoted from a group, or is shown by another client.
    pub terminal_id: String,
    /// Optional, ephemeral placement context for resolving host interactions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placement: Option<HostTerminalPlacement>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct HostTerminalPlacement {
    pub pane_id: String,
    pub workspace_id: String,
    pub tab_id: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct HostDocumentItemFlags {
    #[serde(default)]
    pub selected: bool,
    #[serde(default)]
    pub expanded: bool,
    #[serde(default)]
    pub disabled: bool,
    #[serde(default)]
    pub attention: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct HostDocumentExtensionActionV1 {
    pub contract: String,
    pub namespace: String,
    pub generation: u64,
    pub revision: u64,
    pub scope_id: String,
    pub action_id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct HostDocumentAction {
    pub id: String,
    pub label: String,
    pub operation: HostDocumentOperation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_id: Option<String>,
    #[serde(default)]
    pub confirmation: HostConfirmation,
    // v1 is read-only: how a user answers an action inside a panel is deferred
    // until the interaction model is designed, so no shape for it is frozen
    // here. An unknown field is ignored by any client, so the shape can be added
    // later without breaking this one.
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HostDocumentOperation {
    /// Routed only to the provider identified by the enclosing document's
    /// namespace and generation. `name` is never resolved globally.
    Extension { name: String },
    /// A fixed host-owned effect. The provider must hold the corresponding
    /// capability and the host still validates the referenced identity.
    Host { effect: HostEffect },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum HostCapability {
    FocusTerminal,
    TerminalInterrupt,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HostEffect {
    FocusTerminal { terminal_id: String },
    TerminalInterrupt { terminal_id: String },
}

impl HostEffect {
    fn required_capability(&self) -> HostCapability {
        match self {
            Self::FocusTerminal { .. } => HostCapability::FocusTerminal,
            Self::TerminalInterrupt { .. } => HostCapability::TerminalInterrupt,
        }
    }

    /// Re-check authority at effect dispatch. A document's declaration is not
    /// a grant and must never be used as one.
    fn authorize_dispatch(
        &self,
        granted_capabilities: &HashSet<HostCapability>,
        confirmed: bool,
    ) -> Result<(), HostDocumentError> {
        if !granted_capabilities.contains(&self.required_capability()) {
            return invalid("host effect capability was not granted");
        }
        if matches!(self, Self::TerminalInterrupt { .. }) && !confirmed {
            return invalid("terminal interrupt was not confirmed");
        }
        Ok(())
    }
}

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum HostConfirmation {
    #[default]
    None,
    Required,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostDocumentError(pub String);

impl std::fmt::Display for HostDocumentError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl HostSpacesV1 {
    pub fn validate(&self) -> Result<(), HostDocumentError> {
        if self.contract != "host.spaces.v1" {
            return invalid("unsupported spaces contract");
        }
        if self.generation == 0 || self.revision == 0 {
            return invalid("spaces generation and revision must be non-zero");
        }
        validate_stable_ref(&self.scope_id)?;
        if self.repositories.len() > MAX_SPACES_REPOSITORIES {
            return invalid("spaces snapshot has too many repositories");
        }

        let mut repository_ids = HashSet::with_capacity(self.repositories.len());
        let mut workspace_ids = HashSet::new();
        let mut workspace_count = self.standalone.len();
        for repository in &self.repositories {
            validate_stable_ref(&repository.repository_id)?;
            validate_text(&repository.label)?;
            if !repository_ids.insert(repository.repository_id.as_str()) {
                return invalid("spaces repository ids must be unique");
            }
            workspace_count = workspace_count
                .checked_add(repository.workspaces.len())
                .ok_or_else(|| {
                    HostDocumentError("spaces snapshot has too many workspaces".into())
                })?;
            for workspace in &repository.workspaces {
                validate_space_workspace(workspace, &mut workspace_ids)?;
            }
        }
        for workspace in &self.standalone {
            validate_space_workspace(workspace, &mut workspace_ids)?;
        }
        if workspace_count > MAX_SPACES_WORKSPACES
            || workspace_count + self.repositories.len() > MAX_SPACES_ITEMS
        {
            return invalid("spaces snapshot has too many workspaces");
        }
        if !self.order.is_empty() {
            if self.order.len() != self.repositories.len() + self.standalone.len() {
                return invalid("spaces order must contain every top-level space exactly once");
            }
            let standalone_ids = self
                .standalone
                .iter()
                .map(|workspace| workspace.workspace_id.as_str())
                .collect::<HashSet<_>>();
            let mut ordered = HashSet::new();
            for entry in &self.order {
                let key = match entry {
                    HostSpaceOrderRef::Repository { repository_id } => {
                        if !repository_ids.contains(repository_id.as_str()) {
                            return invalid("spaces order references an unknown repository");
                        }
                        format!("repository:{repository_id}")
                    }
                    HostSpaceOrderRef::StandaloneWorkspace { workspace_id } => {
                        if !standalone_ids.contains(workspace_id.as_str()) {
                            return invalid(
                                "spaces order references an unknown standalone workspace",
                            );
                        }
                        format!("workspace:{workspace_id}")
                    }
                };
                if !ordered.insert(key) {
                    return invalid("spaces order contains duplicate entries");
                }
            }
        }
        Ok(())
    }
}

impl HostRelationshipsV1 {
    pub fn validate(&self) -> Result<(), HostDocumentError> {
        if self.contract != HOST_RELATIONSHIPS_CONTRACT_V1 {
            return invalid("unsupported relationships contract");
        }
        validate_snapshot_header(self.generation, self.revision, &self.scope_id)?;
        if self.relationships.len() > MAX_RELATIONSHIPS {
            return invalid("relationship snapshot has too many records");
        }

        let mut ids = HashSet::with_capacity(self.relationships.len());
        for relationship in &self.relationships {
            validate_stable_ref(&relationship.relationship_id)?;
            validate_namespace(&relationship.kind)?;
            if !ids.insert(relationship.relationship_id.as_str()) {
                return invalid("relationship ids must be unique");
            }
            if let Some(terminal) = &relationship.terminal {
                validate_terminal_ref(terminal)?;
            }
            if relationship.metadata.len() > MAX_RELATIONSHIP_METADATA_ENTRIES {
                return invalid("relationship metadata has too many entries");
            }
            for (key, value) in &relationship.metadata {
                validate_namespace(key)?;
                validate_metadata_value(value, 0)?;
            }
            let encoded_size = serde_json::to_vec(&relationship.metadata)
                .map_err(|_| HostDocumentError("relationship metadata is invalid".into()))?
                .len();
            if encoded_size > MAX_RELATIONSHIP_METADATA_BYTES {
                return invalid("relationship metadata is too large");
            }
        }

        let mut sibling_ranks = HashSet::new();
        let mut child_counts: HashMap<Option<&str>, usize> = HashMap::new();
        for relationship in &self.relationships {
            let parent = relationship.parent_id.as_deref();
            if let Some(parent_id) = parent {
                validate_stable_ref(parent_id)?;
                if parent_id == relationship.relationship_id || !ids.contains(parent_id) {
                    return invalid("relationship parent is invalid");
                }
            }
            if !sibling_ranks.insert((parent, relationship.sibling_rank)) {
                return invalid("relationship sibling ranks must be unique");
            }
            let count = child_counts.entry(parent).or_default();
            *count += 1;
            if parent.is_some() && *count > MAX_RELATIONSHIP_FAN_OUT {
                return invalid("relationship parent has too many children");
            }
        }
        validate_relationship_tree(&self.relationships)?;
        if serde_json::to_vec(self)
            .map_err(|_| HostDocumentError("relationship snapshot is not serializable".into()))?
            .len()
            > MAX_RELATIONSHIP_SNAPSHOT_BYTES
        {
            return invalid("relationship snapshot exceeds extension transport capacity");
        }
        Ok(())
    }
}

impl HostAgentActivityV1 {
    pub fn validate(&self) -> Result<(), HostDocumentError> {
        if self.contract != HOST_AGENT_ACTIVITY_CONTRACT_V1 {
            return invalid("unsupported agent activity contract");
        }
        validate_snapshot_header(self.generation, self.revision, &self.scope_id)?;
        if self.activities.len() > MAX_AGENT_ACTIVITIES {
            return invalid("agent activity snapshot has too many records");
        }

        let mut terminal_ids = HashSet::with_capacity(self.activities.len());
        for activity in &self.activities {
            validate_terminal_ref(&activity.terminal)?;
            if !terminal_ids.insert(activity.terminal.terminal_id.as_str()) {
                return invalid("agent activity terminal identities must be unique");
            }
            validate_text(&activity.source)?;
            validate_local_id(&activity.evidence_kind)?;
            if activity.labels.len() > MAX_AGENT_ACTIVITY_LABELS {
                return invalid("agent activity has too many labels");
            }
            for (key, value) in &activity.labels {
                validate_local_id(key)?;
                validate_text(value)?;
            }
        }
        if let Some(terminals) = &self.terminals {
            if terminals.len() > MAX_AGENT_ACTIVITIES {
                return invalid("agent activity snapshot has too many terminal placements");
            }
            let mut ids = HashSet::with_capacity(terminals.len());
            for terminal in terminals {
                validate_terminal_ref(terminal)?;
                if terminal.placement.is_none() || !ids.insert(&terminal.terminal_id) {
                    return invalid("terminal placements must be complete and unique");
                }
            }
            if self
                .activities
                .iter()
                .any(|activity| !terminals.contains(&activity.terminal))
            {
                return invalid("agent activity is outside current terminal placements");
            }
        }
        if serde_json::to_vec(self)
            .map_err(|_| HostDocumentError("agent activity snapshot is not serializable".into()))?
            .len()
            > MAX_AGENT_ACTIVITY_SNAPSHOT_BYTES
        {
            return invalid("agent activity snapshot exceeds extension transport capacity");
        }
        Ok(())
    }
}

impl HostTerminalGroupsV1 {
    pub fn validate(&self) -> Result<(), HostDocumentError> {
        if self.contract != "host.terminal-groups.v1" {
            return invalid("unsupported terminal-group contract");
        }
        if self.generation == 0 || self.revision == 0 {
            return invalid("terminal-group generation and revision must be non-zero");
        }
        validate_stable_ref(&self.scope_id)?;
        let mut group_ids = HashSet::new();
        let mut terminal_ids = HashSet::new();
        for member in &self.ungrouped {
            validate_terminal_ref(&member.terminal)?;
            if !terminal_ids.insert(member.terminal.terminal_id.as_str()) {
                return invalid("a terminal may appear only once in a group snapshot");
            }
        }
        for group in &self.groups {
            validate_stable_ref(&group.group_id)?;
            if !group_ids.insert(group.group_id.as_str()) {
                return invalid("terminal group ids must be unique");
            }
            for member in &group.members {
                validate_terminal_ref(&member.terminal)?;
                if !terminal_ids.insert(member.terminal.terminal_id.as_str()) {
                    return invalid("a terminal may appear only once in a group snapshot");
                }
            }
            if let Some(selected) = group.selected_terminal_id.as_deref() {
                if !group
                    .members
                    .iter()
                    .any(|member| member.terminal.terminal_id == selected)
                {
                    return invalid("selected terminal must be a group member");
                }
            }
        }
        Ok(())
    }
}

impl HostDocumentV1 {
    pub fn authorize_extension_action_dispatch(
        &self,
        action_id: &str,
        host_snapshot: &HostTerminalGroupsV1,
    ) -> Result<HostDocumentExtensionActionV1, HostDocumentError> {
        self.validate()?;
        host_snapshot.validate()?;
        if self.generation != host_snapshot.generation || self.revision != host_snapshot.revision {
            return invalid("extension action document does not match host snapshot revision");
        }
        if !matches!(
            &self.target,
            HostTarget::Tab { tab_id } if tab_id == &host_snapshot.scope_id
        ) {
            return invalid("extension action document is outside host snapshot scope");
        }
        let action = self
            .actions
            .iter()
            .find(|action| action.id == action_id)
            .ok_or_else(|| HostDocumentError("document action not found".into()))?;
        let HostDocumentOperation::Extension { name } = &action.operation else {
            return invalid("document action is not provider-owned");
        };
        let enabled_references = self.items.iter().filter(|item| {
            !item.flags.disabled && item.action_ids.iter().any(|id| id == &action.id)
        });
        if let Some(subject_id) = action.subject_id.as_deref() {
            if !enabled_references
                .into_iter()
                .any(|item| item.id == subject_id)
            {
                return invalid("document action subject is disabled or unavailable");
            }
        } else if enabled_references.count() == 0 {
            return invalid("document action is not reachable from an enabled item");
        }
        if action.confirmation == HostConfirmation::Required {
            return invalid("provider-owned destructive actions require a host effect contract");
        }
        Ok(HostDocumentExtensionActionV1 {
            contract: HOST_DOCUMENT_CONTRACT_V1.into(),
            namespace: self.namespace.clone(),
            generation: self.generation,
            revision: self.revision,
            scope_id: host_snapshot.scope_id.clone(),
            action_id: action.id.clone(),
            name: name.clone(),
            subject_id: action.subject_id.clone(),
        })
    }

    /// Authorize one host effect against both the provider grant and the
    /// identities/scope in this validated document.
    pub fn authorize_host_effect_dispatch(
        &self,
        action_id: &str,
        host_snapshot: &HostTerminalGroupsV1,
        granted_capabilities: &HashSet<HostCapability>,
        confirmed: bool,
    ) -> Result<&HostEffect, HostDocumentError> {
        self.validate()?;
        host_snapshot.validate()?;
        if self.generation != host_snapshot.generation || self.revision != host_snapshot.revision {
            return invalid("host effect document does not match host snapshot revision");
        }
        let action = self
            .actions
            .iter()
            .find(|action| action.id == action_id)
            .ok_or_else(|| HostDocumentError("document action not found".into()))?;
        let HostDocumentOperation::Host { effect } = &action.operation else {
            return invalid("document action is not a host effect");
        };
        effect.authorize_dispatch(granted_capabilities, confirmed)?;
        let terminal_id = match effect {
            HostEffect::FocusTerminal { terminal_id }
            | HostEffect::TerminalInterrupt { terminal_id } => terminal_id,
        };
        let subject = action
            .subject_id
            .as_deref()
            .and_then(|subject| self.items.iter().find(|item| item.id == subject))
            .ok_or_else(|| HostDocumentError("host effect requires a document subject".into()))?;
        if subject.flags.disabled {
            return invalid("host effect subject is disabled");
        }
        if !subject.action_ids.iter().any(|id| id == &action.id) {
            return invalid("host effect is not reachable from its subject");
        }
        let terminal = subject
            .terminal
            .as_ref()
            .filter(|terminal| terminal.terminal_id == *terminal_id)
            .ok_or_else(|| {
                HostDocumentError("host effect terminal is outside its subject".into())
            })?;
        let host_terminal = host_snapshot
            .groups
            .iter()
            .flat_map(|group| &group.members)
            .chain(&host_snapshot.ungrouped)
            .map(|member| &member.terminal)
            .find(|candidate| candidate.terminal_id == *terminal_id)
            .ok_or_else(|| HostDocumentError("host effect terminal is not host-owned".into()))?;
        if terminal != host_terminal || !terminal_ref_matches_target(host_terminal, &self.target) {
            return invalid("host effect terminal is outside document scope");
        }
        Ok(effect)
    }

    pub fn validate(&self) -> Result<(), HostDocumentError> {
        let is_view = self.contract == HOST_VIEW_CONTRACT_V1;
        if !is_view && self.contract != HOST_DOCUMENT_CONTRACT_V1 {
            return invalid("unsupported host document contract");
        }
        validate_namespace(&self.namespace)?;
        if self.generation == 0 || self.revision == 0 {
            return invalid("generation and revision must be non-zero");
        }
        if self.items.len() > MAX_DOCUMENT_ITEMS {
            return invalid("document has too many items");
        }
        if self.actions.len() > MAX_DOCUMENT_ACTIONS {
            return invalid("document has too many actions");
        }

        let mut item_ids = HashSet::with_capacity(self.items.len());
        for item in &self.items {
            validate_local_id(&item.id)?;
            validate_text(&item.label)?;
            if let Some(detail) = &item.detail {
                validate_text(detail)?;
            }
            if !item_ids.insert(item.id.as_str()) {
                return invalid("document item ids must be unique");
            }
            if let Some(terminal) = &item.terminal {
                validate_terminal_ref(terminal)?;
            }
            validate_view_item_fields(item, is_view)?;
        }
        if is_view {
            validate_item_depth(&self.items)?;
        }
        for item in &self.items {
            if let Some(parent) = item.parent_id.as_deref() {
                if parent == item.id || !item_ids.contains(parent) {
                    return invalid("document item parent is invalid");
                }
            }
        }
        validate_item_cycles(&self.items)?;

        let capabilities = self
            .required_capabilities
            .iter()
            .copied()
            .collect::<HashSet<_>>();
        if capabilities.len() != self.required_capabilities.len() {
            return invalid("document capabilities must be unique");
        }
        let mut action_ids = HashSet::with_capacity(self.actions.len());
        for action in &self.actions {
            validate_local_id(&action.id)?;
            validate_text(&action.label)?;
            match &action.operation {
                HostDocumentOperation::Extension { name } => validate_local_id(name)?,
                HostDocumentOperation::Host { effect } => {
                    if !capabilities.contains(&effect.required_capability()) {
                        return invalid("host effect capability was not declared");
                    }
                    match effect {
                        HostEffect::FocusTerminal { terminal_id } => {
                            validate_stable_ref(terminal_id)?
                        }
                        HostEffect::TerminalInterrupt { terminal_id } => {
                            validate_stable_ref(terminal_id)?;
                            if action.confirmation != HostConfirmation::Required {
                                return invalid("terminal interrupt requires confirmation");
                            }
                        }
                    }
                }
            }
            if !action_ids.insert(action.id.as_str()) {
                return invalid("document action ids must be unique");
            }
        }
        for item in &self.items {
            for action_id in &item.action_ids {
                let Some(action) = self.actions.iter().find(|action| action.id == *action_id)
                else {
                    return invalid("document item references an unknown action");
                };
                if action
                    .subject_id
                    .as_deref()
                    .is_some_and(|subject_id| subject_id != item.id)
                {
                    return invalid("document item references an action for another subject");
                }
            }
        }
        Ok(())
    }
}

fn validate_snapshot_header(
    generation: u64,
    revision: u64,
    scope_id: &str,
) -> Result<(), HostDocumentError> {
    if generation == 0 || revision == 0 {
        return invalid("snapshot generation and revision must be non-zero");
    }
    validate_stable_ref(scope_id)
}

fn validate_relationship_tree(relationships: &[HostRelationship]) -> Result<(), HostDocumentError> {
    let parents = relationships
        .iter()
        .map(|relationship| {
            (
                relationship.relationship_id.as_str(),
                relationship.parent_id.as_deref(),
            )
        })
        .collect::<HashMap<_, _>>();
    for relationship in relationships {
        let mut current = Some(relationship.relationship_id.as_str());
        let mut visited = HashSet::new();
        let mut depth = 0;
        while let Some(id) = current {
            if !visited.insert(id) {
                return invalid("relationship hierarchy contains a cycle");
            }
            depth += 1;
            if depth > MAX_RELATIONSHIP_DEPTH {
                return invalid("relationship hierarchy is too deep");
            }
            current = parents.get(id).copied().flatten();
        }
    }
    Ok(())
}

fn validate_metadata_value(
    value: &serde_json::Value,
    depth: usize,
) -> Result<(), HostDocumentError> {
    if depth > MAX_RELATIONSHIP_METADATA_DEPTH {
        return invalid("relationship metadata is too deep");
    }
    match value {
        serde_json::Value::String(value) => validate_text(value),
        serde_json::Value::Array(values) => {
            for value in values {
                validate_metadata_value(value, depth + 1)?;
            }
            Ok(())
        }
        serde_json::Value::Object(values) => {
            for (key, value) in values {
                validate_text(key)?;
                validate_metadata_value(value, depth + 1)?;
            }
            Ok(())
        }
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {
            Ok(())
        }
    }
}

fn validate_space_workspace<'a>(
    workspace: &'a HostSpaceWorkspaceRef,
    workspace_ids: &mut HashSet<&'a str>,
) -> Result<(), HostDocumentError> {
    validate_stable_ref(&workspace.workspace_id)?;
    validate_text(&workspace.label)?;
    if !workspace_ids.insert(workspace.workspace_id.as_str()) {
        return invalid("a workspace may appear only once in a spaces snapshot");
    }
    Ok(())
}

fn validate_namespace(value: &str) -> Result<(), HostDocumentError> {
    if value.len() > 128
        || !value.contains('.')
        || !value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'-')
        })
    {
        return invalid("invalid extension namespace");
    }
    Ok(())
}

fn validate_local_id(value: &str) -> Result<(), HostDocumentError> {
    if value.is_empty()
        || value.len() > 128
        || !value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b':' | b'/' | b'_' | b'-')
        })
    {
        return invalid("invalid local id");
    }
    Ok(())
}

fn validate_stable_ref(value: &str) -> Result<(), HostDocumentError> {
    validate_local_id(value)
}

fn validate_text(value: &str) -> Result<(), HostDocumentError> {
    if value.chars().count() > MAX_DOCUMENT_TEXT_CHARS
        || value.chars().any(|character| character.is_control())
    {
        return invalid("invalid document text");
    }
    Ok(())
}

/// View blocks exist only on `host.view.v1`. A legacy document that carries
/// them is rejected instead of being drawn with fields its consumers cannot
/// see, so a provider cannot publish view semantics under the old name.
fn validate_view_item_fields(
    item: &HostDocumentItem,
    is_view: bool,
) -> Result<(), HostDocumentError> {
    let has_view_fields = item.view.is_some();
    if !is_view {
        return if has_view_fields {
            invalid("view item fields require the view contract")
        } else {
            Ok(())
        };
    }
    let Some(view) = &item.view else {
        return Ok(());
    };
    if view
        .affordance
        .as_ref()
        .is_some_and(|value| value.to_string().len() > 8192)
    {
        return invalid("view affordance exceeds byte bound");
    }
    if let Some(action) = &view.action {
        if action.trim().is_empty() || action.chars().count() > MAX_VIEW_ACTION_CHARS {
            return invalid("invalid view action text");
        }
        validate_text(action)?;
    }
    if (view.pane_id.is_some() || view.view_id.is_some()) && item.parent_id.is_some() {
        return invalid("only a panel root may name its pane or view");
    }
    if view.view_id.is_some() != view.pane_id.is_some() {
        return invalid("a panel root names both its pane and its view");
    }
    if view.pane_id.is_none() && item.parent_id.is_none() {
        return invalid("a view block needs a panel root or a parent section");
    }
    if let Some(kind) = &view.kind {
        validate_local_id(kind)?;
    }
    if let Some(pane_id) = &view.pane_id {
        validate_local_id(pane_id)?;
    }
    if let Some(view_id) = &view.view_id {
        validate_local_id(view_id)?;
    }
    Ok(())
}

fn validate_item_depth(items: &[HostDocumentItem]) -> Result<(), HostDocumentError> {
    let parents = items
        .iter()
        .map(|item| (item.id.as_str(), item.parent_id.as_deref()))
        .collect::<HashMap<_, _>>();
    for item in items {
        let mut current = Some(item.id.as_str());
        let mut depth = 0;
        while let Some(id) = current {
            depth += 1;
            if depth > MAX_VIEW_DEPTH {
                return invalid("view hierarchy is too deep");
            }
            current = parents.get(id).copied().flatten();
        }
    }
    Ok(())
}

fn terminal_ref_matches_target(terminal: &HostTerminalRef, target: &HostTarget) -> bool {
    match target {
        HostTarget::Session => true,
        HostTarget::Workspace { workspace_id } => terminal
            .placement
            .as_ref()
            .is_some_and(|placement| placement.workspace_id == *workspace_id),
        HostTarget::Tab { tab_id } => terminal
            .placement
            .as_ref()
            .is_some_and(|placement| placement.tab_id == *tab_id),
        HostTarget::Pane { pane_id } => terminal
            .placement
            .as_ref()
            .is_some_and(|placement| placement.pane_id == *pane_id),
    }
}

fn validate_terminal_ref(terminal: &HostTerminalRef) -> Result<(), HostDocumentError> {
    validate_stable_ref(&terminal.terminal_id)?;
    if let Some(placement) = &terminal.placement {
        validate_stable_ref(&placement.pane_id)?;
        validate_stable_ref(&placement.workspace_id)?;
        validate_stable_ref(&placement.tab_id)?;
    }
    Ok(())
}

fn validate_item_cycles(items: &[HostDocumentItem]) -> Result<(), HostDocumentError> {
    let parents = items
        .iter()
        .map(|item| (item.id.as_str(), item.parent_id.as_deref()))
        .collect::<HashMap<_, _>>();
    let mut complete = HashSet::new();
    for item in items {
        let mut current = Some(item.id.as_str());
        let mut path = HashSet::new();
        while let Some(id) = current {
            if complete.contains(id) {
                break;
            }
            if !path.insert(id) {
                return invalid("document item hierarchy contains a cycle");
            }
            current = parents.get(id).copied().flatten();
        }
        complete.extend(path);
    }
    Ok(())
}

fn invalid<T>(message: &str) -> Result<T, HostDocumentError> {
    Err(HostDocumentError(message.into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn relationships_snapshot() -> HostRelationshipsV1 {
        HostRelationshipsV1 {
            contract: HOST_RELATIONSHIPS_CONTRACT_V1.into(),
            generation: 2,
            revision: 3,
            scope_id: "session_alpha".into(),
            relationships: vec![
                HostRelationship {
                    relationship_id: "root".into(),
                    kind: "example.delegation".into(),
                    terminal: None,
                    parent_id: None,
                    sibling_rank: 0,
                    lifecycle: HostRelationshipLifecycle::Active,
                    metadata: BTreeMap::from([(
                        "example.delegation.purpose".into(),
                        serde_json::json!("coordinate"),
                    )]),
                },
                HostRelationship {
                    relationship_id: "child".into(),
                    kind: "example.delegation".into(),
                    terminal: Some(HostTerminalRef {
                        terminal_id: "term_child".into(),
                        placement: None,
                    }),
                    parent_id: Some("root".into()),
                    sibling_rank: 0,
                    lifecycle: HostRelationshipLifecycle::Retained,
                    metadata: BTreeMap::new(),
                },
            ],
        }
    }

    #[test]
    fn relationships_require_namespaced_bounded_acyclic_trees() {
        relationships_snapshot().validate().unwrap();

        let mut cyclic = relationships_snapshot();
        cyclic.relationships[0].parent_id = Some("child".into());
        assert!(cyclic.validate().unwrap_err().to_string().contains("cycle"));

        let mut duplicate_rank = relationships_snapshot();
        duplicate_rank.relationships[1].parent_id = None;
        assert!(duplicate_rank
            .validate()
            .unwrap_err()
            .to_string()
            .contains("sibling ranks"));

        let mut unnamespaced = relationships_snapshot();
        unnamespaced.relationships[0].kind = "delegation".into();
        assert!(unnamespaced.validate().is_err());
    }

    #[test]
    fn relationships_are_bounded_by_extension_transport_capacity() {
        let mut snapshot = relationships_snapshot();
        snapshot.relationships = (0..64)
            .map(|index| HostRelationship {
                relationship_id: format!("r{index}"),
                kind: "vqro.delegation".into(),
                terminal: None,
                parent_id: None,
                sibling_rank: index,
                lifecycle: HostRelationshipLifecycle::Active,
                metadata: BTreeMap::from([(
                    "vqro.delegation.payload".into(),
                    serde_json::Value::String("x".repeat(MAX_DOCUMENT_TEXT_CHARS)),
                )]),
            })
            .collect();
        assert!(snapshot
            .validate()
            .unwrap_err()
            .to_string()
            .contains("transport capacity"));
    }

    #[test]
    fn agent_activity_is_bounded_by_extension_transport_capacity() {
        let snapshot = HostAgentActivityV1 {
            contract: HOST_AGENT_ACTIVITY_CONTRACT_V1.into(),
            generation: 1,
            revision: 1,
            scope_id: "session".into(),
            terminals: None,
            activities: (0..MAX_AGENT_ACTIVITIES)
                .map(|index| HostAgentActivityRecord {
                    terminal: HostTerminalRef {
                        terminal_id: format!("term_{index}"),
                        placement: None,
                    },
                    activity: HostAgentActivityState::Working,
                    source: "x".repeat(MAX_DOCUMENT_TEXT_CHARS),
                    evidence_kind: "hook_state".into(),
                    state_change_seq: index as u64,
                    labels: BTreeMap::new(),
                })
                .collect(),
        };
        assert!(snapshot
            .validate()
            .unwrap_err()
            .to_string()
            .contains("transport capacity"));
    }

    #[test]
    fn agent_activity_has_unique_terminals_and_round_trippable_attribution() {
        let record = HostAgentActivityRecord {
            terminal: HostTerminalRef {
                terminal_id: "term_1".into(),
                placement: None,
            },
            activity: HostAgentActivityState::Working,
            source: "custom hook".into(),
            evidence_kind: "future/evidence".into(),
            state_change_seq: 17,
            labels: BTreeMap::from([("name".into(), "Builder".into())]),
        };
        let mut snapshot = HostAgentActivityV1 {
            contract: HOST_AGENT_ACTIVITY_CONTRACT_V1.into(),
            generation: 4,
            revision: 5,
            scope_id: "session_alpha".into(),
            activities: vec![record.clone()],
            terminals: None,
        };
        snapshot.validate().unwrap();
        let decoded: HostAgentActivityV1 =
            serde_json::from_value(serde_json::to_value(&snapshot).unwrap()).unwrap();
        assert_eq!(decoded.activities[0].source, "custom hook");
        assert_eq!(decoded.activities[0].evidence_kind, "future/evidence");

        snapshot.activities.push(record);
        assert!(snapshot
            .validate()
            .unwrap_err()
            .to_string()
            .contains("unique"));
    }

    fn spaces_snapshot() -> HostSpacesV1 {
        HostSpacesV1 {
            contract: "host.spaces.v1".into(),
            generation: 4,
            revision: 9,
            scope_id: "session_alpha".into(),
            repositories: vec![HostSpaceRepository {
                repository_id: "repo_1".into(),
                label: "vqro".into(),
                workspaces: vec![HostSpaceWorkspaceRef {
                    workspace_id: "workspace_main".into(),
                    label: "main".into(),
                }],
            }],
            standalone: vec![HostSpaceWorkspaceRef {
                workspace_id: "workspace_notes".into(),
                label: "Notes".into(),
            }],
            order: Vec::new(),
        }
    }

    #[test]
    fn spaces_snapshot_validates_session_scope_and_unique_workspace_refs() {
        spaces_snapshot().validate().unwrap();

        let mut duplicate = spaces_snapshot();
        duplicate.standalone[0].workspace_id = "workspace_main".into();
        assert!(duplicate
            .validate()
            .unwrap_err()
            .to_string()
            .contains("only once"));

        let mut unfenced = spaces_snapshot();
        unfenced.revision = 0;
        assert_eq!(
            unfenced.validate().unwrap_err().to_string(),
            "spaces generation and revision must be non-zero"
        );
    }

    #[test]
    fn spaces_snapshot_is_bounded_by_document_capacity() {
        let mut oversized = spaces_snapshot();
        oversized.repositories[0].workspaces = (0..MAX_SPACES_WORKSPACES)
            .map(|index| HostSpaceWorkspaceRef {
                workspace_id: format!("workspace_{index}"),
                label: format!("Workspace {index}"),
            })
            .collect();
        oversized.standalone.clear();
        assert!(oversized
            .validate()
            .unwrap_err()
            .to_string()
            .contains("too many workspaces"));
    }

    fn view_item(id: &str, parent: Option<&str>) -> HostDocumentItem {
        HostDocumentItem {
            id: id.into(),
            parent_id: parent.map(str::to_owned),
            role: HostItemRole::Section,
            label: "row".into(),
            detail: None,
            terminal: None,
            action_ids: Vec::new(),
            flags: HostDocumentItemFlags::default(),
            view: Some(HostViewItem {
                kind: parent.is_some().then(|| "row".to_owned()),
                pane_id: parent.is_none().then(|| "w1:p2".to_owned()),
                view_id: parent.is_none().then(|| "tasks".to_owned()),
                state: parent.is_some().then_some(HostViewState::Blocked),
                action: parent.is_some().then(|| "unblock".to_owned()),
                waiting: parent.is_some().then_some(HostViewWaiting::Them),
                since: parent.is_some().then_some(1_700_000_000),
                affordance: None,
            }),
        }
    }

    fn view_document() -> HostDocumentV1 {
        HostDocumentV1 {
            contract: HOST_VIEW_CONTRACT_V1.into(),
            namespace: "example.view".into(),
            generation: 7,
            revision: 2,
            surface: HostSurface::Navigation,
            target: HostTarget::Session,
            items: vec![
                view_item("panel:1", None),
                view_item("panel:1:row", Some("panel:1")),
            ],
            actions: Vec::new(),
            required_capabilities: Vec::new(),
        }
    }

    #[test]
    fn view_contract_accepts_every_v1_block() {
        let document = view_document();
        document.validate().unwrap();
        // A legacy document may not smuggle view blocks in.
        let mut legacy = document.clone();
        legacy.contract = HOST_DOCUMENT_CONTRACT_V1.into();
        assert!(legacy.validate().unwrap_err().0.contains("view contract"));
    }

    #[test]
    fn view_contract_needs_a_panel_root_or_a_parent_for_a_view_block() {
        let mut document = view_document();
        document.items[1].parent_id = None;
        document.items[1].view.as_mut().unwrap().pane_id = None;
        document.items[1].view.as_mut().unwrap().view_id = None;
        assert!(document
            .validate()
            .unwrap_err()
            .0
            .contains("panel root or a parent"));
        // A panel root names both its pane and its view.
        let mut document = view_document();
        document.items[0].view.as_mut().unwrap().view_id = None;
        assert!(document.validate().unwrap_err().0.contains("both its pane"));
    }

    #[test]
    fn view_action_text_is_bounded() {
        let mut document = view_document();
        document.items[1].view.as_mut().unwrap().action =
            Some("x".repeat(MAX_VIEW_ACTION_CHARS + 1));
        assert!(document
            .validate()
            .unwrap_err()
            .0
            .contains("invalid view action text"));
    }

    fn document() -> HostDocumentV1 {
        HostDocumentV1 {
            contract: HOST_DOCUMENT_CONTRACT_V1.into(),
            namespace: "vqro.collections".into(),
            generation: 7,
            revision: 2,
            surface: HostSurface::Pane,
            target: HostTarget::Tab {
                tab_id: "w1:t1".into(),
            },
            items: vec![
                HostDocumentItem {
                    id: "group:1".into(),
                    parent_id: None,
                    role: HostItemRole::Group,
                    label: "Review".into(),
                    detail: None,
                    terminal: None,
                    action_ids: Vec::new(),
                    flags: HostDocumentItemFlags {
                        expanded: true,
                        ..Default::default()
                    },
                    view: None,
                },
                HostDocumentItem {
                    id: "member:1".into(),
                    parent_id: Some("group:1".into()),
                    role: HostItemRole::Terminal,
                    label: "Agent".into(),
                    detail: None,
                    terminal: Some(HostTerminalRef {
                        terminal_id: "term_7".into(),
                        placement: Some(HostTerminalPlacement {
                            pane_id: "w1:p2".into(),
                            workspace_id: "w1".into(),
                            tab_id: "w1:t1".into(),
                        }),
                    }),
                    action_ids: vec!["select:1".into()],
                    flags: HostDocumentItemFlags::default(),
                    view: None,
                },
            ],
            actions: vec![HostDocumentAction {
                id: "select:1".into(),
                label: "Select".into(),
                operation: HostDocumentOperation::Extension {
                    name: "member.select".into(),
                },
                subject_id: Some("member:1".into()),
                confirmation: HostConfirmation::None,
            }],
            required_capabilities: Vec::new(),
        }
    }

    fn host_snapshot() -> HostTerminalGroupsV1 {
        HostTerminalGroupsV1 {
            contract: "host.terminal-groups.v1".into(),
            generation: 7,
            revision: 2,
            scope_id: "w1:t1".into(),
            groups: vec![HostTerminalGroup {
                group_id: "collection_1".into(),
                label: Some("Review".into()),
                selected_terminal_id: Some("term_7".into()),
                members: vec![HostTerminalGroupMember {
                    terminal: document().items[1].terminal.clone().unwrap(),
                    label: "Agent".into(),
                    lifecycle: HostTerminalLifecycle::Live,
                }],
            }],
            ungrouped: Vec::new(),
        }
    }

    #[test]
    fn collection_document_uses_semantics_and_terminal_refs_without_geometry() {
        let document = document();
        document.validate().unwrap();
        let encoded = serde_json::to_value(document).unwrap();
        assert_eq!(encoded["items"][1]["terminal"]["terminal_id"], "term_7");
        assert_eq!(
            encoded["items"][1]["terminal"]["placement"]["pane_id"],
            "w1:p2"
        );
        for forbidden in ["x", "y", "width", "height", "color", "focus", "input"] {
            assert!(encoded["items"][1].get(forbidden).is_none());
        }
    }

    #[test]
    fn terminal_identity_survives_placement_changes() {
        let mut document = document();
        let stable_id = document.items[1]
            .terminal
            .as_ref()
            .unwrap()
            .terminal_id
            .clone();
        document.items[1].terminal.as_mut().unwrap().placement = Some(HostTerminalPlacement {
            pane_id: "w2:p9".into(),
            workspace_id: "w2".into(),
            tab_id: "w2:t4".into(),
        });

        document.validate().unwrap();
        let terminal = document.items[1].terminal.as_ref().unwrap();
        assert_eq!(terminal.terminal_id, stable_id);
        assert_eq!(terminal.placement.as_ref().unwrap().tab_id, "w2:t4");
    }

    #[test]
    fn document_binds_extension_operations_to_its_provider_namespace() {
        let document = document();
        let HostDocumentOperation::Extension { name } = &document.actions[0].operation else {
            panic!("extension operation")
        };
        assert_eq!(name, "member.select");
        assert_eq!(document.namespace, "vqro.collections");
        assert!(serde_json::to_string(&document.actions[0])
            .unwrap()
            .contains("member.select"));
        assert!(!serde_json::to_string(&document.actions[0])
            .unwrap()
            .contains("vqro.spaces"));
    }

    #[test]
    fn host_effects_require_an_explicit_allowlisted_capability() {
        let mut document = document();
        document.actions[0].operation = HostDocumentOperation::Host {
            effect: HostEffect::FocusTerminal {
                terminal_id: "term_7".into(),
            },
        };
        assert_eq!(
            document.validate().unwrap_err().to_string(),
            "host effect capability was not declared"
        );
        document
            .required_capabilities
            .push(HostCapability::FocusTerminal);
        document.validate().unwrap();

        document.required_capabilities = vec![HostCapability::TerminalInterrupt];
        document.actions[0].operation = HostDocumentOperation::Host {
            effect: HostEffect::TerminalInterrupt {
                terminal_id: "term_7".into(),
            },
        };
        assert_eq!(
            document.validate().unwrap_err().to_string(),
            "terminal interrupt requires confirmation"
        );
        document.actions[0].confirmation = HostConfirmation::Required;
        document.validate().unwrap();
        let effect = match &document.actions[0].operation {
            HostDocumentOperation::Host { effect } => effect,
            HostDocumentOperation::Extension { .. } => unreachable!(),
        };
        assert!(effect.authorize_dispatch(&HashSet::new(), true).is_err());
        let grants = HashSet::from([HostCapability::TerminalInterrupt]);
        assert!(effect.authorize_dispatch(&grants, false).is_err());
        effect.authorize_dispatch(&grants, true).unwrap();
        document
            .authorize_host_effect_dispatch("select:1", &host_snapshot(), &grants, true)
            .unwrap();
        let mut disabled = document.clone();
        let subject_id = disabled.actions[0].subject_id.clone().unwrap();
        disabled
            .items
            .iter_mut()
            .find(|item| item.id == subject_id)
            .unwrap()
            .flags
            .disabled = true;
        assert!(disabled
            .authorize_host_effect_dispatch("select:1", &host_snapshot(), &grants, true)
            .unwrap_err()
            .to_string()
            .contains("disabled"));
        let mut hidden = document.clone();
        hidden
            .items
            .iter_mut()
            .find(|item| item.id == subject_id)
            .unwrap()
            .action_ids
            .clear();
        assert!(hidden
            .authorize_host_effect_dispatch("select:1", &host_snapshot(), &grants, true)
            .unwrap_err()
            .to_string()
            .contains("reachable"));

        let mut escaped = document.clone();
        escaped.required_capabilities = vec![HostCapability::FocusTerminal];
        escaped.actions[0].operation = HostDocumentOperation::Host {
            effect: HostEffect::FocusTerminal {
                terminal_id: "term_outside".into(),
            },
        };
        assert!(escaped
            .authorize_host_effect_dispatch(
                "select:1",
                &host_snapshot(),
                &HashSet::from([HostCapability::FocusTerminal]),
                true,
            )
            .unwrap_err()
            .to_string()
            .contains("outside"));
    }

    #[test]
    fn document_rejects_unknown_actions_and_hierarchy_cycles() {
        let mut disabled = document();
        let subject_id = disabled.actions[0].subject_id.clone().unwrap();
        disabled
            .items
            .iter_mut()
            .find(|item| item.id == subject_id)
            .unwrap()
            .flags
            .disabled = true;
        assert!(disabled
            .authorize_extension_action_dispatch("select:1", &host_snapshot())
            .unwrap_err()
            .to_string()
            .contains("disabled"));
        disabled.actions[0].subject_id = None;
        assert!(disabled
            .authorize_extension_action_dispatch("select:1", &host_snapshot())
            .unwrap_err()
            .to_string()
            .contains("enabled item"));

        let mut unknown = document();
        unknown.items[1].action_ids = vec!["missing".into()];
        assert!(unknown.validate().is_err());

        let mut crossed = document();
        crossed.items[0].action_ids = vec![crossed.actions[0].id.clone()];
        assert!(crossed
            .validate()
            .unwrap_err()
            .to_string()
            .contains("another subject"));

        let mut cyclic = document();
        cyclic.items[0].parent_id = Some("member:1".into());
        assert!(cyclic.validate().is_err());
    }

    #[test]
    fn generation_and_revision_are_required_for_fencing() {
        let mut stale = document();
        stale.generation = 0;
        assert_eq!(
            stale.validate().unwrap_err().to_string(),
            "generation and revision must be non-zero"
        );
    }
}
