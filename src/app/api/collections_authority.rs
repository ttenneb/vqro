//! Atomic Collections package-authority cutover and compatibility publication.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::{Duration, Instant};

use super::collections_candidate::production_pin;
use super::App;
use crate::collections_extension_contract::{
    CollectionsActionDependencyV1, CollectionsActionInputV1, CollectionsActionInvocationV1,
    CollectionsActionKindV1, CollectionsAuthorityPackageV1, CollectionsAuthorityPhaseV1,
    CollectionsAuthorityStateV1, CollectionsAuthorityV1, CollectionsDocumentProfileV1,
    CollectionsEffectPlanV1, COLLECTIONS_ACTION_INVOCATION_CONTRACT_V1,
    COLLECTIONS_ACTION_PLAN_METHOD_V1, COLLECTIONS_NAMESPACE,
};
use crate::extension_state::{
    ExtensionStateOperationV1, ExtensionStateTransactionV1, EXTENSION_STATE_CONTRACT_V1,
};

// R3 admission is open only after the exact-state cutover, coordinator recovery, compatibility,
// transport, replacement, rollback, and fail-closed evidence recorded in the release ledger.
const COLLECTIONS_AUTHORITY_CUTOVER_ADMITTED: bool = true;

#[cfg(test)]
mod shutdown_fixture {
    // Test-only already-admitted planning/publication records. Package admission
    // is tested separately; no external artifact or authority is installed here.
    use super::*;

    impl App {
        pub(crate) fn test_shutdown_read_authorization(
            &self,
        ) -> crate::extension_host_invocation::HostReadInvocationAuthorization {
            crate::extension_host_invocation::HostReadInvocationAuthorization {
                package_id: "vqro.collections".into(),
                service_id: "collections".into(),
                namespace: COLLECTIONS_NAMESPACE.into(),
                artifact_sha256: "a".repeat(64),
                artifact_version: "test".into(),
                catalog_generation: 1,
                package_generation: 1,
                service_generation: 1,
                artifact_generation: 1,
                provider_generation: 1,
                runtime_generation: 1,
                state: None,
                terminals: None,
                allowed_methods: Default::default(),
                authority: crate::extension_host_state::HostInvocationAuthority::new(1, 1, 1),
                terminal_observations: Default::default(),
            }
        }

        pub(crate) fn test_queue_shutdown_action(
            &mut self,
        ) -> (Arc<AtomicBool>, std::sync::mpsc::Receiver<String>) {
            assert!(!self.collections_mutations_blocked);
            let (respond_to, rx) = std::sync::mpsc::channel();
            let canceled = Arc::new(AtomicBool::new(false));
            let authorization = self.test_shutdown_read_authorization();
            let sequence = self.reserve_collections_action_sequence().unwrap();
            let pending = PendingCollectionsAction {
                request_id: "shutdown-action".into(),
                respond_to,
                invocation: CollectionsActionInvocationV1 {
                    contract: COLLECTIONS_ACTION_INVOCATION_CONTRACT_V1.into(),
                    ticket: format!("ticket:{sequence}"),
                    idempotency_key: format!("shutdown-{sequence}"),
                    document_revision: 1,
                    actions_revision: 1,
                    action_id: "archive".into(),
                    expected_store_generation: self.extension_state.store_generation,
                    expected_namespace_revision: 0,
                    dependency: CollectionsActionDependencyV1::Container {
                        container_id: "container_0000000000000001".into(),
                    },
                    input: CollectionsActionInputV1::SetLabel {
                        container_id: "container_0000000000000001".into(),
                        label: None,
                    },
                },
                observations: authorization.terminal_observations(),
                authorization,
                expected_state: self
                    .extension_state
                    .snapshot(COLLECTIONS_NAMESPACE)
                    .unwrap()
                    .into(),
                collection: crate::api::schema::CollectionInfo {
                    collection_id: "collection_1".into(),
                    workspace_id: self.state.workspaces[0].id.clone(),
                    tab_id: self.state.workspaces[0].tabs[0].id.to_string(),
                    label: None,
                    focused: false,
                    selected_pane_id: None,
                    members: Vec::new(),
                },
                deadline: Instant::now() + Duration::from_secs(30),
                canceled: canceled.clone(),
            };
            self.collections_pending_actions.insert(77, pending);
            self.collections_mutations_blocked = true;
            (canceled, rx)
        }

        pub(crate) fn test_publish_shutdown_checkpoint(
            &mut self,
            paths: crate::persist::coordinator::PersistencePaths,
            published: std::sync::mpsc::Sender<()>,
            release: std::sync::mpsc::Receiver<()>,
        ) -> std::thread::JoinHandle<()> {
            assert!(self.collections_mutations_blocked);
            let pending = self.collections_pending_actions.remove(&77).unwrap();
            let state = self.extension_state.clone();
            let authority = self.collections_authority.clone();
            let session = self.capture_session_snapshot();
            let events = self.event_tx.clone();
            std::thread::spawn(move || {
                let result =
                    crate::persist::coordinator::persist_checkpoint_paused_after_session_for_test(
                        &paths, &session, &state, published, release,
                    )
                    .map_err(|error| error.to_string());
                events
                    .blocking_send(crate::events::AppEvent::CollectionsCheckpointPersisted(
                        Box::new(CollectionsCheckpointResult {
                            pending,
                            state,
                            authority,
                            result,
                        }),
                    ))
                    .unwrap();
            })
        }
    }
}

pub(crate) struct PendingCollectionsAction {
    request_id: String,
    respond_to: std::sync::mpsc::Sender<String>,
    invocation: CollectionsActionInvocationV1,
    authorization: crate::extension_host_invocation::HostReadInvocationAuthorization,
    observations: crate::extension_host_invocation::TrustedTerminalSnapshotObservations,
    expected_state: crate::extension_host_state::HostStateSnapshotV1,
    collection: crate::api::schema::CollectionInfo,
    deadline: Instant,
    canceled: Arc<AtomicBool>,
}

