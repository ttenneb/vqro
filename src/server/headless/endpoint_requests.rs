use super::*;

impl HeadlessServer {
    pub(super) fn handle_client_shell_endpoint_request(
        &mut self,
        client_id: u64,
        boot_id: String,
        request: Box<api::schema::Request>,
    ) -> bool {
        self.handle_endpoint_api_request(client_id, boot_id, request, false)
    }

    pub(super) fn handle_native_endpoint_request(
        &mut self,
        client_id: u64,
        boot_id: String,
        request: Box<api::schema::Request>,
    ) -> bool {
        self.handle_endpoint_api_request(client_id, boot_id, request, true)
    }

    fn send_endpoint_api_error(
        &mut self,
        client_id: u64,
        native: bool,
        boot_id: String,
        request_id: String,
        code: &'static str,
        message: impl Into<String>,
    ) {
        let message = message.into();
        if native {
            let response =
                crate::server::client_commands::error_response(request_id.clone(), code, message);
            if let Ok(message) = crate::native_api::response_chunk_message(
                boot_id,
                request_id,
                true,
                response.as_bytes(),
            ) {
                self.send_to_client(client_id, message);
            }
        } else {
            let message =
                crate::server::client_commands::error_message(boot_id, request_id, code, message);
            self.send_to_client(client_id, message);
        }
    }

    pub(super) fn handle_endpoint_api_response_chunk(
        &mut self,
        client_id: u64,
        boot_id: String,
        request_id: String,
        final_chunk: bool,
        data: Vec<u8>,
        native: bool,
    ) -> bool {
        let command_surface_revision = self.clients.get(&client_id).and_then(|client| {
            (matches!(client.mode, ClientConnectionMode::ClientShell)
                && client.shell_endpoint_command_in_flight
                && boot_id == self.client_shell_boot_id)
                .then_some(client.shell_endpoint_command_surface_revision)
                .flatten()
        });
        let Some(command_surface_revision) = command_surface_revision else {
            return false;
        };
        let completed_deferred_response = self.clients.get_mut(&client_id).and_then(|client| {
            let response = client.shell_deferred_navigation_response.as_mut()?;
            response.extend_from_slice(&data);
            final_chunk
                .then(|| client.shell_deferred_navigation_response.take())
                .flatten()
        });
        if final_chunk {
            if let Some(client) = self.clients.get_mut(&client_id) {
                client.shell_endpoint_command_in_flight = false;
                client.shell_endpoint_command_surface_revision = None;
                client.shell_deferred_navigation_request_id = None;
            }
        }
        let deferred_tab_id = completed_deferred_response
            .as_deref()
            .and_then(Self::deferred_endpoint_navigation_tab_id);
        let focus_before = self.shell_focus_target(client_id);
        let focused_tabs_before = self.focused_shell_tabs();
        let navigation_changed = self.clients.get(&client_id).is_some_and(|client| {
            client.is_active_shell_client()
                && client.shell_projection_revision == command_surface_revision
        }) && deferred_tab_id
            .as_deref()
            .is_some_and(|tab_id| self.focus_shell_client_on_tab(client_id, tab_id));
        let geometry_changed =
            navigation_changed && self.claim_shell_tab_geometry(client_id, false);
        if navigation_changed {
            self.reconcile_client_shell_locations();
            let focus_after = self.shell_focus_target(client_id);
            let focused_tabs_after = self.focused_shell_tabs();
            self.app.accept_current_focus_without_events();
            self.send_shell_navigation_focus_events(
                focus_before.as_ref(),
                focus_after.as_ref(),
                &focused_tabs_before,
                &focused_tabs_after,
            );
        }
        if native {
            if let Ok(message) =
                crate::native_api::response_chunk_message(boot_id, request_id, final_chunk, &data)
            {
                self.send_to_client(client_id, message);
            }
        } else {
            self.send_to_client(
                client_id,
                ServerMessage::ClientShellEndpointResponseChunk {
                    boot_id,
                    request_id,
                    final_chunk,
                    data,
                },
            );
        }
        navigation_changed | geometry_changed
    }

