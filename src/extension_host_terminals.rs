//! Staged read-only `host.terminals.v1` package host-call contract.
//!
//! Production package activation does not issue this module's invocation leases.
//! Legacy services and package preflight therefore continue to receive disabled host calls.

use std::collections::{BTreeSet, HashSet};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc,
};
use std::time::{Duration, Instant};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::extension_runtime::{
    RuntimeErrorBodyV1, RuntimeHostCallHandler, RuntimeHostCallResponseV1, RuntimeHostCallV1,
    RuntimeIdentityV1,
};

pub const HOST_TERMINALS_CONTRACT_V1: &str = "host.terminals.v1";
pub const HOST_TERMINALS_READ_CAPABILITY: &str = "host.terminals.read";
pub const HOST_TERMINALS_SNAPSHOT_METHOD: &str = "terminals.snapshot";
pub const MAX_HOST_TERMINALS_RESPONSE_BYTES: usize = 64 * 1024;
const ACTOR_WAIT_POLL: Duration = Duration::from_millis(10);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HostTerminalsSnapshotParamsV1 {
    pub contract: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct HostTerminalsLeaseFenceV1 {
    pub catalog_generation: u64,
    pub package_generation: u64,
    pub service_generation: u64,
    pub artifact_generation: u64,
    pub provider_generation: u64,
    pub runtime_generation: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HostTerminalLayoutItemV1 {
    Tiled {
        terminal_id: crate::terminal::TerminalId,
    },
    Container {
        container_id: String,
        terminal_ids: Vec<crate::terminal::TerminalId>,
        selected_terminal_id: Option<crate::terminal::TerminalId>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct HostTerminalsSnapshotV1 {
    pub contract: String,
    pub workspace_id: String,
    pub tab_id: crate::workspace::TabId,
    pub lease_fence: HostTerminalsLeaseFenceV1,
    pub fingerprint_sha256: String,
    pub layout: Vec<HostTerminalLayoutItemV1>,
}

#[derive(Clone, Debug)]
pub(crate) struct HostTerminalsInvocationAuthorization {
    pub(crate) package_id: String,
    pub(crate) service_id: String,
    pub(crate) namespace: String,
    pub(crate) artifact_sha256: String,
    pub(crate) artifact_version: String,
    pub(crate) workspace_id: String,
    pub(crate) tab_id: crate::workspace::TabId,
    pub(crate) fence: HostTerminalsLeaseFenceV1,
    pub(crate) authority: Arc<crate::extension_host_state::HostInvocationAuthority>,
    pub(crate) event_tx: tokio::sync::mpsc::Sender<crate::events::AppEvent>,
}

#[derive(Clone, Debug)]
pub(crate) struct HostTerminalsInvocationLease {
    pub(crate) outer_request_id: String,
    pub(crate) identity: RuntimeIdentityV1,
    pub(crate) package_id: String,
    pub(crate) service_id: String,
    pub(crate) namespace: String,
    pub(crate) artifact_sha256: String,
    pub(crate) artifact_version: String,
    pub(crate) workspace_id: String,
    pub(crate) tab_id: crate::workspace::TabId,
    pub(crate) fence: HostTerminalsLeaseFenceV1,
    pub(crate) deadline: Instant,
    pub(crate) canceled: Arc<AtomicBool>,
    pub(crate) authority: Arc<crate::extension_host_state::HostInvocationAuthority>,
}

impl HostTerminalsInvocationAuthorization {
    pub(crate) fn bind(
        &self,
        outer_request_id: String,
        identity: RuntimeIdentityV1,
        deadline: Instant,
        canceled: Arc<AtomicBool>,
    ) -> HostTerminalsInvocationLease {
        HostTerminalsInvocationLease {
            outer_request_id,
            identity,
            package_id: self.package_id.clone(),
            service_id: self.service_id.clone(),
            namespace: self.namespace.clone(),
            artifact_sha256: self.artifact_sha256.clone(),
            artifact_version: self.artifact_version.clone(),
            workspace_id: self.workspace_id.clone(),
            tab_id: self.tab_id.clone(),
            fence: self.fence.clone(),
            deadline,
            canceled,
            authority: self.authority.clone(),
        }
    }
}

impl HostTerminalsInvocationLease {
    pub(crate) fn authority_current(&self) -> bool {
        self.authority.permits_generations(
            self.fence.service_generation,
            self.fence.provider_generation,
            self.fence.runtime_generation,
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HostTerminalsActorMethod {
    Capture,
    Validate,
}

#[derive(Debug)]
pub(crate) struct HostTerminalsActorRequest {
    pub(crate) lease: HostTerminalsInvocationLease,
    #[cfg(test)]
    pub(crate) call_id: String,
    #[cfg(test)]
    pub(crate) capability: String,
    pub(crate) method: HostTerminalsActorMethod,
    pub(crate) respond_to: mpsc::SyncSender<Result<serde_json::Value, RuntimeErrorBodyV1>>,
}

#[cfg(test)]
pub(crate) struct HostTerminalsSuccessGate {
    pub(crate) received: std::sync::Barrier,
    pub(crate) release: std::sync::Barrier,
}

#[cfg(test)]
impl HostTerminalsSuccessGate {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            received: std::sync::Barrier::new(2),
            release: std::sync::Barrier::new(2),
        })
    }
}

pub(crate) struct HostTerminalsCallHandler {
    identity: RuntimeIdentityV1,
    grants: BTreeSet<String>,
    lease: HostTerminalsInvocationLease,
    event_tx: tokio::sync::mpsc::Sender<crate::events::AppEvent>,
    #[cfg(test)]
    success_gate: Option<Arc<HostTerminalsSuccessGate>>,
}

impl HostTerminalsCallHandler {
    pub(crate) fn new(
        identity: RuntimeIdentityV1,
        grants: BTreeSet<String>,
        lease: HostTerminalsInvocationLease,
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
    pub(crate) fn with_success_gate(mut self, gate: Arc<HostTerminalsSuccessGate>) -> Self {
        self.success_gate = Some(gate);
        self
    }

    fn response_error(
        &self,
        call: &RuntimeHostCallV1,
        error: RuntimeErrorBodyV1,
    ) -> RuntimeHostCallResponseV1 {
        RuntimeHostCallResponseV1 {
            kind: "host_response".into(),
            call_id: call.call_id.clone(),
            identity: self.identity.clone(),
            result: None,
            error: Some(error),
        }
    }

    fn admitted(&self, call: &RuntimeHostCallV1) -> Result<(), RuntimeErrorBodyV1> {
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
            || call.identity.generation != self.lease.fence.runtime_generation
            || call.depth != 1
            || call.namespace != self.lease.namespace
            || call.namespace == "host"
            || call.namespace.starts_with("host.")
            || call.method != HOST_TERMINALS_SNAPSHOT_METHOD
        {
            return Err(invalid());
        }
        if call.capability != HOST_TERMINALS_READ_CAPABILITY
            || !self.grants.contains(HOST_TERMINALS_READ_CAPABILITY)
        {
            return Err(RuntimeErrorBodyV1 {
                code: "capability_denied".into(),
                message: "host.terminals.read was not granted".into(),
            });
        }
        let params: HostTerminalsSnapshotParamsV1 = serde_json::from_value(call.params.clone())
            .map_err(|error| RuntimeErrorBodyV1 {
                code: "invalid_host_call".into(),
                message: format!("invalid terminal snapshot parameters: {error}"),
            })?;
        if params.contract != HOST_TERMINALS_CONTRACT_V1 {
            return Err(invalid());
        }
        if !self.lease.authority_current() {
            return Err(stale_error("host call lease was revoked or superseded"));
        }
        Ok(())
    }

    fn actor_roundtrip(
        &self,
        call: &RuntimeHostCallV1,
        method: HostTerminalsActorMethod,
    ) -> Result<serde_json::Value, RuntimeErrorBodyV1> {
        #[cfg(not(test))]
        let _ = call;
        if self.lease.canceled.load(Ordering::Acquire) {
            return Err(canceled_error());
        }
        if Instant::now() >= self.lease.deadline {
            return Err(timeout_error());
        }
        let (respond_to, response) = mpsc::sync_channel(1);
        let request = HostTerminalsActorRequest {
            lease: self.lease.clone(),
            #[cfg(test)]
            call_id: call.call_id.clone(),
            #[cfg(test)]
            capability: call.capability.clone(),
            method,
            respond_to,
        };
        self.event_tx
            .try_send(crate::events::AppEvent::ExtensionHostTerminalsCall(
                Box::new(request),
            ))
            .map_err(|_| RuntimeErrorBodyV1 {
                code: "host_call_busy".into(),
                message: "host terminals actor queue is unavailable".into(),
            })?;
        loop {
            if self.lease.canceled.load(Ordering::Acquire) {
                return Err(canceled_error());
            }
            let Some(remaining) = self.lease.deadline.checked_duration_since(Instant::now()) else {
                return Err(timeout_error());
            };
            match response.recv_timeout(remaining.min(ACTOR_WAIT_POLL)) {
                Ok(result) => return result,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(RuntimeErrorBodyV1 {
                        code: "host_call_unavailable".into(),
                        message: "host terminals actor response channel closed".into(),
                    });
                }
            }
        }
    }
}

impl RuntimeHostCallHandler for HostTerminalsCallHandler {
    fn handle_host_call(&self, call: RuntimeHostCallV1) -> RuntimeHostCallResponseV1 {
        if let Err(error) = self.admitted(&call) {
            return self.response_error(&call, error);
        }
        if self.lease.canceled.load(Ordering::Acquire) {
            return self.response_error(&call, canceled_error());
        }
        if Instant::now() >= self.lease.deadline {
            return self.response_error(&call, timeout_error());
        }
        let result = match self.actor_roundtrip(&call, HostTerminalsActorMethod::Capture) {
            Ok(result) => result,
            Err(error) => return self.response_error(&call, error),
        };
        // Snapshot data linearized in the Capture actor turn. This second turn is
        // only a final authorization gate: authority/scope revocation processed
        // after capture can reject the captured result, but data is not recaptured.
        if let Err(error) = self.actor_roundtrip(&call, HostTerminalsActorMethod::Validate) {
            return self.response_error(&call, error);
        }
        #[cfg(test)]
        if let Some(gate) = &self.success_gate {
            gate.received.wait();
            gate.release.wait();
        }
        // Shared cancellation/deadline/revocation can still change concurrently;
        // recheck after the authorization gate without claiming a fence after return.
        if self.lease.canceled.load(Ordering::Acquire) {
            return self.response_error(&call, canceled_error());
        }
        if Instant::now() >= self.lease.deadline {
            return self.response_error(&call, timeout_error());
        }
        if !self.lease.authority_current() {
            return self.response_error(
                &call,
                stale_error("host call lease was revoked after final authorization gate"),
            );
        }
        RuntimeHostCallResponseV1 {
            kind: "host_response".into(),
            call_id: call.call_id,
            identity: self.identity.clone(),
            result: Some(result),
            error: None,
        }
    }
}

pub(crate) fn stale_error(message: &str) -> RuntimeErrorBodyV1 {
    RuntimeErrorBodyV1 {
        code: "stale_host_call_lease".into(),
        message: message.into(),
    }
}

fn canceled_error() -> RuntimeErrorBodyV1 {
    RuntimeErrorBodyV1 {
        code: "host_call_canceled".into(),
        message: "outer invocation was canceled".into(),
    }
}

fn timeout_error() -> RuntimeErrorBodyV1 {
    RuntimeErrorBodyV1 {
        code: "host_call_timeout".into(),
        message: "outer invocation deadline expired".into(),
    }
}

pub(crate) fn build_snapshot(
    workspace: &crate::workspace::Workspace,
    tab: &crate::workspace::Tab,
    terminals: &std::collections::HashMap<
        crate::terminal::TerminalId,
        crate::terminal::TerminalState,
    >,
    fence: HostTerminalsLeaseFenceV1,
) -> Result<HostTerminalsSnapshotV1, RuntimeErrorBodyV1> {
    // Scalar/count preflight happens before any response-sized allocation.
    if tab.panes.len() > crate::admission::MAX_TAB_PANES {
        return Err(resource_error("tab terminal admission bound was exceeded"));
    }
    let mut container_count = 0_usize;
    for container in tab.layout.collections() {
        container_count = container_count
            .checked_add(1)
            .ok_or_else(|| resource_error("container count overflowed"))?;
        if container.members().len() > crate::admission::MAX_GROUP_MEMBERS {
            return Err(resource_error(
                "container member admission bound was exceeded",
            ));
        }
    }
    if container_count > crate::admission::MAX_GROUPS_PER_TAB {
        return Err(resource_error("container admission bound was exceeded"));
    }
    let expected_container_ids: HashSet<_> = tab
        .layout
        .collections()
        .map(|container| container.id)
        .collect();
    if expected_container_ids.len() != container_count {
        return Err(malformed_error(
            "container map contains duplicate identities",
        ));
    }
    let leaves = tab.layout.leaves();
    if leaves.len() != tab.layout.leaf_count() {
        return Err(malformed_error("layout leaf traversal is inconsistent"));
    }

    let mut seen_panes = HashSet::with_capacity(tab.panes.len());
    let mut seen_terminals = HashSet::with_capacity(tab.panes.len());
    let mut seen_containers = HashSet::with_capacity(container_count);
    let mut layout = Vec::with_capacity(leaves.len());
    let terminal_for = |pane_id: crate::layout::PaneId| -> Result<crate::terminal::TerminalId, RuntimeErrorBodyV1> {
        let terminal_id = tab.panes.get(&pane_id)
            .map(|pane| pane.attached_terminal_id.clone())
            .ok_or_else(|| malformed_error("layout references a missing terminal placement"))?;
        if !terminals.contains_key(&terminal_id) {
            return Err(malformed_error("terminal placement references missing host state"));
        }
        Ok(terminal_id)
    };

    for leaf in leaves {
        match leaf {
            crate::layout::LayoutLeaf::Pane(pane_id) => {
                if !seen_panes.insert(pane_id) {
                    return Err(malformed_error("terminal placement is duplicated"));
                }
                let terminal_id = terminal_for(pane_id)?;
                if !seen_terminals.insert(terminal_id.clone()) {
                    return Err(malformed_error("terminal identity is duplicated"));
                }
                layout.push(HostTerminalLayoutItemV1::Tiled { terminal_id });
            }
            crate::layout::LayoutLeaf::Collection(container_id) => {
                if !seen_containers.insert(container_id) {
                    return Err(malformed_error("layout duplicates a container identity"));
                }
                let container = tab
                    .layout
                    .collection(container_id)
                    .ok_or_else(|| malformed_error("layout references a missing container"))?;
                if container.members().len() > crate::admission::MAX_GROUP_MEMBERS {
                    return Err(resource_error(
                        "container member admission bound was exceeded",
                    ));
                }
                let mut terminal_ids = Vec::with_capacity(container.members().len());
                for pane_id in container.members() {
                    if !seen_panes.insert(*pane_id) {
                        return Err(malformed_error("terminal placement is duplicated"));
                    }
                    let terminal_id = terminal_for(*pane_id)?;
                    if !seen_terminals.insert(terminal_id.clone()) {
                        return Err(malformed_error("terminal identity is duplicated"));
                    }
                    terminal_ids.push(terminal_id);
                }
                let selected_terminal_id = match container.selected() {
                    Some(pane_id) => Some(terminal_for(pane_id)?),
                    None if terminal_ids.is_empty() => None,
                    None => return Err(malformed_error("nonempty container has no selection")),
                };
                if selected_terminal_id
                    .as_ref()
                    .is_some_and(|selected| !terminal_ids.contains(selected))
                {
                    return Err(malformed_error("container selection is not a member"));
                }
                layout.push(HostTerminalLayoutItemV1::Container {
                    container_id: format!("container_{:016x}", container_id.raw()),
                    terminal_ids,
                    selected_terminal_id,
                });
            }
        }
    }
    if seen_panes.len() != tab.panes.len() || seen_containers != expected_container_ids {
        return Err(malformed_error(
            "a host terminal or exact container identity has no layout placement",
        ));
    }

    #[derive(Serialize)]
    struct FingerprintInput<'a> {
        contract: &'a str,
        workspace_id: &'a str,
        tab_id: &'a crate::workspace::TabId,
        lease_fence: &'a HostTerminalsLeaseFenceV1,
        layout: &'a [HostTerminalLayoutItemV1],
    }
    let canonical = serde_json::to_vec(&FingerprintInput {
        contract: HOST_TERMINALS_CONTRACT_V1,
        workspace_id: &workspace.id,
        tab_id: &tab.id,
        lease_fence: &fence,
        layout: &layout,
    })
    .map_err(|error| RuntimeErrorBodyV1 {
        code: "host_call_unavailable".into(),
        message: format!("cannot fingerprint terminal snapshot: {error}"),
    })?;
    let snapshot = HostTerminalsSnapshotV1 {
        contract: HOST_TERMINALS_CONTRACT_V1.into(),
        workspace_id: workspace.id.clone(),
        tab_id: tab.id.clone(),
        lease_fence: fence,
        fingerprint_sha256: format!("{:x}", Sha256::digest(&canonical)),
        layout,
    };
    let encoded = serde_json::to_vec(&snapshot).map_err(|error| RuntimeErrorBodyV1 {
        code: "host_call_unavailable".into(),
        message: format!("cannot encode terminal snapshot: {error}"),
    })?;
    if encoded.len() > MAX_HOST_TERMINALS_RESPONSE_BYTES {
        return Err(resource_error("terminal snapshot exceeds 64 KiB"));
    }
    Ok(snapshot)
}

fn malformed_error(message: &str) -> RuntimeErrorBodyV1 {
    RuntimeErrorBodyV1 {
        code: "malformed_host_state".into(),
        message: message.into(),
    }
}

fn resource_error(message: &str) -> RuntimeErrorBodyV1 {
    RuntimeErrorBodyV1 {
        code: "resource_limit".into(),
        message: message.into(),
    }
}

#[cfg(test)]
pub(crate) fn install_test_package_and_authorize(
    app: &mut crate::app::App,
    installed: &crate::extension_package::InstalledPackageV1,
    service_id: &str,
    namespace: &str,
    runtime_generation: u64,
) -> HostTerminalsInvocationAuthorization {
    let _ = crate::extension_host_state::install_test_package_and_authorize(
        app,
        installed,
        service_id,
        namespace,
        runtime_generation,
    );
    let workspace = crate::workspace::Workspace::test_new("host-terminals-transport");
    let workspace_id = workspace.id.clone();
    let tab_id = workspace.tabs[0].id.clone();
    for pane in workspace.tabs[0].panes.values() {
        let terminal_id = pane.attached_terminal_id.clone();
        app.state.terminals.insert(
            terminal_id.clone(),
            crate::terminal::TerminalState::new(terminal_id, "/".into()),
        );
    }
    app.state.workspaces = vec![workspace];
    let binding = crate::extension_host_state::HostProviderBinding::from_installed(
        installed, service_id, namespace,
    )
    .unwrap();
    app.stage_host_terminals_authorization(
        &binding,
        &workspace_id,
        &tab_id,
        5,
        7,
        runtime_generation,
    )
    .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::layout::Direction;

    fn fixture() -> (
        crate::workspace::Workspace,
        std::collections::HashMap<crate::terminal::TerminalId, crate::terminal::TerminalState>,
    ) {
        let mut workspace = crate::workspace::Workspace::test_new("neutral");
        workspace.test_split(Direction::Horizontal);
        let terminal_ids: Vec<_> = workspace.tabs[0]
            .panes
            .values()
            .map(|pane| pane.attached_terminal_id.clone())
            .collect();
        let terminals = terminal_ids
            .into_iter()
            .map(|id| {
                (
                    id.clone(),
                    crate::terminal::TerminalState::new(id, "/".into()),
                )
            })
            .collect();
        (workspace, terminals)
    }

    fn fence() -> HostTerminalsLeaseFenceV1 {
        HostTerminalsLeaseFenceV1 {
            catalog_generation: 1,
            package_generation: 2,
            service_generation: 3,
            artifact_generation: 4,
            provider_generation: 5,
            runtime_generation: 4,
        }
    }

    #[test]
    fn snapshot_is_coherent_pane_free_bounded_and_fingerprinted() {
        let (workspace, terminals) = fixture();
        let snapshot = build_snapshot(&workspace, &workspace.tabs[0], &terminals, fence()).unwrap();
        let encoded = serde_json::to_vec(&snapshot).unwrap();
        let text = String::from_utf8(encoded.clone()).unwrap();
        assert!(encoded.len() <= MAX_HOST_TERMINALS_RESPONSE_BYTES);
        assert!(!text.contains("pane"));
        assert!(!text.contains("collection_"));
        assert_eq!(snapshot.fingerprint_sha256.len(), 64);
        assert_eq!(snapshot.layout.len(), 2);
        let repeated = build_snapshot(&workspace, &workspace.tabs[0], &terminals, fence()).unwrap();
        assert_eq!(snapshot, repeated);
    }

    #[test]
    fn neutral_containers_preserve_identity_members_selection_and_empty_state() {
        let (mut workspace, terminals) = fixture();
        let tab = &mut workspace.tabs[0];
        let target = tab.layout.leaves()[0];
        let container = tab
            .create_collection_near(
                target,
                Direction::Vertical,
                0.5,
                Some("secret label".into()),
            )
            .unwrap();
        let empty = build_snapshot(&workspace, &workspace.tabs[0], &terminals, fence()).unwrap();
        assert!(empty.layout.iter().any(|item| matches!(item,
            HostTerminalLayoutItemV1::Container { container_id, terminal_ids, selected_terminal_id }
                if container_id.starts_with("container_") && !container_id.starts_with("collection_")
                    && terminal_ids.is_empty() && selected_terminal_id.is_none()
        )));

        let pane = workspace.tabs[0].layout.pane_ids()[0];
        workspace.tabs[0]
            .collect_tiled_pane(pane, container)
            .unwrap();
        let populated =
            build_snapshot(&workspace, &workspace.tabs[0], &terminals, fence()).unwrap();
        assert!(populated.layout.iter().any(|item| matches!(item,
            HostTerminalLayoutItemV1::Container { terminal_ids, selected_terminal_id, .. }
                if terminal_ids.len() == 1 && selected_terminal_id.as_ref() == terminal_ids.first()
        )));
    }

    #[test]
    fn admission_bounds_fail_before_snapshot_allocation() {
        let (mut workspace, mut terminals) = fixture();
        while workspace.tabs[0].panes.len() <= crate::admission::MAX_TAB_PANES {
            let pane_id = crate::layout::PaneId::alloc();
            let terminal_id = crate::terminal::TerminalId::alloc().unwrap();
            workspace.tabs[0]
                .panes
                .insert(pane_id, crate::pane::PaneState::new(terminal_id.clone()));
            terminals.insert(
                terminal_id.clone(),
                crate::terminal::TerminalState::new(terminal_id, "/".into()),
            );
        }
        let terminal_count = terminals.len();
        let error =
            build_snapshot(&workspace, &workspace.tabs[0], &terminals, fence()).unwrap_err();
        assert_eq!(error.code, "resource_limit");
        assert_eq!(terminals.len(), terminal_count);
    }

    #[test]
    fn malformed_duplicate_and_omitted_container_identities_fail_closed() {
        let (mut workspace, terminals) = fixture();
        let tab = &mut workspace.tabs[0];
        let target = tab.layout.leaves()[0];
        let first = tab
            .create_collection_near(target, Direction::Vertical, 0.5, None)
            .unwrap();
        let second = tab
            .create_collection_near(target, Direction::Vertical, 0.5, None)
            .unwrap();
        assert_ne!(first, second);
        let panes = tab.layout.pane_ids();
        let first_container = crate::terminal_container::TerminalContainerId::from(first);
        let root = crate::layout::TypedNode::Split {
            direction: Direction::Horizontal,
            ratio: 0.5,
            first: Box::new(crate::layout::TypedNode::Leaf(
                crate::layout::LayoutLeaf::Pane(panes[0]),
            )),
            second: Box::new(crate::layout::TypedNode::Split {
                direction: Direction::Vertical,
                ratio: 0.5,
                first: Box::new(crate::layout::TypedNode::Leaf(
                    crate::layout::LayoutLeaf::Pane(panes[1]),
                )),
                second: Box::new(crate::layout::TypedNode::Split {
                    direction: Direction::Horizontal,
                    ratio: 0.5,
                    first: Box::new(crate::layout::TypedNode::Leaf(
                        crate::layout::LayoutLeaf::Collection(first_container),
                    )),
                    second: Box::new(crate::layout::TypedNode::Leaf(
                        crate::layout::LayoutLeaf::Collection(first_container),
                    )),
                }),
            }),
        };
        tab.layout.replace_typed_root_for_test(root);
        let error =
            build_snapshot(&workspace, &workspace.tabs[0], &terminals, fence()).unwrap_err();
        assert_eq!(error.code, "malformed_host_state");
        assert!(error.message.contains("duplicates"));
    }

    #[test]
    fn malformed_identity_or_missing_host_terminal_fails_closed() {
        let (mut workspace, mut terminals) = fixture();
        let panes: Vec<_> = workspace.tabs[0].panes.keys().copied().collect();
        let duplicate = workspace.tabs[0].panes[&panes[0]]
            .attached_terminal_id
            .clone();
        let old = std::mem::replace(
            &mut workspace.tabs[0]
                .panes
                .get_mut(&panes[1])
                .unwrap()
                .attached_terminal_id,
            duplicate,
        );
        terminals.remove(&old);
        let error =
            build_snapshot(&workspace, &workspace.tabs[0], &terminals, fence()).unwrap_err();
        assert_eq!(error.code, "malformed_host_state");
    }

    #[test]
    fn schema_and_frozen_endpoints_expose_only_the_staged_read_contract() {
        let schema =
            serde_json::to_string(&crate::extension_spec::host_terminals_schema_document())
                .unwrap();
        for forbidden in [
            "pane_id",
            "collection_",
            "archive",
            "agent",
            "status",
            "policy",
            "effect",
            "event",
        ] {
            assert!(
                !schema.to_ascii_lowercase().contains(forbidden),
                "schema contains {forbidden}"
            );
        }
        assert!(schema.contains(HOST_TERMINALS_READ_CAPABILITY));
        assert!(schema.contains(HOST_TERMINALS_SNAPSHOT_METHOD));
        let native = include_str!("../docs/next/api/vqro-api.schema.json");
        let endpoint = include_str!("../tests/fixtures/endpoint-method-shapes-v1.json");
        assert!(!native.contains(HOST_TERMINALS_CONTRACT_V1));
        assert!(!endpoint.contains(HOST_TERMINALS_CONTRACT_V1));
        assert!(!endpoint.contains(HOST_TERMINALS_SNAPSHOT_METHOD));
    }

    fn staged_app() -> (
        crate::app::App,
        crate::extension_host_state::HostStateProviderBinding,
    ) {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = crate::app::App::new(
            &crate::config::Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        let (workspace, terminals) = fixture();
        app.state.workspaces = vec![workspace];
        app.state.terminals = terminals;
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
        let binding = crate::extension_host_state::HostStateProviderBinding::test_new(
            "example.package",
            "reader",
            "example.extension",
            "a".repeat(64),
            "1.0.0",
        );
        (app, binding)
    }

    fn staged_handler(
        app: &mut crate::app::App,
        binding: &crate::extension_host_state::HostStateProviderBinding,
        canceled: Arc<AtomicBool>,
    ) -> (
        HostTerminalsCallHandler,
        Arc<crate::extension_host_state::HostInvocationAuthority>,
    ) {
        let workspace_id = app.state.workspaces[0].id.clone();
        let tab_id = app.state.workspaces[0].tabs[0].id.clone();
        let authorization = app
            .stage_host_terminals_authorization(binding, &workspace_id, &tab_id, 5, 7, 11)
            .unwrap();
        let authority = authorization.authority.clone();
        let identity = RuntimeIdentityV1 {
            package_id: "example.package".into(),
            namespace: "example.extension".into(),
            service_id: "reader".into(),
            generation: 11,
        };
        let lease = authorization.bind(
            "outer-1".into(),
            identity.clone(),
            Instant::now() + Duration::from_secs(2),
            canceled,
        );
        (
            HostTerminalsCallHandler::new(
                identity,
                [HOST_TERMINALS_READ_CAPABILITY.to_string()].into(),
                lease,
                authorization.event_tx,
            ),
            authority,
        )
    }

    fn process_actor_turn(app: &mut crate::app::App, expected: HostTerminalsActorMethod) {
        let event = app.event_rx.blocking_recv().unwrap();
        let crate::events::AppEvent::ExtensionHostTerminalsCall(request) = event else {
            panic!("terminal actor request");
        };
        assert_eq!(request.method, expected);
        app.handle_extension_host_terminals_call(*request);
    }

    fn call() -> RuntimeHostCallV1 {
        RuntimeHostCallV1 {
            kind: "host_call".into(),
            call_id: "call-1".into(),
            identity: RuntimeIdentityV1 {
                package_id: "example.package".into(),
                namespace: "example.extension".into(),
                service_id: "reader".into(),
                generation: 11,
            },
            depth: 1,
            capability: HOST_TERMINALS_READ_CAPABILITY.into(),
            method: HOST_TERMINALS_SNAPSHOT_METHOD.into(),
            namespace: "example.extension".into(),
            params: serde_json::json!({"contract": HOST_TERMINALS_CONTRACT_V1}),
        }
    }

    #[test]
    fn actor_bridge_returns_one_coherent_scoped_snapshot() {
        let (mut app, binding) = staged_app();
        let (handler, _) = staged_handler(&mut app, &binding, Arc::new(AtomicBool::new(false)));
        let join = std::thread::spawn(move || handler.handle_host_call(call()));
        process_actor_turn(&mut app, HostTerminalsActorMethod::Capture);
        process_actor_turn(&mut app, HostTerminalsActorMethod::Validate);
        let response = join.join().unwrap();
        assert!(response.error.is_none());
        let result = response.result.unwrap();
        assert_eq!(result["workspace_id"], app.state.workspaces[0].id);
        assert_eq!(
            result["tab_id"],
            app.state.workspaces[0].tabs[0].id.as_str()
        );
    }

    #[test]
    fn topology_change_between_turns_returns_exact_capture_and_fingerprint() {
        let (mut app, binding) = staged_app();
        let (handler, _) = staged_handler(&mut app, &binding, Arc::new(AtomicBool::new(false)));
        let fence = handler.lease.fence.clone();
        let captured = build_snapshot(
            &app.state.workspaces[0],
            &app.state.workspaces[0].tabs[0],
            &app.state.terminals,
            fence.clone(),
        )
        .unwrap();
        let join = std::thread::spawn(move || handler.handle_host_call(call()));
        process_actor_turn(&mut app, HostTerminalsActorMethod::Capture);

        let pane_id = app.state.workspaces[0].test_split(Direction::Vertical);
        let terminal_id = app.state.workspaces[0].tabs[0]
            .terminal_id(pane_id)
            .unwrap()
            .clone();
        app.state.terminals.insert(
            terminal_id.clone(),
            crate::terminal::TerminalState::new(terminal_id, "/".into()),
        );
        let changed = build_snapshot(
            &app.state.workspaces[0],
            &app.state.workspaces[0].tabs[0],
            &app.state.terminals,
            fence,
        )
        .unwrap();
        assert_ne!(captured.fingerprint_sha256, changed.fingerprint_sha256);

        process_actor_turn(&mut app, HostTerminalsActorMethod::Validate);
        let response = join.join().unwrap();
        assert!(response.error.is_none());
        let returned: HostTerminalsSnapshotV1 =
            serde_json::from_value(response.result.unwrap()).unwrap();
        assert_eq!(returned, captured);
        assert_ne!(returned.fingerprint_sha256, changed.fingerprint_sha256);
        assert_ne!(returned.layout, changed.layout);
    }

    #[test]
    fn expired_lease_never_reaches_actor() {
        let (mut app, binding) = staged_app();
        let (mut handler, _) = staged_handler(&mut app, &binding, Arc::new(AtomicBool::new(false)));
        handler.lease.deadline = Instant::now();
        let response = handler.handle_host_call(call());
        assert_eq!(response.error.unwrap().code, "host_call_timeout");
        assert!(app.event_rx.try_recv().is_err());
    }

    #[test]
    fn cancellation_and_stale_scope_never_capture_or_hide_terminals() {
        let (mut app, binding) = staged_app();
        let canceled = Arc::new(AtomicBool::new(true));
        let (handler, _) = staged_handler(&mut app, &binding, canceled);
        let response = handler.handle_host_call(call());
        assert_eq!(response.error.unwrap().code, "host_call_canceled");
        assert!(app.event_rx.try_recv().is_err());

        let terminal_count = app.state.terminals.len();
        let (handler, _) = staged_handler(&mut app, &binding, Arc::new(AtomicBool::new(false)));
        let join = std::thread::spawn(move || handler.handle_host_call(call()));
        app.state.workspaces[0].tabs[0].id = crate::workspace::TabId::alloc().unwrap();
        let event = app.event_rx.blocking_recv().unwrap();
        let crate::events::AppEvent::ExtensionHostTerminalsCall(request) = event else {
            panic!("terminal actor request");
        };
        app.handle_extension_host_terminals_call(*request);
        assert_eq!(
            join.join().unwrap().error.unwrap().code,
            "stale_host_call_lease"
        );
        assert_eq!(
            app.state.terminals.len(),
            terminal_count,
            "malformed/stale reads must not hide PTYs"
        );
    }

    #[test]
    fn catalog_artifact_and_scope_changes_before_final_authorization_fail() {
        for case in ["catalog", "artifact", "scope"] {
            let (mut app, binding) = staged_app();
            let (handler, _) = staged_handler(&mut app, &binding, Arc::new(AtomicBool::new(false)));
            let join = std::thread::spawn(move || handler.handle_host_call(call()));
            process_actor_turn(&mut app, HostTerminalsActorMethod::Capture);
            match case {
                "catalog" => app.marketplace_state.packages.revision += 1,
                "artifact" => {
                    app.marketplace_state
                        .packages
                        .packages
                        .get_mut("example.package")
                        .unwrap()
                        .active
                        .as_mut()
                        .unwrap()
                        .package_sha256 = "b".repeat(64);
                }
                "scope" => {
                    app.state.workspaces[0].tabs[0].id = crate::workspace::TabId::alloc().unwrap();
                }
                _ => unreachable!(),
            }
            process_actor_turn(&mut app, HostTerminalsActorMethod::Validate);
            assert_eq!(
                join.join().unwrap().error.unwrap().code,
                "stale_host_call_lease",
                "{case} replacement must be observed by the final authorization gate"
            );
        }
    }

    #[test]
    fn future_lifecycle_retirement_owns_shared_authority_revocation() {
        let (mut app, binding) = staged_app();
        let (_, authority) = staged_handler(&mut app, &binding, Arc::new(AtomicBool::new(false)));
        assert!(authority.permits_generations(5, 7, 11));
        app.revoke_host_invocation_authority("example.package", "reader");
        assert!(!authority.permits_generations(5, 7, 11));
        assert!(!app
            .host_invocation_authorities
            .contains_key(&("example.package".into(), "reader".into())));
    }

    #[test]
    fn shared_revocation_rechecked_after_final_authorization_gate() {
        let (mut app, binding) = staged_app();
        let (handler, authority) =
            staged_handler(&mut app, &binding, Arc::new(AtomicBool::new(false)));
        let gate = HostTerminalsSuccessGate::new();
        let handler = handler.with_success_gate(gate.clone());
        let join = std::thread::spawn(move || handler.handle_host_call(call()));
        process_actor_turn(&mut app, HostTerminalsActorMethod::Capture);
        process_actor_turn(&mut app, HostTerminalsActorMethod::Validate);
        gate.received.wait();
        authority.revoke();
        gate.release.wait();
        assert_eq!(
            join.join().unwrap().error.unwrap().code,
            "stale_host_call_lease"
        );
    }

    #[test]
    fn process_and_component_share_the_canonical_terminal_handler() {
        let dispatcher = include_str!("extension_runtime/mod.rs");
        let composite = include_str!("extension_host_invocation.rs");
        let process = include_str!("extension_runtime/process.rs");
        let component = include_str!("extension_runtime/component.rs");
        assert_eq!(
            dispatcher.matches("HostTerminalsCallHandler::new").count(),
            1
        );
        assert_eq!(
            composite.matches("HostTerminalsCallHandler::new").count(),
            1
        );
        assert!(!process.contains("HostTerminalsCallHandler"));
        assert!(!component.contains("HostTerminalsCallHandler"));
    }

    #[test]
    fn snapshot_construction_is_not_reachable_from_render_or_layout_paths() {
        let render = include_str!("server/headless/render.rs");
        let layout = include_str!("layout.rs");
        let ui = include_str!("ui.rs");
        for source in [render, layout, ui] {
            assert!(!source.contains("build_snapshot("));
            assert!(!source.contains("HostTerminalsSnapshotV1"));
        }
    }
}
