//! Non-publishing Collections package candidate scheduler and audit cache.
//!
//! The App actor only captures authority and validates completions. Component invocation and all
//! waits happen on the bounded package worker in `extension_candidate`.

use std::collections::{BTreeMap, HashMap};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::{Duration, Instant};

use super::App;
use crate::collections_extension_contract::{
    CollectionsActionDeclarationsV1, CollectionsDocumentProfileV1, CollectionsProductionPinV1,
    CollectionsProfileContainerV1, CollectionsProfileTopologyV1, CollectionsTerminalPlacementV1,
    COLLECTIONS_ACTIONS_LIST_METHOD_V1, COLLECTIONS_PROFILE_RENDER_METHOD_V1,
};
use crate::extension_host_invocation::{
    CompositeSnapshotObservation, HostReadInvocationAuthorization, HostReadInvocationPurpose,
    TrustedTerminalSnapshotObservations,
};
use crate::extension_host_state::HostProviderBinding;
use crate::host_document_v2::{
    host_state_snapshot_dependency, terminal_snapshot_dependency, DocumentProducerFenceV2,
    DocumentScopeV2, HostDocumentRenderParamsV2, HOST_DOCUMENT_RENDER_CONTRACT_V2,
};
use crate::quarantined_audit_cache::{AuditCacheKey, AuditTicket, QuarantinedAuditCache};
use crate::workspace::TabId;

const SERVICE_ID: &str = "collections";
const INVOCATION_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_PENDING_CANDIDATE_SCOPES: usize = crate::quarantined_audit_cache::MAX_AUDIT_AUTHORITIES;

#[derive(Clone, Debug, PartialEq, Eq)]
struct DesiredObservation {
    artifact_generation: u64,
    runtime_generation: u64,
    state_store_generation: u64,
    state_revision: u64,
    state_values: BTreeMap<String, serde_json::Value>,
    terminal_fingerprint: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum CandidateAuditKind {
    Profile,
    Actions,
}

enum CandidateAuditTicket {
    Profile(AuditTicket),
    Actions(AuditTicket),
}

enum ValidatedCandidate {
    Profile(Box<CollectionsDocumentProfileV1>),
    Actions(CollectionsActionDeclarationsV1),
}

struct PendingCandidate {
    key: AuditCacheKey,
    kind: CandidateAuditKind,
    ticket: CandidateAuditTicket,
    authorization: HostReadInvocationAuthorization,
    observer: TrustedTerminalSnapshotObservations,
    params: HostDocumentRenderParamsV2,
    topology: CollectionsProfileTopologyV1,
    expected_state: crate::extension_host_state::HostStateSnapshotV1,
    expected_terminals: crate::extension_host_terminals::HostTerminalsSnapshotV1,
    desired: DesiredObservation,
    deadline: Instant,
    canceled: Arc<AtomicBool>,
}

pub(crate) struct CollectionsShadowOverlay {
    state: crate::extension_state::DurableExtensionStateV1,
    imported_values: BTreeMap<String, serde_json::Value>,
}

impl Default for CollectionsShadowOverlay {
    fn default() -> Self {
        Self {
            state: crate::extension_state::DurableExtensionStateV1::new(
                "collections_shadow_overlay".into(),
                1,
            )
            .expect("static Collections shadow store identity is valid"),
            imported_values: BTreeMap::new(),
        }
    }
}

impl CollectionsShadowOverlay {
    pub(crate) fn state(&self) -> &crate::extension_state::DurableExtensionStateV1 {
        &self.state
    }

    pub(crate) fn imported_values(&self) -> &BTreeMap<String, serde_json::Value> {
        &self.imported_values
    }