    fn handle_endpoint_api_request(
        &mut self,
        client_id: u64,
        boot_id: String,
        mut request: Box<api::schema::Request>,
        native: bool,
    ) -> bool {
        let Some(client) = self.clients.get(&client_id) else {
            return false;
        };
        if !matches!(client.mode, ClientConnectionMode::ClientShell) {
            self.remove_client_and_resize_if_needed(client_id);
            return true;
        }
        let request_id = request.id.clone();
        if !native && !crate::server::client_commands::supports_client_shell_method(&request.method)
        {
            self.send_endpoint_api_error(
                client_id,
                false,
                boot_id,
                request_id,
                "unsupported_endpoint_command",
                "this method is not available through the frozen client bridge",
            );
            return false;
        }
        if native && !crate::native_api::endpoint_supports(&request.method) {
            self.send_endpoint_api_error(
                client_id,
                true,
                boot_id,
                request_id,
                "unsupported_native_endpoint_method",
                "this API method requires the local socket or a presentation-specific control",
            );
            return false;
        }
        if boot_id != self.client_shell_boot_id {
            self.send_endpoint_api_error(
                client_id,
                native,
                boot_id,
                request_id,
                "stale_boot",
                "endpoint command targeted an earlier server boot",
            );
            return false;
        }
        let surface_active = client.shell_surface_active;
        if let api::schema::Method::ClientShellSurfaceSet(params) = &request.method {
            if native {
                self.send_endpoint_api_error(
                    client_id,
                    true,
                    boot_id,
                    request_id,
                    "presentation_method",
                    "client shell surface state is not part of the native vqro API",
                );
                return false;
            }
            let Some((changed, projection_revision)) =
                self.set_client_shell_surface_active(client_id, params.active)
            else {
                return false;
            };
            self.send_to_client(
                client_id,
                crate::server::client_commands::success_message_with_result(
                    boot_id,
                    request_id,
                    api::schema::ResponseResult::ClientShellSurfaceSet {
                        active: params.active,
                        projection_revision,
                    },
                ),
            );
            return changed;
        }
        if client.shell_endpoint_command_in_flight {
            self.send_endpoint_api_error(
                client_id,
                native,
                boot_id,
                request_id,
                "endpoint_busy",
                "this endpoint is still processing another command",
            );
            return false;
        }
        if !native && !surface_active {
            self.send_endpoint_api_error(
                client_id,
                false,
                boot_id,
                request_id,
                "surface_inactive",
                "this method requires an active client shell surface",
            );
            return false;
        }

        let api_request_id = format!(
            "{}:{}:{client_id}:{request_id}",
            if native { "vqro-api" } else { "endpoint" },
            self.client_shell_boot_id
        );
        request.id = api_request_id.clone();
        let (respond_to, response_rx) = std::sync::mpsc::channel();
        if let Err(err) = crate::server::client_commands::spawn_response_waiter_for_lane(
            client_id,
            boot_id.clone(),
            request_id.clone(),
            response_rx,
            self.server_event_tx.clone(),
            native,
        ) {
            self.send_endpoint_api_error(
                client_id,
                native,
                boot_id,
                request_id,
                "server_unavailable",
                format!("failed to start endpoint response bridge: {err}"),
            );
            return false;
        }
        if let Some(client) = self.clients.get_mut(&client_id) {
            client.shell_endpoint_command_in_flight = true;
            client.shell_endpoint_command_surface_revision = Some(client.shell_projection_revision);
            let deferred_worktree = matches!(
                &request.method,
                api::schema::Method::WorktreeCreate(_) | api::schema::Method::WorktreeRemove(_)
            );
            let deferred_navigation = matches!(
                &request.method,
                api::schema::Method::WorktreeCreate(params) if params.focus
            );
            client.shell_deferred_navigation_request_id =
                deferred_worktree.then(|| api_request_id.clone());
            client.shell_deferred_navigation_response = deferred_navigation.then(Vec::new);
        }
        let foreground_changed = self.promote_client_to_foreground(client_id);
        foreground_changed
            | self.handle_client_shell_api_request(
                client_id,
                api::ApiRequestMessage {
                    origin: if native {
                        crate::api::ApiOrigin::NativeEndpoint { client_id, boot_id }
                    } else {
                        crate::api::ApiOrigin::LegacyEndpointV1 { client_id, boot_id }
                    },
                    request: *request,
                    respond_to,
                    response_write_complete: None,
                    stream_active: None,
                },
            )
    }
}
