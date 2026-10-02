use ratatui::layout::Direction;

use super::{responses, App};
use crate::api::schema::{
    CollectionAddParams, CollectionCloseDisposition, CollectionCloseParams, CollectionCreateParams,
    CollectionInfo, CollectionListParams, CollectionMemberInfo, CollectionMemberTarget,
    CollectionPromoteParams, CollectionReorderParams, CollectionSelectParams, CollectionTarget,
    ResponseResult,
};
use crate::layout::{CollectionId, LayoutLeaf};

const MAX_HOST_DOCUMENT_CONFIRMATION_GRANTS: usize = 8_192;
const MAX_COLLECTIONS_ACTION_SEQUENCE: u64 = (1 << 60) - 1;
pub(crate) const COLLECTIONS_ACTION_IDEMPOTENCY_PREFIXES: &[&str] =
    &["collections-r3-create-", "collections-r3-archive-"];
const MAX_HOST_SEMANTIC_RETRY_ATTEMPTS: u32 = 6;
const MAX_HOST_DOCUMENT_RETRY_ATTEMPTS: u32 = 6;
const MAX_HOST_DOCUMENTS_PROJECTION_BYTES: usize = crate::protocol::MAX_FRAME_SIZE / 2;

fn bounded_document_text(value: &str) -> String {
    value
        .chars()
        .filter(|character| !character.is_control())
        .take(crate::extension_contract::MAX_DOCUMENT_TEXT_CHARS)
        .collect()
}

#[derive(Debug, Clone)]
pub(crate) enum DeferredHostDocumentSnapshot {
    TerminalGroups(crate::extension_contract::HostTerminalGroupsV1),
    Relationships(crate::extension_contract::HostRelationshipsV1),
    AgentActivity(crate::extension_contract::HostAgentActivityV1),
}

#[derive(Debug)]
pub(super) enum AuthorizedHostDocumentAction {
    Focus(crate::api::schema::PaneTarget),
    Interrupt(crate::api::schema::PaneTarget),
    Provider(
        Box<(
            crate::extension_contract::HostTerminalGroupsV1,
            crate::extension_contract::HostDocumentExtensionActionV1,
        )>,
    ),
}

#[derive(Clone)]
pub(crate) struct HostDocumentConfirmationGrant {
    pub(crate) token: String,
    pub(crate) terminal_id: String,
    pub(crate) expires_at: std::time::Instant,
}

fn direction(value: crate::api::schema::SplitDirection) -> Direction {
    match value {
        crate::api::schema::SplitDirection::Right => Direction::Horizontal,
        crate::api::schema::SplitDirection::Down => Direction::Vertical,
    }
}

fn collection_id_text(id: CollectionId) -> String {
    format!("collection_{}", id.raw())
}

impl App {
    pub(crate) fn reserve_collections_action_sequence(&mut self) -> Result<u64, String> {
        let sequence = self
            .next_collections_action_id
            .checked_add(1)
            .filter(|sequence| *sequence <= MAX_COLLECTIONS_ACTION_SEQUENCE)
            .ok_or("Collections action sequence exhausted")?;
        self.next_collections_action_id = sequence;
        Ok(sequence)
    }

    pub(super) fn emit_collection_event(&mut self, action: &str, collection: CollectionInfo) {
        if let Some((ws_idx, tab_idx)) = self.parse_tab_id(&collection.tab_id) {
            self.refresh_collection_document(ws_idx, tab_idx);
        }
        self.emit_event(crate::api::schema::EventEnvelope {
            event: crate::api::schema::EventKind::CollectionUpdated,
            data: crate::api::schema::EventData::CollectionUpdated {
                action: action.into(),
                collection,
            },
        });
    }

    pub(crate) fn refresh_collection_documents(&mut self) {
        if self.collections_authority.phase
            == crate::collections_extension_contract::CollectionsAuthorityPhaseV1::Legacy
        {
            if let Err(error) = self.rebuild_collections_import_overlay() {
                tracing::warn!(%error, "Collections pre-cutover import failed");
            }
        }
        let mut live_tab_ids = std::collections::HashSet::from([
            crate::spaces_extension_contract::SPACES_SCOPE_ID.to_string(),
        ]);
        for (ws_idx, workspace) in self.state.workspaces.iter().enumerate() {
            for tab_idx in 0..workspace.tabs.len() {
                if let Some(tab_id) = self.public_tab_id(ws_idx, tab_idx) {
                    live_tab_ids.insert(tab_id);
                }
            }
        }
        self.host_document_services.retain_scopes(&live_tab_ids);
        let retired_documents = self
            .host_document_caches
            .iter()
            .filter(|((_, scope_id), _)| !live_tab_ids.contains(scope_id))
            .filter_map(|((_, scope_id), cache)| {
                let document = cache.current()?;
                Some((
                    document.namespace.clone(),
                    scope_id.clone(),
                    document.generation,
                    document.revision,
                ))
            })
            .collect::<Vec<_>>();
        self.host_document_caches
            .retain(|(_, scope_id), _| live_tab_ids.contains(scope_id));
        for (namespace, scope_id, generation, revision) in retired_documents {
            self.emit_host_document_updated(namespace, scope_id, generation, revision, false);
        }
        self.host_document_request_watermarks
            .retain(|(_, scope_id), _| live_tab_ids.contains(scope_id));
        self.host_document_retry_snapshots
            .retain(|(_, scope_id), _| live_tab_ids.contains(scope_id));
        self.host_document_retry_scheduled
            .retain(|(_, scope_id, _, _)| live_tab_ids.contains(scope_id));
        self.host_document_retry_attempts
            .retain(|(_, scope_id, _, _), _| live_tab_ids.contains(scope_id));
        self.host_document_retry_order.retain(|key| {
            live_tab_ids.contains(&key.1) && self.host_document_retry_snapshots.contains_key(key)
        });
        // Scope retirement can release the same process-global slots used by either source.
        self.retry_one_host_document_render();

        let tabs = self
            .state
            .workspaces
            .iter()
            .enumerate()
            .flat_map(|(ws_idx, workspace)| {
                (0..workspace.tabs.len()).map(move |tab_idx| (ws_idx, tab_idx))
            })
            .collect::<Vec<_>>();
        for (ws_idx, tab_idx) in tabs {
            self.refresh_collection_document(ws_idx, tab_idx);
        }
        self.refresh_spaces_document();
        self.refresh_relationships_document();
        self.refresh_agent_activity_document();
    }

    fn next_host_document_revision(&mut self) -> u64 {
        let revision = self.next_host_document_revision.max(1);
        self.next_host_document_revision = revision.saturating_add(1).max(1);
        revision
    }

    fn next_host_spaces_document_revision(&mut self) -> u64 {
        let revision = self.next_host_spaces_document_revision.max(1);
        self.next_host_spaces_document_revision = revision.saturating_add(1).max(1);
        revision
    }

    fn next_host_relationships_document_revision(&mut self) -> u64 {
        let revision = self.next_host_relationships_document_revision.max(1);
        self.next_host_relationships_document_revision = revision.saturating_add(1).max(1);
        revision
    }

    fn next_host_agent_activity_document_revision(&mut self) -> u64 {
        let revision = self.next_host_agent_activity_document_revision.max(1);
        self.next_host_agent_activity_document_revision = revision.saturating_add(1).max(1);
        revision
    }

    pub(super) fn refresh_collection_document(&mut self, ws_idx: usize, tab_idx: usize) {
        let revision = self.next_host_document_revision();
        let namespaces = self
            .host_document_services
            .namespaces()
            .filter(|namespace| {
                self.host_document_services.source(namespace)
                    == Some(crate::api::schema::HostDocumentSourceKind::TerminalGroups)
            })
            .map(str::to_owned)
            .collect::<Vec<_>>();
        for namespace in namespaces {
            let generation = self
                .host_document_services
                .generation(&namespace)
                .unwrap_or(1);
            let Ok(snapshot) =
                self.legacy_terminal_groups_snapshot(ws_idx, tab_idx, generation, revision)
            else {
                continue;
            };
            let scope_id = snapshot.scope_id.clone();
            self.host_document_confirmation_grants.retain(
                |(grant_namespace, grant_scope, _, _, _), _| {
                    grant_namespace != &namespace || grant_scope != &scope_id
                },
            );
            self.host_document_request_watermarks.insert(
                (namespace.clone(), scope_id.clone()),
                (generation, revision),
            );
            let request_key = (namespace.clone(), scope_id.clone());
            let cache = self
                .host_document_caches
                .entry(request_key.clone())
                .or_default();
            if namespace == crate::collections_extension_contract::COLLECTIONS_NAMESPACE
                && self.collections_authority.phase
                    == crate::collections_extension_contract::CollectionsAuthorityPhaseV1::Package
            {
                // Keep the last audited package projection visible while its bounded replacement
                // runs. Runtime failure clears all projections through the fail-closed path.
                continue;
            }
            cache.disable();
            if let Err(error) = self
                .host_document_services
                .request(&namespace, snapshot.clone())
            {
                if error.contains("global document render queue is full") {
                    self.defer_host_document_render(request_key.clone(), snapshot.clone());
                }
                tracing::warn!(%namespace, %error, "host document provider unavailable");
            } else {
                self.host_document_retry_snapshots.remove(&request_key);
            }
        }
        if let Some((workspace_id, tab_id)) =
            self.state.workspaces.get(ws_idx).and_then(|workspace| {
                workspace
                    .tabs
                    .get(tab_idx)
                    .map(|tab| (workspace.id.clone(), tab.id.clone()))
            })
        {
            if let Err(error) = self.schedule_collections_candidate(workspace_id, tab_id, revision)
            {
                tracing::debug!(%error, "Collections candidate audit was not scheduled");
            }
        }
    }

    fn defer_host_document_render(
        &mut self,
        key: (String, String),
        snapshot: crate::extension_contract::HostTerminalGroupsV1,
    ) -> bool {
        if !self.host_document_retry_snapshots.contains_key(&key) {
            if self.host_document_retry_snapshots.len()
                >= crate::admission::MAX_SESSION_PANES
                    * crate::extension_contract::MAX_DOCUMENT_PROVIDERS
            {
                return false;
            }
            self.host_document_retry_order.push_back(key.clone());
        }
        self.host_document_retry_snapshots
            .insert(key, DeferredHostDocumentSnapshot::TerminalGroups(snapshot));
        true
    }