    fn rebuild(
        &mut self,
        durable: &crate::extension_state::DurableExtensionStateV1,
        migration_epoch: u64,
        mut values: BTreeMap<String, serde_json::Value>,
    ) -> Result<bool, String> {
        values.insert(
            "shadow_import".into(),
            serde_json::json!({"schema": "vqro.collections.shadow-import", "schema_version": 1}),
        );
        let state = build_collections_import_state(durable, &values, migration_epoch)?;
        if values == self.imported_values && state == self.state {
            return Ok(false);
        }
        self.state = state;
        self.imported_values = values;
        Ok(true)
    }
}

pub(crate) struct CollectionsCandidateWork {
    attempt_id: u64,
    method: &'static str,
    authorization: HostReadInvocationAuthorization,
    params: serde_json::Value,
    deadline: Instant,
    canceled: Arc<AtomicBool>,
}

#[derive(Default)]
pub(crate) struct CollectionsCandidateCoordinator {
    profile_cache: QuarantinedAuditCache<CollectionsDocumentProfileV1>,
    action_cache: QuarantinedAuditCache<CollectionsActionDeclarationsV1>,
    profile_topologies: BTreeMap<AuditCacheKey, CollectionsProfileTopologyV1>,
    pending: HashMap<u64, PendingCandidate>,
    pending_by_scope: HashMap<(String, CandidateAuditKind), u64>,
    profile_desired_by_scope: HashMap<String, DesiredObservation>,
    action_desired_by_scope: HashMap<String, DesiredObservation>,
    next_attempt_id: u64,
}

impl CollectionsCandidateCoordinator {
    #[allow(clippy::too_many_arguments)]
    fn begin(
        &mut self,
        app: &mut App,
        binding: &HostProviderBinding,
        workspace_id: String,
        tab_id: TabId,
        artifact_generation: u64,
        runtime_generation: u64,
        requested_revision: u64,
        kind: CandidateAuditKind,
    ) -> Result<Option<CollectionsCandidateWork>, String> {
        validate_exact_candidate(app, binding, artifact_generation)?;
        let record = &app.marketplace_state.packages.packages[binding.package_id()];
        let service_generation = artifact_generation;
        let provider_generation = record.revision;
        let canceled = Arc::new(AtomicBool::new(false));
        let deadline = Instant::now() + INVOCATION_TIMEOUT;
        let authorization = app.stage_host_read_authorization(
            binding,
            HostReadInvocationPurpose {
                state: if app.collections_authority.phase
                    == crate::collections_extension_contract::CollectionsAuthorityPhaseV1::Package
                {
                    crate::extension_host_invocation::HostStateReadSource::Durable
                } else {
                    crate::extension_host_invocation::HostStateReadSource::CollectionsShadow
                },
                terminals: Some((workspace_id.clone(), tab_id.clone())),
                provisional_container: None,
            },
            service_generation,
            provider_generation,
            runtime_generation,
        )?;
        let expected_state = current_state_snapshot(app, &authorization)?;
        let expected_terminals =
            current_terminal_snapshot(app, &authorization, &workspace_id, &tab_id)?;
        let desired = DesiredObservation {
            artifact_generation,
            runtime_generation,
            state_store_generation: expected_state.store_generation,
            state_revision: expected_state.revision,
            state_values: expected_state.values.clone(),
            terminal_fingerprint: expected_terminals.fingerprint_sha256.clone(),
        };
        let scope_id = tab_id.to_string();
        let already_desired = match kind {
            CandidateAuditKind::Profile => self.profile_desired_by_scope.get(&scope_id),
            CandidateAuditKind::Actions => self.action_desired_by_scope.get(&scope_id),
        } == Some(&desired);
        if already_desired {
            return Ok(None);
        }

        let state_dependency = host_state_snapshot_dependency(
            &expected_state.contract,
            &expected_state.store_id,
            expected_state.store_generation,
            &expected_state.namespace,
            expected_state.revision,
        )
        .map_err(|error| error.to_string())?;
        let terminals_dependency = terminal_snapshot_dependency(
            &scope_id,
            &expected_terminals.fingerprint_sha256,
            provider_generation,
        )
        .map_err(|error| error.to_string())?;
        let producer = DocumentProducerFenceV2 {
            package_id: binding.package_id().into(),
            service_id: binding.service_id().into(),
            artifact_sha256: binding.artifact_sha256().into(),
            runtime_generation,
            provider_generation,
            scope_generation: provider_generation,
        };
        let scope = DocumentScopeV2 {
            contract: crate::collections_extension_contract::COLLECTIONS_SCOPE_CONTRACT_V1.into(),
            scope_id: scope_id.clone(),
        };
        let params = HostDocumentRenderParamsV2 {
            contract: HOST_DOCUMENT_RENDER_CONTRACT_V2.into(),
            producer: producer.clone(),
            scope: scope.clone(),
            requested_revision,
        };
        params.validate().map_err(|error| error.to_string())?;
        let pane_ids = app
            .state
            .workspaces
            .iter()
            .find(|workspace| workspace.id == workspace_id)
            .and_then(|workspace| workspace.tabs.iter().find(|tab| tab.id == tab_id))
            .ok_or("candidate topology scope disappeared")?
            .panes
            .iter()
            .map(|(pane_id, pane)| {
                (
                    pane.attached_terminal_id.clone(),
                    format!("pane_{}", pane_id.raw()),
                )
            })
            .collect::<HashMap<_, _>>();
        let topology = topology_from_snapshot(
            &expected_terminals,
            &pane_ids,
            producer.clone(),
            state_dependency.clone(),
            terminals_dependency.clone(),
        )?;
        let mut dependencies = vec![state_dependency, terminals_dependency];
        dependencies.sort();
        let key = AuditCacheKey {
            producer: producer.producer(),
            scope,
        };
        let pending_key = (scope_id.clone(), kind);
        if let Some(older_attempt) = self.pending_by_scope.remove(&pending_key) {
            if let Some(older) = self.pending.remove(&older_attempt) {
                older.canceled.store(true, Ordering::Release);
                self.fail_ticket(older.ticket);
            }
        } else if self.pending.len() >= MAX_PENDING_CANDIDATE_SCOPES * 2 {
            return Err("Collections candidate pending capacity reached".into());
        }
        let ticket = match kind {
            CandidateAuditKind::Profile => {
                self.profile_cache
                    .retire_scope(&key.scope)
                    .map_err(|error| format!("cannot retire prior profile scope: {error:?}"))?;
                self.profile_topologies
                    .retain(|cache_key, _| cache_key.scope != key.scope);
                self.profile_cache
                    .update_authority(key.clone(), producer, dependencies)
                    .map_err(|error| format!("cannot install profile authority: {error:?}"))?;
                CandidateAuditTicket::Profile(
                    self.profile_cache
                        .begin_audit(&key)
                        .map_err(|error| format!("cannot issue profile audit: {error:?}"))?,
                )
            }
            CandidateAuditKind::Actions => {
                self.action_cache
                    .retire_scope(&key.scope)
                    .map_err(|error| format!("cannot retire prior action scope: {error:?}"))?;
                self.action_cache
                    .update_authority(key.clone(), producer, dependencies)
                    .map_err(|error| format!("cannot install action authority: {error:?}"))?;
                CandidateAuditTicket::Actions(
                    self.action_cache
                        .begin_audit(&key)
                        .map_err(|error| format!("cannot issue action audit: {error:?}"))?,
                )
            }
        };
        let attempt_id = self
            .next_attempt_id
            .checked_add(1)
            .ok_or("Collections candidate attempt sequence exhausted")?;
        let observer = authorization.terminal_observations();
        let encoded_params = serde_json::to_value(&params)
            .map_err(|error| format!("cannot encode candidate request: {error}"))?;
        self.next_attempt_id = attempt_id;
        match kind {
            CandidateAuditKind::Profile => {
                self.profile_desired_by_scope
                    .insert(scope_id.clone(), desired.clone());
            }
            CandidateAuditKind::Actions => {
                self.action_desired_by_scope
                    .insert(scope_id.clone(), desired.clone());
            }
        }
        self.pending_by_scope.insert(pending_key, attempt_id);
        self.pending.insert(
            attempt_id,
            PendingCandidate {
                key,
                kind,
                ticket,
                authorization: authorization.clone(),
                observer,
                params,
                topology,
                expected_state,
                expected_terminals,
                desired,
                deadline,
                canceled: Arc::clone(&canceled),
            },
        );
        Ok(Some(CollectionsCandidateWork {
            attempt_id,
            method: match kind {
                CandidateAuditKind::Profile => COLLECTIONS_PROFILE_RENDER_METHOD_V1,
                CandidateAuditKind::Actions => COLLECTIONS_ACTIONS_LIST_METHOD_V1,
            },
            authorization,
            params: encoded_params,
            deadline,
            canceled,
        }))
    }

