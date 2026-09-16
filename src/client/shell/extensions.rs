use super::*;
use crossterm::event::{KeyCode, KeyModifiers};

const EXTENSION_CATEGORIES: [&str; 3] = ["Agents", "Workflow", "Interface"];

impl ClientShellState {
    pub(super) fn open_extensions_overlay(&mut self, outcome: &mut ClientShellInput) {
        self.overlay = Some(ClientShellOverlay::Extensions(ClientExtensionsOverlay {
            tab: ClientExtensionsTab::Browse,
            query: String::new(),
            search_focused: false,
            category: None,
            selected: 0,
            extensions: Vec::new(),
            loading: true,
            mutating: false,
            confirmation: None,
            message: None,
        }));
        if !self.push_endpoint_method_with_kind(
            crate::api::schema::Method::ExtensionList(
                crate::api::schema::ExtensionListParams::default(),
            ),
            PendingEndpointKind::ExtensionList,
            outcome,
        ) {
            if let Some(ClientShellOverlay::Extensions(extensions)) = self.overlay.as_mut() {
                extensions.loading = false;
                extensions.message = Some("Extensions are unavailable on this server.".into());
            }
        }
        outcome.repaint = true;
    }

    fn move_extension_selection(&mut self, delta: isize) {
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

    fn queue_extension_mutation(
        &mut self,
        method: crate::api::schema::Method,
        outcome: &mut ClientShellInput,
    ) {
        if let Some(ClientShellOverlay::Extensions(extensions)) = self.overlay.as_mut() {
            extensions.mutating = true;
            extensions.confirmation = None;
            extensions.message = None;
        }
        if !self.push_endpoint_method_with_kind(
            method,
            PendingEndpointKind::ExtensionMutation,
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
        let Some(extension) = self.selected_extension() else {
            return;
        };
        let confirmation = match self.overlay.as_ref() {
            Some(ClientShellOverlay::Extensions(extensions)) => extensions.confirmation,
            _ => None,
        };
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
            if let Some(ClientShellOverlay::Extensions(extensions)) = self.overlay.as_mut() {
                extensions.confirmation = Some(ClientExtensionConfirmation::Install);
            }
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
                }
            } else {
                self.overlay = None;
            }
            outcome.repaint = true;
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
                        extensions.query.push(character);
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
                    if let Some(ClientShellOverlay::Extensions(extensions)) = self.overlay.as_mut()
                    {
                        extensions.confirmation = Some(ClientExtensionConfirmation::Remove);
                    }
                }
            }
            KeyCode::Char('x') if modifiers.is_empty() => {
                if self
                    .selected_extension()
                    .is_some_and(|extension| !extension.installed)
                {
                    if let Some(ClientShellOverlay::Extensions(extensions)) = self.overlay.as_mut()
                    {
                        extensions.confirmation = Some(ClientExtensionConfirmation::DeleteData);
                    }
                }
            }
            KeyCode::Char('u') if modifiers.is_empty() => {
                if self.selected_extension().is_some_and(|extension| {
                    extension.installed && extension.available_version != extension.active_version
                }) {
                    if let Some(ClientShellOverlay::Extensions(extensions)) = self.overlay.as_mut()
                    {
                        extensions.confirmation = Some(ClientExtensionConfirmation::Update);
                    }
                }
            }
            KeyCode::Char('b') if modifiers.is_empty() => {
                if self
                    .selected_extension()
                    .is_some_and(|extension| extension.rollback_version.is_some())
                {
                    if let Some(ClientShellOverlay::Extensions(extensions)) = self.overlay.as_mut()
                    {
                        extensions.confirmation = Some(ClientExtensionConfirmation::Rollback);
                    }
                }
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
            PendingEndpointKind::ExtensionList => {
                if let Some(ClientShellOverlay::Extensions(extensions)) = self.overlay.as_mut() {
                    extensions.loading = false;
                    match result {
                        Ok(crate::api::schema::ResponseResult::ExtensionList {
                            extensions: listed,
                        }) => {
                            extensions.extensions = listed;
                            extensions.selected = 0;
                            extensions.message = None;
                        }
                        Ok(_) => extensions.message = Some("Unexpected extension response.".into()),
                        Err(error) => extensions.message = Some(error.message),
                    }
                }
                (true, Vec::new())
            }
            PendingEndpointKind::ExtensionMutation => {
                if let Some(ClientShellOverlay::Extensions(extensions)) = self.overlay.as_mut() {
                    extensions.mutating = false;
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
                        }
                        Ok(crate::api::schema::ResponseResult::ExtensionDataDeleted { .. }) => {
                            extensions.message = Some("Extension data deleted.".into());
                        }
                        Ok(_) => extensions.message = Some("Unexpected extension response.".into()),
                        Err(error) => extensions.message = Some(error.message),
                    }
                    let count = extensions.filtered_indices().len();
                    extensions.selected = extensions.selected.min(count.saturating_sub(1));
                }
                (true, Vec::new())
            }
            _ => (false, Vec::new()),
        }
    }
}
