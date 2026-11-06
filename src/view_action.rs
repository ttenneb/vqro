//! Neutral, bounded view-action preparation. Package subjects stay opaque.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PrepareParams {
    pub namespace: String,
    pub scope_id: String,
    pub pane_id: String,
    pub consumer_pane_id: String,
    pub terminal_scope: Vec<String>,
    pub generation: u64,
    pub revision: u64,
    pub item_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub option: Option<String>,
    pub operation_id: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SubmitParams {
    pub token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirmation_token: Option<String>,
    #[serde(default)]
    pub cancel: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Choice {
    pub key: String,
    pub label: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Affordance {
    Action {
        label: String,
        detail: String,
        subject: Value,
    },
    Choice {
        label: String,
        detail: String,
        subject: Value,
        options: Vec<Choice>,
        default: String,
        reversible: bool,
    },
}
pub(crate) fn token() -> Result<String, String> {
    let mut bytes = [0u8; 16];
    crate::platform::fill_secure_random(&mut bytes)
        .map_err(|error| format!("action nonce unavailable: {error}"))?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}
fn text(value: &str, limit: usize) -> bool {
    !value.trim().is_empty() && value.len() <= limit && !value.chars().any(char::is_control)
}
impl Affordance {
    pub(crate) fn parse(value: &Value) -> Result<Self, String> {
        if value.to_string().len() > 8192 {
            return Err("view affordance exceeds bound".into());
        }
        let result: Self =
            serde_json::from_value(value.clone()).map_err(|_| "unsupported view affordance")?;
        let (label, detail, subject) = match &result {
            Self::Action {
                label,
                detail,
                subject,
            }
            | Self::Choice {
                label,
                detail,
                subject,
                ..
            } => (label, detail, subject),
        };
        if !text(label, 128)
            || !text(detail, 1024)
            || !subject.is_object()
            || subject.as_object().is_none_or(|value| value.is_empty())
        {
            return Err("invalid view action presentation or opaque subject".into());
        }
        if let Self::Choice {
            options, default, ..
        } = &result
        {
            let mut keys = std::collections::HashSet::new();
            if !(2..=4).contains(&options.len())
                || options.iter().any(|choice| {
                    !text(&choice.key, 128)
                        || !text(&choice.label, 256)
                        || !keys.insert(&choice.key)
                })
                || !options.iter().any(|choice| &choice.key == default)
            {
                return Err("invalid bounded view choices/default".into());
            }
        }
        Ok(result)
    }
    pub(crate) fn subject(&self) -> &Value {
        match self {
            Self::Action { subject, .. } | Self::Choice { subject, .. } => subject,
        }
    }
    pub(crate) fn select(&self, option: Option<&str>) -> Result<bool, String> {
        match self {
            Self::Action { .. } if option.is_none() => Ok(false),
            Self::Choice {
                options,
                reversible,
                ..
            } if option.is_some_and(|key| options.iter().any(|choice| choice.key == key)) => {
                Ok(!reversible)
            }
            _ => Err("select exactly one listed option; no implicit default".into()),
        }
    }
}
impl PrepareParams {
    pub(crate) fn validate(&self) -> Result<(), String> {
        if self.scope_id != "session"
            || self.generation == 0
            || self.revision == 0
            || [
                &self.namespace,
                &self.pane_id,
                &self.consumer_pane_id,
                &self.item_id,
            ]
            .iter()
            .any(|value| !text(value, 256))
            || !text(&self.operation_id, 128)
            || !self
                .operation_id
                .bytes()
                .all(|value| value.is_ascii_alphanumeric() || b"._:-".contains(&value))
        {
            return Err("invalid session view-action identity".into());
        }
        let unique = self
            .terminal_scope
            .iter()
            .collect::<std::collections::HashSet<_>>();
        if self.terminal_scope.is_empty()
            || self.terminal_scope.len() > crate::admission::MAX_SESSION_PANES
            || unique.len() != self.terminal_scope.len()
            || self.terminal_scope.iter().any(|pane| !text(pane, 256))
            || !self.terminal_scope.contains(&self.pane_id)
        {
            return Err("invalid expected full terminal scope".into());
        }
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Binding {
    pub selection: PrepareParams,
    pub service_id: String,
    pub service_generation: u64,
    pub consumer: crate::protocol::PaneSurfaceViewEntryV2,
    pub affordance: Affordance,
}
#[derive(Debug, Clone)]
pub(crate) struct Prepared {
    pub token: String,
    pub confirmation: Option<String>,
    pub expires: Instant,
    pub binding: Binding,
    pub identity: crate::client_invocation::ClientIdentity,
}
impl Prepared {
    pub(crate) fn new(
        binding: Binding,
        identity: crate::client_invocation::ClientIdentity,
    ) -> Result<Self, String> {
        let irreversible = binding
            .affordance
            .select(binding.selection.option.as_deref())?;
        Ok(Self {
            token: token()?,
            confirmation: if irreversible { Some(token()?) } else { None },
            expires: Instant::now() + Duration::from_secs(60),
            binding,
            identity,
        })
    }
    pub(crate) fn check(&self, request: &SubmitParams, now: Instant) -> Result<(), String> {
        if request.token != self.token || now >= self.expires {
            return Err("preparation expired or belongs to another connection".into());
        }
        if !request.cancel && self.confirmation != request.confirmation_token {
            return Err("exact prepared choice requires explicit confirmation".into());
        }
        Ok(())
    }
}
#[derive(Debug)]
pub(crate) struct ClientState {
    pub nonce: String,
    pub busy: Option<String>,
    pub prepared: Option<Prepared>,
    pub canceled: std::sync::Arc<std::sync::atomic::AtomicBool>,
}
impl Default for ClientState {
    fn default() -> Self {
        Self {
            nonce: token().unwrap_or_default(),
            busy: None,
            prepared: None,
            canceled: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }
}
impl Drop for ClientState {
    fn drop(&mut self) {
        self.canceled
            .store(true, std::sync::atomic::Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn choice() -> Value {
        json!({"kind":"choice","label":"Answer","detail":"Full question","subject":{"opaque":1},"options":[{"key":"go","label":"Go"},{"key":"wait","label":"Wait"}],"default":"wait","reversible":false})
    }
    #[test]
    fn view_affordance_never_applies_default_and_requires_listed_choice() {
        let affordance = Affordance::parse(&choice()).unwrap();
        assert!(affordance.select(None).is_err());
        assert!(affordance.select(Some("other")).is_err());
        assert_eq!(affordance.select(Some("go")), Ok(true));
    }
    #[test]
    fn view_affordance_unsupported_malformed_and_overflow_stay_read_only() {
        for value in [
            json!({"kind":"future"}),
            json!({"kind":"action","label":"Accept","detail":"Question","subject":null}),
            {
                let mut value = choice();
                value["options"][1]["key"] = json!("go");
                value
            },
            {
                let mut value = choice();
                value["detail"] = json!("x".repeat(1025));
                value
            },
        ] {
            assert!(Affordance::parse(&value).is_err());
        }
    }
    #[test]
    fn view_preparation_binds_confirmation_expiry_and_exact_opaque_subject() {
        let selection = PrepareParams {
            namespace: "example".into(),
            scope_id: "session".into(),
            pane_id: "w1:p1".into(),
            consumer_pane_id: "w1:p2".into(),
            terminal_scope: vec!["w1:p1".into()],
            generation: 1,
            revision: 2,
            item_id: "row".into(),
            option: Some("go".into()),
            operation_id: "original".into(),
        };
        let binding = Binding {
            selection,
            service_id: "example.actions".into(),
            service_generation: 3,
            consumer: crate::protocol::PaneSurfaceViewEntryV2 {
                pane_id: "w1:p2".into(),
                extension: "example".into(),
                view: "work".into(),
                scope: vec!["w1:p1".into()],
            },
            affordance: Affordance::parse(&choice()).unwrap(),
        };
        let identity = crate::client_invocation::ClientIdentity {
            boot_id: "boot".into(),
            pid: 3,
            birth: 4,
            tty: crate::client_invocation::ClientTtyDescriptor {
                kind: "unix".into(),
                device: "1:2:3".into(),
            },
        };
        let prepared = Prepared::new(binding, identity).unwrap();
        let mut submit = SubmitParams {
            token: prepared.token.clone(),
            confirmation_token: None,
            cancel: false,
        };
        assert!(prepared.check(&submit, Instant::now()).is_err());
        submit.confirmation_token = prepared.confirmation.clone();
        assert!(prepared.check(&submit, Instant::now()).is_ok());
        assert!(prepared.check(&submit, prepared.expires).is_err());
        submit.token = "other-client".into();
        assert!(prepared.check(&submit, Instant::now()).is_err());
        assert_eq!(prepared.binding.affordance.subject(), &json!({"opaque":1}));
    }
}
