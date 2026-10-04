use super::*;

fn secure_confirmation_token() -> std::io::Result<String> {
    use base64::Engine as _;
    let mut bytes = [0u8; 32];
    crate::platform::fill_secure_random(&mut bytes)?;
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes))
}

impl HeadlessServer {
    pub(super) fn render_terminal_views(
        &self,
        target: crate::ui::TabSurfaceTarget,
        projection_revision: u64,
        interest: &crate::protocol::endpoint::ClientTerminalViewsInterest,
        cell_size: crate::kitty_graphics::HostCellSize,
        sources: Option<&HashSet<crate::layout::PaneId>>,
    ) -> crate::protocol::endpoint::ServerTerminalViewsSurface {
        let tab = self
            .app
            .state
            .workspaces
            .get(target.workspace_index)
            .and_then(|workspace| workspace.tabs.get(target.tab_index));
        let allowed = tab
            .map(|tab| tab.layout.pane_ids().into_iter().collect::<HashSet<_>>())
            .unwrap_or_default();
        let views = interest
            .views
            .iter()
            .filter_map(|view| {
                let (workspace_index, pane_id) = self.app.parse_pane_id(&view.pane_id)?;
                if workspace_index != target.workspace_index
                    || !allowed.contains(&pane_id)
                    || sources.is_some_and(|sources| !sources.contains(&pane_id))
                {
                    return None;
                }
                let runtime = self.app.state.runtime_for_pane_in_workspace(
                    &self.app.terminal_runtimes,
                    workspace_index,
                    pane_id,
                )?;
                let area = Rect::new(0, 0, view.size.cols, view.size.rows);
                let (buffer, cursor) =
                    crate::server::render_stream::render_terminal_virtual(runtime, area);
                let hyperlinks = runtime.visible_hyperlinks(area);
                Some(crate::protocol::endpoint::ServerTerminalViewSurface {
                    pane_id: view.pane_id.clone(),
                    frame: FrameData::from_ratatui_buffer_with_hyperlinks(
                        &buffer,
                        cursor,
                        &hyperlinks,
                    ),
                    scroll: runtime.scroll_metrics().map(|metrics| {
                        crate::protocol::PaneSurfaceScrollMetrics {
                            offset_from_bottom: metrics.offset_from_bottom as u64,
                            max_offset_from_bottom: metrics.max_offset_from_bottom as u64,
                            viewport_rows: metrics.viewport_rows as u64,
                        }
                    }),
                    mouse_reporting: runtime.mouse_reporting_enabled(),
                    sgr_pixel_mouse: runtime.sgr_pixel_mouse_enabled(),
                    pixel_width: u32::from(view.size.cols) * cell_size.width_px,
                    pixel_height: u32::from(view.size.rows) * cell_size.height_px,
                })
            })
            .collect();
        crate::protocol::endpoint::ServerTerminalViewsSurface {
            boot_id: self.client_shell_boot_id.clone(),
            projection_revision,
            replace: sources.is_none(),
            views,
        }
    }

    fn shell_focused_runtime(
        &self,
        client_id: u64,
    ) -> Option<(&crate::terminal::TerminalRuntime, crate::layout::PaneId)> {
        if self.popup_owner_tab_id == self.shell_tab_id_for_client(client_id) {
            if let Some(popup) = &self.app.state.popup_pane {
                return self
                    .app
                    .terminal_runtimes
                    .get(&popup.terminal_id)
                    .map(|runtime| (runtime, popup.pane_id));
            }
        }
        let target = self.shell_target_for_client(client_id)?;
        let tab = self
            .app
            .state
            .workspaces
            .get(target.workspace_index)?
            .tabs
            .get(target.tab_index)?;
        let pane_id = tab.layout.focused();
        self.app
            .state
            .runtime_for_pane_in_workspace(
                &self.app.terminal_runtimes,
                target.workspace_index,
                pane_id,
            )
            .map(|runtime| (runtime, pane_id))
    }

    pub(super) fn stream_host_mouse_capture_mode(&mut self) {
        let requested = self
            .clients
            .iter()
            .filter_map(|(&client_id, client)| match &client.mode {
                ClientConnectionMode::ClientShell => {
                    let focused = client
                        .shell_surface_active
                        .then(|| self.shell_focused_runtime(client_id))
                        .flatten();
                    let child_requests_mouse =
                        focused.is_some_and(|(runtime, _)| runtime.mouse_reporting_enabled());
                    let sgr_pixels = client.pixel_mouse
                        && focused.is_some_and(|(runtime, _)| runtime.sgr_pixel_mouse_enabled());
                    Some((
                        client_id,
                        client.shell_surface_active
                            && (client.shell_mouse_capture || child_requests_mouse),
                        client.shell_surface_active && sgr_pixels,
                    ))
                }
                ClientConnectionMode::TerminalAttach { terminal_id } => {
                    let runtime = self.runtime_for_terminal_id_string(terminal_id);
                    let child_requests_mouse = runtime
                        .is_some_and(crate::terminal::TerminalRuntime::mouse_reporting_enabled);
                    let sgr_pixels = child_requests_mouse
                        && client.pixel_mouse
                        && runtime
                            .is_some_and(crate::terminal::TerminalRuntime::sgr_pixel_mouse_enabled);
                    Some((client_id, child_requests_mouse, sgr_pixels))
                }
                ClientConnectionMode::TerminalPending
                | ClientConnectionMode::TerminalObserve { .. } => None,
            })
            .collect::<Vec<_>>();

        let mut broken_clients = Vec::new();
        for (client_id, enabled, sgr_pixels) in requested {
            let Some(client) = self.clients.get_mut(&client_id) else {
                continue;
            };
            if client.host_mouse_capture_active == Some(enabled)
                && client.host_sgr_pixels_active == Some(sgr_pixels)
            {
                continue;
            }
            let Some(writer) = &client.writer else {
                continue;
            };
            let serialized = match Self::frame_server_message(&ServerMessage::MouseCapture {
                enabled,
                sgr_pixels,
            }) {
                Ok(framed) => framed,
                Err(err) => {
                    warn!(err = %err, "failed to serialize mouse capture mode for client");
                    continue;
                }
            };
            if writer.control.send(serialized).is_err() {
                debug!(
                    client_id,
                    "client writer channel closed during mouse capture update"
                );
                broken_clients.push(client_id);
                continue;
            }
            client.host_mouse_capture_active = Some(enabled);
            client.host_sgr_pixels_active = Some(sgr_pixels);
        }

        for client_id in broken_clients {
            self.remove_client_and_resize_if_needed(client_id);
        }
    }