    fn finalize(
        &mut self,
        app: &App,
        attempt_id: u64,
        result: Result<serde_json::Value, String>,
    ) -> Result<(), String> {
        let pending = self
            .pending
            .remove(&attempt_id)
            .ok_or("Collections candidate attempt is absent, stale, or replayed")?;
        self.pending_by_scope
            .remove(&(pending.params.scope.scope_id.clone(), pending.kind));
        let outcome = self.validate_completion(app, &pending, result);
        match (pending.ticket, outcome) {
            (CandidateAuditTicket::Profile(ticket), Ok(ValidatedCandidate::Profile(profile))) => {
                self.profile_cache
                    .accept(ticket, *profile)
                    .map_err(|error| format!("candidate cache rejected profile: {error:?}"))?;
                self.profile_topologies
                    .retain(|key, _| self.profile_cache.get(key).is_some());
                self.profile_topologies
                    .insert(pending.key.clone(), pending.topology.clone());
                Ok(())
            }
            (CandidateAuditTicket::Actions(ticket), Ok(ValidatedCandidate::Actions(actions))) => {
                self.action_cache
                    .accept(ticket, actions)
                    .map_err(|error| format!("candidate cache rejected actions: {error:?}"))
            }
            (ticket, Ok(_)) => {
                self.fail_ticket(ticket);
                self.clear_desired(
                    pending.kind,
                    &pending.params.scope.scope_id,
                    &pending.desired,
                );
                Err("candidate audit result kind changed".into())
            }
            (ticket, Err(error)) => {
                self.fail_ticket(ticket);
                self.clear_desired(
                    pending.kind,
                    &pending.params.scope.scope_id,
                    &pending.desired,
                );
                Err(error)
            }
        }
    }

    fn validate_completion(
        &self,
        app: &App,
        pending: &PendingCandidate,
        result: Result<serde_json::Value, String>,
    ) -> Result<ValidatedCandidate, String> {
        if pending.canceled.load(Ordering::Acquire) || Instant::now() >= pending.deadline {
            return Err("Collections candidate invocation was canceled or expired".into());
        }
        validate_authorization(app, &pending.authorization)?;
        let current_state = current_state_snapshot(app, &pending.authorization)?;
        let current_terminals = current_terminal_snapshot(
            app,
            &pending.authorization,
            &pending.expected_terminals.workspace_id,
            &pending.expected_terminals.tab_id,
        )?;
        let CompositeSnapshotObservation { state, terminals } = pending
            .observer
            .exact_state_then_terminals()
            .map_err(|error| format!("candidate host-read trace rejected: {error:?}"))?;
        if state != pending.expected_state
            || current_state != pending.expected_state
            || terminals != pending.expected_terminals
            || current_terminals != pending.expected_terminals
        {
            return Err("candidate captured, observed, and current snapshots differ".into());
        }
        let value = result.map_err(|error| format!("candidate runtime failed: {error}"))?;
        match pending.kind {
            CandidateAuditKind::Profile => {
                let profile: CollectionsDocumentProfileV1 = serde_json::from_value(value)
                    .map_err(|error| format!("candidate returned malformed profile: {error}"))?;
                profile
                    .validate(&pending.topology)
                    .map_err(|error| format!("candidate profile rejected: {error}"))?;
                if profile.document.producer != pending.params.producer
                    || profile.document.scope != pending.params.scope
                    || profile.document.revision != pending.params.requested_revision
                {
                    return Err("candidate profile did not echo host authority exactly".into());
                }
                Ok(ValidatedCandidate::Profile(Box::new(profile)))
            }
            CandidateAuditKind::Actions => {
                let actions: CollectionsActionDeclarationsV1 = serde_json::from_value(value)
                    .map_err(|error| format!("candidate returned malformed actions: {error}"))?;
                let profile = self
                    .profile_cache
                    .get(&pending.key)
                    .ok_or("candidate actions completed before an audited profile")?;
                actions
                    .validate_for(profile, &pending.topology)
                    .map_err(|error| format!("candidate actions rejected: {error}"))?;
                Ok(ValidatedCandidate::Actions(actions))
            }
        }
    }

