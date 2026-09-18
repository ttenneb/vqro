//! Staged package-bound composite read invocation context.
//!
//! This is internal runtime infrastructure only. Ordinary production package-service invocations
//! do not issue these contexts. A context may expose the two staged read methods that have landed
//! so far, but never state mutation.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{atomic::AtomicBool, Arc};
use std::time::Instant;

use crate::extension_runtime::{
    RuntimeError, RuntimeHostCallHandler, RuntimeHostCallResponseV1, RuntimeHostCallV1,
    RuntimeIdentityV1,
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum HostStateReadSource {
    #[default]
    None,
    Durable,
    CollectionsShadow,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct HostReadInvocationPurpose {
    pub(crate) state: HostStateReadSource,
    pub(crate) terminals: Option<(String, crate::workspace::TabId)>,
}

#[derive(Clone, Debug)]
pub(crate) struct HostReadInvocationAuthorization {
    pub(crate) package_id: String,
    pub(crate) service_id: String,
    pub(crate) namespace: String,
    pub(crate) artifact_sha256: String,
    pub(crate) artifact_version: String,
    pub(crate) catalog_generation: u64,
    pub(crate) package_generation: u64,
    pub(crate) service_generation: u64,
    pub(crate) artifact_generation: u64,
    pub(crate) provider_generation: u64,
    pub(crate) runtime_generation: u64,
    pub(crate) state: Option<crate::extension_host_state::HostStateInvocationAuthorization>,
    pub(crate) terminals:
        Option<crate::extension_host_terminals::HostTerminalsInvocationAuthorization>,
    pub(crate) allowed_methods: BTreeMap<String, String>,
    pub(crate) authority: Arc<crate::extension_host_state::HostInvocationAuthority>,
    pub(crate) terminal_observations: TrustedTerminalSnapshotObservations,
}

impl HostReadInvocationAuthorization {
    pub(crate) fn terminal_observations(&self) -> TrustedTerminalSnapshotObservations {
        self.terminal_observations.clone()
    }

    pub(crate) fn bind(
        self,
        outer_request_id: String,
        identity: RuntimeIdentityV1,
        granted_capabilities: &BTreeSet<String>,
        deadline: Instant,
        canceled: Arc<AtomicBool>,
    ) -> Result<BoundHostReadInvocation, RuntimeError> {
        let mut expected_methods = BTreeMap::new();
        if self.state.is_some() {
            expected_methods.insert(
                crate::extension_host_state::HOST_STATE_SNAPSHOT_METHOD.into(),
                crate::extension_host_state::HOST_STATE_READ_CAPABILITY.into(),
            );
        }
        if self.terminals.is_some() {
            expected_methods.insert(
                crate::extension_host_terminals::HOST_TERMINALS_SNAPSHOT_METHOD.into(),
                crate::extension_host_terminals::HOST_TERMINALS_READ_CAPABILITY.into(),
            );
        }
        let state_matches = self.state.as_ref().is_none_or(|state| {
            state.source != HostStateReadSource::None
                && state.package_id == self.package_id
                && state.service_id == self.service_id
                && state.namespace == self.namespace
                && state.artifact_sha256 == self.artifact_sha256
                && state.artifact_version == self.artifact_version
                && state.catalog_revision == self.catalog_generation
                && state.package_revision == self.package_generation
                && state.manager_generation == self.service_generation
                && state.provider_generation == self.provider_generation
                && state.artifact_generation == self.artifact_generation
                && Arc::ptr_eq(&state.authority, &self.authority)
        });
        let terminals_match = self.terminals.as_ref().is_none_or(|terminals| {
            terminals.package_id == self.package_id
                && terminals.service_id == self.service_id
                && terminals.namespace == self.namespace
                && terminals.artifact_sha256 == self.artifact_sha256
                && terminals.artifact_version == self.artifact_version
                && terminals.fence.catalog_generation == self.catalog_generation
                && terminals.fence.package_generation == self.package_generation
                && terminals.fence.service_generation == self.service_generation
                && terminals.fence.artifact_generation == self.artifact_generation
                && terminals.fence.provider_generation == self.provider_generation
                && terminals.fence.runtime_generation == self.runtime_generation
                && Arc::ptr_eq(&terminals.authority, &self.authority)
        });
        if outer_request_id.is_empty()
            || identity.package_id != self.package_id
            || identity.service_id != self.service_id
            || identity.namespace != self.namespace
            || identity.generation != self.runtime_generation
            || self.artifact_generation == 0
            || self.runtime_generation == 0
            || self.service_generation == 0
            || self.provider_generation == 0
            || self.catalog_generation == 0
            || self.package_generation == 0
            || self.artifact_sha256.is_empty()
            || self.artifact_version.is_empty()
            || expected_methods.is_empty()
            || self.allowed_methods != expected_methods
            || !state_matches
            || !terminals_match
            || !self.authority.permits_generations(
                self.service_generation,
                self.provider_generation,
                self.runtime_generation,
            )
        {
            return Err(RuntimeError::Configuration(
                "composite host-read authorization does not match the runtime binding".into(),
            ));
        }
        if self
            .allowed_methods
            .values()
            .any(|capability| !granted_capabilities.contains(capability))
        {
            return Err(RuntimeError::Configuration(
                "composite host-read purpose exceeds installed package grants".into(),
            ));
        }

        let state = self.state.map(|authorization| {
            let event_tx = authorization.event_tx.clone();
            let lease = authorization.bind(
                outer_request_id.clone(),
                identity.clone(),
                deadline,
                Arc::clone(&canceled),
            );
            crate::extension_host_state::HostStateCallHandler::new(
                identity.clone(),
                granted_capabilities.clone(),
                lease,
                event_tx,
            )
        });
        let terminals = self.terminals.map(|authorization| {
            let event_tx = authorization.event_tx.clone();
            let lease = authorization.bind(outer_request_id, identity.clone(), deadline, canceled);
            crate::extension_host_terminals::HostTerminalsCallHandler::new(
                identity.clone(),
                granted_capabilities.clone(),
                lease,
                event_tx,
            )
        });
        Ok(BoundHostReadInvocation {
            identity,
            namespace: self.namespace,
            allowed_methods: self.allowed_methods,
            state,
            terminals,
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CompositeSnapshotObservation {
    pub(crate) state: crate::extension_host_state::HostStateSnapshotV1,
    pub(crate) terminals: crate::extension_host_terminals::HostTerminalsSnapshotV1,
}

#[derive(Debug, Default)]
struct TerminalObservationState {
    expect_state_then_terminals: bool,
    trace: Vec<String>,
    state_snapshot: Option<crate::extension_host_state::HostStateSnapshotV1>,
    terminal_snapshot: Option<crate::extension_host_terminals::HostTerminalsSnapshotV1>,
    terminal_count: usize,
    invalid: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct TrustedTerminalSnapshotObservations {
    state: Arc<std::sync::Mutex<TerminalObservationState>>,
}

#[allow(dead_code)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TerminalObservationError {
    Zero,
    Multiple(usize),
    Invalid(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CompositeObservationError {
    Invalid(String),
}

impl crate::extension_runtime::RuntimeHostCallObserver for TrustedTerminalSnapshotObservations {
    fn observe_accepted(&self, method: &str, response: &RuntimeHostCallResponseV1) {
        let mut state = self.state.lock().expect("host-read observation lock");
        if !state.expect_state_then_terminals {
            if method != crate::extension_host_terminals::HOST_TERMINALS_SNAPSHOT_METHOD
                || response.error.is_some()
            {
                return;
            }
            let validation = response
                .result
                .clone()
                .ok_or_else(|| "accepted terminal response has no result".to_string())
                .and_then(crate::extension_host_terminals::validate_snapshot_value);
            match validation {
                Ok(snapshot) => {
                    state.trace.push(method.into());
                    state.terminal_count += 1;
                    state.terminal_snapshot = Some(snapshot);
                }
                Err(error) => {
                    if state.invalid.is_none() {
                        state.invalid = Some(error);
                    }
                }
            }
            return;
        }

        if state.trace.len() < 3 {
            state.trace.push(method.into());
        }
        if response.error.is_some() {
            if state.invalid.is_none() {
                state.invalid = Some(format!("{method} returned an error"));
            }
            return;
        }
        let result = response
            .result
            .clone()
            .ok_or_else(|| format!("{method} response has no result"));
        match method {
            crate::extension_host_state::HOST_STATE_SNAPSHOT_METHOD => {
                let validation = result.and_then(validate_state_snapshot_value);
                match validation {
                    Ok(snapshot) if state.state_snapshot.is_none() => {
                        state.state_snapshot = Some(snapshot)
                    }
                    Ok(_) => {
                        state
                            .invalid
                            .get_or_insert_with(|| "duplicate state snapshot".into());
                    }
                    Err(error) => {
                        state.invalid.get_or_insert(error);
                    }
                };
            }
            crate::extension_host_terminals::HOST_TERMINALS_SNAPSHOT_METHOD => {
                let validation =
                    result.and_then(crate::extension_host_terminals::validate_snapshot_value);
                match validation {
                    Ok(snapshot) if state.terminal_snapshot.is_none() => {
                        state.terminal_count += 1;
                        state.terminal_snapshot = Some(snapshot)
                    }
                    Ok(_) => {
                        state.terminal_count += 1;
                        state
                            .invalid
                            .get_or_insert_with(|| "duplicate terminal snapshot".into());
                    }
                    Err(error) => {
                        state.invalid.get_or_insert(error);
                    }
                };
            }
            _ => {
                state
                    .invalid
                    .get_or_insert_with(|| format!("unrelated observed method {method}"));
            }
        }
    }
}

fn validate_state_snapshot_value(
    value: serde_json::Value,
) -> Result<crate::extension_host_state::HostStateSnapshotV1, String> {
    let snapshot: crate::extension_host_state::HostStateSnapshotV1 =
        serde_json::from_value(value.clone())
            .map_err(|error| format!("invalid host.state.v1 snapshot: {error}"))?;
    if snapshot.contract != crate::extension_host_state::HOST_STATE_CONTRACT_V1
        || snapshot.store_generation == 0
        || !valid_state_store_id(&snapshot.store_id)
        || !crate::extension_state::valid_namespace(&snapshot.namespace)
    {
        return Err("invalid host.state.v1 snapshot identity".into());
    }
    if serde_json::to_value(&snapshot).map_err(|error| error.to_string())? != value {
        return Err("host.state.v1 snapshot is not canonical".into());
    }
    Ok(snapshot)
}

fn valid_state_store_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':'))
}

impl TrustedTerminalSnapshotObservations {
    pub(crate) fn state_then_terminals() -> Self {
        Self {
            state: Arc::new(std::sync::Mutex::new(TerminalObservationState {
                expect_state_then_terminals: true,
                ..TerminalObservationState::default()
            })),
        }
    }
    #[allow(dead_code)]
    pub(crate) fn exactly_one(
        &self,
    ) -> Result<crate::extension_host_terminals::HostTerminalsSnapshotV1, TerminalObservationError>
    {
        let state = self.state.lock().expect("terminal observation lock");
        if let Some(error) = &state.invalid {
            return Err(TerminalObservationError::Invalid(error.clone()));
        }
        match (&state.terminal_snapshot, state.terminal_count) {
            (None, _) => Err(TerminalObservationError::Zero),
            (Some(snapshot), 1) => Ok(snapshot.clone()),
            (Some(_), count) => Err(TerminalObservationError::Multiple(count)),
        }
    }

    pub(crate) fn exact_state_then_terminals(
        &self,
    ) -> Result<CompositeSnapshotObservation, CompositeObservationError> {
        let state = self.state.lock().expect("host-read observation lock");
        if !state.expect_state_then_terminals {
            return Err(CompositeObservationError::Invalid(
                "composite observation expectation is absent".into(),
            ));
        }
        if let Some(error) = &state.invalid {
            return Err(CompositeObservationError::Invalid(error.clone()));
        }
        if state.trace
            != [
                crate::extension_host_state::HOST_STATE_SNAPSHOT_METHOD,
                crate::extension_host_terminals::HOST_TERMINALS_SNAPSHOT_METHOD,
            ]
        {
            return Err(CompositeObservationError::Invalid(format!(
                "expected exact state.snapshot then terminals.snapshot trace, observed {:?}",
                state.trace
            )));
        }
        Ok(CompositeSnapshotObservation {
            state: state.state_snapshot.clone().ok_or_else(|| {
                CompositeObservationError::Invalid("state snapshot is absent".into())
            })?,
            terminals: state.terminal_snapshot.clone().ok_or_else(|| {
                CompositeObservationError::Invalid("terminal snapshot is absent".into())
            })?,
        })
    }
}

pub(crate) struct BoundHostReadInvocation {
    identity: RuntimeIdentityV1,
    namespace: String,
    allowed_methods: BTreeMap<String, String>,
    state: Option<crate::extension_host_state::HostStateCallHandler>,
    terminals: Option<crate::extension_host_terminals::HostTerminalsCallHandler>,
}

impl BoundHostReadInvocation {
    pub(crate) fn identity(&self) -> RuntimeIdentityV1 {
        self.identity.clone()
    }

    pub(crate) fn namespace(&self) -> String {
        self.namespace.clone()
    }

    pub(crate) fn allowed_methods(&self) -> BTreeMap<String, String> {
        self.allowed_methods.clone()
    }
}

impl RuntimeHostCallHandler for BoundHostReadInvocation {
    fn handle_host_call(&self, call: RuntimeHostCallV1) -> RuntimeHostCallResponseV1 {
        match call.method.as_str() {
            crate::extension_host_state::HOST_STATE_SNAPSHOT_METHOD => match &self.state {
                Some(handler) => handler.handle_host_call(call),
                None => unavailable(&self.identity, call),
            },
            crate::extension_host_terminals::HOST_TERMINALS_SNAPSHOT_METHOD => {
                match &self.terminals {
                    Some(handler) => handler.handle_host_call(call),
                    None => unavailable(&self.identity, call),
                }
            }
            _ => unavailable(&self.identity, call),
        }
    }
}

fn unavailable(identity: &RuntimeIdentityV1, call: RuntimeHostCallV1) -> RuntimeHostCallResponseV1 {
    RuntimeHostCallResponseV1 {
        kind: "host_response".into(),
        call_id: call.call_id,
        identity: identity.clone(),
        result: None,
        error: Some(crate::extension_runtime::RuntimeErrorBodyV1 {
            code: "invalid_host_call".into(),
            message: "host call method is outside the composite read purpose".into(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extension_runtime::RuntimeHostCallRouter;

    fn staged_app() -> (
        crate::app::App,
        crate::extension_host_state::HostProviderBinding,
    ) {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = crate::app::App::new(
            &crate::config::Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        let workspace = crate::workspace::Workspace::test_new("composite-host-read");
        for pane in workspace.tabs[0].panes.values() {
            let terminal_id = pane.attached_terminal_id.clone();
            app.state.terminals.insert(
                terminal_id.clone(),
                crate::terminal::TerminalState::new(terminal_id, "/".into()),
            );
        }
        app.state.workspaces = vec![workspace];
        app.marketplace_state.packages.revision = 1;
        app.marketplace_state.packages.packages.insert(
            "example.package".into(),
            crate::extension_package::PackageLifecycleRecordV1 {
                package_id: "example.package".into(),
                source: crate::extension_package::PackageProvenanceV1::File {
                    package_sha256: "a".repeat(64),
                },
                previous_source: None,
                namespaces: vec!["example.extension".into()],
                revision: 2,
                status: crate::extension_package::PackageLifecycleStatusV1::Enabled,
                active: Some(crate::extension_package::PackageArtifactPointerV1 {
                    package_sha256: "a".repeat(64),
                    version: "1.0.0".into(),
                    runtime_generation: 11,
                }),
                previous: None,
                activation_error: None,
                pending_activation: None,
                retained_data: false,
            },
        );
        let binding = crate::extension_host_state::HostProviderBinding::test_new(
            "example.package",
            "reader",
            "example.extension",
            "a".repeat(64),
            "1.0.0",
        );
        (app, binding)
    }

    fn call(identity: &RuntimeIdentityV1, capability: &str, method: &str) -> RuntimeHostCallV1 {
        let contract = match method {
            crate::extension_host_state::HOST_STATE_SNAPSHOT_METHOD => "host.state.v1",
            _ => "host.terminals.v1",
        };
        RuntimeHostCallV1 {
            kind: "host_call".into(),
            call_id: method.into(),
            identity: identity.clone(),
            depth: 1,
            capability: capability.into(),
            method: method.into(),
            namespace: identity.namespace.clone(),
            params: serde_json::json!({"contract": contract}),
        }
    }

    #[derive(Clone)]
    struct ScriptedResponse(RuntimeHostCallResponseV1);

    impl RuntimeHostCallHandler for ScriptedResponse {
        fn handle_host_call(&self, _call: RuntimeHostCallV1) -> RuntimeHostCallResponseV1 {
            self.0.clone()
        }
    }

    fn observation_identity() -> RuntimeIdentityV1 {
        RuntimeIdentityV1 {
            package_id: "example.package".into(),
            namespace: "example.extension".into(),
            service_id: "reader".into(),
            generation: 4,
        }
    }

    fn valid_snapshot() -> crate::extension_host_terminals::HostTerminalsSnapshotV1 {
        let mut snapshot = crate::extension_host_terminals::HostTerminalsSnapshotV1 {
            contract: crate::extension_host_terminals::HOST_TERMINALS_CONTRACT_V1.into(),
            workspace_id: "w1".into(),
            tab_id: crate::workspace::TabId::parse("tab_0123456789abcdef0123456789abcdef").unwrap(),
            lease_fence: crate::extension_host_terminals::HostTerminalsLeaseFenceV1 {
                catalog_generation: 1,
                package_generation: 2,
                service_generation: 3,
                artifact_generation: 4,
                provider_generation: 5,
                runtime_generation: 4,
            },
            fingerprint_sha256: String::new(),
            layout: Vec::new(),
        };
        snapshot.fingerprint_sha256 =
            crate::extension_host_terminals::snapshot_fingerprint(&snapshot).unwrap();
        snapshot
    }

    fn accepted_response(result: serde_json::Value) -> RuntimeHostCallResponseV1 {
        RuntimeHostCallResponseV1 {
            kind: "host_response".into(),
            call_id: crate::extension_host_terminals::HOST_TERMINALS_SNAPSHOT_METHOD.into(),
            identity: observation_identity(),
            result: Some(result),
            error: None,
        }
    }

    fn observation_router(
        response: RuntimeHostCallResponseV1,
    ) -> (
        RuntimeHostCallRouter,
        TrustedTerminalSnapshotObservations,
        RuntimeHostCallV1,
    ) {
        let identity = observation_identity();
        let observations = TrustedTerminalSnapshotObservations::default();
        let router = RuntimeHostCallRouter::new(
            identity.clone(),
            identity.namespace.clone(),
            BTreeMap::from([(
                crate::extension_host_terminals::HOST_TERMINALS_SNAPSHOT_METHOD.into(),
                crate::extension_host_terminals::HOST_TERMINALS_READ_CAPABILITY.into(),
            )]),
            ScriptedResponse(response),
        )
        .with_observer(observations.clone());
        let call = call(
            &identity,
            crate::extension_host_terminals::HOST_TERMINALS_READ_CAPABILITY,
            crate::extension_host_terminals::HOST_TERMINALS_SNAPSHOT_METHOD,
        );
        (router, observations, call)
    }

    fn canonicalize_snapshot_value(mut value: serde_json::Value) -> serde_json::Value {
        value["fingerprint_sha256"] = serde_json::Value::String(String::new());
        let snapshot: crate::extension_host_terminals::HostTerminalsSnapshotV1 =
            serde_json::from_value(value.clone()).unwrap();
        value["fingerprint_sha256"] = serde_json::Value::String(
            crate::extension_host_terminals::snapshot_fingerprint(&snapshot).unwrap(),
        );
        value
    }

    #[test]
    fn router_observation_detects_zero_and_multiple_successes() {
        let response = accepted_response(serde_json::to_value(valid_snapshot()).unwrap());
        let (router, observations, call) = observation_router(response);
        assert_eq!(
            observations.exactly_one(),
            Err(TerminalObservationError::Zero)
        );
        assert!(router.handle(call.clone()).error.is_none());
        assert_eq!(observations.exactly_one(), Ok(valid_snapshot()));
        assert!(router.handle(call).error.is_none());
        assert_eq!(
            observations.exactly_one(),
            Err(TerminalObservationError::Multiple(2))
        );
    }

    #[test]
    fn router_rejected_envelopes_never_poison_observations() {
        let valid = accepted_response(serde_json::to_value(valid_snapshot()).unwrap());
        let mut wrong_kind = valid.clone();
        wrong_kind.kind = "response".into();
        let mut wrong_call_id = valid.clone();
        wrong_call_id.call_id = "other".into();
        let mut wrong_identity = valid.clone();
        wrong_identity.identity.namespace = "other.extension".into();
        let mut both = valid.clone();
        both.error = Some(crate::extension_runtime::RuntimeErrorBodyV1 {
            code: "malicious".into(),
            message: "both".into(),
        });
        let mut neither = valid.clone();
        neither.result = None;
        for response in [wrong_kind, wrong_call_id, wrong_identity, both, neither] {
            let (router, observations, call) = observation_router(response);
            assert_eq!(
                router.handle(call).error.unwrap().code,
                "invalid_host_response"
            );
            assert_eq!(
                observations.exactly_one(),
                Err(TerminalObservationError::Zero)
            );
        }

        let mut failed = valid;
        failed.result = None;
        failed.error = Some(crate::extension_runtime::RuntimeErrorBodyV1 {
            code: "stale_host_call_lease".into(),
            message: "revoked".into(),
        });
        let (router, observations, call) = observation_router(failed);
        assert_eq!(
            router.handle(call).error.unwrap().code,
            "stale_host_call_lease"
        );
        assert_eq!(
            observations.exactly_one(),
            Err(TerminalObservationError::Zero)
        );
    }

    #[test]
    fn malformed_accepted_snapshots_poison_observations_as_invalid() {
        let base = serde_json::to_value(valid_snapshot()).unwrap();
        let terminal_a = "term_00000000000000000000000000000001";
        let terminal_b = "term_00000000000000000000000000000002";
        let mut cases = Vec::new();

        let mut unknown = base.clone();
        unknown["unknown"] = serde_json::json!(true);
        cases.push(unknown);
        let mut unknown_fence = base.clone();
        unknown_fence["lease_fence"]["unknown"] = serde_json::json!(true);
        cases.push(unknown_fence);
        let mut contract = base.clone();
        contract["contract"] = serde_json::json!("host.terminals.v2");
        cases.push(contract);
        let mut workspace = base.clone();
        workspace["workspace_id"] = serde_json::json!("workspace");
        cases.push(workspace);
        let mut oversized_workspace = base.clone();
        oversized_workspace["workspace_id"] = serde_json::json!(format!("w{}", "1".repeat(32)));
        cases.push(oversized_workspace);
        let mut tab = base.clone();
        tab["tab_id"] = serde_json::json!("tab_BAD");
        cases.push(tab);
        let mut fingerprint = base.clone();
        fingerprint["fingerprint_sha256"] = serde_json::json!("A".repeat(64));
        cases.push(fingerprint);
        let mut mismatch = base.clone();
        mismatch["fingerprint_sha256"] = serde_json::json!("a".repeat(64));
        cases.push(mismatch);
        for field in [
            "catalog_generation",
            "package_generation",
            "service_generation",
            "artifact_generation",
            "provider_generation",
            "runtime_generation",
        ] {
            let mut fence = base.clone();
            fence["lease_fence"][field] = serde_json::json!(0);
            cases.push(fence);
        }

        let mut bad_container = base.clone();
        bad_container["layout"] = serde_json::json!([{
            "kind": "container",
            "container_id": "container_BAD",
            "terminal_ids": [],
            "selected_terminal_id": null
        }]);
        cases.push(canonicalize_snapshot_value(bad_container));
        let mut unknown_layout_field = base.clone();
        unknown_layout_field["layout"] = serde_json::json!([{
            "kind": "tiled",
            "terminal_id": terminal_a,
            "unknown": true
        }]);
        cases.push(unknown_layout_field);
        let mut duplicate_terminal = base.clone();
        duplicate_terminal["layout"] = serde_json::json!([
            {"kind": "tiled", "terminal_id": terminal_a},
            {"kind": "tiled", "terminal_id": terminal_a}
        ]);
        cases.push(canonicalize_snapshot_value(duplicate_terminal));
        let mut duplicate_container = base.clone();
        duplicate_container["layout"] = serde_json::json!([
            {"kind": "container", "container_id": "container_0000000000000001", "terminal_ids": [], "selected_terminal_id": null},
            {"kind": "container", "container_id": "container_0000000000000001", "terminal_ids": [], "selected_terminal_id": null}
        ]);
        cases.push(canonicalize_snapshot_value(duplicate_container));
        let mut missing_selected = base;
        missing_selected["layout"] = serde_json::json!([{
            "kind": "container",
            "container_id": "container_0000000000000001",
            "terminal_ids": [terminal_a],
            "selected_terminal_id": terminal_b
        }]);
        cases.push(canonicalize_snapshot_value(missing_selected));

        for value in cases {
            let (router, observations, call) = observation_router(accepted_response(value));
            assert!(router.handle(call).error.is_none());
            assert!(matches!(
                observations.exactly_one(),
                Err(TerminalObservationError::Invalid(_))
            ));
        }
    }

    fn state_snapshot_value() -> serde_json::Value {
        serde_json::json!({
            "contract": "host.state.v1",
            "store_id": "session:0123456789abcdef",
            "store_generation": 1,
            "namespace": "example.extension",
            "revision": 0,
            "sequence": 0,
            "values": {"full": {"nested": true}}
        })
    }

    fn composite_observations() -> TrustedTerminalSnapshotObservations {
        TrustedTerminalSnapshotObservations::state_then_terminals()
    }

    fn observe_value(
        observations: &TrustedTerminalSnapshotObservations,
        method: &str,
        value: serde_json::Value,
    ) {
        crate::extension_runtime::RuntimeHostCallObserver::observe_accepted(
            observations,
            method,
            &accepted_response(value),
        );
    }

    #[test]
    fn composite_observation_requires_exact_successful_a_then_b_trace() {
        let observations = composite_observations();
        let state_value = state_snapshot_value();
        let terminal_value = serde_json::to_value(valid_snapshot()).unwrap();
        observe_value(
            &observations,
            crate::extension_host_state::HOST_STATE_SNAPSHOT_METHOD,
            state_value.clone(),
        );
        observe_value(
            &observations,
            crate::extension_host_terminals::HOST_TERMINALS_SNAPSHOT_METHOD,
            terminal_value.clone(),
        );
        let observed = observations.exact_state_then_terminals().unwrap();
        assert_eq!(
            serde_json::to_value(observed.state).unwrap(),
            state_value,
            "the complete state snapshot is retained"
        );
        assert_eq!(
            serde_json::to_value(observed.terminals).unwrap(),
            terminal_value
        );

        for methods in [
            vec!["terminals.snapshot", "state.snapshot"],
            vec!["state.snapshot"],
            vec!["state.snapshot", "terminals.snapshot", "state.snapshot"],
            vec!["state.snapshot", "state.snapshot", "terminals.snapshot"],
            vec!["state.snapshot", "unrelated.read", "terminals.snapshot"],
        ] {
            let observations = composite_observations();
            for method in methods {
                let value = if method == crate::extension_host_state::HOST_STATE_SNAPSHOT_METHOD {
                    state_snapshot_value()
                } else {
                    serde_json::to_value(valid_snapshot()).unwrap()
                };
                observe_value(&observations, method, value);
            }
            assert!(observations.exact_state_then_terminals().is_err());
        }
    }

    #[test]
    fn composite_observation_records_failed_and_malformed_successes() {
        let observations = composite_observations();
        let mut failed = accepted_response(state_snapshot_value());
        failed.result = None;
        failed.error = Some(crate::extension_runtime::RuntimeErrorBodyV1 {
            code: "state_failed".into(),
            message: "failed".into(),
        });
        crate::extension_runtime::RuntimeHostCallObserver::observe_accepted(
            &observations,
            crate::extension_host_state::HOST_STATE_SNAPSHOT_METHOD,
            &failed,
        );
        assert!(observations.exact_state_then_terminals().is_err());

        for (method, value) in [
            (
                crate::extension_host_state::HOST_STATE_SNAPSHOT_METHOD,
                serde_json::json!({"contract": "host.state.v1"}),
            ),
            (
                crate::extension_host_terminals::HOST_TERMINALS_SNAPSHOT_METHOD,
                serde_json::json!({"contract": "host.terminals.v1"}),
            ),
        ] {
            let observations = composite_observations();
            observe_value(&observations, method, value);
            assert!(observations.exact_state_then_terminals().is_err());
        }
    }

    #[test]
    fn atomic_staging_fails_without_installing_or_superseding_authority() {
        let (mut app, binding) = staged_app();
        let tab_id = app.state.workspaces[0].tabs[0].id.clone();
        let error = app
            .stage_host_read_authorization(
                &binding,
                HostReadInvocationPurpose {
                    state: crate::extension_host_invocation::HostStateReadSource::Durable,
                    terminals: Some(("missing".into(), tab_id)),
                },
                3,
                4,
                11,
            )
            .unwrap_err();
        assert!(error.contains("exact workspace and tab"));
        assert!(app.host_invocation_authorities.is_empty());
    }

    #[test]
    fn deadline_between_composite_calls_rejects_without_an_actor_request() {
        let (mut app, binding) = staged_app();
        let authorization = app
            .stage_host_read_authorization(
                &binding,
                HostReadInvocationPurpose {
                    state: crate::extension_host_invocation::HostStateReadSource::Durable,
                    terminals: None,
                },
                3,
                4,
                11,
            )
            .unwrap();
        let identity = RuntimeIdentityV1 {
            package_id: "example.package".into(),
            namespace: "example.extension".into(),
            service_id: "reader".into(),
            generation: 11,
        };
        let bound = authorization
            .bind(
                "outer".into(),
                identity.clone(),
                binding.capabilities(),
                Instant::now(),
                Arc::new(AtomicBool::new(false)),
            )
            .unwrap();
        let router = RuntimeHostCallRouter::new(
            bound.identity(),
            bound.namespace(),
            bound.allowed_methods(),
            bound,
        );
        let response = router.handle(call(
            &identity,
            crate::extension_host_state::HOST_STATE_READ_CAPABILITY,
            crate::extension_host_state::HOST_STATE_SNAPSHOT_METHOD,
        ));
        assert_eq!(response.error.unwrap().code, "host_call_timeout");
        assert!(app.event_rx.try_recv().is_err());
    }

    #[test]
    fn one_context_routes_both_reads_and_revocation_rejects_the_second() {
        let (mut app, binding) = staged_app();
        let workspace_id = app.state.workspaces[0].id.clone();
        let tab_id = app.state.workspaces[0].tabs[0].id.clone();
        let authorization = app
            .stage_host_read_authorization(
                &binding,
                HostReadInvocationPurpose {
                    state: crate::extension_host_invocation::HostStateReadSource::Durable,
                    terminals: Some((workspace_id, tab_id)),
                },
                3,
                4,
                11,
            )
            .unwrap();
        assert!(authorization.state.is_some());
        assert!(authorization.terminals.is_some());
        let authority = Arc::clone(&authorization.authority);
        let identity = RuntimeIdentityV1 {
            package_id: "example.package".into(),
            namespace: "example.extension".into(),
            service_id: "reader".into(),
            generation: 11,
        };
        let canceled = Arc::new(AtomicBool::new(false));
        let bound = authorization
            .bind(
                "outer".into(),
                identity.clone(),
                binding.capabilities(),
                Instant::now() + std::time::Duration::from_secs(2),
                Arc::clone(&canceled),
            )
            .unwrap();
        let router = Arc::new(RuntimeHostCallRouter::new(
            bound.identity(),
            bound.namespace(),
            bound.allowed_methods(),
            bound,
        ));
        let first_router = Arc::clone(&router);
        let first_identity = identity.clone();
        let first = std::thread::spawn(move || {
            first_router.handle(call(
                &first_identity,
                crate::extension_host_terminals::HOST_TERMINALS_READ_CAPABILITY,
                crate::extension_host_terminals::HOST_TERMINALS_SNAPSHOT_METHOD,
            ))
        });
        for expected in [
            crate::extension_host_terminals::HostTerminalsActorMethod::Capture,
            crate::extension_host_terminals::HostTerminalsActorMethod::Validate,
        ] {
            let event = app.event_rx.blocking_recv().unwrap();
            let crate::events::AppEvent::ExtensionHostTerminalsCall(request) = event else {
                panic!("terminal actor request");
            };
            assert_eq!(request.method, expected);
            app.handle_extension_host_terminals_call(*request);
        }
        assert!(first.join().unwrap().error.is_none());
        canceled.store(true, std::sync::atomic::Ordering::Release);
        let canceled_read = router.handle(call(
            &identity,
            crate::extension_host_state::HOST_STATE_READ_CAPABILITY,
            crate::extension_host_state::HOST_STATE_SNAPSHOT_METHOD,
        ));
        assert_eq!(canceled_read.error.unwrap().code, "host_call_canceled");
        assert!(app.event_rx.try_recv().is_err());
        canceled.store(false, std::sync::atomic::Ordering::Release);
        let transact = router.handle(call(
            &identity,
            crate::extension_host_state::HOST_STATE_WRITE_CAPABILITY,
            crate::extension_host_state::HOST_STATE_TRANSACT_METHOD,
        ));
        assert_eq!(transact.error.unwrap().code, "invalid_host_call");
        assert!(app.event_rx.try_recv().is_err());
        authority.revoke();
        let second = router.handle(call(
            &identity,
            crate::extension_host_state::HOST_STATE_READ_CAPABILITY,
            crate::extension_host_state::HOST_STATE_SNAPSHOT_METHOD,
        ));
        assert_eq!(second.error.unwrap().code, "stale_host_call_lease");
        assert!(app.event_rx.try_recv().is_err());
    }
}
