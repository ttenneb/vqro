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
const MAX_HOST_SPACES_RETRY_ATTEMPTS: u32 = 6;
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
    Spaces(crate::extension_contract::HostSpacesV1),
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
        let mut live_tab_ids = std::collections::HashSet::from([
            crate::first_party::spaces::SESSION_SCOPE_ID.to_string(),
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
            .collections_shadow_documents
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
        self.collections_shadow_documents
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

    fn refresh_collection_document(&mut self, ws_idx: usize, tab_idx: usize) {
        let revision = self.next_host_document_revision();
        let mut namespaces = self
            .host_document_services
            .namespaces()
            .filter(|namespace| {
                self.host_document_services.source(namespace)
                    == Some(crate::api::schema::HostDocumentSourceKind::TerminalGroups)
            })
            .map(str::to_owned)
            .collect::<Vec<_>>();
        if !namespaces
            .iter()
            .any(|namespace| namespace == crate::first_party::collections::NAMESPACE)
        {
            namespaces.push(crate::first_party::collections::NAMESPACE.into());
        }
        for namespace in namespaces {
            let generation = self
                .host_document_services
                .generation(&namespace)
                .unwrap_or(1);
            let Ok(snapshot) =
                self.collections_shadow_snapshot(ws_idx, tab_idx, generation, revision)
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
                .collections_shadow_documents
                .entry(request_key.clone())
                .or_default();
            cache.disable();
            if namespace == crate::first_party::collections::NAMESPACE {
                let _ = cache.install_fallback(&snapshot);
            }
            if let Err(error) = self
                .host_document_services
                .request(&namespace, snapshot.clone())
            {
                if error.contains("global document render queue is full") {
                    self.defer_host_document_render(request_key.clone(), snapshot.clone());
                }
                if namespace == crate::first_party::collections::NAMESPACE {
                    tracing::warn!(%error, "Collections provider unavailable; using generic fallback");
                } else {
                    tracing::warn!(%namespace, %error, "host document provider unavailable");
                }
            } else {
                self.host_document_retry_snapshots.remove(&request_key);
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
                DeferredHostDocumentSnapshot::Spaces(snapshot) => self
                    .host_document_services
                    .request_spaces(&key.0, snapshot.clone()),
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
                Err(_) if key.0 == crate::first_party::collections::NAMESPACE => {
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
        let namespace = crate::first_party::spaces::NAMESPACE;
        let scope_id = crate::first_party::spaces::SESSION_SCOPE_ID.to_string();
        let Some(generation) = self.host_document_services.generation(namespace) else {
            self.collections_shadow_documents
                .remove(&(namespace.into(), scope_id.clone()));
            self.host_document_request_watermarks
                .remove(&(namespace.into(), scope_id));
            self.host_spaces_last_desired = None;
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
        if snapshot.validate().is_err() {
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
            .insert(key.clone(), (generation, revision));
        self.disable_host_document_projection(&key, generation, revision);
        match self
            .host_document_services
            .request_spaces(namespace, snapshot.clone())
        {
            Ok(()) => {
                self.host_document_retry_snapshots.remove(&key);
            }
            Err(error) if error.contains("global document render queue is full") => {
                if !self.host_document_retry_snapshots.contains_key(&key) {
                    self.host_document_retry_order.push_back(key.clone());
                }
                self.host_document_retry_snapshots
                    .insert(key, DeferredHostDocumentSnapshot::Spaces(snapshot));
            }
            Err(error) => {
                self.host_spaces_last_desired = None;
                self.schedule_host_spaces_retry();
                tracing::warn!(%error, "Spaces provider unavailable; using native fallback")
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
        let namespace = crate::first_party::delegation::NAMESPACE;
        let scope_id = crate::first_party::spaces::SESSION_SCOPE_ID.to_string();
        let Some(generation) = self.host_document_services.generation(namespace) else {
            self.collections_shadow_documents
                .remove(&(namespace.into(), scope_id.clone()));
            self.host_document_request_watermarks
                .remove(&(namespace.into(), scope_id));
            self.host_relationships_last_desired = None;
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
                    kind: namespace.into(),
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
            self.collections_shadow_documents.remove(&key);
            self.host_document_request_watermarks.remove(&key);
            self.host_document_retry_snapshots.remove(&key);
            self.host_relationships_last_desired = None;
            return;
        }
        if self
            .host_relationships_last_desired
            .as_ref()
            .is_some_and(|previous| {
                let mut normalized = previous.clone();
                normalized.revision = 1;
                normalized == snapshot
            })
        {
            return;
        }
        snapshot.revision = self.next_host_relationships_document_revision();
        self.host_relationships_last_desired = Some(snapshot.clone());
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
                self.host_relationships_last_desired = None;
                self.schedule_host_semantic_retry(namespace);
                tracing::warn!(%error, "Delegation provider unavailable; using relationship fallback");
            }
        }
    }

    pub(crate) fn refresh_agent_activity_document(&mut self) {
        let namespace = crate::first_party::agent_status::NAMESPACE;
        let scope_id = crate::first_party::spaces::SESSION_SCOPE_ID.to_string();
        let Some(generation) = self.host_document_services.generation(namespace) else {
            self.collections_shadow_documents
                .remove(&(namespace.into(), scope_id.clone()));
            self.host_document_request_watermarks
                .remove(&(namespace.into(), scope_id));
            self.host_agent_activity_last_desired = None;
            return;
        };
        let mut seen = std::collections::HashSet::new();
        let mut activities = Vec::new();
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
                    let Some(agent_label) = terminal.effective_agent_label() else {
                        continue;
                    };
                    let Some(terminal_ref) = self.host_terminal_ref_for_pane(*pane_id) else {
                        continue;
                    };
                    let (source, evidence_kind) = terminal.effective_evidence_attribution();
                    let effective_display_agent = terminal.effective_presentation().display_agent;
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
                    activities.push(crate::extension_contract::HostAgentActivityRecord {
                        terminal: terminal_ref,
                        activity,
                        source: bounded_document_text(source),
                        evidence_kind: evidence_kind.into(),
                        state_change_seq: terminal.last_agent_state_change_seq.unwrap_or(0),
                        labels: std::collections::BTreeMap::from([
                            ("agent".into(), bounded_document_text(agent_label)),
                            (
                                "display".into(),
                                bounded_document_text(
                                    effective_display_agent.as_deref().unwrap_or(agent_label),
                                ),
                            ),
                        ]),
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
        };
        let key = (namespace.to_string(), scope_id.clone());
        if snapshot.validate().is_err() {
            self.collections_shadow_documents.remove(&key);
            self.host_document_request_watermarks.remove(&key);
            self.host_document_retry_snapshots.remove(&key);
            self.host_agent_activity_last_desired = None;
            return;
        }
        if self
            .host_agent_activity_last_desired
            .as_ref()
            .is_some_and(|previous| {
                let mut normalized = previous.clone();
                normalized.revision = 1;
                normalized == snapshot
            })
        {
            return;
        }
        snapshot.revision = self.next_host_agent_activity_document_revision();
        self.host_agent_activity_last_desired = Some(snapshot.clone());
        self.host_document_request_watermarks
            .insert(key.clone(), (generation, snapshot.revision));
        self.disable_host_document_projection(&key, generation, snapshot.revision);
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
                self.host_agent_activity_last_desired = None;
                self.schedule_host_semantic_retry(namespace);
                tracing::warn!(%error, "Agent Status provider unavailable; using native fallback");
            }
        }
    }

    fn disable_host_document_projection(
        &mut self,
        key: &(String, String),
        generation: u64,
        revision: u64,
    ) {
        let was_available = self
            .collections_shadow_documents
            .get(key)
            .and_then(|cache| cache.current())
            .is_some();
        if let Some(cache) = self.collections_shadow_documents.get_mut(key) {
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

    fn emit_host_document_updated(
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
            match result.result {
                Ok(document) => {
                    self.complete_host_semantic_retry(&result.namespace);
                    let _ = self
                        .collections_shadow_documents
                        .entry(key)
                        .or_default()
                        .publish_for_namespace(
                            &result.namespace,
                            result.snapshot.generation,
                            result.provider_priority,
                            &result.grants,
                            document,
                        );
                }
                Err(error) => {
                    self.host_relationships_last_desired = None;
                    self.schedule_host_semantic_retry(&result.namespace);
                    tracing::warn!(%error, "Delegation document render failed; using relationship fallback");
                }
            }
        }
        if current
            && self
                .collections_shadow_documents
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
            match result.result {
                Ok(document) => {
                    self.complete_host_semantic_retry(&result.namespace);
                    let _ = self
                        .collections_shadow_documents
                        .entry(key)
                        .or_default()
                        .publish_for_namespace(
                            &result.namespace,
                            result.snapshot.generation,
                            result.provider_priority,
                            &result.grants,
                            document,
                        );
                }
                Err(error) => {
                    self.host_agent_activity_last_desired = None;
                    self.schedule_host_semantic_retry(&result.namespace);
                    tracing::warn!(%error, "Agent Status document render failed; using native fallback");
                }
            }
        }
        if current
            && self
                .collections_shadow_documents
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

    pub(crate) fn handle_host_spaces_document_result(
        &mut self,
        mut result: crate::extension_service::HostSpacesDocumentAsyncResult,
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
            match result.result {
                Ok(document) => {
                    self.host_spaces_retry_epoch =
                        self.host_spaces_retry_epoch.saturating_add(1).max(1);
                    self.host_spaces_retry_attempt = 0;
                    self.host_spaces_retry_scheduled = false;
                    let cache = self.collections_shadow_documents.entry(key).or_default();
                    let _ = cache.publish_for_namespace(
                        &result.namespace,
                        result.snapshot.generation,
                        result.provider_priority,
                        &result.grants,
                        document,
                    );
                }
                Err(error) => {
                    self.host_spaces_last_desired = None;
                    self.schedule_host_spaces_retry();
                    tracing::warn!(%error, "Spaces document render failed; using native fallback");
                }
            }
        }
        if current
            && self
                .collections_shadow_documents
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

    fn schedule_host_spaces_retry(&mut self) {
        if self.host_spaces_retry_scheduled
            || self.host_spaces_retry_attempt >= MAX_HOST_SPACES_RETRY_ATTEMPTS
            || self
                .host_document_services
                .generation(crate::first_party::spaces::NAMESPACE)
                .is_none()
        {
            if self.host_spaces_retry_attempt >= MAX_HOST_SPACES_RETRY_ATTEMPTS {
                tracing::warn!(
                    attempts = self.host_spaces_retry_attempt,
                    "Spaces provider retry budget exhausted; retaining native fallback"
                );
            }
            return;
        }
        let exponent = self.host_spaces_retry_attempt.min(6);
        let delay_ms = 100_u64.saturating_mul(1_u64 << exponent).min(5_000);
        self.host_spaces_retry_attempt = self.host_spaces_retry_attempt.saturating_add(1);
        self.host_spaces_retry_scheduled = true;
        let epoch = self.host_spaces_retry_epoch;
        let event_tx = self.event_tx.clone();
        if let Err(error) = std::thread::Builder::new()
            .name("vqro-spaces-retry".into())
            .spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(delay_ms));
                let mut event = crate::events::AppEvent::HostSpacesRetryDue { epoch };
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
            self.host_spaces_retry_scheduled = false;
            tracing::warn!(%error, "failed to schedule Spaces provider retry");
        }
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
        let retry_snapshot = (result.namespace == crate::first_party::collections::NAMESPACE
            && result.result.is_err())
        .then(|| (request_key.clone(), result.snapshot.clone()));
        let available = {
            let cache = self
                .collections_shadow_documents
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
                if result.namespace == crate::first_party::collections::NAMESPACE {
                    tracing::warn!(%error, "Collections provider result rejected; using generic fallback");
                    let _ = cache.install_fallback(&result.snapshot);
                } else {
                    tracing::warn!(namespace = %result.namespace, %error, "host document provider result rejected");
                }
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
        retry_snapshot
            .is_some_and(|(key, snapshot)| self.schedule_host_document_render_retry(key, snapshot))
    }

    pub(super) fn authorize_host_document_action(
        &mut self,
        params: &crate::api::schema::HostDocumentActionParams,
    ) -> Result<AuthorizedHostDocumentAction, String> {
        let cache = self
            .collections_shadow_documents
            .get(&(params.namespace.clone(), params.scope_id.clone()))
            .or_else(|| {
                self.collections_shadow_documents
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
            .collections_shadow_snapshot(ws_idx, tab_idx, params.generation, params.revision)
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
                                scope_id == crate::first_party::spaces::SESSION_SCOPE_ID
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
            crate::first_party::spaces::SESSION_SCOPE_ID.to_string(),
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
            .collections_shadow_documents
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

    fn collections_shadow_snapshot(
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
            .layout
            .collections()
            .map(|collection| HostTerminalGroup {
                group_id: collection_id_text(collection.id),
                label: collection.label.clone(),
                selected_terminal_id: collection.selected().and_then(|pane_id| {
                    tab.panes
                        .get(&pane_id)
                        .map(|pane| pane.attached_terminal_id.to_string())
                }),
                members: collection
                    .members()
                    .iter()
                    .filter_map(|pane_id| member(*pane_id, collection.is_archived(*pane_id)))
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
    pub(crate) fn collections_shadow_document(
        &self,
        ws_idx: usize,
        tab_idx: usize,
        generation: u64,
        revision: u64,
    ) -> Result<
        crate::extension_contract::HostDocumentV1,
        crate::extension_contract::HostDocumentError,
    > {
        let snapshot = self.collections_shadow_snapshot(ws_idx, tab_idx, generation, revision)?;
        crate::first_party::collections::project(&snapshot)
    }

    fn find_collection(&self, id: CollectionId) -> Option<(usize, usize)> {
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
        let selected = collection.selected();
        Some(CollectionInfo {
            collection_id,
            workspace_id: self.public_workspace_id(ws_idx),
            tab_id: self.public_tab_id(ws_idx, tab_idx)?,
            label: collection.label.clone(),
            focused: tab.layout.focused_leaf() == LayoutLeaf::Collection(id),
            selected_pane_id: selected.and_then(|pane| self.public_pane_id(ws_idx, pane)),
            members: collection
                .members()
                .iter()
                .enumerate()
                .filter_map(|(index, pane)| {
                    Some(CollectionMemberInfo {
                        pane_id: self.public_pane_id(ws_idx, *pane)?,
                        index,
                        archived: collection.is_archived(*pane),
                        selected: selected == Some(*pane),
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
                    if let Some(info) = self.collection_info(ws_idx, tab_idx, id) {
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
        responses::encode_success(
            request_id,
            ResponseResult::CollectionList {
                collections: self.collection_infos(workspace_filter, tab_filter),
            },
        )
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
        params: CollectionCreateParams,
    ) -> String {
        let Some((ws_idx, pane_id)) = self.parse_current_public_pane_id(&params.target_pane_id)
        else {
            return responses::encode_error(request_id, "not_found", "target pane was not found");
        };
        let Some(tab_idx) = self.state.workspaces[ws_idx].find_tab_index_for_pane(pane_id) else {
            return responses::encode_error(request_id, "not_found", "target tab was not found");
        };
        let group_count = self.state.workspaces[ws_idx].tabs[tab_idx]
            .layout
            .collections()
            .count();
        if let Err(error) = crate::admission::group(group_count) {
            return responses::encode_error(request_id, "resource_limit", error.to_string());
        }
        let created = self.state.workspaces[ws_idx].tabs[tab_idx].create_collection_near(
            LayoutLeaf::Pane(pane_id),
            direction(params.direction),
            params.ratio.unwrap_or(0.5),
            params.label,
        );
        let Ok(collection_id) = created else {
            return responses::encode_error(
                request_id,
                "invalid_placement",
                "collection could not be created at that target",
            );
        };
        if params.focus {
            let _ = self.state.workspaces[ws_idx].tabs[tab_idx]
                .layout
                .focus_leaf(LayoutLeaf::Collection(collection_id));
        }
        self.state.mark_session_dirty();
        let collection = self
            .collection_info(ws_idx, tab_idx, collection_id)
            .expect("created collection");
        self.emit_collection_event("created", collection.clone());
        responses::encode_success(request_id, ResponseResult::CollectionInfo { collection })
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
                    tab.layout.focus_leaf(LayoutLeaf::Collection(collection));
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
        params: CollectionMemberTarget,
        archived: bool,
    ) -> String {
        self.mutate_collection_member(
            request_id,
            &params.collection_id,
            &params.pane_id,
            |tab, collection, pane| tab.set_collection_member_archived(collection, pane, archived),
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
                collection_id,
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
            self.state.workspaces[ws_idx].tabs[tab_idx]
                .layout
                .focus_pane(target);
        }
        self.state.mark_session_dirty();
        let collection = self
            .collection_info(ws_idx, tab_idx, collection_id)
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
                    let (events, render_notify, render_dirty) = {
                        let tab = &self.state.workspaces[ws_idx].tabs[tab_idx];
                        (
                            tab.events.clone(),
                            tab.render_notify.clone(),
                            tab.render_dirty.clone(),
                        )
                    };
                    let mut created = Vec::with_capacity(members.len());
                    for pane_id in members {
                        let Some(taken) = self.state.workspaces[ws_idx].take_pane_for_move(pane_id)
                        else {
                            return responses::encode_error(
                                request_id,
                                "collection_close_failed",
                                "collection member could not be detached",
                            );
                        };
                        let tab_idx = self.state.workspaces[ws_idx].create_tab_from_existing_pane(
                            taken.moved,
                            None,
                            events.clone(),
                            render_notify.clone(),
                            render_dirty.clone(),
                        );
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
    use crate::api::schema::{Method, Request, SplitDirection};

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
    fn collection_shadow_uses_stable_terminal_identity_and_generic_contracts() {
        let (app, collection, member) = app_with_nonempty_collection();
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&member]
            .attached_terminal_id
            .to_string();

        let document = app.collections_shadow_document(0, 0, 4, 9).unwrap();

        assert_eq!(
            document.namespace,
            crate::first_party::collections::NAMESPACE
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
    fn failed_collections_render_schedules_delayed_recovery() {
        let mut app = app_with_workspace();
        app.host_document_services =
            crate::extension_service::HostDocumentServiceCatalog::with_test_provider(
                crate::first_party::collections::NAMESPACE,
                7,
            );
        let snapshot = app.collections_shadow_snapshot(0, 0, 7, 1).unwrap();
        let key = (
            crate::first_party::collections::NAMESPACE.to_string(),
            snapshot.scope_id.clone(),
        );
        let retry = (key.0.clone(), key.1.clone(), 7, 1);
        handle_current_result(
            &mut app,
            crate::extension_service::HostDocumentAsyncResult {
                _render_slot: None,
                namespace: key.0.clone(),
                provider_priority: 100,
                snapshot: snapshot.clone(),
                result: Err("provider crashed".into()),
                grants: std::collections::HashSet::new(),
            },
        );

        assert!(app.host_document_retry_snapshots.contains_key(&key));
        assert!(app.host_document_retry_scheduled.contains(&retry));
        assert_eq!(app.host_document_retry_attempts.get(&retry), Some(&1));
        assert!(app
            .host_documents()
            .iter()
            .any(|entry| entry.document.namespace == "host.fallback"));

        handle_current_result(
            &mut app,
            crate::extension_service::HostDocumentAsyncResult {
                _render_slot: None,
                namespace: key.0,
                provider_priority: 100,
                result: crate::first_party::collections::project(&snapshot)
                    .map_err(|error| error.to_string()),
                snapshot,
                grants: std::collections::HashSet::from([
                    crate::extension_contract::HostCapability::FocusTerminal,
                ]),
            },
        );
        assert!(app
            .host_documents()
            .iter()
            .any(|entry| entry.document.namespace == crate::first_party::collections::NAMESPACE));
        assert!(!app.host_document_retry_scheduled.contains(&retry));
    }

    #[test]
    fn third_party_document_is_projected_and_authorizes_its_declared_action() {
        let mut app = app_with_workspace();
        let namespace = "example.collections";
        app.host_document_services =
            crate::extension_service::HostDocumentServiceCatalog::with_test_providers(&[
                (namespace, 9),
                (crate::first_party::collections::NAMESPACE, 10),
            ]);
        let snapshot = app.collections_shadow_snapshot(0, 0, 9, 1).unwrap();
        let mut document = crate::first_party::collections::project(&snapshot).unwrap();
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
        let builtin_snapshot = app.collections_shadow_snapshot(0, 0, 10, 1).unwrap();
        let builtin_document = crate::first_party::collections::project(&builtin_snapshot).unwrap();
        handle_current_result(
            &mut app,
            crate::extension_service::HostDocumentAsyncResult {
                _render_slot: None,
                namespace: crate::first_party::collections::NAMESPACE.into(),
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
                crate::first_party::collections::NAMESPACE,
                9,
            );
        let snapshot = app.collections_shadow_snapshot(0, 0, 9, 1).unwrap();
        let scope_id = snapshot.scope_id.clone();
        let document = crate::first_party::collections::project(&snapshot).unwrap();
        handle_current_result(
            &mut app,
            crate::extension_service::HostDocumentAsyncResult {
                _render_slot: None,
                namespace: crate::first_party::collections::NAMESPACE.into(),
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
                namespace: Some(crate::first_party::collections::NAMESPACE.into()),
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
            crate::first_party::collections::NAMESPACE
        );
        assert!(app.event_hub.events_after(0).iter().any(|(_, event)| {
            matches!(
                &event.data,
                crate::api::schema::EventData::HostDocumentUpdated {
                    namespace,
                    available: true,
                    ..
                } if namespace == crate::first_party::collections::NAMESPACE
            )
        }));
        let sequence = app.event_hub.current_sequence();
        app.disable_host_document_projection(
            &(crate::first_party::collections::NAMESPACE.into(), scope_id),
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
    fn terminal_interrupt_requires_confirmation_before_dispatch() {
        let mut app = app_with_workspace();
        let namespace = "example.interrupt";
        app.host_document_services =
            crate::extension_service::HostDocumentServiceCatalog::with_test_provider(namespace, 8);
        let snapshot = app.collections_shadow_snapshot(0, 0, 8, 2).unwrap();
        let mut document = crate::first_party::collections::project(&snapshot).unwrap();
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
    fn host_fallback_namespace_action_remains_authorized() {
        let mut app = app_with_workspace();
        app.refresh_collection_documents();
        let entry = app
            .host_documents()
            .into_iter()
            .find(|entry| entry.document.namespace == "host.fallback")
            .expect("disabled test worker installs host fallback");
        let document = entry.document;
        let action = document.actions.first().unwrap();
        assert!(matches!(
            app.authorize_host_document_action(&crate::api::schema::HostDocumentActionParams {
                namespace: document.namespace.clone(),
                scope_id: match &document.target {
                    crate::extension_contract::HostTarget::Tab { tab_id } => tab_id.clone(),
                    _ => panic!("tab fallback"),
                },
                generation: document.generation,
                revision: document.revision,
                action_id: action.id.clone(),
                confirmed: false,
                confirmation_token: None,
            })
            .unwrap(),
            AuthorizedHostDocumentAction::Focus(_)
        ));
    }

    #[test]
    fn provider_owned_action_is_fenced_and_correlated_to_its_catalog_generation() {
        let mut app = app_with_workspace();
        let namespace = "example.actions";
        app.host_document_services =
            crate::extension_service::HostDocumentServiceCatalog::with_test_action_sink(
                namespace, 12,
            );
        let snapshot = app.collections_shadow_snapshot(0, 0, 12, 4).unwrap();
        let mut document = crate::first_party::collections::project(&snapshot).unwrap();
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
    fn pane_focus_refreshes_document_revision_and_selection() {
        let (mut app, _collection, member) = app_with_nonempty_collection();
        let root = app.state.workspaces[0].tabs[0].root_pane;
        app.state.focus_pane_in_workspace(0, root);
        app.refresh_collection_documents();
        let before = app.host_documents()[0].document.revision;
        let pane_id = app.public_pane_id(0, member).unwrap();
        let response =
            app.handle_pane_focus("focus".into(), crate::api::schema::PaneTarget { pane_id });
        let _: crate::api::schema::SuccessResponse = serde_json::from_str(&response).unwrap();
        let document = &app.host_documents()[0].document;
        assert!(document.revision > before);
        assert_eq!(app.state.workspaces[0].focused_pane_id(), Some(member));
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
        let snapshot = app.collections_shadow_snapshot(0, 0, 12, 4).unwrap();
        app.host_document_services.set_desired_revision_for_test(
            namespace,
            snapshot.scope_id.clone(),
            snapshot.generation,
            snapshot.revision,
        );
        let mut document = crate::first_party::collections::project(&snapshot).unwrap();
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
    fn pane_rename_refreshes_document_and_removed_target_is_retired() {
        let mut app = app_with_workspace();
        let pane_id = app
            .public_pane_id(0, app.state.workspaces[0].focused_pane_id().unwrap())
            .unwrap();
        let response = app.handle_pane_rename(
            "rename".into(),
            crate::api::schema::PaneRenameParams {
                pane_id,
                label: Some("renamed terminal".into()),
            },
        );
        assert!(serde_json::from_str::<crate::api::schema::SuccessResponse>(&response).is_ok());
        let documents = app.host_documents();
        assert_eq!(documents.len(), 1);
        assert!(documents[0]
            .document
            .items
            .iter()
            .any(|item| item.label == "renamed terminal"));
        app.state.workspaces[0].tabs.clear();
        app.refresh_collection_documents();
        assert!(app.collections_shadow_documents.is_empty());
        assert!(app.host_documents().is_empty());
    }

    #[test]
    fn documents_for_removed_targets_are_retired_from_projection() {
        let mut app = app_with_workspace();
        let namespace = crate::first_party::collections::NAMESPACE;
        app.host_document_services =
            crate::extension_service::HostDocumentServiceCatalog::with_test_provider(namespace, 4);
        let snapshot = app.collections_shadow_snapshot(0, 0, 4, 1).unwrap();
        let document = crate::first_party::collections::project(&snapshot).unwrap();
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
        assert!(app.collections_shadow_documents.is_empty());
    }

    #[test]
    fn provider_cannot_choose_result_revision_or_scope() {
        let mut app = app_with_workspace();
        let namespace = crate::first_party::collections::NAMESPACE;
        app.host_document_services =
            crate::extension_service::HostDocumentServiceCatalog::with_test_provider(namespace, 7);
        let snapshot = app.collections_shadow_snapshot(0, 0, 7, 1).unwrap();
        let mut document = crate::first_party::collections::project(&snapshot).unwrap();
        document.revision = 99;
        document.target = crate::extension_contract::HostTarget::Tab {
            tab_id: "other-tab".into(),
        };
        handle_current_result(
            &mut app,
            crate::extension_service::HostDocumentAsyncResult {
                _render_slot: None,
                namespace: namespace.into(),
                provider_priority: 100,
                snapshot,
                result: Ok(document),
                grants: std::collections::HashSet::from([
                    crate::extension_contract::HostCapability::FocusTerminal,
                ]),
            },
        );
        let documents = app.host_documents();
        assert_eq!(documents.len(), 1);
        assert_eq!(documents[0].document.namespace, "host.fallback");
        assert_eq!(documents[0].document.revision, 1);
    }

    #[test]
    fn saturated_new_scope_is_retried_when_a_global_render_slot_releases() {
        let mut app = app_with_workspace();
        let namespace = crate::first_party::collections::NAMESPACE;
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
    fn shared_render_retry_queue_wakes_spaces_after_terminal_group_release() {
        let mut app = app_with_workspace();
        let collections = crate::first_party::collections::NAMESPACE;
        let spaces = crate::first_party::spaces::NAMESPACE;
        app.host_document_services =
            crate::extension_service::HostDocumentServiceCatalog::with_test_saturated_render_providers(&[
                (collections, 7, crate::api::schema::HostDocumentSourceKind::TerminalGroups),
                (spaces, 8, crate::api::schema::HostDocumentSourceKind::Spaces),
            ]);
        app.refresh_collection_documents();
        assert_eq!(app.host_document_retry_snapshots.len(), 2);

        app.host_document_services.release_test_render_slot();
        app.retry_one_host_document_render();
        assert_eq!(
            app.host_document_services
                .pending_render_count_for_test(collections),
            1
        );
        assert!(app.host_document_retry_snapshots.contains_key(&(
            spaces.into(),
            crate::first_party::spaces::SESSION_SCOPE_ID.into()
        )));

        app.host_document_services
            .retain_scopes(&std::collections::HashSet::from([
                crate::first_party::spaces::SESSION_SCOPE_ID.to_string(),
            ]));
        app.retry_one_host_document_render();
        assert_eq!(
            app.host_document_services
                .pending_render_count_for_test(spaces),
            1
        );
        assert!(app.host_document_retry_snapshots.is_empty());
    }

    #[test]
    fn transient_spaces_failure_schedules_bounded_idle_retry() {
        let mut app = app_with_workspace();
        let namespace = crate::first_party::spaces::NAMESPACE;
        app.host_document_services =
            crate::extension_service::HostDocumentServiceCatalog::with_test_document_source_provider(
                namespace,
                9,
                crate::api::schema::HostDocumentSourceKind::Spaces,
            );
        let snapshot = crate::extension_contract::HostSpacesV1 {
            contract: "host.spaces.v1".into(),
            generation: 9,
            revision: 4,
            scope_id: crate::first_party::spaces::SESSION_SCOPE_ID.into(),
            repositories: Vec::new(),
            standalone: Vec::new(),
            order: Vec::new(),
        };
        app.host_document_request_watermarks.insert(
            (namespace.into(), snapshot.scope_id.clone()),
            (snapshot.generation, snapshot.revision),
        );
        app.handle_host_spaces_document_result(
            crate::extension_service::HostSpacesDocumentAsyncResult {
                _render_slot: None,
                namespace: namespace.into(),
                provider_priority: 100,
                snapshot,
                result: Err("transient crash".into()),
                grants: std::collections::HashSet::new(),
            },
        );
        assert!(app.host_spaces_retry_scheduled);
        assert_eq!(app.host_spaces_retry_attempt, 1);
        let event = app
            .event_rx
            .blocking_recv()
            .expect("retry timer should wake an idle app");
        assert!(matches!(
            &event,
            crate::events::AppEvent::HostSpacesRetryDue { .. }
        ));
        app.handle_internal_event(event);
        assert!(!app.host_spaces_retry_scheduled);
        assert!(app.host_spaces_last_desired.is_some());
        assert_eq!(
            app.host_document_services
                .pending_render_count_for_test(namespace),
            1
        );
    }

    #[test]
    fn spaces_failure_stops_after_retry_budget_until_new_lifecycle_work() {
        let mut app = app_with_workspace();
        let namespace = crate::first_party::spaces::NAMESPACE;
        app.host_document_services =
            crate::extension_service::HostDocumentServiceCatalog::with_test_document_source_provider(
                namespace,
                9,
                crate::api::schema::HostDocumentSourceKind::Spaces,
            );
        app.host_spaces_retry_attempt = MAX_HOST_SPACES_RETRY_ATTEMPTS;
        app.host_spaces_retry_scheduled = false;
        let snapshot = crate::extension_contract::HostSpacesV1 {
            contract: "host.spaces.v1".into(),
            generation: 9,
            revision: 4,
            scope_id: crate::first_party::spaces::SESSION_SCOPE_ID.into(),
            repositories: Vec::new(),
            standalone: Vec::new(),
            order: Vec::new(),
        };
        app.host_document_request_watermarks.insert(
            (namespace.into(), snapshot.scope_id.clone()),
            (snapshot.generation, snapshot.revision),
        );
        app.handle_host_spaces_document_result(
            crate::extension_service::HostSpacesDocumentAsyncResult {
                _render_slot: None,
                namespace: namespace.into(),
                provider_priority: 100,
                snapshot,
                result: Err("persistent invalid output".into()),
                grants: std::collections::HashSet::new(),
            },
        );
        assert!(!app.host_spaces_retry_scheduled);
        assert_eq!(
            app.host_spaces_retry_attempt,
            MAX_HOST_SPACES_RETRY_ATTEMPTS
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
            scope_id: crate::first_party::spaces::SESSION_SCOPE_ID.into(),
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
        let snapshot = app.host_relationships_last_desired.as_ref().unwrap();
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
            state_labels: std::collections::HashMap::new(),
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
        let snapshot = app.host_agent_activity_last_desired.as_ref().unwrap();
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
        assert!(!serde_json::to_string(snapshot).unwrap().contains("done"));
        let previous_revision = snapshot.revision;

        app.sync_agent_metadata_deadline();
        let deadline = app.agent_metadata_deadline.unwrap();
        app.expire_metadata_at(deadline, deadline + std::time::Duration::from_millis(1));
        let refreshed = app.host_agent_activity_last_desired.as_ref().unwrap();
        assert!(refreshed.revision > previous_revision);
        assert_ne!(
            refreshed.activities[0]
                .labels
                .get("display")
                .map(String::as_str),
            Some("Friendly Claude")
        );
    }

    #[test]
    fn hook_state_reports_refresh_agent_activity_documents() {
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
            .as_ref()
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

        let snapshot = app.host_agent_activity_last_desired.as_ref().unwrap();
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
        assert!(app.host_relationships_last_desired.is_none());
        assert!(!app.collections_shadow_documents.contains_key(&(
            namespace.into(),
            crate::first_party::spaces::SESSION_SCOPE_ID.into()
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
            let mut cache = crate::first_party::collections::ShadowDocumentCache::default();
            cache
                .publish_for_namespace(
                    &namespace,
                    1,
                    100,
                    &std::collections::HashSet::new(),
                    document,
                )
                .unwrap();
            app.collections_shadow_documents.insert(
                (
                    namespace,
                    crate::first_party::spaces::SESSION_SCOPE_ID.into(),
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

    #[test]
    fn retired_provider_action_completion_is_final_fenced_on_actor() {
        let mut app = app_with_workspace();
        let namespace = crate::first_party::collections::NAMESPACE;
        app.host_document_services =
            crate::extension_service::HostDocumentServiceCatalog::with_test_provider(namespace, 7);
        let snapshot = app.collections_shadow_snapshot(0, 0, 7, 3).unwrap();
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
        let namespace = crate::first_party::collections::NAMESPACE;
        app.host_document_services =
            crate::extension_service::HostDocumentServiceCatalog::with_test_provider(namespace, 7);
        let newest = app.collections_shadow_snapshot(0, 0, 7, 2).unwrap();
        app.host_document_request_watermarks.insert(
            (namespace.into(), newest.scope_id.clone()),
            (newest.generation, newest.revision),
        );
        let older = app.collections_shadow_snapshot(0, 0, 7, 1).unwrap();
        let older_document = crate::first_party::collections::project(&older).unwrap();
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
        let namespace = crate::first_party::collections::NAMESPACE;
        app.host_document_services =
            crate::extension_service::HostDocumentServiceCatalog::with_test_provider(namespace, 7);
        let newer_snapshot = app.collections_shadow_snapshot(0, 0, 7, 2).unwrap();
        let newer_document = crate::first_party::collections::project(&newer_snapshot).unwrap();
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
        let stale_snapshot = app.collections_shadow_snapshot(0, 0, 7, 1).unwrap();
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

    #[test]
    fn collection_create_and_list_use_stable_ids() {
        let mut app = app_with_workspace();
        let root = app.state.workspaces[0].tabs[0].root_pane;
        let pane_id = app.public_pane_id(0, root).expect("public pane id");
        let response = app.handle_api_request(Request {
            id: "create".into(),
            method: Method::CollectionCreate(CollectionCreateParams {
                target_pane_id: pane_id,
                direction: SplitDirection::Right,
                ratio: Some(0.4),
                label: Some("helpers".into()),
                focus: true,
            }),
        });
        let created: crate::api::schema::SuccessResponse =
            serde_json::from_str(&response).expect("response");
        let ResponseResult::CollectionInfo { collection } = created.result else {
            panic!("collection response")
        };
        assert!(collection.collection_id.starts_with("collection_"));
        assert_eq!(collection.label.as_deref(), Some("helpers"));
        assert!(collection.focused);

        let listed = app.handle_api_request(Request {
            id: "list".into(),
            method: Method::CollectionList(CollectionListParams::default()),
        });
        let listed: crate::api::schema::SuccessResponse =
            serde_json::from_str(&listed).expect("response");
        let ResponseResult::CollectionList { collections } = listed.result else {
            panic!("list response")
        };
        assert_eq!(collections.len(), 1);
        assert_eq!(collections[0].collection_id, collection.collection_id);
    }
}
