use crate::api::schema::{
    ExtensionConfirmedParams, ExtensionInfo, ExtensionListParams, ExtensionReorderParams,
    ExtensionRepositoryOverrideInfo, ExtensionRepositoryOverrideParams, ExtensionSetEnabledParams,
    ExtensionUpdateParams, ResponseResult,
};
use crate::app::App;
use crate::extensions::{CuratedExtension, ExtensionPackage};

use super::responses::{encode_error, encode_success};

impl App {
    fn extension_info(&self, extension: &CuratedExtension) -> ExtensionInfo {
        let state = self
            .extension_state
            .entries
            .get(&extension.repository)
            .expect("extension state is normalized against its bundled index");
        let (package_kind, integration_target) = match &extension.package {
            ExtensionPackage::Bundled { .. } => ("bundled", None),
            ExtensionPackage::AgentIntegration { target } => {
                ("agent_integration", Some(target.clone()))
            }
            ExtensionPackage::GithubPlugin { .. } => ("github_plugin", None),
        };
        ExtensionInfo {
            id: extension.id.clone(),
            repository: extension.repository.clone(),
            source_commit: extension.source_commit.clone(),
            source_sha256: extension.source_sha256.clone(),
            manifest_sha256: extension.manifest_sha256.clone(),
            name: extension.name.clone(),
            summary: extension.summary.clone(),
            details: extension.details.clone(),
            categories: extension.categories.clone(),
            package_kind: package_kind.into(),
            integration_status: integration_target.as_deref().and_then(|target| {
                let target = self.integration_target(target)?;
                crate::integration::installed_integration_statuses()
                    .into_iter()
                    .find(|status| status.target == target)
                    .map(|status| match status.state {
                        crate::integration::IntegrationStatusKind::NotInstalled => "not_installed",
                        crate::integration::IntegrationStatusKind::Current => "current",
                        crate::integration::IntegrationStatusKind::Outdated => "outdated",
                    })
                    .map(str::to_owned)
            }),
            integration_target,
            // v1 exposes lifecycle methods for forward compatibility, but the
            // host does not advertise update controls without a locally
            // retained, digest-verified artifact.
            available_version: state.active_version.clone(),
            active_version: state.active_version.clone(),
            installed: state.installed,
            enabled: state.enabled,
            install_supported: match &extension.package {
                ExtensionPackage::Bundled { .. } => true,
                ExtensionPackage::AgentIntegration { target } => {
                    self.integration_target(target).is_some()
                }
                ExtensionPackage::GithubPlugin { .. } => false,
            },
            repository_override_supported: matches!(
                &extension.package,
                ExtensionPackage::Bundled { provider } if provider == "collections"
            ),
            load_order: self
                .extension_state
                .load_order
                .iter()
                .position(|repository| repository == &extension.repository)
                .unwrap_or(usize::MAX),
            pending_version: None,
            rollback_version: None,
            activation_error: state.activation_error.clone(),
            repository_overrides: state
                .repository_overrides
                .iter()
                .map(|(repository_id, enabled)| ExtensionRepositoryOverrideInfo {
                    repository_id: repository_id.clone(),
                    enabled: *enabled,
                })
                .collect(),
        }
    }

