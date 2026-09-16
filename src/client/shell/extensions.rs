use super::*;
use crossterm::event::{KeyCode, KeyModifiers};

const EXTENSION_CATEGORIES: [&str; 3] = ["Agents", "Workflow", "Interface"];

impl ClientShellState {
    pub(super) fn open_extensions_overlay(&mut self, outcome: &mut ClientShellInput) {
        let repository_context = self.focused_repository_id();
        let generation = self.next_request_id;
        self.overlay = Some(ClientShellOverlay::Extensions(ClientExtensionsOverlay {
            generation,
            tab: ClientExtensionsTab::Browse,
            query: String::new(),
            search_focused: false,
            category: None,
            selected: 0,
            extensions: Vec::new(),
            loading: true,
            mutating: false,
            confirmation: None,
            confirmation_extension: None,
            repository_context,
            message: None,
        }));
        if !self.push_endpoint_method_with_kind(
            crate::api::schema::Method::ExtensionList(
                crate::api::schema::ExtensionListParams::default(),
            ),
            PendingEndpointKind::ExtensionList(generation),
            outcome,
        ) {
            if let Some(ClientShellOverlay::Extensions(extensions)) = self.overlay.as_mut() {
                extensions.loading = false;
                extensions.message = Some("Extensions are unavailable on this server.".into());
            }
        }
        outcome.repaint = true;
    }

    pub(super) fn move_extension_selection(&mut self, delta: isize) {
        let Some(ClientShellOverlay::Extensions(extensions)) = self.overlay.as_mut() else {
            return;
        };
        let count = extensions.filtered_indices().len();
        if count == 0 {
            extensions.selected = 0;
        } else {
            extensions.selected = (extensions.selected as isize + delta)
                .clamp(0, count.saturating_sub(1) as isize)
                as usize;
        }
    }

    fn cycle_extension_category(&mut self) {
        let Some(ClientShellOverlay::Extensions(extensions)) = self.overlay.as_mut() else {
            return;
        };
        let next = extensions
            .category
            .as_deref()
            .and_then(|current| {
                EXTENSION_CATEGORIES
                    .iter()
                    .position(|candidate| *candidate == current)
                    .map(|index| index + 1)
            })
            .unwrap_or(0);
        extensions.category = EXTENSION_CATEGORIES
            .get(next)
            .map(|value| (*value).to_string());
        extensions.selected = 0;
    }

    fn selected_extension(&self) -> Option<crate::api::schema::ExtensionInfo> {
        match self.overlay.as_ref() {
            Some(ClientShellOverlay::Extensions(extensions)) => {
                extensions.selected_extension().cloned()
            }
            _ => None,
        }
    }

    pub(super) fn focused_repository_id(&self) -> Option<String> {
        let workspace_id = self.snapshot.as_deref()?.focused_workspace_id.as_deref()?;
        self.structure
            .as_deref()?
            .repositories
            .iter()
            .find(|repository| {
                repository
                    .checkouts
                    .iter()
                    .any(|checkout| checkout.workspace_id == workspace_id)
            })
            .and_then(|repository| repository.durable_identity.clone())
    }

    fn set_extension_confirmation(&mut self, confirmation: ClientExtensionConfirmation) {
        let extension = self.selected_extension();
        if let Some(ClientShellOverlay::Extensions(extensions)) = self.overlay.as_mut() {
            extensions.confirmation = extension.as_ref().map(|_| confirmation);
            extensions.confirmation_extension = extension.map(Box::new);
            extensions.search_focused = false;
        }
    }

    fn queue_extension_mutation(
        &mut self,
        method: crate::api::schema::Method,
        outcome: &mut ClientShellInput,
    ) {
        let generation = match self.overlay.as_ref() {
            Some(ClientShellOverlay::Extensions(extensions)) if !extensions.loading => {
                extensions.generation
            }
            Some(ClientShellOverlay::Extensions(_)) => return,
            _ => return,
        };
        if let Some(ClientShellOverlay::Extensions(extensions)) = self.overlay.as_mut() {
            extensions.mutating = true;
            extensions.confirmation = None;
            extensions.confirmation_extension = None;
            extensions.search_focused = false;
            extensions.message = None;
        }
        if !self.push_endpoint_method_with_kind(
            method,
            PendingEndpointKind::ExtensionMutation(generation),
            outcome,
        ) {
            if let Some(ClientShellOverlay::Extensions(extensions)) = self.overlay.as_mut() {
                extensions.mutating = false;
                extensions.message = Some("This server does not support that action.".into());
            }
        }
        outcome.repaint = true;
    }

