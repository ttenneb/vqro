//! Reservation-bound, unpublished labeled Collections creation.

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::{Duration, Instant};

use ratatui::layout::Direction;

use super::collections_candidate::{
    current_state_snapshot, current_terminal_snapshot, production_pin, topology_from_snapshot,
};
use super::App;
use crate::collections_extension_contract::*;
use crate::extension_host_invocation::{
    HostReadInvocationAuthorization, HostReadInvocationPurpose, HostStateReadSource,
    TrustedTerminalSnapshotObservations,
};
use crate::extension_state::{
    ExtensionStateOperationV1, ExtensionStateTransactionV1, EXTENSION_STATE_CONTRACT_V1,
};
use crate::host_document_v2::{
    host_state_snapshot_dependency, terminal_snapshot_dependency, DocumentProducerFenceV2,
    DocumentScopeV2, HostDocumentRenderParamsV2, HOST_DOCUMENT_RENDER_CONTRACT_V2,
};

const CREATE_ATTEMPT_BIT: u64 = 1 << 62;
const TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CreateStage {
    Profile,
    Actions,
    Plan,
}

struct PreparedCreateRead {
    authorization: HostReadInvocationAuthorization,
    state: crate::extension_host_state::HostStateSnapshotV1,
    terminals: crate::extension_host_terminals::HostTerminalsSnapshotV1,
    topology: CollectionsProfileTopologyV1,
    render_params: HostDocumentRenderParamsV2,
    deadline: Instant,
    canceled: Arc<AtomicBool>,
}

pub(crate) struct PendingCollectionsCreate {
    request_id: String,
    respond_to: std::sync::mpsc::Sender<String>,
    params: crate::api::schema::CollectionCreateParams,
    workspace_id: String,
    tab_id: crate::workspace::TabId,
    workspace_index: usize,
    tab_index: usize,
    pane_id: crate::layout::PaneId,
    reserved_id: crate::layout::CollectionId,
    container_id: String,
    sequence: u64,
    stage: CreateStage,
    authorization: HostReadInvocationAuthorization,
    observations: TrustedTerminalSnapshotObservations,
    expected_state: crate::extension_host_state::HostStateSnapshotV1,
    expected_terminals: Option<crate::extension_host_terminals::HostTerminalsSnapshotV1>,
    topology: Option<CollectionsProfileTopologyV1>,
    render_params: HostDocumentRenderParamsV2,
    profile: Option<CollectionsDocumentProfileV1>,
    actions: Option<CollectionsActionDeclarationsV1>,
    invocation: Option<CollectionsActionInvocationV1>,
    deadline: Instant,
    canceled: Arc<AtomicBool>,
}

impl PendingCollectionsCreate {
    pub(crate) fn cancel(self, message: &str) {
        self.canceled.store(true, Ordering::Release);
        let _ = self.respond_to.send(super::responses::encode_error(
            self.request_id,
            "collections_authority_unavailable",
            message,
        ));
    }
}

impl std::fmt::Debug for PendingCollectionsCreate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PendingCollectionsCreate")
            .field("request_id", &self.request_id)
            .field("stage", &self.stage)
            .field("container_id", &self.container_id)
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
pub(crate) struct CollectionsCreateCheckpointResult {
    pending: PendingCollectionsCreate,
    state: crate::extension_state::DurableExtensionStateV1,
    authority: CollectionsAuthorityV1,
    result: Result<(), String>,
}

impl App {
    pub(crate) fn defer_collections_labeled_create(
        &mut self,
        request_id: String,
        params: crate::api::schema::CollectionCreateParams,
        respond_to: std::sync::mpsc::Sender<String>,
    ) -> bool {
        match self.start_collections_labeled_create(request_id.clone(), params, respond_to.clone())
        {
            Ok(()) => true,
            Err(error) => {
                self.collections_mutations_blocked = false;
                let _ = respond_to.send(super::responses::encode_error(
                    request_id,
                    "collections_create_rejected",
                    &error,
                ));
                false
            }
        }
    }