impl std::fmt::Debug for PendingCollectionsAction {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PendingCollectionsAction")
            .field("request_id", &self.request_id)
            .field("invocation", &self.invocation)
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
pub(crate) struct CollectionsCheckpointResult {
    pending: PendingCollectionsAction,
    state: crate::extension_state::DurableExtensionStateV1,
    authority: CollectionsAuthorityV1,
    result: Result<(), String>,
}

impl App {
    pub(crate) fn validate_loaded_collections_authority(&mut self) {
        let valid = self.collections_authority.validate().is_ok()
            && match (
                self.collections_authority.phase,
                self.collections_authority.state.as_ref(),
            ) {
                (CollectionsAuthorityPhaseV1::Legacy, None) => true,
                (CollectionsAuthorityPhaseV1::Package, Some(expected)) => self
                    .extension_state
                    .snapshot(COLLECTIONS_NAMESPACE)
                    .is_ok_and(|state| {
                        state.store_generation == expected.store_generation
                            && state.revision == expected.namespace_revision
                    }),
                _ => false,
            };
        if !valid {
            self.session_persistence_blocked = true;
            self.extension_state_persistence_blocked = true;
            self.disable_collections_package_authority_runtime();
            tracing::error!("Collections durable authority/state fence mismatch; package methods are disabled and both images are preserved");
        }
    }

    /// Performs the one durable two-image transition only after every enabled tab has an audited
    /// profile and matching action declaration from the exact production pin.
    pub(crate) fn maybe_cutover_collections_authority(&mut self) -> Result<(), String> {
        if !COLLECTIONS_AUTHORITY_CUTOVER_ADMITTED {
            return Ok(());
        }
        if self.collections_authority.phase == CollectionsAuthorityPhaseV1::Package {
            return Ok(());
        }
        if !self.policy.persist_session
            || self.session_persistence_blocked
            || self.extension_state_persistence_blocked
            || self.collections_mutations_blocked
        {
            return Ok(());
        }
        let pin = production_pin()?;
        let Some(candidate) = self
            .extension_package_services
            .get(&pin.package_id)
            .and_then(|services| services.binding(&pin.service_id))
        else {
            return Ok(());
        };
        if candidate.artifact_sha256() != pin.package_sha256 {
            return Ok(());
        }

        let mut expected_scopes = BTreeSet::new();
        for workspace in &self.state.workspaces {
            for tab in &workspace.tabs {
                expected_scopes.insert(tab.id.to_string());
            }
        }
        if expected_scopes.is_empty() {
            return Ok(());
        }
        let profiles = self
            .collections_candidate_audits
            .profiles()
            .filter(|profile| {
                profile.document.producer.artifact_sha256 == pin.package_sha256
                    && expected_scopes.contains(&profile.document.scope.scope_id)
            })
            .cloned()
            .collect::<Vec<_>>();
        let profile_scopes = profiles
            .iter()
            .map(|profile| profile.document.scope.scope_id.clone())
            .collect::<BTreeSet<_>>();
        if profile_scopes != expected_scopes || profiles.len() != expected_scopes.len() {
            return Ok(());
        }
        if profiles.iter().any(|profile| {
            self.collections_candidate_audits
                .actions_for(profile)
                .is_none_or(|actions| actions.document_revision != profile.document.revision)
        }) {
            return Ok(());
        }

        if self
            .session_save_thread
            .as_ref()
            .is_some_and(|thread| !thread.is_finished())
        {
            return Ok(());
        }
        if let Some(thread) = self.session_save_thread.take() {
            let _ = thread.join();
        }
        self.collections_mutations_blocked = true;
        let result = self.persist_collections_cutover(&pin);
        self.collections_mutations_blocked = false;
        if result.is_ok()
            && self.collections_authority.phase == CollectionsAuthorityPhaseV1::Package
        {
            // Publish the already-audited exact-state profiles in the same App turn as authority.
            // Callers must never observe package authority without its compatibility document.
            self.publish_ready_collections_package_profiles();
            if !self.collections_package_runtime_available() {
                return Err("Collections cutover profile publication failed closed".into());
            }
        }
        result
    }