    pub(super) fn confirm_extension_action(&mut self, outcome: &mut ClientShellInput) {
        let (confirmation, confirmed_extension) = match self.overlay.as_ref() {
            Some(ClientShellOverlay::Extensions(extensions)) => (
                extensions.confirmation,
                extensions.confirmation_extension.clone(),
            ),
            _ => (None, None),
        };
        let Some(extension) = confirmed_extension else {
            return;
        };
        let extension = *extension;
        let method = match confirmation {
            Some(ClientExtensionConfirmation::Install) => {
                crate::api::schema::Method::ExtensionInstall(
                    crate::api::schema::ExtensionConfirmedParams {
                        repository: extension.repository,
                        confirmed: true,
                    },
                )
            }
            Some(ClientExtensionConfirmation::Remove) => {
                crate::api::schema::Method::ExtensionRemove(
                    crate::api::schema::ExtensionConfirmedParams {
                        repository: extension.repository,
                        confirmed: true,
                    },
                )
            }
            Some(ClientExtensionConfirmation::DeleteData) => {
                crate::api::schema::Method::ExtensionDataDelete(
                    crate::api::schema::ExtensionConfirmedParams {
                        repository: extension.repository,
                        confirmed: true,
                    },
                )
            }
            Some(ClientExtensionConfirmation::Update) => {
                crate::api::schema::Method::ExtensionUpdate(
                    crate::api::schema::ExtensionUpdateParams {
                        repository: extension.repository,
                        version: extension.available_version,
                        approved: true,
                    },
                )
            }
            Some(ClientExtensionConfirmation::Rollback) => {
                crate::api::schema::Method::ExtensionRollback(
                    crate::api::schema::ExtensionConfirmedParams {
                        repository: extension.repository,
                        confirmed: true,
                    },
                )
            }
            None => return,
        };
        self.queue_extension_mutation(method, outcome);
    }

    pub(super) fn activate_selected_extension(&mut self, outcome: &mut ClientShellInput) {
        let Some(extension) = self.selected_extension() else {
            return;
        };
        if !extension.installed {
            if !extension.install_supported {
                if let Some(ClientShellOverlay::Extensions(extensions)) = self.overlay.as_mut() {
                    extensions.message =
                        Some("Installation is not supported for this curated entry.".into());
                }
                outcome.repaint = true;
                return;
            }
            self.set_extension_confirmation(ClientExtensionConfirmation::Install);
            outcome.repaint = true;
            return;
        }
        if !extension.enabled
            && !extension.artifact_verified
            && extension.package_kind == crate::api::schema::ExtensionPackageKind::AgentIntegration
        {
            self.set_extension_confirmation(ClientExtensionConfirmation::Update);
            outcome.repaint = true;
            return;
        }
        self.queue_extension_mutation(
            crate::api::schema::Method::ExtensionEnable(
                crate::api::schema::ExtensionSetEnabledParams {
                    repository: extension.repository,
                    enabled: !extension.enabled,
                },
            ),
            outcome,
        );
    }