    fn start_collections_labeled_create(
        &mut self,
        request_id: String,
        params: crate::api::schema::CollectionCreateParams,
        respond_to: std::sync::mpsc::Sender<String>,
    ) -> Result<(), String> {
        if !self.collections_package_runtime_available()
            || self.collections_mutations_blocked
            || self.extension_state_transaction_pending
        {
            return Err("authoritative labeled creation is unavailable or busy".into());
        }
        let (workspace_index, pane_id) = self
            .parse_current_public_pane_id(&params.target_pane_id)
            .ok_or("target pane was not found")?;
        let tab_index = self.state.workspaces[workspace_index]
            .find_tab_index_for_pane(pane_id)
            .ok_or("target tab was not found")?;
        let group_count = self.state.workspaces[workspace_index].tabs[tab_index]
            .layout
            .collections()
            .count();
        crate::admission::group(group_count).map_err(|error| error.to_string())?;
        let reserved_id =
            crate::layout::CollectionId::alloc().map_err(|_| "collection identity exhausted")?;
        let container_id =
            crate::terminal_container::format_terminal_container_id(reserved_id.into());
        let workspace_id = self.state.workspaces[workspace_index].id.clone();
        let tab_id = self.state.workspaces[workspace_index].tabs[tab_index]
            .id
            .clone();
        let sequence = self.reserve_collections_action_sequence()?;
        let prepared = self.prepare_create_read(&workspace_id, &tab_id, &container_id, sequence)?;
        let authorization = prepared.authorization;
        let expected_state = prepared.state;
        let expected_terminals = prepared.terminals;
        let topology = prepared.topology;
        let render_params = prepared.render_params;
        let deadline = prepared.deadline;
        let canceled = prepared.canceled;
        let attempt_id = CREATE_ATTEMPT_BIT | (sequence << 2);
        self.collections_mutations_blocked = true;
        self.collections_pending_creates.insert(
            attempt_id,
            PendingCollectionsCreate {
                request_id,
                respond_to,
                params,
                workspace_id,
                tab_id,
                workspace_index,
                tab_index,
                pane_id,
                reserved_id,
                container_id,
                sequence,
                stage: CreateStage::Profile,
                observations: authorization.terminal_observations(),
                authorization: authorization.clone(),
                expected_state,
                expected_terminals: Some(expected_terminals),
                topology: Some(topology),
                render_params: render_params.clone(),
                profile: None,
                actions: None,
                invocation: None,
                deadline,
                canceled: Arc::clone(&canceled),
            },
        );
        let submission = self.submit_create_work(
            attempt_id,
            COLLECTIONS_PROFILE_RENDER_METHOD_V1,
            serde_json::to_value(render_params).map_err(|e| e.to_string())?,
            authorization,
            deadline,
            canceled,
        );
        if submission.is_err() {
            self.collections_pending_creates.remove(&attempt_id);
        }
        submission
    }

    fn prepare_create_read(
        &mut self,
        workspace_id: &str,
        tab_id: &crate::workspace::TabId,
        container_id: &str,
        revision: u64,
    ) -> Result<PreparedCreateRead, String> {
        let pin = production_pin()?;
        let services = self
            .extension_package_services
            .get(&pin.package_id)
            .ok_or("Collections worker is absent")?;
        let binding = services
            .binding(&pin.service_id)
            .ok_or("Collections binding is absent")?
            .clone();
        let runtime_generation = services
            .runtime_generation(&pin.service_id)
            .ok_or("Collections runtime generation is absent")?;
        let record = &self.marketplace_state.packages.packages[&pin.package_id];
        let artifact_generation = record
            .active
            .as_ref()
            .ok_or("Collections artifact is absent")?
            .runtime_generation;
        let provider_generation = record.revision;
        let authorization = self.stage_host_read_authorization(
            &binding,
            HostReadInvocationPurpose {
                state: HostStateReadSource::Durable,
                terminals: Some((workspace_id.to_owned(), tab_id.clone())),
                provisional_container: Some(container_id.to_owned()),
            },
            artifact_generation,
            provider_generation,
            runtime_generation,
        )?;
        let state = current_state_snapshot(self, &authorization)?;
        let terminals = current_terminal_snapshot(self, &authorization, workspace_id, tab_id)?;
        let state_dependency = host_state_snapshot_dependency(
            &state.contract,
            &state.store_id,
            state.store_generation,
            &state.namespace,
            state.revision,
        )
        .map_err(|e| e.to_string())?;
        let terminals_dependency = terminal_snapshot_dependency(
            &tab_id.to_string(),
            &terminals.fingerprint_sha256,
            provider_generation,
        )
        .map_err(|e| e.to_string())?;
        let producer = DocumentProducerFenceV2 {
            package_id: pin.package_id,
            service_id: pin.service_id,
            artifact_sha256: pin.package_sha256,
            runtime_generation,
            provider_generation,
            scope_generation: provider_generation,
        };
        let pane_ids = self
            .state
            .workspaces
            .iter()
            .find(|w| w.id == workspace_id)
            .and_then(|w| w.tabs.iter().find(|t| &t.id == tab_id))
            .ok_or("reserved scope disappeared")?
            .panes
            .iter()
            .map(|(id, pane)| {
                (
                    pane.attached_terminal_id.clone(),
                    format!("pane_{}", id.raw()),
                )
            })
            .collect();
        let topology = topology_from_snapshot(
            &terminals,
            &pane_ids,
            producer.clone(),
            state_dependency,
            terminals_dependency,
        )?;
        let render_params = HostDocumentRenderParamsV2 {
            contract: HOST_DOCUMENT_RENDER_CONTRACT_V2.into(),
            producer,
            scope: DocumentScopeV2 {
                contract: COLLECTIONS_SCOPE_CONTRACT_V1.into(),
                scope_id: tab_id.to_string(),
            },
            requested_revision: revision,
        };
        render_params.validate().map_err(|e| e.to_string())?;
        let deadline = Instant::now() + TIMEOUT;
        Ok(PreparedCreateRead {
            authorization,
            state,
            terminals,
            topology,
            render_params,
            deadline,
            canceled: Arc::new(AtomicBool::new(false)),
        })
    }