    fn persist_collections_cutover(
        &mut self,
        pin: &crate::collections_extension_contract::CollectionsProductionPinV1,
    ) -> Result<(), String> {
        let migration_epoch = self
            .collections_authority
            .authority_epoch
            .checked_add(1)
            .ok_or("Collections authority epoch exhausted")?;
        let next_state = super::collections_candidate::build_collections_import_state(
            &self.extension_state,
            self.collections_import_overlay.imported_values(),
            migration_epoch,
        )?;
        let candidate_snapshot = self
            .collections_import_overlay
            .state()
            .snapshot(COLLECTIONS_NAMESPACE)
            .map_err(|error| format!("cannot inspect audited candidate state: {error:?}"))?;
        let next_snapshot = next_state
            .snapshot(COLLECTIONS_NAMESPACE)
            .map_err(|error| format!("cannot inspect package state: {error:?}"))?;
        if next_snapshot != candidate_snapshot {
            return Err("audited Collections candidate state changed before cutover".into());
        }
        let namespace_revision = next_snapshot.revision;
        let authority = CollectionsAuthorityV1 {
            contract: crate::collections_extension_contract::COLLECTIONS_AUTHORITY_CONTRACT_V1
                .into(),
            phase: CollectionsAuthorityPhaseV1::Package,
            authority_epoch: migration_epoch,
            package: Some(CollectionsAuthorityPackageV1 {
                package_id: pin.package_id.clone(),
                service_id: pin.service_id.clone(),
                package_sha256: pin.package_sha256.clone(),
                component_sha256: pin.component_sha256.clone(),
                profile_contract: pin.profile_contract.clone(),
            }),
            state: Some(CollectionsAuthorityStateV1 {
                store_generation: next_state.store_generation,
                namespace_revision,
            }),
            migration_id: Some(format!("collections_r3_cutover_{migration_epoch}")),
            minimum_reader_version:
                crate::collections_extension_contract::COLLECTIONS_PACKAGE_AUTHORITY_MINIMUM_READER_VERSION,
        };
        authority
            .validate()
            .map_err(|error| format!("invalid package authority: {error}"))?;
        let mut session = self.capture_session_snapshot();
        session.version = crate::persist::SNAPSHOT_VERSION;
        session.collections_authority = authority.clone();
        #[cfg(test)]
        let persisted = if let Some(paths) = self.collections_checkpoint_paths_override.as_ref() {
            crate::persist::persist_collections_checkpoint_at(paths, &session, &next_state)
        } else {
            crate::persist::persist_collections_checkpoint(&session, &next_state)
        };
        #[cfg(not(test))]
        let persisted = crate::persist::persist_collections_checkpoint(&session, &next_state);
        if let Err(error) = persisted {
            self.session_persistence_blocked = true;
            self.extension_state_persistence_blocked = true;
            return Err(format!("Collections cutover persistence failed: {error}"));
        }

        // This is the sole publication point: both durable images are committed before any
        // in-memory namespace, policy, document, or action authority changes.
        self.extension_state = next_state;
        self.collections_authority = authority;
        self.collections_import_overlay.clear();
        self.extension_state_transaction_pending = false;
        self.state.session_dirty = false;
        self.session_save_deadline = None;
        tracing::info!(
            authority_epoch = migration_epoch,
            "Collections package authority published"
        );
        Ok(())
    }

    pub(super) fn publish_ready_collections_package_profiles(&mut self) {
        if self.collections_authority.phase != CollectionsAuthorityPhaseV1::Package {
            return;
        }
        let Some(package) = self.collections_authority.package.as_ref() else {
            return;
        };
        let package_sha256 = package.package_sha256.clone();
        let runtime_available = self
            .extension_package_services
            .get(&package.package_id)
            .and_then(|services| services.binding(&package.service_id))
            .is_some();
        if !runtime_available {
            self.disable_collections_package_authority_runtime();
            return;
        }
        let profiles = self
            .collections_candidate_audits
            .profiles()
            .filter(|profile| profile.document.producer.artifact_sha256 == package_sha256)
            .cloned()
            .collect::<Vec<CollectionsDocumentProfileV1>>();
        'profiles: for profile in profiles {
            let package_scope_id = profile.document.scope.scope_id.clone();
            let located = self.state.workspaces.iter().enumerate().find_map(
                |(workspace_index, workspace)| {
                    workspace
                        .tabs
                        .iter()
                        .enumerate()
                        .find_map(|(tab_index, tab)| {
                            (tab.id.to_string() == package_scope_id)
                                .then_some((workspace_index, tab_index))
                        })
                },
            );
            let Some((workspace_index, tab_index)) = located else {
                continue;
            };
            let Some(topology) = self
                .collections_candidate_audits
                .topology_for(&profile)
                .cloned()
            else {
                continue;
            };
            let mut projection = match profile.project_compatibility(&topology) {
                Ok(projection) => projection,
                Err(_) => continue,
            };
            let Some(compatibility_scope_id) = self.public_tab_id(workspace_index, tab_index)
            else {
                continue;
            };
            let compatibility_workspace_id = self.public_workspace_id(workspace_index);
            let tab = &self.state.workspaces[workspace_index].tabs[tab_index];
            let pane_ids = tab
                .panes
                .iter()
                .filter_map(|(pane_id, pane)| {
                    self.public_pane_id(workspace_index, *pane_id)
                        .map(|public_id| {
                            (
                                format!("pane_{}", pane_id.raw()),
                                (pane.attached_terminal_id.to_string(), public_id),
                            )
                        })
                })
                .collect::<HashMap<_, _>>();
            if !translate_generation_one_identities(
                &mut projection,
                &compatibility_workspace_id,
                &compatibility_scope_id,
                &pane_ids,
            ) {
                continue 'profiles;
            }
            let generation = projection.document.generation;
            let revision = projection.document.revision;
            let key = (
                COLLECTIONS_NAMESPACE.to_string(),
                compatibility_scope_id.clone(),
            );
            let cache = self.host_document_caches.entry(key.clone()).or_default();
            cache.disable();
            if cache
                .publish_for_namespace(
                    COLLECTIONS_NAMESPACE,
                    generation,
                    100,
                    &HashSet::new(),
                    projection.document.clone(),
                )
                .is_ok()
            {
                self.host_document_request_watermarks
                    .insert(key, (generation, revision));
                self.collections_package_projections
                    .insert(compatibility_scope_id, projection);
            }
        }
    }

    pub(crate) fn defer_collections_archive(
        &mut self,
        request_id: String,
        params: crate::api::schema::CollectionMemberTarget,
        archived: bool,
        respond_to: std::sync::mpsc::Sender<String>,
    ) -> bool {
        let start = self.start_collections_archive_action(
            request_id.clone(),
            &params,
            archived,
            respond_to.clone(),
        );
        if let Err(error) = start {
            let response =
                super::responses::encode_error(request_id, "collections_action_rejected", &error);
            let _ = respond_to.send(response);
            self.collections_mutations_blocked = false;
            return false;
        }
        true
    }

