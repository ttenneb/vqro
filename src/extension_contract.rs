//! Public, versioned declarative contracts shared by bundled and third-party extensions.
//!
//! These values contain semantics and stable references only. The host remains
//! responsible for geometry, styling, focus, input, accessibility, PTYs, and
//! effect execution.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;

pub const HOST_DOCUMENT_CONTRACT_V1: &str = "host.document.v1";
pub const MAX_DOCUMENT_PROVIDERS: usize = 16;
pub const MAX_DOCUMENT_ITEMS: usize = 512;
pub const MAX_DOCUMENT_ACTIONS: usize = 256;
pub const MAX_DOCUMENT_TEXT_CHARS: usize = 4096;

/// A host-owned terminal-group projection supplied to document providers.
/// It contains stable identities and semantic state, never PTYs or geometry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostTerminalGroupsV1 {
    pub contract: String,
    pub generation: u64,
    pub revision: u64,
    pub scope_id: String,
    pub groups: Vec<HostTerminalGroup>,
    #[serde(default)]
    pub ungrouped: Vec<HostTerminalGroupMember>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostTerminalGroup {
    pub group_id: String,
    pub label: Option<String>,
    pub members: Vec<HostTerminalGroupMember>,
    pub selected_terminal_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostTerminalGroupMember {
    pub terminal: HostTerminalRef,
    pub label: String,
    pub lifecycle: HostTerminalLifecycle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostTerminalLifecycle {
    Live,
    Exited,
    Archived,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostSurface {
    Navigation,
    Pane,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HostTarget {
    Session,
    Workspace { workspace_id: String },
    Tab { tab_id: String },
    Pane { pane_id: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostItemRole {
    Group,
    Terminal,
    Section,
    Command,
    Status,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostTerminalRef {
    /// Stable runtime identity. It does not change when a pane moves, is
    /// promoted from a group, or is shown by another client.
    pub terminal_id: String,
    /// Optional, ephemeral placement context for resolving host interactions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placement: Option<HostTerminalPlacement>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostTerminalPlacement {
    pub pane_id: String,
    pub workspace_id: String,
    pub tab_id: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostDocumentAction {
    pub id: String,
    pub label: String,
    pub operation: HostDocumentOperation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_id: Option<String>,
    #[serde(default)]
    pub confirmation: HostConfirmation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HostDocumentOperation {
    /// Routed only to the provider identified by the enclosing document's
    /// namespace and generation. `name` is never resolved globally.
    Extension { name: String },
    /// A fixed host-owned effect. The provider must hold the corresponding
    /// capability and the host still validates the referenced identity.
    Host { effect: HostEffect },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostCapability {
    FocusTerminal,
    TerminalInterrupt,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
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
        if self.contract != HOST_DOCUMENT_CONTRACT_V1 {
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
    for item in items {
        let mut current = item.parent_id.as_deref();
        let mut visited = HashSet::new();
        while let Some(id) = current {
            if !visited.insert(id) {
                return invalid("document item hierarchy contains a cycle");
            }
            current = items
                .iter()
                .find(|candidate| candidate.id == id)
                .and_then(|candidate| candidate.parent_id.as_deref());
        }
    }
    Ok(())
}

fn invalid<T>(message: &str) -> Result<T, HostDocumentError> {
    Err(HostDocumentError(message.into()))
}

#[cfg(test)]
mod tests {
    use super::*;

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