    pub(super) fn stream_direct_terminal_keyboard_mode(&mut self) {
        let shell_modes = self
            .clients
            .iter()
            .filter(|(_, client)| client.is_shell_client())
            .map(|(&client_id, client)| {
                let report_all = client.shell_surface_active
                    && self
                        .shell_focused_runtime(client_id)
                        .is_some_and(|(runtime, _)| {
                            let protocol = runtime.keyboard_protocol();
                            protocol.reports_all_keys()
                                || (protocol.reports_event_types()
                                    && runtime.modify_other_keys_level() > 0)
                        });
                (client_id, report_all)
            })
            .collect::<Vec<_>>();
        let mut broken_clients = Vec::new();
        for (client_id, report_all) in shell_modes {
            let Some(client) = self.clients.get_mut(&client_id) else {
                continue;
            };
            if client.host_keyboard_report_all_active == Some(report_all) {
                continue;
            }
            let Some(writer) = &client.writer else {
                continue;
            };
            let serialized = match Self::frame_server_message(
                &ServerMessage::ClientShellKeyboardReportAll {
                    enabled: report_all,
                },
            ) {
                Ok(serialized) => serialized,
                Err(err) => {
                    warn!(err = %err, "failed to serialize client shell keyboard report-all mode");
                    continue;
                }
            };
            if writer.control.send(serialized).is_err() {
                broken_clients.push(client_id);
                continue;
            }
            client.host_keyboard_report_all_active = Some(report_all);
        }

        let requested = self
            .clients
            .iter()
            .filter_map(|(&client_id, client)| {
                let ClientConnectionMode::TerminalAttach { terminal_id } = &client.mode else {
                    return None;
                };
                let (flags, modify_other_keys_level) = self
                    .runtime_for_terminal_id_string(terminal_id)
                    .map_or((0, 0), |runtime| {
                        let flags = match runtime.keyboard_protocol() {
                            crate::input::KeyboardProtocol::Legacy => 0,
                            crate::input::KeyboardProtocol::Kitty { flags } => flags,
                        };
                        (flags, runtime.modify_other_keys_level())
                    });
                Some((client_id, flags, modify_other_keys_level))
            })
            .collect::<Vec<_>>();

        for (client_id, flags, modify_other_keys_level) in requested {
            let Some(client) = self.clients.get_mut(&client_id) else {
                continue;
            };
            if client.host_keyboard_protocol_active == Some((flags, modify_other_keys_level)) {
                continue;
            }
            let Some(writer) = &client.writer else {
                continue;
            };
            let serialized =
                match Self::frame_server_message(&ServerMessage::DirectTerminalKeyboardProtocol {
                    flags,
                    modify_other_keys_level,
                }) {
                    Ok(framed) => framed,
                    Err(err) => {
                        warn!(err = %err, "failed to serialize direct terminal keyboard mode");
                        continue;
                    }
                };
            if writer.control.send(serialized).is_err() {
                debug!(
                    client_id,
                    "client writer channel closed during direct terminal keyboard update"
                );
                broken_clients.push(client_id);
                continue;
            }
            client.host_keyboard_protocol_active = Some((flags, modify_other_keys_level));
        }

        for client_id in broken_clients {
            self.remove_client_and_resize_if_needed(client_id);
        }
    }

    pub(super) fn has_pending_presentation_work(
        &self,
        needs_full_render: bool,
        needs_graphics_render: bool,
    ) -> bool {
        needs_full_render || needs_graphics_render || self.app.render_dirty.has_immediate_work()
    }

    pub(super) fn sync_immediate_pty_sources(&self) {
        let (has_app_target, direct_terminal_targets) = self.pty_render_targets();
        let mut pane_ids = HashSet::new();
        if has_app_target {
            for (&client_id, client) in &self.clients {
                if !client.is_active_shell_client() || client.writer.is_none() {
                    continue;
                }
                let Some(target) = self.shell_target_for_client(client_id) else {
                    continue;
                };
                let Some(tab) = self
                    .app
                    .state
                    .workspaces
                    .get(target.workspace_index)
                    .and_then(|workspace| workspace.tabs.get(target.tab_index))
                else {
                    continue;
                };
                if tab.zoomed {
                    pane_ids.insert(tab.layout.focused());
                } else {
                    pane_ids.extend(
                        tab.layout
                            .panes(Rect::new(0, 0, 1, 1))
                            .into_iter()
                            .map(|pane| pane.id),
                    );
                }
                if let Some(interest) = client.shell_terminal_views_interest.as_ref() {
                    pane_ids.extend(interest.views.iter().filter_map(|view| {
                        self.app
                            .parse_pane_id(&view.pane_id)
                            .filter(|(workspace_index, _)| {
                                *workspace_index == target.workspace_index
                            })
                            .map(|(_, pane_id)| pane_id)
                    }));
                }
                if self.popup_owner_tab_id == self.shell_tab_id_for_client(client_id) {
                    if let Some(popup) = &self.app.state.popup_pane {
                        pane_ids.insert(popup.pane_id);
                    }
                }
            }
        }
        if !direct_terminal_targets.is_empty() {
            for workspace in &self.app.state.workspaces {
                for tab in &workspace.tabs {
                    pane_ids.extend(tab.panes.iter().filter_map(|(&pane_id, pane)| {
                        direct_terminal_targets
                            .contains(pane.attached_terminal_id.as_str())
                            .then_some(pane_id)
                    }));
                }
            }
            if let Some(popup) = &self.app.state.popup_pane {
                if direct_terminal_targets.contains(popup.terminal_id.as_str()) {
                    pane_ids.insert(popup.pane_id);
                }
            }
        }
        self.app.render_dirty.set_immediate_pty_sources(pane_ids);
    }

