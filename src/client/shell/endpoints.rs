use super::*;

#[derive(Clone, Debug)]
pub(crate) struct ClientEndpointAgentViewProjection {
    generation: Option<u64>,
    boot_id: String,
    revision: u64,
    pub(crate) view: Result<Option<crate::api::schema::AgentViewSetParams>, ()>,
}

#[derive(Clone, Debug)]
pub(crate) struct ClientShellEndpoint {
    pub(crate) endpoint_id: ClientEndpointId,
    pub(crate) label: String,
    pub(crate) status: ClientEndpointStatus,
    pub(crate) snapshot: Option<Box<ClientShellSnapshot>>,
    /// Connection generation that produced `snapshot`. `None` is reserved for local tests.
    pub(crate) snapshot_generation: Option<u64>,
    pub(crate) structure: Option<Box<crate::protocol::endpoint::ClientShellStructureProjection>>,
    pub(crate) structure_generation: Option<u64>,
    pub(crate) host_documents: Option<Box<crate::protocol::endpoint::HostDocumentsProjectionV1>>,
    pub(crate) host_documents_generation: Option<u64>,
    pub(crate) agent_recency: HashMap<String, u64>,
    pub(super) agent_presentation: super::endpoint_agent_state::EndpointAgentPresentation,
    pub(crate) agent_view_projection: Option<ClientEndpointAgentViewProjection>,
    pending_agent_view_projection: Option<ClientEndpointAgentViewProjection>,
    pub(crate) agent_view_projection_supported: bool,
    pub(crate) methods: Option<HashSet<String>>,
    pub(crate) capabilities: Option<HashSet<String>>,
}

impl ClientShellEndpoint {
    pub(super) fn current_host_documents(
        &self,
    ) -> Option<&crate::protocol::endpoint::HostDocumentsProjectionV1> {
        let snapshot = self.snapshot.as_ref()?;
        (self.host_documents_generation == self.snapshot_generation)
            .then_some(self.host_documents.as_deref()?)
            .filter(|documents| {
                documents.boot_id == snapshot.boot_id && documents.revision == snapshot.revision
            })
    }

    pub(super) fn current_structure(
        &self,
    ) -> Option<&crate::protocol::endpoint::ClientShellStructureProjection> {
        let snapshot = self.snapshot.as_ref()?;
        (self.structure_generation == self.snapshot_generation)
            .then_some(self.structure.as_deref()?)
            .filter(|structure| {
                structure.boot_id == snapshot.boot_id && structure.revision == snapshot.revision
            })
    }
}

pub(super) struct MachineHit {
    pub(super) rect: Rect,
    pub(super) collapse_toggle: Rect,
    pub(super) endpoint_id: ClientEndpointId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ClientEndpointFocusTarget {
    Workspace(String),
    Tab(String),
    Pane(String),
}

impl ClientShellState {
    pub(crate) fn set_endpoint_catalog(&mut self, profiles: &[SavedSshEndpoint]) {
        let mut next = Vec::with_capacity(profiles.len().saturating_add(1));
        let local = self
            .endpoints
            .iter()
            .find(|endpoint| endpoint.endpoint_id.is_local())
            .cloned()
            .unwrap_or_else(local_endpoint);
        next.push(local);
        for profile in profiles {
            let endpoint_id = ClientEndpointId::Ssh(profile.id.clone());
            let previous = self
                .endpoints
                .iter()
                .find(|endpoint| endpoint.endpoint_id == endpoint_id)
                .filter(|endpoint| {
                    profile.enabled && endpoint.status != ClientEndpointStatus::Disabled
                });
            next.push(ClientShellEndpoint {
                endpoint_id,
                label: profile.label.clone(),
                status: previous.map_or(
                    if profile.enabled {
                        ClientEndpointStatus::Connecting
                    } else {
                        ClientEndpointStatus::Disabled
                    },
                    |endpoint| endpoint.status,
                ),
                snapshot: previous.and_then(|endpoint| endpoint.snapshot.clone()),
                snapshot_generation: previous.and_then(|endpoint| endpoint.snapshot_generation),
                structure: previous.and_then(|endpoint| endpoint.structure.clone()),
                structure_generation: previous.and_then(|endpoint| endpoint.structure_generation),
                host_documents: previous.and_then(|endpoint| endpoint.host_documents.clone()),
                host_documents_generation: previous
                    .and_then(|endpoint| endpoint.host_documents_generation),
                agent_recency: previous
                    .map(|endpoint| endpoint.agent_recency.clone())
                    .unwrap_or_default(),
                agent_presentation: previous
                    .map(|endpoint| endpoint.agent_presentation.clone())
                    .unwrap_or_default(),
                agent_view_projection: previous
                    .and_then(|endpoint| endpoint.agent_view_projection.clone()),
                pending_agent_view_projection: previous
                    .and_then(|endpoint| endpoint.pending_agent_view_projection.clone()),
                agent_view_projection_supported: previous
                    .is_some_and(|endpoint| endpoint.agent_view_projection_supported),
                methods: previous.and_then(|endpoint| endpoint.methods.clone()),
                capabilities: previous.and_then(|endpoint| endpoint.capabilities.clone()),
            });
        }

        if !next
            .iter()
            .any(|endpoint| endpoint.endpoint_id == self.active_endpoint_id)
        {
            self.select_unavailable_local();
        }
        self.collapsed_endpoints.retain(|endpoint_id| {
            next.iter()
                .any(|endpoint| &endpoint.endpoint_id == endpoint_id)
        });
        self.endpoints = next;
        if let Some(endpoint_id) = self.overlay.as_ref().and_then(|overlay| match overlay {
            ClientShellOverlay::Recovery(recovery) => Some(recovery.endpoint_id.clone()),
            _ => None,
        }) {
            self.refresh_recovery_overlay(&endpoint_id);
        }
    }

