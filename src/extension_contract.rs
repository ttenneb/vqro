//! Public, versioned declarative contracts shared by bundled and third-party extensions.
//!
//! These values contain semantics and stable references only. The host remains
//! responsible for geometry, styling, focus, input, accessibility, PTYs, and
//! effect execution.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;

pub const HOST_DOCUMENT_CONTRACT_V1: &str = "host.document.v1";
pub const MAX_DOCUMENT_ITEMS: usize = 512;
pub const MAX_DOCUMENT_ACTIONS: usize = 256;
pub const MAX_DOCUMENT_TEXT_CHARS: usize = 4096;

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
    FocusWorkspace,
    OpenExternalUrl,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HostEffect {
    FocusTerminal { terminal_id: String },
    FocusWorkspace { workspace_id: String },
    OpenExternalUrl { url: String },
}

impl HostEffect {
    fn required_capability(&self) -> HostCapability {
        match self {
            Self::FocusTerminal { .. } => HostCapability::FocusTerminal,
            Self::FocusWorkspace { .. } => HostCapability::FocusWorkspace,
            Self::OpenExternalUrl { .. } => HostCapability::OpenExternalUrl,
        }
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

impl HostDocumentV1 {
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
                validate_stable_ref(&terminal.terminal_id)?;
                if let Some(placement) = &terminal.placement {
                    validate_stable_ref(&placement.pane_id)?;
                    validate_stable_ref(&placement.workspace_id)?;
                    validate_stable_ref(&placement.tab_id)?;
                }
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
                        HostEffect::FocusWorkspace { workspace_id } => {
                            validate_stable_ref(workspace_id)?
                        }
                        HostEffect::OpenExternalUrl { url }
                            if url.starts_with("https://") || url.starts_with("http://") => {}
                        HostEffect::OpenExternalUrl { .. } => {
                            return invalid("external URL effect must use HTTP or HTTPS")
                        }
                    }
                }
            }
            if !action_ids.insert(action.id.as_str()) {
                return invalid("document action ids must be unique");
            }
        }
        for item in &self.items {
            if item
                .action_ids
                .iter()
                .any(|action| !action_ids.contains(action.as_str()))
            {
                return invalid("document item references an unknown action");
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
    }

    #[test]
    fn document_rejects_unknown_actions_and_hierarchy_cycles() {
        let mut unknown = document();
        unknown.items[1].action_ids = vec!["missing".into()];
        assert!(unknown.validate().is_err());

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