    fn submit_create_work(
        &self,
        attempt_id: u64,
        method: &str,
        params: serde_json::Value,
        authorization: HostReadInvocationAuthorization,
        deadline: Instant,
        canceled: Arc<AtomicBool>,
    ) -> Result<(), String> {
        let pin = production_pin()?;
        self.extension_package_services
            .get(&pin.package_id)
            .ok_or("Collections worker was retired")?
            .invoke(
                &pin.service_id,
                attempt_id,
                method.into(),
                params,
                authorization,
                deadline,
                canceled,
            )
    }

    pub(crate) fn finish_collections_create_stage(
        &mut self,
        attempt_id: u64,
        result: Result<serde_json::Value, String>,
    ) -> bool {
        let Some(pending) = self.collections_pending_creates.remove(&attempt_id) else {
            return false;
        };
        let runtime_failed = result.is_err();
        let request_id = pending.request_id.clone();
        let respond_to = pending.respond_to.clone();
        let canceled = Arc::clone(&pending.canceled);
        let outcome = self.advance_collections_create(pending, result);
        if let Err(error) = outcome {
            canceled.store(true, Ordering::Release);
            self.collections_mutations_blocked = false;
            if runtime_failed {
                self.disable_collections_package_authority_runtime();
            }
            let _ = respond_to.send(super::responses::encode_error(
                request_id,
                "collections_create_rejected",
                &error,
            ));
        }
        true
    }

    fn validate_create_reads(&self, pending: &PendingCollectionsCreate) -> Result<(), String> {
        if pending.canceled.load(Ordering::Acquire) || Instant::now() >= pending.deadline {
            return Err("labeled creation expired or was canceled".into());
        }
        let observed = pending
            .observations
            .exact_state_then_terminals()
            .map_err(|e| format!("provisional read trace rejected: {e:?}"))?;
        let current_state = current_state_snapshot(self, &pending.authorization)?;
        let current_terminals = current_terminal_snapshot(
            self,
            &pending.authorization,
            &pending.workspace_id,
            &pending.tab_id,
        )?;
        if observed.state != pending.expected_state
            || current_state != pending.expected_state
            || pending.expected_terminals.as_ref() != Some(&observed.terminals)
            || pending.expected_terminals.as_ref() != Some(&current_terminals)
        {
            return Err("provisional observations changed".into());
        }
        Ok(())
    }