    fn pty_render_targets(&self) -> (bool, HashSet<&str>) {
        let mut has_app_target = false;
        let mut direct_terminal_targets = HashSet::new();
        for client in self
            .clients
            .values()
            .filter(|client| client.writer.is_some())
        {
            match &client.mode {
                ClientConnectionMode::ClientShell if client.shell_surface_active => {
                    has_app_target = true;
                }
                ClientConnectionMode::ClientShell => {}
                ClientConnectionMode::TerminalAttach { terminal_id }
                | ClientConnectionMode::TerminalObserve { terminal_id } => {
                    direct_terminal_targets.insert(terminal_id.as_str());
                }
                ClientConnectionMode::TerminalPending => {}
            }
        }
        (has_app_target, direct_terminal_targets)
    }

    fn pty_source_visible_to_render_targets(
        &self,
        pane_id: crate::layout::PaneId,
        has_app_target: bool,
        direct_terminal_targets: &HashSet<&str>,
    ) -> bool {
        let terminal_id = self.terminal_id_for_pane(pane_id);
        (has_app_target && (terminal_id.is_none() || self.any_shell_surface_contains_pane(pane_id)))
            || terminal_id.is_none_or(|source| direct_terminal_targets.contains(source.as_str()))
    }

    pub(super) fn pty_sources_visible_to_any_render_target(
        &self,
        sources: &HashSet<crate::layout::PaneId>,
    ) -> bool {
        let (has_app_target, direct_terminal_targets) = self.pty_render_targets();
        if !has_app_target && direct_terminal_targets.is_empty() {
            return false;
        }

        sources.iter().copied().any(|pane_id| {
            self.pty_source_visible_to_render_targets(
                pane_id,
                has_app_target,
                &direct_terminal_targets,
            )
        })
    }

    fn terminal_id_for_pane(
        &self,
        pane_id: crate::layout::PaneId,
    ) -> Option<&crate::terminal::TerminalId> {
        if let Some(popup) = self
            .app
            .state
            .popup_pane
            .as_ref()
            .filter(|popup| popup.pane_id == pane_id)
        {
            return Some(&popup.terminal_id);
        }
        self.app
            .find_pane(pane_id)
            .map(|(_, pane)| &pane.attached_terminal_id)
    }

    fn any_shell_surface_contains_pane(&self, pane_id: crate::layout::PaneId) -> bool {
        self.clients.iter().any(|(&client_id, client)| {
            if !client.is_active_shell_client() || client.writer.is_none() {
                return false;
            }
            if self
                .app
                .state
                .popup_pane
                .as_ref()
                .is_some_and(|popup| popup.pane_id == pane_id)
            {
                return self.popup_owner_tab_id == self.shell_tab_id_for_client(client_id);
            }
            let Some(target) = self.shell_target_for_client(client_id) else {
                return false;
            };
            let Some(tab) = self
                .app
                .state
                .workspaces
                .get(target.workspace_index)
                .and_then(|workspace| workspace.tabs.get(target.tab_index))
            else {
                return false;
            };
            if !tab.panes.contains_key(&pane_id) {
                return false;
            }
            let terminal_view_visible =
                client
                    .shell_terminal_views_interest
                    .as_ref()
                    .is_some_and(|interest| {
                        interest.views.iter().any(|view| {
                            self.app.parse_pane_id(&view.pane_id)
                                == Some((target.workspace_index, pane_id))
                        })
                    });
            let collection_selected = tab
                .layout
                .collections()
                .find(|collection| collection.members().contains(&pane_id))
                .is_none_or(|collection| collection.selected() == Some(pane_id));
            terminal_view_visible
                || (collection_selected && (!tab.zoomed || tab.layout.focused() == pane_id))
        })
    }