    fn fail_submission(&mut self, attempt_id: u64, error: String) -> Result<(), String> {
        let pending = self
            .pending
            .remove(&attempt_id)
            .ok_or("Collections candidate submission is no longer pending")?;
        self.pending_by_scope
            .remove(&(pending.params.scope.scope_id.clone(), pending.kind));
        self.clear_desired(
            pending.kind,
            &pending.params.scope.scope_id,
            &pending.desired,
        );
        self.fail_ticket(pending.ticket);
        Err(error)
    }

    fn fail_ticket(&mut self, ticket: CandidateAuditTicket) {
        match ticket {
            CandidateAuditTicket::Profile(ticket) => {
                let _ = self.profile_cache.fail(ticket);
            }
            CandidateAuditTicket::Actions(ticket) => {
                let _ = self.action_cache.fail(ticket);
            }
        }
    }

    fn clear_desired(
        &mut self,
        kind: CandidateAuditKind,
        scope_id: &str,
        expected: &DesiredObservation,
    ) {
        let desired = match kind {
            CandidateAuditKind::Profile => &mut self.profile_desired_by_scope,
            CandidateAuditKind::Actions => &mut self.action_desired_by_scope,
        };
        if desired.get(scope_id) == Some(expected) {
            desired.remove(scope_id);
        }
    }

    #[cfg(test)]
    pub(crate) fn cached_len(&self) -> usize {
        self.profile_cache.len()
    }

    #[cfg(test)]
    pub(crate) fn action_cached_len(&self) -> usize {
        self.action_cache.len()
    }

    #[cfg(test)]
    pub(crate) fn pending_len(&self) -> usize {
        self.pending.len()
    }

    pub(crate) fn profiles(&self) -> impl Iterator<Item = &CollectionsDocumentProfileV1> {
        self.profile_cache.values()
    }

    pub(crate) fn actions_for(
        &self,
        profile: &CollectionsDocumentProfileV1,
    ) -> Option<&CollectionsActionDeclarationsV1> {
        self.action_cache.get(&AuditCacheKey {
            producer: profile.document.producer.producer(),
            scope: profile.document.scope.clone(),
        })
    }

    pub(crate) fn topology_for(
        &self,
        profile: &CollectionsDocumentProfileV1,
    ) -> Option<&CollectionsProfileTopologyV1> {
        self.profile_topologies.get(&AuditCacheKey {
            producer: profile.document.producer.producer(),
            scope: profile.document.scope.clone(),
        })
    }

