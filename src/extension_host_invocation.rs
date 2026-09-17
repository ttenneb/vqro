//! Staged package-bound composite read invocation context.
//!
//! This is internal runtime infrastructure only. Production package activation does not issue
//! these contexts. A context may expose the two public read methods that have landed so far, but
//! never state mutation.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{atomic::AtomicBool, Arc};
use std::time::Instant;

use crate::extension_runtime::{
    RuntimeError, RuntimeHostCallHandler, RuntimeHostCallResponseV1, RuntimeHostCallV1,
    RuntimeIdentityV1,
};

#[derive(Clone, Debug, Default)]
pub(crate) struct HostReadInvocationPurpose {
    pub(crate) state_snapshot: bool,
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
}

impl HostReadInvocationAuthorization {
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
            state.package_id == self.package_id
                && state.service_id == self.service_id
                && state.namespace == self.namespace
                && state.artifact_sha256 == self.artifact_sha256
                && state.artifact_version == self.artifact_version
                && state.catalog_revision == self.catalog_generation
                && state.package_revision == self.package_generation
                && state.manager_generation == self.service_generation
                && state.provider_generation == self.provider_generation
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
            || self.artifact_generation != self.runtime_generation
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

    #[test]
    fn atomic_staging_fails_without_installing_or_superseding_authority() {
        let (mut app, binding) = staged_app();
        let tab_id = app.state.workspaces[0].tabs[0].id.clone();
        let error = app
            .stage_host_read_authorization(
                &binding,
                HostReadInvocationPurpose {
                    state_snapshot: true,
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
                    state_snapshot: true,
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
                    state_snapshot: true,
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
