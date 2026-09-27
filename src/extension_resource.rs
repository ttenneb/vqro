//! Terminal-owned, provider-generation-fenced extension resources.
//!
//! Resources are transient declarative data. They never transfer terminal, process, layout,
//! input, or rendering ownership to a provider and are never persisted across host boots.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const TERMINAL_RESOURCE_CONTRACT_V1: &str = "host.terminal-resource.v1";
pub const MAX_RESOURCES_PER_REPLACEMENT: usize = 512;
pub const MAX_RESOURCE_PAYLOAD_BYTES: usize = 8 * 1024;
pub const MAX_RESOURCE_TTL_MS: u64 = 60 * 1000;
pub const MIN_RESOURCE_TTL_MS: u64 = 250;
pub const MAX_CLIENT_INTERESTS: usize = 64;

#[derive(
    Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, schemars::JsonSchema,
)]
pub struct ResourceProviderV1 {
    pub namespace: String,
    pub service_id: String,
    pub generation: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TerminalResourceV1 {
    pub terminal_id: String,
    pub resource_id: String,
    pub kind: String,
    pub schema: String,
    pub payload: Value,
}

/// Complete atomic replacement for one provider generation and aggregate interest revision.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TerminalResourceReplacementV1 {
    pub contract: String,
    pub provider: ResourceProviderV1,
    pub interest_revision: u64,
    pub publication_revision: u64,
    pub valid_for_ms: u64,
    pub resources: Vec<TerminalResourceV1>,
}