    pub(crate) fn select_unavailable_local(&mut self) {
        self.reset_endpoint_projection();
        self.active_endpoint_id = ClientEndpointId::Local;
        self.mode = ClientShellMode::Terminal;
        self.snapshot = None;
        self.graphics.set_scope("local:unavailable");
        self.reconcile_input_source();
    }

    pub(crate) fn retire_endpoint(&mut self, endpoint_id: &ClientEndpointId) {
        self.retire_endpoint_notifications(endpoint_id);
        if let Some(endpoint) = self
            .endpoints
            .iter_mut()
            .find(|endpoint| &endpoint.endpoint_id == endpoint_id)
        {
            endpoint.status = ClientEndpointStatus::Disabled;
            endpoint.snapshot = None;
            endpoint.snapshot_generation = None;
            endpoint.structure = None;
            endpoint.structure_generation = None;
            endpoint.methods = None;
            endpoint.capabilities = None;
            endpoint.agent_recency.clear();
            endpoint.agent_presentation = Default::default();
            endpoint.agent_view_projection = None;
            endpoint.pending_agent_view_projection = None;
            endpoint.agent_view_projection_supported = false;
        }
        self.refresh_recovery_overlay(endpoint_id);
    }

    pub(crate) fn set_endpoint_status(
        &mut self,
        endpoint_id: &ClientEndpointId,
        status: ClientEndpointStatus,
    ) {
        if endpoint_id == &self.active_endpoint_id && status != ClientEndpointStatus::Online {
            self.inspector = None;
            self.last_pane_viewport_request = None;
            if matches!(
                self.overlay.as_ref(),
                Some(ClientShellOverlay::PaneInterruptConfirm(_))
            ) {
                self.overlay = None;
            }
        }
        if let Some(endpoint) = self
            .endpoints
            .iter_mut()
            .find(|endpoint| &endpoint.endpoint_id == endpoint_id)
        {
            endpoint.status = status;
        }
    }

    pub(crate) fn mark_endpoint_disconnected(&mut self, endpoint_id: &ClientEndpointId) {
        self.set_endpoint_status(endpoint_id, ClientEndpointStatus::Reconnecting);
        if endpoint_id == &self.active_endpoint_id {
            self.inspector = None;
            self.last_pane_viewport_request = None;
            if matches!(
                self.overlay.as_ref(),
                Some(ClientShellOverlay::PaneInterruptConfirm(_))
            ) {
                self.overlay = None;
            }
            let pending = self.pending_requests.keys().cloned().collect::<Vec<_>>();
            for request_id in pending {
                self.cancel_endpoint_request(&request_id);
            }
            self.pending_integration_installs = 0;
            self.pane_scroll_in_flight.clear();
            self.pane_scroll_queued.clear();
        }
    }

    pub(crate) fn set_endpoint_agent_view_projection_supported(
        &mut self,
        endpoint_id: &ClientEndpointId,
        supported: bool,
    ) {
        if let Some(endpoint) = self
            .endpoints
            .iter_mut()
            .find(|endpoint| &endpoint.endpoint_id == endpoint_id)
        {
            endpoint.agent_view_projection_supported = supported;
            if !supported {
                endpoint.agent_view_projection = None;
                endpoint.pending_agent_view_projection = None;
            }
        }
    }

    pub(crate) fn set_endpoint_methods_for(
        &mut self,
        endpoint_id: &ClientEndpointId,
        methods: Option<Vec<String>>,
    ) {
        let methods = methods.map(|methods| methods.into_iter().collect::<HashSet<_>>());
        if let Some(endpoint) = self
            .endpoints
            .iter_mut()
            .find(|endpoint| &endpoint.endpoint_id == endpoint_id)
        {
            endpoint.methods = methods;
        }
    }

    pub(crate) fn set_endpoint_capabilities_for(
        &mut self,
        endpoint_id: &ClientEndpointId,
        capabilities: Vec<String>,
    ) {
        if let Some(endpoint) = self
            .endpoints
            .iter_mut()
            .find(|endpoint| &endpoint.endpoint_id == endpoint_id)
        {
            endpoint.capabilities = Some(capabilities.into_iter().collect());
        }
    }

