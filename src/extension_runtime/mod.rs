//! Runtime-neutral extension dispatch and lifecycle contracts.
//!
//! Legacy configured process services and WASI Preview 2 components implement one
//! dispatcher contract. Installed packages are admitted only as constrained components;
//! the process adapter remains solely for backwards-compatible host configuration.

pub(crate) mod component;
pub(crate) mod process;

#[cfg(test)]
mod collections_package_conformance;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc,
};
use std::time::{Duration, Instant};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub(crate) use component::ComponentRuntimeFactory;
pub(crate) use process::ProcessRuntimeFactory;

pub const EXTENSION_SERVICE_CONTRACT_V1: &str = "vqro.service.v1";
pub const MAX_RUNTIME_FRAME_BYTES: usize = crate::extension_contract::MAX_EXTENSION_FRAME_BYTES;
/// Maximum encoded result admitted for the structural host-document render method.
/// Kept runtime-local so the production dispatcher does not activate document-v2 wiring.
const MAX_HOST_DOCUMENT_RENDER_RESPONSE_BYTES: usize = 128 * 1024;
pub const MAX_RUNTIME_QUEUE_DEPTH: usize = 8;
pub const MAX_OUTSTANDING_HOST_CALLS: usize = 1;
pub const MAX_HOST_CALLS_PER_INVOCATION: usize = 8;
pub const MAX_HOST_CALL_DEPTH: u8 = 1;
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(3);
pub const REQUEST_TIMEOUT: Duration = Duration::from_millis(750);

#[derive(Debug)]
pub(crate) enum RuntimeError {
    Configuration(String),
    Io(std::io::Error),
    Protocol(String),
    Trap(String),
    ResourceLimit(String),
    FuelExhausted,
    Unsupported(String),
    QueueFull(String),
    StaleGeneration { expected: u64, actual: u64 },
    Remote(String),
    Timeout,
    Canceled,
    Exited,
    Backoff,
    RestartBudget(String),
}

impl RuntimeError {
    pub(crate) fn requires_restart(&self) -> bool {
        matches!(
            self,
            Self::Io(_)
                | Self::Protocol(_)
                | Self::Trap(_)
                | Self::ResourceLimit(_)
                | Self::FuelExhausted
                | Self::QueueFull(_)
                | Self::Timeout
                | Self::Canceled
                | Self::Exited
        )
    }

    pub(crate) fn counts_as_failure(&self) -> bool {
        !matches!(
            self,
            Self::Canceled | Self::Unsupported(_) | Self::StaleGeneration { .. }
        )
    }

    pub(crate) fn code(&self) -> &'static str {
        match self {
            Self::Configuration(_) => "configuration",
            Self::Io(_) => "io",
            Self::Protocol(_) => "protocol",
            Self::Trap(_) => "trap",
            Self::ResourceLimit(_) => "resource_limit",
            Self::FuelExhausted => "fuel_exhausted",
            Self::Unsupported(_) => "unsupported",
            Self::QueueFull(_) => "queue_full",
            Self::StaleGeneration { .. } => "stale_generation",
            Self::Remote(_) => "remote",
            Self::Timeout => "timeout",
            Self::Canceled => "canceled",
            Self::Exited => "exited",
            Self::Backoff => "backoff",
            Self::RestartBudget(_) => "restart_budget",
        }
    }
}

impl std::fmt::Display for RuntimeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Configuration(message)
            | Self::Protocol(message)
            | Self::Trap(message)
            | Self::ResourceLimit(message)
            | Self::Unsupported(message)
            | Self::QueueFull(message)
            | Self::Remote(message) => formatter.write_str(message),
            Self::Io(error) => write!(formatter, "{error}"),
            Self::FuelExhausted => formatter.write_str("extension component exhausted fuel"),
            Self::Timeout => formatter.write_str("extension service timed out"),
            Self::Canceled => formatter.write_str("extension service request was canceled"),
            Self::Exited => formatter.write_str("extension service exited"),
            Self::Backoff => formatter.write_str("extension service is restarting"),
            Self::StaleGeneration { expected, actual } => write!(
                formatter,
                "stale extension runtime generation: expected {expected}, got {actual}"
            ),
            Self::RestartBudget(message) => write!(
                formatter,
                "extension service restart budget exhausted: {message}"
            ),
        }
    }
}

