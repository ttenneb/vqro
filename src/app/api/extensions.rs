use crate::api::schema::{
    ExtensionConfirmedParams, ExtensionInfo, ExtensionListParams, ExtensionPackageKind,
    ExtensionReorderParams, ExtensionRepositoryOverrideInfo, ExtensionRepositoryOverrideParams,
    ExtensionSetEnabledParams, ExtensionUpdateParams, IntegrationState, ResponseResult,
};
use crate::app::App;
use crate::extensions::{CuratedExtension, ExtensionPackage};

use super::responses::{encode_error, encode_success};

impl App {
    fn extension_info(&self, extension: &CuratedExtension) -> ExtensionInfo {
        let statuses = crate::integration::installed_integration_statuses();
        self.extension_info_with_statuses(extension, &statuses)
    }

    fn extension_info_with_statuses(
        &self,
        extension: &CuratedExtension,
        statuses: &[crate::integration::IntegrationStatus],
    ) -> ExtensionInfo {
        let state = self
            .marketplace_state
            .entries
            .get(&extension.repository)
            .expect("extension state is normalized against its bundled index");
        let (package_kind, integration_target) = match &extension.package {
            ExtensionPackage::Bundled { .. } => (ExtensionPackageKind::Bundled, None),
            ExtensionPackage::AgentIntegration { target } => {
                (ExtensionPackageKind::AgentIntegration, target.supported())
            }
            ExtensionPackage::GithubPlugin { .. } => (ExtensionPackageKind::GithubPlugin, None),
        };
        ExtensionInfo {
            id: extension.id.clone(),
            repository: extension.repository.clone(),
            source_commit: extension.source_commit.clone(),
            source_sha256: extension.source_sha256.clone(),
            manifest_sha256: extension.manifest_sha256.clone(),
            manifest_path: extension
                .has_pinned_source()
                .then(|| extension.manifest_path.clone()),
            name: extension.name.clone(),
            summary: extension.summary.clone(),
            details: extension.details.clone(),
            categories: extension.categories.clone(),
            package_kind,
            integration_status: integration_target.and_then(|target| {
                statuses
                    .iter()
                    .find(|status| status.target == target)
                    .map(|status| match status.state {
                        crate::integration::IntegrationStatusKind::NotInstalled => {
                            IntegrationState::NotInstalled
                        }
                        crate::integration::IntegrationStatusKind::Current => {
                            IntegrationState::Current
                        }
                        crate::integration::IntegrationStatusKind::Outdated => {
                            IntegrationState::Outdated
                        }
                    })
            }),
            integration_target,
            // v1 exposes lifecycle methods for forward compatibility, but the
            // host does not advertise update controls without a locally
            // retained, digest-verified artifact.
            available_version: if extension.has_pinned_source() {
                extension.version.clone()
            } else {
                state.active_version.clone()
            },
            active_version: state.active_version.clone(),
            installed: state.installed,
            enabled: state.enabled,
            install_supported: extension.has_pinned_source()
                && match &extension.package {
                    ExtensionPackage::Bundled { .. } => true,
                    ExtensionPackage::AgentIntegration { .. } => true,
                    ExtensionPackage::GithubPlugin { .. } => false,
                },
            artifact_verified: state
                .active_artifact
                .as_ref()
                .is_some_and(crate::extension_artifact::retained),
            repository_override_supported: matches!(
                &extension.package,
                ExtensionPackage::Bundled { provider } if provider == "collections"
            ),
            load_order: self
                .marketplace_state
                .load_order
                .iter()
                .position(|repository| repository == &extension.repository)
                .unwrap_or(usize::MAX),
            pending_version: state.pending_version.clone(),
            rollback_version: state
                .installed
                .then_some(state.rollback_artifact.as_ref())
                .flatten()
                .filter(|artifact| crate::extension_artifact::retained(artifact))
                .map(|artifact| artifact.version.clone()),
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

    fn recover_extension_data_deletion(
        &mut self,
        path: &std::path::Path,
        marketplace_state: &mut crate::extensions::MarketplaceStateFile,
    ) -> Result<bool, String> {
        let Some(namespace) =
            crate::extensions::apply_extension_data_deletion(path, marketplace_state)?
        else {
            return Ok(false);
        };
        if self.extension_state_persistence_blocked {
            return Err("authoritative extension state is unavailable".into());
        }
        if self.extension_state_transaction_pending {
            return Err("authoritative extension state is busy; retry shortly".into());
        }
        let mut candidate = self.extension_state.clone();
        let commit = candidate
            .clear_namespace(&namespace, format!("marketplace-delete-{namespace}"))
            .map_err(|error| format!("clear authoritative extension state: {error:?}"))?;
        if let Some(state_path) = self.extension_state_path.as_deref() {
            if let Err(error) =
                crate::persist::extension_state::save_atomically(state_path, &candidate)
            {
                self.extension_state_persistence_blocked = error.replaced;
                return Err(format!(
                    "persist authoritative extension state deletion: {}",
                    error.source
                ));
            }
        }
        self.extension_state = candidate;
        if let Some(commit) = commit.as_ref() {
            self.emit_extension_state_committed(commit);
        }
        crate::extensions::finish_extension_data_deletion(path)?;
        Ok(true)
    }

    pub(crate) fn recover_extension_state_on_startup(&mut self) {
        if !self.policy.persist_plugin_registry {
            return;
        }
        let result = (|| {
            let path = crate::extensions::state_storage_path()?;
            let _lock = crate::extensions::lock_state_file(&path)?;
            let mut marketplace_state =
                crate::extensions::load_state_from_path(&path, &self.extension_index)?;
            self.recover_extension_data_deletion(&path, &mut marketplace_state)?;
            self.recover_extension_lifecycle(&path, &mut marketplace_state)?;
            self.marketplace_state = marketplace_state;
            self.apply_bundled_extension_state();
            Ok::<(), String>(())
        })();
        if let Err(error) = result {
            tracing::warn!(%error, "failed to recover extension state during startup");
        }
    }

    fn recover_extension_lifecycle(
        &mut self,
        path: &std::path::Path,
        marketplace_state: &mut crate::extensions::MarketplaceStateFile,
    ) -> Result<bool, String> {
        let Some(pending) = crate::extensions::pending_extension_lifecycle(path)? else {
            return Ok(false);
        };
        let target = pending.target;
        let requires_artifact = self
            .extension_index
            .extensions
            .iter()
            .find(|extension| extension.repository == pending.repository)
            .is_some_and(CuratedExtension::has_pinned_source);
        let runtime_result = if pending.enabled {
            if let Some(artifact) = pending.active_artifact.as_ref() {
                crate::integration::install_target_from_artifact(target, artifact)
                    .map(|_| ())
                    .map_err(|error| error.to_string())
            } else if requires_artifact {
                // The journal is the sole authority for pre-mutation ownership. A newly
                // persisted artifact may describe the interrupted replacement and must
                // never be installed while rolling back to an artifact-free state.
                crate::integration::uninstall_target_artifact(target)
                    .map(|_| ())
                    .map_err(|error| error.to_string())
            } else {
                crate::integration::install_target(target)
                    .map(|_| ())
                    .map_err(|error| error.to_string())
            }
        } else {
            crate::integration::uninstall_target_artifact(target)
                .map(|_| ())
                .map_err(|error| error.to_string())
        };
        let state = marketplace_state
            .entries
            .get_mut(&pending.repository)
            .ok_or_else(|| {
                format!(
                    "extension lifecycle journal references missing state for '{}'",
                    pending.repository
                )
            })?;
        state.installed = pending.installed;
        state.enabled = pending.enabled && runtime_result.is_ok();
        state.activation_error = runtime_result.as_ref().err().map(ToString::to_string);
        state.active_artifact = pending.active_artifact;
        state.rollback_artifact = pending.rollback_artifact;
        if let Some(version) = pending.active_version {
            state.active_version = version;
        }
        state.rollback_version = state
            .rollback_artifact
            .as_ref()
            .map(|artifact| artifact.version.clone());
        if !pending.load_order.is_empty() {
            marketplace_state.load_order = pending.load_order;
        }
        crate::extensions::save_state_to_path(path, marketplace_state)?;
        if let Err(error) = runtime_result {
            // Keep the journal so the next refresh retries the rollback. The persisted
            // state remains disabled and records the failure instead of activating an
            // artifact whose ownership differs from the journal.
            tracing::warn!(%error, "extension lifecycle rollback remains pending");
            return Ok(true);
        }
        crate::extensions::finish_extension_lifecycle(path)?;
        Ok(true)
    }

    fn adopt_external_marketplace_state(
        &mut self,
        marketplace_state: crate::extensions::MarketplaceStateFile,
    ) -> bool {
        if marketplace_state == self.marketplace_state {
            return false;
        }
        let load_order_changed = self.marketplace_state.load_order != marketplace_state.load_order;
        self.marketplace_state = marketplace_state;
        let rebuilt_for_enablement = self.apply_bundled_extension_state();
        if load_order_changed && !rebuilt_for_enablement {
            self.rebuild_extension_catalogs();
        }
        true
    }

    fn lock_extension_state_for_mutation(
        &mut self,
    ) -> Result<Option<crate::extensions::ExtensionStateLock>, String> {
        if !self.policy.persist_plugin_registry {
            return Ok(None);
        }
        let path = crate::extensions::state_storage_path()?;
        let guard = crate::extensions::try_lock_state_file(&path)?
            .ok_or_else(|| "extension state is busy; retry shortly".to_string())?;
        let mut marketplace_state =
            crate::extensions::load_state_from_path(&path, &self.extension_index)?;
        self.recover_extension_data_deletion(&path, &mut marketplace_state)?;
        if crate::extensions::pending_extension_lifecycle(&path)?.is_some() {
            return Err(
                "a previous extension lifecycle change is still being recovered; retry shortly"
                    .into(),
            );
        }
        self.adopt_external_marketplace_state(marketplace_state);
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
        if let Err(error) = self.recover_extension_data_deletion(&path, &mut loaded) {
            tracing::warn!(%error, "failed to recover extension data deletion during refresh");
            return false;
        }
        if let Err(error) = self.recover_extension_lifecycle(&path, &mut loaded) {
            tracing::warn!(%error, "failed to recover extension lifecycle during refresh");
            return false;
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
        if !self.adopt_external_marketplace_state(loaded) {
            return false;
        }
        self.refresh_collection_documents();
        true
    }

    fn begin_agent_lifecycle(
        &self,
        extension: &CuratedExtension,
        lock: Option<&crate::extensions::ExtensionStateLock>,
        recovery_state: &crate::extensions::MarketplaceStateFile,
    ) -> Result<bool, String> {
        let (ExtensionPackage::AgentIntegration { target }, Some(lock)) =
            (&extension.package, lock)
        else {
            return Ok(false);
        };
        let Some(target) = target.supported() else {
            return Ok(false);
        };
        let state = recovery_state
            .entries
            .get(&extension.repository)
            .ok_or_else(|| "extension recovery state is missing".to_string())?;
        // This is a rollback journal. Until the new state is durably saved and
        // the journal is removed, recovery restores the pre-mutation state.
        crate::extensions::begin_extension_lifecycle(
            lock.path(),
            &crate::extensions::PendingExtensionLifecycle {
                repository: extension.repository.clone(),
                target,
                installed: state.installed,
                enabled: state.enabled,
                active_artifact: state.active_artifact.clone(),
                rollback_artifact: state.rollback_artifact.clone(),
                active_version: Some(state.active_version.clone()),
                load_order: recovery_state.load_order.clone(),
            },
        )?;
        Ok(true)
    }

    fn compensate_failed_state_save(
        &mut self,
        extension: &CuratedExtension,
        lock: Option<&crate::extensions::ExtensionStateLock>,
        previous: crate::extensions::MarketplaceStateFile,
        journaled: bool,
        save_error: String,
    ) -> String {
        match self.try_restore_extension_state(previous, extension) {
            Ok(()) => match self.finish_agent_lifecycle(lock, journaled) {
                Ok(()) => save_error,
                Err(error) => format!("{save_error}; failed to clear rollback journal: {error}"),
            },
            Err(error) => format!(
                "{save_error}; runtime compensation failed and rollback remains pending: {error}"
            ),
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
            crate::extensions::save_state_to_path(&path, &self.marketplace_state)?;
        }
        Ok(())
    }

    fn try_restore_extension_state(
        &mut self,
        previous: crate::extensions::MarketplaceStateFile,
        extension: &CuratedExtension,
    ) -> Result<(), String> {
        let should_be_active = previous
            .entries
            .get(&extension.repository)
            .is_some_and(|state| state.installed && state.enabled);
        self.marketplace_state = previous;
        if should_be_active {
            self.activate_curated_extension(extension)
        } else {
            self.deactivate_curated_extension(extension)
        }
        .map_err(|error| error.to_string())
    }

    fn repository_identity(&self, repository: &crate::repository::Repository) -> Option<String> {
        repository
            .checkout_workspace_ids
            .iter()
            .filter_map(|workspace_id| {
                self.state
                    .workspaces
                    .iter()
                    .find(|workspace| &workspace.id == workspace_id)
            })
            .filter_map(|workspace| self.git_status_cache.get(&workspace.cached_git_status_key))
            .filter_map(|entry| entry.fingerprint.as_ref())
            .find_map(|fingerprint| {
                crate::repository::canonical_repository_web_url(
                    fingerprint.preferred_repository_remote_url()?,
                )
            })
            .or_else(|| Some(repository.id.clone()))
    }

    fn repository_identity_for_workspace(&self, ws_idx: usize) -> Option<String> {
        let workspace = self.state.workspaces.get(ws_idx)?;
        let repository = self
            .state
            .repositories
            .iter()
            .find(|repository| repository.checkout_workspace_ids.contains(&workspace.id))?;
        self.repository_identity(repository)
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
        self.marketplace_state
            .enabled_for_repository(&extension.repository, repository_identity.as_deref())
    }

    pub(crate) fn apply_bundled_extension_state(&mut self) -> bool {
        let mut disabled_extensions = crate::extensions::unavailable_bundled_providers(
            &self.extension_index,
            &self.marketplace_state,
        );
        disabled_extensions.extend(self.configured_disabled_extensions.iter().cloned());
        if self.disabled_extensions != disabled_extensions {
            self.disabled_extensions = disabled_extensions;
            self.rebuild_extension_catalogs();
            true
        } else {
            false
        }
    }

    fn activate_curated_extension(&mut self, extension: &CuratedExtension) -> Result<(), String> {
        self.activate_curated_extension_with_asset(extension, None)
    }

    fn activate_curated_extension_with_asset(
        &mut self,
        extension: &CuratedExtension,
        staged_asset: Option<&[u8]>,
    ) -> Result<(), String> {
        match &extension.package {
            ExtensionPackage::Bundled { .. } => {
                self.apply_bundled_extension_state();
                Ok(())
            }
            ExtensionPackage::AgentIntegration { target } => {
                let target = target.supported().ok_or_else(|| {
                    "this curated agent integration has no supported adapter".to_string()
                })?;
                if let Some(asset) = staged_asset {
                    crate::integration::install_target_from_asset(target, asset)
                        .map(|_| ())
                        .map_err(|error| error.to_string())
                } else if let Some(artifact) = self
                    .marketplace_state
                    .entries
                    .get(&extension.repository)
                    .and_then(|state| state.active_artifact.as_ref())
                {
                    crate::integration::install_target_from_artifact(target, artifact)
                        .map(|_| ())
                        .map_err(|error| error.to_string())
                } else if extension.has_pinned_source() {
                    Err(
                        "verified Vqro source artifact is not retained; verify the pinned package before enabling it"
                            .into(),
                    )
                } else {
                    Err("feature_disabled: this adapter has no Vqro extension artifact".into())
                }
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
                let Some(target) = target.supported() else {
                    return Ok(());
                };
                let marketplace_managed = self
                    .marketplace_state
                    .entries
                    .get(&extension.repository)
                    .and_then(|state| state.active_artifact.as_ref())
                    .is_some();
                if !marketplace_managed {
                    return Ok(());
                }
                crate::integration::uninstall_target_artifact(target)
                    .map(|_| ())
                    .map_err(|error| error.to_string())
            }
            ExtensionPackage::GithubPlugin { .. } => Ok(()),
        }
    }

    pub(crate) fn extension_artifact_request_requires_staging(
        &self,
        request: &crate::api::schema::Request,
    ) -> bool {
        let repository = match &request.method {
            crate::api::schema::Method::ExtensionInstall(params) => &params.repository,
            crate::api::schema::Method::ExtensionUpdate(params) => &params.repository,
            _ => return false,
        };
        self.extension_index
            .extensions
            .iter()
            .find(|extension| &extension.repository == repository)
            .is_some_and(|extension| {
                extension.has_pinned_source()
                    && matches!(
                        &extension.package,
                        ExtensionPackage::AgentIntegration { .. }
                    )
            })
    }

    pub(crate) fn start_deferred_extension_artifact_request(
        &self,
        request: crate::api::schema::Request,
        respond_to: std::sync::mpsc::Sender<String>,
    ) {
        let id = request.id;
        let (repository, action) = match request.method {
            crate::api::schema::Method::ExtensionInstall(params) if params.confirmed => (
                params.repository,
                crate::extension_artifact::ExtensionArtifactAction::Install,
            ),
            crate::api::schema::Method::ExtensionUpdate(params) if params.approved => (
                params.repository,
                crate::extension_artifact::ExtensionArtifactAction::Update {
                    version: params.version,
                },
            ),
            crate::api::schema::Method::ExtensionInstall(_) => {
                let _ = respond_to.send(encode_error(
                    id,
                    "confirmation_required",
                    "extension installation requires explicit confirmation",
                ));
                return;
            }
            crate::api::schema::Method::ExtensionUpdate(_) => {
                let _ = respond_to.send(encode_error(
                    id,
                    "confirmation_required",
                    "extension update requires explicit approval",
                ));
                return;
            }
            _ => {
                let _ = respond_to.send(encode_error(
                    id,
                    "extension_artifact_invalid",
                    "request does not stage an extension artifact",
                ));
                return;
            }
        };
        let Some(extension) = self
            .extension_index
            .extensions
            .iter()
            .find(|extension| extension.repository == repository)
            .cloned()
        else {
            let _ = respond_to.send(encode_error(
                id,
                "extension_not_found",
                "extension is not in the curated index",
            ));
            return;
        };
        if let crate::extension_artifact::ExtensionArtifactAction::Update { version } = &action {
            if !matches!(
                &extension.package,
                ExtensionPackage::AgentIntegration { .. }
            ) {
                let _ = respond_to.send(encode_error(
                    id,
                    "extension_update_unavailable",
                    "native bundled providers are updated with the host binary",
                ));
                return;
            }
            if version != &extension.version {
                let _ = respond_to.send(encode_error(
                    id,
                    "extension_version_mismatch",
                    "requested update does not match the pinned catalog version",
                ));
                return;
            }
        }
        let Some(expected_state) = self.marketplace_state.entries.get(&repository).cloned() else {
            let _ = respond_to.send(encode_error(
                id,
                "extension_state_invalid",
                "extension state is missing",
            ));
            return;
        };
        if matches!(
            action,
            crate::extension_artifact::ExtensionArtifactAction::Update { .. }
        ) && !expected_state.installed
        {
            let _ = respond_to.send(encode_error(
                id,
                "extension_not_installed",
                "install the extension before updating it",
            ));
            return;
        }
        let Some(stage_slot) = crate::extension_artifact::ExtensionArtifactStageSlot::acquire()
        else {
            let _ = respond_to.send(encode_error(
                id,
                "extension_artifact_busy",
                "too many extension artifacts are already being staged",
            ));
            return;
        };
        let event_tx = self.event_tx.clone();
        std::thread::spawn(move || {
            let _stage_slot = stage_slot;
            let staged = crate::extension_artifact::stage(&extension);
            let activation_asset = match (&staged, &extension.package) {
                (Ok(staged), ExtensionPackage::AgentIntegration { target }) => target
                    .supported()
                    .ok_or_else(|| "unsupported integration has no artifact asset".to_string())
                    .and_then(|target| {
                        crate::integration::artifact_asset_path(target)
                            .map_err(|error| error.to_string())
                    })
                    .and_then(|path| staged.read_file(path))
                    .map(Some),
                _ => Ok(None),
            };
            let _ = event_tx.blocking_send(crate::events::AppEvent::ExtensionArtifactStaged(
                Box::new(crate::extension_artifact::ExtensionArtifactStageResult {
                    id,
                    repository,
                    action,
                    expected_state,
                    staged,
                    activation_asset,
                    respond_to,
                }),
            ));
        });
    }

    pub(crate) fn handle_extension_artifact_staged(
        &mut self,
        result: crate::extension_artifact::ExtensionArtifactStageResult,
    ) {
        let respond_to = result.respond_to.clone();
        let response = self.finish_extension_artifact_staged(result);
        let _ = respond_to.send(response);
    }

    fn finish_extension_artifact_staged(
        &mut self,
        result: crate::extension_artifact::ExtensionArtifactStageResult,
    ) -> String {
        let activation_asset = match result.activation_asset.clone() {
            Ok(asset) => asset,
            Err(error) => {
                return encode_error(result.id.clone(), "extension_artifact_asset_invalid", error);
            }
        };
        let staged = match result.staged {
            Ok(staged) => staged,
            Err(error) => {
                return encode_error(result.id.clone(), "extension_artifact_stage_failed", error);
            }
        };
        let extension = match self.curated_extension(&result.repository) {
            Ok(extension) => extension,
            Err(code) => {
                return encode_error(
                    result.id.clone(),
                    code,
                    "extension is not in the curated index",
                );
            }
        };
        if !matches!(
            &extension.package,
            ExtensionPackage::AgentIntegration { .. }
        ) {
            return encode_error(
                result.id.clone(),
                "extension_artifact_unsupported",
                "native bundled providers update with the Vqro binary",
            );
        }
        if staged.descriptor().version != extension.version
            || staged.descriptor().commit != extension.source_commit.clone().unwrap_or_default()
        {
            return encode_error(
                result.id.clone(),
                "extension_artifact_stale",
                "staged artifact no longer matches the curated index",
            );
        }
        let state_lock = match self.lock_extension_state_for_mutation() {
            Ok(lock) => lock,
            Err(error) => {
                return encode_error(result.id.clone(), "extension_state_lock_failed", error);
            }
        };
        if self.marketplace_state.entries.get(&result.repository) != Some(&result.expected_state) {
            return encode_error(
                result.id.clone(),
                "extension_state_conflict",
                "extension changed while its pinned source was being staged; retry",
            );
        }
        // Moving a verified directory is intentionally the first operation under
        // the state lock. An unreferenced retained artifact is safe after a crash.
        let artifact =
            match crate::extension_artifact::commit(staged, &result.expected_state.data_id) {
                Ok(artifact) => artifact,
                Err(error) => {
                    return encode_error(
                        result.id.clone(),
                        "extension_artifact_commit_failed",
                        error,
                    );
                }
            };
        let previous = self.marketplace_state.clone();
        let same_active_artifact =
            result
                .expected_state
                .active_artifact
                .as_ref()
                .is_some_and(|active| {
                    active.commit == artifact.commit
                        && active.source_sha256 == artifact.source_sha256
                        && active.manifest_sha256 == artifact.manifest_sha256
                });
        let should_enable = matches!(
            result.action,
            crate::extension_artifact::ExtensionArtifactAction::Install
        ) || result.expected_state.enabled;
        let obsolete_rollback = previous
            .entries
            .get(&result.repository)
            .and_then(|state| state.rollback_artifact.clone());
        match &result.action {
            crate::extension_artifact::ExtensionArtifactAction::Install => {
                self.marketplace_state.install(&extension);
            }
            crate::extension_artifact::ExtensionArtifactAction::Update { .. } => {}
        }
        if let Some(state) = self.marketplace_state.entries.get_mut(&result.repository) {
            if matches!(
                result.action,
                crate::extension_artifact::ExtensionArtifactAction::Update { .. }
            ) && !same_active_artifact
            {
                state.rollback_artifact = state
                    .active_artifact
                    .take()
                    .filter(|artifact| crate::extension_artifact::verify(artifact).is_ok());
                state.rollback_version = state
                    .rollback_artifact
                    .as_ref()
                    .map(|artifact| artifact.version.clone());
            }
            state.active_artifact = Some(artifact);
            state.active_version = extension.version.clone();
            state.pending_version = None;
            state.activation_error = None;
        }
        let journaled = match self.begin_agent_lifecycle(&extension, state_lock.as_ref(), &previous)
        {
            Ok(journaled) => journaled,
            Err(error) => {
                self.marketplace_state = previous;
                return encode_error(
                    result.id.clone(),
                    "extension_lifecycle_journal_failed",
                    error,
                );
            }
        };
        if should_enable {
            let activation =
                self.activate_curated_extension_with_asset(&extension, activation_asset.as_deref());
            let activation = match activation {
                Ok(()) => Ok(()),
                Err(error) => match self.deactivate_curated_extension(&extension) {
                    Ok(()) => Err(error.to_string()),
                    Err(compensation) => {
                        let restored =
                            self.try_restore_extension_state(previous.clone(), &extension);
                        if restored.is_ok() {
                            let _ = self.finish_agent_lifecycle(state_lock.as_ref(), journaled);
                        }
                        let detail = match restored {
                            Ok(()) => format!(
                                "{error}; cleaning up the partial integration failed: {compensation}"
                            ),
                            Err(restore) => format!(
                                "{error}; cleanup failed: {compensation}; rollback remains pending: {restore}"
                            ),
                        };
                        return encode_error(
                            result.id.clone(),
                            "extension_activation_failed",
                            detail,
                        );
                    }
                },
            };
            let _ = self
                .marketplace_state
                .finish_activation(&extension.repository, activation);
        }
        if let Err(error) = self.save_extension_state(state_lock.as_ref()) {
            let error = self.compensate_failed_state_save(
                &extension,
                state_lock.as_ref(),
                previous,
                journaled,
                error,
            );
            return encode_error(result.id.clone(), "extension_state_save_failed", error);
        }
        if let Err(error) = self.finish_agent_lifecycle(state_lock.as_ref(), journaled) {
            return encode_error(
                result.id.clone(),
                "extension_lifecycle_journal_failed",
                error,
            );
        }
        if let Some(obsolete) = obsolete_rollback {
            let still_referenced = self
                .marketplace_state
                .entries
                .get(&result.repository)
                .is_some_and(|state| {
                    state
                        .active_artifact
                        .as_ref()
                        .map(|artifact| &artifact.path)
                        == Some(&obsolete.path)
                        || state
                            .rollback_artifact
                            .as_ref()
                            .map(|artifact| &artifact.path)
                            == Some(&obsolete.path)
                });
            if !still_referenced {
                if let Err(error) = crate::extension_artifact::purge_artifact(&obsolete) {
                    tracing::warn!(%error, "failed to purge obsolete extension rollback artifact");
                }
            }
        }
        self.refresh_collection_documents();
        encode_success(
            result.id.clone(),
            ResponseResult::ExtensionChanged {
                extension: self.extension_info(&extension),
            },
        )
    }

    pub(super) fn handle_extension_list(&self, id: String, params: ExtensionListParams) -> String {
        let statuses = crate::integration::installed_integration_statuses();
        let mut extensions = self
            .extension_index
            .search(
                params.query.as_deref().unwrap_or_default(),
                params.category.as_deref(),
            )
            .into_iter()
            .map(|extension| self.extension_info_with_statuses(extension, &statuses))
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
        let install_supported = extension.has_pinned_source()
            && match &extension.package {
                ExtensionPackage::Bundled { .. } => true,
                ExtensionPackage::AgentIntegration { .. } => true,
                ExtensionPackage::GithubPlugin { .. } => false,
            };
        if !install_supported {
            return encode_error(
                id,
                "feature_disabled",
                "this adapter cannot be installed until a verified Vqro extension artifact is published",
            );
        }
        if self.policy.persist_plugin_registry
            && extension.has_pinned_source()
            && matches!(
                &extension.package,
                ExtensionPackage::AgentIntegration { .. }
            )
        {
            return encode_error(
                id,
                "extension_artifact_staging_required",
                "pinned extension installation must be staged off the server event loop",
            );
        }
        let previous = self.marketplace_state.clone();
        self.marketplace_state.install(&extension);
        let journaled =
            match self.begin_agent_lifecycle(&extension, _state_lock.as_ref(), &previous) {
                Ok(journaled) => journaled,
                Err(error) => return encode_error(id, "extension_lifecycle_journal_failed", error),
            };
        let activation = self.activate_curated_extension(&extension);
        if let Err(error) = &activation {
            let _ = self
                .marketplace_state
                .finish_activation(&extension.repository, Err(error.clone()));
        } else {
            let _ = self
                .marketplace_state
                .finish_activation(&extension.repository, Ok(()));
        }
        if let Err(error) = self.save_extension_state(_state_lock.as_ref()) {
            let error = self.compensate_failed_state_save(
                &extension,
                _state_lock.as_ref(),
                previous,
                journaled,
                error,
            );
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
        let previous = self.marketplace_state.clone();
        if let Err(error) = self
            .marketplace_state
            .set_enabled(&extension.repository, params.enabled)
        {
            return encode_error(id, "extension_state_invalid", error);
        }
        let journaled =
            match self.begin_agent_lifecycle(&extension, _state_lock.as_ref(), &previous) {
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
                if let Err(cleanup) = self.deactivate_curated_extension(&extension) {
                    let compensation =
                        self.try_restore_extension_state(previous.clone(), &extension);
                    if compensation.is_ok() {
                        let _ = self.finish_agent_lifecycle(_state_lock.as_ref(), journaled);
                    }
                    let error = match compensation {
                        Ok(()) => format!("{error}; partial integration cleanup failed: {cleanup}"),
                        Err(restore) => format!(
                            "{error}; cleanup failed: {cleanup}; rollback remains pending: {restore}"
                        ),
                    };
                    return encode_error(id, "extension_activation_failed", error);
                }
                let _ = self
                    .marketplace_state
                    .finish_activation(&extension.repository, Err(error));
            } else {
                let compensation = self.try_restore_extension_state(previous, &extension);
                if compensation.is_ok() {
                    let _ = self.finish_agent_lifecycle(_state_lock.as_ref(), journaled);
                }
                let error = match compensation {
                    Ok(()) => error,
                    Err(compensation) => format!(
                        "{error}; restoring the integration also failed and rollback remains pending: {compensation}"
                    ),
                };
                return encode_error(id, "extension_deactivation_failed", error);
            }
        }
        if let Err(error) = self.save_extension_state(_state_lock.as_ref()) {
            let error = self.compensate_failed_state_save(
                &extension,
                _state_lock.as_ref(),
                previous,
                journaled,
                error,
            );
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
        let previous = self.marketplace_state.clone();
        if let Err(error) = self.marketplace_state.remove(&extension.repository) {
            return encode_error(id, "extension_state_invalid", error);
        }
        let journaled =
            match self.begin_agent_lifecycle(&extension, _state_lock.as_ref(), &previous) {
                Ok(journaled) => journaled,
                Err(error) => return encode_error(id, "extension_lifecycle_journal_failed", error),
            };
        if let Err(error) = self.deactivate_curated_extension(&extension) {
            let compensation = self.try_restore_extension_state(previous, &extension);
            if compensation.is_ok() {
                let _ = self.finish_agent_lifecycle(_state_lock.as_ref(), journaled);
            }
            let error = match compensation {
                Ok(()) => error,
                Err(compensation) => format!(
                    "{error}; restoring the integration also failed and rollback remains pending: {compensation}"
                ),
            };
            return encode_error(id, "extension_deactivation_failed", error);
        }
        if let Err(error) = self.save_extension_state(_state_lock.as_ref()) {
            let error = self.compensate_failed_state_save(
                &extension,
                _state_lock.as_ref(),
                previous,
                journaled,
                error,
            );
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
        if self.marketplace_state.entries[&extension.repository].installed {
            return encode_error(
                id,
                "extension_still_installed",
                "remove the extension before deleting its data",
            );
        }
        let data_id = self.marketplace_state.entries[&extension.repository]
            .data_id
            .clone();
        if !self.policy.persist_plugin_registry {
            if let Err(error) = crate::extensions::purge_extension_data(&data_id) {
                return encode_error(id, "extension_data_delete_failed", error);
            }
            if let Err(error) = self
                .marketplace_state
                .clear_retained_data(&extension.repository)
            {
                return encode_error(id, "extension_state_invalid", error);
            }
            let commit = match self
                .extension_state
                .clear_namespace(&data_id, format!("marketplace-delete-{data_id}"))
            {
                Ok(commit) => commit,
                Err(error) => {
                    return encode_error(
                        id,
                        "extension_state_invalid",
                        format!("clear authoritative extension state: {error:?}"),
                    );
                }
            };
            if let Some(commit) = commit.as_ref() {
                self.emit_extension_state_committed(commit);
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
        let mut marketplace_state = self.marketplace_state.clone();
        if let Err(error) =
            self.recover_extension_data_deletion(&state_path, &mut marketplace_state)
        {
            return encode_error(id, "extension_data_delete_failed", error);
        }
        self.marketplace_state = marketplace_state;
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
        let previous = self.marketplace_state.clone();
        if let Err(error) = self
            .marketplace_state
            .move_to(&extension.repository, params.index)
        {
            return encode_error(id, "extension_state_invalid", error);
        }
        self.apply_bundled_extension_state();
        self.rebuild_extension_catalogs();
        if let Err(error) = self.save_extension_state(_state_lock.as_ref()) {
            self.marketplace_state = previous;
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
            self.repository_identity(repository).as_ref() == Some(&params.repository_id)
        });
        if !repository_identity_is_current {
            return encode_error(
                id,
                "extension_repository_not_found",
                "repository identity is not part of this session",
            );
        }
        let previous = self.marketplace_state.clone();
        if let Err(error) = self.marketplace_state.set_repository_override(
            &extension.repository,
            params.repository_id,
            params.enabled,
        ) {
            return encode_error(id, "extension_state_invalid", error);
        }
        self.apply_bundled_extension_state();
        if let Err(error) = self.save_extension_state(_state_lock.as_ref()) {
            self.marketplace_state = previous;
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
        let extension = match self.curated_extension(&params.repository) {
            Ok(extension) => extension,
            Err(code) => return encode_error(id, code, "extension is not in the curated index"),
        };
        if !params.approved {
            return encode_error(
                id,
                "confirmation_required",
                "extension update requires explicit approval",
            );
        }
        if params.version != extension.version {
            return encode_error(
                id,
                "extension_version_mismatch",
                "requested update does not match the pinned catalog version",
            );
        }
        encode_error(
            id,
            "extension_artifact_staging_required",
            "pinned extension updates must be staged off the server event loop",
        )
    }

    pub(super) fn handle_extension_rollback(
        &mut self,
        id: String,
        params: ExtensionConfirmedParams,
    ) -> String {
        if !params.confirmed {
            return encode_error(
                id,
                "confirmation_required",
                "extension rollback requires explicit confirmation",
            );
        }
        let state_lock = match self.lock_extension_state_for_mutation() {
            Ok(lock) => lock,
            Err(error) => return encode_error(id, "extension_state_lock_failed", error),
        };
        let extension = match self.curated_extension(&params.repository) {
            Ok(extension) => extension,
            Err(code) => return encode_error(id, code, "extension is not in the curated index"),
        };
        if !matches!(
            &extension.package,
            ExtensionPackage::AgentIntegration { .. }
        ) {
            return encode_error(
                id,
                "extension_rollback_unavailable",
                "native bundled providers are rolled back with the host binary",
            );
        }
        if !self.marketplace_state.entries[&extension.repository].installed {
            return encode_error(
                id,
                "extension_rollback_unavailable",
                "install the extension before rolling it back",
            );
        }
        let Some(rollback) = self.marketplace_state.entries[&extension.repository]
            .rollback_artifact
            .clone()
        else {
            return encode_error(
                id,
                "extension_rollback_unavailable",
                "no verified previous artifact is retained",
            );
        };
        if let Err(error) = crate::extension_artifact::verify(&rollback) {
            return encode_error(id, "extension_rollback_unavailable", error);
        }
        let previous = self.marketplace_state.clone();
        let enabled = self.marketplace_state.entries[&extension.repository].enabled;
        let state = self
            .marketplace_state
            .entries
            .get_mut(&extension.repository)
            .expect("curated state is normalized");
        let active = state.active_artifact.replace(rollback.clone());
        state.rollback_artifact = active;
        state.active_version = rollback.version.clone();
        state.rollback_version = state
            .rollback_artifact
            .as_ref()
            .map(|artifact| artifact.version.clone());
        state.activation_error = None;

        let journaled = match self.begin_agent_lifecycle(&extension, state_lock.as_ref(), &previous)
        {
            Ok(journaled) => journaled,
            Err(error) => {
                self.marketplace_state = previous;
                return encode_error(id, "extension_lifecycle_journal_failed", error);
            }
        };
        if enabled {
            if let Err(error) = self.activate_curated_extension(&extension) {
                let compensation = self.try_restore_extension_state(previous.clone(), &extension);
                if compensation.is_ok() {
                    let _ = self.finish_agent_lifecycle(state_lock.as_ref(), journaled);
                }
                let message = match compensation {
                    Ok(()) => error,
                    Err(compensation) => format!(
                        "{error}; restoring the active integration also failed: {compensation}"
                    ),
                };
                return encode_error(id, "extension_activation_failed", message);
            }
        }
        if let Err(error) = self.save_extension_state(state_lock.as_ref()) {
            let error = self.compensate_failed_state_save(
                &extension,
                state_lock.as_ref(),
                previous,
                journaled,
                error,
            );
            return encode_error(id, "extension_state_save_failed", error);
        }
        if let Err(error) = self.finish_agent_lifecycle(state_lock.as_ref(), journaled) {
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
        let pinned_defaults = extensions
            .iter()
            .filter(|extension| {
                matches!(
                    extension.name.as_str(),
                    "Collections"
                        | "Spaces"
                        | "Delegation"
                        | "Agent Status"
                        | "Pi"
                        | "Claude Code"
                        | "Codex"
                )
            })
            .collect::<Vec<_>>();
        assert!(pinned_defaults.iter().all(|extension| {
            extension.source_commit.is_some()
                && extension.manifest_path.is_some()
                && !extension.artifact_verified
        }));
        assert!(pinned_defaults
            .iter()
            .filter(|extension| extension.package_kind == ExtensionPackageKind::Bundled)
            .all(|extension| extension.install_supported));
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
    fn disabling_agent_status_does_not_disable_agent_control_apis() {
        let mut app = test_app();
        let disabled = app.handle_api_request(Request {
            id: "disable".into(),
            method: Method::ExtensionEnable(ExtensionSetEnabledParams {
                repository: "ttenneb/vqro-extension-agent-status".into(),
                enabled: false,
            }),
        });
        let _ = result(&disabled);
        assert!(app
            .host_document_services
            .generation("vqro.agent-status")
            .is_none());

        let response: serde_json::Value = serde_json::from_str(&app.handle_api_request(Request {
            id: "focus".into(),
            method: Method::AgentFocus(crate::api::schema::AgentTarget {
                target: "missing".into(),
            }),
        }))
        .unwrap();
        assert_ne!(response["error"]["code"], "extension_disabled");
    }

    #[test]
    fn pinned_updates_require_approval_and_deferred_staging() {
        let mut app = test_app();
        let repository = "ttenneb/vqro-extension-pi";
        let unapproved: serde_json::Value =
            serde_json::from_str(&app.handle_api_request(Request {
                id: "update".into(),
                method: Method::ExtensionUpdate(ExtensionUpdateParams {
                    repository: repository.into(),
                    version: "9.0.0".into(),
                    approved: false,
                }),
            }))
            .unwrap();
        assert_eq!(unapproved["error"]["code"], "confirmation_required");

        let approved: serde_json::Value = serde_json::from_str(&app.handle_api_request(Request {
            id: "update".into(),
            method: Method::ExtensionUpdate(ExtensionUpdateParams {
                repository: repository.into(),
                version: "9.0.0".into(),
                approved: true,
            }),
        }))
        .unwrap();
        assert_eq!(
            approved["error"]["code"],
            "extension_artifact_staging_required"
        );
    }

    #[test]
    fn externally_reordered_state_rebuilds_runtime_arbitration() {
        let mut app = test_app();
        let before = app
            .host_document_services
            .generation("vqro.collections")
            .unwrap();
        let mut external = app.marketplace_state.clone();
        external.load_order.swap(0, 1);

        assert!(app.adopt_external_marketplace_state(external));

        let after = app
            .host_document_services
            .generation("vqro.collections")
            .unwrap();
        assert_ne!(before, after);
    }

    #[test]
    fn native_bundled_install_does_not_stage_source_attestations() {
        let app = test_app();
        let native = Request {
            id: "install-native".into(),
            method: Method::ExtensionInstall(ExtensionConfirmedParams {
                repository: "ttenneb/vqro-extension-collections".into(),
                confirmed: true,
            }),
        };
        let agent = Request {
            id: "install-agent".into(),
            method: Method::ExtensionInstall(ExtensionConfirmedParams {
                repository: "ttenneb/vqro-extension-pi".into(),
                confirmed: true,
            }),
        };

        assert!(!app.extension_artifact_request_requires_staging(&native));
        assert!(app.extension_artifact_request_requires_staging(&agent));
    }

    #[test]
    fn data_deletion_clears_authoritative_extension_namespace() {
        let mut app = test_app();
        app.extension_state
            .transact(&crate::extension_state::ExtensionStateTransactionV1 {
                contract: crate::extension_state::EXTENSION_STATE_CONTRACT_V1.into(),
                namespace: "vqro.collections".into(),
                expected_store_generation: 1,
                expected_revision: 0,
                idempotency_key: "seed-delete-test".into(),
                operations: vec![crate::extension_state::ExtensionStateOperationV1 {
                    key: "retained".into(),
                    value: Some(serde_json::json!(true)),
                }],
            })
            .unwrap();
        let _ = result(&app.handle_api_request(Request {
            id: "remove".into(),
            method: Method::ExtensionRemove(ExtensionConfirmedParams {
                repository: "ttenneb/vqro-extension-collections".into(),
                confirmed: true,
            }),
        }));
        let _ = result(&app.handle_api_request(Request {
            id: "delete".into(),
            method: Method::ExtensionDataDelete(ExtensionConfirmedParams {
                repository: "ttenneb/vqro-extension-collections".into(),
                confirmed: true,
            }),
        }));
        assert!(app
            .extension_state
            .snapshot("vqro.collections")
            .unwrap()
            .values
            .is_empty());
    }

    #[test]
    fn default_disable_and_remove_never_mutate_legacy_herdr_adapter_files() {
        let _env = crate::integration::integration_env_lock();
        let root = std::env::temp_dir().join(format!(
            "vqro-marketplace-preserve-herdr-{}",
            std::process::id()
        ));
        let extensions = root.join("extensions");
        std::fs::create_dir_all(&extensions).unwrap();
        let legacy = extensions.join("herdr-agent-state.ts");
        std::fs::write(&legacy, "legacy-herdr-adapter").unwrap();
        let previous = std::env::var_os("PI_CODING_AGENT_DIR");
        unsafe { std::env::set_var("PI_CODING_AGENT_DIR", &root) };

        let mut app = test_app();
        assert!(!extensions.join("vqro-agent-state.ts").exists());
        for method in [
            Method::ExtensionEnable(ExtensionSetEnabledParams {
                repository: "ttenneb/vqro-extension-pi".into(),
                enabled: false,
            }),
            Method::ExtensionRemove(ExtensionConfirmedParams {
                repository: "ttenneb/vqro-extension-pi".into(),
                confirmed: true,
            }),
        ] {
            let _ = result(&app.handle_api_request(Request {
                id: "preserve".into(),
                method,
            }));
            assert_eq!(
                std::fs::read_to_string(&legacy).unwrap(),
                "legacy-herdr-adapter"
            );
        }

        match previous {
            Some(value) => unsafe { std::env::set_var("PI_CODING_AGENT_DIR", value) },
            None => unsafe { std::env::remove_var("PI_CODING_AGENT_DIR") },
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn startup_lifecycle_recovery_uses_only_journaled_artifact_ownership() {
        let _config_env = crate::config::test_config_env_lock().lock().unwrap();
        let _integration_env = crate::integration::integration_env_lock();
        let root = std::env::temp_dir().join(format!(
            "vqro-marketplace-startup-recovery-{}",
            std::process::id()
        ));
        let config_home = root.join("config");
        let pi_root = root.join("pi");
        let extensions_dir = pi_root.join("extensions");
        std::fs::create_dir_all(&extensions_dir).unwrap();
        let adapter = extensions_dir.join("vqro-extension-agent-state.ts");
        std::fs::write(&adapter, "interrupted-new-adapter").unwrap();
        let previous_xdg = std::env::var_os("XDG_CONFIG_HOME");
        let previous_pi = std::env::var_os("PI_CODING_AGENT_DIR");
        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", &config_home);
            std::env::set_var("PI_CODING_AGENT_DIR", &pi_root);
        }

        let index = crate::extensions::CuratedIndex::bundled().unwrap();
        let state_path = crate::extensions::state_storage_path().unwrap();
        let repository = "ttenneb/vqro-extension-pi";
        let mut persisted = crate::extensions::MarketplaceStateFile::defaults(&index);
        persisted
            .entries
            .get_mut(repository)
            .unwrap()
            .active_artifact = Some(crate::extensions::VerifiedExtensionArtifact {
            extension_id: "vqro.pi".into(),
            version: "2.0.0".into(),
            commit: "a".repeat(40),
            source_sha256: "b".repeat(64),
            manifest_sha256: "c".repeat(64),
            manifest_path: "vqro-extension.toml".into(),
            path: root.join("new-artifact").display().to_string(),
        });
        crate::extensions::save_state_to_path(&state_path, &persisted).unwrap();
        crate::extensions::begin_extension_lifecycle(
            &state_path,
            &crate::extensions::PendingExtensionLifecycle {
                repository: repository.into(),
                target: crate::api::schema::IntegrationTarget::Pi,
                installed: true,
                enabled: true,
                active_artifact: None,
                rollback_artifact: None,
                active_version: Some("1.0.0".into()),
                load_order: persisted.load_order.clone(),
            },
        )
        .unwrap();

        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let app = App::new(
            &crate::config::Config::default(),
            crate::app::AppPolicy {
                restore_session: false,
                persist_session: false,
                persist_plugin_registry: true,
                background_updates: false,
            },
            None,
            api_rx,
            crate::api::EventHub::default(),
        );

        let recovered = &app.marketplace_state.entries[repository];
        assert!(recovered.installed);
        assert!(recovered.enabled);
        assert!(recovered.active_artifact.is_none());
        assert!(!adapter.exists());
        assert!(crate::extensions::pending_extension_lifecycle(&state_path)
            .unwrap()
            .is_none());

        match previous_xdg {
            Some(value) => unsafe { std::env::set_var("XDG_CONFIG_HOME", value) },
            None => unsafe { std::env::remove_var("XDG_CONFIG_HOME") },
        }
        match previous_pi {
            Some(value) => unsafe { std::env::set_var("PI_CODING_AGENT_DIR", value) },
            None => unsafe { std::env::remove_var("PI_CODING_AGENT_DIR") },
        }
        let _ = std::fs::remove_dir_all(root);
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

        assert_eq!(response["error"]["code"], "feature_disabled");
        assert!(!app.marketplace_state.entries[repository].installed);
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