#[derive(
    Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, schemars::JsonSchema,
)]
pub struct TerminalResourceInterestV1 {
    pub terminal_id: String,
    pub kind: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ClientResourceInterestsV1 {
    pub contract: String,
    pub client_id: String,
    pub client_generation: u64,
    pub revision: u64,
    pub interests: Vec<TerminalResourceInterestV1>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TerminalResourceError {
    InvalidContract,
    InvalidIdentifier,
    InvalidGeneration,
    StaleGeneration,
    StaleInterest { actual: u64 },
    StaleRevision,
    UnknownTerminal,
    DuplicateResource,
    UninterestedResource,
    InvalidTtl,
    LimitExceeded(&'static str),
    Serialization,
}

#[derive(Clone, Debug)]
struct ProviderPartition {
    provider: ResourceProviderV1,
    publication_revision: u64,
    expires_at: Option<Instant>,
    resources: Vec<TerminalResourceV1>,
}

#[derive(Clone, Debug, Default)]
struct ClientInterests {
    generation: u64,
    revision: u64,
    interests: BTreeSet<TerminalResourceInterestV1>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TerminalResourceProjectionV1 {
    pub registry_revision: u64,
    pub provider: ResourceProviderV1,
    pub publication_revision: u64,
    pub resource: TerminalResourceV1,
}

#[derive(Clone, Debug, Default)]
pub struct TerminalResourceRegistry {
    partitions: BTreeMap<(String, String), ProviderPartition>,
    active_terminals: HashSet<String>,
    retired_terminals: HashSet<String>,
    clients: HashMap<String, ClientInterests>,
    aggregate_interest: HashMap<TerminalResourceInterestV1, usize>,
    interest_revision: u64,
    registry_revision: u64,
}

impl TerminalResourceRegistry {
    pub fn register_terminal(&mut self, terminal_id: String) -> Result<(), TerminalResourceError> {
        validate_identifier(&terminal_id)?;
        if self.retired_terminals.contains(&terminal_id) {
            return Err(TerminalResourceError::UnknownTerminal);
        }
        if self.active_terminals.contains(&terminal_id) {
            return Ok(());
        }
        let next_revision = self.next_registry_revision()?;
        self.active_terminals.insert(terminal_id);
        self.registry_revision = next_revision;
        Ok(())
    }

    /// Advances authoritative provider identity before any request is sent to a new process.
    pub fn activate_provider(
        &mut self,
        provider: ResourceProviderV1,
    ) -> Result<(), TerminalResourceError> {
        validate_provider(&provider)?;
        let key = (provider.namespace.clone(), provider.service_id.clone());
        if self
            .partitions
            .get(&key)
            .is_some_and(|current| provider.generation <= current.provider.generation)
        {
            return Err(TerminalResourceError::StaleGeneration);
        }
        let next_revision = self.next_registry_revision()?;
        self.partitions.insert(
            key,
            ProviderPartition {
                provider,
                publication_revision: 0,
                expires_at: None,
                resources: Vec::new(),
            },
        );
        self.registry_revision = next_revision;
        Ok(())
    }

    pub fn replace(
        &mut self,
        replacement: TerminalResourceReplacementV1,
        now: Instant,
    ) -> Result<(), TerminalResourceError> {
        validate_replacement(&replacement)?;
        if replacement.interest_revision != self.interest_revision {
            return Err(TerminalResourceError::StaleInterest {
                actual: self.interest_revision,
            });
        }
        for resource in &replacement.resources {
            if !self.active_terminals.contains(&resource.terminal_id) {
                return Err(TerminalResourceError::UnknownTerminal);
            }
            let interest = TerminalResourceInterestV1 {
                terminal_id: resource.terminal_id.clone(),
                kind: resource.kind.clone(),
            };
            if self.interest_count(&interest) == 0 {
                return Err(TerminalResourceError::UninterestedResource);
            }
        }
        let key = (
            replacement.provider.namespace.clone(),
            replacement.provider.service_id.clone(),
        );
        let current = self
            .partitions
            .get(&key)
            .ok_or(TerminalResourceError::StaleGeneration)?;
        if replacement.provider.generation != current.provider.generation {
            return Err(TerminalResourceError::StaleGeneration);
        }
        if replacement.publication_revision <= current.publication_revision {
            return Err(TerminalResourceError::StaleRevision);
        }
        let expires_at = now
            .checked_add(Duration::from_millis(replacement.valid_for_ms))
            .ok_or(TerminalResourceError::InvalidTtl)?;
        let next_revision = self.next_registry_revision()?;
        self.partitions.insert(
            key,
            ProviderPartition {
                provider: replacement.provider,
                publication_revision: replacement.publication_revision,
                expires_at: Some(expires_at),
                resources: replacement.resources,
            },
        );
        self.registry_revision = next_revision;
        Ok(())
    }

    pub fn replace_client_interests(
        &mut self,
        replacement: ClientResourceInterestsV1,
    ) -> Result<bool, TerminalResourceError> {
        if replacement.contract != TERMINAL_RESOURCE_CONTRACT_V1 {
            return Err(TerminalResourceError::InvalidContract);
        }
        validate_identifier(&replacement.client_id)?;
        if replacement.client_generation == 0 {
            return Err(TerminalResourceError::InvalidGeneration);
        }
        if replacement.interests.len() > MAX_CLIENT_INTERESTS {
            return Err(TerminalResourceError::LimitExceeded("client interests"));
        }
        let mut next = BTreeSet::new();
        for interest in replacement.interests {
            validate_interest(&interest)?;
            if !self.active_terminals.contains(&interest.terminal_id) {
                return Err(TerminalResourceError::UnknownTerminal);
            }
            next.insert(interest);
        }
        if let Some(current) = self.clients.get(&replacement.client_id) {
            if replacement.client_generation < current.generation
                || (replacement.client_generation == current.generation
                    && replacement.revision <= current.revision)
            {
                return Err(TerminalResourceError::StaleRevision);
            }
        }
        let changed =
            self.clients
                .get(&replacement.client_id)
                .map_or(!next.is_empty(), |previous| {
                    previous.generation != replacement.client_generation
                        || previous.interests != next
                });
        let revisions = changed
            .then(|| {
                Ok((
                    self.next_interest_revision()?,
                    self.next_registry_revision()?,
                ))
            })
            .transpose()?;
        self.clients.insert(
            replacement.client_id,
            ClientInterests {
                generation: replacement.client_generation,
                revision: replacement.revision,
                interests: next,
            },
        );
        if let Some((interest_revision, registry_revision)) = revisions {
            self.interest_revision = interest_revision;
            self.registry_revision = registry_revision;
            self.rebuild_aggregate_interest();
            self.prune_uninterested_resources();
        }
        Ok(changed)
    }

    pub fn remove_client(
        &mut self,
        client_id: &str,
        client_generation: u64,
    ) -> Result<bool, TerminalResourceError> {
        let removed = self
            .clients
            .get(client_id)
            .is_some_and(|client| client.generation == client_generation);
        if removed {
            let interest_revision = self.next_interest_revision()?;
            let registry_revision = self.next_registry_revision()?;
            self.clients.remove(client_id);
            self.interest_revision = interest_revision;
            self.registry_revision = registry_revision;
            self.rebuild_aggregate_interest();
            self.prune_uninterested_resources();
        }
        Ok(removed)
    }

    pub fn retire_terminal(&mut self, terminal_id: &str) -> Result<(), TerminalResourceError> {
        if !self.active_terminals.contains(terminal_id) {
            return Ok(());
        }
        let interest_revision = self.next_interest_revision()?;
        let registry_revision = self.next_registry_revision()?;
        self.active_terminals.remove(terminal_id);
        self.retired_terminals.insert(terminal_id.into());
        for client in self.clients.values_mut() {
            client
                .interests
                .retain(|interest| interest.terminal_id != terminal_id);
        }
        self.interest_revision = interest_revision;
        self.registry_revision = registry_revision;
        self.rebuild_aggregate_interest();
        self.prune_uninterested_resources();
        Ok(())
    }

    pub fn expire(&mut self, now: Instant) -> Result<usize, TerminalResourceError> {
        let has_expired = self.partitions.values().any(|partition| {
            partition.expires_at.is_some_and(|deadline| deadline <= now)
                && !partition.resources.is_empty()
        });
        let next_revision = has_expired
            .then(|| self.next_registry_revision())
            .transpose()?;
        let mut expired = 0;
        for partition in self.partitions.values_mut() {
            if partition.expires_at.is_some_and(|deadline| deadline <= now) {
                expired += partition.resources.len();
                partition.resources.clear();
                partition.expires_at = None;
            }
        }
        if let Some(next_revision) = next_revision {
            self.registry_revision = next_revision;
        }
        Ok(expired)
    }

    pub fn resources(
        &mut self,
        terminal_id: &str,
        kind: &str,
        now: Instant,
    ) -> Result<Vec<TerminalResourceProjectionV1>, TerminalResourceError> {
        self.expire(now)?;
        let interest = TerminalResourceInterestV1 {
            terminal_id: terminal_id.into(),
            kind: kind.into(),
        };
        if self.interest_count(&interest) == 0 {
            return Ok(Vec::new());
        }
        let registry_revision = self.registry_revision;
        Ok(self
            .partitions
            .values()
            .flat_map(|partition| {
                partition
                    .resources
                    .iter()
                    .filter(move |resource| {
                        resource.terminal_id == terminal_id && resource.kind == kind
                    })
                    .cloned()
                    .map(move |resource| TerminalResourceProjectionV1 {
                        registry_revision,
                        provider: partition.provider.clone(),
                        publication_revision: partition.publication_revision,
                        resource,
                    })
            })
            .collect())
    }

    pub fn interest_revision(&self) -> u64 {
        self.interest_revision
    }

    pub fn registry_revision(&self) -> u64 {
        self.registry_revision
    }

    pub fn interest_count(&self, interest: &TerminalResourceInterestV1) -> usize {
        self.aggregate_interest.get(interest).copied().unwrap_or(0)
    }

    pub fn aggregated_interests(&self) -> Vec<(TerminalResourceInterestV1, usize)> {
        let mut interests = self
            .aggregate_interest
            .iter()
            .map(|(interest, count)| (interest.clone(), *count))
            .collect::<Vec<_>>();
        interests.sort_by(|left, right| left.0.cmp(&right.0));
        interests
    }

    fn rebuild_aggregate_interest(&mut self) {
        self.aggregate_interest.clear();
        for client in self.clients.values() {
            for interest in &client.interests {
                *self.aggregate_interest.entry(interest.clone()).or_default() += 1;
            }
        }
    }

    fn prune_uninterested_resources(&mut self) {
        let aggregate = &self.aggregate_interest;
        for partition in self.partitions.values_mut() {
            partition.resources.retain(|resource| {
                aggregate.contains_key(&TerminalResourceInterestV1 {
                    terminal_id: resource.terminal_id.clone(),
                    kind: resource.kind.clone(),
                })
            });
        }
    }

    fn next_interest_revision(&self) -> Result<u64, TerminalResourceError> {
        self.interest_revision
            .checked_add(1)
            .ok_or(TerminalResourceError::LimitExceeded(
                "interest revision exhausted",
            ))
    }

    fn next_registry_revision(&self) -> Result<u64, TerminalResourceError> {
        self.registry_revision
            .checked_add(1)
            .ok_or(TerminalResourceError::LimitExceeded(
                "registry revision exhausted",
            ))
    }
}

fn validate_provider(provider: &ResourceProviderV1) -> Result<(), TerminalResourceError> {
    if provider.generation == 0 {
        return Err(TerminalResourceError::InvalidGeneration);
    }
    validate_identifier(&provider.namespace)?;
    validate_identifier(&provider.service_id)
}

fn validate_replacement(
    replacement: &TerminalResourceReplacementV1,
) -> Result<(), TerminalResourceError> {
    if replacement.contract != TERMINAL_RESOURCE_CONTRACT_V1 {
        return Err(TerminalResourceError::InvalidContract);
    }
    validate_provider(&replacement.provider)?;
    if replacement.publication_revision == 0 {
        return Err(TerminalResourceError::StaleRevision);
    }
    if !(MIN_RESOURCE_TTL_MS..=MAX_RESOURCE_TTL_MS).contains(&replacement.valid_for_ms) {
        return Err(TerminalResourceError::InvalidTtl);
    }
    if replacement.resources.len() > MAX_RESOURCES_PER_REPLACEMENT {
        return Err(TerminalResourceError::LimitExceeded("resources"));
    }
    let mut ids = BTreeSet::new();
    for resource in &replacement.resources {
        for identifier in [
            resource.terminal_id.as_str(),
            resource.resource_id.as_str(),
            resource.kind.as_str(),
            resource.schema.as_str(),
        ] {
            validate_identifier(identifier)?;
        }
        if !ids.insert((&resource.terminal_id, &resource.resource_id)) {
            return Err(TerminalResourceError::DuplicateResource);
        }
        let size = serde_json::to_vec(&resource.payload)
            .map_err(|_| TerminalResourceError::Serialization)?
            .len();
        if size > MAX_RESOURCE_PAYLOAD_BYTES {
            return Err(TerminalResourceError::LimitExceeded("resource payload"));
        }
    }
    Ok(())
}

fn validate_interest(interest: &TerminalResourceInterestV1) -> Result<(), TerminalResourceError> {
    validate_identifier(&interest.terminal_id)?;
    validate_identifier(&interest.kind)
}

fn validate_identifier(value: &str) -> Result<(), TerminalResourceError> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':'))
    {
        return Err(TerminalResourceError::InvalidIdentifier);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider(generation: u64) -> ResourceProviderV1 {
        ResourceProviderV1 {
            namespace: "example.extension".into(),
            service_id: "telemetry".into(),
            generation,
        }
    }

    fn replacement(
        generation: u64,
        interest_revision: u64,
        publication_revision: u64,
    ) -> TerminalResourceReplacementV1 {
        TerminalResourceReplacementV1 {
            contract: TERMINAL_RESOURCE_CONTRACT_V1.into(),
            provider: provider(generation),
            interest_revision,
            publication_revision,
            valid_for_ms: 500,
            resources: vec![TerminalResourceV1 {
                terminal_id: "term_1".into(),
                resource_id: "cpu".into(),
                kind: "telemetry".into(),
                schema: "example.telemetry.v1".into(),
                payload: serde_json::json!({"percent": 4}),
            }],
        }
    }

    fn interests(client: &str, generation: u64, revision: u64) -> ClientResourceInterestsV1 {
        ClientResourceInterestsV1 {
            contract: TERMINAL_RESOURCE_CONTRACT_V1.into(),
            client_id: client.into(),
            client_generation: generation,
            revision,
            interests: vec![TerminalResourceInterestV1 {
                terminal_id: "term_1".into(),
                kind: "telemetry".into(),
            }],
        }
    }

    fn registry() -> TerminalResourceRegistry {
        let mut registry = TerminalResourceRegistry::default();
        registry.register_terminal("term_1".into()).unwrap();
        registry.activate_provider(provider(1)).unwrap();
        registry
    }

    #[test]
    fn active_generation_interest_and_publication_fence_replacements() {
        let now = Instant::now();
        let mut registry = registry();
        registry
            .replace_client_interests(interests("a", 1, 1))
            .unwrap();
        registry.replace(replacement(1, 1, 1), now).unwrap();
        registry.activate_provider(provider(2)).unwrap();
        assert_eq!(
            registry.replace(replacement(1, 1, 2), now),
            Err(TerminalResourceError::StaleGeneration)
        );
        registry.replace(replacement(2, 1, 1), now).unwrap();
        assert_eq!(
            registry.replace(replacement(2, 1, 1), now),
            Err(TerminalResourceError::StaleRevision)
        );
    }

    #[test]
    fn retired_terminals_and_old_connection_cleanup_cannot_win_races() {
        let mut registry = registry();
        registry
            .replace_client_interests(interests("a", 1, 1))
            .unwrap();
        registry
            .replace_client_interests(interests("a", 2, 1))
            .unwrap();
        assert!(!registry.remove_client("a", 1).unwrap());
        assert_eq!(registry.aggregated_interests()[0].1, 1);
        registry.retire_terminal("term_1").unwrap();
        assert_eq!(
            registry.replace_client_interests(interests("a", 2, 2)),
            Err(TerminalResourceError::UnknownTerminal)
        );
    }

    #[test]
    fn ttl_is_monotonic_and_mutations_advance_registry_revision() {
        let now = Instant::now();
        let mut registry = registry();
        registry
            .replace_client_interests(interests("a", 1, 1))
            .unwrap();
        registry.replace(replacement(1, 1, 1), now).unwrap();
        let published_revision = registry.registry_revision();
        assert_eq!(
            registry
                .resources("term_1", "telemetry", now)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            registry.expire(now + Duration::from_millis(500)).unwrap(),
            1
        );
        assert!(registry.registry_revision() > published_revision);
        assert!(registry
            .resources("term_1", "telemetry", now + Duration::from_millis(500))
            .unwrap()
            .is_empty());
    }
}