    pub(super) fn route_extensions_key(
        &mut self,
        key: &crate::input::TerminalKey,
        outcome: &mut ClientShellInput,
    ) -> bool {
        let Some(ClientShellOverlay::Extensions(extensions)) = self.overlay.as_ref() else {
            return false;
        };
        if extensions.mutating {
            return true;
        }
        let (code, modifiers) = crate::config::normalize_key_combo((key.code, key.modifiers));
        let search_focused = extensions.search_focused;
        let confirming = extensions.confirmation.is_some();
        if code == KeyCode::Esc {
            if search_focused {
                if let Some(ClientShellOverlay::Extensions(extensions)) = self.overlay.as_mut() {
                    extensions.search_focused = false;
                }
            } else if confirming {
                if let Some(ClientShellOverlay::Extensions(extensions)) = self.overlay.as_mut() {
                    extensions.confirmation = None;
                    extensions.confirmation_extension = None;
                }
            } else {
                self.overlay = None;
            }
            outcome.repaint = true;
            return true;
        }
        if self.hits.extensions_popup.is_empty() || self.hits.extensions_tabs.is_empty() {
            return true;
        }
        if confirming {
            if code == KeyCode::Enter {
                self.confirm_extension_action(outcome);
            }
            return true;
        }
        if search_focused {
            match code {
                KeyCode::Enter => {
                    if let Some(ClientShellOverlay::Extensions(extensions)) = self.overlay.as_mut()
                    {
                        extensions.search_focused = false;
                    }
                }
                KeyCode::Backspace => {
                    if let Some(ClientShellOverlay::Extensions(extensions)) = self.overlay.as_mut()
                    {
                        extensions.query.pop();
                        extensions.selected = 0;
                    }
                }
                KeyCode::Char(character)
                    if modifiers.difference(KeyModifiers::SHIFT).is_empty() =>
                {
                    if let Some(ClientShellOverlay::Extensions(extensions)) = self.overlay.as_mut()
                    {
                        let text = key
                            .generated_text
                            .clone()
                            .unwrap_or_else(|| character.to_string());
                        extensions.query.push_str(&text);
                        extensions.selected = 0;
                    }
                }
                _ => {}
            }
            outcome.repaint = true;
            return true;
        }
        match code {
            KeyCode::Char('/') if modifiers.is_empty() => {
                if let Some(ClientShellOverlay::Extensions(extensions)) = self.overlay.as_mut() {
                    extensions.search_focused = true;
                }
            }
            KeyCode::Left | KeyCode::Char('h') if modifiers.is_empty() => {
                if let Some(ClientShellOverlay::Extensions(extensions)) = self.overlay.as_mut() {
                    extensions.tab = ClientExtensionsTab::Browse;
                    extensions.selected = 0;
                }
            }
            KeyCode::Right | KeyCode::Char('l') if modifiers.is_empty() => {
                if let Some(ClientShellOverlay::Extensions(extensions)) = self.overlay.as_mut() {
                    extensions.tab = ClientExtensionsTab::Installed;
                    extensions.selected = 0;
                }
            }
            KeyCode::Char('c') if modifiers.is_empty() => self.cycle_extension_category(),
            KeyCode::Up | KeyCode::Char('k') if modifiers.is_empty() => {
                self.move_extension_selection(-1)
            }
            KeyCode::Down | KeyCode::Char('j') if modifiers.is_empty() => {
                self.move_extension_selection(1)
            }
            KeyCode::Enter | KeyCode::Char(' ') if modifiers.is_empty() => {
                self.activate_selected_extension(outcome)
            }
            KeyCode::Char('r') if modifiers.is_empty() => {
                if self
                    .selected_extension()
                    .is_some_and(|extension| extension.installed)
                {
                    self.set_extension_confirmation(ClientExtensionConfirmation::Remove);
                }
            }
            KeyCode::Char('x') if modifiers.is_empty() => {
                if self
                    .selected_extension()
                    .is_some_and(|extension| !extension.installed)
                {
                    self.set_extension_confirmation(ClientExtensionConfirmation::DeleteData);
                }
            }
            KeyCode::Char('[') if modifiers.is_empty() => {
                if let Some(extension) = self
                    .selected_extension()
                    .filter(|extension| extension.installed && extension.load_order > 0)
                {
                    self.queue_extension_mutation(
                        crate::api::schema::Method::ExtensionReorder(
                            crate::api::schema::ExtensionReorderParams {
                                repository: extension.repository,
                                index: extension.load_order.saturating_sub(1),
                            },
                        ),
                        outcome,
                    );
                }
            }
            KeyCode::Char(']') if modifiers.is_empty() => {
                if let Some(extension) = self
                    .selected_extension()
                    .filter(|extension| extension.installed)
                {
                    self.queue_extension_mutation(
                        crate::api::schema::Method::ExtensionReorder(
                            crate::api::schema::ExtensionReorderParams {
                                repository: extension.repository,
                                index: extension.load_order.saturating_add(1),
                            },
                        ),
                        outcome,
                    );
                }
            }
            KeyCode::Char('o') if modifiers.is_empty() => {
                if let (Some(extension), Some(repository_id)) = (
                    self.selected_extension().filter(|extension| {
                        extension.installed && extension.repository_override_supported
                    }),
                    self.overlay.as_ref().and_then(|overlay| match overlay {
                        ClientShellOverlay::Extensions(extensions) => {
                            extensions.repository_context.clone()
                        }
                        _ => None,
                    }),
                ) {
                    let enabled = extension
                        .repository_overrides
                        .iter()
                        .find(|override_info| override_info.repository_id == repository_id)
                        .map_or(Some(!extension.enabled), |_| None);
                    self.queue_extension_mutation(
                        crate::api::schema::Method::ExtensionRepositoryOverrideSet(
                            crate::api::schema::ExtensionRepositoryOverrideParams {
                                repository: extension.repository,
                                repository_id,
                                enabled,
                            },
                        ),
                        outcome,
                    );
                } else if let Some(ClientShellOverlay::Extensions(extensions)) =
                    self.overlay.as_mut()
                {
                    extensions.message = Some(
                        "Repository overrides require an installed bundled extension and a focused repository."
                            .into(),
                    );
                }
            }
            KeyCode::Char('u') if modifiers.is_empty() => {
                if self.selected_extension().is_some_and(|extension| {
                    extension.installed
                        && extension.package_kind
                            == crate::api::schema::ExtensionPackageKind::AgentIntegration
                }) {
                    self.set_extension_confirmation(ClientExtensionConfirmation::Update);
                }
            }
            KeyCode::Char('b')
                if modifiers.is_empty()
                    && self.selected_extension().is_some_and(|extension| {
                        extension.package_kind
                            == crate::api::schema::ExtensionPackageKind::AgentIntegration
                            && extension.rollback_version.is_some()
                    }) =>
            {
                self.set_extension_confirmation(ClientExtensionConfirmation::Rollback);
            }
            _ => {}
        }
        outcome.repaint = true;
        true
    }