    pub(super) fn handle_extension_service_result(
        &mut self,
        result: crate::extension_service::ExtensionAsyncResult,
    ) {
        let client_id = result.identity.client_id;
        if result.holds_work_slot {
            crate::extension_service::ExtensionServiceManager::consume_completion_slot();
        }
        let manager_is_current = self
            .app
            .extension_services
            .matches_manager_generation(result.manager_generation);
        if manager_is_current {
            self.app.extension_services.complete(&result.identity);
        }
        let generation_is_current = manager_is_current
            && self
                .app
                .extension_services
                .matches_generation(&result.service_id, result.service_generation);
        match result.kind {
            crate::extension_service::ExtensionAsyncResultKind::Render(rendered) => {
                let pending = self
                    .clients
                    .get(&client_id)
                    .and_then(|client| client.pending_pane_view.clone());
                let Some(pending) = pending else {
                    return;
                };
                let identity_matches = pending.service_id == result.service_id
                    && pending.service_generation == result.service_generation
                    && pending.request.boot_id == result.identity.boot_id
                    && pending.request.projection_revision == result.identity.projection_revision
                    && pending.request.request_id == result.identity.request_id
                    && pending.request.pane_id == result.identity.target_pane_id
                    && pending.request.contribution_revision
                        == result.identity.contribution_revision
                    && pending.request.boot_id == self.client_shell_boot_id
                    && self.clients.get(&client_id).is_some_and(|client| {
                        client.shell_projection_revision == result.identity.projection_revision
                    });
                if !identity_matches {
                    return;
                }
                let mut response = crate::protocol::endpoint::ServerPaneViewResult {
                    boot_id: result.identity.boot_id.clone(),
                    projection_revision: result.identity.projection_revision,
                    request_id: result.identity.request_id.clone(),
                    pane_id: result.identity.target_pane_id.clone(),
                    contribution_revision: result.identity.contribution_revision,
                    owner_service_id: result.service_id.clone(),
                    service_generation: result.service_generation,
                    document: None,
                    actions: Vec::new(),
                    error: None,
                };
                // Every accepted matching completion ends this exact pending request. A
                // retiring failure is deliberately delivered under its retired generation so
                // the client cannot remain stuck loading while old leases stay invalid.
                if let Some(client) = self.clients.get_mut(&client_id) {
                    client.pending_pane_view = None;
                }
                if rendered.is_err() {
                    self.app
                        .extension_services
                        .fail_over_view_provider(&result.service_id);
                }
                if !generation_is_current {
                    response.error = Some(
                        "extension service generation retired or replaced; retry request".into(),
                    );
                } else {
                    match rendered {
                        Ok(view) => {
                            let allowed_actions =
                                crate::extension_service::declared_actions_allowed(
                                    &view.actions,
                                    &result.granted_capabilities,
                                );
                            let contribution = crate::pane_contribution::PaneContribution {
                                owner_service_id: result.service_id.clone(),
                                target: crate::pane_contribution::PaneContributionTarget::Pane {
                                    pane_id: result.identity.target_pane_id.clone(),
                                },
                                placement:
                                    crate::pane_contribution::PaneContributionPlacement::Accessory,
                                revision: result.identity.contribution_revision,
                                document: view.document.clone(),
                                actions: view.actions.clone(),
                            };
                            if !allowed_actions || contribution.validate().is_err() {
                                self.app
                                    .extension_services
                                    .fail_over_view_provider(&result.service_id);
                                response.error =
                                    Some("extension returned an invalid contribution".into());
                            } else {
                                response.document = Some(view.document);
                                response.actions = view.actions.clone();
                                if let Some(client) = self.clients.get_mut(&client_id) {
                                    client.pane_contribution_lease =
                                        Some(crate::server::clients::PaneContributionLease {
                                            identity: result.identity,
                                            service_id: result.service_id.clone(),
                                            service_generation: result.service_generation,
                                            actions: view.actions,
                                        });
                                }
                            }
                        }
                        Err(error) => response.error = Some(error),
                    }
                }
                if let Ok(data) = serde_json::to_string(&response) {
                    self.send_to_client(
                        client_id,
                        crate::protocol::ServerMessage::EndpointControl {
                            kind: crate::protocol::endpoint::PANE_VIEW_RESULT_KIND.into(),
                            data,
                        },
                    );
                }
            }
            crate::extension_service::ExtensionAsyncResultKind::Action(action_result) => {
                let pending = self
                    .clients
                    .get(&client_id)
                    .and_then(|client| client.pending_pane_action.clone());
                let lease = self
                    .clients
                    .get(&client_id)
                    .and_then(|client| client.pane_contribution_lease.clone());
                let (Some(pending), Some(lease)) = (pending, lease) else {
                    return;
                };
                if pending.boot_id != result.identity.boot_id
                    || pending.projection_revision != result.identity.projection_revision
                    || pending.request_id != result.identity.request_id
                    || pending.pane_id != result.identity.target_pane_id
                    || pending.contribution_revision != result.identity.contribution_revision
                    || pending.owner_service_id != result.service_id
                    || pending.service_generation != result.service_generation
                    || lease.identity.contribution_revision != result.identity.contribution_revision
                {
                    return;
                }
                let mut response = crate::protocol::endpoint::ServerPaneViewActionResult {
                    request_id: pending.request_id.clone(),
                    pane_id: pending.pane_id.clone(),
                    contribution_revision: pending.contribution_revision,
                    action_id: pending.action_id.clone(),
                    confirmation_token: None,
                    error: None,
                };
                if !generation_is_current {
                    response.error = Some(
                        "extension service generation retired or replaced; retry action".into(),
                    );
                } else {
                    match action_result {
                    Ok(crate::extension_service::ExtensionHostEffect::TerminalInterrupt)
                        if pending.action_id == "terminal.interrupt"
                            && result.granted_capabilities.contains(
                                &crate::api::schema::ExtensionServiceCapability::TerminalInterrupt,
                            ) =>
                    {
                        let terminal = self.app.parse_pane_id(&pending.pane_id).and_then(
                            |(workspace_index, pane_id)| {
                                self.app
                                    .state
                                    .terminal_id_for_pane(workspace_index, pane_id)
                            },
                        );
                        if let (Some(terminal_id), Ok(token)) =
                            (terminal, secure_confirmation_token())
                        {
                            response.confirmation_token = Some(token.clone());
                            if let Some(client) = self.clients.get_mut(&client_id) {
                                client.pending_pane_confirmation = Some(
                                        crate::server::clients::PendingPaneEffectConfirmation {
                                            token,
                                            expires_at: std::time::Instant::now()
                                                + std::time::Duration::from_secs(30),
                                            lease,
                                            action_id: pending.action_id.clone(),
                                            terminal_id,
                                            effect: crate::extension_service::ExtensionHostEffect::TerminalInterrupt,
                                        },
                                    );
                            }
                        } else {
                            response.error =
                                Some("target terminal or secure randomness is unavailable".into());
                        }
                    }
                    Ok(_) => {
                        response.error = Some("extension returned an undeclared effect".into())
                    }
                    Err(error) => response.error = Some(error),
                    }
                }
                if let Some(client) = self.clients.get_mut(&client_id) {
                    client.pending_pane_action = None;
                }
                if let Ok(data) = serde_json::to_string(&response) {
                    self.send_to_client(
                        client_id,
                        crate::protocol::ServerMessage::EndpointControl {
                            kind: crate::protocol::endpoint::PANE_VIEW_ACTION_RESULT_KIND.into(),
                            data,
                        },
                    );
                }
            }
        }
    }