    pub(super) fn supports_endpoint_capability(&self, capability: &str) -> bool {
        self.endpoints
            .iter()
            .find(|endpoint| endpoint.endpoint_id == self.active_endpoint_id)
            .and_then(|endpoint| endpoint.capabilities.as_ref())
            .is_some_and(|capabilities| capabilities.contains(capability))
    }

    pub(crate) fn endpoint_projection_available(&self, endpoint_id: &ClientEndpointId) -> bool {
        self.endpoints.iter().any(|endpoint| {
            &endpoint.endpoint_id == endpoint_id
                && endpoint.status == ClientEndpointStatus::Online
                && endpoint.snapshot.is_some()
        })
    }

    pub(crate) fn activate_endpoint_projection(&mut self, endpoint_id: &ClientEndpointId) -> bool {
        let Some(endpoint) = self
            .endpoints
            .iter()
            .find(|endpoint| &endpoint.endpoint_id == endpoint_id)
        else {
            return false;
        };
        if endpoint.status != ClientEndpointStatus::Online {
            return false;
        }
        let Some(snapshot) = endpoint.snapshot.clone() else {
            return false;
        };
        let endpoint_boot_id = snapshot.boot_id.clone();
        if endpoint_id != &self.active_endpoint_id {
            let previous_boot_id = self
                .endpoints
                .iter()
                .find(|candidate| candidate.endpoint_id == self.active_endpoint_id)
                .and_then(|candidate| {
                    candidate
                        .snapshot
                        .as_ref()
                        .map(|snapshot| snapshot.boot_id.clone())
                });
            if let Some(previous_boot_id) = previous_boot_id {
                let pending = self
                    .pending_requests
                    .iter()
                    .filter(|(_, request)| request.boot_id == previous_boot_id)
                    .map(|(request_id, _)| request_id.clone())
                    .collect::<Vec<_>>();
                for request_id in pending {
                    self.cancel_endpoint_request(&request_id);
                }
            }
            self.active_endpoint_id = endpoint_id.clone();
            self.inspector = None;
            self.last_pane_viewport_request = None;
            if matches!(
                self.overlay.as_ref(),
                Some(
                    ClientShellOverlay::PaneInterruptConfirm(_) | ClientShellOverlay::Extensions(_)
                )
            ) {
                self.overlay = None;
            }
            // Fence any response sent by the previous server after its requests
            // were removed above. The new endpoint may reuse request IDs, but not
            // the previous endpoint boot identity.
            self.pending_requests
                .retain(|_, request| request.boot_id == endpoint_boot_id);
            self.pane_surface = None;
            self.pending_pane_surface = None;
        }
        self.apply_active_snapshot(snapshot);
        self.apply_cached_endpoint_structure(endpoint_id);
        true
    }

    pub(crate) fn endpoint_status(
        &self,
        endpoint_id: &ClientEndpointId,
    ) -> Option<ClientEndpointStatus> {
        self.endpoints
            .iter()
            .find(|endpoint| &endpoint.endpoint_id == endpoint_id)
            .map(|endpoint| endpoint.status)
    }

    pub(crate) fn endpoint_has_snapshot(&self, endpoint_id: &ClientEndpointId) -> bool {
        self.endpoints
            .iter()
            .find(|endpoint| &endpoint.endpoint_id == endpoint_id)
            .is_some_and(|endpoint| endpoint.snapshot.is_some())
    }

    pub(crate) fn endpoint_is_online(&self, endpoint_id: &ClientEndpointId) -> bool {
        self.endpoint_status(endpoint_id) == Some(ClientEndpointStatus::Online)
            && self.endpoint_has_snapshot(endpoint_id)
    }

    pub(crate) fn endpoint_boot_id(&self, endpoint_id: &ClientEndpointId) -> Option<&str> {
        self.endpoints
            .iter()
            .find(|endpoint| &endpoint.endpoint_id == endpoint_id)?
            .snapshot
            .as_deref()
            .map(|snapshot| snapshot.boot_id.as_str())
    }

    pub(crate) fn endpoint_snapshot_matches(
        &self,
        endpoint_id: &ClientEndpointId,
        generation: u64,
        boot_id: &str,
        revision: u64,
    ) -> bool {
        self.endpoints
            .iter()
            .find(|endpoint| &endpoint.endpoint_id == endpoint_id)
            .is_some_and(|endpoint| {
                endpoint
                    .snapshot_generation
                    .is_none_or(|snapshot_generation| snapshot_generation == generation)
                    && endpoint.snapshot.as_deref().is_some_and(|snapshot| {
                        snapshot.boot_id == boot_id && snapshot.revision == revision
                    })
            })
    }

