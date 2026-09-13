//! Minimal host-owned envelope for extension-contributed pane views.
//!
//! Contributions describe bounded content and declared actions. They never receive a renderer,
//! terminal buffer, or arbitrary input surface.

use serde::{Deserialize, Serialize};

use crate::view_document::{ViewAction, ViewDocument, ViewDocumentError};

/// Numeric keyboard activation is intentionally limited to one visible digit row.
pub const MAX_CONTRIBUTION_ACTIONS: usize = 9;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PaneContribution {
    pub owner_service_id: String,
    pub target: PaneContributionTarget,
    pub placement: PaneContributionPlacement,
    pub revision: u64,
    pub document: ViewDocument,
    #[serde(default)]
    pub actions: Vec<ViewAction>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PaneContributionTarget {
    Pane { pane_id: String },
}

impl PaneContributionTarget {
    pub fn pane_id(&self) -> &str {
        match self {
            Self::Pane { pane_id } => pane_id,
        }
    }
}

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum PaneContributionPlacement {
    #[default]
    Accessory,
    Replacement,
}

impl PaneContribution {
    pub fn validate(&self) -> Result<(), ViewDocumentError> {
        validate_id("owner service id", &self.owner_service_id)?;
        validate_id("target pane id", self.target.pane_id())?;
        if self.revision == 0 {
            return Err(ViewDocumentError(
                "contribution revision must be positive".into(),
            ));
        }
        self.document.validate()?;
        if self.actions.len() > MAX_CONTRIBUTION_ACTIONS {
            return Err(ViewDocumentError("too many contribution actions".into()));
        }
        let mut ids = std::collections::HashSet::new();
        for action in &self.actions {
            validate_id("action id", &action.id)?;
            validate_text("action label", &action.label)?;
            if !ids.insert(action.id.as_str()) {
                return Err(ViewDocumentError("duplicate action id".into()));
            }
        }
        Ok(())
    }
}

fn validate_id(label: &str, value: &str) -> Result<(), ViewDocumentError> {
    if value.is_empty()
        || value.len() > 120
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':'))
    {
        return Err(ViewDocumentError(format!("invalid {label}")));
    }
    Ok(())
}

fn validate_text(label: &str, value: &str) -> Result<(), ViewDocumentError> {
    if value.chars().count() > crate::view_document::MAX_TEXT_CHARS {
        return Err(ViewDocumentError(format!(
            "{label} exceeds character limit"
        )));
    }
    if value
        .chars()
        .any(|character| character.is_control() && !matches!(character, '\n' | '\t'))
    {
        return Err(ViewDocumentError(format!(
            "{label} contains a control character"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contribution(placement: PaneContributionPlacement) -> PaneContribution {
        PaneContribution {
            owner_service_id: "vqro.terminal-inspector.telemetry".into(),
            target: PaneContributionTarget::Pane {
                pane_id: "w1:p2".into(),
            },
            placement,
            revision: 1,
            document: ViewDocument {
                title: "Terminal intelligence".into(),
                subtitle: None,
                sections: Vec::new(),
            },
            actions: vec![ViewAction {
                id: "terminal.interrupt".into(),
                label: "Interrupt process".into(),
                destructive: true,
            }],
        }
    }

    #[test]
    fn validates_accessory_and_replacement_envelopes() {
        contribution(PaneContributionPlacement::Accessory)
            .validate()
            .unwrap();
        contribution(PaneContributionPlacement::Replacement)
            .validate()
            .unwrap();
    }

    #[test]
    fn rejects_unversioned_or_duplicate_action_envelopes() {
        let mut value = contribution(PaneContributionPlacement::Accessory);
        value.revision = 0;
        assert!(value.validate().is_err());
        value.revision = 1;
        value.actions.push(value.actions[0].clone());
        assert!(value.validate().unwrap_err().0.contains("duplicate"));
    }
}