impl std::error::Error for RuntimeError {}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RuntimeIdentityV1 {
    pub package_id: String,
    pub namespace: String,
    pub service_id: String,
    pub generation: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeGrants {
    pub legacy: Vec<crate::api::schema::ExtensionServiceCapability>,
    pub capabilities: BTreeSet<String>,
    /// Only verified package-v1 runtimes may receive staged host-call leases.
    pub installed_package: bool,
}

impl RuntimeGrants {
    fn permits(&self, capability: &str) -> bool {
        self.capabilities.contains(capability)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RuntimeRequestV1 {
    pub contract: String,
    pub request_id: String,
    pub identity: RuntimeIdentityV1,
    pub method: String,
    pub params: serde_json::Value,
}

// Reserved by the runtime-neutral contract; production event delivery is not enabled.
#[allow(dead_code)]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RuntimeEventV1 {
    pub contract: String,
    pub event_id: String,
    pub identity: RuntimeIdentityV1,
    pub method: String,
    pub params: serde_json::Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RuntimeHostCallV1 {
    #[serde(rename = "type")]
    pub kind: String,
    pub call_id: String,
    pub identity: RuntimeIdentityV1,
    pub depth: u8,
    pub capability: String,
    pub method: String,
    pub namespace: String,
    pub params: serde_json::Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RuntimeHostCallResponseV1 {
    #[serde(rename = "type")]
    pub kind: String,
    pub call_id: String,
    pub identity: RuntimeIdentityV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<RuntimeErrorBodyV1>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RuntimeErrorBodyV1 {
    pub code: String,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NegotiatedRuntimeDescriptor {
    pub identity: RuntimeIdentityV1,
    pub methods: BTreeSet<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuntimeStopReason {
    Requested,
    Canceled,
    Deadline,
    Protocol,
    Drop,
}

pub(crate) trait RuntimeHostCallHandler: Send + Sync {
    fn handle_host_call(&self, call: RuntimeHostCallV1) -> RuntimeHostCallResponseV1;
}

#[cfg(test)]
pub(crate) trait RuntimeHostCallObserver: Send + Sync {
    /// Receives only responses whose envelope has passed the router's exact validation.
    fn observe_accepted(&self, method: &str, response: &RuntimeHostCallResponseV1);
}

pub(crate) struct RuntimeHostCallRouter {
    identity: RuntimeIdentityV1,
    namespace: String,
    allowed_methods: BTreeMap<String, String>,
    outstanding: AtomicBool,
    total_calls: AtomicUsize,
    call_limit_exceeded: AtomicBool,
    handler: Box<dyn RuntimeHostCallHandler>,
    #[cfg(test)]
    observer: Option<Box<dyn RuntimeHostCallObserver>>,
}

impl RuntimeHostCallRouter {
    pub(crate) fn new(
        identity: RuntimeIdentityV1,
        namespace: String,
        allowed_methods: BTreeMap<String, String>,
        handler: impl RuntimeHostCallHandler + 'static,
    ) -> Self {
        Self {
            identity,
            namespace,
            allowed_methods,
            outstanding: AtomicBool::new(false),
            total_calls: AtomicUsize::new(0),
            call_limit_exceeded: AtomicBool::new(false),
            handler: Box::new(handler),
            #[cfg(test)]
            observer: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_observer(
        mut self,
        observer: impl RuntimeHostCallObserver + 'static,
    ) -> Self {
        self.observer = Some(Box::new(observer));
        self
    }

    pub(crate) fn handle(&self, call: RuntimeHostCallV1) -> RuntimeHostCallResponseV1 {
        let reject = |code: &str, message: &str| RuntimeHostCallResponseV1 {
            kind: "host_response".into(),
            call_id: call.call_id.clone(),
            identity: self.identity.clone(),
            result: None,
            error: Some(RuntimeErrorBodyV1 {
                code: code.into(),
                message: message.into(),
            }),
        };
        let previous = self.total_calls.fetch_add(1, Ordering::AcqRel);
        if previous >= MAX_HOST_CALLS_PER_INVOCATION {
            self.call_limit_exceeded.store(true, Ordering::Release);
            return reject("resource_limit", "host-call count limit exceeded");
        }
        let encoded = match serde_json::to_vec(&call) {
            Ok(encoded) => encoded,
            Err(_) => return reject("invalid_host_call", "host call cannot be encoded"),
        };
        if encoded.len() > MAX_RUNTIME_FRAME_BYTES
            || call.kind != "host_call"
            || call.identity != self.identity
            || call.namespace != self.namespace
            || call.namespace == "host"
            || call.namespace.starts_with("host.")
            || call.depth != MAX_HOST_CALL_DEPTH
        {
            return reject(
                "invalid_host_call",
                "host call frame, identity, namespace, or depth is invalid",
            );
        }
        let Some(required_capability) = self.allowed_methods.get(&call.method) else {
            return reject("invalid_host_call", "host call method is not allowed");
        };
        if required_capability != &call.capability {
            return reject(
                "capability_denied",
                "host call capability does not authorize the method",
            );
        }
        if self
            .outstanding
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return reject("host_call_busy", "another host call is already outstanding");
        }
        struct OutstandingGuard<'a>(&'a AtomicBool);
        impl Drop for OutstandingGuard<'_> {
            fn drop(&mut self) {
                self.0.store(false, Ordering::Release);
            }
        }
        let _guard = OutstandingGuard(&self.outstanding);
        let expected_call_id = call.call_id.clone();
        let expected_identity = call.identity.clone();
        #[cfg(test)]
        let accepted_method = call.method.clone();
        let response = self.handler.handle_host_call(call);
        if response.kind != "host_response"
            || response.call_id != expected_call_id
            || response.identity != expected_identity
            || response.result.is_some() == response.error.is_some()
        {
            return RuntimeHostCallResponseV1 {
                kind: "host_response".into(),
                call_id: expected_call_id,
                identity: self.identity.clone(),
                result: None,
                error: Some(RuntimeErrorBodyV1 {
                    code: "invalid_host_response".into(),
                    message: "host-call handler returned a malformed response envelope".into(),
                }),
            };
        }
        #[cfg(test)]
        if let Some(observer) = &self.observer {
            observer.observe_accepted(&accepted_method, &response);
        }
        response
    }

    fn call_limit_exceeded(&self) -> bool {
        self.call_limit_exceeded.load(Ordering::Acquire)
    }
}

pub(crate) type SharedRuntimeHostCalls = Arc<RuntimeHostCallRouter>;

pub(crate) trait ExtensionRuntime: Send {
    fn descriptor(&self) -> &NegotiatedRuntimeDescriptor;

    fn invoke(
        &mut self,
        request: &RuntimeRequestV1,
        deadline: Instant,
        canceled: &AtomicBool,
        host_calls: &SharedRuntimeHostCalls,
        response_limit: usize,
    ) -> Result<serde_json::Value, RuntimeError>;

    // The envelope is defined, but production runtime event delivery remains disabled.
    #[allow(dead_code)]
    fn deliver(
        &mut self,
        _event: &RuntimeEventV1,
        _deadline: Instant,
        _canceled: &AtomicBool,
        _host_calls: &SharedRuntimeHostCalls,
    ) -> Result<(), RuntimeError> {
        Err(RuntimeError::Unsupported(
            "runtime events are not enabled before M2".into(),
        ))
    }

    fn stop(&mut self, reason: RuntimeStopReason);
}

#[derive(Clone, Debug)]
pub(crate) enum RuntimeFactory {
    Process(ProcessRuntimeFactory),
    Component(ComponentRuntimeFactory),
}

impl RuntimeFactory {
    pub(crate) fn legacy(
        id: String,
        command: Vec<String>,
        cwd: std::path::PathBuf,
        granted_capabilities: Vec<crate::api::schema::ExtensionServiceCapability>,
    ) -> Self {
        Self::Process(ProcessRuntimeFactory::legacy(
            id,
            command,
            cwd,
            granted_capabilities,
        ))
    }

    pub(crate) fn installed(
        installed: &crate::extension_package::InstalledPackageV1,
        service_id: &str,
        namespace: &str,
    ) -> Result<Self, RuntimeError> {
        let archive = std::path::Path::new(&installed.immutable_root).join("package.vqrox");
        let package = crate::extension_package::validate_package_file(
            &archive,
            Some(&installed.package_sha256),
        )
        .map_err(RuntimeError::Configuration)?;
        let service = package
            .descriptor
            .manifest
            .services
            .iter()
            .find(|service| service.id == service_id)
            .ok_or_else(|| RuntimeError::Configuration("package service is not declared".into()))?;
        match &service.runtime {
            crate::extension_package::PackageRuntimeV1::Process { .. } => {
                Err(RuntimeError::Unsupported(
                    "installed extension packages must use the constrained component runtime"
                        .into(),
                ))
            }
            crate::extension_package::PackageRuntimeV1::Component { .. } => {
                ComponentRuntimeFactory::installed(installed, service_id, namespace)
                    .map(Self::Component)
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn installed_process(
        installed: &crate::extension_package::InstalledPackageV1,
        service_id: &str,
        namespace: &str,
    ) -> Result<Self, RuntimeError> {
        ProcessRuntimeFactory::installed(installed, service_id, namespace).map(Self::Process)
    }

    fn identity(&self, generation: u64) -> RuntimeIdentityV1 {
        match self {
            Self::Process(factory) => factory.identity(generation),
            Self::Component(factory) => factory.identity(generation),
        }
    }

    fn grants(&self) -> RuntimeGrants {
        match self {
            Self::Process(factory) => factory.grants(),
            Self::Component(factory) => factory.grants(),
        }
    }

    fn start(
        &self,
        generation: u64,
        canceled: &AtomicBool,
    ) -> Result<Box<dyn ExtensionRuntime>, RuntimeError> {
        match self {
            Self::Process(factory) => factory
                .start(generation, canceled)
                .map(|runtime| Box::new(runtime) as Box<dyn ExtensionRuntime>),
            Self::Component(factory) => factory
                .start(generation, canceled)
                .map(|runtime| Box::new(runtime) as Box<dyn ExtensionRuntime>),
        }
    }
}

enum HostInvocationAuthorization {
    #[cfg(test)]
    Composite(Box<crate::extension_host_invocation::HostReadInvocationAuthorization>),
    /// Temporary write-capable conformance path. Composite package invocations are read-only.
    LegacyState(Box<crate::extension_host_state::HostStateInvocationAuthorization>),
    LegacyTerminals(Box<crate::extension_host_terminals::HostTerminalsInvocationAuthorization>),
}

pub(crate) struct ExtensionRuntimeDispatcher {
    identity: RuntimeIdentityV1,
    grants: RuntimeGrants,
    runtime: Box<dyn ExtensionRuntime>,
    next_request_id: u64,
}

impl std::fmt::Debug for ExtensionRuntimeDispatcher {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ExtensionRuntimeDispatcher")
            .field("identity", &self.identity)
            .field("grants", &self.grants)
            .field("next_request_id", &self.next_request_id)
            .finish_non_exhaustive()
    }
}

impl ExtensionRuntimeDispatcher {
    pub(crate) fn start(
        factory: &RuntimeFactory,
        generation: u64,
        canceled: &AtomicBool,
    ) -> Result<Self, RuntimeError> {
        if generation == 0 {
            return Err(RuntimeError::Configuration(
                "runtime generation must be nonzero".into(),
            ));
        }
        let identity = factory.identity(generation);
        let grants = factory.grants();
        let runtime = factory.start(generation, canceled)?;
        if runtime.descriptor().identity != identity {
            return Err(RuntimeError::Protocol(
                "runtime negotiated a different identity or generation".into(),
            ));
        }
        validate_granted_methods(&runtime.descriptor().methods, &grants.legacy)?;
        tracing::info!(
            package_id = %identity.package_id,
            namespace = %identity.namespace,
            service_id = %identity.service_id,
            generation = identity.generation,
            grant_count = grants.capabilities.len(),
            "extension runtime generation started"
        );
        Ok(Self {
            identity,
            grants,
            runtime,
            next_request_id: 1,
        })
    }

    pub(crate) fn methods(&self) -> &BTreeSet<String> {
        &self.runtime.descriptor().methods
    }

    pub(crate) fn request_cancellable(
        &mut self,
        method: &str,
        params: serde_json::Value,
        canceled: &AtomicBool,
    ) -> Result<serde_json::Value, RuntimeError> {
        self.invoke(
            method,
            params,
            self.identity.generation,
            Instant::now() + REQUEST_TIMEOUT,
            canceled,
        )
    }

    pub(crate) fn invoke(
        &mut self,
        method: &str,
        params: serde_json::Value,
        expected_generation: u64,
        deadline: Instant,
        canceled: &AtomicBool,
    ) -> Result<serde_json::Value, RuntimeError> {
        self.invoke_inner(
            method,
            params,
            expected_generation,
            deadline,
            canceled,
            None,
            response_limit_for_method(method),
        )
    }

    /// Staged package-v1 entry point used by host-state conformance. Ordinary package
    /// service invocations do not call it until production state leases are admitted.
    #[allow(dead_code)] // Removed when ordinary package services can receive state leases.
    pub(crate) fn invoke_with_host_state(
        &mut self,
        method: &str,
        params: serde_json::Value,
        expected_generation: u64,
        deadline: Instant,
        canceled: Arc<AtomicBool>,
        authorization: crate::extension_host_state::HostStateInvocationAuthorization,
    ) -> Result<serde_json::Value, RuntimeError> {
        self.invoke_with_host_authorization(
            method,
            params,
            expected_generation,
            deadline,
            canceled,
            HostInvocationAuthorization::LegacyState(Box::new(authorization)),
        )
    }

    /// Test-only package-v1 composite read entry point.
    #[cfg(test)]
    pub(crate) fn invoke_with_host_reads(
        &mut self,
        method: &str,
        params: serde_json::Value,
        expected_generation: u64,
        deadline: Instant,
        canceled: Arc<AtomicBool>,
        authorization: crate::extension_host_invocation::HostReadInvocationAuthorization,
    ) -> Result<serde_json::Value, RuntimeError> {
        self.invoke_with_host_authorization(
            method,
            params,
            expected_generation,
            deadline,
            canceled,
            HostInvocationAuthorization::Composite(Box::new(authorization)),
        )
    }

    /// Compatibility wrapper preserving the original staged terminal authorization semantics.
    #[allow(dead_code)]
    pub(crate) fn invoke_with_host_terminals(
        &mut self,
        method: &str,
        params: serde_json::Value,
        expected_generation: u64,
        deadline: Instant,
        canceled: Arc<AtomicBool>,
        authorization: crate::extension_host_terminals::HostTerminalsInvocationAuthorization,
    ) -> Result<serde_json::Value, RuntimeError> {
        self.invoke_with_host_authorization(
            method,
            params,
            expected_generation,
            deadline,
            canceled,
            HostInvocationAuthorization::LegacyTerminals(Box::new(authorization)),
        )
    }

    fn invoke_with_host_authorization(
        &mut self,
        method: &str,
        params: serde_json::Value,
        expected_generation: u64,
        deadline: Instant,
        canceled: Arc<AtomicBool>,
        authorization: HostInvocationAuthorization,
    ) -> Result<serde_json::Value, RuntimeError> {
        if !self.grants.installed_package {
            return Err(RuntimeError::Configuration(
                "legacy and preflight runtimes cannot receive package host-call leases".into(),
            ));
        }
        self.invoke_inner(
            method,
            params,
            expected_generation,
            deadline,
            canceled.as_ref(),
            Some((authorization, canceled.clone())),
            response_limit_for_method(method),
        )
    }

    #[cfg(test)]
    pub(crate) fn invoke_with_response_limit_for_test(
        &mut self,
        method: &str,
        params: serde_json::Value,
        expected_generation: u64,
        deadline: Instant,
        canceled: &AtomicBool,
        response_limit: usize,
    ) -> Result<serde_json::Value, RuntimeError> {
        self.invoke_inner(
            method,
            params,
            expected_generation,
            deadline,
            canceled,
            None,
            response_limit,
        )
    }

    fn invoke_inner(
        &mut self,
        method: &str,
        params: serde_json::Value,
        expected_generation: u64,
        deadline: Instant,
        canceled: &AtomicBool,
        host_authorization: Option<(HostInvocationAuthorization, Arc<AtomicBool>)>,
        response_limit: usize,
    ) -> Result<serde_json::Value, RuntimeError> {
        let response_limit = response_limit.min(MAX_RUNTIME_FRAME_BYTES);
        if expected_generation != self.identity.generation {
            return Err(RuntimeError::StaleGeneration {
                expected: self.identity.generation,
                actual: expected_generation,
            });
        }
        if canceled.load(std::sync::atomic::Ordering::Acquire) {
            return Err(RuntimeError::Canceled);
        }
        if deadline <= Instant::now() {
            return Err(RuntimeError::Timeout);
        }
        let request_id = self.next_request_id;
        self.next_request_id = self
            .next_request_id
            .checked_add(1)
            .ok_or_else(|| RuntimeError::Protocol("runtime request ID exhausted".into()))?;
        let request = RuntimeRequestV1 {
            contract: EXTENSION_SERVICE_CONTRACT_V1.into(),
            request_id: request_id.to_string(),
            identity: self.identity.clone(),
            method: method.into(),
            params,
        };
        let encoded = serde_json::to_vec(&request).map_err(|error| {
            RuntimeError::Protocol(format!("cannot audit runtime request: {error}"))
        })?;
        if encoded.len() > MAX_RUNTIME_FRAME_BYTES {
            return Err(RuntimeError::Protocol(
                "runtime request exceeds frame limit".into(),
            ));
        }
        let started = Instant::now();
        tracing::debug!(
            package_id = %self.identity.package_id,
            namespace = %self.identity.namespace,
            service_id = %self.identity.service_id,
            generation = self.identity.generation,
            request_id,
            method,
            deadline_ms = deadline.saturating_duration_since(Instant::now()).as_millis() as u64,
            "dispatching extension runtime request"
        );
        let host_calls = match host_authorization {
            #[cfg(test)]
            Some((HostInvocationAuthorization::Composite(authorization), canceled)) => {
                let observer = authorization.terminal_observations();
                let bound = authorization.bind(
                    request.request_id.clone(),
                    self.identity.clone(),
                    &self.grants.capabilities,
                    deadline,
                    canceled,
                )?;
                Arc::new(
                    RuntimeHostCallRouter::new(
                        bound.identity(),
                        bound.namespace(),
                        bound.allowed_methods(),
                        bound,
                    )
                    .with_observer(observer),
                )
            }
            Some((HostInvocationAuthorization::LegacyTerminals(authorization), canceled)) => {
                let event_tx = authorization.event_tx.clone();
                let lease = authorization.bind(
                    request.request_id.clone(),
                    self.identity.clone(),
                    deadline,
                    canceled,
                );
                let allowed_methods = BTreeMap::from([(
                    crate::extension_host_terminals::HOST_TERMINALS_SNAPSHOT_METHOD.into(),
                    crate::extension_host_terminals::HOST_TERMINALS_READ_CAPABILITY.into(),
                )]);
                Arc::new(RuntimeHostCallRouter::new(
                    self.identity.clone(),
                    self.identity.namespace.clone(),
                    allowed_methods,
                    crate::extension_host_terminals::HostTerminalsCallHandler::new(
                        self.identity.clone(),
                        self.grants.capabilities.clone(),
                        lease,
                        event_tx,
                    ),
                ))
            }
            Some((HostInvocationAuthorization::LegacyState(authorization), canceled)) => {
                let event_tx = authorization.event_tx.clone();
                let lease = authorization.bind(
                    request.request_id.clone(),
                    self.identity.clone(),
                    deadline,
                    canceled,
                );
                let allowed_methods = BTreeMap::from([
                    (
                        crate::extension_host_state::HOST_STATE_SNAPSHOT_METHOD.into(),
                        crate::extension_host_state::HOST_STATE_READ_CAPABILITY.into(),
                    ),
                    (
                        crate::extension_host_state::HOST_STATE_TRANSACT_METHOD.into(),
                        crate::extension_host_state::HOST_STATE_WRITE_CAPABILITY.into(),
                    ),
                ]);
                Arc::new(RuntimeHostCallRouter::new(
                    self.identity.clone(),
                    self.identity.namespace.clone(),
                    allowed_methods,
                    crate::extension_host_state::HostStateCallHandler::new(
                        self.identity.clone(),
                        self.grants.capabilities.clone(),
                        lease,
                        event_tx,
                    ),
                ))
            }
            None => {
                let state_capability = if self
                    .grants
                    .permits(crate::extension_host_state::HOST_STATE_READ_CAPABILITY)
                {
                    crate::extension_host_state::HOST_STATE_READ_CAPABILITY
                } else {
                    "state"
                };
                let terminals_capability = if self
                    .grants
                    .permits(crate::extension_host_terminals::HOST_TERMINALS_READ_CAPABILITY)
                {
                    crate::extension_host_terminals::HOST_TERMINALS_READ_CAPABILITY
                } else {
                    "terminal_snapshot"
                };
                let allowed_methods = BTreeMap::from([
                    (
                        crate::extension_host_state::HOST_STATE_SNAPSHOT_METHOD.into(),
                        state_capability.into(),
                    ),
                    (
                        crate::extension_host_terminals::HOST_TERMINALS_SNAPSHOT_METHOD.into(),
                        terminals_capability.into(),
                    ),
                ]);
                Arc::new(RuntimeHostCallRouter::new(
                    self.identity.clone(),
                    self.identity.namespace.clone(),
                    allowed_methods,
                    DisabledHostCalls {
                        identity: self.identity.clone(),
                        grants: self.grants.clone(),
                    },
                ))
            }
        };
        let mut result =
            self.runtime
                .invoke(&request, deadline, canceled, &host_calls, response_limit);
        if host_calls.call_limit_exceeded() {
            result = Err(RuntimeError::Protocol(
                "service exceeded the host-call bound".into(),
            ));
        }
        if result.as_ref().is_err_and(|error| error.requires_restart()) {
            let reason = match &result {
                Err(RuntimeError::Canceled) => RuntimeStopReason::Canceled,
                Err(RuntimeError::Timeout) => RuntimeStopReason::Deadline,
                _ => RuntimeStopReason::Protocol,
            };
            self.runtime.stop(reason);
        }
        if let Err(error) = &result {
            tracing::warn!(
                package_id = %self.identity.package_id,
                service_id = %self.identity.service_id,
                generation = self.identity.generation,
                request_id,
                method,
                error_code = error.code(),
                elapsed_ms = started.elapsed().as_millis() as u64,
                "extension runtime request failed"
            );
        } else {
            tracing::debug!(
                package_id = %self.identity.package_id,
                service_id = %self.identity.service_id,
                generation = self.identity.generation,
                request_id,
                method,
                elapsed_ms = started.elapsed().as_millis() as u64,
                "extension runtime request completed"
            );
        }
        result
    }

    pub(crate) fn stop(&mut self, reason: RuntimeStopReason) {
        self.runtime.stop(reason);
    }
}

fn response_limit_for_method(method: &str) -> usize {
    if method == "host.document.render" {
        MAX_HOST_DOCUMENT_RENDER_RESPONSE_BYTES
    } else {
        MAX_RUNTIME_FRAME_BYTES
    }
}

fn validate_granted_methods(
    methods: &BTreeSet<String>,
    granted: &[crate::api::schema::ExtensionServiceCapability],
) -> Result<(), RuntimeError> {
    use crate::api::schema::ExtensionServiceCapability;
    let requires_view = granted.contains(&ExtensionServiceCapability::ViewContribution);
    let requires_document = granted.contains(&ExtensionServiceCapability::HostDocument);
    let requires_action =
        requires_view && granted.contains(&ExtensionServiceCapability::TerminalInterrupt);
    let requires_choices = granted.contains(&ExtensionServiceCapability::ActionChoices);
    if (requires_view && !methods.contains("view.render"))
        || (requires_document && !methods.contains("host.document.render"))
        || (requires_action && !methods.contains("view.action"))
        || (requires_choices
            && (!methods.contains(crate::action_choices::ACTION_CHOICES_LIST_METHOD_V1)
                || !methods.contains(crate::action_choices::ACTION_CHOICE_INVOKE_METHOD_V1)))
    {
        return Err(RuntimeError::Protocol(
            "runtime does not support the methods required by its grants".into(),
        ));
    }
    Ok(())
}

impl Drop for ExtensionRuntimeDispatcher {
    fn drop(&mut self) {
        self.runtime.stop(RuntimeStopReason::Drop);
    }
}

#[cfg(test)]
fn handle_app_events_until<T: std::fmt::Debug>(
    app: &mut crate::app::App,
    context: &str,
    worker: std::thread::JoinHandle<T>,
    mut is_completion: impl FnMut(&crate::events::AppEvent) -> bool,
) -> std::thread::JoinHandle<T> {
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut handled = 0;
    loop {
        if Instant::now() >= deadline {
            fail_app_event_wait(context, handled, "timed out", worker);
        }
        match app.event_rx.try_recv() {
            Ok(event) => {
                let completed = is_completion(&event);
                app.handle_internal_event(event);
                handled += 1;
                if handled > crate::app::APP_EVENT_CHANNEL_CAPACITY {
                    fail_app_event_wait(context, handled, "event limit exceeded", worker);
                }
                if completed {
                    return worker;
                }
            }
            Err(tokio::sync::mpsc::error::TryRecvError::Empty) => std::thread::yield_now(),
            Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                fail_app_event_wait(context, handled, "app event channel disconnected", worker);
            }
        }
    }
}

#[cfg(test)]
fn host_state_actor_request_matches(
    event: &crate::events::AppEvent,
    outer_request_id: &str,
    identity: &RuntimeIdentityV1,
    call_id: &str,
    capability: &str,
    expected_method: &str,
) -> bool {
    let method_matches = |method: &crate::extension_host_state::HostStateActorMethod| match method {
        crate::extension_host_state::HostStateActorMethod::Snapshot => {
            expected_method == crate::extension_host_state::HOST_STATE_SNAPSHOT_METHOD
        }
        crate::extension_host_state::HostStateActorMethod::Transact(_) => {
            expected_method == crate::extension_host_state::HOST_STATE_TRANSACT_METHOD
        }
    };
    matches!(event,
        crate::events::AppEvent::ExtensionHostStateCall(request)
            if request.lease.outer_request_id == outer_request_id
                && request.lease.identity == *identity
                && request.call_id == call_id
                && request.capability == capability
                && method_matches(&request.method)
    )
}

#[cfg(test)]
fn host_terminals_actor_request_matches(
    event: &crate::events::AppEvent,
    outer_request_id: &str,
    identity: &RuntimeIdentityV1,
    call_id: &str,
    capability: &str,
    expected: crate::extension_host_terminals::HostTerminalsActorMethod,
) -> bool {
    matches!(event,
        crate::events::AppEvent::ExtensionHostTerminalsCall(request)
            if request.lease.outer_request_id == outer_request_id
                && request.lease.identity == *identity
                && request.call_id == call_id
                && request.capability == capability
                && request.method == expected
    )
}

#[cfg(test)]
fn take_host_terminals_actor_request(
    app: &mut crate::app::App,
    outer_request_id: &str,
    identity: &RuntimeIdentityV1,
    call_id: &str,
    capability: &str,
    expected: crate::extension_host_terminals::HostTerminalsActorMethod,
    context: &str,
) -> Box<crate::extension_host_terminals::HostTerminalsActorRequest> {
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut handled = 0;
    loop {
        assert!(Instant::now() < deadline, "timed out waiting for {context}");
        match app.event_rx.try_recv() {
            Ok(event)
                if host_terminals_actor_request_matches(
                    &event,
                    outer_request_id,
                    identity,
                    call_id,
                    capability,
                    expected,
                ) =>
            {
                let crate::events::AppEvent::ExtensionHostTerminalsCall(request) = event else {
                    unreachable!("matching helper only accepts terminal actor requests");
                };
                return request;
            }
            Ok(event) => {
                app.handle_internal_event(event);
                handled += 1;
                assert!(
                    handled <= crate::app::APP_EVENT_CHANNEL_CAPACITY,
                    "event limit exceeded waiting for {context}"
                );
            }
            Err(tokio::sync::mpsc::error::TryRecvError::Empty) => std::thread::yield_now(),
            Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                panic!("app event channel disconnected waiting for {context}");
            }
        }
    }
}

#[cfg(test)]
fn fail_app_event_wait<T: std::fmt::Debug>(
    context: &str,
    handled: usize,
    reason: &str,
    worker: std::thread::JoinHandle<T>,
) -> ! {
    if worker.is_finished() {
        panic!(
            "{reason} waiting for {context} after handling {handled} app events; worker result: {:?}",
            worker.join()
        );
    }
    panic!(
        "{reason} waiting for {context} after handling {handled} app events; worker is still running"
    );
}

struct DisabledHostCalls {
    identity: RuntimeIdentityV1,
    grants: RuntimeGrants,
}

impl RuntimeHostCallHandler for DisabledHostCalls {
    fn handle_host_call(&self, call: RuntimeHostCallV1) -> RuntimeHostCallResponseV1 {
        let error = if call.kind != "host_call"
            || call.identity != self.identity
            || call.depth == 0
            || call.depth > MAX_HOST_CALL_DEPTH
            || call.namespace != self.identity.namespace
            || call.namespace == "host"
            || call.namespace.starts_with("host.")
        {
            RuntimeErrorBodyV1 {
                code: "invalid_host_call".into(),
                message: "host call identity, namespace, or depth is invalid".into(),
            }
        } else if !self.grants.permits(&call.capability) {
            RuntimeErrorBodyV1 {
                code: "capability_denied".into(),
                message: "host call capability was not granted".into(),
            }
        } else {
            // The historical code/message is externally observable and retained unchanged.
            // "M2" here means production neutral host-call lease/admission remains unavailable.
            RuntimeErrorBodyV1 {
                code: "unsupported_before_m2".into(),
                message: "neutral host-call methods are disabled until M2".into(),
            }
        };
        tracing::warn!(
            package_id = %self.identity.package_id,
            service_id = %self.identity.service_id,
            generation = self.identity.generation,
            call_id = %call.call_id,
            capability = %call.capability,
            method = %call.method,
            error_code = %error.code,
            "extension runtime host call rejected"
        );
        RuntimeHostCallResponseV1 {
            kind: "host_response".into(),
            call_id: call.call_id,
            identity: self.identity.clone(),
            result: None,
            error: Some(error),
        }
    }
}

// Shared process/component conformance vectors for the runtime-neutral dispatcher.
#[allow(dead_code)]
#[derive(Clone, Debug, PartialEq)]
pub struct RuntimeConformanceVector {
    pub name: &'static str,
    pub method: &'static str,
    pub params: serde_json::Value,
    pub expected: serde_json::Value,
}

#[allow(dead_code)]
pub fn shared_conformance_vectors() -> Vec<RuntimeConformanceVector> {
    vec![
        RuntimeConformanceVector {
            name: "echo_object",
            method: "conformance.echo",
            params: serde_json::json!({"text": "vqro", "count": 2}),
            expected: serde_json::json!({"text": "vqro", "count": 2}),
        },
        RuntimeConformanceVector {
            name: "echo_empty_array",
            method: "conformance.echo",
            params: serde_json::json!([]),
            expected: serde_json::json!([]),
        },
        RuntimeConformanceVector {
            name: "document_projection",
            method: "conformance.document",
            params: serde_json::json!({"scope": "fixture"}),
            expected: serde_json::json!({"document_id": "fixture", "nodes": []}),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    struct EchoHostCalls;

    impl RuntimeHostCallHandler for EchoHostCalls {
        fn handle_host_call(&self, call: RuntimeHostCallV1) -> RuntimeHostCallResponseV1 {
            RuntimeHostCallResponseV1 {
                kind: "host_response".into(),
                call_id: call.call_id,
                identity: call.identity,
                result: Some(serde_json::Value::Null),
                error: None,
            }
        }
    }

    #[test]
    fn document_render_uses_contract_response_limit() {
        assert_eq!(
            response_limit_for_method("host.document.render"),
            crate::host_document_v2::MAX_DOCUMENT_V2_BYTES
        );
        assert_eq!(
            response_limit_for_method("conformance.echo"),
            MAX_RUNTIME_FRAME_BYTES
        );
    }

    #[test]
    fn host_call_router_normalizes_malformed_handler_responses() {
        struct MaliciousHostCalls {
            response: std::sync::Mutex<Option<RuntimeHostCallResponseV1>>,
        }
        impl RuntimeHostCallHandler for MaliciousHostCalls {
            fn handle_host_call(&self, _call: RuntimeHostCallV1) -> RuntimeHostCallResponseV1 {
                self.response.lock().unwrap().take().unwrap()
            }
        }
        let identity = RuntimeIdentityV1 {
            package_id: "example.runtime".into(),
            namespace: "example.runtime".into(),
            service_id: "echo".into(),
            generation: 7,
        };
        let valid = RuntimeHostCallResponseV1 {
            kind: "host_response".into(),
            call_id: "call-1".into(),
            identity: identity.clone(),
            result: Some(serde_json::Value::Null),
            error: None,
        };
        let mut cases = Vec::new();
        let mut wrong_kind = valid.clone();
        wrong_kind.kind = "response".into();
        cases.push(wrong_kind);
        let mut wrong_call = valid.clone();
        wrong_call.call_id = "other".into();
        cases.push(wrong_call);
        let mut wrong_identity = valid.clone();
        wrong_identity.identity.generation += 1;
        cases.push(wrong_identity);
        let mut neither = valid.clone();
        neither.result = None;
        cases.push(neither);
        let mut both = valid;
        both.error = Some(RuntimeErrorBodyV1 {
            code: "forged".into(),
            message: "forged".into(),
        });
        cases.push(both);

        for response in cases {
            let router = RuntimeHostCallRouter::new(
                identity.clone(),
                identity.namespace.clone(),
                BTreeMap::from([("state.snapshot".into(), "state".into())]),
                MaliciousHostCalls {
                    response: std::sync::Mutex::new(Some(response)),
                },
            );
            let normalized = router.handle(RuntimeHostCallV1 {
                kind: "host_call".into(),
                call_id: "call-1".into(),
                identity: identity.clone(),
                depth: 1,
                capability: "state".into(),
                method: "state.snapshot".into(),
                namespace: identity.namespace.clone(),
                params: serde_json::json!({}),
            });
            assert_eq!(normalized.kind, "host_response");
            assert_eq!(normalized.call_id, "call-1");
            assert_eq!(normalized.identity, identity);
            assert!(normalized.result.is_none());
            assert_eq!(normalized.error.unwrap().code, "invalid_host_response");
        }
    }

    #[test]
    fn host_call_router_centrally_binds_identity_namespace_depth_capability_and_count() {
        let identity = RuntimeIdentityV1 {
            package_id: "example.runtime".into(),
            namespace: "example.runtime".into(),
            service_id: "echo".into(),
            generation: 7,
        };
        let router = RuntimeHostCallRouter::new(
            identity.clone(),
            identity.namespace.clone(),
            BTreeMap::from([("state.snapshot".into(), "state".into())]),
            EchoHostCalls,
        );
        let call = |namespace: &str, capability: &str| RuntimeHostCallV1 {
            kind: "host_call".into(),
            call_id: "call-1".into(),
            identity: identity.clone(),
            depth: 1,
            capability: capability.into(),
            method: "state.snapshot".into(),
            namespace: namespace.into(),
            params: serde_json::json!({}),
        };
        assert_eq!(
            router
                .handle(call("another.namespace", "state"))
                .error
                .unwrap()
                .code,
            "invalid_host_call"
        );
        assert_eq!(
            router
                .handle(call("example.runtime", "terminal_snapshot"))
                .error
                .unwrap()
                .code,
            "capability_denied"
        );
        let mut wrong_depth = call("example.runtime", "state");
        wrong_depth.depth = 0;
        assert_eq!(
            router.handle(wrong_depth).error.unwrap().code,
            "invalid_host_call"
        );
        let mut wrong_generation = call("example.runtime", "state");
        wrong_generation.identity.generation += 1;
        assert_eq!(
            router.handle(wrong_generation).error.unwrap().code,
            "invalid_host_call"
        );
        let mut wrong_package = call("example.runtime", "state");
        wrong_package.identity.package_id = "other.package".into();
        assert_eq!(
            router.handle(wrong_package).error.unwrap().code,
            "invalid_host_call"
        );
        let mut wrong_method = call("example.runtime", "state");
        wrong_method.method = "state.transact".into();
        assert_eq!(
            router.handle(wrong_method).error.unwrap().code,
            "invalid_host_call"
        );
        assert!(router
            .handle(call("example.runtime", "state"))
            .error
            .is_none());
        let count_router = RuntimeHostCallRouter::new(
            identity.clone(),
            identity.namespace.clone(),
            BTreeMap::from([("state.snapshot".into(), "state".into())]),
            EchoHostCalls,
        );
        for _ in 0..MAX_HOST_CALLS_PER_INVOCATION {
            assert!(count_router
                .handle(call("example.runtime", "state"))
                .error
                .is_none());
        }
        assert_eq!(
            count_router
                .handle(call("example.runtime", "state"))
                .error
                .unwrap()
                .code,
            "resource_limit"
        );
    }

    #[test]
    fn host_call_router_allows_only_one_concurrently_outstanding_call() {
        struct BlockingHostCalls {
            entered: Arc<std::sync::Barrier>,
            release: Arc<std::sync::Barrier>,
        }
        impl RuntimeHostCallHandler for BlockingHostCalls {
            fn handle_host_call(&self, call: RuntimeHostCallV1) -> RuntimeHostCallResponseV1 {
                self.entered.wait();
                self.release.wait();
                RuntimeHostCallResponseV1 {
                    kind: "host_response".into(),
                    call_id: call.call_id,
                    identity: call.identity,
                    result: Some(serde_json::Value::Null),
                    error: None,
                }
            }
        }
        let identity = RuntimeIdentityV1 {
            package_id: "example.runtime".into(),
            namespace: "example.runtime".into(),
            service_id: "echo".into(),
            generation: 7,
        };
        let entered = Arc::new(std::sync::Barrier::new(2));
        let release = Arc::new(std::sync::Barrier::new(2));
        let router = Arc::new(RuntimeHostCallRouter::new(
            identity.clone(),
            identity.namespace.clone(),
            BTreeMap::from([("state.snapshot".into(), "state".into())]),
            BlockingHostCalls {
                entered: Arc::clone(&entered),
                release: Arc::clone(&release),
            },
        ));
        let call = || RuntimeHostCallV1 {
            kind: "host_call".into(),
            call_id: "call".into(),
            identity: identity.clone(),
            depth: 1,
            capability: "state".into(),
            method: "state.snapshot".into(),
            namespace: identity.namespace.clone(),
            params: serde_json::json!({}),
        };
        let worker_router = Arc::clone(&router);
        let first_call = call();
        let worker = std::thread::spawn(move || worker_router.handle(first_call));
        entered.wait();
        assert_eq!(router.handle(call()).error.unwrap().code, "host_call_busy");
        release.wait();
        assert!(worker.join().unwrap().error.is_none());
    }
}