    fn curated_extension(&self, repository: &str) -> Result<CuratedExtension, &'static str> {
        self.extension_index
            .extensions
            .iter()
            .find(|extension| extension.repository == repository)
            .cloned()
            .ok_or("extension_not_found")
    }

    fn lock_extension_state_for_mutation(
        &mut self,
    ) -> Result<Option<crate::extensions::ExtensionStateLock>, String> {
        if !self.policy.persist_plugin_registry {
            return Ok(None);
        }
        let path = crate::extensions::state_storage_path()?;
        let guard = crate::extensions::lock_state_file(&path)?;
        self.extension_state =
            crate::extensions::load_state_from_path(&path, &self.extension_index)?;
        crate::extensions::finish_extension_data_deletion(&path, &mut self.extension_state)?;
        if crate::extensions::pending_extension_lifecycle(&path)?.is_some() {
            return Err(
                "a previous extension lifecycle change is still being recovered; retry shortly"
                    .into(),
            );
        }
        self.apply_bundled_extension_state();
        Ok(Some(guard))
    }

    pub(crate) fn refresh_external_extension_state_if_due(
        &mut self,
        now: std::time::Instant,
    ) -> bool {
        if now < self.next_extension_state_refresh {
            return false;
        }
        self.next_extension_state_refresh = now + std::time::Duration::from_secs(1);
        if !self.policy.persist_plugin_registry {
            return false;
        }
        let path = match crate::extensions::state_storage_path() {
            Ok(path) => path,
            Err(error) => {
                tracing::warn!(%error, "failed to resolve extension state for refresh");
                return false;
            }
        };
        let Some(_lock) = (match crate::extensions::try_lock_state_file(&path) {
            Ok(lock) => lock,
            Err(error) => {
                tracing::warn!(%error, "failed to refresh extension state");
                return false;
            }
        }) else {
            return false;
        };
        let mut loaded = match crate::extensions::load_state_from_path(&path, &self.extension_index)
        {
            Ok(state) => state,
            Err(error) => {
                tracing::warn!(%error, "failed to reload extension state");
                return false;
            }
        };
        if let Err(error) = crate::extensions::finish_extension_data_deletion(&path, &mut loaded) {
            tracing::warn!(%error, "failed to recover extension data deletion during refresh");
            return false;
        }
        if let Some(pending) = match crate::extensions::pending_extension_lifecycle(&path) {
            Ok(pending) => pending,
            Err(error) => {
                tracing::warn!(%error, "failed to read extension lifecycle recovery journal");
                return false;
            }
        } {
            let Some(target) = self.integration_target(&pending.target) else {
                tracing::warn!(target = %pending.target, "discarding unrecoverable extension lifecycle journal for unknown target");
                let _ = crate::extensions::finish_extension_lifecycle(&path);
                return false;
            };
            let result = if pending.enabled {
                crate::integration::install_target(target).map(|_| ())
            } else {
                crate::integration::uninstall_target(target).map(|_| ())
            };
            let Some(state) = loaded.entries.get_mut(&pending.repository) else {
                tracing::warn!(repository = %pending.repository, "discarding unrecoverable extension lifecycle journal for missing state");
                let _ = crate::extensions::finish_extension_lifecycle(&path);
                return false;
            };
            state.installed = pending.installed;
            state.enabled = pending.enabled && result.is_ok();
            state.activation_error = result.as_ref().err().map(ToString::to_string);
            if let Err(error) = crate::extensions::save_state_to_path(&path, &loaded) {
                tracing::warn!(%error, "failed to persist recovered extension lifecycle");
                return false;
            }
            if let Err(error) = crate::extensions::finish_extension_lifecycle(&path) {
                tracing::warn!(%error, "failed to finish extension lifecycle recovery");
                return false;
            }
        }
        let mut migrated = false;
        for repository in &self.state.repositories {
            let identities =
                crate::repository::extension_repository_identities(&repository.git_common_dir);
            if let Some((current, previous)) = identities.split_first() {
                for previous in previous {
                    migrated |= loaded.migrate_repository_override(previous, current);
                }
            }
        }
        if migrated {
            if let Err(error) = crate::extensions::save_state_to_path(&path, &loaded) {
                tracing::warn!(%error, "failed to persist repository override identity migration");
                return false;
            }
        }
        if loaded == self.extension_state {
            return false;
        }
        self.extension_state = loaded;
        self.apply_bundled_extension_state();
        self.rebuild_extension_catalogs();
        self.refresh_collection_documents();
        true
    }

    fn begin_agent_lifecycle(
        &self,
        extension: &CuratedExtension,
        lock: Option<&crate::extensions::ExtensionStateLock>,
        installed: bool,
        enabled: bool,
    ) -> Result<bool, String> {
        let (ExtensionPackage::AgentIntegration { target }, Some(lock)) =
            (&extension.package, lock)
        else {
            return Ok(false);
        };
        if self.integration_target(target).is_none() {
            return Ok(false);
        }
        crate::extensions::begin_extension_lifecycle(
            lock.path(),
            &crate::extensions::PendingExtensionLifecycle {
                repository: extension.repository.clone(),
                target: target.clone(),
                installed,
                enabled,
            },
        )?;
        Ok(true)
    }

    fn restore_agent_lifecycle_journal(
        &self,
        extension: &CuratedExtension,
        lock: Option<&crate::extensions::ExtensionStateLock>,
        previous: &crate::extensions::ExtensionStateFile,
        journaled: bool,
    ) {
        if !journaled {
            return;
        }
        if let Some(state) = previous.entries.get(&extension.repository) {
            let _ = self.begin_agent_lifecycle(extension, lock, state.installed, state.enabled);
        }
    }

    fn finish_agent_lifecycle(
        &self,
        lock: Option<&crate::extensions::ExtensionStateLock>,
        journaled: bool,
    ) -> Result<(), String> {
        if journaled {
            crate::extensions::finish_extension_lifecycle(
                lock.expect("journal requires state lock").path(),
            )?;
        }
        Ok(())
    }

    fn save_extension_state(
        &self,
        lock: Option<&crate::extensions::ExtensionStateLock>,
    ) -> Result<(), String> {
        if self.policy.persist_plugin_registry {
            let path = match lock {
                Some(lock) => lock.path().to_path_buf(),
                None => crate::extensions::state_storage_path()?,
            };
            crate::extensions::save_state_to_path(&path, &self.extension_state)?;
        }
        Ok(())
    }

    fn restore_extension_state(
        &mut self,
        previous: crate::extensions::ExtensionStateFile,
        extension: &CuratedExtension,
    ) {
        let should_be_active = previous
            .entries
            .get(&extension.repository)
            .is_some_and(|state| state.installed && state.enabled);
        self.extension_state = previous;
        let result = if should_be_active {
            self.activate_curated_extension(extension)
        } else {
            self.deactivate_curated_extension(extension)
        };
        if let Err(error) = result {
            tracing::warn!(extension = %extension.id, %error, "failed to compensate extension lifecycle mutation");
        }
    }

    fn repository_identity_for_workspace(&self, ws_idx: usize) -> Option<String> {
        let workspace = self.state.workspaces.get(ws_idx)?;
        let repository = self
            .state
            .repositories
            .iter()
            .find(|repository| repository.checkout_workspace_ids.contains(&workspace.id))?;
        std::fs::read_to_string(
            repository
                .git_common_dir
                .join("vqro-extension-repository-identity"),
        )
        .ok()
        .and_then(|identity| identity.lines().next().map(str::trim).map(str::to_owned))
        .filter(|identity| !identity.is_empty())
    }

    pub(crate) fn bundled_extension_enabled(&self, provider: &str, ws_idx: Option<usize>) -> bool {
        let namespace = format!("vqro.{provider}");
        if self.configured_disabled_extensions.contains(&namespace) {
            return false;
        }
        let Some(extension) = self.extension_index.extensions.iter().find(|extension| {
            matches!(&extension.package, ExtensionPackage::Bundled { provider: candidate } if candidate == provider)
        }) else {
            return false;
        };
        let repository_identity =
            ws_idx.and_then(|index| self.repository_identity_for_workspace(index));
        self.extension_state
            .enabled_for_repository(&extension.repository, repository_identity.as_deref())
    }

    pub(crate) fn apply_bundled_extension_state(&mut self) {
        let mut disabled_extensions = crate::extensions::unavailable_bundled_providers(
            &self.extension_index,
            &self.extension_state,
        );
        disabled_extensions.extend(self.configured_disabled_extensions.iter().cloned());
        if self.disabled_extensions != disabled_extensions {
            self.disabled_extensions = disabled_extensions;
            self.rebuild_extension_catalogs();
        }
    }

    fn integration_target(&self, label: &str) -> Option<crate::api::schema::IntegrationTarget> {
        crate::integration::integration_recommendations()
            .into_iter()
            .find(|recommendation| {
                serde_json::to_value(recommendation.target)
                    .ok()
                    .and_then(|value| value.as_str().map(str::to_owned))
                    .is_some_and(|target| target.eq_ignore_ascii_case(label))
            })
            .map(|recommendation| recommendation.target)
    }

    fn activate_curated_extension(&mut self, extension: &CuratedExtension) -> Result<(), String> {
        match &extension.package {
            ExtensionPackage::Bundled { .. } => {
                self.apply_bundled_extension_state();
                Ok(())
            }
            ExtensionPackage::AgentIntegration { target } => {
                let target = self.integration_target(target).ok_or_else(|| {
                    format!("agent integration '{target}' is not available through this endpoint")
                })?;
                crate::integration::install_target(target)
                    .map(|_| ())
                    .map_err(|error| error.to_string())
            }
            ExtensionPackage::GithubPlugin { .. } => {
                Err("GitHub package installation is not available through extension v1".into())
            }
        }
    }

    fn deactivate_curated_extension(&mut self, extension: &CuratedExtension) -> Result<(), String> {
        match &extension.package {
            ExtensionPackage::Bundled { .. } => {
                self.apply_bundled_extension_state();
                Ok(())
            }
            ExtensionPackage::AgentIntegration { target } => {
                let Some(target) = self.integration_target(target) else {
                    return Ok(());
                };
                crate::integration::uninstall_target(target)
                    .map(|_| ())
                    .map_err(|error| error.to_string())
            }
            ExtensionPackage::GithubPlugin { .. } => Ok(()),
        }
    }

    pub(super) fn handle_extension_list(&self, id: String, params: ExtensionListParams) -> String {
        let mut extensions = self
            .extension_index
            .search(
                params.query.as_deref().unwrap_or_default(),
                params.category.as_deref(),
            )
            .into_iter()
            .map(|extension| self.extension_info(extension))
            .filter(|extension| {
                params
                    .installed
                    .is_none_or(|installed| extension.installed == installed)
            })
            .collect::<Vec<_>>();
        extensions.sort_by(|left, right| {
            left.load_order
                .cmp(&right.load_order)
                .then_with(|| left.name.cmp(&right.name))
        });
        encode_success(id, ResponseResult::ExtensionList { extensions })
    }

    pub(super) fn handle_extension_install(
        &mut self,
        id: String,
        params: ExtensionConfirmedParams,
    ) -> String {
        if !params.confirmed {
            return encode_error(
                id,
                "confirmation_required",
                "extension installation requires explicit confirmation",
            );
        }
        let _state_lock = match self.lock_extension_state_for_mutation() {
            Ok(lock) => lock,
            Err(error) => return encode_error(id, "extension_state_lock_failed", error),
        };
        let extension = match self.curated_extension(&params.repository) {
            Ok(extension) => extension,
            Err(code) => return encode_error(id, code, "extension is not in the curated index"),
        };
        let install_supported = match &extension.package {
            ExtensionPackage::Bundled { .. } => true,
            ExtensionPackage::AgentIntegration { target } => {
                self.integration_target(target).is_some()
            }
            ExtensionPackage::GithubPlugin { .. } => false,
        };
        if !install_supported {
            return encode_error(
                id,
                "extension_install_unavailable",
                "installation is not supported for this curated entry",
            );
        }
        let previous = self.extension_state.clone();
        self.extension_state.install(&extension);
        let journaled =
            match self.begin_agent_lifecycle(&extension, _state_lock.as_ref(), true, true) {
                Ok(journaled) => journaled,
                Err(error) => return encode_error(id, "extension_lifecycle_journal_failed", error),
            };
        let activation = self.activate_curated_extension(&extension);
        if let Err(error) = &activation {
            let _ = self
                .extension_state
                .finish_activation(&extension.repository, Err(error.clone()));
        } else {
            let _ = self
                .extension_state
                .finish_activation(&extension.repository, Ok(()));
        }
        if let Err(error) = self.save_extension_state(_state_lock.as_ref()) {
            self.restore_agent_lifecycle_journal(
                &extension,
                _state_lock.as_ref(),
                &previous,
                journaled,
            );
            self.restore_extension_state(previous, &extension);
            return encode_error(id, "extension_state_save_failed", error);
        }
        if let Err(error) = self.finish_agent_lifecycle(_state_lock.as_ref(), journaled) {
            return encode_error(id, "extension_lifecycle_journal_failed", error);
        }
        self.refresh_collection_documents();
        encode_success(
            id,
            ResponseResult::ExtensionChanged {
                extension: self.extension_info(&extension),
            },
        )
    }

    pub(super) fn handle_extension_enable(
        &mut self,
        id: String,
        params: ExtensionSetEnabledParams,
    ) -> String {
        let _state_lock = match self.lock_extension_state_for_mutation() {
            Ok(lock) => lock,
            Err(error) => return encode_error(id, "extension_state_lock_failed", error),
        };
        let extension = match self.curated_extension(&params.repository) {
            Ok(extension) => extension,
            Err(code) => return encode_error(id, code, "extension is not in the curated index"),
        };
        let previous = self.extension_state.clone();
        if let Err(error) = self
            .extension_state
            .set_enabled(&extension.repository, params.enabled)
        {
            return encode_error(id, "extension_state_invalid", error);
        }
        let journaled = match self.begin_agent_lifecycle(
            &extension,
            _state_lock.as_ref(),
            true,
            params.enabled,
        ) {
            Ok(journaled) => journaled,
            Err(error) => return encode_error(id, "extension_lifecycle_journal_failed", error),
        };
        let activation = if params.enabled {
            self.activate_curated_extension(&extension)
        } else {
            self.deactivate_curated_extension(&extension)
        };
        if let Err(error) = activation {
            if params.enabled {
                let _ = self
                    .extension_state
                    .finish_activation(&extension.repository, Err(error));
            } else {
                self.restore_extension_state(previous, &extension);
                let _ = self.finish_agent_lifecycle(_state_lock.as_ref(), journaled);
                return encode_error(id, "extension_deactivation_failed", error);
            }
        }
        if let Err(error) = self.save_extension_state(_state_lock.as_ref()) {
            self.restore_agent_lifecycle_journal(
                &extension,
                _state_lock.as_ref(),
                &previous,
                journaled,
            );
            self.restore_extension_state(previous, &extension);
            return encode_error(id, "extension_state_save_failed", error);
        }
        if let Err(error) = self.finish_agent_lifecycle(_state_lock.as_ref(), journaled) {
            return encode_error(id, "extension_lifecycle_journal_failed", error);
        }
        self.refresh_collection_documents();
        encode_success(
            id,
            ResponseResult::ExtensionChanged {
                extension: self.extension_info(&extension),
            },
        )
    }

    pub(super) fn handle_extension_remove(
        &mut self,
        id: String,
        params: ExtensionConfirmedParams,
    ) -> String {
        if !params.confirmed {
            return encode_error(
                id,
                "confirmation_required",
                "extension removal requires explicit confirmation",
            );
        }
        let _state_lock = match self.lock_extension_state_for_mutation() {
            Ok(lock) => lock,
            Err(error) => return encode_error(id, "extension_state_lock_failed", error),
        };
        let extension = match self.curated_extension(&params.repository) {
            Ok(extension) => extension,
            Err(code) => return encode_error(id, code, "extension is not in the curated index"),
        };
        let previous = self.extension_state.clone();
        if let Err(error) = self.extension_state.remove(&extension.repository) {
            return encode_error(id, "extension_state_invalid", error);
        }
        let journaled =
            match self.begin_agent_lifecycle(&extension, _state_lock.as_ref(), false, false) {
                Ok(journaled) => journaled,
                Err(error) => return encode_error(id, "extension_lifecycle_journal_failed", error),
            };
        if let Err(error) = self.deactivate_curated_extension(&extension) {
            self.restore_extension_state(previous, &extension);
            let _ = self.finish_agent_lifecycle(_state_lock.as_ref(), journaled);
            return encode_error(id, "extension_deactivation_failed", error);
        }
        if let Err(error) = self.save_extension_state(_state_lock.as_ref()) {
            self.restore_agent_lifecycle_journal(
                &extension,
                _state_lock.as_ref(),
                &previous,
                journaled,
            );
            self.restore_extension_state(previous, &extension);
            return encode_error(id, "extension_state_save_failed", error);
        }
        if let Err(error) = self.finish_agent_lifecycle(_state_lock.as_ref(), journaled) {
            return encode_error(id, "extension_lifecycle_journal_failed", error);
        }
        self.refresh_collection_documents();
        encode_success(
            id,
            ResponseResult::ExtensionChanged {
                extension: self.extension_info(&extension),
            },
        )
    }

    pub(super) fn handle_extension_data_delete(
        &mut self,
        id: String,
        params: ExtensionConfirmedParams,
    ) -> String {
        if !params.confirmed {
            return encode_error(
                id,
                "confirmation_required",
                "extension data deletion requires explicit confirmation",
            );
        }
        let _state_lock = match self.lock_extension_state_for_mutation() {
            Ok(lock) => lock,
            Err(error) => return encode_error(id, "extension_state_lock_failed", error),
        };
        let extension = match self.curated_extension(&params.repository) {
            Ok(extension) => extension,
            Err(code) => return encode_error(id, code, "extension is not in the curated index"),
        };
        if self.extension_state.entries[&extension.repository].installed {
            return encode_error(
                id,
                "extension_still_installed",
                "remove the extension before deleting its data",
            );
        }
        let data_id = self.extension_state.entries[&extension.repository]
            .data_id
            .clone();
        if !self.policy.persist_plugin_registry {
            if let Err(error) = crate::extensions::purge_extension_data(&data_id) {
                return encode_error(id, "extension_data_delete_failed", error);
            }
            if let Err(error) = self
                .extension_state
                .clear_retained_data(&extension.repository)
            {
                return encode_error(id, "extension_state_invalid", error);
            }
            return encode_success(
                id,
                ResponseResult::ExtensionDataDeleted {
                    repository: extension.repository,
                },
            );
        }
        let state_path = match crate::extensions::state_storage_path() {
            Ok(path) => path,
            Err(error) => return encode_error(id, "extension_state_path_failed", error),
        };
        if let Err(error) = crate::extensions::begin_extension_data_deletion(
            &state_path,
            &extension.repository,
            &data_id,
        ) {
            return encode_error(id, "extension_data_delete_failed", error);
        }
        if let Err(error) = crate::extensions::finish_extension_data_deletion(
            &state_path,
            &mut self.extension_state,
        ) {
            return encode_error(id, "extension_data_delete_failed", error);
        }
        encode_success(
            id,
            ResponseResult::ExtensionDataDeleted {
                repository: extension.repository,
            },
        )
    }

    pub(super) fn handle_extension_reorder(
        &mut self,
        id: String,
        params: ExtensionReorderParams,
    ) -> String {
        let _state_lock = match self.lock_extension_state_for_mutation() {
            Ok(lock) => lock,
            Err(error) => return encode_error(id, "extension_state_lock_failed", error),
        };
        let extension = match self.curated_extension(&params.repository) {
            Ok(extension) => extension,
            Err(code) => return encode_error(id, code, "extension is not in the curated index"),
        };
        let previous = self.extension_state.clone();
        if let Err(error) = self
            .extension_state
            .move_to(&extension.repository, params.index)
        {
            return encode_error(id, "extension_state_invalid", error);
        }
        self.apply_bundled_extension_state();
        self.rebuild_extension_catalogs();
        if let Err(error) = self.save_extension_state(_state_lock.as_ref()) {
            self.extension_state = previous;
            self.apply_bundled_extension_state();
            self.rebuild_extension_catalogs();
            return encode_error(id, "extension_state_save_failed", error);
        }
        encode_success(
            id,
            ResponseResult::ExtensionChanged {
                extension: self.extension_info(&extension),
            },
        )
    }

    pub(super) fn handle_extension_repository_override_set(
        &mut self,
        id: String,
        params: ExtensionRepositoryOverrideParams,
    ) -> String {
        let _state_lock = match self.lock_extension_state_for_mutation() {
            Ok(lock) => lock,
            Err(error) => return encode_error(id, "extension_state_lock_failed", error),
        };
        let extension = match self.curated_extension(&params.repository) {
            Ok(extension) => extension,
            Err(code) => return encode_error(id, code, "extension is not in the curated index"),
        };
        if !matches!(
            &extension.package,
            ExtensionPackage::Bundled { provider } if provider == "collections"
        ) {
            return encode_error(
                id,
                "extension_repository_override_unsupported",
                "this extension has no repository-scoped runtime contribution",
            );
        }
        let repository_identity_is_current = self.state.repositories.iter().any(|repository| {
            crate::repository::extension_repository_identities(&repository.git_common_dir).first()
                == Some(&params.repository_id)
        });
        if !repository_identity_is_current {
            return encode_error(
                id,
                "extension_repository_not_found",
                "repository identity is not part of this session",
            );
        }
        let previous = self.extension_state.clone();
        if let Err(error) = self.extension_state.set_repository_override(
            &extension.repository,
            params.repository_id,
            params.enabled,
        ) {
            return encode_error(id, "extension_state_invalid", error);
        }
        self.apply_bundled_extension_state();
        if let Err(error) = self.save_extension_state(_state_lock.as_ref()) {
            self.extension_state = previous;
            self.apply_bundled_extension_state();
            return encode_error(id, "extension_state_save_failed", error);
        }
        self.refresh_collection_documents();
        encode_success(
            id,
            ResponseResult::ExtensionChanged {
                extension: self.extension_info(&extension),
            },
        )
    }

    pub(super) fn handle_extension_update(
        &mut self,
        id: String,
        params: ExtensionUpdateParams,
    ) -> String {
        if let Err(code) = self.curated_extension(&params.repository) {
            return encode_error(id, code, "extension is not in the curated index");
        }
        encode_error(
            id,
            "extension_update_unavailable",
            "no digest-verified update artifact is staged",
        )
    }

    pub(super) fn handle_extension_rollback(
        &mut self,
        id: String,
        params: ExtensionConfirmedParams,
    ) -> String {
        if let Err(code) = self.curated_extension(&params.repository) {
            return encode_error(id, code, "extension is not in the curated index");
        }
        encode_error(
            id,
            "extension_rollback_unavailable",
            "no verified previous artifact is retained",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::schema::{Method, Request, SuccessResponse};

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

    fn result(response: &str) -> ResponseResult {
        serde_json::from_str::<SuccessResponse>(response)
            .expect("success response")
            .result
    }

    #[test]
    fn extension_list_exposes_exact_default_installed_set() {
        let mut app = test_app();
        let response = app.handle_api_request(Request {
            id: "extensions".into(),
            method: Method::ExtensionList(ExtensionListParams::default()),
        });
        let ResponseResult::ExtensionList { extensions } = result(&response) else {
            panic!("extension list response");
        };
        let installed = extensions
            .iter()
            .filter(|extension| extension.installed && extension.enabled)
            .map(|extension| extension.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            installed,
            [
                "Collections",
                "Spaces",
                "Delegation",
                "Agent Status",
                "Pi",
                "Claude Code",
                "Codex"
            ]
        );
        assert!(extensions
            .iter()
            .find(|extension| extension.name == "OMP")
            .is_some_and(|extension| !extension.installed));
    }

    #[test]
    fn removed_runtime_extensions_reject_their_public_methods() {
        let mut app = test_app();
        for (repository, method) in [
            (
                "ttenneb/vqro-extension-delegation",
                Method::DelegationTree(crate::api::schema::EmptyParams::default()),
            ),
            (
                "ttenneb/vqro-extension-agent-status",
                Method::AgentList(crate::api::schema::EmptyParams::default()),
            ),
        ] {
            let removed = app.handle_api_request(Request {
                id: "remove".into(),
                method: Method::ExtensionRemove(ExtensionConfirmedParams {
                    repository: repository.into(),
                    confirmed: true,
                }),
            });
            let _ = result(&removed);
            let rejected: serde_json::Value =
                serde_json::from_str(&app.handle_api_request(Request {
                    id: "runtime".into(),
                    method,
                }))
                .unwrap();
            assert_eq!(rejected["error"]["code"], "extension_disabled");
        }
    }

    #[test]
    fn unsupported_curated_agent_install_is_rejected_without_state_change() {
        let mut app = test_app();
        let repository = "vqro/bundled-integrations#letta";
        let response: serde_json::Value = serde_json::from_str(&app.handle_api_request(Request {
            id: "install".into(),
            method: Method::ExtensionInstall(ExtensionConfirmedParams {
                repository: repository.into(),
                confirmed: true,
            }),
        }))
        .unwrap();

        assert_eq!(response["error"]["code"], "extension_install_unavailable");
        assert!(!app.extension_state.entries[repository].installed);
    }

    #[test]
    fn bundled_removal_requires_confirmation_and_is_reinstallable() {
        let mut app = test_app();
        let repository = "ttenneb/vqro-extension-collections".to_string();
        let rejected: serde_json::Value = serde_json::from_str(&app.handle_api_request(Request {
            id: "remove".into(),
            method: Method::ExtensionRemove(ExtensionConfirmedParams {
                repository: repository.clone(),
                confirmed: false,
            }),
        }))
        .unwrap();
        assert_eq!(rejected["error"]["code"], "confirmation_required");

        let removed = app.handle_api_request(Request {
            id: "remove".into(),
            method: Method::ExtensionRemove(ExtensionConfirmedParams {
                repository: repository.clone(),
                confirmed: true,
            }),
        });
        assert!(matches!(
            result(&removed),
            ResponseResult::ExtensionChanged {
                extension: ExtensionInfo {
                    installed: false,
                    enabled: false,
                    ..
                }
            }
        ));
        assert!(app.disabled_extensions.contains("vqro.collections"));

        let installed = app.handle_api_request(Request {
            id: "install".into(),
            method: Method::ExtensionInstall(ExtensionConfirmedParams {
                repository,
                confirmed: true,
            }),
        });
        assert!(matches!(
            result(&installed),
            ResponseResult::ExtensionChanged {
                extension: ExtensionInfo {
                    installed: true,
                    enabled: true,
                    ..
                }
            }
        ));
        assert!(!app.disabled_extensions.contains("vqro.collections"));
    }
}