    fn start_collections_archive_action(
        &mut self,
        request_id: String,
        params: &crate::api::schema::CollectionMemberTarget,
        archived: bool,
        respond_to: std::sync::mpsc::Sender<String>,
    ) -> Result<(), String> {
        if !self.collections_package_runtime_available()
            || self.collections_mutations_blocked
            || self.extension_state_transaction_pending
            || self
                .session_save_thread
                .as_ref()
                .is_some_and(|thread| !thread.is_finished())
        {
            return Err("the authoritative Collections package is unavailable or busy".into());
        }
        let collection_id = crate::layout::CollectionId::parse(&params.collection_id)
            .map_err(|_| "invalid collection id")?;
        let (workspace_index, tab_index) = self
            .find_collection(collection_id)
            .ok_or("collection was not found")?;
        let (pane_workspace_index, pane_id) = self
            .parse_current_public_pane_id(&params.pane_id)
            .ok_or("pane was not found")?;
        if pane_workspace_index != workspace_index {
            return Err("pane is outside the collection workspace".into());
        }
        let tab = &self.state.workspaces[workspace_index].tabs[tab_index];
        let collection = tab
            .collection(collection_id)
            .ok_or("collection was not found")?;
        if !collection.contains_member(pane_id) {
            return Err("pane is not a collection member".into());
        }
        let terminal_id = tab
            .panes
            .get(&pane_id)
            .ok_or("pane was not found")?
            .attached_terminal_id
            .to_string();
        let package_scope_id = tab.id.to_string();
        let package_sha256 = self
            .collections_authority
            .package
            .as_ref()
            .ok_or("package authority metadata is absent")?
            .package_sha256
            .clone();
        let profile = self
            .collections_candidate_audits
            .profiles()
            .filter(|profile| {
                profile.document.scope.scope_id == package_scope_id
                    && profile.document.producer.artifact_sha256 == package_sha256
            })
            .max_by_key(|profile| profile.document.revision)
            .cloned()
            .ok_or("audited Collections profile is unavailable")?;
        let actions = self
            .collections_candidate_audits
            .actions_for(&profile)
            .filter(|actions| actions.document_revision == profile.document.revision)
            .cloned()
            .ok_or("audited Collections actions are unavailable")?;
        let container_id = crate::terminal_container::format_terminal_container_id(
            crate::terminal_container::TerminalContainerId::from(collection_id),
        );
        let action = actions
            .actions
            .iter()
            .find(|action| {
                action.action == CollectionsActionKindV1::SetArchived
                    && action.id.ends_with(&terminal_id)
            })
            .ok_or("set_archived is not declared for this terminal")?;
        let dependency = profile
            .document
            .dependencies
            .iter()
            .find(|dependency| {
                dependency.contract == crate::extension_host_terminals::HOST_TERMINALS_CONTRACT_V1
            })
            .ok_or("terminal dependency is absent")?;
        let fingerprint = dependency
            .scope_id
            .rsplit_once('/')
            .map(|(_, fingerprint)| fingerprint.to_owned())
            .ok_or("terminal dependency fingerprint is malformed")?;
        let state = self
            .extension_state
            .snapshot(COLLECTIONS_NAMESPACE)
            .map_err(|error| format!("cannot capture Collections state: {error:?}"))?;
        let action_sequence = self.reserve_collections_action_sequence()?;
        let invocation = CollectionsActionInvocationV1 {
            contract: COLLECTIONS_ACTION_INVOCATION_CONTRACT_V1.into(),
            ticket: format!("ticket:r3-{action_sequence}"),
            idempotency_key: format!("collections-r3-archive-{action_sequence}"),
            document_revision: profile.document.revision,
            actions_revision: actions.revision,
            action_id: action.id.clone(),
            expected_store_generation: state.store_generation,
            expected_namespace_revision: state.revision,
            dependency: CollectionsActionDependencyV1::TerminalMembership {
                container_id,
                terminal_id: terminal_id.clone(),
                terminal_snapshot_fingerprint_sha256: fingerprint,
                provider_generation: dependency.generation,
            },
            input: CollectionsActionInputV1::SetArchived {
                container_id: crate::terminal_container::format_terminal_container_id(
                    crate::terminal_container::TerminalContainerId::from(collection_id),
                ),
                terminal_id,
                archived,
            },
        };
        actions
            .validate_invocation(&invocation)
            .map_err(|error| format!("invalid Collections action invocation: {error}"))?;
        let pin = production_pin()?;
        let services = self
            .extension_package_services
            .get(&pin.package_id)
            .ok_or("Collections package worker is absent")?;
        let binding = services
            .binding(&pin.service_id)
            .ok_or("Collections package binding is absent")?
            .clone();
        let runtime_generation = services
            .runtime_generation(&pin.service_id)
            .ok_or("Collections package runtime generation is absent")?;
        let artifact_generation = self.marketplace_state.packages.packages[&pin.package_id]
            .active
            .as_ref()
            .ok_or("Collections active artifact is absent")?
            .runtime_generation;
        let provider_generation =
            self.marketplace_state.packages.packages[&pin.package_id].revision;
        let authorization = self.stage_host_read_authorization(
            &binding,
            crate::extension_host_invocation::HostReadInvocationPurpose {
                state: crate::extension_host_invocation::HostStateReadSource::Durable,
                terminals: None,
                provisional_container: None,
            },
            artifact_generation,
            provider_generation,
            runtime_generation,
        )?;
        let expected_state = crate::extension_host_state::HostStateSnapshotV1::from(state);
        let observations = authorization.terminal_observations();
        let deadline = Instant::now() + Duration::from_secs(2);
        let canceled = Arc::new(AtomicBool::new(false));
        let attempt_id = (1_u64 << 63) | action_sequence;
        let mut collection_info = self
            .collection_info(workspace_index, tab_index, collection_id)
            .ok_or("collection projection is unavailable")?;
        if let Some(member) = collection_info
            .members
            .iter_mut()
            .find(|member| member.pane_id == params.pane_id)
        {
            member.archived = archived;
        }
        self.collections_mutations_blocked = true;
        self.collections_pending_actions.insert(
            attempt_id,
            PendingCollectionsAction {
                request_id,
                respond_to,
                invocation: invocation.clone(),
                authorization: authorization.clone(),
                observations,
                expected_state,
                collection: collection_info,
                deadline,
                canceled: Arc::clone(&canceled),
            },
        );
        let params = serde_json::to_value(invocation)
            .map_err(|error| format!("cannot encode Collections action: {error}"))?;
        let submission = self
            .extension_package_services
            .get(&pin.package_id)
            .ok_or("Collections package worker was retired")?
            .invoke(
                &pin.service_id,
                attempt_id,
                COLLECTIONS_ACTION_PLAN_METHOD_V1.into(),
                params,
                authorization,
                deadline,
                canceled,
            );
        if let Err(error) = submission {
            self.collections_pending_actions.remove(&attempt_id);
            self.collections_mutations_blocked = false;
            return Err(error);
        }
        Ok(())
    }