    pub(super) fn render_and_stream(&mut self) {
        let full_started = crate::render_prof::timer();
        let render_targets = render_targets(&self.clients, self.foreground_client_id);

        if render_targets.is_empty() {
            let (cols, rows) = self.effective_size;
            let area = Rect::new(0, 0, cols, rows);
            let resize_panes = self.app.state.view.pane_infos.is_empty();
            if resize_panes {
                crate::ui::compute_view_with_runtime_registry(
                    &mut self.app.state,
                    &self.app.terminal_runtimes,
                    area,
                );
            } else {
                crate::ui::compute_view_without_resizing_panes(
                    &mut self.app.state,
                    &self.app.terminal_runtimes,
                    area,
                );
            }
            self.app.full_redraw_pending = false;
            crate::render_prof::duration_since("full_render.total", full_started);
            debug!(
                cols,
                rows, resize_panes, "updated geometry with no attached clients"
            );
            return;
        }

        // Resize from the controlling client's geometry before drawing any observer.
        // Retained updates fall back here when a pane changes alternate screens.
        for (client_id, (cols, rows), cell_size, _, _) in &render_targets {
            let Some(client) = self.clients.get(client_id) else {
                continue;
            };
            if !client.is_active_shell_client() {
                continue;
            }
            let Some(tab_id) = self.shell_tab_id_for_client(*client_id) else {
                continue;
            };
            if self.tab_geometry_controllers.get(&tab_id) != Some(client_id) {
                continue;
            }
            let changed = client
                .render_state
                .last_pane_surface()
                .is_none_or(|surface| {
                    surface.panes.iter().any(|pane| {
                        let Some((workspace_index, pane_id)) =
                            self.app.parse_pane_id(&pane.pane_id)
                        else {
                            return false;
                        };
                        self.app
                            .state
                            .runtime_for_pane_in_workspace(
                                &self.app.terminal_runtimes,
                                workspace_index,
                                pane_id,
                            )
                            .is_some_and(|runtime| {
                                runtime.alternate_screen_active() != pane.alternate_screen_active
                            })
                    })
                });
            if changed {
                if let Some(target) = self.shell_target_for_client(*client_id) {
                    let excluded_panes = client
                        .shell_pane_viewport
                        .as_ref()
                        .filter(|viewport| {
                            viewport.boot_id == self.client_shell_boot_id
                                && viewport.projection_revision == client.shell_projection_revision
                                && viewport.size.is_some()
                        })
                        .map(|viewport| &viewport.pane_id)
                        .into_iter()
                        .chain(
                            client
                                .shell_terminal_views_interest
                                .as_ref()
                                .filter(|interest| {
                                    interest.boot_id == self.client_shell_boot_id
                                        && interest.projection_revision
                                            == client.shell_projection_revision
                                })
                                .into_iter()
                                .flat_map(|interest| {
                                    interest.views.iter().map(|view| &view.pane_id)
                                }),
                        )
                        .filter_map(|pane_id| self.app.parse_pane_id(pane_id))
                        .filter_map(|(workspace_index, pane_id)| {
                            (workspace_index == target.workspace_index).then_some(pane_id)
                        })
                        .collect::<HashSet<_>>();
                    crate::ui::resize_tab_surface_except(
                        &self.app.state,
                        &self.app.terminal_runtimes,
                        target.workspace_index,
                        target.tab_index,
                        Rect::new(0, 0, *cols, *rows),
                        if cell_size.is_known() {
                            *cell_size
                        } else {
                            crate::kitty_graphics::HostCellSize::default()
                        },
                        &excluded_panes,
                    );
                }
            }
        }

        let shared_host_documents = render_targets
            .iter()
            .any(|(client_id, _, _, _, mode)| {
                matches!(mode, ClientConnectionMode::ClientShell)
                    && self
                        .clients
                        .get(client_id)
                        .is_some_and(|client| client.shell_host_documents_enabled)
            })
            .then(|| self.app.host_documents());

        let mut broken_clients: Vec<u64> = Vec::new();
        for (client_id, (cols, rows), cell_size, _is_foreground, mode) in render_targets {
            #[cfg(unix)]
            if matches!(mode, ClientConnectionMode::TerminalObserve { .. })
                && self
                    .clients
                    .get(&client_id)
                    .is_some_and(|client| client.deferred_render() != DeferredRender::None)
            {
                // The writer-drained event schedules recovery, even if pane output stops.
                continue;
            }
            let area = Rect::new(0, 0, cols, rows);
            let shell_mode = matches!(mode, ClientConnectionMode::ClientShell);
            let shell_target = self.shell_target_for_client(client_id);
            let shell_tab_id = self.shell_tab_id_for_client(client_id);
            let shell_shows_popup = shell_tab_id.as_deref() == self.popup_owner_tab_id.as_deref();
            let mut shell_projection_revision = 0;
            let mut restore_pane_viewport = false;
            if matches!(mode, ClientConnectionMode::ClientShell) {
                let location = self
                    .clients
                    .get(&client_id)
                    .and_then(|client| client.shell_location.clone());
                let terminal_view_panes = shell_target
                    .and_then(|target| {
                        self.app
                            .state
                            .workspaces
                            .get(target.workspace_index)
                            .and_then(|workspace| workspace.tabs.get(target.tab_index))
                            .map(|tab| {
                                tab.layout
                                    .pane_ids()
                                    .into_iter()
                                    .filter_map(|pane_id| {
                                        self.app.public_pane_id(target.workspace_index, pane_id)
                                    })
                                    .collect::<HashSet<_>>()
                            })
                    })
                    .unwrap_or_default();
                let agent_view = self.app.state.agent_view_override.clone();
                let Some(client) = self.clients.get_mut(&client_id) else {
                    continue;
                };
                // The projection is a published codec: a client that cannot
                // parse view documents is never sent one, because its own
                // validation would reject the whole projection. A client that
                // advertises `host.view.v1` gets them alongside the legacy
                // entries.
                let host_documents = if client.shell_host_documents_enabled {
                    filter_host_documents_for_client(
                        shared_host_documents.clone().unwrap_or_default(),
                        client.shell_host_views_enabled,
                    )
                } else {
                    Vec::new()
                };
                let mut candidate = client_shell_snapshot(
                    &self.app,
                    &self.client_shell_boot_id,
                    client.shell_projection_revision,
                    None,
                    location.as_ref(),
                );
                candidate.config_diagnostic = if client.shell_uses_endpoint_keybindings {
                    self.server_config_diagnostic.clone()
                } else {
                    self.server_config_diagnostic_without_keybindings.clone()
                };
                candidate.revision = client.shell_projection_revision;
                if client.shell_pane_viewport.as_ref().is_some_and(|request| {
                    !candidate
                        .panes
                        .iter()
                        .any(|pane| pane.pane_id == request.pane_id)
                }) {
                    restore_pane_viewport |= client.shell_pane_viewport.take().is_some();
                }
                let mut structure = client.shell_structure_enabled.then(|| {
                    crate::server::client_shell::structure_projection(
                        &self.app,
                        &self.client_shell_boot_id,
                        client.shell_projection_revision,
                    )
                });
                let structure_changed = client.shell_structure_enabled
                    && client.shell_structure.as_ref() != structure.as_ref();
                let agent_view_changed = client.shell_agent_view != agent_view;
                let host_documents_changed = client.shell_host_documents_enabled
                    && client.shell_host_documents != host_documents;
                if client.shell_snapshot.as_ref() != Some(&candidate)
                    || structure_changed
                    || agent_view_changed
                    || host_documents_changed
                {
                    client.shell_projection_revision =
                        client.shell_projection_revision.saturating_add(1);
                    restore_pane_viewport |= client.shell_pane_viewport.take().is_some();
                    if let Some(mut interest) = client.shell_terminal_views_interest.take() {
                        // Terminal previews are presentation geometry, not extension leases. Keep
                        // valid preview claims across projection-only updates so focusing collection
                        // chrome cannot briefly restore the full leaf and then shrink the PTY again.
                        let panes_still_exist = interest
                            .views
                            .iter()
                            .all(|view| terminal_view_panes.contains(&view.pane_id));
                        if panes_still_exist {
                            interest.projection_revision = client.shell_projection_revision;
                            client.shell_terminal_views_interest = Some(interest);
                        } else {
                            restore_pane_viewport = true;
                        }
                    }
                    if let Some(pending) = client.pending_pane_view.as_ref() {
                        self.app.extension_services.cancel(
                            &crate::extension_service::ExtensionRequestIdentity {
                                client_id,
                                boot_id: pending.request.boot_id.clone(),
                                projection_revision: pending.request.projection_revision,
                                request_id: pending.request.request_id.clone(),
                                target_pane_id: pending.request.pane_id.clone(),
                                contribution_revision: pending.request.contribution_revision,
                            },
                        );
                    }
                    if let Some(pending) = client.pending_pane_action.as_ref() {
                        self.app.extension_services.cancel(
                            &crate::extension_service::ExtensionRequestIdentity {
                                client_id,
                                boot_id: pending.boot_id.clone(),
                                projection_revision: pending.projection_revision,
                                request_id: pending.request_id.clone(),
                                target_pane_id: pending.pane_id.clone(),
                                contribution_revision: pending.contribution_revision,
                            },
                        );
                    }
                    client.pending_pane_view = None;
                    client.pane_contribution_lease = None;
                    client.pending_pane_action = None;
                    client.pending_pane_confirmation = None;
                    candidate.revision = client.shell_projection_revision;
                    if let Some(structure) = structure.as_mut() {
                        structure.revision = client.shell_projection_revision;
                    }
                    let mut controls = Vec::new();
                    if agent_view.is_some() || client.shell_agent_view.is_some() {
                        controls.push(crate::protocol::endpoint::agent_view_projection_message(
                            &candidate.boot_id,
                            candidate.revision,
                            agent_view.as_ref(),
                        ));
                    }
                    controls.push(crate::protocol::endpoint::snapshot_message(&candidate));
                    if client.shell_host_documents_enabled {
                        controls.push(crate::protocol::endpoint::host_documents_message(
                            &crate::protocol::endpoint::HostDocumentsProjectionV1 {
                                boot_id: candidate.boot_id.clone(),
                                revision: candidate.revision,
                                documents: host_documents.clone(),
                            },
                        ));
                    }
                    if let Some(structure) = structure.as_ref() {
                        controls.push(crate::protocol::endpoint::structure_message(structure));
                    }
                    let expected_controls = controls.len();
                    let mut framed_controls = Vec::with_capacity(expected_controls);
                    for control in controls {
                        let message = match control {
                            Ok(message) => message,
                            Err(err) => {
                                warn!(client_id, err = %err, "failed to encode endpoint projection");
                                broken_clients.push(client_id);
                                continue;
                            }
                        };
                        match Self::frame_server_message(&message) {
                            Ok(framed) => framed_controls.push(framed),
                            Err(err) => {
                                warn!(client_id, err = %err, "failed to frame endpoint projection");
                                broken_clients.push(client_id);
                            }
                        }
                    }
                    if framed_controls.len() != expected_controls {
                        continue;
                    }
                    let Some(writer) = client.writer.as_ref() else {
                        broken_clients.push(client_id);
                        continue;
                    };
                    if framed_controls
                        .into_iter()
                        .any(|framed| writer.control.send(framed).is_err())
                    {
                        broken_clients.push(client_id);
                        continue;
                    }
                    client.shell_snapshot = Some(candidate);
                    client.shell_agent_view = agent_view;
                    client.shell_structure = structure;
                    client.shell_host_documents = host_documents;
                }
                shell_projection_revision = client.shell_projection_revision;
                if !client.shell_surface_active {
                    client.clear_deferred_render();
                    continue;
                }
            }
            if restore_pane_viewport {
                let _ = self.resize_shell_tab_if_controller(client_id, true);
            }
            let shell_graphics_delivery = self
                .clients
                .get(&client_id)
                .map(|client| client.shell_graphics_delivery.clone())
                .unwrap_or_default();
            let mut surface_parts = None;
            let frame = match mode {
                ClientConnectionMode::ClientShell => {
                    let render_started = crate::render_prof::timer();
                    let render_cell_size = if cell_size.is_known() {
                        cell_size
                    } else {
                        crate::kitty_graphics::HostCellSize::default()
                    };
                    let crate::server::client_shell::RenderedPaneSurface {
                        frame,
                        panes,
                        collections,
                        splits,
                        popup,
                        graphics,
                        graphics_delivery: next_graphics_delivery,
                    } = render_client_shell_pane_surface(
                        &mut self.app,
                        shell_target,
                        area,
                        false,
                        shell_shows_popup,
                        render_cell_size,
                        &shell_graphics_delivery,
                        client_id,
                    );
                    crate::render_prof::duration_since(
                        "full_render.render_tab_surface_virtual",
                        render_started,
                    );
                    surface_parts = Some((
                        panes,
                        collections,
                        splits,
                        popup,
                        graphics,
                        next_graphics_delivery,
                    ));
                    frame
                }
                ClientConnectionMode::TerminalPending => continue,
                ClientConnectionMode::TerminalAttach { terminal_id }
                | ClientConnectionMode::TerminalObserve { terminal_id } => {
                    let Some(runtime) = self.runtime_for_terminal_id_string(&terminal_id) else {
                        self.send_to_client(
                            client_id,
                            ServerMessage::ServerShutdown {
                                reason: Some(format!(
                                    "terminal attach ended: terminal {terminal_id} not found"
                                )),
                            },
                        );
                        broken_clients.push(client_id);
                        continue;
                    };
                    let render_started = crate::render_prof::timer();
                    let (buffer, cursor) =
                        crate::server::render_stream::render_terminal_virtual(runtime, area);
                    crate::render_prof::duration_since(
                        "full_render.render_terminal_virtual",
                        render_started,
                    );
                    let hyperlinks_started = crate::render_prof::timer();
                    let hyperlinks = runtime.visible_hyperlinks(area);
                    crate::render_prof::duration_since(
                        "full_render.visible_hyperlinks",
                        hyperlinks_started,
                    );
                    let frame_started = crate::render_prof::timer();
                    let frame = FrameData::from_ratatui_buffer_with_hyperlinks(
                        &buffer,
                        cursor,
                        &hyperlinks,
                    );
                    crate::render_prof::duration_since("full_render.frame_build", frame_started);
                    frame
                }
            };

            let terminal_views_surface = if shell_mode {
                self.clients
                    .get(&client_id)
                    .filter(|client| client.shell_terminal_views_enabled)
                    .and_then(|client| client.shell_terminal_views_interest.as_ref())
                    .filter(|interest| {
                        interest.boot_id == self.client_shell_boot_id
                            && interest.projection_revision == shell_projection_revision
                    })
                    .zip(shell_target)
                    .map(|(interest, target)| {
                        self.render_terminal_views(
                            target,
                            shell_projection_revision,
                            interest,
                            if cell_size.is_known() {
                                cell_size
                            } else {
                                crate::kitty_graphics::HostCellSize::default()
                            },
                            None,
                        )
                    })
            } else {
                None
            };
            let Some(client) = self.clients.get_mut(&client_id) else {
                continue;
            };
            let Some(writer) = client.writer.as_ref().cloned() else {
                crate::render_prof::event("full_render.writer_missing");
                continue;
            };
            let terminal_views_render = terminal_views_surface.as_ref().and_then(|surface| {
                let data = serde_json::to_string(surface).ok()?;
                Self::frame_server_message(&ServerMessage::EndpointControl {
                    kind: crate::protocol::endpoint::TERMINAL_VIEWS_SURFACE_KIND.into(),
                    data,
                })
                .ok()
            });
            let has_graphics = surface_parts
                .as_ref()
                .is_some_and(|(_, _, _, _, graphics, _)| {
                    !graphics.assets.is_empty()
                        || !graphics.placements.is_empty()
                        || !graphics.retained_assets.is_empty()
                });
            let mut next_shell_graphics_delivery = None;
            let prepared = if let Some((panes, collections, splits, popup, graphics, delivery)) =
                surface_parts
            {
                next_shell_graphics_delivery = Some(delivery);
                client
                    .render_state
                    .prepare_pane_surface(protocol::PaneSurfaceFrame {
                        boot_id: self.client_shell_boot_id.clone(),
                        projection_revision: shell_projection_revision,
                        surface_revision: 0,
                        frame,
                        panes,
                        collections,
                        splits,
                        popup,
                        graphics,
                    })
            } else {
                client.render_state.prepare_frame(frame)
            };
            let Some(mut prepared) = prepared else {
                if let Some(framed) = terminal_views_render {
                    match writer.render.try_send(framed) {
                        Ok(()) => client.clear_deferred_render(),
                        Err(std::sync::mpsc::TrySendError::Full(_)) => {
                            client.defer_full_render();
                        }
                        Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {
                            broken_clients.push(client_id);
                        }
                    }
                } else {
                    client.clear_deferred_render();
                }
                crate::render_prof::event("full_render.skip_identical");
                continue;
            };
            let max = if has_graphics {
                MAX_GRAPHICS_FRAME_SIZE
            } else {
                crate::protocol::MAX_FRAME_SIZE
            };
            let mut shell_assets_deferred = false;
            let serialized = match Self::frame_server_message_with_max(prepared.message(), max) {
                Ok(frame) => frame,
                Err(protocol::FramingError::Oversized { claimed, max }) if has_graphics => {
                    warn!(
                        client_id,
                        claimed, max, "dropping graphics assets from oversized pane surface"
                    );
                    if !prepared.strip_pane_surface_assets() {
                        crate::render_prof::event("full_render.serialize_oversized");
                        continue;
                    }
                    next_shell_graphics_delivery = None;
                    shell_assets_deferred = true;
                    match Self::frame_server_message(prepared.message()) {
                        Ok(framed) => framed,
                        Err(err) => {
                            warn!(client_id, err = %err, "failed to serialize pane surface without assets");
                            broken_clients.push(client_id);
                            crate::render_prof::event("full_render.serialize_error");
                            continue;
                        }
                    }
                }
                Err(protocol::FramingError::Oversized { claimed, max }) => {
                    warn!(
                        client_id,
                        claimed, max, "skipping oversized frame for client"
                    );
                    crate::render_prof::event("full_render.serialize_oversized");
                    continue;
                }
                Err(err) => {
                    warn!(client_id, err = %err, "failed to serialize frame");
                    broken_clients.push(client_id);
                    crate::render_prof::event("full_render.serialize_error");
                    continue;
                }
            };
            let shell_graphics_pending = next_shell_graphics_delivery
                .as_ref()
                .is_some_and(crate::kitty_graphics::surface::DeliveryCache::has_pending);
            match writer.render.try_send(serialized) {
                Ok(()) => {
                    if let Some(delivery) = next_shell_graphics_delivery {
                        client.shell_graphics_delivery = delivery;
                    }
                    client.render_state.commit_sent_frame(prepared);
                    let terminal_views_deferred = terminal_views_render
                        .is_some_and(|framed| writer.render.send_ordered(framed).is_err());
                    if shell_graphics_pending || shell_assets_deferred || terminal_views_deferred {
                        client.defer_full_render();
                    } else {
                        client.clear_deferred_render();
                    }
                    crate::render_prof::event("full_render.sent");
                }
                Err(std::sync::mpsc::TrySendError::Full(_)) => {
                    client.defer_full_render();
                }
                Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {
                    broken_clients.push(client_id);
                }
            }
        }

        if !broken_clients.is_empty() {
            for client_id in broken_clients {
                self.remove_client_and_resize_if_needed(client_id);
            }
        }

        let (cols, rows) = self.effective_size;
        // Full-frame recovery is tracked per connection. A slow client must not
        // keep responsive peers on the global full-render path while it waits
        // for its render slot to drain.
        self.app.full_redraw_pending = false;
        crate::render_prof::duration_since("full_render.total", full_started);
        debug!(cols, rows, foreground_client_id = ?self.foreground_client_id, "rendered virtual frame(s)");
    }
}