    fn advance_collections_create(
        &mut self,
        mut pending: PendingCollectionsCreate,
        result: Result<serde_json::Value, String>,
    ) -> Result<(), String> {
        match pending.stage {
            CreateStage::Profile => {
                self.validate_create_reads(&pending)?;
                let profile: CollectionsDocumentProfileV1 = serde_json::from_value(
                    result.map_err(|e| format!("provisional profile failed: {e}"))?,
                )
                .map_err(|e| e.to_string())?;
                profile
                    .validate(
                        pending
                            .topology
                            .as_ref()
                            .ok_or("provisional topology is absent")?,
                    )
                    .map_err(|e| e.to_string())?;
                if profile.document.producer != pending.render_params.producer
                    || profile.document.scope != pending.render_params.scope
                    || profile.document.revision != pending.render_params.requested_revision
                {
                    return Err("provisional profile did not echo authority".into());
                }
                pending.profile = Some(profile);
                let params =
                    serde_json::to_value(&pending.render_params).map_err(|e| e.to_string())?;
                self.resubmit_create(
                    pending,
                    CreateStage::Actions,
                    COLLECTIONS_ACTIONS_LIST_METHOD_V1,
                    params,
                )
            }
            CreateStage::Actions => {
                self.validate_create_reads(&pending)?;
                let actions: CollectionsActionDeclarationsV1 = serde_json::from_value(
                    result.map_err(|e| format!("provisional actions failed: {e}"))?,
                )
                .map_err(|e| e.to_string())?;
                actions
                    .validate_for(
                        pending
                            .profile
                            .as_ref()
                            .ok_or("provisional profile is absent")?,
                        pending
                            .topology
                            .as_ref()
                            .ok_or("provisional topology is absent")?,
                    )
                    .map_err(|e| e.to_string())?;
                let declaration = actions
                    .actions
                    .iter()
                    .find(|a| {
                        a.action == CollectionsActionKindV1::SetLabel
                            && a.id.ends_with(&pending.container_id)
                    })
                    .ok_or("provisional set_label declaration is absent")?;
                let state = self
                    .extension_state
                    .snapshot(COLLECTIONS_NAMESPACE)
                    .map_err(|e| format!("cannot inspect Collections state: {e:?}"))?;
                let invocation = CollectionsActionInvocationV1 {
                    contract: COLLECTIONS_ACTION_INVOCATION_CONTRACT_V1.into(),
                    ticket: format!("ticket:r3-create-{}", pending.sequence),
                    idempotency_key: format!("collections-r3-create-{}", pending.sequence),
                    document_revision: actions.document_revision,
                    actions_revision: actions.revision,
                    action_id: declaration.id.clone(),
                    expected_store_generation: state.store_generation,
                    expected_namespace_revision: state.revision,
                    dependency: CollectionsActionDependencyV1::Container {
                        container_id: pending.container_id.clone(),
                    },
                    input: CollectionsActionInputV1::SetLabel {
                        container_id: pending.container_id.clone(),
                        label: pending.params.label.clone(),
                    },
                };
                actions
                    .validate_invocation(&invocation)
                    .map_err(|e| e.to_string())?;
                pending.actions = Some(actions);
                pending.invocation = Some(invocation.clone());
                self.resubmit_create_state_only(
                    pending,
                    serde_json::to_value(invocation).map_err(|e| e.to_string())?,
                )
            }
            CreateStage::Plan => self.finish_create_plan(pending, result),
        }
    }

    fn resubmit_create(
        &mut self,
        mut pending: PendingCollectionsCreate,
        stage: CreateStage,
        method: &str,
        params: serde_json::Value,
    ) -> Result<(), String> {
        let prepared = self.prepare_create_read(
            &pending.workspace_id,
            &pending.tab_id,
            &pending.container_id,
            pending.sequence,
        )?;
        let auth = prepared.authorization;
        let deadline = prepared.deadline;
        let canceled = prepared.canceled;
        pending.stage = stage;
        pending.authorization = auth.clone();
        pending.observations = auth.terminal_observations();
        pending.expected_state = prepared.state;
        pending.expected_terminals = Some(prepared.terminals);
        pending.topology = Some(prepared.topology);
        pending.render_params = prepared.render_params;
        pending.deadline = deadline;
        pending.canceled = Arc::clone(&canceled);
        let id = CREATE_ATTEMPT_BIT
            | (pending.sequence << 2)
            | match stage {
                CreateStage::Actions => 1,
                _ => 0,
            };
        self.collections_pending_creates.insert(id, pending);
        let submission = self.submit_create_work(id, method, params, auth, deadline, canceled);
        if submission.is_err() {
            self.collections_pending_creates.remove(&id);
        }
        submission
    }

