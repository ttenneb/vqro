//! Typed dynamic action choices produced by an already-supervised extension service.
//!
//! The host sends only an opaque validated choice id back to the provider. Choice discovery is
//! never a render-time RPC and does not authorize host effects.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

pub const ACTION_CHOICES_CONTRACT_V1: &str = "host.action-choices.v1";
pub const ACTION_CHOICES_LIST_METHOD_V1: &str = "host.action.choices.v1";
pub const ACTION_CHOICE_INVOKE_METHOD_V1: &str = "host.action.choice.invoke.v1";
pub const MAX_ACTION_CHOICES: usize = 128;
pub const MAX_CHOICE_TEXT_BYTES: usize = 512;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ActionChoiceKindV1 {
    Command,
    Boolean,
    EnumValue,
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ActionChoiceV1 {
    pub choice_id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub kind: ActionChoiceKindV1,
    #[serde(default)]
    pub selected: bool,
    #[serde(default)]
    pub disabled: bool,
    #[serde(default)]
    pub confirmation_required: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ActionChoicesRequestV1 {
    pub contract: String,
    pub request_id: String,
    pub action_id: String,
    pub provider_generation: u64,
    pub context_revision: u64,
    pub context: serde_json::Value,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ActionChoiceSetV1 {
    pub contract: String,
    pub request_id: String,
    pub action_id: String,
    pub choice_set_id: String,
    pub revision: u64,
    pub provider_generation: u64,
    pub context_revision: u64,
    pub choices: Vec<ActionChoiceV1>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ActionChoiceInvocationV1 {
    pub contract: String,
    pub request_id: String,
    pub action_id: String,
    pub choice_set_id: String,
    pub choice_revision: u64,
    pub choice_id: String,
    pub provider_generation: u64,
    pub context_revision: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ActionChoiceError {
    InvalidContract,
    InvalidIdentifier,
    InvalidText,
    DuplicateChoice,
    EmptyChoices,
    LimitExceeded,
    RequestMismatch,
    ActionMismatch,
    ContextRevisionMismatch,
    StaleChoiceSet,
    ChoiceUnavailable,
    ConfirmationRequired,
}

impl ActionChoicesRequestV1 {
    pub fn validate(&self) -> Result<(), ActionChoiceError> {
        validate_contract(&self.contract)?;
        for identifier in [&self.request_id, &self.action_id] {
            validate_identifier(identifier)?;
        }
        if self.provider_generation == 0 {
            return Err(ActionChoiceError::StaleChoiceSet);
        }
        Ok(())
    }
}

impl ActionChoiceSetV1 {
    pub fn validate_for(&self, request: &ActionChoicesRequestV1) -> Result<(), ActionChoiceError> {
        request.validate()?;
        validate_contract(&self.contract)?;
        if self.request_id != request.request_id {
            return Err(ActionChoiceError::RequestMismatch);
        }
        if self.action_id != request.action_id {
            return Err(ActionChoiceError::ActionMismatch);
        }
        if self.provider_generation != request.provider_generation {
            return Err(ActionChoiceError::StaleChoiceSet);
        }
        if self.context_revision != request.context_revision {
            return Err(ActionChoiceError::ContextRevisionMismatch);
        }
        validate_identifier(&self.choice_set_id)?;
        if self.revision == 0 || self.choices.is_empty() {
            return Err(ActionChoiceError::EmptyChoices);
        }
        if self.choices.len() > MAX_ACTION_CHOICES {
            return Err(ActionChoiceError::LimitExceeded);
        }
        let mut ids = BTreeSet::new();
        for choice in &self.choices {
            validate_identifier(&choice.choice_id)?;
            validate_text(&choice.label)?;
            if let Some(description) = &choice.description {
                validate_text(description)?;
            }
            if !ids.insert(&choice.choice_id) {
                return Err(ActionChoiceError::DuplicateChoice);
            }
        }
        Ok(())
    }

    pub fn validate_invocation(
        &self,
        invocation: &ActionChoiceInvocationV1,
        confirmed_by_host: bool,
    ) -> Result<&ActionChoiceV1, ActionChoiceError> {
        validate_contract(&invocation.contract)?;
        for identifier in [
            &invocation.request_id,
            &invocation.action_id,
            &invocation.choice_set_id,
            &invocation.choice_id,
        ] {
            validate_identifier(identifier)?;
        }
        if invocation.action_id != self.action_id {
            return Err(ActionChoiceError::ActionMismatch);
        }
        if invocation.choice_set_id != self.choice_set_id
            || invocation.choice_revision != self.revision
            || invocation.provider_generation != self.provider_generation
        {
            return Err(ActionChoiceError::StaleChoiceSet);
        }
        if invocation.context_revision != self.context_revision {
            return Err(ActionChoiceError::ContextRevisionMismatch);
        }
        let choice = self
            .choices
            .iter()
            .find(|choice| choice.choice_id == invocation.choice_id)
            .ok_or(ActionChoiceError::ChoiceUnavailable)?;
        if choice.disabled {
            return Err(ActionChoiceError::ChoiceUnavailable);
        }
        if choice.confirmation_required && !confirmed_by_host {
            return Err(ActionChoiceError::ConfirmationRequired);
        }
        Ok(choice)
    }
}

fn validate_contract(contract: &str) -> Result<(), ActionChoiceError> {
    if contract != ACTION_CHOICES_CONTRACT_V1 {
        return Err(ActionChoiceError::InvalidContract);
    }
    Ok(())
}

fn validate_identifier(identifier: &str) -> Result<(), ActionChoiceError> {
    if identifier.is_empty()
        || identifier.len() > 128
        || !identifier
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':'))
    {
        return Err(ActionChoiceError::InvalidIdentifier);
    }
    Ok(())
}

fn validate_text(text: &str) -> Result<(), ActionChoiceError> {
    if text.trim().is_empty()
        || text.len() > MAX_CHOICE_TEXT_BYTES
        || text.chars().any(char::is_control)
    {
        return Err(ActionChoiceError::InvalidText);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> ActionChoicesRequestV1 {
        ActionChoicesRequestV1 {
            contract: ACTION_CHOICES_CONTRACT_V1.into(),
            request_id: "request-1".into(),
            action_id: "set-mode".into(),
            provider_generation: 9,
            context_revision: 4,
            context: serde_json::json!({"terminal_id": "term_1"}),
        }
    }

    fn choices() -> ActionChoiceSetV1 {
        ActionChoiceSetV1 {
            contract: ACTION_CHOICES_CONTRACT_V1.into(),
            request_id: "request-1".into(),
            action_id: "set-mode".into(),
            choice_set_id: "choices-1".into(),
            revision: 2,
            provider_generation: 9,
            context_revision: 4,
            choices: vec![ActionChoiceV1 {
                choice_id: "fast".into(),
                label: "Fast".into(),
                description: None,
                kind: ActionChoiceKindV1::EnumValue,
                selected: false,
                disabled: false,
                confirmation_required: false,
            }],
        }
    }

    #[test]
    fn choice_sets_are_request_and_context_fenced() {
        let mut mismatched = choices();
        mismatched.context_revision = 5;
        assert_eq!(
            mismatched.validate_for(&request()),
            Err(ActionChoiceError::ContextRevisionMismatch)
        );
        choices().validate_for(&request()).unwrap();
    }

    #[test]
    fn invocation_uses_only_an_emitted_choice_identity() {
        let set = choices();
        let invocation = ActionChoiceInvocationV1 {
            contract: ACTION_CHOICES_CONTRACT_V1.into(),
            request_id: "invoke-1".into(),
            action_id: "set-mode".into(),
            choice_set_id: "choices-1".into(),
            choice_revision: 2,
            choice_id: "fast".into(),
            provider_generation: 9,
            context_revision: 4,
        };
        assert_eq!(
            set.validate_invocation(&invocation, false)
                .unwrap()
                .choice_id,
            "fast"
        );
    }
}