    pub(crate) fn publish_checkpointed_create(
        &mut self,
        profile: CollectionsDocumentProfileV1,
        actions: CollectionsActionDeclarationsV1,
        topology: CollectionsProfileTopologyV1,
    ) -> Result<(), String> {
        let key = AuditCacheKey {
            producer: profile.document.producer.producer(),
            scope: profile.document.scope.clone(),
        };
        let scope_id = key.scope.scope_id.clone();
        for kind in [CandidateAuditKind::Profile, CandidateAuditKind::Actions] {
            if let Some(attempt_id) = self.pending_by_scope.remove(&(scope_id.clone(), kind)) {
                if let Some(pending) = self.pending.remove(&attempt_id) {
                    pending.canceled.store(true, Ordering::Release);
                    self.fail_ticket(pending.ticket);
                }
            }
        }
        self.profile_desired_by_scope.remove(&scope_id);
        self.action_desired_by_scope.remove(&scope_id);

        let mut dependencies = profile.document.dependencies.clone();
        dependencies.sort();
        let producer = profile.document.producer.clone();
        self.profile_cache
            .retire_scope(&key.scope)
            .map_err(|error| format!("cannot retire checkpointed profile scope: {error:?}"))?;
        self.action_cache
            .retire_scope(&key.scope)
            .map_err(|error| format!("cannot retire checkpointed action scope: {error:?}"))?;
        self.profile_topologies
            .retain(|cache_key, _| cache_key.scope != key.scope);
        self.profile_cache
            .update_authority(key.clone(), producer.clone(), dependencies.clone())
            .map_err(|error| format!("cannot install checkpointed profile authority: {error:?}"))?;
        self.action_cache
            .update_authority(key.clone(), producer, dependencies)
            .map_err(|error| format!("cannot install checkpointed action authority: {error:?}"))?;
        let action_ticket = self
            .action_cache
            .begin_audit(&key)
            .map_err(|error| format!("cannot issue checkpointed action audit: {error:?}"))?;
        let profile_ticket = self
            .profile_cache
            .begin_audit(&key)
            .map_err(|error| format!("cannot issue checkpointed profile audit: {error:?}"))?;
        self.action_cache
            .accept(action_ticket, actions)
            .map_err(|error| format!("cannot publish checkpointed actions: {error:?}"))?;
        self.profile_cache
            .accept(profile_ticket, profile)
            .map_err(|error| format!("cannot publish checkpointed profile: {error:?}"))?;
        self.profile_topologies
            .retain(|cache_key, _| self.profile_cache.get(cache_key).is_some());
        self.profile_topologies.insert(key, topology);
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn first_profile(&self) -> Option<&CollectionsDocumentProfileV1> {
        self.profile_cache.first_value()
    }

    #[cfg(test)]
    pub(crate) fn first_actions(&self) -> Option<&CollectionsActionDeclarationsV1> {
        self.action_cache.first_value()
    }
}

pub(super) fn build_collections_import_state(
    durable: &crate::extension_state::DurableExtensionStateV1,
    values: &BTreeMap<String, serde_json::Value>,
    migration_epoch: u64,
) -> Result<crate::extension_state::DurableExtensionStateV1, String> {
    if values.len() > crate::extension_state::MAX_STATE_KEYS_PER_NAMESPACE {
        return Err("legacy Collections policy exceeds shadow state capacity".into());
    }
    let mut state = durable.clone();
    state
        .clear_namespace(
            crate::collections_extension_contract::COLLECTIONS_NAMESPACE,
            format!("collections-cutover-clear-{migration_epoch}"),
        )
        .map_err(|error| format!("cannot clear pre-cutover package state: {error:?}"))?;
    let operations = values
        .iter()
        .map(
            |(key, value)| crate::extension_state::ExtensionStateOperationV1 {
                key: key.clone(),
                value: Some(value.clone()),
            },
        )
        .collect::<Vec<_>>();
    let mut expected_revision = state
        .snapshot(crate::collections_extension_contract::COLLECTIONS_NAMESPACE)
        .map_err(|error| format!("cannot inspect package state: {error:?}"))?
        .revision;
    for (chunk_index, chunk) in operations
        .chunks(crate::extension_state::MAX_STATE_OPERATIONS)
        .enumerate()
    {
        let commit = state
            .transact(&crate::extension_state::ExtensionStateTransactionV1 {
                contract: crate::extension_state::EXTENSION_STATE_CONTRACT_V1.into(),
                namespace: crate::collections_extension_contract::COLLECTIONS_NAMESPACE.into(),
                expected_store_generation: state.store_generation,
                expected_revision,
                idempotency_key: format!("collections-cutover-{migration_epoch}-{chunk_index}"),
                operations: chunk.to_vec(),
            })
            .map_err(|error| format!("cannot import Collections package state: {error:?}"))?;
        expected_revision = commit.revision;
    }
    Ok(state)
}

impl App {
    pub(crate) fn rebuild_collections_shadow_overlay(&mut self) -> Result<bool, String> {
        let mut values = BTreeMap::new();
        for workspace in &self.state.workspaces {
            for tab in &workspace.tabs {
                for collection in tab.collections() {
                    let container_id = crate::terminal_container::format_terminal_container_id(
                        crate::terminal_container::TerminalContainerId::from(collection.id()),
                    );
                    let mut archived_terminal_ids = collection
                        .archived_members()
                        .map(|pane_id| {
                            tab.terminal_id(pane_id)
                                .map(ToString::to_string)
                                .ok_or("legacy Collection member has no terminal identity")
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    archived_terminal_ids.sort();
                    let mut record = serde_json::Map::from_iter([
                        (
                            "archived_terminal_ids".into(),
                            serde_json::to_value(archived_terminal_ids)
                                .map_err(|error| error.to_string())?,
                        ),
                        (
                            "container_id".into(),
                            serde_json::Value::String(container_id.clone()),
                        ),
                        (
                            "schema".into(),
                            serde_json::Value::String("vqro.collections.policy".into()),
                        ),
                        ("schema_version".into(), serde_json::Value::from(1)),
                    ]);
                    if let Some(label) = collection.label() {
                        record.insert("label".into(), serde_json::Value::String(label.into()));
                    }
                    if values
                        .insert(container_id, serde_json::Value::Object(record))
                        .is_some()
                    {
                        return Err(
                            "legacy Collections contain a duplicate container identity".into()
                        );
                    }
                }
            }
        }
        let migration_epoch = self
            .collections_authority
            .authority_epoch
            .checked_add(1)
            .ok_or("Collections authority epoch exhausted")?;
        self.collections_shadow_overlay
            .rebuild(&self.extension_state, migration_epoch, values)
    }

    pub(crate) fn schedule_collections_candidate(
        &mut self,
        workspace_id: String,
        tab_id: TabId,
        requested_revision: u64,
    ) -> Result<bool, String> {
        let (binding, artifact_generation, runtime_generation) = {
            let slot = self
                .extension_package_services
                .get(crate::collections_extension_contract::COLLECTIONS_PACKAGE_ID)
                .filter(|slot| slot.is_ready())
                .ok_or("Collections package candidate is not ready")?;
            let binding = slot
                .binding(SERVICE_ID)
                .cloned()
                .ok_or("Collections candidate service binding is absent")?;
            let artifact_generation = self
                .marketplace_state
                .packages
                .packages
                .get(binding.package_id())
                .and_then(|record| record.active.as_ref())
                .map(|active| active.runtime_generation)
                .ok_or("Collections candidate artifact is not active")?;
            let runtime_generation = slot
                .runtime_generation(SERVICE_ID)
                .ok_or("Collections candidate runtime generation is absent")?;
            (binding, artifact_generation, runtime_generation)
        };
        let mut submitted = false;
        for kind in [CandidateAuditKind::Profile, CandidateAuditKind::Actions] {
            let mut coordinator = std::mem::take(&mut self.collections_candidate_audits);
            let work = coordinator.begin(
                self,
                &binding,
                workspace_id.clone(),
                tab_id.clone(),
                artifact_generation,
                runtime_generation,
                requested_revision,
                kind,
            );
            self.collections_candidate_audits = coordinator;
            let Some(work) = work? else {
                continue;
            };
            let submission = self
                .extension_package_services
                .get(binding.package_id())
                .ok_or("Collections candidate was retired before submission")?
                .invoke(
                    SERVICE_ID,
                    work.attempt_id,
                    work.method.into(),
                    work.params,
                    work.authorization,
                    work.deadline,
                    work.canceled,
                );
            if let Err(error) = submission {
                let mut coordinator = std::mem::take(&mut self.collections_candidate_audits);
                let rejected = coordinator.fail_submission(work.attempt_id, error);
                self.collections_candidate_audits = coordinator;
                return rejected.map(|()| false);
            }
            submitted = true;
        }
        Ok(submitted)
    }

    pub(crate) fn finalize_collections_candidate(
        &mut self,
        attempt_id: u64,
        result: Result<serde_json::Value, String>,
    ) -> Result<(), String> {
        let mut coordinator = std::mem::take(&mut self.collections_candidate_audits);
        let outcome = coordinator.finalize(self, attempt_id, result);
        self.collections_candidate_audits = coordinator;
        if outcome.is_ok() {
            self.maybe_cutover_collections_authority()?;
            self.publish_ready_collections_package_profiles();
        }
        outcome
    }
}

pub(crate) fn production_pin() -> Result<CollectionsProductionPinV1, String> {
    crate::collections_extension_contract::embedded_collections_production_pin()
}

fn validate_exact_candidate(
    app: &App,
    binding: &HostProviderBinding,
    artifact_generation: u64,
) -> Result<(), String> {
    let pin = production_pin()?;
    let record = app
        .marketplace_state
        .packages
        .packages
        .get(&pin.package_id)
        .ok_or("Collections candidate package is not registered")?;
    let active = record
        .active
        .as_ref()
        .ok_or("Collections candidate package has no active artifact")?;
    let exact_source = matches!(&record.source,
        crate::extension_package::PackageProvenanceV1::Git {
            url,
            source_commit,
            package_path,
            package_id,
            version: _,
            package_sha256,
            manifest_sha256,
        } if url == &pin.source_url
            && source_commit == &pin.source_commit
            && package_path == &pin.package_path
            && package_id == &pin.package_id
            && package_sha256 == &pin.package_sha256
            && manifest_sha256 == &pin.manifest_sha256);
    if !exact_source
        || record.status != crate::extension_package::PackageLifecycleStatusV1::Enabled
        || record.namespaces != [pin.package_id.as_str()]
        || active.package_sha256 != pin.package_sha256
        || active.runtime_generation != artifact_generation
        || binding.package_id() != pin.package_id
        || binding.service_id() != pin.service_id
        || binding.namespace() != pin.package_id
        || binding.artifact_sha256() != pin.package_sha256
        || !binding
            .capabilities()
            .contains(crate::extension_host_state::HOST_STATE_READ_CAPABILITY)
        || !binding
            .capabilities()
            .contains(crate::extension_host_terminals::HOST_TERMINALS_READ_CAPABILITY)
        || binding.capabilities().len() != 2
    {
        return Err("Collections candidate does not match the frozen production pin".into());
    }
    Ok(())
}

fn validate_authorization(
    app: &App,
    authorization: &HostReadInvocationAuthorization,
) -> Result<(), String> {
    let record = app
        .marketplace_state
        .packages
        .packages
        .get(&authorization.package_id)
        .ok_or("authorized package is not registered")?;
    let active = record
        .active
        .as_ref()
        .ok_or("authorized package has no active artifact")?;
    let current_authority = app
        .host_invocation_authorities
        .get(&(
            authorization.package_id.clone(),
            authorization.service_id.clone(),
        ))
        .is_some_and(|authority| Arc::ptr_eq(authority, &authorization.authority));
    if app.marketplace_state.packages.revision != authorization.catalog_generation
        || record.revision != authorization.package_generation
        || record.status != crate::extension_package::PackageLifecycleStatusV1::Enabled
        || active.package_sha256 != authorization.artifact_sha256
        || active.version != authorization.artifact_version
        || active.runtime_generation != authorization.artifact_generation
        || !current_authority
        || !authorization.authority.permits_generations(
            authorization.service_generation,
            authorization.provider_generation,
            authorization.runtime_generation,
        )
    {
        return Err("Collections candidate authorization is stale or mismatched".into());
    }
    Ok(())
}

pub(super) fn topology_from_snapshot(
    snapshot: &crate::extension_host_terminals::HostTerminalsSnapshotV1,
    pane_ids: &HashMap<crate::terminal::TerminalId, String>,
    producer: DocumentProducerFenceV2,
    state_dependency: crate::host_document_v2::DocumentDependencyV2,
    terminals_dependency: crate::host_document_v2::DocumentDependencyV2,
) -> Result<CollectionsProfileTopologyV1, String> {
    let mut containers = Vec::new();
    let mut placements = Vec::new();
    for item in &snapshot.layout {
        let crate::extension_host_terminals::HostTerminalLayoutItemV1::Container {
            container_id,
            terminal_ids,
            selected_terminal_id,
        } = item
        else {
            continue;
        };
        containers.push(CollectionsProfileContainerV1 {
            container_id: container_id.clone(),
            terminal_ids: terminal_ids.iter().map(ToString::to_string).collect(),
            selected_terminal_id: selected_terminal_id.as_ref().map(ToString::to_string),
        });
        for terminal_id in terminal_ids {
            placements.push(CollectionsTerminalPlacementV1 {
                terminal_id: terminal_id.to_string(),
                pane_id: pane_ids
                    .get(terminal_id)
                    .cloned()
                    .ok_or("candidate topology terminal has no pane placement")?,
            });
        }
    }
    Ok(CollectionsProfileTopologyV1 {
        workspace_id: snapshot.workspace_id.clone(),
        tab_id: snapshot.tab_id.to_string(),
        containers,
        placements,
        producer,
        state_dependency,
        terminals_dependency,
    })
}

pub(super) fn current_terminal_snapshot(
    app: &App,
    authorization: &HostReadInvocationAuthorization,
    workspace_id: &str,
    tab_id: &TabId,
) -> Result<crate::extension_host_terminals::HostTerminalsSnapshotV1, String> {
    let current_authority = app
        .host_invocation_authorities
        .get(&(
            authorization.package_id.clone(),
            authorization.service_id.clone(),
        ))
        .is_some_and(|authority| Arc::ptr_eq(authority, &authorization.authority));
    if !current_authority {
        return Err("Collections candidate authority was superseded".into());
    }
    let terminals = authorization
        .terminals
        .as_ref()
        .ok_or("terminal authorization is absent")?;
    let (workspace, tab) = app
        .state
        .workspaces
        .iter()
        .find(|workspace| workspace.id == workspace_id)
        .and_then(|workspace| {
            workspace
                .tabs
                .iter()
                .find(|tab| &tab.id == tab_id)
                .map(|tab| (workspace, tab))
        })
        .ok_or("exact workspace/tab scope disappeared")?;
    let mut snapshot = crate::extension_host_terminals::build_snapshot(
        workspace,
        tab,
        &app.state.terminals,
        terminals.fence.clone(),
    )
    .map_err(|error| error.message)?;
    if let Some(container_id) = &terminals.provisional_container {
        crate::extension_host_terminals::append_provisional_container(&mut snapshot, container_id)
            .map_err(|error| error.message)?;
    }
    Ok(snapshot)
}

pub(super) fn current_state_snapshot(
    app: &App,
    authorization: &HostReadInvocationAuthorization,
) -> Result<crate::extension_host_state::HostStateSnapshotV1, String> {
    let state = authorization
        .state
        .as_ref()
        .ok_or("state authorization is absent")?;
    let source = match state.source {
        crate::extension_host_invocation::HostStateReadSource::CollectionsShadow => {
            app.collections_shadow_overlay.state()
        }
        crate::extension_host_invocation::HostStateReadSource::Durable => &app.extension_state,
        crate::extension_host_invocation::HostStateReadSource::None => {
            return Err("state authorization source is absent".into());
        }
    };
    if state.namespace != authorization.namespace
        || state.store_id != source.store_id
        || state.store_generation != source.store_generation
    {
        return Err("state authorization store or namespace changed".into());
    }
    source
        .snapshot(&state.namespace)
        .map(Into::into)
        .map_err(|error| format!("cannot capture current state snapshot: {error:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checkpointed_create_publishes_profile_actions_and_topology_together() {
        let profile: CollectionsDocumentProfileV1 = serde_json::from_str(include_str!(
            "../../../docs/next/api/vqro-collections-v1.fixtures/document-profile-valid.json"
        ))
        .unwrap();
        let actions: CollectionsActionDeclarationsV1 = serde_json::from_str(include_str!(
            "../../../docs/next/api/vqro-collections-v1.fixtures/actions-valid.json"
        ))
        .unwrap();
        let topology = CollectionsProfileTopologyV1 {
            workspace_id: "w1".into(),
            tab_id: profile.document.scope.scope_id.clone(),
            containers: vec![CollectionsProfileContainerV1 {
                container_id: "container_000000000000002a".into(),
                terminal_ids: vec![
                    "term_11111111111111111111111111111111".into(),
                    "term_22222222222222222222222222222222".into(),
                ],
                selected_terminal_id: Some("term_11111111111111111111111111111111".into()),
            }],
            placements: vec![
                CollectionsTerminalPlacementV1 {
                    terminal_id: "term_11111111111111111111111111111111".into(),
                    pane_id: "pane_1".into(),
                },
                CollectionsTerminalPlacementV1 {
                    terminal_id: "term_22222222222222222222222222222222".into(),
                    pane_id: "pane_2".into(),
                },
            ],
            producer: profile.document.producer.clone(),
            state_dependency: profile.document.dependencies[0].clone(),
            terminals_dependency: profile.document.dependencies[1].clone(),
        };
        let mut coordinator = CollectionsCandidateCoordinator::default();

        coordinator
            .publish_checkpointed_create(profile.clone(), actions.clone(), topology.clone())
            .unwrap();

        assert_eq!(coordinator.first_profile(), Some(&profile));
        assert_eq!(coordinator.first_actions(), Some(&actions));
        assert_eq!(coordinator.actions_for(&profile), Some(&actions));
        assert_eq!(coordinator.topology_for(&profile), Some(&topology));

        let mut replacement_profile = profile.clone();
        replacement_profile.document.producer.runtime_generation += 1;
        replacement_profile.document.revision += 1;
        let mut replacement_actions = actions.clone();
        replacement_actions.document_revision = replacement_profile.document.revision;
        let mut replacement_topology = topology.clone();
        replacement_topology.producer = replacement_profile.document.producer.clone();
        coordinator
            .publish_checkpointed_create(
                replacement_profile.clone(),
                replacement_actions.clone(),
                replacement_topology.clone(),
            )
            .unwrap();

        assert_eq!(coordinator.cached_len(), 1);
        assert_eq!(coordinator.action_cached_len(), 1);
        assert_eq!(coordinator.first_profile(), Some(&replacement_profile));
        assert_eq!(coordinator.first_actions(), Some(&replacement_actions));
        assert_eq!(
            coordinator.actions_for(&replacement_profile),
            Some(&replacement_actions)
        );
        assert_eq!(
            coordinator.topology_for(&replacement_profile),
            Some(&replacement_topology)
        );
    }

    #[test]
    fn legacy_import_is_rebuildable_nonpersistent_and_change_sensitive() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        let durable_before = app.extension_state.clone();
        let mut workspace = crate::workspace::Workspace::test_new("shadow-import");
        let root = workspace.tabs[0].layout.pane_ids()[0];
        let member = workspace.test_split(ratatui::layout::Direction::Vertical);
        let collection_id = workspace.tabs[0]
            .create_collection_near(
                crate::layout::LayoutLeaf::Pane(root),
                ratatui::layout::Direction::Vertical,
                0.5,
                Some(String::new()),
            )
            .unwrap();
        workspace.tabs[0]
            .collect_tiled_pane(member, collection_id)
            .unwrap();
        let terminal_id = workspace.tabs[0].terminal_id(member).unwrap().to_string();
        let container_id = crate::terminal_container::format_terminal_container_id(
            crate::terminal_container::TerminalContainerId::from(collection_id),
        );
        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();

        assert!(app.rebuild_collections_shadow_overlay().unwrap());
        let first = app
            .collections_shadow_overlay
            .state()
            .snapshot(crate::collections_extension_contract::COLLECTIONS_NAMESPACE)
            .unwrap();
        let policy = &first.values[&container_id];
        assert_eq!(policy["label"], "");
        assert_eq!(policy["archived_terminal_ids"], serde_json::json!([]));
        assert_eq!(app.extension_state, durable_before);

        assert!(!app.rebuild_collections_shadow_overlay().unwrap());
        assert_eq!(
            app.collections_shadow_overlay.state().store_generation,
            first.store_generation
        );

        app.state.workspaces[0].tabs[0]
            .set_collection_member_archived(collection_id, member, true)
            .unwrap();
        assert!(app.rebuild_collections_shadow_overlay().unwrap());
        let second = app
            .collections_shadow_overlay
            .state()
            .snapshot(crate::collections_extension_contract::COLLECTIONS_NAMESPACE)
            .unwrap();
        assert_eq!(second.store_generation, first.store_generation);
        assert_eq!(second.revision, first.revision);
        assert_eq!(
            second.values[&container_id]["archived_terminal_ids"],
            serde_json::json!([terminal_id])
        );
        assert_eq!(app.extension_state, durable_before);

        let fresh = CollectionsShadowOverlay::default();
        assert!(fresh
            .state()
            .snapshot(crate::collections_extension_contract::COLLECTIONS_NAMESPACE)
            .unwrap()
            .values
            .is_empty());
    }
}
