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
            git_install_mode: false,
            git_inspection: None,
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
        let _ = self.push_endpoint_method_with_kind(
            crate::api::schema::Method::ExtensionPackageList(crate::api::schema::EmptyParams {}),
            PendingEndpointKind::ExtensionPackageList(generation),
            outcome,
        );
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
        let pending = if matches!(
            method,
            crate::api::schema::Method::ExtensionPackageInstall(_)
                | crate::api::schema::Method::ExtensionPackageUpdate(_)
                | crate::api::schema::Method::ExtensionPackageEnable(_)
                | crate::api::schema::Method::ExtensionPackageDisable(_)
                | crate::api::schema::Method::ExtensionPackageRemove(_)
                | crate::api::schema::Method::ExtensionPackageRollback(_)
        ) {
            PendingEndpointKind::ExtensionPackageMutation(generation)
        } else {
            PendingEndpointKind::ExtensionMutation(generation)
        };
        if !self.push_endpoint_method_with_kind(method, pending, outcome) {
            if let Some(ClientShellOverlay::Extensions(extensions)) = self.overlay.as_mut() {
                extensions.mutating = false;
                extensions.message = Some("This server does not support that action.".into());
            }
        }
        outcome.repaint = true;
    }

    pub(super) fn confirm_extension_action(&mut self, outcome: &mut ClientShellInput) {
        let (confirmation, confirmed_extension, git_inspection) = match self.overlay.as_ref() {
            Some(ClientShellOverlay::Extensions(extensions)) => (
                extensions.confirmation,
                extensions.confirmation_extension.clone(),
                extensions.git_inspection.clone(),
            ),
            _ => (None, None, None),
        };
        let Some(extension) = confirmed_extension else {
            return;
        };
        let extension = *extension;
        let package_method = if extension.package_kind
            == crate::api::schema::ExtensionPackageKind::SandboxedPackage
        {
            match confirmation {
                Some(ClientExtensionConfirmation::Install) => git_inspection.map(|inspection| {
                    crate::api::schema::Method::ExtensionPackageInstall(
                        crate::api::schema::ExtensionPackageApprovalParams {
                            source: inspection.source,
                            expected_package_sha256: inspection.package_sha256,
                            expected_manifest_sha256: inspection.manifest_sha256,
                            approved: true,
                        },
                    )
                }),
                Some(ClientExtensionConfirmation::Remove) => {
                    Some(crate::api::schema::Method::ExtensionPackageRemove(
                        crate::api::schema::ExtensionPackageMutationParams {
                            package_id: extension.id.clone(),
                            confirmed: true,
                        },
                    ))
                }
                Some(ClientExtensionConfirmation::Update) => git_inspection.map(|inspection| {
                    crate::api::schema::Method::ExtensionPackageUpdate(
                        crate::api::schema::ExtensionPackageApprovalParams {
                            source: inspection.source,
                            expected_package_sha256: inspection.package_sha256,
                            expected_manifest_sha256: inspection.manifest_sha256,
                            approved: true,
                        },
                    )
                }),
                Some(ClientExtensionConfirmation::Rollback) => {
                    Some(crate::api::schema::Method::ExtensionPackageRollback(
                        crate::api::schema::ExtensionPackageMutationParams {
                            package_id: extension.id.clone(),
                            confirmed: true,
                        },
                    ))
                }
                _ => None,
            }
        } else {
            None
        };
        if extension.package_kind == crate::api::schema::ExtensionPackageKind::SandboxedPackage {
            if let Some(method) = package_method {
                self.queue_extension_mutation(method, outcome);
            }
            return;
        }
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
        let method = if extension.package_kind
            == crate::api::schema::ExtensionPackageKind::SandboxedPackage
        {
            let params = crate::api::schema::ExtensionPackageMutationParams {
                package_id: extension.id,
                confirmed: true,
            };
            if extension.enabled {
                crate::api::schema::Method::ExtensionPackageDisable(params)
            } else {
                crate::api::schema::Method::ExtensionPackageEnable(params)
            }
        } else {
            crate::api::schema::Method::ExtensionEnable(
                crate::api::schema::ExtensionSetEnabledParams {
                    repository: extension.repository,
                    enabled: !extension.enabled,
                },
            )
        };
        self.queue_extension_mutation(method, outcome);
    }

    fn inspect_git_query(&mut self, outcome: &mut ClientShellInput) {
        let (generation, query) = match self.overlay.as_ref() {
            Some(ClientShellOverlay::Extensions(extensions)) => {
                (extensions.generation, extensions.query.clone())
            }
            _ => return,
        };
        let mut fields = query.split_whitespace();
        let (Some(url), Some(commit)) = (fields.next(), fields.next()) else {
            if let Some(ClientShellOverlay::Extensions(extensions)) = self.overlay.as_mut() {
                extensions.message = Some(
                    "Enter: https://repository.example/path.git <40-char commit> [package path]"
                        .into(),
                );
            }
            return;
        };
        let package_path = fields.next().unwrap_or("package.vqrox");
        if fields.next().is_some() {
            if let Some(ClientShellOverlay::Extensions(extensions)) = self.overlay.as_mut() {
                extensions.message = Some("Git package source has too many fields.".into());
            }
            return;
        }
        if let Some(ClientShellOverlay::Extensions(extensions)) = self.overlay.as_mut() {
            extensions.loading = true;
            extensions.search_focused = false;
            extensions.message = Some("Inspecting exact Git package…".into());
        }
        if !self.push_endpoint_method_with_kind(
            crate::api::schema::Method::ExtensionPackageInspect(
                crate::api::schema::ExtensionPackageInspectParams {
                    source: crate::api::schema::ExtensionPackageSource {
                        url: url.into(),
                        commit: commit.into(),
                        package_path: package_path.into(),
                    },
                },
            ),
            PendingEndpointKind::ExtensionPackageInspect(generation),
            outcome,
        ) {
            if let Some(ClientShellOverlay::Extensions(extensions)) = self.overlay.as_mut() {
                extensions.loading = false;
                extensions.message = Some("This server cannot inspect Git packages.".into());
            }
        }
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
                    let git_install_mode = matches!(
                        self.overlay.as_ref(),
                        Some(ClientShellOverlay::Extensions(extensions))
                            if extensions.git_install_mode
                    );
                    if git_install_mode {
                        self.inspect_git_query(outcome);
                    } else if let Some(ClientShellOverlay::Extensions(extensions)) =
                        self.overlay.as_mut()
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
            KeyCode::Char('g') if modifiers.is_empty() => {
                if let Some(ClientShellOverlay::Extensions(extensions)) = self.overlay.as_mut() {
                    extensions.git_install_mode = true;
                    extensions.git_inspection = None;
                    extensions.query.clear();
                    extensions.search_focused = true;
                    extensions.message = Some(
                        "Git source: URL, exact 40-character commit, optional package path".into(),
                    );
                }
            }
            KeyCode::Char('/') if modifiers.is_empty() => {
                if let Some(ClientShellOverlay::Extensions(extensions)) = self.overlay.as_mut() {
                    extensions.git_install_mode = false;
                }

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
                if let Some(extension) = self.selected_extension().filter(|extension| {
                    extension.installed
                        && matches!(
                            extension.package_kind,
                            crate::api::schema::ExtensionPackageKind::AgentIntegration
                                | crate::api::schema::ExtensionPackageKind::SandboxedPackage
                        )
                }) {
                    if extension.package_kind
                        == crate::api::schema::ExtensionPackageKind::SandboxedPackage
                    {
                        if let Some(ClientShellOverlay::Extensions(extensions)) =
                            self.overlay.as_mut()
                        {
                            extensions.git_install_mode = true;
                            extensions.git_inspection = None;
                            extensions.query = format!("{} ", extension.repository);
                            extensions.search_focused = true;
                            extensions.message = Some(
                                "Update: enter the new exact commit and optional package path"
                                    .into(),
                            );
                        }
                    } else {
                        self.set_extension_confirmation(ClientExtensionConfirmation::Update);
                    }
                }
            }
            KeyCode::Char('b')
                if modifiers.is_empty()
                    && self.selected_extension().is_some_and(|extension| {
                        matches!(
                            extension.package_kind,
                            crate::api::schema::ExtensionPackageKind::AgentIntegration
                                | crate::api::schema::ExtensionPackageKind::SandboxedPackage
                        ) && extension.rollback_version.is_some()
                    }) =>
            {
                self.set_extension_confirmation(ClientExtensionConfirmation::Rollback);
            }
            _ => {}
        }
        outcome.repaint = true;
        true
    }

    fn git_package_extension(
        package: crate::api::schema::InstalledExtensionPackageInfo,
    ) -> crate::api::schema::ExtensionInfo {
        crate::api::schema::ExtensionInfo {
            id: package.package_id.clone(),
            repository: package.source.url,
            source_commit: Some(package.source.commit),
            source_sha256: Some(package.package_sha256),
            manifest_sha256: Some(package.manifest_sha256),
            manifest_path: Some(package.source.package_path),
            name: package.package_id,
            summary: "Pinned Git extension package".into(),
            details: "Executable services use the constrained WASI component runtime.".into(),
            categories: Vec::new(),
            requested_capabilities: package.requested_capabilities,
            package_kind: crate::api::schema::ExtensionPackageKind::SandboxedPackage,
            integration_target: None,
            integration_status: None,
            available_version: package.version.clone(),
            active_version: package.version,
            installed: !package.removed_retaining_data,
            enabled: package.enabled,
            install_supported: true,
            artifact_verified: true,
            repository_override_supported: false,
            load_order: package.load_order,
            pending_version: None,
            rollback_version: package.rollback_version,
            activation_error: package.activation_error,
            repository_overrides: Vec::new(),
        }
    }

    fn inspected_git_extension(
        inspection: &crate::api::schema::ExtensionPackageInspection,
        installed: bool,
    ) -> crate::api::schema::ExtensionInfo {
        crate::api::schema::ExtensionInfo {
            id: inspection.package_id.clone(),
            repository: inspection.source.url.clone(),
            source_commit: Some(inspection.source.commit.clone()),
            source_sha256: Some(inspection.package_sha256.clone()),
            manifest_sha256: Some(inspection.manifest_sha256.clone()),
            manifest_path: Some(inspection.source.package_path.clone()),
            name: inspection.package_id.clone(),
            summary: "Pinned Git extension package".into(),
            details: format!(
                "Component services: {} · namespaces: {}",
                inspection.services.join(", "),
                inspection.namespaces.join(", ")
            ),
            categories: Vec::new(),
            requested_capabilities: inspection.requested_capabilities.clone(),
            package_kind: crate::api::schema::ExtensionPackageKind::SandboxedPackage,
            integration_target: None,
            integration_status: None,
            available_version: inspection.version.clone(),
            active_version: inspection.version.clone(),
            installed,
            enabled: installed,
            install_supported: true,
            artifact_verified: true,
            repository_override_supported: false,
            load_order: usize::MAX,
            pending_version: None,
            rollback_version: None,
            activation_error: None,
            repository_overrides: Vec::new(),
        }
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
                            let mut listed = listed;
                            listed.extend(
                                extensions
                                    .extensions
                                    .iter()
                                    .filter(|extension| {
                                        extension.package_kind
                                            == crate::api::schema::ExtensionPackageKind::SandboxedPackage
                                    })
                                    .cloned(),
                            );
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
            PendingEndpointKind::ExtensionPackageList(generation) => {
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
                    match result {
                        Ok(crate::api::schema::ResponseResult::ExtensionPackageList {
                            packages,
                        }) => {
                            extensions.extensions.retain(|extension| {
                                extension.package_kind
                                    != crate::api::schema::ExtensionPackageKind::SandboxedPackage
                            });
                            extensions
                                .extensions
                                .extend(packages.into_iter().map(Self::git_package_extension));
                        }
                        Ok(_) => extensions.message = Some("Unexpected package response.".into()),
                        Err(_) => {}
                    }
                }
                (true, Vec::new())
            }
            PendingEndpointKind::ExtensionPackageInspect(generation) => {
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
                    match result {
                        Ok(crate::api::schema::ResponseResult::ExtensionPackageInspected {
                            inspection,
                        }) => {
                            let installed = extensions.extensions.iter().any(|extension| {
                                extension.id == inspection.package_id && extension.installed
                            });
                            let preview = Self::inspected_git_extension(&inspection, installed);
                            if let Some(existing) = extensions
                                .extensions
                                .iter_mut()
                                .find(|extension| extension.id == preview.id)
                            {
                                *existing = preview.clone();
                            } else {
                                extensions.extensions.push(preview.clone());
                            }
                            extensions.selected = extensions
                                .filtered_indices()
                                .iter()
                                .position(|index| extensions.extensions[*index].id == preview.id)
                                .unwrap_or(0);
                            extensions.git_inspection = Some(inspection);
                            extensions.git_install_mode = false;
                            extensions.confirmation = Some(if installed {
                                ClientExtensionConfirmation::Update
                            } else {
                                ClientExtensionConfirmation::Install
                            });
                            extensions.confirmation_extension = Some(Box::new(preview));
                            extensions.message = Some(
                                "Review the exact source, revision, and capabilities; Enter approves."
                                    .into(),
                            );
                        }
                        Ok(_) => {
                            extensions.message = Some("Unexpected inspection response.".into())
                        }
                        Err(error) => extensions.message = Some(error.message),
                    }
                }
                (true, Vec::new())
            }
            PendingEndpointKind::ExtensionPackageMutation(generation) => {
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
                    extensions.mutating = false;
                    match result {
                        Ok(crate::api::schema::ResponseResult::ExtensionPackageChanged {
                            package,
                        }) => {
                            let changed = Self::git_package_extension(package);
                            if let Some(existing) = extensions
                                .extensions
                                .iter_mut()
                                .find(|extension| extension.id == changed.id)
                            {
                                *existing = changed;
                            } else {
                                extensions.extensions.push(changed);
                            }
                            extensions.git_inspection = None;
                            extensions.message = Some("Extension package updated.".into());
                        }
                        Ok(_) => extensions.message = Some("Unexpected package response.".into()),
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

#[cfg(test)]
mod package_inventory_tests {
    use super::ClientShellState;

    #[test]
    fn installed_git_package_is_presented_as_a_sandboxed_package() {
        let package = crate::api::schema::InstalledExtensionPackageInfo {
            package_id: "vqro.spaces".into(),
            version: "1.0.0".into(),
            source: crate::api::schema::ExtensionPackageSource {
                url: "https://github.com/ttenneb/vqro-extension-spaces.git".into(),
                commit: "a".repeat(40),
                package_path: "dist/vqro-spaces.vqrox".into(),
            },
            package_sha256: "b".repeat(64),
            manifest_sha256: "c".repeat(64),
            enabled: true,
            removed_retaining_data: false,
            load_order: 0,
            requested_capabilities: Vec::new(),
            rollback_version: None,
            activation_error: None,
        };

        let extension = ClientShellState::git_package_extension(package);
        assert_eq!(
            extension.package_kind,
            crate::api::schema::ExtensionPackageKind::SandboxedPackage
        );
        assert!(extension.artifact_verified);
        assert_eq!(extension.source_sha256, Some("b".repeat(64)));
        assert_eq!(extension.manifest_sha256, Some("c".repeat(64)));
        assert!(extension.requested_capabilities.is_empty());
    }
}
