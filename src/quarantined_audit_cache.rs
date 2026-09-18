//! Nonpersistent audit cache for producer-owned, scope-bound values.
//!
//! The registry is intentionally not attached to runtime, persistence, `AppState`, terminal,
//! pane, render, or client state. Callers explicitly advance authority, start an audit, and
//! publish only through a one-shot ticket. The generic value has no execution semantics.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::host_document_v2::serialized_len_capped;
use crate::host_document_v2::validate_dependencies;
use crate::host_document_v2::validate_producer_fence;
use crate::host_document_v2::validate_scope;
use crate::host_document_v2::CappedSerializationError;
use crate::host_document_v2::DocumentDependencyV2;
use crate::host_document_v2::DocumentProducerFenceV2;
use crate::host_document_v2::DocumentProducerV2;
use crate::host_document_v2::DocumentScopeV2;

pub const MAX_AUDIT_AUTHORITIES: usize = 64;
pub const MAX_AUDIT_CACHE_ENTRIES: usize = 64;
pub const MAX_AUDIT_CACHE_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct AuditCacheKey {
    pub producer: DocumentProducerV2,
    pub scope: DocumentScopeV2,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CurrentAuthority {
    fence: DocumentProducerFenceV2,
    dependencies: Vec<DocumentDependencyV2>,
    latest_attempt: u64,
}

/// A move-only, one-shot proof of the authority observed when an audit began.
#[derive(Debug, PartialEq, Eq)]
pub struct AuditTicket {
    key: AuditCacheKey,
    fence: DocumentProducerFenceV2,
    dependencies: Vec<DocumentDependencyV2>,
    attempt: u64,
}

#[derive(Clone, Debug)]
struct AuditEntry<V> {
    value: V,
    encoded_bytes: usize,
    accepted_order: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuditCacheError {
    InvalidAuthority(String),
    AuthorityCapacity,
    UnknownAuthority,
    StaleAudit,
    Serialization,
    EntryTooLarge,
    SequenceExhausted,
    AccountingInvariant,
}

/// A bounded, process-local cache. It has no serde implementation by design and is never
/// persisted. `V` may be any serializable output that has completed the caller's audit.
#[derive(Debug)]
pub struct QuarantinedAuditCache<V> {
    authority: BTreeMap<AuditCacheKey, CurrentAuthority>,
    entries: BTreeMap<AuditCacheKey, AuditEntry<V>>,
    total_bytes: usize,
    next_attempt: u64,
    next_accept_order: u64,
    max_authorities: usize,
    max_entries: usize,
    max_bytes: usize,
}

impl<V> Default for QuarantinedAuditCache<V> {
    fn default() -> Self {
        Self::new(
            MAX_AUDIT_AUTHORITIES,
            MAX_AUDIT_CACHE_ENTRIES,
            MAX_AUDIT_CACHE_BYTES,
        )
    }
}

impl<V> QuarantinedAuditCache<V> {
    pub fn new(max_authorities: usize, max_entries: usize, max_bytes: usize) -> Self {
        Self {
            authority: BTreeMap::new(),
            entries: BTreeMap::new(),
            total_bytes: 0,
            next_attempt: 0,
            next_accept_order: 0,
            max_authorities,
            max_entries,
            max_bytes,
        }
    }

    /// Installs exact authority. New keys are rejected at capacity; authority is never evicted
    /// implicitly. Callers must revoke a producer or retire a scope before admitting another key.
    pub fn update_authority(
        &mut self,
        key: AuditCacheKey,
        fence: DocumentProducerFenceV2,
        dependencies: Vec<DocumentDependencyV2>,
    ) -> Result<bool, AuditCacheError> {
        validate_producer_fence(&fence)
            .map_err(|error| AuditCacheError::InvalidAuthority(error.to_string()))?;
        validate_scope(&key.scope)
            .map_err(|error| AuditCacheError::InvalidAuthority(error.to_string()))?;
        validate_dependencies(&dependencies)
            .map_err(|error| AuditCacheError::InvalidAuthority(error.to_string()))?;
        if key.producer != fence.producer() {
            return Err(AuditCacheError::InvalidAuthority(
                "cache key producer does not match producer fence".into(),
            ));
        }

        if self
            .authority
            .get(&key)
            .is_some_and(|current| current.fence == fence && current.dependencies == dependencies)
        {
            return Ok(false);
        }
        if !self.authority.contains_key(&key) && self.authority.len() >= self.max_authorities {
            return Err(AuditCacheError::AuthorityCapacity);
        }

        self.remove_entry(&key)?;
        self.authority.insert(
            key,
            CurrentAuthority {
                fence,
                dependencies,
                latest_attempt: 0,
            },
        );
        Ok(true)
    }

    /// Captures one immutable attempt. Starting a newer attempt makes all older completions stale.
    pub fn begin_audit(&mut self, key: &AuditCacheKey) -> Result<AuditTicket, AuditCacheError> {
        let attempt = self
            .next_attempt
            .checked_add(1)
            .ok_or(AuditCacheError::SequenceExhausted)?;
        let current = self
            .authority
            .get_mut(key)
            .ok_or(AuditCacheError::UnknownAuthority)?;
        self.next_attempt = attempt;
        current.latest_attempt = attempt;
        Ok(AuditTicket {
            key: key.clone(),
            fence: current.fence.clone(),
            dependencies: current.dependencies.clone(),
            attempt,
        })
    }

    /// Consumes a successful audit attempt before serialization or accounting. Every outcome is
    /// terminal for the move-only ticket, including serialization, capacity, and sequence errors.
    pub fn accept(&mut self, ticket: AuditTicket, value: V) -> Result<(), AuditCacheError>
    where
        V: Serialize,
    {
        self.consume_ticket(&ticket)?;

        let encoded_bytes = match serialized_len_capped(&value, self.max_bytes) {
            Ok(bytes) => bytes,
            Err(CappedSerializationError::LimitExceeded) => {
                return Err(AuditCacheError::EntryTooLarge);
            }
            Err(CappedSerializationError::Serialization) => {
                return Err(AuditCacheError::Serialization);
            }
        };
        if self.max_entries == 0 {
            return Err(AuditCacheError::EntryTooLarge);
        }
        let accepted_order = self
            .next_accept_order
            .checked_add(1)
            .ok_or(AuditCacheError::SequenceExhausted)?;

        let previous_bytes = self
            .entries
            .get(&ticket.key)
            .map_or(0, |entry| entry.encoded_bytes);
        let without_previous = self
            .total_bytes
            .checked_sub(previous_bytes)
            .ok_or(AuditCacheError::AccountingInvariant)?;
        let next_total = without_previous
            .checked_add(encoded_bytes)
            .ok_or(AuditCacheError::AccountingInvariant)?;

        self.entries.insert(
            ticket.key,
            AuditEntry {
                value,
                encoded_bytes,
                accepted_order,
            },
        );
        self.total_bytes = next_total;
        self.next_accept_order = accepted_order;
        self.evict_to_limits()
    }

    /// Applies a failure only if it is still current. The move-only ticket is consumed before the
    /// entry is cleared, so no accepted or rejected attempt can be replayed.
    pub fn fail(&mut self, ticket: AuditTicket) -> Result<(), AuditCacheError> {
        self.consume_ticket(&ticket)?;
        self.remove_entry(&ticket.key)
    }

    pub fn get(&self, key: &AuditCacheKey) -> Option<&V> {
        self.entries.get(key).map(|entry| &entry.value)
    }

    #[cfg(test)]
    pub(crate) fn first_value(&self) -> Option<&V> {
        self.entries.values().next().map(|entry| &entry.value)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn authority_len(&self) -> usize {
        self.authority.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn encoded_bytes(&self) -> usize {
        self.total_bytes
    }

    /// Explicitly frees every bounded authority/cache slot owned by a producer.
    pub fn revoke_producer(
        &mut self,
        producer: &DocumentProducerV2,
    ) -> Result<usize, AuditCacheError> {
        let keys = self
            .authority
            .keys()
            .filter(|key| &key.producer == producer)
            .cloned()
            .collect::<Vec<_>>();
        self.remove_authorities(&keys)?;
        Ok(keys.len())
    }

    /// Explicitly frees one named scope across producers.
    pub fn retire_scope(&mut self, scope: &DocumentScopeV2) -> Result<usize, AuditCacheError> {
        let keys = self
            .authority
            .keys()
            .filter(|key| &key.scope == scope)
            .cloned()
            .collect::<Vec<_>>();
        self.remove_authorities(&keys)?;
        Ok(keys.len())
    }

    fn consume_ticket(&mut self, ticket: &AuditTicket) -> Result<(), AuditCacheError> {
        let current = self
            .authority
            .get_mut(&ticket.key)
            .ok_or(AuditCacheError::StaleAudit)?;
        if current.latest_attempt != ticket.attempt
            || current.fence != ticket.fence
            || current.dependencies != ticket.dependencies
        {
            return Err(AuditCacheError::StaleAudit);
        }
        current.latest_attempt = 0;
        Ok(())
    }

    fn remove_authorities(&mut self, keys: &[AuditCacheKey]) -> Result<(), AuditCacheError> {
        let removed_bytes = keys.iter().try_fold(0_usize, |total, key| {
            total
                .checked_add(self.entries.get(key).map_or(0, |entry| entry.encoded_bytes))
                .ok_or(AuditCacheError::AccountingInvariant)
        })?;
        let next_total = self
            .total_bytes
            .checked_sub(removed_bytes)
            .ok_or(AuditCacheError::AccountingInvariant)?;
        for key in keys {
            self.authority.remove(key);
            self.entries.remove(key);
        }
        self.total_bytes = next_total;
        Ok(())
    }

    fn remove_entry(&mut self, key: &AuditCacheKey) -> Result<(), AuditCacheError> {
        let Some(previous_bytes) = self.entries.get(key).map(|entry| entry.encoded_bytes) else {
            return Ok(());
        };
        let next_total = self
            .total_bytes
            .checked_sub(previous_bytes)
            .ok_or(AuditCacheError::AccountingInvariant)?;
        self.entries.remove(key);
        self.total_bytes = next_total;
        Ok(())
    }

    fn evict_to_limits(&mut self) -> Result<(), AuditCacheError> {
        while self.entries.len() > self.max_entries || self.total_bytes > self.max_bytes {
            let victim = self
                .entries
                .iter()
                .min_by_key(|(key, entry)| (entry.accepted_order, *key))
                .map(|(key, _)| key.clone())
                .ok_or(AuditCacheError::AccountingInvariant)?;
            self.remove_entry(&victim)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use serde::ser::{Error as _, SerializeSeq as _};

    use super::*;

    fn fence(generation: u64) -> DocumentProducerFenceV2 {
        DocumentProducerFenceV2 {
            package_id: "example.document".into(),
            service_id: "producer".into(),
            artifact_sha256: "a".repeat(64),
            runtime_generation: generation,
            provider_generation: generation,
            scope_generation: generation,
        }
    }

    fn key(scope: &str) -> AuditCacheKey {
        AuditCacheKey {
            producer: fence(1).producer(),
            scope: DocumentScopeV2 {
                contract: "host.session.v1".into(),
                scope_id: scope.into(),
            },
        }
    }

    fn dependency(revision: u64) -> Vec<DocumentDependencyV2> {
        vec![DocumentDependencyV2 {
            contract: "host.scope.v1".into(),
            scope_id: "session".into(),
            revision,
            generation: 1,
        }]
    }

    fn activate<V>(cache: &mut QuarantinedAuditCache<V>, key: &AuditCacheKey) {
        cache
            .update_authority(key.clone(), fence(1), Vec::new())
            .unwrap();
    }

    #[test]
    fn late_attempts_cannot_replace_or_clear_newer_values() {
        let mut cache = QuarantinedAuditCache::default();
        let key = key("session");
        cache
            .update_authority(key.clone(), fence(1), dependency(1))
            .unwrap();
        let old = cache.begin_audit(&key).unwrap();
        let current = cache.begin_audit(&key).unwrap();
        cache.accept(current, "new").unwrap();
        assert_eq!(cache.accept(old, "old"), Err(AuditCacheError::StaleAudit));
        assert_eq!(cache.get(&key), Some(&"new"));

        let stale_failure = cache.begin_audit(&key).unwrap();
        let newer = cache.begin_audit(&key).unwrap();
        cache.accept(newer, "newer").unwrap();
        assert_eq!(cache.fail(stale_failure), Err(AuditCacheError::StaleAudit));
        assert_eq!(cache.get(&key), Some(&"newer"));
    }

    #[test]
    fn authority_changes_quarantine_entries_and_stale_completions() {
        let mut cache = QuarantinedAuditCache::default();
        let key = key("session");
        cache
            .update_authority(key.clone(), fence(1), dependency(1))
            .unwrap();
        let ticket = cache.begin_audit(&key).unwrap();
        cache.accept(ticket, "accepted").unwrap();
        let stale = cache.begin_audit(&key).unwrap();
        cache
            .update_authority(key.clone(), fence(2), dependency(2))
            .unwrap();
        assert!(cache.get(&key).is_none());
        assert_eq!(
            cache.accept(stale, "stale"),
            Err(AuditCacheError::StaleAudit)
        );
    }

    #[test]
    fn invalid_authority_and_zero_capacity_are_rejected() {
        let mut cache = QuarantinedAuditCache::<String>::new(0, 1, 1024);
        let key = key("session");
        assert_eq!(
            cache.update_authority(key.clone(), fence(1), Vec::new()),
            Err(AuditCacheError::AuthorityCapacity)
        );

        let mut cache = QuarantinedAuditCache::<String>::new(1, 1, 1024);
        let mut mismatched = fence(1);
        mismatched.service_id = "other".into();
        assert!(matches!(
            cache.update_authority(key.clone(), mismatched, Vec::new()),
            Err(AuditCacheError::InvalidAuthority(_))
        ));
        assert_eq!(
            cache.begin_audit(&key),
            Err(AuditCacheError::UnknownAuthority)
        );
    }

    #[test]
    fn current_failure_clears_value_and_consumes_attempt() {
        let mut cache = QuarantinedAuditCache::new(1, 1, 1024);
        let key = key("session");
        activate(&mut cache, &key);
        let accepted = cache.begin_audit(&key).unwrap();
        cache.accept(accepted, "value").unwrap();
        let failed = cache.begin_audit(&key).unwrap();
        cache.fail(failed).unwrap();
        assert!(cache.is_empty());
        assert_eq!(cache.authority[&key].latest_attempt, 0);
    }

    #[test]
    fn zero_entry_capacity_consumes_and_rejects_audit() {
        let mut cache = QuarantinedAuditCache::new(1, 0, 1024);
        let key = key("session");
        activate(&mut cache, &key);
        let ticket = cache.begin_audit(&key).unwrap();
        assert_eq!(
            cache.accept(ticket, "value"),
            Err(AuditCacheError::EntryTooLarge)
        );
        assert_eq!(cache.authority[&key].latest_attempt, 0);
        assert!(cache.is_empty());
    }

    #[test]
    fn authority_capacity_requires_explicit_revocation_or_retirement() {
        let mut cache = QuarantinedAuditCache::<String>::new(2, 2, 1024);
        let first = key("a");
        let second = key("b");
        let third = key("c");
        activate(&mut cache, &first);
        activate(&mut cache, &second);
        assert_eq!(cache.authority_len(), 2);
        assert_eq!(
            cache.update_authority(third.clone(), fence(1), Vec::new()),
            Err(AuditCacheError::AuthorityCapacity)
        );
        assert_eq!(cache.revoke_producer(&first.producer).unwrap(), 2);
        assert_eq!(cache.authority_len(), 0);
        activate(&mut cache, &third);

        let fourth = key("d");
        activate(&mut cache, &fourth);
        assert_eq!(cache.retire_scope(&third.scope).unwrap(), 1);
        assert_eq!(cache.authority_len(), 1);
        activate(&mut cache, &first);
        assert_eq!(cache.authority_len(), 2);
    }

    #[test]
    fn updating_existing_authority_is_allowed_at_capacity() {
        let mut cache = QuarantinedAuditCache::<String>::new(1, 1, 1024);
        let key = key("session");
        activate(&mut cache, &key);
        assert!(cache
            .update_authority(key.clone(), fence(2), dependency(2))
            .unwrap());
        assert_eq!(cache.authority_len(), 1);
    }

    struct LargeStream {
        serialized_items: Rc<Cell<usize>>,
    }

    impl Serialize for LargeStream {
        fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
        where
            S: serde::Serializer,
        {
            let mut sequence = serializer.serialize_seq(Some(usize::MAX))?;
            for _ in 0..usize::MAX {
                self.serialized_items.set(
                    self.serialized_items
                        .get()
                        .checked_add(1)
                        .ok_or_else(|| S::Error::custom("test serialization counter overflow"))?,
                );
                sequence.serialize_element("xxxxxxxxxxxxxxxx")?;
            }
            sequence.end()
        }
    }

    #[test]
    fn capped_writer_aborts_streaming_serialization_and_consumes_ticket() {
        let mut cache = QuarantinedAuditCache::new(1, 1, 64);
        let key = key("session");
        activate(&mut cache, &key);
        let count = Rc::new(Cell::new(0));
        let ticket = cache.begin_audit(&key).unwrap();
        assert_eq!(
            cache.accept(
                ticket,
                LargeStream {
                    serialized_items: Rc::clone(&count),
                },
            ),
            Err(AuditCacheError::EntryTooLarge)
        );
        assert!(count.get() < 10, "serializer did not abort at the cap");
        assert_eq!(cache.authority[&key].latest_attempt, 0);

        // The rejected attempt was consumed; only a newly issued ticket may publish.
        let replacement = cache.begin_audit(&key).unwrap();
        cache
            .accept(
                replacement,
                LargeStream {
                    serialized_items: Rc::new(Cell::new(0)),
                },
            )
            .unwrap_err();
    }

    struct SerializationFailure;

    impl Serialize for SerializationFailure {
        fn serialize<S>(&self, _serializer: S) -> Result<S::Ok, S::Error>
        where
            S: serde::Serializer,
        {
            Err(S::Error::custom("intentional failure"))
        }
    }

    #[test]
    fn serialization_and_sequence_failures_consume_current_attempt() {
        let mut cache = QuarantinedAuditCache::new(1, 1, 1024);
        let key = key("session");
        activate(&mut cache, &key);
        let ticket = cache.begin_audit(&key).unwrap();
        assert_eq!(
            cache.accept(ticket, SerializationFailure),
            Err(AuditCacheError::Serialization)
        );
        assert!(cache.is_empty());
        assert_eq!(cache.authority[&key].latest_attempt, 0);

        let mut sequence_cache = QuarantinedAuditCache::new(1, 1, 1024);
        activate(&mut sequence_cache, &key);
        sequence_cache.next_accept_order = u64::MAX;
        let ticket = sequence_cache.begin_audit(&key).unwrap();
        assert_eq!(
            sequence_cache.accept(ticket, "value"),
            Err(AuditCacheError::SequenceExhausted)
        );
        assert!(sequence_cache.is_empty());
        assert_eq!(sequence_cache.authority[&key].latest_attempt, 0);
    }

    #[test]
    fn attempt_sequence_overflow_preserves_authority_and_entries() {
        let mut cache = QuarantinedAuditCache::new(1, 1, 1024);
        let key = key("session");
        activate(&mut cache, &key);
        let ticket = cache.begin_audit(&key).unwrap();
        cache.accept(ticket, "value").unwrap();
        cache.next_attempt = u64::MAX;
        assert_eq!(
            cache.begin_audit(&key),
            Err(AuditCacheError::SequenceExhausted)
        );
        assert_eq!(cache.get(&key), Some(&"value"));
    }

    #[test]
    fn accounting_overflow_and_underflow_are_reported_without_mutating_entry() {
        let mut cache = QuarantinedAuditCache::new(1, 1, 1024);
        let key = key("session");
        activate(&mut cache, &key);
        let ticket = cache.begin_audit(&key).unwrap();
        cache.accept(ticket, "value").unwrap();
        cache.total_bytes = usize::MAX;
        let ticket = cache.begin_audit(&key).unwrap();
        assert_eq!(
            cache.accept(ticket, "a substantially larger replacement value"),
            Err(AuditCacheError::AccountingInvariant)
        );
        assert_eq!(cache.get(&key), Some(&"value"));
        assert_eq!(cache.authority[&key].latest_attempt, 0);

        cache.total_bytes = 0;
        assert_eq!(
            cache.retire_scope(&key.scope),
            Err(AuditCacheError::AccountingInvariant)
        );
        assert!(cache.get(&key).is_some());
        assert_eq!(cache.authority_len(), 1);
    }

    #[test]
    fn eviction_is_oldest_first_and_byte_bounded() {
        let mut cache = QuarantinedAuditCache::new(3, 2, 15);
        let first = key("a");
        let second = key("b");
        let third = key("c");
        for (key, value) in [(&first, "first"), (&second, "second"), (&third, "third")] {
            activate(&mut cache, key);
            let ticket = cache.begin_audit(key).unwrap();
            cache.accept(ticket, value).unwrap();
        }
        assert!(cache.get(&first).is_none());
        assert_eq!(cache.get(&second), Some(&"second"));
        assert_eq!(cache.get(&third), Some(&"third"));
        assert!(cache.encoded_bytes() <= 15);
        assert!(cache.len() <= 2);
    }

    #[test]
    fn revocation_and_scope_retirement_clear_authority_values_and_tickets() {
        let mut cache = QuarantinedAuditCache::default();
        let first = key("a");
        let second = key("b");
        for key in [&first, &second] {
            activate(&mut cache, key);
            let ticket = cache.begin_audit(key).unwrap();
            cache.accept(ticket, key.scope.scope_id.clone()).unwrap();
        }
        let stale = cache.begin_audit(&first).unwrap();
        assert_eq!(cache.retire_scope(&first.scope).unwrap(), 1);
        assert_eq!(
            cache.accept(stale, "stale".into()),
            Err(AuditCacheError::StaleAudit)
        );
        assert!(cache.get(&first).is_none());
        assert_eq!(cache.revoke_producer(&second.producer).unwrap(), 1);
        assert!(cache.is_empty());
        assert_eq!(cache.authority_len(), 0);
        assert_eq!(
            cache.begin_audit(&second),
            Err(AuditCacheError::UnknownAuthority)
        );
    }
}