    pub(crate) fn finish_collections_action_plan(
        &mut self,
        attempt_id: u64,
        result: Result<serde_json::Value, String>,
    ) -> bool {
        let Some(pending) = self.collections_pending_actions.remove(&attempt_id) else {
            return false;
        };
        let prepared = self.prepare_collections_action_checkpoint(&pending, result);
        let (state, authority, session) = match prepared {
            Ok(prepared) => prepared,
            Err(error) => {
                self.collections_mutations_blocked = false;
                self.disable_collections_package_authority_runtime();
                let response = super::responses::encode_error(
                    pending.request_id,
                    "collections_action_rejected",
                    &error,
                );
                let _ = pending.respond_to.send(response);
                return true;
            }
        };
        let event_tx = self.event_tx.clone();
        let state_for_worker = state.clone();
        let authority_for_worker = authority.clone();
        std::thread::spawn(move || {
            let result =
                crate::persist::persist_collections_checkpoint(&session, &state_for_worker)
                    .map_err(|error| error.to_string());
            let _ =
                event_tx.blocking_send(crate::events::AppEvent::CollectionsCheckpointPersisted(
                    Box::new(CollectionsCheckpointResult {
                        pending,
                        state: state_for_worker,
                        authority: authority_for_worker,
                        result,
                    }),
                ));
        });
        true
    }

    fn prepare_collections_action_checkpoint(
        &self,
        pending: &PendingCollectionsAction,
        result: Result<serde_json::Value, String>,
    ) -> Result<
        (
            crate::extension_state::DurableExtensionStateV1,
            CollectionsAuthorityV1,
            crate::persist::SessionSnapshot,
        ),
        String,
    > {
        if pending.canceled.load(Ordering::Acquire) || Instant::now() >= pending.deadline {
            return Err("Collections action expired or was canceled".into());
        }
        let current_authority = self
            .host_invocation_authorities
            .get(&(
                pending.authorization.package_id.clone(),
                pending.authorization.service_id.clone(),
            ))
            .is_some_and(|authority| Arc::ptr_eq(authority, &pending.authorization.authority));
        if !current_authority {
            return Err("Collections action authority was superseded".into());
        }
        let observed = pending
            .observations
            .exactly_one_state()
            .map_err(|error| format!("Collections action host-read trace rejected: {error:?}"))?;
        let current = crate::extension_host_state::HostStateSnapshotV1::from(
            self.extension_state
                .snapshot(COLLECTIONS_NAMESPACE)
                .map_err(|error| format!("cannot inspect Collections state: {error:?}"))?,
        );
        if observed != pending.expected_state || current != pending.expected_state {
            return Err("Collections state changed while the package planned the action".into());
        }
        let plan: CollectionsEffectPlanV1 = serde_json::from_value(
            result.map_err(|error| format!("Collections package action failed: {error}"))?,
        )
        .map_err(|error| format!("Collections package returned a malformed plan: {error}"))?;
        plan.validate_for(&pending.invocation)
            .map_err(|error| format!("Collections package plan was rejected: {error}"))?;
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
            .map_err(|error| format!("Collections state CAS failed: {error:?}"))?;
        let mut authority = self.collections_authority.clone();
        authority.state = Some(CollectionsAuthorityStateV1 {
            store_generation: commit.store_generation,
            namespace_revision: commit.revision,
        });
        let mut session = self.capture_session_snapshot();
        session.collections_authority = authority.clone();
        Ok((state, authority, session))
    }

    /// A pending plan/create still lives in these maps only before publication.
    /// Once a checkpoint worker owns it, the map is empty and its mutation fence
    /// must be released only by that worker's persistence completion.
    pub(crate) fn cancel_prepublication_collections_for_shutdown(&mut self) -> bool {
        let had_pending = !self.collections_pending_actions.is_empty()
            || !self.collections_pending_creates.is_empty();
        for (_, pending) in self.collections_pending_actions.drain() {
            pending.canceled.store(true, Ordering::Release);
            let _ = pending.respond_to.send(super::responses::encode_error(
                pending.request_id,
                "server_unavailable",
                "server is shutting down before publication",
            ));
        }
        for (_, pending) in self.collections_pending_creates.drain() {
            pending.cancel("server is shutting down before publication");
        }
        if had_pending {
            self.collections_mutations_blocked = false;
        }
        had_pending
    }

    pub(crate) fn finish_collections_checkpoint(&mut self, result: CollectionsCheckpointResult) {
        self.collections_mutations_blocked = false;
        let pending = result.pending;
        match result.result {
            Ok(()) => {
                self.extension_state = result.state;
                self.collections_authority = result.authority;
                self.extension_state_transaction_pending = false;
                self.state.session_dirty = false;
                self.session_save_deadline = None;
                self.refresh_collection_documents();
                let response = super::responses::encode_success(
                    pending.request_id,
                    crate::api::schema::ResponseResult::CollectionInfo {
                        collection: pending.collection,
                    },
                );
                let _ = pending.respond_to.send(response);
            }
            Err(error) => {
                self.session_persistence_blocked = true;
                self.extension_state_persistence_blocked = true;
                self.disable_collections_package_authority_runtime();
                let response = super::responses::encode_error(
                    pending.request_id,
                    "collections_checkpoint_failed",
                    &error,
                );
                let _ = pending.respond_to.send(response);
            }
        }
    }