    pub(crate) fn endpoint_snapshot_identity(
        &self,
        endpoint_id: &ClientEndpointId,
        generation: u64,
    ) -> Option<(&str, u64)> {
        let endpoint = self
            .endpoints
            .iter()
            .find(|endpoint| &endpoint.endpoint_id == endpoint_id)?;
        if endpoint
            .snapshot_generation
            .is_some_and(|snapshot_generation| snapshot_generation != generation)
        {
            return None;
        }
        endpoint
            .snapshot
            .as_deref()
            .map(|snapshot| (snapshot.boot_id.as_str(), snapshot.revision))
    }

    pub(crate) fn set_endpoint_agent_view_projection_for_generation(
        &mut self,
        endpoint_id: &ClientEndpointId,
        generation: u64,
        projection: crate::client::endpoint::DecodedAgentViewProjection,
    ) {
        self.set_endpoint_agent_view_projection(
            endpoint_id,
            Some(generation),
            projection.boot_id,
            projection.revision,
            projection.view,
        );
    }

    #[cfg(test)]
    pub(crate) fn set_test_endpoint_agent_view(
        &mut self,
        endpoint_id: &ClientEndpointId,
        view: Option<crate::api::schema::AgentViewSetParams>,
    ) {
        let Some((boot_id, revision)) = self
            .endpoints
            .iter()
            .find(|endpoint| &endpoint.endpoint_id == endpoint_id)
            .and_then(|endpoint| {
                endpoint
                    .snapshot
                    .as_deref()
                    .map(|snapshot| (snapshot.boot_id.clone(), snapshot.revision))
            })
        else {
            return;
        };
        self.set_endpoint_agent_view_projection_supported(endpoint_id, true);
        self.set_endpoint_agent_view_projection(endpoint_id, None, boot_id, revision, Ok(view));
    }

    #[cfg(test)]
    pub(crate) fn set_test_endpoint_agent_view_projection(
        &mut self,
        endpoint_id: &ClientEndpointId,
        boot_id: &str,
        revision: u64,
        view: Option<crate::api::schema::AgentViewSetParams>,
    ) {
        self.set_endpoint_agent_view_projection(
            endpoint_id,
            None,
            boot_id.to_owned(),
            revision,
            Ok(view),
        );
    }

    fn set_endpoint_agent_view_projection(
        &mut self,
        endpoint_id: &ClientEndpointId,
        generation: Option<u64>,
        boot_id: String,
        revision: u64,
        view: Result<Option<crate::api::schema::AgentViewSetParams>, ()>,
    ) {
        let Some(endpoint) = self
            .endpoints
            .iter_mut()
            .find(|endpoint| &endpoint.endpoint_id == endpoint_id)
        else {
            return;
        };
        if endpoint.snapshot_generation == generation
            && endpoint
                .snapshot
                .as_deref()
                .is_some_and(|snapshot| snapshot.boot_id == boot_id && snapshot.revision > revision)
        {
            return;
        }
        let next = ClientEndpointAgentViewProjection {
            generation,
            boot_id,
            revision,
            view,
        };
        let matches_snapshot = endpoint.snapshot_generation == next.generation
            && endpoint.snapshot.as_deref().is_some_and(|snapshot| {
                snapshot.boot_id == next.boot_id && snapshot.revision == next.revision
            });
        let slot = if matches_snapshot {
            &mut endpoint.agent_view_projection
        } else {
            &mut endpoint.pending_agent_view_projection
        };
        if slot.as_ref().is_some_and(|current| {
            current.generation == next.generation
                && current.boot_id == next.boot_id
                && current.revision >= next.revision
        }) {
            return;
        }
        *slot = Some(next);
    }

    pub(crate) fn endpoint_agent_view(
        endpoint: &ClientShellEndpoint,
    ) -> Option<&Result<Option<crate::api::schema::AgentViewSetParams>, ()>> {
        let snapshot = endpoint.snapshot.as_deref()?;
        let projection = endpoint.agent_view_projection.as_ref()?;
        (projection.generation == endpoint.snapshot_generation
            && projection.boot_id == snapshot.boot_id
            && projection.revision == snapshot.revision)
            .then_some(&projection.view)
    }

    /// A terminal normally starts focused. `None` means this host cannot report focus events,
    /// not that the endpoint has no viewer; activation therefore sends an explicit true baseline.
    pub(crate) fn host_focus_baseline(&self) -> bool {
        self.outer_focused.unwrap_or(true)
    }

    pub(crate) fn endpoint_label(&self, endpoint_id: &ClientEndpointId) -> &str {
        self.endpoints
            .iter()
            .find(|endpoint| &endpoint.endpoint_id == endpoint_id)
            .map_or("Unknown endpoint", |endpoint| endpoint.label.as_str())
    }

    pub(crate) fn active_endpoint_label(&self) -> &str {
        self.endpoint_label(&self.active_endpoint_id)
    }

    pub(crate) fn endpoint_is_active(&self, endpoint_id: &ClientEndpointId) -> bool {
        &self.active_endpoint_id == endpoint_id
    }

    pub(crate) fn multi_endpoint_active(&self) -> bool {
        self.endpoints.len() > 1
    }