    pub(super) fn handle_extensions_endpoint_result(
        &mut self,
        kind: PendingEndpointKind,
        result: Result<crate::api::schema::ResponseResult, ClientShellEndpointError>,
    ) -> (bool, Vec<ClientShellAction>) {
        match kind {
            PendingEndpointKind::ExtensionList(generation) => {
                if !matches!(
                    self.overlay,
                    Some(ClientShellOverlay::Extensions(ClientExtensionsOverlay {
                        generation: current,
                        ..
                    })) if current == generation
                ) {
                    return (true, Vec::new());
                }
                if let Some(ClientShellOverlay::Extensions(extensions)) = self.overlay.as_mut() {
                    extensions.loading = false;
                    extensions.mutating = false;
                    match result {
                        Ok(crate::api::schema::ResponseResult::ExtensionList {
                            extensions: listed,
                        }) => {
                            let selected_repository = extensions
                                .selected_extension()
                                .map(|extension| extension.repository.clone());
                            extensions.extensions = listed;
                            extensions.selected = selected_repository
                                .as_ref()
                                .and_then(|repository| {
                                    extensions.filtered_indices().iter().position(|index| {
                                        extensions.extensions[*index].repository == *repository
                                    })
                                })
                                .unwrap_or(0);
                            extensions.message = None;
                        }
                        Ok(_) => extensions.message = Some("Unexpected extension response.".into()),
                        Err(error) => extensions.message = Some(error.message),
                    }
                }
                (true, Vec::new())
            }
            PendingEndpointKind::ExtensionMutation(generation) => {
                if !matches!(
                    self.overlay,
                    Some(ClientShellOverlay::Extensions(ClientExtensionsOverlay {
                        generation: current,
                        ..
                    })) if current == generation
                ) {
                    return (true, Vec::new());
                }
                let mut refresh = false;
                if let Some(ClientShellOverlay::Extensions(extensions)) = self.overlay.as_mut() {
                    match result {
                        Ok(crate::api::schema::ResponseResult::ExtensionChanged { extension }) => {
                            if let Some(existing) = extensions
                                .extensions
                                .iter_mut()
                                .find(|existing| existing.repository == extension.repository)
                            {
                                *existing = extension;
                            }
                            extensions.message = Some("Extension updated.".into());
                            refresh = true;
                        }
                        Ok(crate::api::schema::ResponseResult::ExtensionDataDeleted { .. }) => {
                            extensions.message = Some("Extension data deleted.".into());
                            refresh = true;
                        }
                        Ok(_) => extensions.message = Some("Unexpected extension response.".into()),
                        Err(error) => extensions.message = Some(error.message),
                    }
                    let count = extensions.filtered_indices().len();
                    extensions.selected = extensions.selected.min(count.saturating_sub(1));
                }
                let actions = if refresh {
                    let mut deferred = ClientShellInput::default();
                    if let Some(ClientShellOverlay::Extensions(extensions)) = self.overlay.as_mut()
                    {
                        extensions.loading = true;
                    }
                    let queued = self.push_endpoint_method_with_kind(
                        crate::api::schema::Method::ExtensionList(
                            crate::api::schema::ExtensionListParams::default(),
                        ),
                        PendingEndpointKind::ExtensionList(generation),
                        &mut deferred,
                    );
                    if !queued {
                        if let Some(ClientShellOverlay::Extensions(extensions)) =
                            self.overlay.as_mut()
                        {
                            extensions.loading = false;
                            extensions.mutating = false;
                            extensions.message = Some(
                                "Extension changed, but this server no longer supports refresh."
                                    .into(),
                            );
                        }
                    }
                    deferred.actions
                } else {
                    if let Some(ClientShellOverlay::Extensions(extensions)) = self.overlay.as_mut()
                    {
                        extensions.mutating = false;
                    }
                    Vec::new()
                };
                (true, actions)
            }
            _ => (false, Vec::new()),
        }
    }
}