    pub(crate) fn collections_package_runtime_available(&self) -> bool {
        if self.collections_authority.phase != CollectionsAuthorityPhaseV1::Package
            || self.collections_mutations_blocked
        {
            return false;
        }
        let Some(package) = self.collections_authority.package.as_ref() else {
            return false;
        };
        let runtime_ready = self
            .extension_package_services
            .get(&package.package_id)
            .and_then(|services| services.binding(&package.service_id))
            .is_some_and(|binding| binding.artifact_sha256() == package.package_sha256);
        runtime_ready
            && self
                .state
                .workspaces
                .iter()
                .enumerate()
                .flat_map(|(workspace_index, workspace)| {
                    workspace
                        .tabs
                        .iter()
                        .enumerate()
                        .filter_map(move |(tab_index, _)| {
                            self.public_tab_id(workspace_index, tab_index)
                        })
                })
                .all(|tab_id| self.collections_package_projections.contains_key(&tab_id))
    }

    pub(super) fn disable_collections_package_authority_runtime(&mut self) {
        if self.collections_authority.phase != CollectionsAuthorityPhaseV1::Package {
            return;
        }
        self.collections_package_projections.clear();
        self.collections_mutations_blocked = false;
        for (_, pending) in self.collections_pending_creates.drain() {
            pending.cancel("the authoritative Collections package was retired");
        }
        for (_, pending) in self.collections_pending_actions.drain() {
            pending.canceled.store(true, Ordering::Release);
            let response = super::responses::encode_error(
                pending.request_id,
                "collections_authority_unavailable",
                "the authoritative Collections package was retired",
            );
            let _ = pending.respond_to.send(response);
        }
        for ((namespace, _), cache) in &mut self.host_document_caches {
            if namespace == COLLECTIONS_NAMESPACE {
                cache.disable();
            }
        }
        self.revoke_host_invocation_authority_for_package(
            crate::collections_extension_contract::COLLECTIONS_PACKAGE_ID,
        );
    }
}