    fn resubmit_create_state_only(
        &mut self,
        mut pending: PendingCollectionsCreate,
        params: serde_json::Value,
    ) -> Result<(), String> {
        let pin = production_pin()?;
        let services = self
            .extension_package_services
            .get(&pin.package_id)
            .ok_or("Collections worker is absent")?;
        let binding = services
            .binding(&pin.service_id)
            .ok_or("Collections binding is absent")?
            .clone();
        let runtime = services
            .runtime_generation(&pin.service_id)
            .ok_or("runtime generation absent")?;
        let record = &self.marketplace_state.packages.packages[&pin.package_id];
        let artifact = record
            .active
            .as_ref()
            .ok_or("artifact absent")?
            .runtime_generation;
        let provider = record.revision;
        let auth = self.stage_host_read_authorization(
            &binding,
            HostReadInvocationPurpose {
                state: HostStateReadSource::Durable,
                terminals: None,
                provisional_container: None,
            },
            artifact,
            provider,
            runtime,
        )?;
        let state = current_state_snapshot(self, &auth)?;
        let deadline = Instant::now() + TIMEOUT;
        let canceled = Arc::new(AtomicBool::new(false));
        pending.stage = CreateStage::Plan;
        pending.authorization = auth.clone();
        pending.observations = auth.terminal_observations();
        pending.expected_state = state;
        pending.expected_terminals = None;
        pending.deadline = deadline;
        pending.canceled = Arc::clone(&canceled);
        let id = CREATE_ATTEMPT_BIT | (pending.sequence << 2) | 2;
        self.collections_pending_creates.insert(id, pending);
        let submission = self.submit_create_work(
            id,
            COLLECTIONS_ACTION_PLAN_METHOD_V1,
            params,
            auth,
            deadline,
            canceled,
        );
        if submission.is_err() {
            self.collections_pending_creates.remove(&id);
        }
        submission
    }

    fn finish_create_plan(
        &mut self,
        pending: PendingCollectionsCreate,
        result: Result<serde_json::Value, String>,
    ) -> Result<(), String> {
        if pending.canceled.load(Ordering::Acquire) || Instant::now() >= pending.deadline {
            return Err("labeled creation plan expired or was canceled".into());
        }
        let authority_current = self
            .host_invocation_authorities
            .get(&(
                pending.authorization.package_id.clone(),
                pending.authorization.service_id.clone(),
            ))
            .is_some_and(|authority| Arc::ptr_eq(authority, &pending.authorization.authority));
        if !authority_current {
            return Err("labeled creation authority was superseded".into());
        }
        let observed = pending
            .observations
            .exactly_one_state()
            .map_err(|e| format!("create plan read trace rejected: {e:?}"))?;
        let current = crate::extension_host_state::HostStateSnapshotV1::from(
            self.extension_state
                .snapshot(COLLECTIONS_NAMESPACE)
                .map_err(|e| format!("state unavailable: {e:?}"))?,
        );
        if observed != pending.expected_state || current != pending.expected_state {
            return Err("Collections state changed during create planning".into());
        }
        let plan: CollectionsEffectPlanV1 =
            serde_json::from_value(result.map_err(|e| format!("create plan failed: {e}"))?)
                .map_err(|e| e.to_string())?;
        plan.validate_for(
            pending
                .invocation
                .as_ref()
                .ok_or("create invocation absent")?,
        )
        .map_err(|e| e.to_string())?;
        let mut state = self.extension_state.clone();
        let commit = state
            .transact(&ExtensionStateTransactionV1 {
                contract: EXTENSION_STATE_CONTRACT_V1.into(),
                namespace: COLLECTIONS_NAMESPACE.into(),
                expected_store_generation: plan.expected_store_generation,
                expected_revision: plan.expected_namespace_revision,
                idempotency_key: plan.idempotency_key,
                operations: vec![ExtensionStateOperationV1 {
                    key: plan.state.key,
                    value: plan.state.value,
                }],
            })
            .map_err(|e| format!("create state CAS failed: {e:?}"))?;
        let mut authority = self.collections_authority.clone();
        authority.state = Some(CollectionsAuthorityStateV1 {
            store_generation: commit.store_generation,
            namespace_revision: commit.revision,
        });
        let mut session = self.capture_session_snapshot();
        crate::persist::reserve_empty_collection(
            &mut session,
            pending.workspace_index,
            pending.tab_index,
            pending.pane_id.raw(),
            pending.reserved_id,
            direction(&pending.params),
            pending.params.ratio.unwrap_or(0.5),
            pending.params.focus,
        )?;
        session.collections_authority = authority.clone();
        let event_tx = self.event_tx.clone();
        let worker_state = state.clone();
        let worker_authority = authority.clone();
        std::thread::spawn(move || {
            let result = crate::persist::persist_collections_checkpoint(&session, &worker_state)
                .map_err(|e| e.to_string());
            let _ = event_tx.blocking_send(
                crate::events::AppEvent::CollectionsCreateCheckpointPersisted(Box::new(
                    CollectionsCreateCheckpointResult {
                        pending,
                        state: worker_state,
                        authority: worker_authority,
                        result,
                    },
                )),
            );
        });
        Ok(())
    }

