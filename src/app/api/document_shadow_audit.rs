//! Test-only structural shadow coordinator. Nothing in this module is compiled into production.

use std::collections::{HashMap, HashSet};
use std::sync::{atomic::AtomicBool, Arc};
use std::time::Instant;

use serde::{de::DeserializeOwned, Serialize};

use super::App;
use crate::extension_host_invocation::{
    HostReadInvocationAuthorization, HostReadInvocationPurpose, TrustedTerminalSnapshotObservations,
};
use crate::extension_host_state::HostProviderBinding;
use crate::extension_runtime::{RuntimeError, RuntimeRequestV1};
use crate::host_document_v2::{
    terminal_snapshot_dependency, DocumentDependencyV2, DocumentProducerFenceV2, DocumentScopeV2,
    HostDocumentRenderParamsV2, HostDocumentV2, TerminalScopePolicy,
    HOST_DOCUMENT_RENDER_CONTRACT_V2,
};
use crate::quarantined_audit_cache::{AuditCacheKey, AuditTicket, QuarantinedAuditCache};
use crate::workspace::TabId;

pub(crate) const COLLECTIONS_COMMIT: &str = "b590657b23866e36b07e3bbbaed5554e1be5a4fc";
pub(crate) const COLLECTIONS_PACKAGE_SHA256: &str =
    "36230691235fe70ef21b06e1d94c98d9888bce68ed4587d3c979bf03f9d2980b";
pub(crate) const COLLECTIONS_MANIFEST_SHA256: &str =
    "4d270e2646cde5764260e5781eb837c7964078731fb5981578b1d08e80957034";
pub(crate) const COLLECTIONS_REPOSITORY: &str =
    "https://github.com/ttenneb/vqro-extension-collections.git";
const CURATED_CATALOG: &str = "collections-candidate-conformance";
const CURATED_PUBLISHER: &str = "vqro";
const PACKAGE_ID: &str = "vqro.collections";
const SERVICE_ID: &str = "collections";
const NAMESPACE: &str = "vqro.collections";
const METHOD: &str = "host.document.render";

/// Values admitted by the generic cache must have the document-v2 audit semantics.
pub(crate) trait ShadowAuditDocument: Serialize + DeserializeOwned {
    fn audit_validate(
        &self,
        terminals: &HashSet<crate::terminal::TerminalId>,
    ) -> Result<(), String>;
    fn producer(&self) -> &DocumentProducerFenceV2;
    fn scope(&self) -> &DocumentScopeV2;
    fn revision(&self) -> u64;
    fn dependencies(&self) -> &[DocumentDependencyV2];
}

impl ShadowAuditDocument for HostDocumentV2 {
    fn audit_validate(
        &self,
        terminals: &HashSet<crate::terminal::TerminalId>,
    ) -> Result<(), String> {
        self.validate(terminals, TerminalScopePolicy::ExactCoverage)
            .map_err(|error| error.to_string())
    }
    fn producer(&self) -> &DocumentProducerFenceV2 {
        &self.producer
    }
    fn scope(&self) -> &DocumentScopeV2 {
        &self.scope
    }
    fn revision(&self) -> u64 {
        self.revision
    }
    fn dependencies(&self) -> &[DocumentDependencyV2] {
        &self.dependencies
    }
}

pub(crate) struct DocumentShadowAuditWork {
    pub(crate) authorization: HostReadInvocationAuthorization,
    pub(crate) request: RuntimeRequestV1,
    pub(crate) observer: TrustedTerminalSnapshotObservations,
    pub(crate) attempt_id: u64,
    pub(crate) deadline: Instant,
}

#[derive(Clone)]
enum ReadExpectation {
    TerminalsOnly {
        expected: crate::extension_host_terminals::HostTerminalsSnapshotV1,
        dependency: DocumentDependencyV2,
    },
    StateThenTerminals {
        expected_state: crate::extension_host_state::HostStateSnapshotV1,
        expected_terminals: crate::extension_host_terminals::HostTerminalsSnapshotV1,
        dependencies: [DocumentDependencyV2; 2],
    },
}

struct PendingAudit {
    key: AuditCacheKey,
    ticket: AuditTicket,
    authorization: HostReadInvocationAuthorization,
    read_expectation: ReadExpectation,
    validate_collections_candidate: bool,
    params: HostDocumentRenderParamsV2,
    workspace_id: String,
    tab_id: TabId,
    deadline: Instant,
    canceled: Arc<AtomicBool>,
}

impl ReadExpectation {
    fn dependencies(&self) -> &[DocumentDependencyV2] {
        match self {
            Self::TerminalsOnly { dependency, .. } => std::slice::from_ref(dependency),
            Self::StateThenTerminals { dependencies, .. } => dependencies,
        }
    }
}

enum CurrentReadValues {
    TerminalsOnly(crate::extension_host_terminals::HostTerminalsSnapshotV1),
    StateThenTerminals {
        state: crate::extension_host_state::HostStateSnapshotV1,
        terminals: crate::extension_host_terminals::HostTerminalsSnapshotV1,
    },
}

fn finalize_read_expectation(
    expectation: &ReadExpectation,
    observations: &TrustedTerminalSnapshotObservations,
    current: CurrentReadValues,
) -> Result<crate::extension_host_terminals::HostTerminalsSnapshotV1, String> {
    match (expectation, current) {
        (
            ReadExpectation::TerminalsOnly { expected, .. },
            CurrentReadValues::TerminalsOnly(current),
        ) => {
            let observed = observations
                .exactly_one()
                .map_err(|error| format!("shadow terminal observation rejected: {error:?}"))?;
            if &observed != expected || current != *expected {
                return Err("observed, captured, and current terminal snapshots differ".into());
            }
            Ok(current)
        }
        (
            ReadExpectation::StateThenTerminals {
                expected_state,
                expected_terminals,
                ..
            },
            CurrentReadValues::StateThenTerminals { state, terminals },
        ) => {
            let observed = observations
                .exact_state_then_terminals()
                .map_err(|error| format!("shadow composite observation rejected: {error:?}"))?;
            if observed.state != *expected_state || state != *expected_state {
                return Err("observed, captured, and current state snapshots differ".into());
            }
            if observed.terminals != *expected_terminals || terminals != *expected_terminals {
                return Err("observed, captured, and current terminal snapshots differ".into());
            }
            Ok(terminals)
        }
        _ => Err("shadow current reads do not match the staged expectation".into()),
    }
}

pub(crate) struct DocumentShadowAuditCoordinator<D> {
    cache: QuarantinedAuditCache<D>,
    pending: HashMap<u64, PendingAudit>,
    next_attempt_id: u64,
}

impl<D> Default for DocumentShadowAuditCoordinator<D> {
    fn default() -> Self {
        Self {
            cache: QuarantinedAuditCache::default(),
            pending: HashMap::new(),
            next_attempt_id: 0,
        }
    }
}