    #[cfg(test)]
    pub(crate) fn set_snapshot(&mut self, snapshot: Box<ClientShellSnapshot>) {
        let endpoint_id = self.active_endpoint_id.clone();
        self.set_endpoint_snapshot(&endpoint_id, snapshot);
    }

    #[cfg(test)]
    pub(crate) fn set_endpoint_methods(&mut self, methods: Option<Vec<String>>) {
        let endpoint_id = self.active_endpoint_id.clone();
        self.set_endpoint_methods_for(&endpoint_id, methods);
    }

    pub(super) fn supports_endpoint_method(&self, method: &crate::api::schema::Method) -> bool {
        self.endpoints
            .iter()
            .find(|endpoint| endpoint.endpoint_id == self.active_endpoint_id)
            .and_then(|endpoint| endpoint.methods.as_ref())
            .is_none_or(|methods| methods.contains(crate::api::api_method_name(method)))
    }

    pub(super) fn focused_tab_count(&self) -> usize {
        let Some(snapshot) = self.snapshot.as_deref() else {
            return 0;
        };
        snapshot
            .tabs
            .iter()
            .filter(|tab| {
                Some(tab.workspace_id.as_str()) == snapshot.focused_workspace_id.as_deref()
            })
            .count()
    }

    #[cfg(test)]
    pub(crate) fn cache_endpoint_snapshot(
        &mut self,
        endpoint_id: &ClientEndpointId,
        snapshot: Box<ClientShellSnapshot>,
    ) {
        self.cache_endpoint_snapshot_with_surface(endpoint_id, None, snapshot, true);
    }

    pub(crate) fn cache_endpoint_snapshot_for_generation(
        &mut self,
        endpoint_id: &ClientEndpointId,
        generation: u64,
        snapshot: Box<ClientShellSnapshot>,
    ) {
        self.cache_endpoint_snapshot_with_surface(endpoint_id, Some(generation), snapshot, true);
    }

    /// Metadata delivered while an endpoint surface is inactive must never advance this
    /// aggregate client's viewed watermark, even if the frozen source frame still exists.
    pub(crate) fn cache_endpoint_snapshot_inactive_for_generation(
        &mut self,
        endpoint_id: &ClientEndpointId,
        generation: u64,
        snapshot: Box<ClientShellSnapshot>,
    ) {
        self.cache_endpoint_snapshot_with_surface(endpoint_id, Some(generation), snapshot, false);
    }

    fn cache_endpoint_snapshot_with_surface(
        &mut self,
        endpoint_id: &ClientEndpointId,
        generation: Option<u64>,
        mut snapshot: Box<ClientShellSnapshot>,
        acknowledge_surface: bool,
    ) {
        snapshot
            .commands
            .retain(|command| command.action != crate::protocol::ClientShellCommandAction::Unknown);
        let Some(index) = self
            .endpoints
            .iter()
            .position(|endpoint| &endpoint.endpoint_id == endpoint_id)
        else {
            return;
        };
        if matches!(
            (self.endpoints[index].snapshot_generation, generation),
            (Some(current), Some(incoming)) if incoming < current
        ) {
            return;
        }
        if self.endpoints[index].snapshot_generation == generation
            && self.endpoints[index]
                .snapshot
                .as_deref()
                .is_some_and(|previous| {
                    previous.boot_id == snapshot.boot_id && previous.revision > snapshot.revision
                })
        {
            return;
        }
        let boot_changed = self.endpoints[index]
            .snapshot
            .as_deref()
            .is_some_and(|previous| previous.boot_id != snapshot.boot_id);
        if boot_changed {
            self.retire_endpoint_notifications(endpoint_id);
        }
        self.endpoints[index]
            .agent_presentation
            .project_snapshot(&mut snapshot);
        let presented_surface = if acknowledge_surface && endpoint_id == &self.active_endpoint_id {
            self.pane_surface.as_ref()
        } else {
            None
        };
        if let Some(surface) = presented_surface {
            self.endpoints[index]
                .agent_presentation
                .acknowledge_surface(&mut snapshot, surface, self.outer_focused);
        }
        let previous = self.endpoints[index].snapshot.as_deref();
        let mut next_recency = self
            .endpoints
            .iter()
            .flat_map(|endpoint| endpoint.agent_recency.values())
            .copied()
            .max()
            .unwrap_or_default();
        let mut agents = snapshot.agents.iter().collect::<Vec<_>>();
        agents.sort_by_key(|agent| agent.state_change_seq);
        let mut recency = self.endpoints[index].agent_recency.clone();
        for agent in agents {
            let changed = previous
                .and_then(|snapshot| {
                    snapshot
                        .agents
                        .iter()
                        .find(|previous| previous.pane_id == agent.pane_id)
                })
                .is_none_or(|previous| previous.state_change_seq != agent.state_change_seq);
            if changed {
                next_recency = next_recency.saturating_add(1);
                recency.insert(agent.pane_id.clone(), next_recency);
            }
        }
        recency.retain(|pane_id, _| {
            snapshot
                .agents
                .iter()
                .any(|agent| &agent.pane_id == pane_id)
        });
        let endpoint = &mut self.endpoints[index];
        endpoint.agent_recency = recency;
        endpoint.snapshot_generation = generation;
        endpoint.snapshot = Some(snapshot);
        let pending_matches =
            endpoint
                .pending_agent_view_projection
                .as_ref()
                .is_some_and(|projection| {
                    projection.generation == generation
                        && endpoint.snapshot.as_deref().is_some_and(|snapshot| {
                            projection.boot_id == snapshot.boot_id
                                && projection.revision == snapshot.revision
                        })
                });
        if pending_matches {
            endpoint.agent_view_projection = endpoint.pending_agent_view_projection.take();
        } else {
            endpoint.pending_agent_view_projection = None;
            if endpoint
                .agent_view_projection
                .as_ref()
                .is_some_and(|projection| {
                    projection.generation != generation
                        || endpoint.snapshot.as_deref().is_some_and(|snapshot| {
                            projection.boot_id != snapshot.boot_id
                                || projection.revision != snapshot.revision
                        })
                })
            {
                endpoint.agent_view_projection = None;
            }
        }
        self.refresh_recovery_overlay(endpoint_id);
    }