/// What one client may receive: a client that does not advertise
/// `host.view.v1` never gets a view entry, because its own validation of the
/// projection would reject the whole payload. Legacy entries pass through
/// unchanged.
pub(super) fn filter_host_documents_for_client(
    entries: Vec<crate::protocol::endpoint::HostDocumentProjectionEntryV1>,
    host_views_enabled: bool,
) -> Vec<crate::protocol::endpoint::HostDocumentProjectionEntryV1> {
    if host_views_enabled {
        return entries;
    }
    entries
        .into_iter()
        .filter(|entry| entry.document.contract != crate::extension_contract::HOST_VIEW_CONTRACT_V1)
        .collect()
}

#[cfg(test)]
mod host_document_filter_tests {
    use super::filter_host_documents_for_client;
    use crate::extension_contract::{
        HostDocumentV1, HostSurface, HostTarget, HOST_DOCUMENT_CONTRACT_V1, HOST_VIEW_CONTRACT_V1,
    };
    use crate::protocol::endpoint::HostDocumentProjectionEntryV1;

    fn entry(contract: &str, namespace: &str) -> HostDocumentProjectionEntryV1 {
        HostDocumentProjectionEntryV1 {
            provider_priority: 1,
            document: HostDocumentV1 {
                contract: contract.into(),
                namespace: namespace.into(),
                generation: 1,
                revision: 1,
                surface: HostSurface::Navigation,
                target: HostTarget::Session,
                items: Vec::new(),
                actions: Vec::new(),
                required_capabilities: Vec::new(),
            },
            confirmation_tokens: Default::default(),
        }
    }

    #[test]
    fn a_client_without_the_view_kind_never_receives_a_view_entry() {
        let entries = vec![
            entry(HOST_DOCUMENT_CONTRACT_V1, "legacy.one"),
            entry(HOST_VIEW_CONTRACT_V1, "vqro.tasks"),
            entry(HOST_DOCUMENT_CONTRACT_V1, "legacy.two"),
        ];
        let filtered = filter_host_documents_for_client(entries.clone(), false);
        assert_eq!(filtered.len(), 2);
        assert!(filtered
            .iter()
            .all(|entry| entry.document.contract == HOST_DOCUMENT_CONTRACT_V1));
        // A client that advertises the kind receives everything.
        assert_eq!(filter_host_documents_for_client(entries, true).len(), 3);
    }
}