impl<D: ShadowAuditDocument> DocumentShadowAuditCoordinator<D> {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn begin(
        &mut self,
        app: &mut App,
        binding: &HostProviderBinding,
        workspace_id: String,
        tab_id: TabId,
        service_generation: u64,
        provider_generation: u64,
        runtime_generation: u64,
        scope_generation: u64,
        requested_revision: u64,
        deadline: Instant,
        canceled: Arc<AtomicBool>,
    ) -> Result<DocumentShadowAuditWork, String> {
        if Instant::now() >= deadline || canceled.load(std::sync::atomic::Ordering::Acquire) {
            return Err("shadow audit is canceled or expired".into());
        }
        if binding.package_id() != PACKAGE_ID
            || binding.service_id() != SERVICE_ID
            || binding.namespace() != NAMESPACE
            || binding.artifact_sha256() != COLLECTIONS_PACKAGE_SHA256
            || binding.artifact_version() != "0.0.0"
        {
            return Err("binding is not the exact Collections shadow candidate".into());
        }
        validate_candidate_record(app, runtime_generation)?;
        let authorization = app.stage_host_read_authorization(
            binding,
            HostReadInvocationPurpose {
                state_snapshot: false,
                terminals: Some((workspace_id.clone(), tab_id.clone())),
            },
            service_generation,
            provider_generation,
            runtime_generation,
        )?;
        validate_authorization(app, &authorization, workspace_id.as_str(), &tab_id)?;
        let fence = DocumentProducerFenceV2 {
            package_id: PACKAGE_ID.into(),
            service_id: SERVICE_ID.into(),
            artifact_sha256: COLLECTIONS_PACKAGE_SHA256.into(),
            runtime_generation,
            provider_generation,
            scope_generation,
        };
        let scope = DocumentScopeV2 {
            contract: "host.tab.v1".into(),
            scope_id: tab_id.to_string(),
        };
        let params = HostDocumentRenderParamsV2 {
            contract: HOST_DOCUMENT_RENDER_CONTRACT_V2.into(),
            producer: fence.clone(),
            scope: scope.clone(),
            requested_revision,
        };
        params.validate().map_err(|error| error.to_string())?;
        let expected_snapshot =
            current_terminal_snapshot(app, &authorization, &workspace_id, &tab_id)?;
        let expected_dependency = terminal_snapshot_dependency(
            &tab_id.to_string(),
            &expected_snapshot.fingerprint_sha256,
            provider_generation,
        )
        .map_err(|error| error.to_string())?;
        let read_expectation = ReadExpectation::TerminalsOnly {
            expected: expected_snapshot,
            dependency: expected_dependency,
        };
        self.install_pending(
            authorization,
            read_expectation,
            true,
            fence,
            scope,
            params,
            workspace_id,
            tab_id,
            runtime_generation,
            "collections-shadow",
            deadline,
            canceled,
        )
    }

    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    fn begin_composite_for_test(
        &mut self,
        app: &mut App,
        binding: &HostProviderBinding,
        workspace_id: String,
        tab_id: TabId,
        service_generation: u64,
        provider_generation: u64,
        runtime_generation: u64,
        scope_generation: u64,
        requested_revision: u64,
        deadline: Instant,
        canceled: Arc<AtomicBool>,
    ) -> Result<DocumentShadowAuditWork, String> {
        if Instant::now() >= deadline || canceled.load(std::sync::atomic::Ordering::Acquire) {
            return Err("shadow audit is canceled or expired".into());
        }
        let authorization = app.stage_host_read_authorization(
            binding,
            HostReadInvocationPurpose {
                state_snapshot: true,
                terminals: Some((workspace_id.clone(), tab_id.clone())),
            },
            service_generation,
            provider_generation,
            runtime_generation,
        )?;
        validate_authorization(app, &authorization, &workspace_id, &tab_id)?;
        let expected_state = current_state_snapshot(app, &authorization)?;
        let expected_terminals =
            current_terminal_snapshot(app, &authorization, &workspace_id, &tab_id)?;
        let mut dependencies = [
            crate::host_document_v2::host_state_snapshot_dependency(
                &expected_state.contract,
                &expected_state.store_id,
                expected_state.store_generation,
                &expected_state.namespace,
                expected_state.revision,
            )
            .map_err(|error| error.to_string())?,
            terminal_snapshot_dependency(
                tab_id.as_str(),
                &expected_terminals.fingerprint_sha256,
                expected_terminals.lease_fence.provider_generation,
            )
            .map_err(|error| error.to_string())?,
        ];
        dependencies.sort();
        let fence = DocumentProducerFenceV2 {
            package_id: authorization.package_id.clone(),
            service_id: authorization.service_id.clone(),
            artifact_sha256: authorization.artifact_sha256.clone(),
            runtime_generation,
            provider_generation,
            scope_generation,
        };
        let scope = DocumentScopeV2 {
            contract: "host.tab.v1".into(),
            scope_id: tab_id.to_string(),
        };
        let params = HostDocumentRenderParamsV2 {
            contract: HOST_DOCUMENT_RENDER_CONTRACT_V2.into(),
            producer: fence.clone(),
            scope: scope.clone(),
            requested_revision,
        };
        params.validate().map_err(|error| error.to_string())?;
        self.install_pending(
            authorization,
            ReadExpectation::StateThenTerminals {
                expected_state,
                expected_terminals,
                dependencies,
            },
            false,
            fence,
            scope,
            params,
            workspace_id,
            tab_id,
            runtime_generation,
            "composite-shadow",
            deadline,
            canceled,
        )
    }

    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    fn begin_collections_composite_for_test(
        &mut self,
        app: &mut App,
        binding: &HostProviderBinding,
        workspace_id: String,
        tab_id: TabId,
        service_generation: u64,
        provider_generation: u64,
        runtime_generation: u64,
        scope_generation: u64,
        requested_revision: u64,
        deadline: Instant,
        canceled: Arc<AtomicBool>,
    ) -> Result<DocumentShadowAuditWork, String> {
        if Instant::now() >= deadline || canceled.load(std::sync::atomic::Ordering::Acquire) {
            return Err("shadow audit is canceled or expired".into());
        }
        if binding.package_id() != PACKAGE_ID
            || binding.service_id() != SERVICE_ID
            || binding.namespace() != NAMESPACE
            || binding.artifact_sha256() != COLLECTIONS_PACKAGE_SHA256
            || binding.artifact_version() != "0.0.0"
        {
            return Err("binding is not the exact Collections shadow candidate".into());
        }
        validate_candidate_record(app, runtime_generation)?;
        let authorization = app.stage_host_read_authorization(
            binding,
            HostReadInvocationPurpose {
                state_snapshot: true,
                terminals: Some((workspace_id.clone(), tab_id.clone())),
            },
            service_generation,
            provider_generation,
            runtime_generation,
        )?;
        validate_authorization(app, &authorization, &workspace_id, &tab_id)?;
        let expected_state = current_state_snapshot(app, &authorization)?;
        let expected_terminals =
            current_terminal_snapshot(app, &authorization, &workspace_id, &tab_id)?;
        let mut dependencies = [
            crate::host_document_v2::host_state_snapshot_dependency(
                &expected_state.contract,
                &expected_state.store_id,
                expected_state.store_generation,
                &expected_state.namespace,
                expected_state.revision,
            )
            .map_err(|error| error.to_string())?,
            terminal_snapshot_dependency(
                tab_id.as_str(),
                &expected_terminals.fingerprint_sha256,
                expected_terminals.lease_fence.provider_generation,
            )
            .map_err(|error| error.to_string())?,
        ];
        dependencies.sort();
        let fence = DocumentProducerFenceV2 {
            package_id: PACKAGE_ID.into(),
            service_id: SERVICE_ID.into(),
            artifact_sha256: COLLECTIONS_PACKAGE_SHA256.into(),
            runtime_generation,
            provider_generation,
            scope_generation,
        };
        let scope = DocumentScopeV2 {
            contract: "host.tab.v1".into(),
            scope_id: tab_id.to_string(),
        };
        let params = HostDocumentRenderParamsV2 {
            contract: HOST_DOCUMENT_RENDER_CONTRACT_V2.into(),
            producer: fence.clone(),
            scope: scope.clone(),
            requested_revision,
        };
        params.validate().map_err(|error| error.to_string())?;
        self.install_pending(
            authorization,
            ReadExpectation::StateThenTerminals {
                expected_state,
                expected_terminals,
                dependencies,
            },
            true,
            fence,
            scope,
            params,
            workspace_id,
            tab_id,
            runtime_generation,
            "collections-composite-shadow",
            deadline,
            canceled,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn install_pending(
        &mut self,
        authorization: HostReadInvocationAuthorization,
        read_expectation: ReadExpectation,
        validate_collections_candidate: bool,
        fence: DocumentProducerFenceV2,
        scope: DocumentScopeV2,
        params: HostDocumentRenderParamsV2,
        workspace_id: String,
        tab_id: TabId,
        runtime_generation: u64,
        request_prefix: &str,
        deadline: Instant,
        canceled: Arc<AtomicBool>,
    ) -> Result<DocumentShadowAuditWork, String> {
        if Instant::now() >= deadline || canceled.load(std::sync::atomic::Ordering::Acquire) {
            return Err("shadow audit is canceled or expired".into());
        }
        let attempt_id = self
            .next_attempt_id
            .checked_add(1)
            .ok_or_else(|| "shadow attempt sequence exhausted".to_string())?;
        let key = AuditCacheKey {
            producer: fence.producer(),
            scope,
        };
        let older_attempt = self
            .pending
            .iter()
            .find_map(|(attempt, pending)| (pending.key == key).then_some(*attempt));
        if older_attempt.is_none()
            && self.pending.len() >= crate::quarantined_audit_cache::MAX_AUDIT_AUTHORITIES
        {
            return Err("shadow pending audit capacity reached".into());
        }
        if let Some(older_attempt) = older_attempt {
            let older = self
                .pending
                .remove(&older_attempt)
                .ok_or_else(|| "shadow pending audit index changed".to_string())?;
            let _ = self.cache.fail(older.ticket);
        }
        self.cache
            .update_authority(key.clone(), fence, read_expectation.dependencies().to_vec())
            .map_err(|error| format!("cannot install shadow authority: {error:?}"))?;
        let ticket = self
            .cache
            .begin_audit(&key)
            .map_err(|error| format!("cannot issue shadow ticket: {error:?}"))?;
        let observer = authorization.terminal_observations();
        let request = RuntimeRequestV1 {
            contract: crate::extension_runtime::EXTENSION_SERVICE_CONTRACT_V1.into(),
            request_id: format!("{request_prefix}-{attempt_id}"),
            identity: crate::extension_runtime::RuntimeIdentityV1 {
                package_id: authorization.package_id.clone(),
                namespace: authorization.namespace.clone(),
                service_id: authorization.service_id.clone(),
                generation: runtime_generation,
            },
            method: METHOD.into(),
            params: serde_json::to_value(&params).map_err(|error| error.to_string())?,
        };
        self.next_attempt_id = attempt_id;
        self.pending.insert(
            attempt_id,
            PendingAudit {
                key,
                ticket,
                authorization: authorization.clone(),
                read_expectation,
                validate_collections_candidate,
                params,
                workspace_id,
                tab_id,
                deadline,
                canceled,
            },
        );
        Ok(DocumentShadowAuditWork {
            authorization,
            request,
            observer,
            attempt_id,
            deadline,
        })
    }

    pub(crate) fn finalize(
        &mut self,
        app: &mut App,
        attempt_id: u64,
        result: Result<serde_json::Value, RuntimeError>,
    ) -> Result<(), String> {
        let now = Instant::now();
        let pending = self
            .pending
            .remove(&attempt_id)
            .ok_or_else(|| "shadow attempt is absent, stale, or replayed".to_string())?;
        let outcome = self.finalize_pending(app, &pending, result, now);
        match outcome {
            Ok(document) => self
                .cache
                .accept(pending.ticket, document)
                .map_err(|error| format!("shadow cache rejected document: {error:?}")),
            Err(error) => {
                let _ = self.cache.fail(pending.ticket);
                Err(error)
            }
        }
    }

    fn finalize_pending(
        &self,
        app: &App,
        pending: &PendingAudit,
        result: Result<serde_json::Value, RuntimeError>,
        now: Instant,
    ) -> Result<D, String> {
        if pending.canceled.load(std::sync::atomic::Ordering::Acquire) {
            return Err("shadow audit was canceled".into());
        }
        if now >= pending.deadline {
            return Err("shadow audit deadline expired".into());
        }
        if matches!(
            &pending.read_expectation,
            ReadExpectation::StateThenTerminals { .. }
        ) && app.extension_state_persistence_blocked
        {
            return Err("durable extension state authority is unavailable".into());
        }
        let value = result.map_err(|error| format!("shadow runtime failed: {error}"))?;
        if pending.validate_collections_candidate {
            validate_candidate_record(app, pending.params.producer.runtime_generation)?;
        }
        validate_authorization(
            app,
            &pending.authorization,
            &pending.workspace_id,
            &pending.tab_id,
        )?;
        let current_terminals = current_terminal_snapshot(
            app,
            &pending.authorization,
            &pending.workspace_id,
            &pending.tab_id,
        )?;
        let current_reads = match &pending.read_expectation {
            ReadExpectation::TerminalsOnly { .. } => {
                CurrentReadValues::TerminalsOnly(current_terminals)
            }
            ReadExpectation::StateThenTerminals { .. } => CurrentReadValues::StateThenTerminals {
                state: current_state_snapshot(app, &pending.authorization)?,
                terminals: current_terminals,
            },
        };
        let current = finalize_read_expectation(
            &pending.read_expectation,
            &pending.observer(),
            current_reads,
        )?;
        let document: D = serde_json::from_value(value)
            .map_err(|error| format!("malformed shadow document: {error}"))?;
        let terminals = current
            .layout
            .iter()
            .flat_map(|item| match item {
                crate::extension_host_terminals::HostTerminalLayoutItemV1::Tiled {
                    terminal_id,
                } => vec![terminal_id.clone()],
                crate::extension_host_terminals::HostTerminalLayoutItemV1::Container {
                    terminal_ids,
                    ..
                } => terminal_ids.clone(),
            })
            .collect();
        document.audit_validate(&terminals)?;
        if document.producer() != &pending.params.producer
            || document.scope() != &pending.params.scope
            || document.revision() != pending.params.requested_revision
            || document.dependencies() != pending.read_expectation.dependencies()
        {
            return Err("shadow document did not echo host-authored authority exactly".into());
        }
        Ok(document)
    }

    pub(crate) fn cached_len(&self) -> usize {
        self.cache.len()
    }
    pub(crate) fn pending_len(&self) -> usize {
        self.pending.len()
    }
}

impl PendingAudit {
    fn observer(&self) -> TrustedTerminalSnapshotObservations {
        self.authorization.terminal_observations()
    }
}

fn validate_candidate_record(app: &App, runtime_generation: u64) -> Result<(), String> {
    let registry = &app.marketplace_state.packages;
    let record = registry
        .packages
        .get(PACKAGE_ID)
        .ok_or("candidate package is not registered")?;
    let active = record
        .active
        .as_ref()
        .ok_or("candidate package has no active artifact")?;
    let exact_source = matches!(&record.source,
        crate::extension_package::PackageProvenanceV1::Curated {
            catalog,
            publisher,
            repository,
            package_id,
            version,
            source_commit,
            package_sha256,
            manifest_sha256,
            authorized_namespaces,
        } if catalog == CURATED_CATALOG
            && publisher == CURATED_PUBLISHER
            && repository == COLLECTIONS_REPOSITORY
            && package_id == PACKAGE_ID
            && version == "0.0.0"
            && source_commit == COLLECTIONS_COMMIT
            && package_sha256 == COLLECTIONS_PACKAGE_SHA256
            && manifest_sha256 == COLLECTIONS_MANIFEST_SHA256
            && authorized_namespaces == &[NAMESPACE]);
    if !exact_source
        || record.status != crate::extension_package::PackageLifecycleStatusV1::Enabled
        || record.package_id != PACKAGE_ID
        || record.namespaces != [NAMESPACE]
        || active.package_sha256 != COLLECTIONS_PACKAGE_SHA256
        || active.version != "0.0.0"
        || active.runtime_generation != runtime_generation
    {
        return Err("installed candidate provenance or active pointer changed".into());
    }
    Ok(())
}

fn validate_authorization(
    app: &App,
    authorization: &HostReadInvocationAuthorization,
    workspace_id: &str,
    tab_id: &TabId,
) -> Result<(), String> {
    let terminals = authorization
        .terminals
        .as_ref()
        .ok_or("terminal authorization is absent")?;
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
    let state_matches = authorization.state.as_ref().is_none_or(|state| {
        state.package_id == authorization.package_id
            && state.service_id == authorization.service_id
            && state.namespace == authorization.namespace
            && state.artifact_sha256 == authorization.artifact_sha256
            && state.artifact_version == authorization.artifact_version
            && state.catalog_revision == authorization.catalog_generation
            && state.package_revision == authorization.package_generation
            && state.manager_generation == authorization.service_generation
            && state.provider_generation == authorization.provider_generation
            && state.scope_id == state.store_id
            && state.scope_revision == authorization.package_generation
            && Arc::ptr_eq(&state.authority, &authorization.authority)
    });
    if app.marketplace_state.packages.revision != authorization.catalog_generation
        || record.revision != authorization.package_generation
        || record.package_id != authorization.package_id
        || record.status != crate::extension_package::PackageLifecycleStatusV1::Enabled
        || !record
            .namespaces
            .iter()
            .any(|namespace| namespace == &authorization.namespace)
        || active.package_sha256 != authorization.artifact_sha256
        || active.version != authorization.artifact_version
        || active.runtime_generation != authorization.runtime_generation
        || authorization.artifact_generation != authorization.runtime_generation
        || terminals.workspace_id != workspace_id
        || &terminals.tab_id != tab_id
        || !Arc::ptr_eq(&authorization.authority, &terminals.authority)
        || !state_matches
        || !current_authority
        || !authorization.authority.permits_generations(
            authorization.service_generation,
            authorization.provider_generation,
            authorization.runtime_generation,
        )
    {
        return Err("shadow authorization is stale or mismatched".into());
    }
    Ok(())
}

impl App {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn begin_document_shadow_audit(
        &mut self,
        binding: &HostProviderBinding,
        workspace_id: String,
        tab_id: TabId,
        service_generation: u64,
        provider_generation: u64,
        runtime_generation: u64,
        scope_generation: u64,
        requested_revision: u64,
        deadline: Instant,
        canceled: Arc<AtomicBool>,
    ) -> Result<DocumentShadowAuditWork, String> {
        let mut coordinator = std::mem::take(&mut self.document_shadow_audits);
        let result = coordinator.begin(
            self,
            binding,
            workspace_id,
            tab_id,
            service_generation,
            provider_generation,
            runtime_generation,
            scope_generation,
            requested_revision,
            deadline,
            canceled,
        );
        self.document_shadow_audits = coordinator;
        result
    }

    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn begin_collections_composite_shadow_audit_for_test(
        &mut self,
        binding: &HostProviderBinding,
        workspace_id: String,
        tab_id: TabId,
        service_generation: u64,
        provider_generation: u64,
        runtime_generation: u64,
        scope_generation: u64,
        requested_revision: u64,
        deadline: Instant,
        canceled: Arc<AtomicBool>,
    ) -> Result<DocumentShadowAuditWork, String> {
        let mut coordinator = std::mem::take(&mut self.document_shadow_audits);
        let result = coordinator.begin_collections_composite_for_test(
            self,
            binding,
            workspace_id,
            tab_id,
            service_generation,
            provider_generation,
            runtime_generation,
            scope_generation,
            requested_revision,
            deadline,
            canceled,
        );
        self.document_shadow_audits = coordinator;
        result
    }

    pub(crate) fn finalize_document_shadow_audit(
        &mut self,
        attempt_id: u64,
        result: Result<serde_json::Value, RuntimeError>,
    ) -> Result<(), String> {
        let mut coordinator = std::mem::take(&mut self.document_shadow_audits);
        let result = coordinator.finalize(self, attempt_id, result);
        self.document_shadow_audits = coordinator;
        result
    }

    pub(crate) fn document_shadow_cached_for_test(
        &self,
        params: &HostDocumentRenderParamsV2,
    ) -> Option<&HostDocumentV2> {
        self.document_shadow_audits.cache.get(&AuditCacheKey {
            producer: params.producer.producer(),
            scope: params.scope.clone(),
        })
    }
}

fn current_terminal_snapshot(
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
        return Err("shadow authority pointer was superseded".into());
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
    crate::extension_host_terminals::build_snapshot(
        workspace,
        tab,
        &app.state.terminals,
        terminals.fence.clone(),
    )
    .map_err(|error| error.message)
}

fn current_state_snapshot(
    app: &App,
    authorization: &HostReadInvocationAuthorization,
) -> Result<crate::extension_host_state::HostStateSnapshotV1, String> {
    let state = authorization
        .state
        .as_ref()
        .ok_or("state authorization is absent")?;
    if state.namespace != authorization.namespace
        || state.store_id != app.extension_state.store_id
        || state.store_generation != app.extension_state.store_generation
    {
        return Err("state authorization store or namespace changed".into());
    }
    app.extension_state
        .snapshot(&state.namespace)
        .map(Into::into)
        .map_err(|error| format!("cannot capture current state snapshot: {error:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extension_runtime::RuntimeHostCallObserver;
    use std::sync::atomic::Ordering;
    use std::time::Duration;

    const SERVICE_GENERATION: u64 = 31;
    const PROVIDER_GENERATION: u64 = 41;
    const RUNTIME_GENERATION: u64 = 51;

    fn staged_app() -> (App, HostProviderBinding) {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![crate::workspace::Workspace::test_new("shadow")];
        app.state.ensure_test_terminals();
        app.marketplace_state.packages.revision = 11;
        app.marketplace_state.packages.packages.insert(
            PACKAGE_ID.into(),
            crate::extension_package::PackageLifecycleRecordV1 {
                package_id: PACKAGE_ID.into(),
                source: crate::extension_package::PackageProvenanceV1::Curated {
                    catalog: CURATED_CATALOG.into(),
                    publisher: "vqro".into(),
                    repository: COLLECTIONS_REPOSITORY.into(),
                    package_id: PACKAGE_ID.into(),
                    version: "0.0.0".into(),
                    source_commit: COLLECTIONS_COMMIT.into(),
                    package_sha256: COLLECTIONS_PACKAGE_SHA256.into(),
                    manifest_sha256: COLLECTIONS_MANIFEST_SHA256.into(),
                    authorized_namespaces: vec![NAMESPACE.into()],
                },
                previous_source: None,
                namespaces: vec![NAMESPACE.into()],
                revision: 13,
                status: crate::extension_package::PackageLifecycleStatusV1::Enabled,
                active: Some(crate::extension_package::PackageArtifactPointerV1 {
                    package_sha256: COLLECTIONS_PACKAGE_SHA256.into(),
                    version: "0.0.0".into(),
                    runtime_generation: RUNTIME_GENERATION,
                }),
                previous: None,
                activation_error: None,
                pending_activation: None,
                retained_data: false,
            },
        );
        let binding = HostProviderBinding::test_new(
            PACKAGE_ID,
            SERVICE_ID,
            NAMESPACE,
            COLLECTIONS_PACKAGE_SHA256.into(),
            "0.0.0",
        );
        (app, binding)
    }

    fn staged_composite_app() -> (App, HostProviderBinding) {
        let (mut app, _) = staged_app();
        app.marketplace_state.packages.packages.insert(
            "example.document".into(),
            crate::extension_package::PackageLifecycleRecordV1 {
                package_id: "example.document".into(),
                source: crate::extension_package::PackageProvenanceV1::File {
                    package_sha256: "b".repeat(64),
                },
                previous_source: None,
                namespaces: vec!["example.document".into()],
                revision: 17,
                status: crate::extension_package::PackageLifecycleStatusV1::Enabled,
                active: Some(crate::extension_package::PackageArtifactPointerV1 {
                    package_sha256: "b".repeat(64),
                    version: "1.2.3".into(),
                    runtime_generation: RUNTIME_GENERATION,
                }),
                previous: None,
                activation_error: None,
                pending_activation: None,
                retained_data: false,
            },
        );
        let binding = HostProviderBinding::test_new(
            "example.document",
            "reader",
            "example.document",
            "b".repeat(64),
            "1.2.3",
        );
        (app, binding)
    }

    fn begin(
        app: &mut App,
        binding: &HostProviderBinding,
        canceled: Arc<AtomicBool>,
    ) -> DocumentShadowAuditWork {
        let workspace_id = app.state.workspaces[0].id.clone();
        let tab_id = app.state.workspaces[0].tabs[0].id.clone();
        app.begin_document_shadow_audit(
            binding,
            workspace_id,
            tab_id,
            SERVICE_GENERATION,
            PROVIDER_GENERATION,
            RUNTIME_GENERATION,
            61,
            1,
            Instant::now() + Duration::from_secs(30),
            canceled,
        )
        .unwrap()
    }

    fn try_begin_composite(
        app: &mut App,
        binding: &HostProviderBinding,
        deadline: Instant,
        canceled: Arc<AtomicBool>,
    ) -> Result<DocumentShadowAuditWork, String> {
        let workspace_id = app.state.workspaces[0].id.clone();
        let tab_id = app.state.workspaces[0].tabs[0].id.clone();
        let mut coordinator = std::mem::take(&mut app.document_shadow_audits);
        let result = coordinator.begin_composite_for_test(
            app,
            binding,
            workspace_id,
            tab_id,
            SERVICE_GENERATION,
            PROVIDER_GENERATION,
            RUNTIME_GENERATION,
            61,
            1,
            deadline,
            canceled,
        );
        app.document_shadow_audits = coordinator;
        result
    }

    fn begin_composite(
        app: &mut App,
        binding: &HostProviderBinding,
        canceled: Arc<AtomicBool>,
    ) -> DocumentShadowAuditWork {
        try_begin_composite(
            app,
            binding,
            Instant::now() + Duration::from_secs(30),
            canceled,
        )
        .unwrap()
    }

    fn assert_curated_source_race_rejected(
        mutate: impl FnOnce(&mut crate::extension_package::PackageProvenanceV1),
    ) {
        let (mut app, binding) = staged_app();
        let work = begin(&mut app, &binding, Arc::new(AtomicBool::new(false)));
        let value = observe_and_document(&app, &work);
        mutate(
            &mut app
                .marketplace_state
                .packages
                .packages
                .get_mut(PACKAGE_ID)
                .unwrap()
                .source,
        );
        assert!(app
            .finalize_document_shadow_audit(work.attempt_id, Ok(value))
            .is_err());
    }

    fn observe_and_document(app: &App, work: &DocumentShadowAuditWork) -> serde_json::Value {
        let terminals = work.authorization.terminals.as_ref().unwrap();
        let workspace = &app.state.workspaces[0];
        let tab = &workspace.tabs[0];
        let snapshot = crate::extension_host_terminals::build_snapshot(
            workspace,
            tab,
            &app.state.terminals,
            terminals.fence.clone(),
        )
        .unwrap();
        work.observer.observe_accepted(
            crate::extension_host_terminals::HOST_TERMINALS_SNAPSHOT_METHOD,
            &crate::extension_runtime::RuntimeHostCallResponseV1 {
                kind: "host_call_response".into(),
                call_id: "observed".into(),
                identity: work.request.identity.clone(),
                result: Some(serde_json::to_value(&snapshot).unwrap()),
                error: None,
            },
        );
        let params: HostDocumentRenderParamsV2 =
            serde_json::from_value(work.request.params.clone()).unwrap();
        let mut terminal_ids = Vec::new();
        for item in &snapshot.layout {
            match item {
                crate::extension_host_terminals::HostTerminalLayoutItemV1::Tiled {
                    terminal_id,
                } => terminal_ids.push(terminal_id.clone()),
                crate::extension_host_terminals::HostTerminalLayoutItemV1::Container {
                    terminal_ids: ids,
                    ..
                } => terminal_ids.extend(ids.iter().cloned()),
            }
        }
        let roots = (0..terminal_ids.len())
            .map(|index| format!("slot-{index}"))
            .collect::<Vec<_>>();
        let nodes = terminal_ids
            .into_iter()
            .enumerate()
            .map(
                |(index, terminal_id)| crate::host_document_v2::DocumentNodeV2::TerminalSlot {
                    id: format!("slot-{index}"),
                    accessibility: crate::host_document_v2::DocumentAccessibilityV2 {
                        name: format!("Terminal {index}"),
                        description: None,
                    },
                    terminal_id: terminal_id.to_string(),
                },
            )
            .collect();
        serde_json::to_value(HostDocumentV2 {
            contract: crate::host_document_v2::HOST_DOCUMENT_CONTRACT_V2.into(),
            producer: params.producer,
            scope: params.scope,
            revision: params.requested_revision,
            dependencies: vec![terminal_snapshot_dependency(
                &snapshot.tab_id.to_string(),
                &snapshot.fingerprint_sha256,
                snapshot.lease_fence.provider_generation,
            )
            .unwrap()],
            roots,
            nodes,
        })
        .unwrap()
    }

    fn successful_observation(
        work: &DocumentShadowAuditWork,
        result: serde_json::Value,
    ) -> crate::extension_runtime::RuntimeHostCallResponseV1 {
        crate::extension_runtime::RuntimeHostCallResponseV1 {
            kind: "host_response".into(),
            call_id: "observed".into(),
            identity: work.request.identity.clone(),
            result: Some(result),
            error: None,
        }
    }

    fn observe_composite_state(
        app: &App,
        work: &DocumentShadowAuditWork,
    ) -> crate::extension_host_state::HostStateSnapshotV1 {
        let state = current_state_snapshot(app, &work.authorization).unwrap();
        work.observer.observe_accepted(
            crate::extension_host_state::HOST_STATE_SNAPSHOT_METHOD,
            &successful_observation(work, serde_json::to_value(&state).unwrap()),
        );
        state
    }

    fn observe_composite_terminals(
        app: &App,
        work: &DocumentShadowAuditWork,
    ) -> crate::extension_host_terminals::HostTerminalsSnapshotV1 {
        let terminals = current_terminal_snapshot(
            app,
            &work.authorization,
            &app.state.workspaces[0].id,
            &app.state.workspaces[0].tabs[0].id,
        )
        .unwrap();
        work.observer.observe_accepted(
            crate::extension_host_terminals::HOST_TERMINALS_SNAPSHOT_METHOD,
            &successful_observation(work, serde_json::to_value(&terminals).unwrap()),
        );
        terminals
    }

    fn composite_document(
        work: &DocumentShadowAuditWork,
        state: &crate::extension_host_state::HostStateSnapshotV1,
        terminals: &crate::extension_host_terminals::HostTerminalsSnapshotV1,
    ) -> serde_json::Value {
        let params: HostDocumentRenderParamsV2 =
            serde_json::from_value(work.request.params.clone()).unwrap();
        let terminal_ids = terminals
            .layout
            .iter()
            .flat_map(|item| match item {
                crate::extension_host_terminals::HostTerminalLayoutItemV1::Tiled {
                    terminal_id,
                } => vec![terminal_id.clone()],
                crate::extension_host_terminals::HostTerminalLayoutItemV1::Container {
                    terminal_ids,
                    ..
                } => terminal_ids.clone(),
            })
            .collect::<Vec<_>>();
        let roots = (0..terminal_ids.len())
            .map(|index| format!("slot-{index}"))
            .collect::<Vec<_>>();
        let nodes = terminal_ids
            .into_iter()
            .enumerate()
            .map(
                |(index, terminal_id)| crate::host_document_v2::DocumentNodeV2::TerminalSlot {
                    id: format!("slot-{index}"),
                    accessibility: crate::host_document_v2::DocumentAccessibilityV2 {
                        name: format!("Terminal {index}"),
                        description: None,
                    },
                    terminal_id: terminal_id.to_string(),
                },
            )
            .collect();
        let mut dependencies = vec![
            crate::host_document_v2::host_state_snapshot_dependency(
                &state.contract,
                &state.store_id,
                state.store_generation,
                &state.namespace,
                state.revision,
            )
            .unwrap(),
            terminal_snapshot_dependency(
                terminals.tab_id.as_str(),
                &terminals.fingerprint_sha256,
                terminals.lease_fence.provider_generation,
            )
            .unwrap(),
        ];
        dependencies.sort();
        serde_json::to_value(HostDocumentV2 {
            contract: crate::host_document_v2::HOST_DOCUMENT_CONTRACT_V2.into(),
            producer: params.producer,
            scope: params.scope,
            revision: params.requested_revision,
            dependencies,
            roots,
            nodes,
        })
        .unwrap()
    }

    fn observe_composite_and_document(
        app: &App,
        work: &DocumentShadowAuditWork,
    ) -> serde_json::Value {
        let state = observe_composite_state(app, work);
        let terminals = observe_composite_terminals(app, work);
        composite_document(work, &state, &terminals)
    }

    #[test]
    fn accepts_once_and_rejects_replay_late_and_newer_ticket_completion() {
        let (mut app, binding) = staged_app();
        let first = begin(&mut app, &binding, Arc::new(AtomicBool::new(false)));
        let second = begin(&mut app, &binding, Arc::new(AtomicBool::new(false)));
        assert_eq!(app.document_shadow_audits.pending_len(), 1);
        let first_value = observe_and_document(&app, &first);
        assert!(app
            .finalize_document_shadow_audit(first.attempt_id, Ok(first_value))
            .is_err());
        let second_value = observe_and_document(&app, &second);
        app.finalize_document_shadow_audit(second.attempt_id, Ok(second_value))
            .unwrap();
        assert_eq!(app.document_shadow_audits.cached_len(), 1);
        assert!(app
            .finalize_document_shadow_audit(second.attempt_id, Ok(serde_json::Value::Null))
            .is_err());
        assert_eq!(app.document_shadow_audits.pending_len(), 0);
    }

    #[test]
    fn cancellation_deadline_runtime_failure_and_zero_or_multiple_observations_consume_ticket() {
        let (mut app, binding) = staged_app();
        let canceled = Arc::new(AtomicBool::new(false));
        let work = begin(&mut app, &binding, Arc::clone(&canceled));
        let value = observe_and_document(&app, &work);
        canceled.store(true, Ordering::Release);
        assert!(app
            .finalize_document_shadow_audit(work.attempt_id, Ok(value))
            .unwrap_err()
            .contains("canceled"));

        let work = begin(&mut app, &binding, Arc::new(AtomicBool::new(false)));
        assert!(app
            .finalize_document_shadow_audit(work.attempt_id, Err(RuntimeError::Trap("trap".into())))
            .is_err());
        let work = begin(&mut app, &binding, Arc::new(AtomicBool::new(false)));
        assert!(app
            .finalize_document_shadow_audit(work.attempt_id, Err(RuntimeError::Timeout))
            .is_err());
        let work = begin(&mut app, &binding, Arc::new(AtomicBool::new(false)));
        app.document_shadow_audits
            .pending
            .get_mut(&work.attempt_id)
            .unwrap()
            .deadline = Instant::now();
        assert!(app
            .finalize_document_shadow_audit(work.attempt_id, Ok(serde_json::Value::Null))
            .unwrap_err()
            .contains("deadline"));
        let work = begin(&mut app, &binding, Arc::new(AtomicBool::new(false)));
        assert!(app
            .finalize_document_shadow_audit(work.attempt_id, Ok(serde_json::Value::Null))
            .unwrap_err()
            .contains("Zero"));
        let work = begin(&mut app, &binding, Arc::new(AtomicBool::new(false)));
        let value = observe_and_document(&app, &work);
        let _ = observe_and_document(&app, &work);
        assert!(app
            .finalize_document_shadow_audit(work.attempt_id, Ok(value))
            .unwrap_err()
            .contains("Multiple"));
        assert_eq!(app.document_shadow_audits.cached_len(), 0);
    }

    #[test]
    fn pending_is_globally_bounded_and_one_per_cache_key() {
        let (mut app, binding) = staged_app();
        app.state.workspaces = (0..=crate::quarantined_audit_cache::MAX_AUDIT_AUTHORITIES)
            .map(|index| crate::workspace::Workspace::test_new(&format!("audit-{index}")))
            .collect();
        app.state.terminals.clear();
        app.state.ensure_test_terminals();
        let mut first_attempt = None;
        for index in 0..crate::quarantined_audit_cache::MAX_AUDIT_AUTHORITIES {
            let workspace_id = app.state.workspaces[index].id.clone();
            let tab_id = app.state.workspaces[index].tabs[0].id.clone();
            let work = app
                .begin_document_shadow_audit(
                    &binding,
                    workspace_id,
                    tab_id,
                    SERVICE_GENERATION,
                    PROVIDER_GENERATION,
                    RUNTIME_GENERATION,
                    61,
                    1,
                    Instant::now() + Duration::from_secs(30),
                    Arc::new(AtomicBool::new(false)),
                )
                .unwrap();
            first_attempt.get_or_insert(work.attempt_id);
        }
        assert_eq!(
            app.document_shadow_audits.pending_len(),
            crate::quarantined_audit_cache::MAX_AUDIT_AUTHORITIES
        );
        let overflow_index = crate::quarantined_audit_cache::MAX_AUDIT_AUTHORITIES;
        let error = app
            .begin_document_shadow_audit(
                &binding,
                app.state.workspaces[overflow_index].id.clone(),
                app.state.workspaces[overflow_index].tabs[0].id.clone(),
                SERVICE_GENERATION,
                PROVIDER_GENERATION,
                RUNTIME_GENERATION,
                61,
                1,
                Instant::now() + Duration::from_secs(30),
                Arc::new(AtomicBool::new(false)),
            )
            .err()
            .unwrap();
        assert!(error.contains("capacity"));

        let replacement = app
            .begin_document_shadow_audit(
                &binding,
                app.state.workspaces[0].id.clone(),
                app.state.workspaces[0].tabs[0].id.clone(),
                SERVICE_GENERATION,
                PROVIDER_GENERATION,
                RUNTIME_GENERATION,
                61,
                2,
                Instant::now() + Duration::from_secs(30),
                Arc::new(AtomicBool::new(false)),
            )
            .unwrap();
        assert_eq!(
            app.document_shadow_audits.pending_len(),
            crate::quarantined_audit_cache::MAX_AUDIT_AUTHORITIES
        );
        assert_ne!(replacement.attempt_id, first_attempt.unwrap());
        assert!(app
            .finalize_document_shadow_audit(first_attempt.unwrap(), Ok(serde_json::Value::Null))
            .is_err());
    }

    #[test]
    fn attempt_id_exhaustion_happens_before_ticket_issuance() {
        let (mut app, binding) = staged_app();
        app.document_shadow_audits.next_attempt_id = u64::MAX;
        let workspace_id = app.state.workspaces[0].id.clone();
        let tab_id = app.state.workspaces[0].tabs[0].id.clone();
        let error = app
            .begin_document_shadow_audit(
                &binding,
                workspace_id,
                tab_id,
                SERVICE_GENERATION,
                PROVIDER_GENERATION,
                RUNTIME_GENERATION,
                61,
                1,
                Instant::now() + Duration::from_secs(30),
                Arc::new(AtomicBool::new(false)),
            )
            .err()
            .expect("attempt exhaustion must reject begin");
        assert!(error.contains("sequence exhausted"));
        assert_eq!(app.document_shadow_audits.pending_len(), 0);
        assert_eq!(app.document_shadow_audits.cache.authority_len(), 0);
    }

    #[test]
    fn every_curated_provenance_field_is_revalidated() {
        type Provenance = crate::extension_package::PackageProvenanceV1;
        assert_curated_source_race_rejected(|source| {
            if let Provenance::Curated { catalog, .. } = source {
                *catalog = "forged".into()
            }
        });
        assert_curated_source_race_rejected(|source| {
            if let Provenance::Curated { publisher, .. } = source {
                *publisher = "forged".into()
            }
        });
        assert_curated_source_race_rejected(|source| {
            if let Provenance::Curated { repository, .. } = source {
                *repository = "forged".into()
            }
        });
        assert_curated_source_race_rejected(|source| {
            if let Provenance::Curated { package_id, .. } = source {
                *package_id = "forged.package".into()
            }
        });
        assert_curated_source_race_rejected(|source| {
            if let Provenance::Curated { version, .. } = source {
                *version = "9.9.9".into()
            }
        });
        assert_curated_source_race_rejected(|source| {
            if let Provenance::Curated { source_commit, .. } = source {
                *source_commit = "0".repeat(40)
            }
        });
        assert_curated_source_race_rejected(|source| {
            if let Provenance::Curated { package_sha256, .. } = source {
                *package_sha256 = "0".repeat(64)
            }
        });
        assert_curated_source_race_rejected(|source| {
            if let Provenance::Curated {
                manifest_sha256, ..
            } = source
            {
                *manifest_sha256 = "0".repeat(64)
            }
        });
        assert_curated_source_race_rejected(|source| {
            if let Provenance::Curated {
                authorized_namespaces,
                ..
            } = source
            {
                authorized_namespaces.clear()
            }
        });
    }

    #[test]
    fn package_artifact_and_exact_workspace_tab_races_are_rejected() {
        let (mut app, binding) = staged_app();
        let work = begin(&mut app, &binding, Arc::new(AtomicBool::new(false)));
        let value = observe_and_document(&app, &work);
        if let crate::extension_package::PackageProvenanceV1::Curated { repository, .. } = &mut app
            .marketplace_state
            .packages
            .packages
            .get_mut(PACKAGE_ID)
            .unwrap()
            .source
        {
            *repository = "https://example.invalid/forged.git".into();
        }
        assert!(app
            .finalize_document_shadow_audit(work.attempt_id, Ok(value))
            .is_err());

        let (mut app, binding) = staged_app();
        let work = begin(&mut app, &binding, Arc::new(AtomicBool::new(false)));
        let value = observe_and_document(&app, &work);
        app.marketplace_state
            .packages
            .packages
            .get_mut(PACKAGE_ID)
            .unwrap()
            .active
            .as_mut()
            .unwrap()
            .package_sha256 = "f".repeat(64);
        assert!(app
            .finalize_document_shadow_audit(work.attempt_id, Ok(value))
            .is_err());

        let (mut app, binding) = staged_app();
        let work = begin(&mut app, &binding, Arc::new(AtomicBool::new(false)));
        let value = observe_and_document(&app, &work);
        app.state.workspaces.clear();
        assert!(app
            .finalize_document_shadow_audit(work.attempt_id, Ok(value))
            .is_err());
    }

    #[test]
    fn synthetic_composite_audit_accepts_exact_state_then_terminals_once() {
        let (mut app, binding) = staged_composite_app();
        let work = begin_composite(&mut app, &binding, Arc::new(AtomicBool::new(false)));
        let value = observe_composite_and_document(&app, &work);
        app.finalize_document_shadow_audit(work.attempt_id, Ok(value.clone()))
            .unwrap();
        assert_eq!(app.document_shadow_audits.cached_len(), 1);
        assert!(app
            .finalize_document_shadow_audit(work.attempt_id, Ok(value))
            .unwrap_err()
            .contains("replayed"));
    }

    fn mutate_composite_namespace(app: &mut App, key: &str) {
        let snapshot = app.extension_state.snapshot("example.document").unwrap();
        app.extension_state
            .transact(&crate::extension_state::ExtensionStateTransactionV1 {
                contract: crate::extension_state::EXTENSION_STATE_CONTRACT_V1.into(),
                namespace: "example.document".into(),
                expected_store_generation: snapshot.store_generation,
                expected_revision: snapshot.revision,
                idempotency_key: format!("mutation-{key}"),
                operations: vec![crate::extension_state::ExtensionStateOperationV1 {
                    key: key.into(),
                    value: Some(serde_json::json!(key)),
                }],
            })
            .unwrap();
    }

    fn mutate_terminal_layout(app: &mut App) {
        let pane = app.state.workspaces[0].test_split(ratatui::layout::Direction::Vertical);
        let terminal = app.state.workspaces[0].tabs[0]
            .terminal_id(pane)
            .unwrap()
            .clone();
        app.state.terminals.insert(
            terminal.clone(),
            crate::terminal::TerminalState::new(terminal, "/".into()),
        );
    }

    #[test]
    fn synthetic_composite_rejects_state_races_and_whole_value_mismatch() {
        let (mut app, binding) = staged_composite_app();
        let work = begin_composite(&mut app, &binding, Arc::new(AtomicBool::new(false)));
        mutate_composite_namespace(&mut app, "before-observation");
        let value = observe_composite_and_document(&app, &work);
        assert!(app
            .finalize_document_shadow_audit(work.attempt_id, Ok(value))
            .unwrap_err()
            .contains("state snapshots differ"));

        let (mut app, binding) = staged_composite_app();
        let work = begin_composite(&mut app, &binding, Arc::new(AtomicBool::new(false)));
        let value = observe_composite_and_document(&app, &work);
        mutate_composite_namespace(&mut app, "after-observation");
        assert!(app
            .finalize_document_shadow_audit(work.attempt_id, Ok(value))
            .unwrap_err()
            .contains("state snapshots differ"));

        let (mut app, binding) = staged_composite_app();
        let work = begin_composite(&mut app, &binding, Arc::new(AtomicBool::new(false)));
        let value = observe_composite_and_document(&app, &work);
        let ReadExpectation::StateThenTerminals { expected_state, .. } = &mut app
            .document_shadow_audits
            .pending
            .get_mut(&work.attempt_id)
            .unwrap()
            .read_expectation
        else {
            panic!("composite expectation")
        };
        expected_state
            .values
            .insert("forged".into(), serde_json::json!(true));
        assert!(app
            .finalize_document_shadow_audit(work.attempt_id, Ok(value))
            .unwrap_err()
            .contains("state snapshots differ"));
    }

    #[test]
    fn synthetic_composite_rejects_state_change_between_a_and_b() {
        let (mut app, binding) = staged_composite_app();
        let work = begin_composite(&mut app, &binding, Arc::new(AtomicBool::new(false)));
        let state = observe_composite_state(&app, &work);
        mutate_composite_namespace(&mut app, "between-a-b");
        let terminals = observe_composite_terminals(&app, &work);
        let value = composite_document(&work, &state, &terminals);
        assert!(app
            .finalize_document_shadow_audit(work.attempt_id, Ok(value))
            .unwrap_err()
            .contains("state snapshots differ"));
    }

    #[test]
    fn synthetic_composite_rejects_terminal_races_around_a_and_b() {
        let (mut app, binding) = staged_composite_app();
        let work = begin_composite(&mut app, &binding, Arc::new(AtomicBool::new(false)));
        mutate_terminal_layout(&mut app);
        let state = observe_composite_state(&app, &work);
        let terminals = observe_composite_terminals(&app, &work);
        let value = composite_document(&work, &state, &terminals);
        assert!(app
            .finalize_document_shadow_audit(work.attempt_id, Ok(value))
            .unwrap_err()
            .contains("terminal snapshots differ"));

        let (mut app, binding) = staged_composite_app();
        let work = begin_composite(&mut app, &binding, Arc::new(AtomicBool::new(false)));
        let state = observe_composite_state(&app, &work);
        mutate_terminal_layout(&mut app);
        let terminals = observe_composite_terminals(&app, &work);
        let value = composite_document(&work, &state, &terminals);
        assert!(app
            .finalize_document_shadow_audit(work.attempt_id, Ok(value))
            .unwrap_err()
            .contains("terminal snapshots differ"));

        let (mut app, binding) = staged_composite_app();
        let work = begin_composite(&mut app, &binding, Arc::new(AtomicBool::new(false)));
        let state = observe_composite_state(&app, &work);
        let terminals = observe_composite_terminals(&app, &work);
        let value = composite_document(&work, &state, &terminals);
        mutate_terminal_layout(&mut app);
        assert!(app
            .finalize_document_shadow_audit(work.attempt_id, Ok(value))
            .unwrap_err()
            .contains("terminal snapshots differ"));
    }

    #[test]
    fn synthetic_composite_rejects_persistence_block_after_observation() {
        let (mut app, binding) = staged_composite_app();
        let work = begin_composite(&mut app, &binding, Arc::new(AtomicBool::new(false)));
        let value = observe_composite_and_document(&app, &work);
        app.extension_state_persistence_blocked = true;
        assert!(app
            .finalize_document_shadow_audit(work.attempt_id, Ok(value))
            .unwrap_err()
            .contains("durable extension state authority is unavailable"));
    }

    fn assert_composite_authority_race_rejected(mutate: impl FnOnce(&mut App)) {
        let (mut app, binding) = staged_composite_app();
        let work = begin_composite(&mut app, &binding, Arc::new(AtomicBool::new(false)));
        let value = observe_composite_and_document(&app, &work);
        mutate(&mut app);
        assert!(app
            .finalize_document_shadow_audit(work.attempt_id, Ok(value))
            .is_err());
    }

    #[test]
    fn synthetic_composite_rejects_dependency_and_authority_races() {
        let (mut app, binding) = staged_composite_app();
        let work = begin_composite(&mut app, &binding, Arc::new(AtomicBool::new(false)));
        let mut value = observe_composite_and_document(&app, &work);
        value["dependencies"].as_array_mut().unwrap().reverse();
        assert!(app
            .finalize_document_shadow_audit(work.attempt_id, Ok(value))
            .is_err());

        assert_composite_authority_race_rejected(|app| {
            app.marketplace_state.packages.revision += 1;
        });
        assert_composite_authority_race_rejected(|app| {
            app.marketplace_state
                .packages
                .packages
                .get_mut("example.document")
                .unwrap()
                .revision += 1;
        });
        assert_composite_authority_race_rejected(|app| {
            app.marketplace_state
                .packages
                .packages
                .get_mut("example.document")
                .unwrap()
                .namespaces
                .clear();
        });
        assert_composite_authority_race_rejected(|app| {
            app.marketplace_state
                .packages
                .packages
                .get_mut("example.document")
                .unwrap()
                .active
                .as_mut()
                .unwrap()
                .package_sha256 = "c".repeat(64);
        });
        assert_composite_authority_race_rejected(|app| {
            app.marketplace_state
                .packages
                .packages
                .get_mut("example.document")
                .unwrap()
                .active
                .as_mut()
                .unwrap()
                .runtime_generation += 1;
        });
        assert_composite_authority_race_rejected(|app| {
            app.revoke_host_invocation_authority("example.document", "reader");
        });
        assert_composite_authority_race_rejected(|app| {
            app.extension_state.store_generation += 1;
        });
    }

    #[test]
    fn synthetic_composite_begin_rejects_immediate_stop_and_supersedes_same_key() {
        let (mut app, binding) = staged_composite_app();
        let canceled = Arc::new(AtomicBool::new(true));
        assert!(try_begin_composite(
            &mut app,
            &binding,
            Instant::now() + Duration::from_secs(30),
            canceled,
        )
        .err()
        .unwrap()
        .contains("canceled or expired"));
        assert_eq!(app.document_shadow_audits.pending_len(), 0);
        assert!(try_begin_composite(
            &mut app,
            &binding,
            Instant::now() - Duration::from_millis(1),
            Arc::new(AtomicBool::new(false)),
        )
        .err()
        .unwrap()
        .contains("canceled or expired"));
        assert_eq!(app.document_shadow_audits.pending_len(), 0);

        let first = begin_composite(&mut app, &binding, Arc::new(AtomicBool::new(false)));
        let second = begin_composite(&mut app, &binding, Arc::new(AtomicBool::new(false)));
        assert_eq!(app.document_shadow_audits.pending_len(), 1);
        assert!(app
            .finalize_document_shadow_audit(first.attempt_id, Ok(serde_json::Value::Null))
            .unwrap_err()
            .contains("stale"));
        let value = observe_composite_and_document(&app, &second);
        app.finalize_document_shadow_audit(second.attempt_id, Ok(value))
            .unwrap();
    }

    #[test]
    fn synthetic_composite_rejects_cancellation_and_deadline() {
        let (mut app, binding) = staged_composite_app();
        let canceled = Arc::new(AtomicBool::new(false));
        let work = begin_composite(&mut app, &binding, Arc::clone(&canceled));
        let value = observe_composite_and_document(&app, &work);
        canceled.store(true, Ordering::Release);
        assert!(app
            .finalize_document_shadow_audit(work.attempt_id, Ok(value))
            .unwrap_err()
            .contains("canceled"));

        let (mut app, binding) = staged_composite_app();
        let work = begin_composite(&mut app, &binding, Arc::new(AtomicBool::new(false)));
        let value = observe_composite_and_document(&app, &work);
        app.document_shadow_audits
            .pending
            .get_mut(&work.attempt_id)
            .unwrap()
            .deadline = Instant::now() - Duration::from_millis(1);
        assert!(app
            .finalize_document_shadow_audit(work.attempt_id, Ok(value))
            .unwrap_err()
            .contains("deadline"));
    }

    #[test]
    fn topology_value_currentness_and_authority_races_are_honest() {
        let (mut app, binding) = staged_app();
        let work = begin(&mut app, &binding, Arc::new(AtomicBool::new(false)));
        let value = observe_and_document(&app, &work);
        let added_pane = app.state.workspaces[0].test_split(ratatui::layout::Direction::Vertical);
        let added_terminal = app.state.workspaces[0].tabs[0]
            .terminal_id(added_pane)
            .unwrap()
            .clone();
        app.state.terminals.insert(
            added_terminal.clone(),
            crate::terminal::TerminalState::new(added_terminal.clone(), "/".into()),
        );
        assert!(app
            .finalize_document_shadow_audit(work.attempt_id, Ok(value))
            .unwrap_err()
            .contains("snapshots differ"));

        let (mut app, binding) = staged_app();
        let work = begin(&mut app, &binding, Arc::new(AtomicBool::new(false)));
        let value = observe_and_document(&app, &work);
        let original = work.observer.exactly_one().unwrap();
        let added_pane = app.state.workspaces[0].test_split(ratatui::layout::Direction::Vertical);
        let added_terminal = app.state.workspaces[0].tabs[0]
            .terminal_id(added_pane)
            .unwrap()
            .clone();
        app.state.terminals.insert(
            added_terminal.clone(),
            crate::terminal::TerminalState::new(added_terminal.clone(), "/".into()),
        );
        assert!(!app.state.workspaces[0].close_pane(added_pane));
        app.state.terminals.remove(&added_terminal);
        let returned_to_a = current_terminal_snapshot(
            &app,
            &work.authorization,
            &app.state.workspaces[0].id,
            &app.state.workspaces[0].tabs[0].id,
        )
        .unwrap();
        assert_eq!(returned_to_a, original);
        app.finalize_document_shadow_audit(work.attempt_id, Ok(value))
            .expect("exact canonical A→B→A is honestly value-current");

        let work = begin(&mut app, &binding, Arc::new(AtomicBool::new(false)));
        let mut value = observe_and_document(&app, &work);
        value["producer"]["scope_generation"] = serde_json::json!(999);
        assert!(app
            .finalize_document_shadow_audit(work.attempt_id, Ok(value))
            .is_err());

        let work = begin(&mut app, &binding, Arc::new(AtomicBool::new(false)));
        let value = observe_and_document(&app, &work);
        app.revoke_host_invocation_authority(PACKAGE_ID, SERVICE_ID);
        assert!(app
            .finalize_document_shadow_audit(work.attempt_id, Ok(value))
            .is_err());

        let work = begin(&mut app, &binding, Arc::new(AtomicBool::new(false)));
        let value = observe_and_document(&app, &work);
        app.marketplace_state.packages.revision += 1;
        assert!(app
            .finalize_document_shadow_audit(work.attempt_id, Ok(value))
            .is_err());
    }
}