fn translate_generation_one_identities(
    projection: &mut crate::collections_extension_contract::CollectionsCompatibilityProjectionV1,
    workspace_id: &str,
    tab_id: &str,
    pane_ids: &HashMap<String, (String, String)>,
) -> bool {
    projection.document.target = crate::extension_contract::HostTarget::Tab {
        tab_id: tab_id.into(),
    };
    for item in &mut projection.document.items {
        let Some(terminal) = &mut item.terminal else {
            continue;
        };
        let Some(placement) = &mut terminal.placement else {
            continue;
        };
        let Some(public_pane_id) = pane_ids
            .values()
            .find(|(terminal_id, _)| terminal_id == &terminal.terminal_id)
            .map(|(_, pane_id)| pane_id.clone())
        else {
            return false;
        };
        placement.pane_id = public_pane_id;
        placement.workspace_id = workspace_id.into();
        placement.tab_id = tab_id.into();
    }
    for collection in &mut projection.collections {
        collection.workspace_id = workspace_id.into();
        collection.tab_id = tab_id.into();
        if let Some(selected) = &mut collection.selected_pane_id {
            let Some((_, public)) = pane_ids.get(selected) else {
                return false;
            };
            *selected = public.clone();
        }
        for member in &mut collection.members {
            let Some((_, public)) = pane_ids.get(&member.pane_id) else {
                return false;
            };
            member.pane_id = public.clone();
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::schema::{CollectionListParams, EmptyParams, Method, Request};

    fn test_app() -> App {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        App::new(
            &crate::config::Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        )
    }

    fn install_unavailable_package_authority(app: &mut App) {
        let pin = production_pin().unwrap();
        let state = app.extension_state.snapshot(COLLECTIONS_NAMESPACE).unwrap();
        app.collections_authority = CollectionsAuthorityV1 {
            contract: crate::collections_extension_contract::COLLECTIONS_AUTHORITY_CONTRACT_V1
                .into(),
            phase: CollectionsAuthorityPhaseV1::Package,
            authority_epoch: 2,
            package: Some(CollectionsAuthorityPackageV1 {
                package_id: pin.package_id,
                service_id: pin.service_id,
                package_sha256: pin.package_sha256,
                component_sha256: pin.component_sha256,
                profile_contract: pin.profile_contract,
            }),
            state: Some(CollectionsAuthorityStateV1 {
                store_generation: state.store_generation,
                namespace_revision: state.revision,
            }),
            migration_id: Some("collections_r3_test".into()),
            minimum_reader_version: 7,
        };
    }

    #[test]
    fn atomic_cutover_publishes_both_images_and_restart_accepts_the_exact_state() {
        use crate::collections_extension_contract::{
            CollectionsActionDeclarationsV1, CollectionsProfileContainerV1,
            CollectionsProfileTopologyV1, CollectionsTerminalPlacementV1,
        };
        use crate::persist::coordinator::{
            PersistenceOwnership, PersistencePaths, StartupRecovery,
        };

        let unique = format!(
            "vqro-collections-cutover-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        );
        let root = std::env::temp_dir().join(unique);
        std::fs::create_dir_all(&root).unwrap();
        let paths = PersistencePaths {
            session: root.join("session.json"),
            extension_state: root.join("extension-state-v1.json"),
            journal: root.join("session-extension-state.journal.json"),
        };

        let mut app = test_app();
        app.policy.persist_session = true;
        app.collections_checkpoint_paths_override = Some(paths.clone());
        let mut workspace = crate::workspace::Workspace::test_new("cutover");
        let workspace_id = workspace.id.clone();
        let tab_id = workspace.tabs[0].id.to_string();
        let first_pane = workspace.tabs[0].layout.pane_ids()[0];
        let second_pane = workspace.test_split(ratatui::layout::Direction::Vertical);
        let collection_id = workspace.tabs[0]
            .create_collection_near(
                crate::layout::LayoutLeaf::Pane(first_pane),
                ratatui::layout::Direction::Vertical,
                0.5,
                Some("Helpers".into()),
            )
            .unwrap();
        workspace.tabs[0]
            .collect_tiled_pane(second_pane, collection_id)
            .unwrap();
        workspace.tabs[0]
            .set_collection_member_archived(collection_id, second_pane, true)
            .unwrap();
        let first_terminal = workspace.tabs[0]
            .terminal_id(first_pane)
            .unwrap()
            .to_string();
        let second_terminal = workspace.tabs[0]
            .terminal_id(second_pane)
            .unwrap()
            .to_string();
        let container_id = crate::terminal_container::format_terminal_container_id(
            crate::terminal_container::TerminalContainerId::from(collection_id),
        );
        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();
        app.rebuild_collections_import_overlay().unwrap();

        let pin = production_pin().unwrap();
        let replace_fixture_ids = |fixture: &str| {
            fixture
                .replace(
                    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    &pin.package_sha256,
                )
                .replace("tab_11111111111111111111111111111111", &tab_id)
                .replace("container_000000000000002a", &container_id)
                .replace("term_11111111111111111111111111111111", &first_terminal)
                .replace("term_22222222222222222222222222222222", &second_terminal)
        };
        let profile: CollectionsDocumentProfileV1 =
            serde_json::from_str(&replace_fixture_ids(include_str!(
                "../../../docs/next/api/vqro-collections-v1.fixtures/document-profile-valid.json"
            )))
            .unwrap();
        let actions: CollectionsActionDeclarationsV1 = serde_json::from_str(&replace_fixture_ids(
            include_str!("../../../docs/next/api/vqro-collections-v1.fixtures/actions-valid.json"),
        ))
        .unwrap();
        let topology = CollectionsProfileTopologyV1 {
            workspace_id,
            tab_id: tab_id.clone(),
            containers: vec![CollectionsProfileContainerV1 {
                container_id,
                terminal_ids: vec![first_terminal.clone(), second_terminal.clone()],
                selected_terminal_id: Some(first_terminal.clone()),
            }],
            placements: vec![
                CollectionsTerminalPlacementV1 {
                    terminal_id: first_terminal,
                    pane_id: format!("pane_{}", first_pane.raw()),
                },
                CollectionsTerminalPlacementV1 {
                    terminal_id: second_terminal,
                    pane_id: format!("pane_{}", second_pane.raw()),
                },
            ],
            producer: profile.document.producer.clone(),
            state_dependency: profile.document.dependencies[0].clone(),
            terminals_dependency: profile.document.dependencies[1].clone(),
        };
        app.collections_candidate_audits
            .publish_checkpointed_create(profile, actions, topology)
            .unwrap();

        let binding = crate::extension_host_state::HostProviderBinding::for_test(
            pin.package_id.clone(),
            pin.service_id.clone(),
            COLLECTIONS_NAMESPACE.into(),
            pin.package_sha256.clone(),
            "1.0.0".into(),
            std::collections::BTreeSet::from([
                crate::extension_host_state::HOST_STATE_READ_CAPABILITY.into(),
                crate::extension_host_terminals::HOST_TERMINALS_READ_CAPABILITY.into(),
            ]),
        );
        app.extension_package_services.insert(
            pin.package_id.clone(),
            crate::extension_candidate::PackageCandidateSlot::ready_for_test(binding, 1),
        );

        assert_eq!(
            app.collections_authority.phase,
            CollectionsAuthorityPhaseV1::Legacy
        );
        app.maybe_cutover_collections_authority().unwrap();
        assert_eq!(
            app.collections_authority.phase,
            CollectionsAuthorityPhaseV1::Package
        );
        assert!(app.collections_package_runtime_available());
        assert!(!paths.journal.exists());

        let ownership = PersistenceOwnership::try_acquire(&paths).unwrap();
        assert_eq!(
            crate::persist::coordinator::recover_checkpoint_at_startup(&ownership, &paths).unwrap(),
            StartupRecovery::NoJournal
        );
        drop(ownership);
        let session =
            crate::persist::parse_snapshot(&std::fs::read_to_string(&paths.session).unwrap())
                .unwrap();
        let state = match crate::persist::extension_state::load(&paths.extension_state) {
            crate::persist::extension_state::ExtensionStateLoad::Loaded(state) => state,
            other => panic!("expected committed extension state, got {other:?}"),
        };
        let authority_state = session.collections_authority.state.as_ref().unwrap();
        let state_snapshot = state.snapshot(COLLECTIONS_NAMESPACE).unwrap();
        assert_eq!(
            authority_state.store_generation,
            state_snapshot.store_generation
        );
        assert_eq!(authority_state.namespace_revision, state_snapshot.revision);

        let mut restarted = test_app();
        restarted.collections_authority = session.collections_authority;
        restarted.extension_state = state;
        restarted.validate_loaded_collections_authority();
        assert!(!restarted.session_persistence_blocked);
        assert!(!restarted.extension_state_persistence_blocked);
        assert_eq!(
            restarted.collections_authority.phase,
            CollectionsAuthorityPhaseV1::Package
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn package_projection_preserves_generation_one_public_identities() {
        use crate::extension_contract::{
            HostDocumentItem, HostDocumentItemFlags, HostDocumentV1, HostItemRole, HostSurface,
            HostTarget, HostTerminalPlacement, HostTerminalRef,
        };
        use crate::protocol::endpoint::{ClientShellCollection, ClientShellCollectionMember};

        let mut projection =
            crate::collections_extension_contract::CollectionsCompatibilityProjectionV1 {
                document: HostDocumentV1 {
                    contract: "host.document.v1".into(),
                    namespace: COLLECTIONS_NAMESPACE.into(),
                    generation: 1,
                    revision: 1,
                    surface: HostSurface::Navigation,
                    target: HostTarget::Tab {
                        tab_id: "tab_0123456789abcdef0123456789abcdef".into(),
                    },
                    items: vec![HostDocumentItem {
                        id: "terminal:term_0123456789abcdef0123456789abcdef".into(),
                        parent_id: None,
                        role: HostItemRole::Terminal,
                        label: "terminal".into(),
                        detail: None,
                        terminal: Some(HostTerminalRef {
                            terminal_id: "term_0123456789abcdef0123456789abcdef".into(),
                            placement: Some(HostTerminalPlacement {
                                pane_id: "pane_7".into(),
                                workspace_id: "internal".into(),
                                tab_id: "tab_0123456789abcdef0123456789abcdef".into(),
                            }),
                        }),
                        action_ids: Vec::new(),
                        flags: HostDocumentItemFlags::default(),
                        view: None,
                    }],
                    actions: Vec::new(),
                    required_capabilities: Vec::new(),
                },
                collections: vec![ClientShellCollection {
                    collection_id: "collection_1".into(),
                    workspace_id: "internal".into(),
                    tab_id: "tab_0123456789abcdef0123456789abcdef".into(),
                    label: Some("review".into()),
                    selected_pane_id: Some("pane_7".into()),
                    members: vec![ClientShellCollectionMember {
                        pane_id: "pane_7".into(),
                        index: 0,
                        archived: false,
                    }],
                }],
            };
        let pane_ids = HashMap::from([(
            "pane_7".into(),
            (
                "term_0123456789abcdef0123456789abcdef".into(),
                "w1:p2".into(),
            ),
        )]);

        assert!(translate_generation_one_identities(
            &mut projection,
            "w1",
            "w1:t1",
            &pane_ids,
        ));
        assert_eq!(
            projection.document.target,
            HostTarget::Tab {
                tab_id: "w1:t1".into()
            }
        );
        let placement = projection.document.items[0]
            .terminal
            .as_ref()
            .unwrap()
            .placement
            .as_ref()
            .unwrap();
        assert_eq!(placement.pane_id, "w1:p2");
        assert_eq!(placement.workspace_id, "w1");
        assert_eq!(placement.tab_id, "w1:t1");
        assert_eq!(
            projection.collections[0].selected_pane_id.as_deref(),
            Some("w1:p2")
        );
        assert_eq!(projection.collections[0].members[0].pane_id, "w1:p2");
    }

    #[test]
    fn legacy_snapshot_without_package_has_no_collections_authority_or_document() {
        let mut app = test_app();
        let collections = app.handle_api_request(Request {
            id: "collections".into(),
            method: Method::CollectionList(CollectionListParams::default()),
        });
        assert!(collections.contains("collections_authority_unavailable"));
        app.refresh_collection_documents();
        assert!(app
            .host_documents()
            .iter()
            .all(|entry| { entry.document.namespace != COLLECTIONS_NAMESPACE }));
    }

    #[test]
    fn package_failure_disables_collection_methods_but_not_generic_api() {
        let mut app = test_app();
        install_unavailable_package_authority(&mut app);

        let collections = app.handle_api_request(Request {
            id: "collections".into(),
            method: Method::CollectionList(CollectionListParams::default()),
        });
        assert!(collections.contains("collections_authority_unavailable"));

        let workspaces = app.handle_api_request(Request {
            id: "workspaces".into(),
            method: Method::WorkspaceList(EmptyParams::default()),
        });
        serde_json::from_str::<crate::api::schema::SuccessResponse>(&workspaces)
            .expect("generic workspace access remains available");
    }

    #[test]
    fn restart_state_fence_mismatch_blocks_both_durable_images() {
        let mut app = test_app();
        install_unavailable_package_authority(&mut app);
        let snapshot = app.extension_state.snapshot(COLLECTIONS_NAMESPACE).unwrap();
        app.extension_state
            .transact(&ExtensionStateTransactionV1 {
                contract: EXTENSION_STATE_CONTRACT_V1.into(),
                namespace: COLLECTIONS_NAMESPACE.into(),
                expected_store_generation: snapshot.store_generation,
                expected_revision: snapshot.revision,
                idempotency_key: "state-fence-mismatch".into(),
                operations: vec![ExtensionStateOperationV1 {
                    key: "container_0000000000000001".into(),
                    value: Some(serde_json::json!({
                        "schema": "vqro.collections.policy",
                        "schema_version": 1,
                        "container_id": "container_0000000000000001",
                        "archived_terminal_ids": []
                    })),
                }],
            })
            .unwrap();

        app.validate_loaded_collections_authority();

        assert!(app.session_persistence_blocked);
        assert!(app.extension_state_persistence_blocked);
        assert!(!app.collections_package_runtime_available());
    }

    #[test]
    fn package_authority_rejects_generic_collection_namespace_writes() {
        let mut app = test_app();
        install_unavailable_package_authority(&mut app);
        let snapshot = app.extension_state.snapshot(COLLECTIONS_NAMESPACE).unwrap();
        let response = app.handle_extension_state_transact(
            "write".into(),
            ExtensionStateTransactionV1 {
                contract: EXTENSION_STATE_CONTRACT_V1.into(),
                namespace: COLLECTIONS_NAMESPACE.into(),
                expected_store_generation: snapshot.store_generation,
                expected_revision: snapshot.revision,
                idempotency_key: "forbidden-write".into(),
                operations: vec![ExtensionStateOperationV1 {
                    key: "container_0000000000000001".into(),
                    value: Some(serde_json::json!({"bad": true})),
                }],
            },
        );
        assert!(response.contains("collections_action_required"));
        assert_eq!(
            app.extension_state.snapshot(COLLECTIONS_NAMESPACE).unwrap(),
            snapshot
        );
    }
}