    pub(crate) fn finish_collections_create_checkpoint(
        &mut self,
        result: CollectionsCreateCheckpointResult,
    ) {
        self.collections_mutations_blocked = false;
        let pending = result.pending;
        match result.result {
            Ok(()) => {
                let located = self
                    .state
                    .workspaces
                    .get(pending.workspace_index)
                    .is_some_and(|w| w.id == pending.workspace_id)
                    && self.state.workspaces[pending.workspace_index]
                        .tabs
                        .get(pending.tab_index)
                        .is_some_and(|t| t.id == pending.tab_id);
                let applied = located
                    && self.state.workspaces[pending.workspace_index].tabs[pending.tab_index]
                        .create_collection_near_reserved(
                            crate::layout::LayoutLeaf::Pane(pending.pane_id),
                            direction(&pending.params),
                            pending.params.ratio.unwrap_or(0.5),
                            pending.reserved_id,
                        )
                        .is_ok();
                if !applied {
                    self.session_persistence_blocked = true;
                    self.extension_state_persistence_blocked = true;
                    self.disable_collections_package_authority_runtime();
                    let _ = pending.respond_to.send(super::responses::encode_error(
                        pending.request_id,
                        "collections_checkpoint_inconsistent",
                        "reserved topology changed during checkpoint",
                    ));
                    return;
                }
                if pending.params.focus {
                    let _ = self.state.workspaces[pending.workspace_index].tabs[pending.tab_index]
                        .focus_layout_leaf(crate::layout::LayoutLeaf::Collection(
                            pending.reserved_id.into(),
                        ));
                }
                self.extension_state = result.state;
                self.collections_authority = result.authority;
                self.state.session_dirty = false;
                self.session_save_deadline = None;
                let checkpointed_publication = pending
                    .profile
                    .clone()
                    .zip(pending.actions.clone())
                    .zip(pending.topology.clone())
                    .ok_or_else(|| "checkpointed create publication is incomplete".to_string())
                    .and_then(|((profile, actions), topology)| {
                        self.collections_candidate_audits
                            .publish_checkpointed_create(profile, actions, topology)
                    });
                if let Err(error) = checkpointed_publication {
                    self.session_persistence_blocked = true;
                    self.extension_state_persistence_blocked = true;
                    self.disable_collections_package_authority_runtime();
                    let _ = pending.respond_to.send(super::responses::encode_error(
                        pending.request_id,
                        "collections_checkpoint_inconsistent",
                        &error,
                    ));
                    return;
                }
                // Publish the checkpointed profile before scheduling its replacement. Beginning a
                // replacement retires the audit-cache entry, while the compatibility projection
                // intentionally remains visible until that replacement is accepted.
                self.publish_ready_collections_package_profiles();
                let Some(mut collection) = self.collection_info(
                    pending.workspace_index,
                    pending.tab_index,
                    pending.reserved_id,
                ) else {
                    self.session_persistence_blocked = true;
                    self.extension_state_persistence_blocked = true;
                    self.disable_collections_package_authority_runtime();
                    let _ = pending.respond_to.send(super::responses::encode_error(
                        pending.request_id,
                        "collections_checkpoint_inconsistent",
                        "checkpointed collection projection was not published",
                    ));
                    return;
                };
                collection.label.clone_from(&pending.params.label);
                self.refresh_collection_documents();
                self.emit_collection_event("created", collection.clone());
                let _ = pending.respond_to.send(super::responses::encode_success(
                    pending.request_id,
                    crate::api::schema::ResponseResult::CollectionInfo { collection },
                ));
            }
            Err(error) => {
                self.session_persistence_blocked = true;
                self.extension_state_persistence_blocked = true;
                self.disable_collections_package_authority_runtime();
                let _ = pending.respond_to.send(super::responses::encode_error(
                    pending.request_id,
                    "collections_checkpoint_failed",
                    &error,
                ));
            }
        }
    }
}

fn direction(params: &crate::api::schema::CollectionCreateParams) -> Direction {
    match params.direction {
        crate::api::schema::SplitDirection::Right => Direction::Horizontal,
        crate::api::schema::SplitDirection::Down => Direction::Vertical,
    }
}
