//! Staged `host.state.v1` infrastructure for package-v1 runtime conformance.
//!
//! This module is not a generally available package-service capability. It binds an
//! explicitly authorized installed-package invocation to the App actor's existing
//! `DurableExtensionStateV1` authority. Ordinary package calls, legacy services, and
//! package preflight continue to use disabled host calls.

use std::collections::BTreeSet;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    mpsc, Arc,
};
use std::time::{Duration, Instant};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::extension_runtime::{
    RuntimeErrorBodyV1, RuntimeHostCallHandler, RuntimeHostCallResponseV1, RuntimeHostCallV1,
    RuntimeIdentityV1,
};

pub const HOST_STATE_CONTRACT_V1: &str = "host.state.v1";
pub const HOST_STATE_READ_CAPABILITY: &str = "host.state.read";
pub const HOST_STATE_WRITE_CAPABILITY: &str = "host.state.write";
pub const HOST_STATE_SNAPSHOT_METHOD: &str = "state.snapshot";
pub const HOST_STATE_TRANSACT_METHOD: &str = "state.transact";
const ACTOR_WAIT_POLL: Duration = Duration::from_millis(10);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum HostStateOperationV1 {
    Set {
        key: String,
        value: serde_json::Value,
    },
    Delete {
        key: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HostStateSnapshotParamsV1 {
    pub contract: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HostStateTransactParamsV1 {
    pub contract: String,
    pub expected_store_generation: u64,
    pub expected_revision: u64,
    pub idempotency_key: String,
    pub operations: Vec<HostStateOperationV1>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct HostStateSnapshotV1 {
    pub contract: String,
    pub store_id: String,
    pub store_generation: u64,
    pub namespace: String,
    pub revision: u64,
    pub sequence: u64,
    pub values: std::collections::BTreeMap<String, serde_json::Value>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct HostStateCommitV1 {
    pub contract: String,
    pub store_id: String,
    pub store_generation: u64,
    pub namespace: String,
    pub revision: u64,
    pub sequence: u64,
    pub replayed: bool,
}

impl From<crate::extension_state::ExtensionStateSnapshotV1> for HostStateSnapshotV1 {
    fn from(snapshot: crate::extension_state::ExtensionStateSnapshotV1) -> Self {
        Self {
            contract: HOST_STATE_CONTRACT_V1.into(),
            store_id: snapshot.store_id,
            store_generation: snapshot.store_generation,
            namespace: snapshot.namespace,
            revision: snapshot.revision,
            sequence: snapshot.sequence,
            values: snapshot.values,
        }
    }
}

impl From<crate::extension_state::ExtensionStateCommitV1> for HostStateCommitV1 {
    fn from(commit: crate::extension_state::ExtensionStateCommitV1) -> Self {
        Self {
            contract: HOST_STATE_CONTRACT_V1.into(),
            store_id: commit.store_id,
            store_generation: commit.store_generation,
            namespace: commit.namespace,
            revision: commit.revision,
            sequence: commit.sequence,
            replayed: commit.replayed,
        }
    }
}

#[derive(Debug)]
pub(crate) struct HostInvocationAuthority {
    revoked: AtomicBool,
    manager_generation: AtomicU64,
    provider_generation: AtomicU64,
    runtime_generation: AtomicU64,
}

impl HostInvocationAuthority {
    // Staged until ordinary production package services can receive explicit leases.
    #[allow(dead_code)]
    pub(crate) fn new(
        manager_generation: u64,
        provider_generation: u64,
        runtime_generation: u64,
    ) -> Arc<Self> {
        Arc::new(Self {
            revoked: AtomicBool::new(false),
            manager_generation: AtomicU64::new(manager_generation),
            provider_generation: AtomicU64::new(provider_generation),
            runtime_generation: AtomicU64::new(runtime_generation),
        })
    }

    // Staged until the production package-provider catalog owns retirement.
    #[allow(dead_code)]
    pub(crate) fn revoke(&self) {
        self.revoked.store(true, Ordering::Release);
    }

    // A catalog rebuild can supersede all three generations without mutating issued leases.
    #[allow(dead_code)]
    pub(crate) fn supersede(
        &self,
        manager_generation: u64,
        provider_generation: u64,
        runtime_generation: u64,
    ) {
        self.manager_generation
            .store(manager_generation, Ordering::Release);
        self.provider_generation
            .store(provider_generation, Ordering::Release);
        self.runtime_generation
            .store(runtime_generation, Ordering::Release);
    }

    pub(crate) fn is_revoked(&self) -> bool {
        self.revoked.load(Ordering::Acquire)
    }

    pub(crate) fn permits_generations(
        &self,
        service_generation: u64,
        provider_generation: u64,
        runtime_generation: u64,
    ) -> bool {
        !self.revoked.load(Ordering::Acquire)
            && self.manager_generation.load(Ordering::Acquire) == service_generation
            && self.provider_generation.load(Ordering::Acquire) == provider_generation
            && self.runtime_generation.load(Ordering::Acquire) == runtime_generation
    }

    pub(crate) fn permits(&self, lease: &HostStateInvocationLease) -> bool {
        self.permits_generations(
            lease.manager_generation,
            lease.provider_generation,
            lease.runtime_generation,
        )
    }
}

/// Package-bound revocation authority shared by staged host-call contracts.
pub(crate) type HostStateLeaseAuthority = HostInvocationAuthority;

#[derive(Clone, Debug)]
pub(crate) struct HostProviderBinding {
    package_id: String,
    service_id: String,
    namespace: String,
    artifact_sha256: String,
    artifact_version: String,
    #[cfg(test)]
    capabilities: BTreeSet<String>,
}

impl HostProviderBinding {
    // Staged until the production package-provider catalog constructs bindings.
    #[allow(dead_code)]
    pub(crate) fn from_installed(
        installed: &crate::extension_package::InstalledPackageV1,
        service_id: &str,
        namespace: &str,
    ) -> Result<Self, String> {
        crate::extension_package::verify_installed_package(
            std::path::Path::new(&installed.immutable_root),
            &installed.package_sha256,
        )?;
        let archive = std::path::Path::new(&installed.immutable_root).join("package.vqrox");
        let package = crate::extension_package::validate_package_file(
            &archive,
            Some(&installed.package_sha256),
        )?;
        let manifest = &package.descriptor.manifest;
        if package.descriptor.package_sha256 != installed.package_sha256
            || package.descriptor.manifest_sha256 != installed.manifest_sha256
            || manifest.id != installed.package_id
            || manifest.version != installed.version
            || !manifest
                .services
                .iter()
                .any(|service| service.id == service_id)
            || !manifest.namespaces.iter().any(|owned| owned == namespace)
        {
            return Err(
                "host provider does not match the verified artifact, service, or namespace".into(),
            );
        }
        Ok(Self {
            package_id: installed.package_id.clone(),
            service_id: service_id.into(),
            namespace: namespace.into(),
            artifact_sha256: installed.package_sha256.clone(),
            artifact_version: installed.version.clone(),
            #[cfg(test)]
            capabilities: manifest
                .services
                .iter()
                .find(|service| service.id == service_id)
                .map(|service| service.capabilities.iter().cloned().collect())
                .ok_or_else(|| "package service is not declared".to_string())?,
        })
    }

    pub(crate) fn package_id(&self) -> &str {
        &self.package_id
    }

    pub(crate) fn service_id(&self) -> &str {
        &self.service_id
    }

    pub(crate) fn namespace(&self) -> &str {
        &self.namespace
    }

    pub(crate) fn artifact_sha256(&self) -> &str {
        &self.artifact_sha256
    }

    pub(crate) fn artifact_version(&self) -> &str {
        &self.artifact_version
    }

    #[cfg(test)]
    pub(crate) fn capabilities(&self) -> &BTreeSet<String> {
        &self.capabilities
    }

    #[cfg(test)]
    pub(crate) fn test_new(
        package_id: &str,
        service_id: &str,
        namespace: &str,
        artifact_sha256: String,
        artifact_version: &str,
    ) -> Self {
        Self {
            package_id: package_id.into(),
            service_id: service_id.into(),
            namespace: namespace.into(),
            artifact_sha256,
            artifact_version: artifact_version.into(),
            capabilities: [
                HOST_STATE_READ_CAPABILITY.to_string(),
                crate::extension_host_terminals::HOST_TERMINALS_READ_CAPABILITY.to_string(),
            ]
            .into_iter()
            .collect(),
        }
    }
}

/// Backward-compatible name for the shared verified package/service/artifact binding.
pub(crate) type HostStateProviderBinding = HostProviderBinding;

#[derive(Clone, Debug)]
pub(crate) struct HostStateInvocationAuthorization {
    pub(crate) package_id: String,
    pub(crate) service_id: String,
    pub(crate) namespace: String,
    pub(crate) artifact_sha256: String,
    pub(crate) artifact_version: String,
    pub(crate) catalog_revision: u64,
    pub(crate) package_revision: u64,
    pub(crate) manager_generation: u64,
    pub(crate) provider_generation: u64,
    pub(crate) scope_id: String,
    pub(crate) scope_revision: u64,
    pub(crate) store_id: String,
    pub(crate) store_generation: u64,
    pub(crate) authority: Arc<HostStateLeaseAuthority>,
    pub(crate) event_tx: tokio::sync::mpsc::Sender<crate::events::AppEvent>,
}

#[derive(Clone, Debug)]
pub(crate) struct HostStateInvocationLease {
    pub(crate) outer_request_id: String,
    pub(crate) identity: RuntimeIdentityV1,
    pub(crate) package_id: String,
    pub(crate) service_id: String,
    pub(crate) namespace: String,
    pub(crate) artifact_sha256: String,
    pub(crate) artifact_version: String,
    pub(crate) catalog_revision: u64,
    pub(crate) package_revision: u64,
    pub(crate) manager_generation: u64,
    pub(crate) provider_generation: u64,
    pub(crate) runtime_generation: u64,
    pub(crate) scope_id: String,
    pub(crate) scope_revision: u64,
    pub(crate) store_id: String,
    pub(crate) store_generation: u64,
    pub(crate) deadline: Instant,
    pub(crate) canceled: Arc<AtomicBool>,
    pub(crate) authority: Arc<HostStateLeaseAuthority>,
}

impl HostStateInvocationAuthorization {
    pub(crate) fn bind(
        &self,
        outer_request_id: String,
        identity: RuntimeIdentityV1,
        deadline: Instant,
        canceled: Arc<AtomicBool>,
    ) -> HostStateInvocationLease {
        HostStateInvocationLease {
            outer_request_id,
            runtime_generation: identity.generation,
            identity,
            package_id: self.package_id.clone(),
            service_id: self.service_id.clone(),
            namespace: self.namespace.clone(),
            artifact_sha256: self.artifact_sha256.clone(),
            artifact_version: self.artifact_version.clone(),
            catalog_revision: self.catalog_revision,
            package_revision: self.package_revision,
            manager_generation: self.manager_generation,
            provider_generation: self.provider_generation,
            scope_id: self.scope_id.clone(),
            scope_revision: self.scope_revision,
            store_id: self.store_id.clone(),
            store_generation: self.store_generation,
            deadline,
            canceled,
            authority: self.authority.clone(),
        }
    }
}

#[derive(Debug)]
pub(crate) enum HostStateActorMethod {
    Snapshot,
    Transact(crate::extension_state::ExtensionStateTransactionV1),
}

#[derive(Debug)]
pub(crate) struct HostStateActorRequest {
    pub(crate) lease: HostStateInvocationLease,
    #[cfg(test)]
    pub(crate) call_id: String,
    #[cfg(test)]
    pub(crate) capability: String,
    pub(crate) method: HostStateActorMethod,
    pub(crate) respond_to: mpsc::SyncSender<Result<serde_json::Value, RuntimeErrorBodyV1>>,
}

#[cfg(test)]
pub(crate) struct HostStateSuccessGate {
    pub(crate) received: std::sync::Barrier,
    pub(crate) release: std::sync::Barrier,
}

#[cfg(test)]
impl HostStateSuccessGate {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            received: std::sync::Barrier::new(2),
            release: std::sync::Barrier::new(2),
        })
    }
}

pub(crate) struct HostStateCallHandler {
    identity: RuntimeIdentityV1,
    grants: BTreeSet<String>,
    lease: HostStateInvocationLease,
    event_tx: tokio::sync::mpsc::Sender<crate::events::AppEvent>,
    #[cfg(test)]
    success_gate: Option<Arc<HostStateSuccessGate>>,
}

impl HostStateCallHandler {
    pub(crate) fn new(
        identity: RuntimeIdentityV1,
        grants: BTreeSet<String>,
        lease: HostStateInvocationLease,
        event_tx: tokio::sync::mpsc::Sender<crate::events::AppEvent>,
    ) -> Self {
        Self {
            identity,
            grants,
            lease,
            event_tx,
            #[cfg(test)]
            success_gate: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_success_gate(mut self, gate: Arc<HostStateSuccessGate>) -> Self {
        self.success_gate = Some(gate);
        self
    }

    #[cfg(test)]
    pub(crate) fn with_deadline(mut self, deadline: Instant) -> Self {
        self.lease.deadline = deadline;
        self
    }

    fn error(
        &self,
        call: &RuntimeHostCallV1,
        code: &str,
        message: &str,
    ) -> RuntimeHostCallResponseV1 {
        RuntimeHostCallResponseV1 {
            kind: "host_response".into(),
            call_id: call.call_id.clone(),
            identity: self.identity.clone(),
            result: None,
            error: Some(RuntimeErrorBodyV1 {
                code: code.into(),
                message: message.into(),
            }),
        }
    }

    fn admitted_method(
        &self,
        call: &RuntimeHostCallV1,
    ) -> Result<HostStateActorMethod, RuntimeErrorBodyV1> {
        let invalid = || RuntimeErrorBodyV1 {
            code: "invalid_host_call".into(),
            message: "host call identity, package ownership, namespace, or depth is invalid".into(),
        };
        if call.kind != "host_call"
            || call.identity != self.identity
            || call.identity != self.lease.identity
            || call.identity.package_id != self.lease.package_id
            || call.identity.service_id != self.lease.service_id
            || call.identity.namespace != self.lease.namespace
            || call.identity.generation != self.lease.runtime_generation
            || call.depth != 1
            || call.namespace != self.lease.namespace
            || call.namespace == "host"
            || call.namespace.starts_with("host.")
        {
            return Err(invalid());
        }
        if !self.lease.authority.permits(&self.lease) {
            return Err(RuntimeErrorBodyV1 {
                code: "stale_host_call_lease".into(),
                message: "host call lease was revoked or superseded".into(),
            });
        }
        let required = match call.method.as_str() {
            HOST_STATE_SNAPSHOT_METHOD => HOST_STATE_READ_CAPABILITY,
            HOST_STATE_TRANSACT_METHOD => HOST_STATE_WRITE_CAPABILITY,
            _ => {
                return Err(RuntimeErrorBodyV1 {
                    code: "invalid_host_call".into(),
                    message: "host call method is not available".into(),
                });
            }
        };
        if call.capability != required || !self.grants.contains(required) {
            return Err(RuntimeErrorBodyV1 {
                code: "capability_denied".into(),
                message: "host call capability was not granted for this method".into(),
            });
        }
        match call.method.as_str() {
            HOST_STATE_SNAPSHOT_METHOD => {
                let params: HostStateSnapshotParamsV1 = serde_json::from_value(call.params.clone())
                    .map_err(|error| RuntimeErrorBodyV1 {
                        code: "invalid_host_call".into(),
                        message: format!("invalid state snapshot parameters: {error}"),
                    })?;
                if params.contract != HOST_STATE_CONTRACT_V1 {
                    return Err(invalid());
                }
                Ok(HostStateActorMethod::Snapshot)
            }
            HOST_STATE_TRANSACT_METHOD => {
                let params: HostStateTransactParamsV1 = serde_json::from_value(call.params.clone())
                    .map_err(|error| RuntimeErrorBodyV1 {
                        code: "invalid_host_call".into(),
                        message: format!("invalid state transaction parameters: {error}"),
                    })?;
                if params.contract != HOST_STATE_CONTRACT_V1 {
                    return Err(invalid());
                }
                let operations = params
                    .operations
                    .into_iter()
                    .map(|operation| match operation {
                        HostStateOperationV1::Set { key, value } => {
                            crate::extension_state::ExtensionStateOperationV1 {
                                key,
                                value: Some(value),
                            }
                        }
                        HostStateOperationV1::Delete { key } => {
                            crate::extension_state::ExtensionStateOperationV1 { key, value: None }
                        }
                    })
                    .collect();
                Ok(HostStateActorMethod::Transact(
                    crate::extension_state::ExtensionStateTransactionV1 {
                        contract: crate::extension_state::EXTENSION_STATE_CONTRACT_V1.into(),
                        namespace: self.lease.namespace.clone(),
                        expected_store_generation: params.expected_store_generation,
                        expected_revision: params.expected_revision,
                        idempotency_key: params.idempotency_key,
                        operations,
                    },
                ))
            }
            _ => Err(invalid()),
        }
    }
}

impl RuntimeHostCallHandler for HostStateCallHandler {
    fn handle_host_call(&self, call: RuntimeHostCallV1) -> RuntimeHostCallResponseV1 {
        let method = match self.admitted_method(&call) {
            Ok(method) => method,
            Err(error) => {
                return self.error(&call, &error.code, &error.message);
            }
        };
        if self.lease.canceled.load(Ordering::Acquire) {
            return self.error(&call, "host_call_canceled", "outer invocation was canceled");
        }
        if Instant::now() >= self.lease.deadline {
            return self.error(
                &call,
                "host_call_timeout",
                "outer invocation deadline expired",
            );
        }
        let (respond_to, response) = mpsc::sync_channel(1);
        let request = HostStateActorRequest {
            lease: self.lease.clone(),
            #[cfg(test)]
            call_id: call.call_id.clone(),
            #[cfg(test)]
            capability: call.capability.clone(),
            method,
            respond_to,
        };
        if self
            .event_tx
            .try_send(crate::events::AppEvent::ExtensionHostStateCall(Box::new(
                request,
            )))
            .is_err()
        {
            return self.error(
                &call,
                "host_call_busy",
                "host state actor queue is unavailable",
            );
        }
        loop {
            if self.lease.canceled.load(Ordering::Acquire) {
                return self.error(&call, "host_call_canceled", "outer invocation was canceled");
            }
            let Some(remaining) = self.lease.deadline.checked_duration_since(Instant::now()) else {
                return self.error(
                    &call,
                    "host_call_timeout",
                    "outer invocation deadline expired",
                );
            };
            match response.recv_timeout(remaining.min(ACTOR_WAIT_POLL)) {
                Ok(Ok(result)) => {
                    #[cfg(test)]
                    if let Some(gate) = &self.success_gate {
                        gate.received.wait();
                        gate.release.wait();
                    }
                    if self.lease.canceled.load(Ordering::Acquire) {
                        return self.error(
                            &call,
                            "host_call_canceled",
                            "outer invocation was canceled after host success",
                        );
                    }
                    if Instant::now() >= self.lease.deadline {
                        return self.error(
                            &call,
                            "host_call_timeout",
                            "outer invocation deadline expired after host success",
                        );
                    }
                    if !self.lease.authority.permits(&self.lease) {
                        return self.error(
                            &call,
                            "stale_host_call_lease",
                            "host call lease was revoked after host success",
                        );
                    }
                    return RuntimeHostCallResponseV1 {
                        kind: "host_response".into(),
                        call_id: call.call_id,
                        identity: self.identity.clone(),
                        result: Some(result),
                        error: None,
                    };
                }
                Ok(Err(error)) => return self.error(&call, &error.code, &error.message),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return self.error(
                        &call,
                        "host_call_unavailable",
                        "host state actor response channel closed",
                    );
                }
            }
        }
    }
}

pub(crate) fn state_error(
    error: &crate::extension_state::ExtensionStateError,
) -> RuntimeErrorBodyV1 {
    let code = match error {
        crate::extension_state::ExtensionStateError::StoreGenerationConflict { .. } => {
            "state_store_mismatch"
        }
        crate::extension_state::ExtensionStateError::RevisionConflict { .. } => {
            "state_revision_conflict"
        }
        crate::extension_state::ExtensionStateError::IdempotencyConflict => {
            "state_idempotency_conflict"
        }
        crate::extension_state::ExtensionStateError::LimitExceeded(_) => "resource_limit",
        _ => "invalid_host_call",
    };
    RuntimeErrorBodyV1 {
        code: code.into(),
        message: format!("{error:?}"),
    }
}

#[cfg(test)]
pub(crate) fn install_test_package_and_authorize(
    app: &mut crate::app::App,
    installed: &crate::extension_package::InstalledPackageV1,
    service_id: &str,
    namespace: &str,
    runtime_generation: u64,
) -> (
    HostStateInvocationAuthorization,
    Arc<HostStateLeaseAuthority>,
) {
    app.marketplace_state.packages.revision = 1;
    app.marketplace_state.packages.packages.insert(
        installed.package_id.clone(),
        crate::extension_package::PackageLifecycleRecordV1 {
            package_id: installed.package_id.clone(),
            source: crate::extension_package::PackageProvenanceV1::File {
                package_sha256: installed.package_sha256.clone(),
            },
            previous_source: None,
            namespaces: vec![namespace.into()],
            revision: 3,
            status: crate::extension_package::PackageLifecycleStatusV1::Enabled,
            active: Some(crate::extension_package::PackageArtifactPointerV1 {
                package_sha256: installed.package_sha256.clone(),
                version: installed.version.clone(),
                runtime_generation,
            }),
            previous: None,
            activation_error: None,
            pending_activation: None,
            retained_data: false,
        },
    );
    let binding =
        HostStateProviderBinding::from_installed(installed, service_id, namespace).unwrap();
    let authorization = app
        .stage_host_state_authorization(&binding, 5, 7, runtime_generation)
        .unwrap();
    let authority = authorization.authority.clone();
    (authorization, authority)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn host_state_handler_and_router_are_send_sync_without_wait_mutexes() {
        assert_send_sync::<HostStateCallHandler>();
        assert_send_sync::<crate::extension_runtime::RuntimeHostCallRouter>();
    }

    #[test]
    fn transaction_operations_distinguish_json_null_from_delete() {
        let set: HostStateOperationV1 = serde_json::from_value(serde_json::json!({
            "op": "set",
            "key": "nullable",
            "value": null
        }))
        .unwrap();
        let delete: HostStateOperationV1 = serde_json::from_value(serde_json::json!({
            "op": "delete",
            "key": "nullable"
        }))
        .unwrap();
        assert_eq!(
            set,
            HostStateOperationV1::Set {
                key: "nullable".into(),
                value: serde_json::Value::Null,
            }
        );
        assert_eq!(
            delete,
            HostStateOperationV1::Delete {
                key: "nullable".into(),
            }
        );
    }

    #[test]
    fn staged_contract_is_absent_from_native_and_frozen_endpoint_shapes() {
        let native = include_str!("../docs/next/api/vqro-api.schema.json");
        let endpoint = include_str!("../tests/fixtures/endpoint-method-shapes-v1.json");
        assert!(!native.contains(HOST_STATE_CONTRACT_V1));
        assert!(!endpoint.contains(HOST_STATE_CONTRACT_V1));
        assert!(!endpoint.contains(HOST_STATE_SNAPSHOT_METHOD));
        assert!(!endpoint.contains(HOST_STATE_TRANSACT_METHOD));
    }

    #[test]
    fn staged_contract_contains_no_deferred_authority_or_product_policy() {
        let schema = serde_json::to_string(&crate::extension_spec::host_state_schema_document())
            .unwrap()
            .to_ascii_lowercase();
        for forbidden in [
            "collection",
            "archive",
            "promote",
            "terminal",
            "relationship",
            "evidence",
            "document",
            "render",
        ] {
            assert!(!schema.contains(forbidden), "schema contains {forbidden}");
        }
        assert!(!schema.contains("events_since"));
        let schema = crate::extension_spec::host_state_schema_document();
        for name in ["snapshot_params", "snapshot", "transact_params", "commit"] {
            assert_eq!(
                schema["schemas"][name]["properties"]["contract"]["const"],
                HOST_STATE_CONTRACT_V1
            );
        }
    }
}
