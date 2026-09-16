//! Versioned, namespaced extension state with atomic compare-and-swap transactions.
//!
//! The host persists this document as one unit. Values are opaque JSON; namespaces are an
//! ownership boundary, not a security boundary between same-user code.

use std::collections::{BTreeMap, VecDeque};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

pub const EXTENSION_STATE_CONTRACT_V1: &str = "host.extension-state.v1";
pub const MAX_STATE_NAMESPACES: usize = 64;
pub const MAX_STATE_KEYS_PER_NAMESPACE: usize = 512;
pub const MAX_STATE_VALUE_BYTES: usize = 32 * 1024;
pub const MAX_STATE_NAMESPACE_BYTES: usize = 128 * 1024;
pub const MAX_STATE_OPERATIONS: usize = 128;
pub const MAX_STATE_EVENTS: usize = 256;
pub const MAX_STATE_RECEIPTS: usize = 64;
pub const MAX_STATE_STORE_BYTES: u64 = 24 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ExtensionStateOperationV1 {
    pub key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ExtensionStateTransactionV1 {
    pub contract: String,
    pub namespace: String,
    pub expected_store_generation: u64,
    pub expected_revision: u64,
    pub idempotency_key: String,
    pub operations: Vec<ExtensionStateOperationV1>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ExtensionStateSnapshotV1 {
    pub contract: String,
    pub store_id: String,
    pub store_generation: u64,
    pub namespace: String,
    pub revision: u64,
    pub sequence: u64,
    pub values: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ExtensionStateEventV1 {
    pub sequence: u64,
    pub namespace: String,
    pub revision: u64,
    pub changed_keys: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ExtensionStateEventPageV1 {
    pub contract: String,
    pub store_id: String,
    pub store_generation: u64,
    pub earliest_sequence: u64,
    pub latest_sequence: u64,
    pub gap: bool,
    pub events: Vec<ExtensionStateEventV1>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ExtensionStateCommitV1 {
    pub contract: String,
    pub store_id: String,
    pub store_generation: u64,
    pub namespace: String,
    pub revision: u64,
    pub sequence: u64,
    pub replayed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct NamespaceState {
    revision: u64,
    values: BTreeMap<String, Value>,
    receipts: VecDeque<Receipt>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Receipt {
    key: String,
    digest: String,
    revision: u64,
    sequence: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DurableExtensionStateV1 {
    pub contract: String,
    pub store_id: String,
    pub store_generation: u64,
    pub sequence: u64,
    namespaces: BTreeMap<String, NamespaceState>,
    events: VecDeque<ExtensionStateEventV1>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExtensionStateError {
    InvalidContract,
    InvalidNamespace,
    InvalidKey,
    InvalidIdempotencyKey,
    StoreGenerationConflict { actual: u64 },
    RevisionConflict { actual: u64 },
    IdempotencyConflict,
    EmptyNamespaceTransaction,
    LimitExceeded(&'static str),
    Serialization,
}

impl DurableExtensionStateV1 {
    pub fn new(store_id: String, store_generation: u64) -> Result<Self, ExtensionStateError> {
        if !valid_identifier(&store_id, 128) || store_generation == 0 {
            return Err(ExtensionStateError::InvalidNamespace);
        }
        Ok(Self {
            contract: EXTENSION_STATE_CONTRACT_V1.into(),
            store_id,
            store_generation,
            sequence: 0,
            namespaces: BTreeMap::new(),
            events: VecDeque::new(),
        })
    }

    pub fn snapshot(
        &self,
        namespace: &str,
    ) -> Result<ExtensionStateSnapshotV1, ExtensionStateError> {
        validate_namespace(namespace)?;
        let state = self.namespaces.get(namespace);
        Ok(ExtensionStateSnapshotV1 {
            contract: EXTENSION_STATE_CONTRACT_V1.into(),
            store_id: self.store_id.clone(),
            store_generation: self.store_generation,
            namespace: namespace.into(),
            revision: state.map_or(0, |state| state.revision),
            sequence: self.sequence,
            values: state.map_or_else(BTreeMap::new, |state| state.values.clone()),
        })
    }

    pub fn transact(
        &mut self,
        transaction: &ExtensionStateTransactionV1,
    ) -> Result<ExtensionStateCommitV1, ExtensionStateError> {
        validate_transaction(transaction)?;
        let digest = transaction_digest(transaction)?;
        if let Some(state) = self.namespaces.get(&transaction.namespace) {
            if let Some(receipt) = state
                .receipts
                .iter()
                .find(|receipt| receipt.key == transaction.idempotency_key)
            {
                if receipt.digest != digest {
                    return Err(ExtensionStateError::IdempotencyConflict);
                }
                return Ok(self.commit(
                    &transaction.namespace,
                    receipt.revision,
                    receipt.sequence,
                    true,
                ));
            }
            if transaction.expected_store_generation != self.store_generation {
                return Err(ExtensionStateError::StoreGenerationConflict {
                    actual: self.store_generation,
                });
            }
            if state.revision != transaction.expected_revision {
                return Err(ExtensionStateError::RevisionConflict {
                    actual: state.revision,
                });
            }
        } else {
            if transaction.expected_store_generation != self.store_generation {
                return Err(ExtensionStateError::StoreGenerationConflict {
                    actual: self.store_generation,
                });
            }
            if transaction.expected_revision != 0 {
                return Err(ExtensionStateError::RevisionConflict { actual: 0 });
            }
        }

        let mut candidate = self.namespaces.get(&transaction.namespace).map_or_else(
            || NamespaceState {
                revision: 0,
                values: BTreeMap::new(),
                receipts: VecDeque::new(),
            },
            Clone::clone,
        );
        let mut changed_keys = Vec::with_capacity(transaction.operations.len());
        for operation in &transaction.operations {
            match &operation.value {
                Some(value) => {
                    candidate
                        .values
                        .insert(operation.key.clone(), value.clone());
                }
                None => {
                    candidate.values.remove(&operation.key);
                }
            }
            changed_keys.push(operation.key.clone());
        }
        changed_keys.sort();
        changed_keys.dedup();
        validate_candidate(&candidate)?;
        let creates_namespace = !self.namespaces.contains_key(&transaction.namespace);
        if creates_namespace && candidate.values.is_empty() {
            return Err(ExtensionStateError::EmptyNamespaceTransaction);
        }
        let retired_namespace =
            if creates_namespace && self.namespaces.len() >= MAX_STATE_NAMESPACES {
                Some(
                    self.namespaces
                        .iter()
                        .filter(|(namespace, state)| {
                            state.values.is_empty()
                                && !self
                                    .events
                                    .iter()
                                    .any(|event| event.namespace.as_str() == namespace.as_str())
                        })
                        .min_by_key(|(_, state)| {
                            state.receipts.back().map_or(0, |receipt| receipt.sequence)
                        })
                        .map(|(namespace, _)| namespace.clone())
                        .ok_or(ExtensionStateError::LimitExceeded("namespaces"))?,
                )
            } else {
                None
            };
        let next_store_generation = retired_namespace
            .as_ref()
            .map(|_| {
                self.store_generation
                    .checked_add(1)
                    .ok_or(ExtensionStateError::LimitExceeded(
                        "store generation exhausted",
                    ))
            })
            .transpose()?;

        candidate.revision = candidate
            .revision
            .checked_add(1)
            .ok_or(ExtensionStateError::LimitExceeded("revision exhausted"))?;
        self.sequence = self
            .sequence
            .checked_add(1)
            .ok_or(ExtensionStateError::LimitExceeded("sequence exhausted"))?;
        candidate.receipts.push_back(Receipt {
            key: transaction.idempotency_key.clone(),
            digest,
            revision: candidate.revision,
            sequence: self.sequence,
        });
        while candidate.receipts.len() > MAX_STATE_RECEIPTS {
            candidate.receipts.pop_front();
        }
        let revision = candidate.revision;
        if let Some(retired_namespace) = retired_namespace {
            self.namespaces.remove(&retired_namespace);
            self.store_generation = next_store_generation.unwrap_or(self.store_generation);
        }
        self.namespaces
            .insert(transaction.namespace.clone(), candidate);
        self.events.push_back(ExtensionStateEventV1 {
            sequence: self.sequence,
            namespace: transaction.namespace.clone(),
            revision,
            changed_keys,
        });
        while self.events.len() > MAX_STATE_EVENTS {
            self.events.pop_front();
        }
        Ok(self.commit(&transaction.namespace, revision, self.sequence, false))
    }

    pub fn clear_namespace(
        &mut self,
        namespace: &str,
        idempotency_key: String,
    ) -> Result<Option<ExtensionStateCommitV1>, ExtensionStateError> {
        validate_namespace(namespace)?;
        let Some(state) = self.namespaces.get(namespace) else {
            return Ok(None);
        };
        let operations = state
            .values
            .keys()
            .cloned()
            .map(|key| ExtensionStateOperationV1 { key, value: None })
            .collect::<Vec<_>>();
        if operations.is_empty() {
            return Ok(None);
        }
        self.transact(&ExtensionStateTransactionV1 {
            contract: EXTENSION_STATE_CONTRACT_V1.into(),
            namespace: namespace.into(),
            expected_store_generation: self.store_generation,
            expected_revision: state.revision,
            idempotency_key,
            operations,
        })
        .map(Some)
    }

    pub fn events_after(&self, sequence: u64, limit: usize) -> ExtensionStateEventPageV1 {
        let earliest_sequence = self
            .events
            .front()
            .map_or(self.sequence, |event| event.sequence);
        let gap = sequence.saturating_add(1) < earliest_sequence;
        ExtensionStateEventPageV1 {
            contract: EXTENSION_STATE_CONTRACT_V1.into(),
            store_id: self.store_id.clone(),
            store_generation: self.store_generation,
            earliest_sequence,
            latest_sequence: self.sequence,
            gap,
            events: if gap {
                Vec::new()
            } else {
                self.events
                    .iter()
                    .filter(|event| event.sequence > sequence)
                    .take(limit.min(MAX_STATE_EVENTS))
                    .cloned()
                    .collect()
            },
        }
    }

    pub fn validate(&self) -> Result<(), ExtensionStateError> {
        if self.contract != EXTENSION_STATE_CONTRACT_V1
            || !valid_identifier(&self.store_id, 128)
            || self.store_generation == 0
            || self.namespaces.len() > MAX_STATE_NAMESPACES
            || self.events.len() > MAX_STATE_EVENTS
        {
            return Err(ExtensionStateError::InvalidContract);
        }
        for (namespace, state) in &self.namespaces {
            validate_namespace(namespace)?;
            validate_candidate(state)?;
            if state.receipts.len() > MAX_STATE_RECEIPTS {
                return Err(ExtensionStateError::LimitExceeded("receipts"));
            }
            for receipt in &state.receipts {
                if !valid_identifier(&receipt.key, 128)
                    || receipt.digest.len() != 64
                    || !receipt.digest.bytes().all(|byte| byte.is_ascii_hexdigit())
                    || receipt.revision == 0
                    || receipt.revision > state.revision
                    || receipt.sequence == 0
                    || receipt.sequence > self.sequence
                {
                    return Err(ExtensionStateError::InvalidContract);
                }
            }
        }
        if self.sequence == 0 {
            if !self.events.is_empty() {
                return Err(ExtensionStateError::InvalidContract);
            }
        } else if self.events.back().map(|event| event.sequence) != Some(self.sequence) {
            return Err(ExtensionStateError::InvalidContract);
        }
        let expected_first = self
            .sequence
            .saturating_sub(self.events.len().saturating_sub(1) as u64);
        let mut previous_sequence = expected_first.saturating_sub(1);
        for event in &self.events {
            let Some(namespace) = self.namespaces.get(&event.namespace) else {
                return Err(ExtensionStateError::InvalidNamespace);
            };
            if event.sequence != previous_sequence.saturating_add(1)
                || event.sequence > self.sequence
                || event.revision == 0
                || event.revision > namespace.revision
                || event.changed_keys.is_empty()
            {
                return Err(ExtensionStateError::InvalidContract);
            }
            for key in &event.changed_keys {
                if !valid_identifier(key, 128) {
                    return Err(ExtensionStateError::InvalidKey);
                }
            }
            previous_sequence = event.sequence;
        }
        Ok(())
    }

    fn commit(
        &self,
        namespace: &str,
        revision: u64,
        sequence: u64,
        replayed: bool,
    ) -> ExtensionStateCommitV1 {
        ExtensionStateCommitV1 {
            contract: EXTENSION_STATE_CONTRACT_V1.into(),
            store_id: self.store_id.clone(),
            store_generation: self.store_generation,
            namespace: namespace.into(),
            revision,
            sequence,
            replayed,
        }
    }
}

fn validate_transaction(
    transaction: &ExtensionStateTransactionV1,
) -> Result<(), ExtensionStateError> {
    if transaction.contract != EXTENSION_STATE_CONTRACT_V1 {
        return Err(ExtensionStateError::InvalidContract);
    }
    validate_namespace(&transaction.namespace)?;
    if !valid_identifier(&transaction.idempotency_key, 128) {
        return Err(ExtensionStateError::InvalidIdempotencyKey);
    }
    if transaction.operations.is_empty() || transaction.operations.len() > MAX_STATE_OPERATIONS {
        return Err(ExtensionStateError::LimitExceeded("operations"));
    }
    for operation in &transaction.operations {
        if !valid_identifier(&operation.key, 128) {
            return Err(ExtensionStateError::InvalidKey);
        }
        if let Some(value) = &operation.value {
            let bytes =
                serde_json::to_vec(value).map_err(|_| ExtensionStateError::Serialization)?;
            if bytes.len() > MAX_STATE_VALUE_BYTES {
                return Err(ExtensionStateError::LimitExceeded("value bytes"));
            }
        }
    }
    Ok(())
}

fn validate_namespace(namespace: &str) -> Result<(), ExtensionStateError> {
    if !valid_identifier(namespace, 128) || !namespace.contains('.') {
        return Err(ExtensionStateError::InvalidNamespace);
    }
    Ok(())
}

fn validate_candidate(state: &NamespaceState) -> Result<(), ExtensionStateError> {
    if state.values.len() > MAX_STATE_KEYS_PER_NAMESPACE {
        return Err(ExtensionStateError::LimitExceeded("keys"));
    }
    for (key, value) in &state.values {
        if !valid_identifier(key, 128) {
            return Err(ExtensionStateError::InvalidKey);
        }
        let bytes = serde_json::to_vec(value).map_err(|_| ExtensionStateError::Serialization)?;
        if bytes.len() > MAX_STATE_VALUE_BYTES {
            return Err(ExtensionStateError::LimitExceeded("value bytes"));
        }
    }
    let bytes =
        serde_json::to_vec(&state.values).map_err(|_| ExtensionStateError::Serialization)?;
    if bytes.len() > MAX_STATE_NAMESPACE_BYTES {
        return Err(ExtensionStateError::LimitExceeded("namespace bytes"));
    }
    Ok(())
}

fn transaction_digest(
    transaction: &ExtensionStateTransactionV1,
) -> Result<String, ExtensionStateError> {
    let bytes = serde_json::to_vec(&(
        &transaction.namespace,
        transaction.expected_store_generation,
        transaction.expected_revision,
        &transaction.operations,
    ))
    .map_err(|_| ExtensionStateError::Serialization)?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

fn valid_identifier(value: &str, max: usize) -> bool {
    !value.is_empty()
        && value.len() <= max
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn transaction(revision: u64, idempotency_key: &str) -> ExtensionStateTransactionV1 {
        ExtensionStateTransactionV1 {
            contract: EXTENSION_STATE_CONTRACT_V1.into(),
            namespace: "example.extension".into(),
            expected_store_generation: 1,
            expected_revision: revision,
            idempotency_key: idempotency_key.into(),
            operations: vec![ExtensionStateOperationV1 {
                key: "selected".into(),
                value: Some(Value::String("one".into())),
            }],
        }
    }

    #[test]
    fn transaction_is_atomic_cas_and_idempotent() {
        let mut store = DurableExtensionStateV1::new("session:one".into(), 1).unwrap();
        let first = store.transact(&transaction(0, "request-1")).unwrap();
        assert_eq!(first.revision, 1);
        let replay = store.transact(&transaction(0, "request-1")).unwrap();
        assert!(replay.replayed);
        assert_eq!(replay.sequence, first.sequence);
        assert_eq!(
            store.transact(&transaction(0, "request-2")),
            Err(ExtensionStateError::RevisionConflict { actual: 1 })
        );
        assert_eq!(store.snapshot("example.extension").unwrap().revision, 1);
    }

    #[test]
    fn an_idempotency_key_cannot_be_reused_for_different_content() {
        let mut store = DurableExtensionStateV1::new("session:one".into(), 1).unwrap();
        store.transact(&transaction(0, "request-1")).unwrap();
        let mut conflicting = transaction(1, "request-1");
        conflicting.operations[0].value = Some(Value::String("two".into()));
        assert_eq!(
            store.transact(&conflicting),
            Err(ExtensionStateError::IdempotencyConflict)
        );
    }

    #[test]
    fn clearing_a_namespace_removes_all_owned_values() {
        let mut store = DurableExtensionStateV1::new("session:one".into(), 1).unwrap();
        store.transact(&transaction(0, "request-1")).unwrap();
        let commit = store
            .clear_namespace("example.extension", "delete-data".into())
            .unwrap()
            .unwrap();
        assert_eq!(commit.revision, 2);
        assert!(store
            .snapshot("example.extension")
            .unwrap()
            .values
            .is_empty());
        assert!(store
            .clear_namespace("missing.extension", "delete-missing".into())
            .unwrap()
            .is_none());
    }

    #[test]
    fn retained_event_gaps_require_resnapshot() {
        let mut store = DurableExtensionStateV1::new("session:one".into(), 1).unwrap();
        for revision in 0..=MAX_STATE_EVENTS {
            store
                .transact(&transaction(
                    revision as u64,
                    &format!("request-{revision}"),
                ))
                .unwrap();
        }
        let page = store.events_after(0, 10);
        assert!(page.gap);
        assert!(page.events.is_empty());
    }

    #[test]
    fn empty_namespace_tombstones_are_reclaimed_with_a_generation_rollover() {
        let mut store = DurableExtensionStateV1::new("session:one".into(), 1).unwrap();
        for index in 0..MAX_STATE_NAMESPACES {
            let namespace = format!("example.extension-{index}");
            let mut put = transaction(0, &format!("put-{index}"));
            put.namespace = namespace.clone();
            store.transact(&put).unwrap();
            let mut delete = transaction(1, &format!("delete-{index}"));
            delete.namespace = namespace;
            delete.operations[0].value = None;
            store.transact(&delete).unwrap();
        }
        for index in 0..MAX_STATE_EVENTS {
            let revision = store.snapshot("example.extension-0").unwrap().revision;
            let mut churn = transaction(revision, &format!("churn-{index}"));
            churn.namespace = "example.extension-0".into();
            store.transact(&churn).unwrap();
        }
        let previous_generation = store.store_generation;
        let mut replacement = transaction(0, "new-namespace");
        replacement.namespace = "example.replacement".into();
        replacement.expected_store_generation = previous_generation;
        let commit = store.transact(&replacement).unwrap();
        assert!(store.store_generation > previous_generation);
        assert_eq!(commit.store_generation, store.store_generation);
        store.validate().unwrap();
    }

    #[test]
    fn serialized_store_validates_after_round_trip() {
        let mut store = DurableExtensionStateV1::new("session:one".into(), 1).unwrap();
        store.transact(&transaction(0, "request-1")).unwrap();
        let decoded: DurableExtensionStateV1 =
            serde_json::from_slice(&serde_json::to_vec(&store).unwrap()).unwrap();
        decoded.validate().unwrap();
        assert_eq!(decoded, store);
    }
}