    pub(crate) fn acknowledge_active_surface_agents(&mut self, surface: &PaneSurfaceFrame) -> bool {
        let Some(index) = self
            .endpoints
            .iter()
            .position(|endpoint| endpoint.endpoint_id == self.active_endpoint_id)
        else {
            return false;
        };
        let changed = {
            let endpoint = &mut self.endpoints[index];
            let Some(snapshot) = endpoint.snapshot.as_deref_mut() else {
                return false;
            };
            endpoint
                .agent_presentation
                .acknowledge_surface(snapshot, surface, self.outer_focused)
        };
        if changed {
            self.snapshot = self.endpoints[index].snapshot.clone();
        }
        changed
    }

    #[cfg(test)]
    pub(crate) fn set_endpoint_snapshot(
        &mut self,
        endpoint_id: &ClientEndpointId,
        snapshot: Box<ClientShellSnapshot>,
    ) {
        self.cache_endpoint_snapshot(endpoint_id, snapshot);
        self.apply_cached_endpoint_snapshot(endpoint_id);
    }

    pub(crate) fn set_endpoint_snapshot_for_generation(
        &mut self,
        endpoint_id: &ClientEndpointId,
        generation: u64,
        snapshot: Box<ClientShellSnapshot>,
    ) {
        let replaces_active_connection = endpoint_id == &self.active_endpoint_id
            && self
                .endpoints
                .iter()
                .find(|endpoint| &endpoint.endpoint_id == endpoint_id)
                .and_then(|endpoint| endpoint.snapshot_generation)
                .is_some_and(|current| generation > current);
        let preserve_navigation_mode =
            replaces_active_connection && self.mode == ClientShellMode::Navigate;
        self.cache_endpoint_snapshot_for_generation(endpoint_id, generation, snapshot);
        if replaces_active_connection {
            let navigation_preview = self.navigate_workspace_id.take();
            self.reset_endpoint_projection();
            self.navigate_workspace_id = navigation_preview;
            self.snapshot = None;
        }
        self.apply_cached_endpoint_snapshot(endpoint_id);
        if preserve_navigation_mode && self.navigate_workspace_id.is_some() {
            self.mode = ClientShellMode::Navigate;
        }
    }

    pub(crate) fn set_terminal_views_surface_for_generation(
        &mut self,
        endpoint_id: &ClientEndpointId,
        generation: u64,
        surface: crate::protocol::endpoint::ServerTerminalViewsSurface,
    ) -> bool {
        if endpoint_id != &self.active_endpoint_id
            || !self.endpoint_snapshot_matches(
                endpoint_id,
                generation,
                &surface.boot_id,
                surface.projection_revision,
            )
            || surface.views.len() > crate::protocol::endpoint::MAX_TERMINAL_VIEWS
        {
            return false;
        }
        let Some(interest) = self
            .last_terminal_views_interest
            .as_ref()
            .filter(|interest| {
                interest.boot_id == surface.boot_id
                    && interest.projection_revision == surface.projection_revision
            })
        else {
            return false;
        };
        let expected = interest
            .views
            .iter()
            .map(|view| (view.pane_id.as_str(), view.size))
            .collect::<HashMap<_, _>>();
        let mut next = HashMap::new();
        let mut total_cells = 0usize;
        for view in surface.views {
            let Some(size) = expected.get(view.pane_id.as_str()) else {
                return false;
            };
            let expected_cells = usize::from(view.frame.width) * usize::from(view.frame.height);
            if view.frame.width != size.cols
                || view.frame.height != size.rows
                || view.frame.cells.len() != expected_cells
            {
                return false;
            }
            total_cells = total_cells.saturating_add(expected_cells);
            if total_cells > crate::protocol::endpoint::MAX_TERMINAL_VIEW_CELLS
                || next.insert(view.pane_id.clone(), view).is_some()
            {
                return false;
            }
        }
        if surface.replace {
            self.terminal_view_surfaces = next;
        } else {
            self.terminal_view_surfaces.extend(next);
        }
        true
    }

