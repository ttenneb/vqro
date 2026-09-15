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
    pub pane_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view_id: Option<String>,
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
    pub operation: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_id: Option<String>,
    #[serde(default)]
    pub confirmation: HostConfirmation,
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
                validate_stable_ref(&terminal.pane_id)?;
                if let Some(view_id) = &terminal.view_id {
                    validate_local_id(view_id)?;
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

        let mut action_ids = HashSet::with_capacity(self.actions.len());
        for action in &self.actions {
            validate_local_id(&action.id)?;
            validate_text(&action.label)?;
            validate_namespaced_operation(&action.operation)?;
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

fn validate_namespaced_operation(value: &str) -> Result<(), HostDocumentError> {
    validate_namespace(value)
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
                        pane_id: "w1:p2".into(),
                        view_id: None,
                    }),
                    action_ids: vec!["select:1".into()],
                    flags: HostDocumentItemFlags::default(),
                },
            ],
            actions: vec![HostDocumentAction {
                id: "select:1".into(),
                label: "Select".into(),
                operation: "vqro.collections.select".into(),
                subject_id: Some("member:1".into()),
                confirmation: HostConfirmation::None,
            }],
        }
    }

    #[test]
    fn collection_document_uses_semantics_and_terminal_refs_without_geometry() {
        let document = document();
        document.validate().unwrap();
        let encoded = serde_json::to_value(document).unwrap();
        assert_eq!(encoded["items"][1]["terminal"]["pane_id"], "w1:p2");
        for forbidden in ["x", "y", "width", "height", "color", "focus", "input"] {
            assert!(encoded["items"][1].get(forbidden).is_none());
        }
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