    pub(super) fn schedule_host_document_render_retry(
        &mut self,
        key: (String, String),
        snapshot: crate::extension_contract::HostTerminalGroupsV1,
    ) -> bool {
        let retry = (
            key.0.clone(),
            key.1.clone(),
            snapshot.generation,
            snapshot.revision,
        );
        let attempts = self
            .host_document_retry_attempts
            .get(&retry)
            .copied()
            .unwrap_or(0);
        if attempts >= MAX_HOST_DOCUMENT_RETRY_ATTEMPTS
            || self.host_document_services.generation(&key.0) != Some(snapshot.generation)
        {
            if attempts >= MAX_HOST_DOCUMENT_RETRY_ATTEMPTS {
                tracing::warn!(
                    namespace = %key.0,
                    scope_id = %key.1,
                    attempts,
                    "host document provider retry budget exhausted; retaining native fallback"
                );
            }
            return false;
        }
        if !self.defer_host_document_render(key.clone(), snapshot) {
            return false;
        }
        if self
            .host_document_retry_scheduled
            .iter()
            .any(|scheduled| scheduled.0 == retry.0 && scheduled.1 == retry.1)
        {
            return true;
        }
        self.host_document_retry_attempts
            .retain(|attempt, _| attempt.0 != retry.0 || attempt.1 != retry.1 || *attempt == retry);
        if !self.host_document_retry_scheduled.insert(retry.clone()) {
            return true;
        }
        self.host_document_retry_attempts
            .insert(retry.clone(), attempts.saturating_add(1));
        let delay_ms = 100_u64.saturating_mul(1_u64 << attempts.min(6)).min(5_000);
        let event_tx = self.event_tx.clone();
        let event_retry = retry.clone();
        match std::thread::Builder::new()
            .name("vqro-document-retry".into())
            .spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(delay_ms));
                let mut event = crate::events::AppEvent::HostDocumentRetryDue {
                    namespace: event_retry.0,
                    scope_id: event_retry.1,
                    generation: event_retry.2,
                    revision: event_retry.3,
                };
                loop {
                    match event_tx.try_send(event) {
                        Ok(()) | Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => return,
                        Err(tokio::sync::mpsc::error::TrySendError::Full(returned)) => {
                            event = returned;
                            std::thread::sleep(std::time::Duration::from_millis(10));
                        }
                    }
                }
            }) {
            Ok(_) => true,
            Err(error) => {
                self.host_document_retry_scheduled.remove(&retry);
                self.host_document_retry_attempts.remove(&retry);
                tracing::warn!(%error, "failed to schedule host document provider retry");
                false
            }
        }
    }

    pub(super) fn retry_one_host_document_render(&mut self) {
        while let Some(key) = self.host_document_retry_order.pop_front() {
            let Some(snapshot) = self.host_document_retry_snapshots.remove(&key) else {
                continue;
            };
            let result = match &snapshot {
                DeferredHostDocumentSnapshot::TerminalGroups(snapshot) => self
                    .host_document_services
                    .request(&key.0, snapshot.clone()),
                DeferredHostDocumentSnapshot::Relationships(snapshot) => self
                    .host_document_services
                    .request_relationships(&key.0, snapshot.clone()),
                DeferredHostDocumentSnapshot::AgentActivity(snapshot) => self
                    .host_document_services
                    .request_agent_activity(&key.0, snapshot.clone()),
            };
            match result {
                Ok(()) => return,
                Err(error) if error.contains("global document render queue is full") => {
                    self.host_document_retry_snapshots
                        .insert(key.clone(), snapshot);
                    self.host_document_retry_order.push_back(key);
                    return;
                }
                Err(_) if key.0 == crate::collections_extension_contract::COLLECTIONS_NAMESPACE => {
                    if let DeferredHostDocumentSnapshot::TerminalGroups(snapshot) = snapshot {
                        self.schedule_host_document_render_retry(key, snapshot);
                    }
                    return;
                }
                Err(_) => continue,
            }
        }
    }

    fn spaces_workspace_label(&self, workspace: &crate::workspace::Workspace) -> String {
        let fallback_label =
            workspace.display_name_from(&self.state.terminals, &self.terminal_runtimes);
        if let Some(custom_name) = workspace.custom_name.as_ref() {
            return custom_name.clone();
        }
        if !workspace
            .checkout
            .as_ref()
            .is_some_and(|checkout| checkout.kind == crate::repository::CheckoutKind::Linked)
        {
            return fallback_label;
        }
        workspace.branch().map_or_else(
            || {
                self.git_status_cache
                    .get(&workspace.cached_git_status_key)
                    .and_then(|entry| entry.fingerprint.as_ref())
                    .and_then(|fingerprint| match &fingerprint.head {
                        crate::workspace::GitHeadIdentity::Detached { oid } => Some(format!(
                            "detached@{}",
                            oid.chars().take(8).collect::<String>()
                        )),
                        crate::workspace::GitHeadIdentity::Branch { .. } => None,
                    })
                    .unwrap_or(fallback_label)
            },
            |branch| {
                branch
                    .strip_prefix("worktree/")
                    .unwrap_or(&branch)
                    .to_owned()
            },
        )
    }

    pub(crate) fn refresh_spaces_document(&mut self) {
        let namespace = crate::spaces_extension_contract::SPACES_NAMESPACE;
        let scope_id = crate::spaces_extension_contract::SPACES_SCOPE_ID.to_string();
        let Some(generation) = self.spaces_package_provider_generation() else {
            self.disable_spaces_package_authority_runtime();
            return;
        };
        let workspace_labels = self
            .state
            .workspaces
            .iter()
            .map(|workspace| (workspace.id.clone(), self.spaces_workspace_label(workspace)))
            .collect::<std::collections::HashMap<_, _>>();
        let mut grouped = std::collections::HashSet::new();
        let repositories = self
            .state
            .repositories
            .iter()
            .map(|repository| {
                let workspaces = repository
                    .checkout_workspace_ids
                    .iter()
                    .filter_map(|workspace_id| {
                        let label = workspace_labels.get(workspace_id)?.clone();
                        grouped.insert(workspace_id.clone());
                        Some(crate::extension_contract::HostSpaceWorkspaceRef {
                            workspace_id: workspace_id.clone(),
                            label,
                        })
                    })
                    .collect();
                crate::extension_contract::HostSpaceRepository {
                    repository_id: repository.id.clone(),
                    label: repository.display_label().into(),
                    workspaces,
                }
            })
            .collect::<Vec<_>>();
        let standalone = self
            .state
            .workspaces
            .iter()
            .filter(|workspace| !grouped.contains(&workspace.id))
            .map(
                |workspace| crate::extension_contract::HostSpaceWorkspaceRef {
                    workspace_id: workspace.id.clone(),
                    label: workspace
                        .display_name_from(&self.state.terminals, &self.terminal_runtimes),
                },
            )
            .collect::<Vec<_>>();
        let mut order = self
            .state
            .space_order
            .iter()
            .filter_map(|space| match space {
                crate::repository::SpaceRef::Repository(repository_id)
                    if repositories
                        .iter()
                        .any(|repository| repository.repository_id == *repository_id) =>
                {
                    Some(crate::extension_contract::HostSpaceOrderRef::Repository {
                        repository_id: repository_id.clone(),
                    })
                }
                crate::repository::SpaceRef::StandaloneWorkspace(workspace_id)
                    if standalone
                        .iter()
                        .any(|workspace| workspace.workspace_id == *workspace_id) =>
                {
                    Some(
                        crate::extension_contract::HostSpaceOrderRef::StandaloneWorkspace {
                            workspace_id: workspace_id.clone(),
                        },
                    )
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        let mut seen_order = std::collections::HashSet::new();
        order.retain(|entry| {
            let key = match entry {
                crate::extension_contract::HostSpaceOrderRef::Repository { repository_id } => {
                    format!("repository:{repository_id}")
                }
                crate::extension_contract::HostSpaceOrderRef::StandaloneWorkspace {
                    workspace_id,
                } => format!("workspace:{workspace_id}"),
            };
            seen_order.insert(key)
        });
        let ordered_repositories = order
            .iter()
            .filter_map(|entry| match entry {
                crate::extension_contract::HostSpaceOrderRef::Repository { repository_id } => {
                    Some(repository_id.clone())
                }
                _ => None,
            })
            .collect::<std::collections::HashSet<_>>();
        for repository in &repositories {
            if !ordered_repositories.contains(&repository.repository_id) {
                order.push(crate::extension_contract::HostSpaceOrderRef::Repository {
                    repository_id: repository.repository_id.clone(),
                });
            }
        }
        let ordered_standalone = order
            .iter()
            .filter_map(|entry| match entry {
                crate::extension_contract::HostSpaceOrderRef::StandaloneWorkspace {
                    workspace_id,
                } => Some(workspace_id.clone()),
                _ => None,
            })
            .collect::<std::collections::HashSet<_>>();
        for workspace in &standalone {
            if !ordered_standalone.contains(&workspace.workspace_id) {
                order.push(
                    crate::extension_contract::HostSpaceOrderRef::StandaloneWorkspace {
                        workspace_id: workspace.workspace_id.clone(),
                    },
                );
            }
        }
        let mut snapshot = crate::extension_contract::HostSpacesV1 {
            contract: "host.spaces.v1".into(),
            generation,
            revision: 1,
            scope_id: scope_id.clone(),
            repositories,
            standalone,
            order,
        };
        if let Err(error) = snapshot.validate() {
            self.disable_spaces_package_authority_runtime();
            tracing::warn!(%error, "host Spaces snapshot is invalid");
            return;
        }
        let unchanged = self
            .host_spaces_last_desired
            .as_ref()
            .is_some_and(|previous| {
                let mut normalized = previous.clone();
                normalized.revision = 1;
                normalized == snapshot
            });
        if unchanged {
            return;
        }
        let revision = self.next_host_spaces_document_revision();
        snapshot.revision = revision;
        self.host_spaces_last_desired = Some(snapshot.clone());
        let key = (namespace.to_string(), scope_id);
        self.host_document_request_watermarks
            .insert(key, (generation, revision));
        if let Err(error) = self.schedule_spaces_candidate(snapshot) {
            if error.contains("candidate queue is full") {
                tracing::debug!(%error, "Spaces package replacement was deferred");
            } else {
                self.disable_spaces_package_authority_runtime();
                tracing::warn!(%error, "Spaces package authority is unavailable");
            }
        }
    }

    fn host_terminal_ref_for_pane(
        &self,
        pane_id: crate::layout::PaneId,
    ) -> Option<crate::extension_contract::HostTerminalRef> {
        let (ws_idx, pane) = self.find_pane(pane_id)?;
        let tab_idx = self.state.workspaces[ws_idx].find_tab_index_for_pane(pane_id)?;
        let terminal = self.state.terminals.get(&pane.attached_terminal_id)?;
        Some(crate::extension_contract::HostTerminalRef {
            terminal_id: terminal.id.to_string(),
            placement: Some(crate::extension_contract::HostTerminalPlacement {
                pane_id: self.public_pane_id(ws_idx, pane_id)?,
                workspace_id: self.public_workspace_id(ws_idx),
                tab_id: self.public_tab_id(ws_idx, tab_idx)?,
            }),
        })
    }

    pub(crate) fn refresh_relationships_document(&mut self) {
        let namespaces = self.semantic_document_namespaces(
            crate::api::schema::HostDocumentSourceKind::Relationships,
        );
        self.host_relationships_last_desired
            .retain(|namespace, _| namespaces.contains(namespace));
        for namespace in namespaces {
            self.refresh_relationships_document_for_namespace(&namespace);
        }
    }

    fn semantic_document_namespaces(
        &self,
        source: crate::api::schema::HostDocumentSourceKind,
    ) -> Vec<String> {
        self.host_document_services
            .namespaces()
            .filter(|namespace| self.host_document_services.source(namespace) == Some(source))
            .map(str::to_owned)
            .collect()
    }

    fn refresh_relationships_document_for_namespace(&mut self, namespace: &str) {
        let scope_id = crate::spaces_extension_contract::SPACES_SCOPE_ID.to_string();
        let Some(generation) = self.host_document_services.generation(namespace) else {
            self.host_document_caches
                .remove(&(namespace.into(), scope_id.clone()));
            self.host_document_request_watermarks
                .remove(&(namespace.into(), scope_id));
            self.host_relationships_last_desired.remove(namespace);
            return;
        };
        let relationships = self
            .state
            .delegations
            .records()
            .values()
            .map(|record| {
                let mut metadata = std::collections::BTreeMap::new();
                if let Some(purpose) = record.purpose.as_ref() {
                    metadata.insert(
                        "vqro.delegation.purpose".into(),
                        serde_json::json!(bounded_document_text(purpose)),
                    );
                }
                crate::extension_contract::HostRelationship {
                    relationship_id: record.id.to_string(),
                    kind: crate::first_party::delegation::NAMESPACE.into(),
                    terminal: record
                        .pane_id
                        .and_then(|pane_id| self.host_terminal_ref_for_pane(pane_id)),
                    parent_id: record.parent_id.map(|id| id.to_string()),
                    sibling_rank: record.sibling_rank,
                    lifecycle: if record.tombstone {
                        crate::extension_contract::HostRelationshipLifecycle::Retained
                    } else {
                        crate::extension_contract::HostRelationshipLifecycle::Active
                    },
                    metadata,
                }
            })
            .collect::<Vec<_>>();
        let mut snapshot = crate::extension_contract::HostRelationshipsV1 {
            contract: crate::extension_contract::HOST_RELATIONSHIPS_CONTRACT_V1.into(),
            generation,
            revision: 1,
            scope_id: scope_id.clone(),
            relationships,
        };
        let key = (namespace.to_string(), scope_id.clone());
        if snapshot.relationships.len() > crate::extension_contract::MAX_DOCUMENT_ITEMS
            || snapshot.validate().is_err()
        {
            self.host_document_caches.remove(&key);
            self.host_document_request_watermarks.remove(&key);
            self.host_document_retry_snapshots.remove(&key);
            self.host_relationships_last_desired.remove(namespace);
            return;
        }
        if self
            .host_relationships_last_desired
            .get(namespace)
            .is_some_and(|previous| {
                let mut normalized = previous.clone();
                normalized.revision = 1;
                normalized == snapshot
            })
        {
            return;
        }
        snapshot.revision = self.next_host_relationships_document_revision();
        self.host_relationships_last_desired
            .insert(namespace.to_owned(), snapshot.clone());
        self.host_document_request_watermarks
            .insert(key.clone(), (generation, snapshot.revision));
        self.disable_host_document_projection(&key, generation, snapshot.revision);
        match self
            .host_document_services
            .request_relationships(namespace, snapshot.clone())
        {
            Ok(()) => {
                self.host_document_retry_snapshots.remove(&key);
            }
            Err(error) if error.contains("global document render queue is full") => {
                if !self.host_document_retry_snapshots.contains_key(&key) {
                    self.host_document_retry_order.push_back(key.clone());
                }
                self.host_document_retry_snapshots
                    .insert(key, DeferredHostDocumentSnapshot::Relationships(snapshot));
            }
            Err(error) => {
                self.host_relationships_last_desired.remove(namespace);
                self.schedule_host_semantic_retry(namespace);
                tracing::warn!(%error, "Delegation provider unavailable; using relationship fallback");
            }
        }
    }

    pub(crate) fn refresh_agent_activity_document(&mut self) {
        let namespaces = self.semantic_document_namespaces(
            crate::api::schema::HostDocumentSourceKind::AgentActivity,
        );
        self.host_agent_activity_last_desired
            .retain(|namespace, _| namespaces.contains(namespace));
        for namespace in namespaces {
            self.refresh_agent_activity_document_for_namespace(&namespace, false);
        }
    }

    pub(crate) fn refresh_extension_document_revision(&mut self, namespace: &str, generation: u64) {
        if !self
            .extension_services
            .matches_generation(namespace, generation)
        {
            return;
        }
        if self.host_document_services.source(namespace)
            == Some(crate::api::schema::HostDocumentSourceKind::AgentActivity)
        {
            self.refresh_agent_activity_document_for_namespace(namespace, true);
        }
    }

    fn refresh_agent_activity_document_for_namespace(&mut self, namespace: &str, force: bool) {
        let scope_id = crate::spaces_extension_contract::SPACES_SCOPE_ID.to_string();
        if force
            && self
                .host_document_caches
                .get(&(namespace.into(), scope_id.clone()))
                .and_then(|cache| cache.current())
                .is_some_and(|document| !document.actions.is_empty())
        {
            // Opaque state invalidation is the actionless observation path, not a way to
            // silently stale previously published controls during a sampling attempt.
            return;
        }
        let Some(generation) = self.host_document_services.generation(namespace) else {
            self.host_document_caches
                .remove(&(namespace.into(), scope_id.clone()));
            self.host_document_request_watermarks
                .remove(&(namespace.into(), scope_id));
            self.host_agent_activity_last_desired.remove(namespace);
            return;
        };
        let mut seen = std::collections::HashSet::new();
        let mut activities = Vec::new();
        let mut terminals = Vec::new();
        for workspace in &self.state.workspaces {
            for tab in &workspace.tabs {
                for (pane_id, pane) in &tab.panes {
                    let Some(terminal) = self.state.terminals.get(&pane.attached_terminal_id)
                    else {
                        continue;
                    };
                    if !seen.insert(terminal.id.clone()) {
                        continue;
                    }
                    let Some(terminal_ref) = self.host_terminal_ref_for_pane(*pane_id) else {
                        continue;
                    };
                    terminals.push(terminal_ref.clone());
                    let Some(agent_label) = terminal.effective_agent_label() else {
                        continue;
                    };
                    let (source, evidence_kind) = terminal.effective_evidence_attribution();
                    let presentation = terminal.effective_presentation();
                    let effective_display_agent = presentation.display_agent;
                    let activity = match terminal.state {
                        crate::detect::AgentState::Idle => {
                            crate::extension_contract::HostAgentActivityState::Idle
                        }
                        crate::detect::AgentState::Working => {
                            crate::extension_contract::HostAgentActivityState::Working
                        }
                        crate::detect::AgentState::Blocked => {
                            crate::extension_contract::HostAgentActivityState::Blocked
                        }
                        crate::detect::AgentState::Unknown => {
                            crate::extension_contract::HostAgentActivityState::Unknown
                        }
                    };
                    let mut labels = std::collections::BTreeMap::from([
                        ("agent".into(), bounded_document_text(agent_label)),
                        (
                            "display".into(),
                            bounded_document_text(
                                effective_display_agent.as_deref().unwrap_or(agent_label),
                            ),
                        ),
                    ]);
                    // Copy existing neutral display-only metadata, not role policy. The fixed
                    // key set keeps source snapshots bounded even for arbitrary metadata input.
                    for state in ["idle", "working", "blocked", "unknown"] {
                        if let Some(label) = presentation.state_labels.get(state) {
                            labels.insert(format!("state_{state}"), bounded_document_text(label));
                        }
                    }
                    activities.push(crate::extension_contract::HostAgentActivityRecord {
                        terminal: terminal_ref,
                        activity,
                        source: bounded_document_text(source),
                        evidence_kind: evidence_kind.into(),
                        state_change_seq: terminal.last_agent_state_change_seq.unwrap_or(0),
                        labels,
                    });
                }
            }
        }
        let mut snapshot = crate::extension_contract::HostAgentActivityV1 {
            contract: crate::extension_contract::HOST_AGENT_ACTIVITY_CONTRACT_V1.into(),
            generation,
            revision: 1,
            scope_id: scope_id.clone(),
            activities,
            terminals: Some(terminals),
        };
        let key = (namespace.to_string(), scope_id.clone());
        if snapshot.validate().is_err() {
            self.host_document_caches.remove(&key);
            self.host_document_request_watermarks.remove(&key);
            self.host_document_retry_snapshots.remove(&key);
            self.host_agent_activity_last_desired.remove(namespace);
            return;
        }
        if !force
            && self
                .host_agent_activity_last_desired
                .get(namespace)
                .is_some_and(|previous| {
                    let mut normalized = previous.clone();
                    normalized.revision = 1;
                    normalized == snapshot
                })
        {
            return;
        }
        snapshot.revision = self.next_host_agent_activity_document_revision();
        self.host_agent_activity_last_desired
            .insert(namespace.to_owned(), snapshot.clone());
        self.host_document_request_watermarks
            .insert(key.clone(), (generation, snapshot.revision));
        // Opaque extension-state invalidation preserves the current actionless observation
        // until a different document is ready. Placement/source changes still retire it.
        if !force {
            self.disable_host_document_projection(&key, generation, snapshot.revision);
        }
        match self
            .host_document_services
            .request_agent_activity(namespace, snapshot.clone())
        {
            Ok(()) => {
                self.host_document_retry_snapshots.remove(&key);
            }
            Err(error) if error.contains("global document render queue is full") => {
                if !self.host_document_retry_snapshots.contains_key(&key) {
                    self.host_document_retry_order.push_back(key.clone());
                }
                self.host_document_retry_snapshots
                    .insert(key, DeferredHostDocumentSnapshot::AgentActivity(snapshot));
            }
            Err(error) => {
                self.host_agent_activity_last_desired.remove(namespace);
                self.schedule_host_semantic_retry(namespace);
                tracing::warn!(%error, "Agent Status provider unavailable; using native fallback");
            }
        }
    }

    pub(crate) fn retire_host_document_provider(&mut self, namespace: &str, generation: u64) {
        if self.host_document_services.generation(namespace) != Some(generation) {
            return;
        }
        self.host_document_services
            .retire_desired_revisions(namespace);
        let retired = self
            .host_document_caches
            .iter()
            .filter(|((owner, _), _)| owner == namespace)
            .filter_map(|(key, cache)| {
                cache
                    .current()
                    .map(|document| (key.clone(), document.revision))
            })
            .collect::<Vec<_>>();
        for (key, revision) in retired {
            self.disable_host_document_projection(&key, generation, revision);
        }
        self.host_document_caches
            .retain(|(owner, _), _| owner != namespace);
        self.host_document_request_watermarks
            .retain(|(owner, _), _| owner != namespace);
        self.host_document_retry_snapshots
            .retain(|(owner, _), _| owner != namespace);
        self.host_document_retry_order
            .retain(|(owner, _)| owner != namespace);
        self.host_document_retry_scheduled
            .retain(|(owner, _, _, _)| owner != namespace);
        self.host_document_retry_attempts
            .retain(|(owner, _, _, _), _| owner != namespace);
        self.host_document_confirmation_grants
            .retain(|(owner, _, _, _, _), _| owner != namespace);
        self.host_relationships_last_desired.remove(namespace);
        self.host_agent_activity_last_desired.remove(namespace);
        // Invalidate already-scheduled semantic retries, but retain their bounded attempt count.
        let epoch = self
            .host_semantic_retry_epochs
            .entry(namespace.into())
            .or_insert(1);
        *epoch = epoch.saturating_add(1).max(1);
        self.host_semantic_retry_scheduled.remove(namespace);
        if self.host_document_services.source(namespace)
            == Some(crate::api::schema::HostDocumentSourceKind::TerminalGroups)
        {
            // Fresh revisions fence both pre-exit results and a respawn raced ahead of retirement.
            // Reuse the existing bounded per-scope retry rather than refreshing other namespaces.
            for ws_idx in 0..self.state.workspaces.len() {
                for tab_idx in 0..self.state.workspaces[ws_idx].tabs.len() {
                    let revision = self.next_host_document_revision();
                    if let Ok(snapshot) =
                        self.legacy_terminal_groups_snapshot(ws_idx, tab_idx, generation, revision)
                    {
                        let key = (namespace.to_string(), snapshot.scope_id.clone());
                        self.host_document_request_watermarks
                            .insert(key.clone(), (generation, revision));
                        self.schedule_host_document_render_retry(key, snapshot);
                    }
                }
            }
        } else {
            self.schedule_host_semantic_retry(namespace);
        }
    }

    fn disable_host_document_projection(
        &mut self,
        key: &(String, String),
        generation: u64,
        revision: u64,
    ) {
        let was_available = self
            .host_document_caches
            .get(key)
            .and_then(|cache| cache.current())
            .is_some();
        if let Some(cache) = self.host_document_caches.get_mut(key) {
            cache.disable();
        }
        if was_available {
            self.emit_host_document_updated(
                key.0.clone(),
                key.1.clone(),
                generation,
                revision,
                false,
            );
        }
    }

    pub(crate) fn emit_host_document_updated(
        &self,
        namespace: String,
        scope_id: String,
        generation: u64,
        revision: u64,
        available: bool,
    ) {
        self.event_hub.push(crate::api::schema::EventEnvelope {
            event: crate::api::schema::EventKind::HostDocumentUpdated,
            data: crate::api::schema::EventData::HostDocumentUpdated {
                namespace,
                scope_id,
                generation,
                revision,
                available,
            },
        });
    }

    pub(crate) fn handle_host_relationships_document_result(
        &mut self,
        mut result: crate::extension_service::HostRelationshipsDocumentAsyncResult,
    ) {
        let render_slot = result._render_slot.take();
        self.host_document_services.complete_render(
            &result.namespace,
            &result.snapshot.scope_id,
            result.snapshot.generation,
            result.snapshot.revision,
        );
        let key = (result.namespace.clone(), result.snapshot.scope_id.clone());
        let current = self.host_document_services.generation(&result.namespace)
            == Some(result.snapshot.generation)
            && self
                .host_document_request_watermarks
                .get(&key)
                .is_some_and(|watermark| {
                    *watermark == (result.snapshot.generation, result.snapshot.revision)
                });
        if current {
            let publication = result.result.and_then(|document| {
                if document.revision != result.snapshot.revision
                    || document.target != crate::extension_contract::HostTarget::Session
                    || !document.items.iter().all(|item| {
                        item.id.starts_with("relationship:")
                            && matches!(
                                item.role,
                                crate::extension_contract::HostItemRole::Terminal
                                    | crate::extension_contract::HostItemRole::Section
                            )
                    })
                {
                    return Err(
                        "relationship document does not match its admitted source or revision"
                            .into(),
                    );
                }
                self.host_document_caches
                    .entry(key)
                    .or_default()
                    .publish_for_namespace(
                        &result.namespace,
                        result.snapshot.generation,
                        result.provider_priority,
                        &result.grants,
                        document,
                    )
                    .map(|_| ())
                    .map_err(|error| error.to_string())
            });
            match publication {
                Ok(()) => self.complete_host_semantic_retry(&result.namespace),
                Err(error) => {
                    self.host_relationships_last_desired
                        .remove(&result.namespace);
                    self.schedule_host_semantic_retry(&result.namespace);
                    tracing::warn!(%error, "Delegation document render failed; using relationship fallback");
                }
            }
        }
        if current
            && self
                .host_document_caches
                .get(&(result.namespace.clone(), result.snapshot.scope_id.clone()))
                .and_then(|cache| cache.current())
                .is_some_and(|document| {
                    document.generation == result.snapshot.generation
                        && document.revision == result.snapshot.revision
                })
        {
            self.emit_host_document_updated(
                result.namespace.clone(),
                result.snapshot.scope_id.clone(),
                result.snapshot.generation,
                result.snapshot.revision,
                true,
            );
        }
        drop(render_slot);
        self.retry_one_host_document_render();
    }

    pub(crate) fn handle_host_agent_activity_document_result(
        &mut self,
        mut result: crate::extension_service::HostAgentActivityDocumentAsyncResult,
    ) {
        let render_slot = result._render_slot.take();
        self.host_document_services.complete_render(
            &result.namespace,
            &result.snapshot.scope_id,
            result.snapshot.generation,
            result.snapshot.revision,
        );
        let key = (result.namespace.clone(), result.snapshot.scope_id.clone());
        let current = self.host_document_services.generation(&result.namespace)
            == Some(result.snapshot.generation)
            && self
                .host_document_request_watermarks
                .get(&key)
                .is_some_and(|watermark| {
                    *watermark == (result.snapshot.generation, result.snapshot.revision)
                });
        if current {
            let publication = result.result.and_then(|document| {
                let attention = document.actions.is_empty()
                    && document.required_capabilities.is_empty()
                    && !document.items.is_empty()
                    && document.items.iter().all(|item| {
                        item.role == crate::extension_contract::HostItemRole::Section
                            && item.id.starts_with("context:attention.")
                            && item.parent_id.is_none()
                            && item.action_ids.is_empty()
                            && item.terminal.as_ref().is_none_or(|terminal| {
                                result
                                    .snapshot
                                    .terminals
                                    .as_ref()
                                    .is_some_and(|terminals| terminals.contains(terminal))
                            })
                    });
                if document.revision != result.snapshot.revision
                    || document.target != crate::extension_contract::HostTarget::Session
                    || !(attention
                        || document.items.iter().all(|item| match item.role {
                            crate::extension_contract::HostItemRole::Group => {
                                item.id.starts_with("activity:")
                            }
                            crate::extension_contract::HostItemRole::Terminal => {
                                !item.id.starts_with("relationship:")
                                    && !item.id.starts_with("context:")
                            }
                            _ => false,
                        }))
                {
                    return Err(
                        "activity document does not match its admitted source or revision".into(),
                    );
                }
                if let Some(current) = self
                    .host_document_caches
                    .get(&key)
                    .and_then(|cache| cache.current())
                {
                    let mut normalized = document.clone();
                    normalized.revision = current.revision;
                    if normalized == *current {
                        // Same output keeps published revision (and therefore actions) unchanged.
                        return Ok(());
                    }
                }
                self.host_document_caches
                    .entry(key)
                    .or_default()
                    .publish_for_namespace(
                        &result.namespace,
                        result.snapshot.generation,
                        result.provider_priority,
                        &result.grants,
                        document,
                    )
                    .map(|_| ())
                    .map_err(|error| error.to_string())
            });
            match publication {
                Ok(()) => self.complete_host_semantic_retry(&result.namespace),
                Err(error) => {
                    self.host_agent_activity_last_desired
                        .remove(&result.namespace);
                    self.schedule_host_semantic_retry(&result.namespace);
                    tracing::warn!(%error, "Agent Status document render failed; using native fallback");
                }
            }
        }
        if current
            && self
                .host_document_caches
                .get(&(result.namespace.clone(), result.snapshot.scope_id.clone()))
                .and_then(|cache| cache.current())
                .is_some_and(|document| {
                    document.generation == result.snapshot.generation
                        && document.revision == result.snapshot.revision
                })
        {
            self.emit_host_document_updated(
                result.namespace.clone(),
                result.snapshot.scope_id.clone(),
                result.snapshot.generation,
                result.snapshot.revision,
                true,
            );
        }
        drop(render_slot);
        self.retry_one_host_document_render();
    }

    fn schedule_host_semantic_retry(&mut self, namespace: &str) {
        let attempts = self
            .host_semantic_retry_attempts
            .get(namespace)
            .copied()
            .unwrap_or(0);
        if self.host_semantic_retry_scheduled.contains(namespace)
            || attempts >= MAX_HOST_SEMANTIC_RETRY_ATTEMPTS
            || self.host_document_services.generation(namespace).is_none()
        {
            return;
        }
        let delay_ms = 100_u64.saturating_mul(1_u64 << attempts.min(6)).min(5_000);
        self.host_semantic_retry_attempts
            .insert(namespace.to_string(), attempts.saturating_add(1));
        self.host_semantic_retry_scheduled
            .insert(namespace.to_string());
        let epoch = self
            .host_semantic_retry_epochs
            .entry(namespace.to_string())
            .or_insert(1)
            .to_owned();
        let namespace = namespace.to_string();
        let thread_namespace = namespace.clone();
        let event_tx = self.event_tx.clone();
        if let Err(error) = std::thread::Builder::new()
            .name("vqro-semantic-document-retry".into())
            .spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(delay_ms));
                let mut event = crate::events::AppEvent::HostSemanticDocumentRetryDue {
                    namespace: thread_namespace,
                    epoch,
                };
                loop {
                    match event_tx.try_send(event) {
                        Ok(()) | Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => return,
                        Err(tokio::sync::mpsc::error::TrySendError::Full(returned)) => {
                            event = returned;
                            std::thread::sleep(std::time::Duration::from_millis(10));
                        }
                    }
                }
            })
        {
            self.host_semantic_retry_scheduled
                .remove(namespace.as_str());
            tracing::warn!(%error, %namespace, "failed to schedule semantic document retry");
        }
    }

    fn complete_host_semantic_retry(&mut self, namespace: &str) {
        let epoch = self
            .host_semantic_retry_epochs
            .entry(namespace.to_string())
            .or_insert(1);
        *epoch = epoch.saturating_add(1).max(1);
        self.host_semantic_retry_attempts.remove(namespace);
        self.host_semantic_retry_scheduled.remove(namespace);
    }

    pub(crate) fn handle_host_document_result(
        &mut self,
        mut result: crate::extension_service::HostDocumentAsyncResult,
    ) {
        let render_slot = result._render_slot.take();
        let delayed_retry = self.apply_host_document_result(result);
        drop(render_slot);
        if !delayed_retry {
            self.retry_one_host_document_render();
        }
    }

    fn apply_host_document_result(
        &mut self,
        result: crate::extension_service::HostDocumentAsyncResult,
    ) -> bool {
        self.host_document_services.complete_render(
            &result.namespace,
            &result.snapshot.scope_id,
            result.snapshot.generation,
            result.snapshot.revision,
        );
        let current_generation = self.host_document_services.generation(&result.namespace);
        let request_key = (result.namespace.clone(), result.snapshot.scope_id.clone());
        let is_current_request = self
            .host_document_request_watermarks
            .get(&request_key)
            .is_some_and(|watermark| {
                *watermark == (result.snapshot.generation, result.snapshot.revision)
            });
        if current_generation != Some(result.snapshot.generation) || !is_current_request {
            return false;
        }
        let retry_id = (
            result.namespace.clone(),
            result.snapshot.scope_id.clone(),
            result.snapshot.generation,
            result.snapshot.revision,
        );
        if result.result.is_ok() {
            self.host_document_retry_attempts.remove(&retry_id);
            self.host_document_retry_scheduled.remove(&retry_id);
            self.host_document_retry_snapshots.remove(&request_key);
            self.host_document_retry_order
                .retain(|queued| queued != &request_key);
        }
        let retry_snapshot = result.result.is_err().then(|| result.snapshot.clone());
        let available = {
            let cache = self
                .host_document_caches
                .entry((result.namespace.clone(), result.snapshot.scope_id.clone()))
                .or_default();
            if cache.current().is_some_and(|current| {
                current.generation > result.snapshot.generation
                    || (current.generation == result.snapshot.generation
                        && (current.revision > result.snapshot.revision
                            || (current.revision == result.snapshot.revision
                                && current.namespace != "host.fallback")))
            }) {
                return false;
            }
            let published = result.result.and_then(|document| {
                if document.revision != result.snapshot.revision
                    || !matches!(
                        &document.target,
                        crate::extension_contract::HostTarget::Tab { tab_id }
                            if tab_id == &result.snapshot.scope_id
                    )
                {
                    return Err("document result does not match requested revision or scope".into());
                }
                cache
                    .publish_for_namespace(
                        &result.namespace,
                        result.snapshot.generation,
                        result.provider_priority,
                        &result.grants,
                        document,
                    )
                    .map_err(|error| error.to_string())
            });
            if let Err(error) = published {
                cache.disable();
                tracing::warn!(namespace = %result.namespace, %error, "host document provider result rejected");
            }
            cache.current().is_some()
        };
        self.emit_host_document_updated(
            result.namespace,
            result.snapshot.scope_id,
            result.snapshot.generation,
            result.snapshot.revision,
            available,
        );
        if let Some(snapshot) = retry_snapshot {
            return self.schedule_host_document_render_retry(request_key, snapshot);
        }
        false
    }

    pub(super) fn authorize_host_document_action(
        &mut self,
        params: &crate::api::schema::HostDocumentActionParams,
    ) -> Result<AuthorizedHostDocumentAction, String> {
        let cache = self
            .host_document_caches
            .get(&(params.namespace.clone(), params.scope_id.clone()))
            .or_else(|| {
                self.host_document_caches
                    .iter()
                    .find(|((_, scope_id), cache)| {
                        scope_id == &params.scope_id
                            && cache
                                .current()
                                .is_some_and(|document| document.namespace == params.namespace)
                    })
                    .map(|(_, cache)| cache)
            })
            .ok_or_else(|| "host document scope is unavailable".to_string())?;
        if self
            .host_document_request_watermarks
            .get(&(params.namespace.clone(), params.scope_id.clone()))
            .is_some_and(|watermark| *watermark != (params.generation, params.revision))
        {
            return Err("host document action revision was superseded".into());
        }
        let grants = cache.grants().clone();
        let document = cache
            .current()
            .filter(|document| {
                document.namespace == params.namespace
                    && document.generation == params.generation
                    && document.revision == params.revision
            })
            .cloned()
            .ok_or_else(|| "host document action revision is stale".to_string())?;
        let (ws_idx, tab_idx) = self
            .state
            .workspaces
            .iter()
            .enumerate()
            .find_map(|(ws_idx, workspace)| {
                workspace.tabs.iter().enumerate().find_map(|(tab_idx, _)| {
                    (self.public_tab_id(ws_idx, tab_idx).as_deref()
                        == Some(params.scope_id.as_str()))
                    .then_some((ws_idx, tab_idx))
                })
            })
            .ok_or_else(|| "host document scope no longer exists".to_string())?;
        let snapshot = self
            .legacy_terminal_groups_snapshot(ws_idx, tab_idx, params.generation, params.revision)
            .map_err(|error| error.to_string())?;
        let Some(action) = document
            .actions
            .iter()
            .find(|action| action.id == params.action_id)
        else {
            return Err("document action not found".into());
        };
        if matches!(
            action.operation,
            crate::extension_contract::HostDocumentOperation::Extension { .. }
        ) {
            let action = document
                .authorize_extension_action_dispatch(&params.action_id, &snapshot)
                .map_err(|error| error.to_string())?;
            return Ok(AuthorizedHostDocumentAction::Provider(Box::new((
                snapshot, action,
            ))));
        }
        let confirmed = if action.confirmation
            == crate::extension_contract::HostConfirmation::Required
        {
            let key = (
                params.namespace.clone(),
                params.scope_id.clone(),
                params.generation,
                params.revision,
                params.action_id.clone(),
            );
            let grant = self
                .host_document_confirmation_grants
                .get(&key)
                .filter(|grant| grant.expires_at > std::time::Instant::now())
                .cloned()
                .ok_or_else(|| {
                    "host-issued confirmation token is missing or expired".to_string()
                })?;
            let expected_terminal = match &action.operation {
                crate::extension_contract::HostDocumentOperation::Host {
                    effect: crate::extension_contract::HostEffect::TerminalInterrupt { terminal_id },
                } => terminal_id,
                _ => return Err("confirmation grant is not bound to an interrupt action".into()),
            };
            if !params.confirmed
                || params.confirmation_token.as_deref() != Some(grant.token.as_str())
                || &grant.terminal_id != expected_terminal
            {
                return Err("host-issued confirmation token is invalid".into());
            }
            true
        } else {
            params.confirmed
        };
        let effect = document
            .authorize_host_effect_dispatch(&params.action_id, &snapshot, &grants, confirmed)
            .map_err(|error| error.to_string())?;
        if action.confirmation == crate::extension_contract::HostConfirmation::Required {
            self.host_document_confirmation_grants.remove(&(
                params.namespace.clone(),
                params.scope_id.clone(),
                params.generation,
                params.revision,
                params.action_id.clone(),
            ));
        }
        let terminal_id = match effect {
            crate::extension_contract::HostEffect::FocusTerminal { terminal_id }
            | crate::extension_contract::HostEffect::TerminalInterrupt { terminal_id } => {
                terminal_id
            }
        };
        let pane_id = snapshot
            .groups
            .iter()
            .flat_map(|group| &group.members)
            .chain(&snapshot.ungrouped)
            .find(|member| member.terminal.terminal_id == *terminal_id)
            .and_then(|member| member.terminal.placement.as_ref())
            .map(|placement| placement.pane_id.clone())
            .ok_or_else(|| "host document terminal placement is unavailable".to_string())?;
        let pane = crate::api::schema::PaneTarget { pane_id };
        Ok(match effect {
            crate::extension_contract::HostEffect::FocusTerminal { .. } => {
                AuthorizedHostDocumentAction::Focus(pane)
            }
            crate::extension_contract::HostEffect::TerminalInterrupt { .. } => {
                AuthorizedHostDocumentAction::Interrupt(pane)
            }
        })
    }

    pub(crate) fn handle_deferred_host_document_api_request(
        &mut self,
        request_id: String,
        params: crate::api::schema::HostDocumentActionParams,
        respond_to: std::sync::mpsc::Sender<String>,
    ) -> bool {
        let action = match self.authorize_host_document_action(&params) {
            Ok(action) => action,
            Err(error) => {
                let confirmation_rejected = error.contains("confirmation token");
                if confirmation_rejected {
                    self.host_document_confirmation_grants.remove(&(
                        params.namespace.clone(),
                        params.scope_id.clone(),
                        params.generation,
                        params.revision,
                        params.action_id.clone(),
                    ));
                }
                let _ = respond_to.send(responses::encode_error(
                    request_id,
                    "host_document_action_rejected",
                    &error,
                ));
                return confirmation_rejected;
            }
        };
        match action {
            AuthorizedHostDocumentAction::Focus(pane) => {
                let response = self.handle_pane_focus(request_id, pane);
                let _ = respond_to.send(response);
                true
            }
            AuthorizedHostDocumentAction::Interrupt(pane) => {
                let response = self.handle_pane_send_keys(
                    request_id,
                    crate::api::schema::PaneSendKeysParams {
                        pane_id: pane.pane_id,
                        keys: vec!["ctrl+c".into()],
                    },
                );
                let _ = respond_to.send(response);
                true
            }
            AuthorizedHostDocumentAction::Provider(provider_action) => {
                let (snapshot, action) = *provider_action;
                if let Err(error) = self.host_document_services.request_action_deferred(
                    snapshot,
                    action,
                    request_id.clone(),
                    respond_to.clone(),
                ) {
                    let _ = respond_to.send(responses::encode_error(
                        request_id,
                        "host_document_action_rejected",
                        &error,
                    ));
                }
                false
            }
        }
    }

    pub(super) fn handle_host_document_list(
        &mut self,
        id: String,
        params: crate::api::schema::HostDocumentListParams,
    ) -> String {
        let documents = self
            .host_documents()
            .into_iter()
            .filter(|entry| {
                params
                    .namespace
                    .as_ref()
                    .is_none_or(|namespace| entry.document.namespace == *namespace)
                    && params.scope_id.as_ref().is_none_or(|scope_id| {
                        match &entry.document.target {
                            crate::extension_contract::HostTarget::Session => {
                                scope_id == crate::spaces_extension_contract::SPACES_SCOPE_ID
                            }
                            crate::extension_contract::HostTarget::Workspace { workspace_id } => {
                                scope_id == workspace_id
                            }
                            crate::extension_contract::HostTarget::Tab { tab_id } => {
                                scope_id == tab_id
                            }
                            crate::extension_contract::HostTarget::Pane { pane_id } => {
                                scope_id == pane_id
                            }
                        }
                    })
            })
            .map(|entry| crate::api::schema::HostDocumentProjection {
                provider_priority: entry.provider_priority,
                document: entry.document,
                confirmation_tokens: entry.confirmation_tokens,
            })
            .collect();
        responses::encode_success(
            id,
            crate::api::schema::ResponseResult::HostDocumentList { documents },
        )
    }

    pub(crate) fn host_documents(
        &mut self,
    ) -> Vec<crate::protocol::endpoint::HostDocumentProjectionEntryV1> {
        let mut live_tab_ids = std::collections::HashSet::from([
            crate::spaces_extension_contract::SPACES_SCOPE_ID.to_string(),
        ]);
        for (ws_idx, workspace) in self.state.workspaces.iter().enumerate() {
            for tab_idx in 0..workspace.tabs.len() {
                if let Some(tab_id) = self.public_tab_id(ws_idx, tab_idx) {
                    live_tab_ids.insert(tab_id);
                }
            }
        }
        let now = std::time::Instant::now();
        self.host_document_confirmation_grants
            .retain(|(_, scope_id, _, _, _), grant| {
                live_tab_ids.contains(scope_id) && grant.expires_at > now
            });
        let mut candidates = self
            .host_document_caches
            .iter()
            .filter(|((_, scope_id), cache)| {
                live_tab_ids.contains(scope_id) && cache.current().is_some()
            })
            .collect::<Vec<_>>();
        candidates.sort_by(|left, right| {
            let (Some(left_document), Some(right_document)) = (left.1.current(), right.1.current())
            else {
                return std::cmp::Ordering::Equal;
            };
            right
                .1
                .provider_priority()
                .cmp(&left.1.provider_priority())
                .then_with(|| left_document.namespace.cmp(&right_document.namespace))
                .then_with(|| left_document.target.cmp(&right_document.target))
        });
        let mut encoded_bytes = 256usize;
        candidates.retain(|(_, cache)| {
            let Some(next) = encoded_bytes.checked_add(cache.projected_encoded_bytes()) else {
                return false;
            };
            if next > MAX_HOST_DOCUMENTS_PROJECTION_BYTES {
                return false;
            }
            encoded_bytes = next;
            true
        });

        let mut documents = Vec::with_capacity(candidates.len());
        for ((_, scope_id), cache) in candidates {
            let Some(document) = cache.current().cloned() else {
                continue;
            };
            let mut confirmation_tokens = std::collections::BTreeMap::new();
            for action in &document.actions {
                let action_is_reachable = action.subject_id.as_deref().is_some_and(|subject_id| {
                    document.items.iter().any(|item| {
                        item.id == subject_id
                            && !item.flags.disabled
                            && item.action_ids.iter().any(|id| id == &action.id)
                    })
                });
                if !action_is_reachable {
                    continue;
                }
                let crate::extension_contract::HostDocumentOperation::Host {
                    effect: crate::extension_contract::HostEffect::TerminalInterrupt { terminal_id },
                } = &action.operation
                else {
                    continue;
                };
                if action.confirmation != crate::extension_contract::HostConfirmation::Required {
                    continue;
                }
                let key = (
                    document.namespace.clone(),
                    scope_id.clone(),
                    document.generation,
                    document.revision,
                    action.id.clone(),
                );
                if !self.host_document_confirmation_grants.contains_key(&key) {
                    if self.host_document_confirmation_grants.len()
                        >= MAX_HOST_DOCUMENT_CONFIRMATION_GRANTS
                    {
                        continue;
                    }
                    let mut bytes = [0_u8; 24];
                    if crate::platform::fill_secure_random(&mut bytes).is_err() {
                        continue;
                    }
                    let token = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
                    self.host_document_confirmation_grants.insert(
                        key.clone(),
                        HostDocumentConfirmationGrant {
                            token,
                            terminal_id: terminal_id.clone(),
                            expires_at: now + std::time::Duration::from_secs(30),
                        },
                    );
                }
                if let Some(grant) = self.host_document_confirmation_grants.get(&key) {
                    confirmation_tokens.insert(action.id.clone(), grant.token.clone());
                }
            }
            documents.push(crate::protocol::endpoint::HostDocumentProjectionEntryV1 {
                provider_priority: cache.provider_priority(),
                document,
                confirmation_tokens,
            });
        }
        documents
    }

    pub(super) fn legacy_terminal_groups_snapshot(
        &self,
        ws_idx: usize,
        tab_idx: usize,
        generation: u64,
        revision: u64,
    ) -> Result<
        crate::extension_contract::HostTerminalGroupsV1,
        crate::extension_contract::HostDocumentError,
    > {
        use crate::extension_contract::{
            HostTerminalGroup, HostTerminalGroupMember, HostTerminalGroupsV1,
            HostTerminalLifecycle, HostTerminalPlacement, HostTerminalRef,
        };
        use crate::layout::PanePlacement;

        let workspace = &self.state.workspaces[ws_idx];
        let tab = &workspace.tabs[tab_idx];
        let workspace_id = self.public_workspace_id(ws_idx);
        let tab_id = self.public_tab_id(ws_idx, tab_idx).ok_or_else(|| {
            crate::extension_contract::HostDocumentError("tab has no stable id".into())
        })?;
        let member = |pane_id: crate::layout::PaneId, archived: bool| {
            let pane = tab.panes.get(&pane_id)?;
            let terminal = self.state.terminals.get(&pane.attached_terminal_id)?;
            Some(HostTerminalGroupMember {
                terminal: HostTerminalRef {
                    terminal_id: terminal.id.to_string(),
                    placement: Some(HostTerminalPlacement {
                        pane_id: self.public_pane_id(ws_idx, pane_id)?,
                        workspace_id: workspace_id.clone(),
                        tab_id: tab_id.clone(),
                    }),
                },
                label: terminal
                    .manual_label
                    .as_ref()
                    .or(terminal.agent_name.as_ref())
                    .or(terminal.terminal_title.as_ref())
                    .cloned()
                    .unwrap_or_else(|| format!("Terminal {}", pane_id.raw())),
                lifecycle: if archived {
                    HostTerminalLifecycle::Archived
                } else {
                    HostTerminalLifecycle::Live
                },
            })
        };
        let groups = tab
            .collections()
            .map(|collection| HostTerminalGroup {
                group_id: collection_id_text(collection.id()),
                label: collection.legacy_label().map(str::to_owned),
                selected_terminal_id: collection.selected().and_then(|pane_id| {
                    tab.panes
                        .get(&pane_id)
                        .map(|pane| pane.attached_terminal_id.to_string())
                }),
                members: collection
                    .members()
                    .iter()
                    .filter_map(|pane_id| member(*pane_id, collection.legacy_is_archived(*pane_id)))
                    .collect(),
            })
            .collect();
        let ungrouped = tab
            .panes
            .keys()
            .copied()
            .filter(|pane_id| tab.pane_placement(*pane_id) == Some(PanePlacement::Tiled))
            .filter_map(|pane_id| member(pane_id, false))
            .collect();
        Ok(HostTerminalGroupsV1 {
            contract: "host.terminal-groups.v1".into(),
            generation,
            revision,
            scope_id: tab_id,
            groups,
            ungrouped,
        })
    }

    #[cfg(test)]
    pub(crate) fn legacy_collections_fixture_document(
        &self,
        ws_idx: usize,
        tab_idx: usize,
        generation: u64,
        revision: u64,
    ) -> Result<
        crate::extension_contract::HostDocumentV1,
        crate::extension_contract::HostDocumentError,
    > {
        let snapshot =
            self.legacy_terminal_groups_snapshot(ws_idx, tab_idx, generation, revision)?;
        crate::first_party::collections_legacy_fixture::project(&snapshot)
    }

    pub(super) fn find_collection(&self, id: CollectionId) -> Option<(usize, usize)> {
        self.state
            .workspaces
            .iter()
            .enumerate()
            .find_map(|(ws_idx, ws)| {
                ws.tabs
                    .iter()
                    .position(|tab| tab.collection(id).is_some())
                    .map(|tab_idx| (ws_idx, tab_idx))
            })
    }

    pub(crate) fn collection_info(
        &self,
        ws_idx: usize,
        tab_idx: usize,
        id: CollectionId,
    ) -> Option<CollectionInfo> {
        let ws = self.state.workspaces.get(ws_idx)?;
        let tab = ws.tabs.get(tab_idx)?;
        let collection = tab.collection(id)?;
        let collection_id = collection_id_text(id);
        let tab_id = self.public_tab_id(ws_idx, tab_idx)?;
        let package_projection = (self.collections_authority.phase
            == crate::collections_extension_contract::CollectionsAuthorityPhaseV1::Package)
            .then(|| self.collections_package_projections.get(&tab_id))
            .flatten()
            .and_then(|projection| {
                projection
                    .collections
                    .iter()
                    .find(|projected| projected.collection_id == collection_id)
            })?;
        let projected_member = |pane_id: &str| {
            package_projection
                .members
                .iter()
                .find(|member| member.pane_id == pane_id)
        };
        Some(CollectionInfo {
            collection_id,
            workspace_id: self.public_workspace_id(ws_idx),
            tab_id,
            label: package_projection.label.clone(),
            focused: tab.layout.focused_leaf() == LayoutLeaf::Collection(id.into()),
            selected_pane_id: package_projection.selected_pane_id.clone(),
            members: collection
                .members()
                .iter()
                .enumerate()
                .filter_map(|(index, pane)| {
                    let pane_id = self.public_pane_id(ws_idx, *pane)?;
                    Some(CollectionMemberInfo {
                        archived: projected_member(&pane_id).is_some_and(|member| member.archived),
                        selected: package_projection.selected_pane_id.as_deref()
                            == Some(pane_id.as_str()),
                        pane_id,
                        index,
                    })
                })
                .collect(),
        })
    }

    pub(crate) fn collection_infos(
        &self,
        workspace_filter: Option<usize>,
        tab_filter: Option<(usize, usize)>,
    ) -> Vec<CollectionInfo> {
        let mut collections = Vec::new();
        for (ws_idx, ws) in self.state.workspaces.iter().enumerate() {
            for (tab_idx, tab) in ws.tabs.iter().enumerate() {
                if workspace_filter.is_some_and(|wanted| wanted != ws_idx)
                    || tab_filter.is_some_and(|wanted| wanted != (ws_idx, tab_idx))
                {
                    continue;
                }
                for id in tab.layout.collection_ids() {
                    if let Some(info) = self.collection_info(ws_idx, tab_idx, id.into()) {
                        collections.push(info);
                    }
                }
            }
        }
        collections
    }

    pub(crate) fn handle_collection_list(
        &self,
        request_id: String,
        params: CollectionListParams,
    ) -> String {
        let workspace_filter = params
            .workspace_id
            .as_deref()
            .and_then(|id| self.parse_workspace_id(id));
        let tab_filter = params
            .tab_id
            .as_deref()
            .and_then(|id| self.parse_tab_id(id));
        if params.workspace_id.is_some() && workspace_filter.is_none()
            || params.tab_id.is_some() && tab_filter.is_none()
        {
            return responses::encode_error(
                request_id,
                "not_found",
                "workspace or tab was not found",
            );
        }
        let collections = self.collection_infos(workspace_filter, tab_filter);
        responses::encode_success(request_id, ResponseResult::CollectionList { collections })
    }

    pub(crate) fn handle_collection_get(
        &self,
        request_id: String,
        target: CollectionTarget,
    ) -> String {
        let Ok(id) = CollectionId::parse(&target.collection_id) else {
            return responses::encode_error(request_id, "invalid_id", "invalid collection id");
        };
        let Some((ws_idx, tab_idx)) = self.find_collection(id) else {
            return responses::encode_error(request_id, "not_found", "collection was not found");
        };
        responses::encode_success(
            request_id,
            ResponseResult::CollectionInfo {
                collection: self
                    .collection_info(ws_idx, tab_idx, id)
                    .expect("located collection"),
            },
        )
    }

    pub(crate) fn handle_collection_create(
        &mut self,
        request_id: String,
        _params: CollectionCreateParams,
    ) -> String {
        responses::encode_error(
            request_id,
            "collections_atomic_create_required",
            "creation requires the package-authority reservation coordinator",
        )
    }

    pub(crate) fn handle_collection_add(
        &mut self,
        request_id: String,
        params: CollectionAddParams,
    ) -> String {
        let Ok(collection_id) = CollectionId::parse(&params.collection_id) else {
            return responses::encode_error(request_id, "invalid_id", "invalid collection id");
        };
        let Some((ws_idx, tab_idx)) = self.find_collection(collection_id) else {
            return responses::encode_error(request_id, "not_found", "collection was not found");
        };
        let Some((pane_ws_idx, pane_id)) = self.parse_current_public_pane_id(&params.pane_id)
        else {
            return responses::encode_error(request_id, "not_found", "pane was not found");
        };
        if pane_ws_idx != ws_idx
            || self.state.workspaces[ws_idx].find_tab_index_for_pane(pane_id) != Some(tab_idx)
        {
            return responses::encode_error(
                request_id,
                "cross_tab_unsupported",
                "the first port only collects panes from the same tab",
            );
        }
        let member_count = self.state.workspaces[ws_idx].tabs[tab_idx]
            .collection(collection_id)
            .map(|collection| collection.members().len())
            .unwrap_or(0);
        if let Err(error) = crate::admission::group_member(member_count) {
            return responses::encode_error(request_id, "resource_limit", error.to_string());
        }
        if self.state.workspaces[ws_idx].tabs[tab_idx]
            .collect_tiled_pane(pane_id, collection_id)
            .is_err()
        {
            return responses::encode_error(
                request_id,
                "invalid_placement",
                "pane could not be added to the collection",
            );
        }
        self.state.mark_session_dirty();
        let collection = self
            .collection_info(ws_idx, tab_idx, collection_id)
            .expect("updated collection");
        self.emit_collection_event("member_added", collection.clone());
        responses::encode_success(request_id, ResponseResult::CollectionInfo { collection })
    }

    pub(crate) fn handle_collection_select(
        &mut self,
        request_id: String,
        params: CollectionSelectParams,
    ) -> String {
        self.mutate_collection_member(
            request_id,
            &params.collection_id,
            &params.pane_id,
            |tab, collection, pane| {
                tab.select_collection_member(collection, pane)?;
                if params.focus {
                    tab.focus_layout_leaf(LayoutLeaf::Collection(collection.into()));
                }
                Ok(())
            },
        )
    }

    pub(crate) fn handle_collection_reorder(
        &mut self,
        request_id: String,
        params: CollectionReorderParams,
    ) -> String {
        self.mutate_collection_member(
            request_id,
            &params.collection_id,
            &params.pane_id,
            |tab, collection, pane| tab.reorder_collection_member(collection, pane, params.index),
        )
    }

    pub(crate) fn handle_collection_archive(
        &mut self,
        request_id: String,
        _params: CollectionMemberTarget,
        _archived: bool,
    ) -> String {
        responses::encode_error(
            request_id,
            "collections_action_required",
            "archive policy changes require an authoritative package action",
        )
    }

    fn mutate_collection_member<F>(
        &mut self,
        request_id: String,
        collection_raw: &str,
        pane_raw: &str,
        mutate: F,
    ) -> String
    where
        F: FnOnce(
            &mut crate::workspace::Tab,
            CollectionId,
            crate::layout::PaneId,
        ) -> Result<(), crate::workspace::CollectionMutationError>,
    {
        let Ok(collection_id) = CollectionId::parse(collection_raw) else {
            return responses::encode_error(request_id, "invalid_id", "invalid collection id");
        };
        let Some((ws_idx, tab_idx)) = self.find_collection(collection_id) else {
            return responses::encode_error(request_id, "not_found", "collection was not found");
        };
        let Some((pane_ws_idx, pane_id)) = self.parse_current_public_pane_id(pane_raw) else {
            return responses::encode_error(request_id, "not_found", "pane was not found");
        };
        if pane_ws_idx != ws_idx
            || mutate(
                &mut self.state.workspaces[ws_idx].tabs[tab_idx],
                collection_id,
                pane_id,
            )
            .is_err()
        {
            return responses::encode_error(
                request_id,
                "invalid_placement",
                "pane is not a member of the collection",
            );
        }
        self.state.mark_session_dirty();
        let collection = self
            .collection_info(ws_idx, tab_idx, collection_id)
            .expect("updated collection");
        self.emit_collection_event("member_updated", collection.clone());
        responses::encode_success(request_id, ResponseResult::CollectionInfo { collection })
    }

    pub(crate) fn handle_collection_promote(
        &mut self,
        request_id: String,
        params: CollectionPromoteParams,
    ) -> String {
        let Some((ws_idx, pane_id)) = self.parse_current_public_pane_id(&params.pane_id) else {
            return responses::encode_error(request_id, "not_found", "pane was not found");
        };
        let Some((target_ws_idx, target)) =
            self.parse_current_public_pane_id(&params.target_pane_id)
        else {
            return responses::encode_error(request_id, "not_found", "target pane was not found");
        };
        if ws_idx != target_ws_idx {
            return responses::encode_error(
                request_id,
                "cross_workspace_unsupported",
                "promotion target must be in the same workspace",
            );
        }
        let Some(tab_idx) = self.state.workspaces[ws_idx].find_tab_index_for_pane(pane_id) else {
            return responses::encode_error(request_id, "not_found", "pane tab was not found");
        };
        if self.state.workspaces[ws_idx].find_tab_index_for_pane(target) != Some(tab_idx) {
            return responses::encode_error(
                request_id,
                "cross_tab_unsupported",
                "promotion target must be in the same tab",
            );
        }
        let Some(crate::layout::PanePlacement::Collection(collection_id)) =
            self.state.workspaces[ws_idx].tabs[tab_idx].pane_placement(pane_id)
        else {
            return responses::encode_error(
                request_id,
                "invalid_placement",
                "pane is not collected",
            );
        };
        if self.state.workspaces[ws_idx].tabs[tab_idx]
            .promote_collection_member_near(
                pane_id,
                collection_id.into(),
                target,
                direction(params.direction),
                params.ratio.unwrap_or(0.5),
            )
            .is_err()
        {
            return responses::encode_error(
                request_id,
                "invalid_placement",
                "pane could not be promoted",
            );
        }
        if !params.focus {
            self.state.workspaces[ws_idx].tabs[tab_idx].focus_pane(target);
        }
        self.state.mark_session_dirty();
        let collection = self
            .collection_info(ws_idx, tab_idx, collection_id.into())
            .expect("updated collection");
        self.emit_collection_event("member_promoted", collection.clone());
        responses::encode_success(request_id, ResponseResult::CollectionInfo { collection })
    }

    pub(crate) fn handle_collection_close(
        &mut self,
        request_id: String,
        params: CollectionCloseParams,
    ) -> String {
        let Ok(collection_id) = CollectionId::parse(&params.collection_id) else {
            return responses::encode_error(request_id, "invalid_id", "invalid collection id");
        };
        let Some((ws_idx, tab_idx)) = self.find_collection(collection_id) else {
            return responses::encode_error(request_id, "not_found", "collection was not found");
        };
        let members = self.state.workspaces[ws_idx].tabs[tab_idx]
            .collection(collection_id)
            .expect("located collection")
            .members()
            .to_vec();
        if members.is_empty() {
            if self.state.workspaces[ws_idx].tabs[tab_idx]
                .remove_empty_collection(collection_id)
                .is_err()
            {
                return responses::encode_error(
                    request_id,
                    "invalid_placement",
                    "collection cannot be removed as the last layout leaf",
                );
            }
        } else {
            let Some(disposition) = params.disposition else {
                return responses::encode_error(
                    request_id,
                    "collection_disposition_required",
                    "non-empty collection close requires cascade_close or promote_members",
                );
            };
            match disposition {
                CollectionCloseDisposition::CascadeClose => {
                    let public_members = members
                        .iter()
                        .filter_map(|pane_id| self.public_pane_id(ws_idx, *pane_id))
                        .collect::<Vec<_>>();
                    if public_members.len() != members.len() {
                        return responses::encode_error(
                            request_id,
                            "collection_close_failed",
                            "collection contains an unaddressable pane",
                        );
                    }
                    for (index, pane_id) in public_members.into_iter().enumerate() {
                        if let Err(response) = self.close_pane(
                            format!("{request_id}:member:{index}"),
                            &crate::api::schema::PaneTarget { pane_id },
                        ) {
                            return response;
                        }
                    }
                    if let Some((remaining_ws, remaining_tab)) = self.find_collection(collection_id)
                    {
                        if self.state.workspaces[remaining_ws].tabs[remaining_tab]
                            .remove_empty_collection(collection_id)
                            .is_err()
                        {
                            return responses::encode_error(
                                request_id,
                                "collection_close_failed",
                                "empty collection could not be removed after closing members",
                            );
                        }
                    }
                }
                CollectionCloseDisposition::PromoteMembers => {
                    if let Err(error) = self.state.workspaces[ws_idx]
                        .ensure_public_tab_numbers_available(members.len())
                    {
                        return responses::encode_error(
                            request_id,
                            "collection_close_failed",
                            error.to_string(),
                        );
                    }
                    let tab_ids = match self.allocate_tab_ids(members.len()) {
                        Ok(ids) => ids,
                        Err(error) => {
                            return responses::encode_error(
                                request_id,
                                "collection_close_failed",
                                error.to_string(),
                            );
                        }
                    };
                    let (events, render_notify, render_dirty) = {
                        let tab = &self.state.workspaces[ws_idx].tabs[tab_idx];
                        (
                            tab.events.clone(),
                            tab.render_notify.clone(),
                            tab.render_dirty.clone(),
                        )
                    };
                    let mut created = Vec::with_capacity(members.len());
                    for (pane_id, tab_id) in members.into_iter().zip(tab_ids) {
                        let Some(taken) = self.state.workspaces[ws_idx].take_pane_for_move(pane_id)
                        else {
                            return responses::encode_error(
                                request_id,
                                "collection_close_failed",
                                "collection member could not be detached",
                            );
                        };
                        let tab_idx = match self.state.workspaces[ws_idx]
                            .create_tab_from_existing_pane(
                                tab_id,
                                taken.moved,
                                None,
                                events.clone(),
                                render_notify.clone(),
                                render_dirty.clone(),
                            ) {
                            Ok(tab_idx) => tab_idx,
                            Err((error, _tab_id, _moved)) => {
                                return responses::encode_error(
                                    request_id,
                                    "collection_close_failed",
                                    error.to_string(),
                                );
                            }
                        };
                        created.push(tab_idx);
                    }
                    if let Some((remaining_ws, remaining_tab)) = self.find_collection(collection_id)
                    {
                        if self.state.workspaces[remaining_ws].tabs[remaining_tab]
                            .remove_empty_collection(collection_id)
                            .is_err()
                        {
                            return responses::encode_error(
                                request_id,
                                "collection_close_failed",
                                "empty collection could not be removed after promoting members",
                            );
                        }
                    }
                    if let Some(first) = created.first().copied() {
                        self.state.workspaces[ws_idx].active_tab = first;
                    }
                }
            }
        }
        self.state.mark_session_dirty();
        self.schedule_session_save();
        self.refresh_collection_documents();
        responses::encode_success(request_id, ResponseResult::Ok {})
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn handle_current_result(
        app: &mut App,
        result: crate::extension_service::HostDocumentAsyncResult,
    ) {
        app.host_document_request_watermarks.insert(
            (result.namespace.clone(), result.snapshot.scope_id.clone()),
            (result.snapshot.generation, result.snapshot.revision),
        );
        app.handle_host_document_result(result);
    }

    fn app_with_workspace() -> App {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![crate::workspace::Workspace::test_new("collections")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.ensure_test_terminals();
        app
    }

    fn app_with_nonempty_collection() -> (App, CollectionId, crate::layout::PaneId) {
        let mut app = app_with_workspace();
        let root = app.state.workspaces[0].tabs[0].root_pane;
        let member = app.state.workspaces[0].test_split(ratatui::layout::Direction::Horizontal);
        let collection = app.state.workspaces[0].tabs[0]
            .create_collection_near(
                crate::layout::LayoutLeaf::Pane(root),
                ratatui::layout::Direction::Vertical,
                0.5,
                Some("helpers".into()),
            )
            .unwrap();
        app.state.workspaces[0].tabs[0]
            .collect_tiled_pane(member, collection)
            .unwrap();
        app.state.ensure_test_terminals();
        (app, collection, member)
    }

    #[test]
    fn collections_action_sequence_rebases_to_durable_state_after_restart() {
        let mut app = app_with_workspace();
        let initial_generation = app.extension_state.store_generation;
        app.extension_state
            .transact(&crate::extension_state::ExtensionStateTransactionV1 {
                contract: crate::extension_state::EXTENSION_STATE_CONTRACT_V1.into(),
                namespace: crate::collections_extension_contract::COLLECTIONS_NAMESPACE.into(),
                expected_store_generation: initial_generation,
                expected_revision: 0,
                // Action 1 failed before state commit; action 2 was the first durable receipt.
                idempotency_key: "collections-r3-create-2".into(),
                operations: vec![crate::extension_state::ExtensionStateOperationV1 {
                    key: "prior-key".into(),
                    value: Some(serde_json::json!({"prior": true})),
                }],
            })
            .unwrap();
        assert_eq!(app.extension_state.sequence, 1);

        app.next_collections_action_id = app.extension_state.idempotency_suffix_high_water(
            crate::collections_extension_contract::COLLECTIONS_NAMESPACE,
            COLLECTIONS_ACTION_IDEMPOTENCY_PREFIXES,
        );
        let sequence = app.reserve_collections_action_sequence().unwrap();
        assert_eq!(sequence, 3);
        app.extension_state
            .transact(&crate::extension_state::ExtensionStateTransactionV1 {
                contract: crate::extension_state::EXTENSION_STATE_CONTRACT_V1.into(),
                namespace: crate::collections_extension_contract::COLLECTIONS_NAMESPACE.into(),
                expected_store_generation: initial_generation,
                expected_revision: 1,
                idempotency_key: format!("collections-r3-create-{sequence}"),
                operations: vec![crate::extension_state::ExtensionStateOperationV1 {
                    key: "next-key".into(),
                    value: Some(serde_json::json!({"next": true})),
                }],
            })
            .expect("first post-restart action must not reuse a durable receipt key");
        app.collections_authority.phase =
            crate::collections_extension_contract::CollectionsAuthorityPhaseV1::Package;
        assert_eq!(
            app.capture_session_snapshot().collections_action_sequence,
            3
        );
    }

    #[test]
    fn collections_action_sequence_rejects_attempt_id_overflow() {
        let mut app = app_with_workspace();
        app.next_collections_action_id = MAX_COLLECTIONS_ACTION_SEQUENCE;
        assert_eq!(
            app.reserve_collections_action_sequence().unwrap_err(),
            "Collections action sequence exhausted"
        );
    }

    #[test]
    fn collection_shadow_uses_stable_terminal_identity_and_generic_contracts() {
        let (app, collection, member) = app_with_nonempty_collection();
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&member]
            .attached_terminal_id
            .to_string();

        let document = app.legacy_collections_fixture_document(0, 0, 4, 9).unwrap();

        assert_eq!(
            document.namespace,
            crate::collections_extension_contract::COLLECTIONS_NAMESPACE
        );
        assert_eq!(document.generation, 4);
        assert_eq!(document.revision, 9);
        let group = document
            .items
            .iter()
            .find(|item| item.id == collection_id_text(collection))
            .or_else(|| {
                document
                    .items
                    .iter()
                    .find(|item| item.id == format!("group:{}", collection_id_text(collection)))
            })
            .expect("collection group projection");
        assert_eq!(group.role, crate::extension_contract::HostItemRole::Group);
        let projected = document
            .items
            .iter()
            .find(|item| {
                item.terminal
                    .as_ref()
                    .is_some_and(|terminal| terminal.terminal_id == terminal_id)
            })
            .expect("member terminal projection");
        assert_eq!(
            projected
                .terminal
                .as_ref()
                .unwrap()
                .placement
                .as_ref()
                .unwrap()
                .pane_id,
            app.public_pane_id(0, member).unwrap()
        );
    }

    #[test]
    fn collection_close_failure_is_atomic_and_allocates_nothing() {
        let (mut app, collection, _member) = app_with_nonempty_collection();
        let before = serde_json::to_value(app.collection_info(0, 0, collection)).unwrap();
        let event_sequence = app.event_hub.current_sequence();
        let next_pane = crate::layout::PaneId::next_for_test();

        let response = app.handle_collection_close(
            "missing-disposition".into(),
            CollectionCloseParams {
                collection_id: collection_id_text(collection),
                disposition: None,
            },
        );
        let response: crate::api::schema::ErrorResponse = serde_json::from_str(&response).unwrap();

        assert_eq!(response.error.code, "collection_disposition_required");
        assert_eq!(
            serde_json::to_value(app.collection_info(0, 0, collection)).unwrap(),
            before
        );
        assert_eq!(app.event_hub.current_sequence(), event_sequence);
        assert_eq!(crate::layout::PaneId::next_for_test(), next_pane);
    }

    #[test]
    fn collection_close_cascades_or_promotes_without_restarting_member_identity() {
        let (mut cascade, collection, member) = app_with_nonempty_collection();
        let closed_pane_id = cascade.public_pane_id(0, member).unwrap();
        let response = cascade.handle_collection_close(
            "cascade".into(),
            CollectionCloseParams {
                collection_id: format!("collection_{}", collection.raw()),
                disposition: Some(CollectionCloseDisposition::CascadeClose),
            },
        );
        assert!(serde_json::from_str::<crate::api::schema::SuccessResponse>(&response).is_ok());
        assert!(cascade.state.workspaces[0]
            .find_tab_index_for_pane(member)
            .is_none());
        assert!(cascade.find_collection(collection).is_none());
        assert!(cascade.host_documents().iter().all(|entry| {
            entry.document.items.iter().all(|item| {
                item.terminal
                    .as_ref()
                    .and_then(|terminal| terminal.placement.as_ref())
                    .is_none_or(|placement| placement.pane_id != closed_pane_id)
            })
        }));

        let (mut promote, collection, member) = app_with_nonempty_collection();
        let public_before = promote.public_pane_id(0, member).unwrap();
        let terminal_before = promote.state.workspaces[0]
            .terminal_id(member)
            .expect("member terminal")
            .clone();
        assert!(promote.state.terminals.contains_key(&terminal_before));
        let response = promote.handle_collection_close(
            "promote".into(),
            CollectionCloseParams {
                collection_id: format!("collection_{}", collection.raw()),
                disposition: Some(CollectionCloseDisposition::PromoteMembers),
            },
        );
        assert!(serde_json::from_str::<crate::api::schema::SuccessResponse>(&response).is_ok());
        assert!(promote.find_collection(collection).is_none());
        assert_eq!(
            promote.public_pane_id(0, member).as_deref(),
            Some(public_before.as_str())
        );
        assert_eq!(promote.state.workspaces[0].tabs.len(), 2);
        assert_eq!(
            promote.state.workspaces[0].terminal_id(member),
            Some(&terminal_before)
        );
        assert!(promote.state.terminals.contains_key(&terminal_before));
    }

    #[test]
    fn host_document_revisions_are_monotonic_from_first_publication() {
        let mut app = app_with_workspace();
        assert_eq!(app.next_host_document_revision(), 1);
        assert_eq!(app.next_host_document_revision(), 2);
        assert_eq!(app.next_host_document_revision(), 3);
    }

    #[test]
    fn third_party_document_is_projected_and_authorizes_its_declared_action() {
        let mut app = app_with_workspace();
        let namespace = "example.collections";
        app.host_document_services =
            crate::extension_service::HostDocumentServiceCatalog::with_test_providers(&[
                (namespace, 9),
                (
                    crate::collections_extension_contract::COLLECTIONS_NAMESPACE,
                    10,
                ),
            ]);
        let snapshot = app.legacy_terminal_groups_snapshot(0, 0, 9, 1).unwrap();
        let mut document =
            crate::first_party::collections_legacy_fixture::project(&snapshot).unwrap();
        document.namespace = namespace.into();
        let action_id = document.actions[0].id.clone();
        let scope_id = snapshot.scope_id.clone();
        handle_current_result(
            &mut app,
            crate::extension_service::HostDocumentAsyncResult {
                _render_slot: None,
                namespace: namespace.into(),
                provider_priority: 200,
                snapshot,
                result: Ok(document),
                grants: std::collections::HashSet::from([
                    crate::extension_contract::HostCapability::FocusTerminal,
                ]),
            },
        );
        let builtin_snapshot = app.legacy_terminal_groups_snapshot(0, 0, 10, 1).unwrap();
        let builtin_document =
            crate::first_party::collections_legacy_fixture::project(&builtin_snapshot).unwrap();
        handle_current_result(
            &mut app,
            crate::extension_service::HostDocumentAsyncResult {
                _render_slot: None,
                namespace: crate::collections_extension_contract::COLLECTIONS_NAMESPACE.into(),
                provider_priority: 100,
                snapshot: builtin_snapshot,
                result: Ok(builtin_document),
                grants: std::collections::HashSet::from([
                    crate::extension_contract::HostCapability::FocusTerminal,
                ]),
            },
        );
        let documents = app.host_documents();
        assert_eq!(documents.len(), 2);
        assert_eq!(documents[0].provider_priority, 200);
        assert_eq!(documents[0].document.namespace, namespace);
        let action = app
            .authorize_host_document_action(&crate::api::schema::HostDocumentActionParams {
                namespace: namespace.into(),
                scope_id,
                generation: 9,
                revision: 1,
                action_id,
                confirmed: false,
                confirmation_token: None,
            })
            .unwrap();
        assert!(matches!(action, AuthorizedHostDocumentAction::Focus(_)));
    }

    #[test]
    fn native_host_document_list_filters_public_projection() {
        let mut app = app_with_workspace();
        app.host_document_services =
            crate::extension_service::HostDocumentServiceCatalog::with_test_provider(
                crate::collections_extension_contract::COLLECTIONS_NAMESPACE,
                9,
            );
        let snapshot = app.legacy_terminal_groups_snapshot(0, 0, 9, 1).unwrap();
        let scope_id = snapshot.scope_id.clone();
        let document = crate::first_party::collections_legacy_fixture::project(&snapshot).unwrap();
        handle_current_result(
            &mut app,
            crate::extension_service::HostDocumentAsyncResult {
                _render_slot: None,
                namespace: crate::collections_extension_contract::COLLECTIONS_NAMESPACE.into(),
                provider_priority: 100,
                snapshot,
                result: Ok(document),
                grants: std::collections::HashSet::from([
                    crate::extension_contract::HostCapability::FocusTerminal,
                ]),
            },
        );

        let response = app.handle_host_document_list(
            "documents".into(),
            crate::api::schema::HostDocumentListParams {
                namespace: Some(
                    crate::collections_extension_contract::COLLECTIONS_NAMESPACE.into(),
                ),
                scope_id: Some(scope_id.clone()),
            },
        );
        let response: crate::api::schema::SuccessResponse =
            serde_json::from_str(&response).unwrap();
        let crate::api::schema::ResponseResult::HostDocumentList { documents } = response.result
        else {
            panic!("expected host document list");
        };
        assert_eq!(documents.len(), 1);
        assert_eq!(
            documents[0].document.namespace,
            crate::collections_extension_contract::COLLECTIONS_NAMESPACE
        );
        assert!(app.event_hub.events_after(0).iter().any(|(_, event)| {
            matches!(
                &event.data,
                crate::api::schema::EventData::HostDocumentUpdated {
                    namespace,
                    available: true,
                    ..
                } if namespace == crate::collections_extension_contract::COLLECTIONS_NAMESPACE
            )
        }));
        let sequence = app.event_hub.current_sequence();
        app.disable_host_document_projection(
            &(
                crate::collections_extension_contract::COLLECTIONS_NAMESPACE.into(),
                scope_id,
            ),
            9,
            2,
        );
        assert!(app
            .event_hub
            .events_after(sequence)
            .iter()
            .any(|(_, event)| {
                matches!(
                    &event.data,
                    crate::api::schema::EventData::HostDocumentUpdated {
                        available: false,
                        revision: 2,
                        ..
                    }
                )
            }));
    }

    #[test]
    fn host_document_action_rejects_stale_generation_revision_confirmation_and_scope() {
        let mut app = app_with_workspace();
        let namespace = "example.interrupt";
        app.host_document_services =
            crate::extension_service::HostDocumentServiceCatalog::with_test_provider(namespace, 8);
        let snapshot = app.legacy_terminal_groups_snapshot(0, 0, 8, 2).unwrap();
        let mut document =
            crate::first_party::collections_legacy_fixture::project(&snapshot).unwrap();
        document.namespace = namespace.into();
        let terminal_id = document.items[0]
            .terminal
            .as_ref()
            .expect("ungrouped terminal")
            .terminal_id
            .clone();
        document.required_capabilities =
            vec![crate::extension_contract::HostCapability::TerminalInterrupt];
        document.actions[0].operation = crate::extension_contract::HostDocumentOperation::Host {
            effect: crate::extension_contract::HostEffect::TerminalInterrupt { terminal_id },
        };
        document.actions[0].confirmation = crate::extension_contract::HostConfirmation::Required;
        let action_id = document.actions[0].id.clone();
        let scope_id = snapshot.scope_id.clone();
        handle_current_result(
            &mut app,
            crate::extension_service::HostDocumentAsyncResult {
                _render_slot: None,
                namespace: namespace.into(),
                provider_priority: 200,
                snapshot,
                result: Ok(document),
                grants: std::collections::HashSet::from([
                    crate::extension_contract::HostCapability::TerminalInterrupt,
                ]),
            },
        );
        app.host_document_request_watermarks
            .insert((namespace.into(), scope_id.clone()), (8, 3));
        let stale_params = crate::api::schema::HostDocumentActionParams {
            namespace: namespace.into(),
            scope_id: scope_id.clone(),
            generation: 8,
            revision: 2,
            action_id: action_id.clone(),
            confirmed: false,
            confirmation_token: None,
        };
        let mut wrong_generation = stale_params.clone();
        wrong_generation.generation = 7;
        assert!(app
            .authorize_host_document_action(&wrong_generation)
            .is_err());
        let mut wrong_scope = stale_params.clone();
        wrong_scope.scope_id = "w-missing:t-missing".into();
        assert!(app.authorize_host_document_action(&wrong_scope).is_err());
        assert!(app
            .authorize_host_document_action(&stale_params)
            .unwrap_err()
            .contains("superseded"));
        app.host_document_request_watermarks
            .insert((namespace.into(), scope_id.clone()), (8, 2));
        let confirmation_token = app
            .host_documents()
            .into_iter()
            .find(|entry| entry.document.namespace == namespace)
            .and_then(|entry| entry.confirmation_tokens.get(&action_id).cloned())
            .expect("host-issued confirmation token");
        let mut params = crate::api::schema::HostDocumentActionParams {
            namespace: namespace.into(),
            scope_id,
            generation: 8,
            revision: 2,
            action_id,
            confirmed: false,
            confirmation_token: None,
        };
        assert!(app.authorize_host_document_action(&params).is_err());
        params.confirmed = true;
        assert!(app.authorize_host_document_action(&params).is_err());
        params.confirmation_token = Some(confirmation_token);
        assert!(matches!(
            app.authorize_host_document_action(&params).unwrap(),
            AuthorizedHostDocumentAction::Interrupt(_)
        ));
        let (respond_to, response_rx) = std::sync::mpsc::channel();
        assert!(app.handle_deferred_host_document_api_request(
            "replay".into(),
            params.clone(),
            respond_to,
        ));
        let response: crate::api::schema::ErrorResponse =
            serde_json::from_str(&response_rx.recv().unwrap()).unwrap();
        assert_eq!(response.error.code, "host_document_action_rejected");
        let replacement = app
            .host_documents()
            .into_iter()
            .find(|entry| entry.document.namespace == namespace)
            .expect("replacement projection");
        assert_eq!(replacement.document.revision, 2);
        let replacement_token = replacement
            .confirmation_tokens
            .values()
            .next()
            .expect("replacement confirmation token");
        assert_ne!(
            replacement_token,
            params.confirmation_token.as_ref().unwrap()
        );
    }

    #[test]
    fn provider_owned_action_is_fenced_and_correlated_to_its_catalog_generation() {
        let mut app = app_with_workspace();
        let namespace = "example.actions";
        app.host_document_services =
            crate::extension_service::HostDocumentServiceCatalog::with_test_action_sink(
                namespace, 12,
            );
        let snapshot = app.legacy_terminal_groups_snapshot(0, 0, 12, 4).unwrap();
        let mut document =
            crate::first_party::collections_legacy_fixture::project(&snapshot).unwrap();
        document.namespace = namespace.into();
        document.required_capabilities.clear();
        document.actions[0].operation =
            crate::extension_contract::HostDocumentOperation::Extension {
                name: "refresh".into(),
            };
        let params = crate::api::schema::HostDocumentActionParams {
            namespace: namespace.into(),
            scope_id: snapshot.scope_id.clone(),
            generation: 12,
            revision: 4,
            action_id: document.actions[0].id.clone(),
            confirmed: false,
            confirmation_token: None,
        };
        handle_current_result(
            &mut app,
            crate::extension_service::HostDocumentAsyncResult {
                _render_slot: None,
                namespace: namespace.into(),
                provider_priority: 200,
                snapshot,
                result: Ok(document),
                grants: std::collections::HashSet::new(),
            },
        );
        let AuthorizedHostDocumentAction::Provider(provider_action) =
            app.authorize_host_document_action(&params).unwrap()
        else {
            panic!("expected provider action")
        };
        let (snapshot, action) = *provider_action;
        assert!(app
            .host_document_services
            .request_action(snapshot, action)
            .unwrap()
            .recv()
            .unwrap()
            .is_ok());
        app.host_document_services =
            crate::extension_service::HostDocumentServiceCatalog::with_test_action_result(
                namespace,
                12,
                Err("provider rejected action".into()),
            );
        let AuthorizedHostDocumentAction::Provider(provider_action) =
            app.authorize_host_document_action(&params).unwrap()
        else {
            panic!("expected provider action")
        };
        let (snapshot, action) = *provider_action;
        assert!(app
            .host_document_services
            .request_action(snapshot, action)
            .unwrap()
            .recv()
            .unwrap()
            .unwrap_err()
            .contains("provider rejected action"));
        let mut stale = params;
        stale.generation = 11;
        assert!(app.authorize_host_document_action(&stale).is_err());
    }

    #[test]
    fn stalled_provider_action_does_not_block_unrelated_host_api_work() {
        let mut app = app_with_workspace();
        let namespace = "example.slow-actions";
        app.host_document_services =
            crate::extension_service::HostDocumentServiceCatalog::with_test_delayed_action(
                namespace,
                12,
                app.event_tx.clone(),
                std::time::Duration::from_millis(200),
            );
        let snapshot = app.legacy_terminal_groups_snapshot(0, 0, 12, 4).unwrap();
        app.host_document_services.set_desired_revision_for_test(
            namespace,
            snapshot.scope_id.clone(),
            snapshot.generation,
            snapshot.revision,
        );
        let mut document =
            crate::first_party::collections_legacy_fixture::project(&snapshot).unwrap();
        document.namespace = namespace.into();
        document.required_capabilities.clear();
        document.actions[0].operation =
            crate::extension_contract::HostDocumentOperation::Extension {
                name: "slow".into(),
            };
        let params = crate::api::schema::HostDocumentActionParams {
            namespace: namespace.into(),
            scope_id: snapshot.scope_id.clone(),
            generation: 12,
            revision: 4,
            action_id: document.actions[0].id.clone(),
            confirmed: false,
            confirmation_token: None,
        };
        handle_current_result(
            &mut app,
            crate::extension_service::HostDocumentAsyncResult {
                _render_slot: None,
                namespace: namespace.into(),
                provider_priority: 200,
                snapshot,
                result: Ok(document),
                grants: std::collections::HashSet::new(),
            },
        );
        let (respond_to, response_rx) = std::sync::mpsc::channel();
        let started = std::time::Instant::now();
        assert!(!app.handle_deferred_host_document_api_request(
            "slow-action".into(),
            params,
            respond_to,
        ));
        let pane_id = app
            .public_pane_id(0, app.state.workspaces[0].focused_pane_id().unwrap())
            .unwrap();
        let response =
            app.handle_pane_focus("focus".into(), crate::api::schema::PaneTarget { pane_id });
        let _: crate::api::schema::SuccessResponse = serde_json::from_str(&response).unwrap();
        assert!(started.elapsed() < std::time::Duration::from_millis(100));
        loop {
            let completion = app.event_rx.blocking_recv().unwrap();
            let action_finished = matches!(
                &completion,
                crate::events::AppEvent::HostDocumentProviderActionFinished { .. }
            );
            app.handle_internal_event(completion);
            if action_finished {
                break;
            }
        }
        let response = response_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        let _: crate::api::schema::SuccessResponse = serde_json::from_str(&response).unwrap();
    }

    #[test]
    fn documents_for_removed_targets_are_retired_from_projection() {
        let mut app = app_with_workspace();
        let namespace = crate::collections_extension_contract::COLLECTIONS_NAMESPACE;
        app.host_document_services =
            crate::extension_service::HostDocumentServiceCatalog::with_test_provider(namespace, 4);
        let snapshot = app.legacy_terminal_groups_snapshot(0, 0, 4, 1).unwrap();
        let document = crate::first_party::collections_legacy_fixture::project(&snapshot).unwrap();
        let late_snapshot = snapshot.clone();
        let late_document = document.clone();
        handle_current_result(
            &mut app,
            crate::extension_service::HostDocumentAsyncResult {
                _render_slot: None,
                namespace: namespace.into(),
                provider_priority: 200,
                snapshot,
                result: Ok(document),
                grants: std::collections::HashSet::from([
                    crate::extension_contract::HostCapability::FocusTerminal,
                ]),
            },
        );
        assert_eq!(app.host_documents().len(), 1);
        app.state.workspaces[0].tabs.clear();
        app.refresh_collection_documents();
        assert!(app.host_document_request_watermarks.is_empty());
        assert!(app.host_documents().is_empty());
        app.handle_host_document_result(crate::extension_service::HostDocumentAsyncResult {
            _render_slot: None,
            namespace: namespace.into(),
            provider_priority: 200,
            snapshot: late_snapshot,
            result: Ok(late_document),
            grants: std::collections::HashSet::from([
                crate::extension_contract::HostCapability::FocusTerminal,
            ]),
        });
        assert!(app.host_document_caches.is_empty());
    }

    #[test]
    fn saturated_new_scope_is_retried_when_a_global_render_slot_releases() {
        let mut app = app_with_workspace();
        let namespace = crate::collections_extension_contract::COLLECTIONS_NAMESPACE;
        app.host_document_services = crate::extension_service::HostDocumentServiceCatalog::
            with_test_saturated_render_provider(namespace, 7);
        app.refresh_collection_documents();
        assert_eq!(app.host_document_retry_snapshots.len(), 1);
        assert_eq!(
            app.host_document_services
                .pending_render_count_for_test(namespace),
            0
        );
        app.host_document_services.release_test_render_slot();
        app.retry_one_host_document_render();
        assert!(app.host_document_retry_snapshots.is_empty());
        assert_eq!(
            app.host_document_services
                .pending_render_count_for_test(namespace),
            1
        );
    }

    #[test]
    fn linked_worktree_spaces_label_preserves_branch_and_custom_name_parity() {
        let mut app = app_with_workspace();
        let workspace = &mut app.state.workspaces[0];
        workspace.custom_name = None;
        workspace.checkout = Some(crate::repository::CheckoutProvenance {
            repository_id: "repo_test".into(),
            checkout_path: std::path::PathBuf::from("/tmp/repo-worktree"),
            kind: crate::repository::CheckoutKind::Linked,
        });
        workspace.cached_git_branch = Some("worktree/feature/spaces".into());
        assert_eq!(
            app.spaces_workspace_label(&app.state.workspaces[0]),
            "feature/spaces"
        );
        app.state.workspaces[0].custom_name = Some("My checkout".into());
        assert_eq!(
            app.spaces_workspace_label(&app.state.workspaces[0]),
            "My checkout"
        );
    }

    #[test]
    fn git_identity_reconciliation_invalidates_spaces_projection() {
        let mut app = app_with_workspace();
        app.host_spaces_last_desired = Some(crate::extension_contract::HostSpacesV1 {
            contract: "host.spaces.v1".into(),
            generation: 1,
            revision: 1,
            scope_id: crate::spaces_extension_contract::SPACES_SCOPE_ID.into(),
            repositories: Vec::new(),
            standalone: Vec::new(),
            order: Vec::new(),
        });
        let workspace_id = app.state.workspaces[0].id.clone();
        let identity_cwd = app.state.workspaces[0].identity_cwd.clone();
        let status_cache_key = app.state.workspaces[0].cached_git_status_key.clone();
        let render_changed = app.handle_internal_event_with_render_impact(
            crate::events::AppEvent::GitStatusRefreshed {
                results: vec![crate::workspace::WorkspaceGitStatus {
                    workspace_id,
                    resolved_identity_cwd: identity_cwd,
                    status_cache_key,
                    demand: crate::workspace::GitStatusRefreshDemand::ALL,
                    auto_label: "updated-repository".into(),
                    branch: Some("feature/spaces".into()),
                    ahead_behind: None,
                    space: None,
                }],
                cache_updates: Vec::new(),
            },
        );
        assert!(render_changed);
        assert!(app.host_spaces_last_desired.is_none());
    }

    #[test]
    fn relationship_snapshot_retains_tombstoned_ancestry() {
        let mut app = app_with_workspace();
        let pane = app.state.workspaces[0].focused_pane_id().unwrap();
        let root = app
            .state
            .delegations
            .create(Some(pane), None, Some("root".into()))
            .unwrap();
        let child = app
            .state
            .delegations
            .create(None, Some(root), Some("child".into()))
            .unwrap();
        app.state.delegations.tombstone_pane(pane);
        let namespace = crate::first_party::delegation::NAMESPACE;
        app.host_document_services =
            crate::extension_service::HostDocumentServiceCatalog::with_test_document_source_provider(
                namespace,
                12,
                crate::api::schema::HostDocumentSourceKind::Relationships,
            );
        app.refresh_relationships_document();
        let snapshot = app.host_relationships_last_desired.get(namespace).unwrap();
        let root_record = snapshot
            .relationships
            .iter()
            .find(|record| record.relationship_id == root.to_string())
            .unwrap();
        let child_record = snapshot
            .relationships
            .iter()
            .find(|record| record.relationship_id == child.to_string())
            .unwrap();
        assert_eq!(
            root_record.lifecycle,
            crate::extension_contract::HostRelationshipLifecycle::Retained
        );
        assert!(root_record.terminal.is_none());
        let root_id = root.to_string();
        assert_eq!(child_record.parent_id.as_deref(), Some(root_id.as_str()));
    }

    #[test]
    fn agent_activity_snapshot_is_attributed_and_never_projects_shared_done() {
        let mut app = app_with_workspace();
        let pane = app.state.workspaces[0].focused_pane_id().unwrap();
        let terminal_id = app.state.workspaces[0]
            .pane_state(pane)
            .unwrap()
            .attached_terminal_id
            .clone();
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal.set_detected_state(
            Some(crate::detect::Agent::Claude),
            crate::detect::AgentState::Idle,
        );
        terminal.last_agent_state_change_seq = Some(17);
        terminal.set_agent_metadata(crate::terminal::AgentMetadataReport {
            source: "user:presentation".into(),
            agent_label: Some("claude".into()),
            applies_to_source: None,
            title: None,
            display_agent: Some("Friendly Claude".into()),
            state_labels: std::collections::HashMap::from([(
                "idle".into(),
                "role requested".into(),
            )]),
            clear_title: false,
            clear_display_agent: false,
            clear_state_labels: false,
            ttl: Some(std::time::Duration::from_secs(1)),
            seq: None,
        });
        let namespace = crate::first_party::agent_status::NAMESPACE;
        app.host_document_services =
            crate::extension_service::HostDocumentServiceCatalog::with_test_document_source_provider(
                namespace,
                13,
                crate::api::schema::HostDocumentSourceKind::AgentActivity,
            );
        app.refresh_agent_activity_document();
        let snapshot = app.host_agent_activity_last_desired.get(namespace).unwrap();
        assert_eq!(snapshot.activities.len(), 1);
        assert_eq!(
            snapshot.activities[0].activity,
            crate::extension_contract::HostAgentActivityState::Idle
        );
        assert_eq!(snapshot.activities[0].source, "host_detection");
        assert_eq!(
            snapshot.activities[0]
                .labels
                .get("display")
                .map(String::as_str),
            Some("Friendly Claude")
        );
        assert_eq!(snapshot.activities[0].state_change_seq, 17);
        assert_eq!(
            snapshot.activities[0]
                .labels
                .get("state_idle")
                .map(String::as_str),
            Some("role requested")
        );
        assert!(!serde_json::to_string(snapshot).unwrap().contains("done"));
        let previous_revision = snapshot.revision;

        app.sync_agent_metadata_deadline();
        let deadline = app.agent_metadata_deadline.unwrap();
        app.expire_metadata_at(deadline, deadline + std::time::Duration::from_millis(1));
        let refreshed = app.host_agent_activity_last_desired.get(namespace).unwrap();
        assert!(refreshed.revision > previous_revision);
        assert_ne!(
            refreshed.activities[0]
                .labels
                .get("display")
                .map(String::as_str),
            Some("Friendly Claude")
        );
        assert!(!refreshed.activities[0].labels.contains_key("state_idle"));
    }

    #[test]
    fn public_semantic_sources_refresh_each_namespace_without_bundled_providers() {
        let mut app = app_with_workspace();
        app.host_document_services = crate::extension_service::HostDocumentServiceCatalog::with_test_saturated_render_providers(&[
            ("example.activity-a", 7, crate::api::schema::HostDocumentSourceKind::AgentActivity),
            ("example.activity-b", 8, crate::api::schema::HostDocumentSourceKind::AgentActivity),
            ("example.relationships", 9, crate::api::schema::HostDocumentSourceKind::Relationships),
        ]);
        app.refresh_agent_activity_document();
        app.refresh_relationships_document();
        assert_eq!(app.host_agent_activity_last_desired.len(), 2);
        assert_eq!(app.host_relationships_last_desired.len(), 1);
        let a = app.host_agent_activity_last_desired["example.activity-a"].clone();
        let b = app.host_agent_activity_last_desired["example.activity-b"].clone();
        assert_eq!(a.generation, 7);
        assert_eq!(b.generation, 8);
        assert_eq!(a.activities, b.activities);
        app.refresh_agent_activity_document();
        assert_eq!(
            app.host_agent_activity_last_desired["example.activity-a"],
            a
        );
        assert_eq!(
            app.host_agent_activity_last_desired["example.activity-b"],
            b
        );
        assert!(app
            .host_document_retry_snapshots
            .keys()
            .all(|(namespace, _)| namespace.starts_with("example.")));
        // Retire the old test catalog before reserving a replacement's global slots.
        app.host_document_services =
            crate::extension_service::HostDocumentServiceCatalog::with_test_providers(&[]);
        app.host_document_services = crate::extension_service::HostDocumentServiceCatalog::with_test_document_source_provider(
            "example.activity-b", 18, crate::api::schema::HostDocumentSourceKind::AgentActivity,
        );
        app.refresh_agent_activity_document();
        app.refresh_relationships_document();
        assert_eq!(app.host_agent_activity_last_desired.len(), 1);
        assert_eq!(
            app.host_agent_activity_last_desired["example.activity-b"].generation,
            18
        );
        assert!(app.host_relationships_last_desired.is_empty());
    }

    #[test]
    fn external_activity_completion_cannot_revive_a_retired_generation() {
        let mut app = app_with_workspace();
        let namespace = "example.external-activity";
        app.host_document_services = crate::extension_service::HostDocumentServiceCatalog::with_test_document_source_provider(
            namespace, 31, crate::api::schema::HostDocumentSourceKind::AgentActivity,
        );
        app.refresh_agent_activity_document();
        let old = app.host_agent_activity_last_desired[namespace].clone();
        let mut document = crate::first_party::agent_status::project(&old).unwrap();
        document.namespace = namespace.into();
        app.host_document_services =
            crate::extension_service::HostDocumentServiceCatalog::with_test_providers(&[]);
        app.host_document_services = crate::extension_service::HostDocumentServiceCatalog::with_test_document_source_provider(
            namespace, 32, crate::api::schema::HostDocumentSourceKind::AgentActivity,
        );
        app.refresh_agent_activity_document();
        let current = app.host_agent_activity_last_desired[namespace].clone();
        app.handle_host_agent_activity_document_result(
            crate::extension_service::HostAgentActivityDocumentAsyncResult {
                _render_slot: None,
                namespace: namespace.into(),
                provider_priority: 100,
                snapshot: old,
                result: Ok(document),
                grants: std::collections::HashSet::new(),
            },
        );
        assert!(app
            .host_document_caches
            .get(&(namespace.into(), current.scope_id.clone()))
            .and_then(|cache| cache.current())
            .is_none());
        assert_eq!(app.host_agent_activity_last_desired[namespace], current);
        let mut document = crate::first_party::agent_status::project(&current).unwrap();
        document.namespace = namespace.into();
        app.handle_host_agent_activity_document_result(
            crate::extension_service::HostAgentActivityDocumentAsyncResult {
                _render_slot: None,
                namespace: namespace.into(),
                provider_priority: 100,
                snapshot: current.clone(),
                result: Ok(document),
                grants: std::collections::HashSet::new(),
            },
        );
        assert_eq!(
            app.host_document_caches[&(namespace.into(), current.scope_id)]
                .current()
                .unwrap()
                .generation,
            32
        );
    }

    #[test]
    fn semantic_cache_publication_rejection_schedules_bounded_retry() {
        use crate::api::schema::HostDocumentSourceKind;
        for source in [
            HostDocumentSourceKind::AgentActivity,
            HostDocumentSourceKind::Relationships,
        ] {
            for invalid_namespace in [true, false] {
                let mut app = app_with_workspace();
                let namespace = "example.rejected";
                app.host_document_services = crate::extension_service::HostDocumentServiceCatalog::with_test_document_source_provider(namespace, 21, source);
                if source == HostDocumentSourceKind::AgentActivity {
                    app.refresh_agent_activity_document();
                    let snapshot = app.host_agent_activity_last_desired[namespace].clone();
                    let mut document =
                        crate::first_party::agent_status::project(&snapshot).unwrap();
                    document.namespace = if invalid_namespace {
                        "other.provider"
                    } else {
                        namespace
                    }
                    .into();
                    if !invalid_namespace {
                        document
                            .required_capabilities
                            .push(crate::extension_contract::HostCapability::FocusTerminal);
                    }
                    app.handle_host_agent_activity_document_result(
                        crate::extension_service::HostAgentActivityDocumentAsyncResult {
                            _render_slot: None,
                            namespace: namespace.into(),
                            provider_priority: 0,
                            snapshot,
                            result: Ok(document),
                            grants: std::collections::HashSet::new(),
                        },
                    );
                    assert!(!app.host_agent_activity_last_desired.contains_key(namespace));
                } else {
                    app.refresh_relationships_document();
                    let snapshot = app.host_relationships_last_desired[namespace].clone();
                    let mut document = crate::first_party::delegation::project(&snapshot).unwrap();
                    document.namespace = if invalid_namespace {
                        "other.provider"
                    } else {
                        namespace
                    }
                    .into();
                    if !invalid_namespace {
                        document
                            .required_capabilities
                            .push(crate::extension_contract::HostCapability::FocusTerminal);
                    }
                    app.handle_host_relationships_document_result(
                        crate::extension_service::HostRelationshipsDocumentAsyncResult {
                            _render_slot: None,
                            namespace: namespace.into(),
                            provider_priority: 0,
                            snapshot,
                            result: Ok(document),
                            grants: std::collections::HashSet::new(),
                        },
                    );
                    assert!(!app.host_relationships_last_desired.contains_key(namespace));
                }
                assert!(app
                    .host_document_caches
                    .values()
                    .all(|cache| cache.current().is_none()));
                assert!(app.host_semantic_retry_scheduled.contains(namespace));
                assert_eq!(app.host_semantic_retry_attempts[namespace], 1);
                app.schedule_host_semantic_retry(namespace);
                assert_eq!(app.host_semantic_retry_attempts[namespace], 1);
            }
        }
    }

    #[test]
    fn hook_state_reports_refresh_agent_activity_documents() {
        let namespace = crate::first_party::agent_status::NAMESPACE;
        let mut app = app_with_workspace();
        let pane_id = app.state.workspaces[0].focused_pane_id().unwrap();
        app.host_document_services =
            crate::extension_service::HostDocumentServiceCatalog::with_test_document_source_provider(
                crate::first_party::agent_status::NAMESPACE,
                13,
                crate::api::schema::HostDocumentSourceKind::AgentActivity,
            );
        app.refresh_agent_activity_document();
        let previous_revision = app
            .host_agent_activity_last_desired
            .get(namespace)
            .unwrap()
            .revision;

        app.handle_internal_event(crate::events::AppEvent::HookStateReported {
            pane_id,
            session_ref: None,
            source: "smoke".into(),
            agent_label: "codex".into(),
            state: crate::detect::AgentState::Working,
            message: None,
            seq: Some(1),
        });

        let snapshot = app.host_agent_activity_last_desired.get(namespace).unwrap();
        assert!(snapshot.revision > previous_revision);
        assert_eq!(snapshot.activities.len(), 1);
        assert_eq!(
            snapshot.activities[0].activity,
            crate::extension_contract::HostAgentActivityState::Working
        );
    }

    #[test]
    fn oversized_relationship_projection_retires_to_native_fallback() {
        let mut app = app_with_workspace();
        let namespace = crate::first_party::delegation::NAMESPACE;
        app.host_document_services =
            crate::extension_service::HostDocumentServiceCatalog::with_test_document_source_provider(
                namespace,
                13,
                crate::api::schema::HostDocumentSourceKind::Relationships,
            );
        for _ in 0..=crate::extension_contract::MAX_DOCUMENT_ITEMS {
            app.state.delegations.create(None, None, None).unwrap();
        }
        app.refresh_relationships_document();
        assert!(app.host_relationships_last_desired.is_empty());
        assert!(!app.host_document_caches.contains_key(&(
            namespace.into(),
            crate::spaces_extension_contract::SPACES_SCOPE_ID.into()
        )));
    }

    #[test]
    fn endpoint_document_projection_drops_over_budget_entries_selectively() {
        let mut app = app_with_workspace();
        for provider in 0..6 {
            let namespace = format!("example.large-{provider}");
            let document = crate::extension_contract::HostDocumentV1 {
                contract: crate::extension_contract::HOST_DOCUMENT_CONTRACT_V1.into(),
                namespace: namespace.clone(),
                generation: 1,
                revision: 1,
                surface: crate::extension_contract::HostSurface::Navigation,
                target: crate::extension_contract::HostTarget::Session,
                items: (0..48)
                    .map(|index| crate::extension_contract::HostDocumentItem {
                        id: format!("item-{index}"),
                        parent_id: None,
                        role: crate::extension_contract::HostItemRole::Section,
                        label: "x".repeat(crate::extension_contract::MAX_DOCUMENT_TEXT_CHARS),
                        detail: None,
                        terminal: None,
                        action_ids: Vec::new(),
                        flags: Default::default(),
                    })
                    .collect(),
                actions: Vec::new(),
                required_capabilities: Vec::new(),
            };
            let mut cache = crate::host_document_cache::HostDocumentCache::default();
            cache
                .publish_for_namespace(
                    &namespace,
                    1,
                    100,
                    &std::collections::HashSet::new(),
                    document,
                )
                .unwrap();
            app.host_document_caches.insert(
                (
                    namespace,
                    crate::spaces_extension_contract::SPACES_SCOPE_ID.into(),
                ),
                cache,
            );
        }

        let entries = app.host_documents();
        assert!(!entries.is_empty());
        assert!(entries.len() < 6);
        let encoded = serde_json::to_vec(&entries).unwrap();
        assert!(encoded.len() < MAX_HOST_DOCUMENTS_PROJECTION_BYTES);
    }

    #[test]
    fn semantic_metadata_is_bounded_before_provider_validation() {
        let oversized = format!(
            "{}\u{0000}",
            "x".repeat(crate::extension_contract::MAX_DOCUMENT_TEXT_CHARS + 10)
        );
        let bounded = bounded_document_text(&oversized);
        assert_eq!(
            bounded.chars().count(),
            crate::extension_contract::MAX_DOCUMENT_TEXT_CHARS
        );
        assert!(!bounded.chars().any(char::is_control));
    }

    #[cfg(unix)]
    fn next_document_event(
        rx: &mut tokio::sync::mpsc::Receiver<crate::events::AppEvent>,
    ) -> crate::events::AppEvent {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if let Ok(event) = rx.try_recv() {
                return event;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "document event deadline"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    #[cfg(unix)]
    fn apply_document_event(app: &mut App, event: crate::events::AppEvent) {
        match event {
            crate::events::AppEvent::HostDocumentFinished(result) => {
                app.handle_host_document_result(*result)
            }
            crate::events::AppEvent::HostDocumentProviderRetired {
                namespace,
                generation,
            } => {
                app.retire_host_document_provider(&namespace, generation);
            }
            _ => panic!("unexpected provider event"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn executable_provider_exit_retires_context_without_source_change() {
        for during_request in [false, true] {
            let mut app = app_with_workspace();
            let namespace = "example.exit-context";
            let other = "example.still-live";
            let root = std::env::temp_dir().join(format!(
                "vqro-document-exit-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(root.join("dead")).unwrap();
            std::fs::create_dir_all(root.join("live")).unwrap();
            let specs = [(namespace, "dead", 7), (other, "live", 8)]
                .into_iter()
                .map(|(name, control, generation)| {
                    (
                        crate::extension_service::ExtensionServiceSpec {
                            id: name.into(),
                            command: vec![
                                "python3".into(),
                                format!(
                                    "{}/tests/fixtures/document-exit-provider.py",
                                    env!("CARGO_MANIFEST_DIR")
                                ),
                                name.into(),
                                root.join(control).display().to_string(),
                            ],
                            cwd: root.clone(),
                            priority: 100,
                            granted_capabilities: vec![
                                crate::api::schema::ExtensionServiceCapability::HostDocument,
                            ],
                        },
                        generation,
                        crate::api::schema::HostDocumentSourceKind::TerminalGroups,
                    )
                })
                .collect();
            let (tx, mut rx) = tokio::sync::mpsc::channel(16);
            app.host_document_services =
                crate::extension_service::HostDocumentServiceCatalog::for_test_document_specs(
                    specs, tx,
                );
            app.refresh_collection_document(0, 0);
            for _ in 0..2 {
                apply_document_event(&mut app, next_document_event(&mut rx));
            }
            assert_eq!(app.host_documents().len(), 2);
            let key = (namespace.to_string(), app.public_tab_id(0, 0).unwrap());
            let published = app.host_document_caches[&key].current().unwrap().clone();
            let source = app
                .legacy_terminal_groups_snapshot(0, 0, 7, published.revision)
                .unwrap();
            if during_request {
                std::fs::write(root.join("dead/die-on-render"), "").unwrap();
                let mut snapshot = source.clone();
                snapshot.revision = app.next_host_document_revision();
                app.host_document_request_watermarks
                    .insert(key.clone(), (7, snapshot.revision));
                app.host_document_services
                    .request(namespace, snapshot)
                    .unwrap();
            } else {
                std::fs::write(root.join("dead/exit"), "").unwrap();
            }
            loop {
                let event = next_document_event(&mut rx);
                let retired = matches!(&event, crate::events::AppEvent::HostDocumentProviderRetired { namespace: name, generation: 7 } if name == namespace);
                apply_document_event(&mut app, event);
                if retired {
                    break;
                }
            }
            let response = app.handle_host_document_list(
                "after-exit".into(),
                crate::api::schema::HostDocumentListParams {
                    namespace: None,
                    scope_id: None,
                },
            );
            let response: crate::api::schema::SuccessResponse =
                serde_json::from_str(&response).unwrap();
            let crate::api::schema::ResponseResult::HostDocumentList { documents } =
                response.result
            else {
                panic!("document list");
            };
            assert_eq!(documents.len(), 1);
            assert_eq!(documents[0].document.namespace, other);
            assert_eq!(
                serde_json::to_value(&source).unwrap(),
                serde_json::to_value(
                    app.legacy_terminal_groups_snapshot(0, 0, 7, published.revision)
                        .unwrap()
                )
                .unwrap()
            );
            // A pre-exit success delivered after retirement cannot revive cached authority.
            app.handle_host_document_result(crate::extension_service::HostDocumentAsyncResult {
                _render_slot: None,
                namespace: namespace.into(),
                provider_priority: 100,
                snapshot: source,
                result: Ok(published),
                grants: std::collections::HashSet::new(),
            });
            assert!(!app
                .host_documents()
                .iter()
                .any(|entry| entry.document.namespace == namespace));
            assert!(app.event_hub.events_after(0).iter().any(|(_, event)| matches!(&event.data,
                crate::api::schema::EventData::HostDocumentUpdated { namespace: name, available: false, .. } if name == namespace)));
            for marker in ["dead/exit", "dead/die-on-render"] {
                let _ = std::fs::remove_file(root.join(marker));
            }
            // Drive the first queued retry while the same supervisor is still in Backoff.
            // Its failed completion must retain a bounded retry, not strand a quiet source.
            app.retry_one_host_document_render();
            let recovery_deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while !app
                .host_documents()
                .iter()
                .any(|entry| entry.document.namespace == namespace)
            {
                while let Ok(event) = rx.try_recv() {
                    apply_document_event(&mut app, event);
                }
                while let Ok(event) = app.event_rx.try_recv() {
                    app.handle_internal_event(event);
                }
                assert!(
                    std::time::Instant::now() < recovery_deadline,
                    "provider did not recover"
                );
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            assert_eq!(app.host_documents().len(), 2);
            drop(app);
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn host_document_retirement_fences_reenable_and_respawn_race() {
        let mut app = app_with_workspace();
        let namespace = "example.activity";
        app.host_document_services = crate::extension_service::HostDocumentServiceCatalog::with_test_document_source_provider(namespace, 8, crate::api::schema::HostDocumentSourceKind::AgentActivity);
        let key = (
            namespace.to_string(),
            crate::spaces_extension_contract::SPACES_SCOPE_ID.to_string(),
        );
        app.host_document_request_watermarks
            .insert(key.clone(), (8, 10));
        app.retire_host_document_provider(namespace, 7);
        assert_eq!(
            app.host_document_request_watermarks.get(&key),
            Some(&(8, 10))
        );
        // A healthy respawn result racing ahead of the exit event is conservatively retired too.
        app.retire_host_document_provider(namespace, 8);
        assert!(!app.host_document_request_watermarks.contains_key(&key));
        assert!(app.host_semantic_retry_scheduled.contains(namespace));
        assert_eq!(app.host_semantic_retry_attempts.get(namespace), Some(&1));
    }

    #[test]
    fn retired_provider_action_completion_is_final_fenced_on_actor() {
        let mut app = app_with_workspace();
        let namespace = crate::collections_extension_contract::COLLECTIONS_NAMESPACE;
        app.host_document_services =
            crate::extension_service::HostDocumentServiceCatalog::with_test_provider(namespace, 7);
        let snapshot = app.legacy_terminal_groups_snapshot(0, 0, 7, 3).unwrap();
        app.host_document_request_watermarks.insert(
            (namespace.into(), snapshot.scope_id.clone()),
            (snapshot.generation, snapshot.revision),
        );
        let (respond_to, response) = std::sync::mpsc::channel();
        app.host_document_request_watermarks.clear();
        app.handle_internal_event(
            crate::events::AppEvent::HostDocumentProviderActionFinished {
                request_id: "retired-action".into(),
                namespace: namespace.into(),
                scope_id: snapshot.scope_id,
                generation: snapshot.generation,
                revision: snapshot.revision,
                holds_action_slot: false,
                result: Ok(()),
                respond_to,
            },
        );
        let rejected: crate::api::schema::ErrorResponse =
            serde_json::from_str(&response.recv().unwrap()).unwrap();
        assert_eq!(rejected.error.code, "host_document_action_rejected");
        assert!(rejected.error.message.contains("retired"));
    }

    #[test]
    fn rejected_newest_request_watermark_blocks_older_completion() {
        let mut app = app_with_workspace();
        let namespace = crate::collections_extension_contract::COLLECTIONS_NAMESPACE;
        app.host_document_services =
            crate::extension_service::HostDocumentServiceCatalog::with_test_provider(namespace, 7);
        let newest = app.legacy_terminal_groups_snapshot(0, 0, 7, 2).unwrap();
        app.host_document_request_watermarks.insert(
            (namespace.into(), newest.scope_id.clone()),
            (newest.generation, newest.revision),
        );
        let older = app.legacy_terminal_groups_snapshot(0, 0, 7, 1).unwrap();
        let older_document =
            crate::first_party::collections_legacy_fixture::project(&older).unwrap();
        app.handle_host_document_result(crate::extension_service::HostDocumentAsyncResult {
            _render_slot: None,
            namespace: namespace.into(),
            provider_priority: 100,
            snapshot: older,
            result: Ok(older_document),
            grants: std::collections::HashSet::from([
                crate::extension_contract::HostCapability::FocusTerminal,
            ]),
        });
        let key = (namespace.to_string(), newest.scope_id);
        assert_eq!(
            app.host_document_request_watermarks.get(&key),
            Some(&(7, 2))
        );
        assert!(app.host_documents().is_empty());
    }

    #[test]
    fn stale_async_document_completion_cannot_replace_newer_publication() {
        let mut app = app_with_workspace();
        let namespace = crate::collections_extension_contract::COLLECTIONS_NAMESPACE;
        app.host_document_services =
            crate::extension_service::HostDocumentServiceCatalog::with_test_provider(namespace, 7);
        let newer_snapshot = app.legacy_terminal_groups_snapshot(0, 0, 7, 2).unwrap();
        let newer_document =
            crate::first_party::collections_legacy_fixture::project(&newer_snapshot).unwrap();
        handle_current_result(
            &mut app,
            crate::extension_service::HostDocumentAsyncResult {
                _render_slot: None,
                namespace: namespace.into(),
                provider_priority: 200,
                snapshot: newer_snapshot,
                result: Ok(newer_document),
                grants: std::collections::HashSet::from([
                    crate::extension_contract::HostCapability::FocusTerminal,
                ]),
            },
        );
        let stale_snapshot = app.legacy_terminal_groups_snapshot(0, 0, 7, 1).unwrap();
        app.handle_host_document_result(crate::extension_service::HostDocumentAsyncResult {
            _render_slot: None,
            namespace: namespace.into(),
            provider_priority: 200,
            snapshot: stale_snapshot,
            result: Err("late failure".into()),
            grants: std::collections::HashSet::new(),
        });
        let documents = app.host_documents();
        assert_eq!(documents.len(), 1);
        assert_eq!(documents[0].document.revision, 2);
    }
}