    pub(super) fn host_document_action_for_pane(
        &self,
        pane_id: &str,
    ) -> Option<crate::api::schema::HostDocumentActionParams> {
        let projection = self
            .endpoints
            .iter()
            .find(|endpoint| endpoint.endpoint_id == self.active_endpoint_id)?
            .current_host_documents()?;
        let entry = projection
            .documents
            .iter()
            .filter(|entry| {
                entry.document.items.iter().any(|item| {
                    item.terminal
                        .as_ref()
                        .and_then(|terminal| terminal.placement.as_ref())
                        .is_some_and(|placement| placement.pane_id == pane_id)
                })
            })
            .max_by(|left, right| {
                left.provider_priority
                    .cmp(&right.provider_priority)
                    .then_with(|| right.document.namespace.cmp(&left.document.namespace))
            })?;
        let document = &entry.document;
        let item = document.items.iter().find(|item| {
            !item.flags.disabled
                && item
                    .terminal
                    .as_ref()
                    .and_then(|terminal| terminal.placement.as_ref())
                    .is_some_and(|placement| placement.pane_id == pane_id)
        })?;
        let action_id = item.action_ids.iter().find(|action_id| {
            document.actions.iter().any(|action| {
                action.id == action_id.as_str()
                    && action
                        .subject_id
                        .as_deref()
                        .is_none_or(|subject_id| subject_id == item.id)
            })
        })?;
        Some(crate::api::schema::HostDocumentActionParams {
            namespace: document.namespace.clone(),
            scope_id: match &document.target {
                crate::extension_contract::HostTarget::Tab { tab_id } => tab_id.clone(),
                _ => return None,
            },
            generation: document.generation,
            revision: document.revision,
            action_id: action_id.to_string(),
            confirmed: false,
            confirmation_token: entry.confirmation_tokens.get(action_id.as_str()).cloned(),
        })
    }

    pub(super) fn route_host_document_action(
        &mut self,
        mut params: crate::api::schema::HostDocumentActionParams,
        fallback: crate::api::schema::Method,
        outcome: &mut ClientShellInput,
    ) {
        let method = crate::api::schema::Method::HostDocumentAction(params.clone());
        if !self.supports_endpoint_method(&method) {
            self.push_endpoint_method(fallback, outcome);
            return;
        }
        if self.host_document_action_requires_confirmation(&params) {
            params.confirmed = true;
            self.overlay = Some(ClientShellOverlay::ConfirmClose(
                ClientConfirmCloseOverlay {
                    target: ClientConfirmCloseTarget::HostDocumentAction(params),
                    title: "Confirm extension action?".into(),
                    detail: "This action can interrupt a terminal process and cannot be undone."
                        .into(),
                },
            ));
            outcome.repaint = true;
        } else {
            self.push_endpoint_method(
                crate::api::schema::Method::HostDocumentAction(params),
                outcome,
            );
        }
    }

    pub(super) fn host_document_action_requires_confirmation(
        &self,
        params: &crate::api::schema::HostDocumentActionParams,
    ) -> bool {
        self.endpoints
            .iter()
            .find(|endpoint| endpoint.endpoint_id == self.active_endpoint_id)
            .and_then(ClientShellEndpoint::current_host_documents)
            .and_then(|projection| {
                projection.documents.iter().find_map(|entry| {
                    let document = &entry.document;
                    (document.namespace == params.namespace
                        && document.generation == params.generation
                        && document.revision == params.revision)
                        .then(|| {
                            document
                                .actions
                                .iter()
                                .find(|action| action.id == params.action_id)
                        })
                        .flatten()
                })
            })
            .is_some_and(|action| {
                action.confirmation == crate::extension_contract::HostConfirmation::Required
            })
    }

    pub(crate) fn set_endpoint_host_documents_for_generation(
        &mut self,
        endpoint_id: &ClientEndpointId,
        generation: u64,
        projection: Box<crate::protocol::endpoint::HostDocumentsProjectionV1>,
    ) {
        let Some(endpoint) = self
            .endpoints
            .iter_mut()
            .find(|endpoint| &endpoint.endpoint_id == endpoint_id)
        else {
            return;
        };
        if endpoint
            .host_documents_generation
            .is_some_and(|current| generation < current)
        {
            return;
        }
        let stale = endpoint.host_documents_generation == Some(generation)
            && endpoint.host_documents.as_ref().is_some_and(|current| {
                current.boot_id == projection.boot_id && current.revision > projection.revision
            });
        if !stale {
            endpoint.host_documents = Some(projection);
            endpoint.host_documents_generation = Some(generation);
        }
    }

    pub(crate) fn set_endpoint_structure_for_generation(
        &mut self,
        endpoint_id: &ClientEndpointId,
        generation: u64,
        projection: Box<crate::protocol::endpoint::ClientShellStructureProjection>,
    ) {
        let Some(endpoint) = self
            .endpoints
            .iter_mut()
            .find(|endpoint| &endpoint.endpoint_id == endpoint_id)
        else {
            return;
        };
        if endpoint
            .structure_generation
            .is_some_and(|current| generation < current)
        {
            return;
        }
        let stale_or_mismatched = endpoint.structure_generation == Some(generation)
            && endpoint.structure.as_ref().is_some_and(|current| {
                current.boot_id == projection.boot_id
                    && (current.revision > projection.revision
                        || current.session_id != projection.session_id)
            });
        if stale_or_mismatched {
            return;
        }
        endpoint.structure = Some(projection);
        endpoint.structure_generation = Some(generation);
        if endpoint_id == &self.active_endpoint_id {
            self.apply_cached_endpoint_structure(endpoint_id);
        }
    }

    fn apply_cached_endpoint_structure(&mut self, endpoint_id: &ClientEndpointId) {
        let matching = self
            .endpoints
            .iter()
            .find(|endpoint| &endpoint.endpoint_id == endpoint_id)
            .and_then(|endpoint| endpoint.current_structure().cloned().map(Box::new));
        if let Some(structure) = matching.as_ref() {
            self.collection_list_views.retain(|collection_id| {
                structure
                    .collections
                    .iter()
                    .any(|collection| &collection.collection_id == collection_id)
            });
            let members = structure
                .collections
                .iter()
                .flat_map(|collection| collection.members.iter())
                .map(|member| member.pane_id.as_str())
                .collect::<HashSet<_>>();
            self.collection_expanded_members
                .retain(|collection_id, expanded| {
                    let valid_collection = structure
                        .collections
                        .iter()
                        .any(|collection| &collection.collection_id == collection_id);
                    expanded.retain(|pane_id| members.contains(pane_id.as_str()));
                    valid_collection
                });
            self.collection_preview_heights
                .retain(|pane_id, _| members.contains(pane_id.as_str()));
            if self
                .collection_entered_pane
                .as_deref()
                .is_some_and(|pane_id| {
                    !structure.collections.iter().any(|collection| {
                        self.collection_list_views
                            .contains(&collection.collection_id)
                            && collection.selected_pane_id.as_deref() == Some(pane_id)
                    })
                })
            {
                self.collection_entered_pane = None;
            }
            if self
                .collection_maximized_pane
                .as_ref()
                .is_some_and(|(collection_id, pane_id)| {
                    !self.collection_list_views.contains(collection_id)
                        || !members.contains(pane_id.as_str())
                        || !structure.collections.iter().any(|collection| {
                            &collection.collection_id == collection_id
                                && collection.selected_pane_id.as_deref() == Some(pane_id.as_str())
                        })
                })
            {
                self.collection_maximized_pane = None;
            }
        }
        self.structure = matching;
        let focused_repository = self.focused_repository_id();
        if let Some(ClientShellOverlay::Extensions(extensions)) = self.overlay.as_mut() {
            extensions.repository_context = focused_repository;
        }
    }

    fn apply_cached_endpoint_snapshot(&mut self, endpoint_id: &ClientEndpointId) {
        let Some(snapshot) = self
            .endpoints
            .iter()
            .find(|endpoint| &endpoint.endpoint_id == endpoint_id)
            .and_then(|endpoint| endpoint.snapshot.clone())
        else {
            return;
        };
        if endpoint_id == &self.active_endpoint_id {
            self.apply_active_snapshot(snapshot);
            self.apply_cached_endpoint_structure(endpoint_id);
        }
    }
}

pub(super) fn endpoint_status_presentation(
    status: ClientEndpointStatus,
    palette: &Palette,
) -> (&'static str, &'static str, ratatui::style::Color) {
    match status {
        ClientEndpointStatus::Connecting => ("◐", "connecting", palette.yellow),
        ClientEndpointStatus::Online => ("●", "online", palette.green),
        ClientEndpointStatus::Reconnecting => ("◐", "reconnecting", palette.yellow),
        ClientEndpointStatus::Attention => ("!", "attention", palette.red),
        ClientEndpointStatus::Disabled => ("·", "disabled", palette.overlay0),
    }
}

pub(super) fn local_endpoint() -> ClientShellEndpoint {
    ClientShellEndpoint {
        endpoint_id: ClientEndpointId::Local,
        label: "Local".into(),
        status: ClientEndpointStatus::Online,
        snapshot: None,
        snapshot_generation: None,
        structure: None,
        structure_generation: None,
        host_documents: None,
        host_documents_generation: None,
        agent_recency: HashMap::new(),
        agent_presentation: Default::default(),
        agent_view_projection: None,
        pending_agent_view_projection: None,
        agent_view_projection_supported: false,
        methods: None,
        capabilities: None,
    }
}
