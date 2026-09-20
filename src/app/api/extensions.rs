use std::sync::Arc;

use crate::api::schema::{
    ExtensionConfirmedParams, ExtensionInfo, ExtensionListParams, ExtensionPackageKind,
    ExtensionPackageMutationParams, ExtensionReorderParams, ExtensionRepositoryOverrideInfo,
    ExtensionRepositoryOverrideParams, ExtensionSetEnabledParams, ExtensionUpdateParams,
    InstalledExtensionPackageInfo, IntegrationState, ResponseResult,
};
use crate::app::App;
use crate::extensions::{CuratedExtension, ExtensionPackage};

use super::responses::{encode_error, encode_success};

fn encode_service_fabric_error(id: String, code: &str, message: impl Into<String>) -> String {
    encode_error(
        id,
        crate::service_fabric::stable_r1_error_code(code),
        message,
    )
}

fn contract_inspections(
    package: &crate::extension_package::ValidatedPackageV1,
) -> (
    Vec<crate::api::schema::ExtensionContractInspection>,
    Vec<crate::api::schema::ExtensionContractInspection>,
) {
    let describe =
        |service_id: &str, endpoint_id: &str, descriptor_sha256: &str, required: Option<bool>| {
            package
                .contract_descriptors
                .get(descriptor_sha256)
                .map(
                    |descriptor| crate::api::schema::ExtensionContractInspection {
                        service_id: service_id.into(),
                        endpoint_id: endpoint_id.into(),
                        contract_id: descriptor.contract_id.clone(),
                        contract_version: descriptor.contract_version.clone(),
                        descriptor_sha256: descriptor_sha256.into(),
                        required,
                        methods: descriptor
                            .methods
                            .iter()
                            .map(
                                |method| crate::api::schema::ExtensionContractMethodInspection {
                                    id: method.id.clone(),
                                    request_information: method.request_information.clone(),
                                    response_information: method.response_information.clone(),
                                    max_request_bytes: method.max_request_bytes,
                                    max_response_bytes: method.max_response_bytes,
                                },
                            )
                            .collect(),
                    },
                )
        };
    let provided = package
        .manifest
        .services
        .iter()
        .flat_map(|service| {
            service.exports.iter().filter_map(|export| {
                describe(&service.id, &export.id, &export.descriptor_sha256, None)
            })
        })
        .collect();
    let required = package
        .manifest
        .services
        .iter()
        .flat_map(|service| {
            service.imports.iter().filter_map(|import| {
                describe(
                    &service.id,
                    &import.id,
                    &import.descriptor_sha256,
                    Some(import.required),
                )
            })
        })
        .collect();
    (provided, required)
}

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
            // Bundled catalog entries are host providers or managed agent assets,
            // not downloaded package-v1 executable services.
            requested_capabilities: Vec::new(),
            provided_contracts: Vec::new(),
            required_contracts: Vec::new(),
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
            repository_override_supported: false,
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

    fn recover_extension_data_deletion_while_gated(
        &mut self,
        path: &std::path::Path,
        marketplace_state: &mut crate::extensions::MarketplaceStateFile,
    ) -> Result<bool, String> {
        let Some(namespace) =
            crate::extensions::apply_extension_data_deletion(path, marketplace_state)?
        else {
            return Ok(false);
        };
        let mut candidate = self.extension_state.clone();
        let commit = candidate
            .clear_namespace(&namespace, format!("marketplace-delete-{namespace}"))
            .map_err(|error| format!("clear authoritative extension state: {error:?}"))?;
        self.commit_extension_state_candidate_while_gated(candidate, commit.as_ref())
            .map_err(|error| format!("persist authoritative extension state deletion: {error}"))?;
        crate::extensions::finish_extension_data_deletion(path)?;
        Ok(true)
    }

    fn recover_extension_data_deletion(
        &mut self,
        path: &std::path::Path,
        marketplace_state: &mut crate::extensions::MarketplaceStateFile,
    ) -> Result<bool, String> {
        self.begin_extension_state_mutation()
            .map_err(|error| error.to_string())?;
        let result = self.recover_extension_data_deletion_while_gated(path, marketplace_state);
        self.finish_extension_state_mutation();
        result
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
            self.recover_package_activation(&mut marketplace_state)?;
            self.marketplace_state = marketplace_state;
            self.apply_bundled_extension_state();
            Ok::<(), String>(())
        })();
        if let Err(error) = result {
            tracing::warn!(%error, "failed to recover extension state during startup");
        }
    }

    fn recover_package_activation(
        &self,
        marketplace_state: &mut crate::extensions::MarketplaceStateFile,
    ) -> Result<bool, String> {
        use crate::extension_package::ActivationRecoveryV1;

        let journal_path = crate::extensions::package_activation_journal_path()?;
        let Some(journal) = crate::extension_package::read_activation_journal(&journal_path)?
        else {
            return Ok(false);
        };
        let active = marketplace_state
            .packages
            .packages
            .get(&journal.package_id)
            .and_then(|record| record.active.as_ref());
        match journal.recovery(active) {
            ActivationRecoveryV1::KeepCommittedCandidate
            | ActivationRecoveryV1::RollBackCandidate => {}
            ActivationRecoveryV1::CommitCandidate => {
                let record = marketplace_state
                    .packages
                    .packages
                    .get(&journal.package_id)
                    .ok_or_else(|| {
                        "package activation journal references a missing package".to_string()
                    })?;
                let prepared = marketplace_state.packages.prepare_activation(
                    &journal.package_id,
                    marketplace_state.packages.revision,
                    record.revision,
                    journal.operation_id.clone(),
                    journal.candidate.clone(),
                    journal.candidate_source.clone(),
                    &self.extension_index.package_v1_trust(),
                )?;
                if prepared.expected_registry_revision != journal.expected_registry_revision
                    || prepared.expected_package_revision != journal.expected_package_revision
                {
                    return Err("package activation recovery revision mismatch".into());
                }
                marketplace_state.packages.commit_activation(&journal)?;
                crate::extensions::save_state_to_path(
                    &crate::extensions::state_storage_path()?,
                    marketplace_state,
                )?;
            }
            ActivationRecoveryV1::Conflict => {
                return Err(
                    "package activation journal conflicts with durable registry state".into(),
                )
            }
        }
        crate::extension_package::remove_activation_journal(&journal_path)?;
        Ok(true)
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
        self.reconcile_service_binding_revocations();
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
        if let Err(error) = self.recover_package_activation(&mut loaded) {
            tracing::warn!(%error, "failed to recover package activation during refresh");
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
        self.activate_persisted_extension_packages();
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
        if matches!(
            request.method,
            crate::api::schema::Method::ExtensionPackageInspect(_)
                | crate::api::schema::Method::ExtensionPackageInstall(_)
                | crate::api::schema::Method::ExtensionPackageUpdate(_)
                | crate::api::schema::Method::ExtensionServiceBindingInvoke(_)
        ) {
            return true;
        }
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
        &mut self,
        request: crate::api::schema::Request,
        respond_to: std::sync::mpsc::Sender<String>,
    ) {
        if matches!(
            request.method,
            crate::api::schema::Method::ExtensionServiceBindingInvoke(_)
        ) {
            self.start_deferred_extension_service_binding_invoke(request, respond_to);
            return;
        }
        if matches!(
            request.method,
            crate::api::schema::Method::ExtensionPackageInspect(_)
                | crate::api::schema::Method::ExtensionPackageInstall(_)
                | crate::api::schema::Method::ExtensionPackageUpdate(_)
        ) {
            self.start_deferred_extension_git_package_request(request, respond_to);
            return;
        }
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

    fn start_deferred_extension_service_binding_invoke(
        &mut self,
        request: crate::api::schema::Request,
        respond_to: std::sync::mpsc::Sender<String>,
    ) {
        let id = request.id;
        let encode_error: fn(String, &str, &str) -> String =
            |id, code, message| encode_service_fabric_error(id, code, message);
        let crate::api::schema::Method::ExtensionServiceBindingInvoke(params) = request.method
        else {
            let _ = respond_to.send(encode_error(
                id,
                "service_binding_unavailable",
                "request is not a service binding invocation",
            ));
            return;
        };
        if self.pending_service_fabric_invocations.len()
            >= crate::service_fabric::MAX_PENDING_SERVICE_FABRIC_INVOCATIONS
        {
            let _ = respond_to.send(encode_error(
                id,
                "service_call_busy",
                "too many service fabric invocations are pending",
            ));
            return;
        }
        let Some(binding) = self
            .marketplace_state
            .service_bindings
            .bindings
            .get(&params.binding_id)
            .cloned()
        else {
            let _ = respond_to.send(encode_error(
                id,
                "service_binding_unavailable",
                "service binding is not installed",
            ));
            return;
        };
        if binding.revision != params.expected_binding_revision {
            let _ = respond_to.send(encode_error(
                id,
                "stale_service_call_lease",
                "service binding revision changed",
            ));
            return;
        }
        if binding.state == crate::service_fabric::ServiceBindingStateV1::Revoked {
            let _ = respond_to.send(encode_error(
                id,
                "service_binding_revoked",
                "service binding is revoked",
            ));
            return;
        }
        if let Err(error) = self.required_service_imports_ready(
            &binding.consent.consumer.package_id,
            &binding.consent.consumer.service_id,
        ) {
            let _ = respond_to.send(encode_error(id, "service_binding_unavailable", &error));
            return;
        }
        let authorization = match self.service_read_authorization(&params.binding_id) {
            Ok(authorization) => authorization,
            Err(error) => {
                let _ = respond_to.send(encode_error(id, "service_binding_unavailable", &error));
                return;
            }
        };
        let attempt_id = match crate::service_fabric::next_service_fabric_outer_attempt_id() {
            Ok(attempt_id) => attempt_id,
            Err(error) => {
                let _ = respond_to.send(encode_error(id, "service_provider_failed", &error));
                return;
            }
        };
        let canceled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let deadline = std::time::Instant::now()
            .checked_add(crate::service_fabric::SERVICE_FABRIC_INVOCATION_TIMEOUT)
            .expect("bounded service fabric deadline");
        let consumer_package_id = binding.consent.consumer.package_id.clone();
        let consumer_service_id = binding.consent.consumer.service_id.clone();
        let consumer_artifact_sha256 = binding.consent.consumer.package_sha256.clone();
        let Some(slot) = self.extension_package_services.get(&consumer_package_id) else {
            let _ = respond_to.send(encode_error(
                id,
                "service_binding_unavailable",
                "service consumer is unavailable",
            ));
            return;
        };
        let consumer_artifact_generation = slot.artifact_generation();
        let Some(consumer_runtime_generation) = slot.runtime_generation(&consumer_service_id)
        else {
            let _ = respond_to.send(encode_error(
                id,
                "service_binding_unavailable",
                "service consumer is unavailable",
            ));
            return;
        };
        let provider_package_id = binding.consent.provider.package_id.clone();
        let provider_service_id = binding.consent.provider.service_id.clone();
        let provider_artifact_sha256 = binding.consent.provider.package_sha256.clone();
        let Some(provider_slot) = self.extension_package_services.get(&provider_package_id) else {
            let _ = respond_to.send(encode_error(
                id,
                "service_binding_unavailable",
                "service provider is unavailable",
            ));
            return;
        };
        let provider_artifact_generation = provider_slot.artifact_generation();
        let Some(provider_runtime_generation) =
            provider_slot.runtime_generation(&provider_service_id)
        else {
            let _ = respond_to.send(encode_error(
                id,
                "service_binding_unavailable",
                "service provider is unavailable",
            ));
            return;
        };
        if let Err(error) = slot.invoke_service_read(
            &consumer_service_id,
            attempt_id,
            params.consumer_method,
            params.params,
            authorization,
            deadline,
            Arc::clone(&canceled),
        ) {
            let _ = respond_to.send(encode_error(id, "service_call_busy", &error));
            return;
        }
        self.pending_service_fabric_invocations.insert(
            attempt_id,
            crate::service_fabric::PendingServiceFabricInvocation {
                request_id: id,
                binding_id: binding.binding_id,
                binding_revision: binding.revision,
                consumer_package_id,
                consumer_service_id,
                consumer_artifact_sha256,
                consumer_artifact_generation,
                consumer_runtime_generation,
                provider_package_id,
                provider_service_id,
                provider_artifact_sha256,
                provider_artifact_generation,
                provider_runtime_generation,
                canceled,
                respond_to,
            },
        );
        let event_tx = self.event_tx.clone();
        std::thread::spawn(move || {
            std::thread::sleep(crate::service_fabric::SERVICE_FABRIC_INVOCATION_TIMEOUT);
            let _ =
                event_tx.blocking_send(crate::events::AppEvent::ExtensionServiceFabricTimeout {
                    attempt_id,
                });
        });
    }

    fn start_deferred_extension_git_package_request(
        &self,
        request: crate::api::schema::Request,
        respond_to: std::sync::mpsc::Sender<String>,
    ) {
        use crate::extension_git_package::GitPackageAction;

        let id = request.id;
        let (source, action) = match request.method {
            crate::api::schema::Method::ExtensionPackageInspect(params) => {
                (params.source, GitPackageAction::Inspect)
            }
            crate::api::schema::Method::ExtensionPackageInstall(params) if params.approved => (
                params.source,
                GitPackageAction::Install {
                    expected_package_sha256: params.expected_package_sha256,
                    expected_manifest_sha256: params.expected_manifest_sha256,
                },
            ),
            crate::api::schema::Method::ExtensionPackageUpdate(params) if params.approved => (
                params.source,
                GitPackageAction::Update {
                    expected_package_sha256: params.expected_package_sha256,
                    expected_manifest_sha256: params.expected_manifest_sha256,
                },
            ),
            crate::api::schema::Method::ExtensionPackageInstall(_)
            | crate::api::schema::Method::ExtensionPackageUpdate(_) => {
                let _ = respond_to.send(encode_error(
                    id,
                    "confirmation_required",
                    "package installation or update requires explicit approval",
                ));
                return;
            }
            _ => {
                let _ = respond_to.send(encode_error(
                    id,
                    "extension_package_invalid",
                    "request is not a Git package operation",
                ));
                return;
            }
        };
        let event_tx = self.event_tx.clone();
        std::thread::spawn(move || {
            let inspection = crate::extension_git_package::inspect(
                &source.url,
                &source.commit,
                Some(&source.package_path),
            );
            let _ = event_tx.blocking_send(crate::events::AppEvent::ExtensionGitPackageFinished(
                Box::new(crate::extension_git_package::GitPackageWorkResult {
                    id,
                    action,
                    inspection,
                    respond_to,
                }),
            ));
        });
    }

    pub(crate) fn handle_extension_git_package_finished(
        &mut self,
        result: crate::extension_git_package::GitPackageWorkResult,
    ) {
        let respond_to = result.respond_to.clone();
        let response = self.finish_extension_git_package(result);
        let _ = respond_to.send(response);
    }

    fn finish_extension_git_package(
        &mut self,
        result: crate::extension_git_package::GitPackageWorkResult,
    ) -> String {
        use crate::extension_git_package::GitPackageAction;
        use crate::extension_package::{
            PackageArtifactPointerV1, PackageLifecycleRecordV1, PackageLifecycleStatusV1,
            PackageProvenanceV1,
        };

        let inspection = match result.inspection {
            Ok(inspection) => inspection,
            Err(error) => {
                return encode_error(result.id, "extension_package_inspect_failed", error)
            }
        };
        let manifest = &inspection.package.manifest;
        let (provided_contracts, required_contracts) = contract_inspections(&inspection.package);
        let api_inspection = crate::api::schema::ExtensionPackageInspection {
            source: crate::api::schema::ExtensionPackageSource {
                url: inspection.source_url.clone(),
                commit: inspection.commit.clone(),
                package_path: inspection.package_path.clone(),
            },
            package_id: manifest.id.clone(),
            version: manifest.version.clone(),
            package_sha256: inspection.package.package_sha256.clone(),
            manifest_sha256: inspection.package.manifest_sha256.clone(),
            namespaces: manifest.namespaces.clone(),
            services: manifest
                .services
                .iter()
                .map(|service| service.id.clone())
                .collect(),
            requested_capabilities: inspection.capabilities.clone(),
            provided_contracts,
            required_contracts,
        };
        let (expected_package, expected_manifest, updating) = match result.action {
            GitPackageAction::Inspect => {
                return encode_success(
                    result.id,
                    ResponseResult::ExtensionPackageInspected {
                        inspection: api_inspection,
                    },
                );
            }
            GitPackageAction::Install {
                expected_package_sha256,
                expected_manifest_sha256,
            } => (expected_package_sha256, expected_manifest_sha256, false),
            GitPackageAction::Update {
                expected_package_sha256,
                expected_manifest_sha256,
            } => (expected_package_sha256, expected_manifest_sha256, true),
        };
        if expected_package != inspection.package.package_sha256
            || expected_manifest != inspection.package.manifest_sha256
        {
            return encode_error(
                result.id,
                "extension_package_approval_mismatch",
                "approved package digests do not match the pinned Git artifact",
            );
        }
        let _lock = match self.lock_extension_state_for_mutation() {
            Ok(lock) => lock,
            Err(error) => return encode_error(result.id, "extension_state_busy", error),
        };
        let previous = self.marketplace_state.clone();
        let provenance = PackageProvenanceV1::Git {
            url: inspection.source_url,
            source_commit: inspection.commit,
            package_path: inspection.package_path,
            package_id: manifest.id.clone(),
            version: manifest.version.clone(),
            package_sha256: inspection.package.package_sha256.clone(),
            manifest_sha256: inspection.package.manifest_sha256.clone(),
        };
        let store = match crate::extensions::package_storage_path() {
            Ok(store) => store,
            Err(error) => return encode_error(result.id, "extension_package_store_failed", error),
        };
        let incoming = match crate::platform::SecurePackageStaging::create(&store.join(".incoming"))
        {
            Ok(incoming) => incoming,
            Err(error) => {
                return encode_error(
                    result.id,
                    "extension_package_store_failed",
                    error.to_string(),
                )
            }
        };
        if let Err(error) = incoming.write_file(
            std::path::Path::new("package.vqrox"),
            &inspection.package_bytes,
            0o600,
        ) {
            return encode_error(
                result.id,
                "extension_package_store_failed",
                error.to_string(),
            );
        }
        let occupied = self.marketplace_state.packages.namespace_owners();
        let installed = match crate::extension_package::install_local_file(
            &incoming.path().join("package.vqrox"),
            &store,
            Some(&inspection.package.package_sha256),
            provenance.clone(),
            &occupied,
            None,
        ) {
            Ok(installed) => installed,
            Err(error) => {
                return encode_error(result.id, "extension_package_install_failed", error)
            }
        };
        let generation = match self
            .marketplace_state
            .packages
            .packages
            .get(&manifest.id)
            .and_then(|record| record.active.as_ref())
        {
            Some(active) => match active.runtime_generation.checked_add(1) {
                Some(generation) => generation,
                None => {
                    return encode_error(
                        result.id,
                        "extension_revision_exhausted",
                        "runtime generation exhausted",
                    )
                }
            },
            None => 1,
        };
        let candidate_services = match self.start_installed_package_services(&installed, generation)
        {
            Ok(services) => services,
            Err(error) => {
                return encode_error(result.id, "extension_package_activation_failed", error)
            }
        };
        let pointer = PackageArtifactPointerV1 {
            package_sha256: inspection.package.package_sha256,
            version: manifest.version.clone(),
            runtime_generation: generation,
        };
        let mut activation_journal = None;
        let mutation = if updating {
            let Some(record) = self.marketplace_state.packages.packages.get(&manifest.id) else {
                return encode_error(
                    result.id,
                    "extension_package_not_found",
                    "install the package before updating it",
                );
            };
            let mut journal = match self.marketplace_state.packages.prepare_activation(
                &manifest.id,
                self.marketplace_state.packages.revision,
                record.revision,
                format!("git-update-{generation}"),
                pointer,
                provenance,
                &self.extension_index.package_v1_trust(),
            ) {
                Ok(journal) => journal,
                Err(error) => {
                    return encode_error(result.id, "extension_package_update_failed", error)
                }
            };
            let journal_path = match crate::extensions::package_activation_journal_path() {
                Ok(path) => path,
                Err(error) => {
                    return encode_error(result.id, "extension_package_update_failed", error)
                }
            };
            if let Err(error) =
                crate::extension_package::write_activation_journal(&journal_path, &journal)
            {
                Err(error)
            } else if let Err(error) = journal.advance(
                crate::extension_package::ActivationPhaseV1::Prepared,
                crate::extension_package::ActivationPhaseV1::ArtifactCommitted,
            ) {
                Err(error)
            } else if let Err(error) =
                crate::extension_package::write_activation_journal(&journal_path, &journal)
            {
                Err(error)
            } else {
                let committed = self.marketplace_state.packages.commit_activation(&journal);
                if committed.is_ok() {
                    activation_journal = Some((journal_path, journal));
                }
                committed
            }
        } else {
            let record = PackageLifecycleRecordV1 {
                package_id: manifest.id.clone(),
                source: provenance,
                previous_source: None,
                namespaces: manifest.namespaces.clone(),
                revision: 1,
                status: PackageLifecycleStatusV1::Enabled,
                active: Some(pointer),
                previous: None,
                activation_error: None,
                pending_activation: None,
                retained_data: true,
            };
            self.marketplace_state
                .register_package_v1(
                    &self.extension_index,
                    record,
                    self.marketplace_state.packages.revision,
                )
                .map(|_| ())
        };
        if let Err(error) = mutation {
            self.marketplace_state = previous;
            if let Ok(path) = crate::extensions::package_activation_journal_path() {
                let _ = crate::extension_package::remove_activation_journal(&path);
            }
            return encode_error(result.id, "extension_package_activation_failed", error);
        }
        if let Err(error) = self.save_extension_state(_lock.as_ref()) {
            self.marketplace_state = previous;
            if let Some((journal_path, _)) = activation_journal.as_ref() {
                let _ = crate::extension_package::remove_activation_journal(journal_path);
            }
            return encode_error(result.id, "extension_state_save_failed", error);
        }
        if let Some((journal_path, mut journal)) = activation_journal {
            if let Err(error) = journal
                .advance(
                    crate::extension_package::ActivationPhaseV1::ArtifactCommitted,
                    crate::extension_package::ActivationPhaseV1::RegistryCommitted,
                )
                .and_then(|()| {
                    crate::extension_package::write_activation_journal(&journal_path, &journal)
                })
                .and_then(|()| crate::extension_package::remove_activation_journal(&journal_path))
            {
                tracing::warn!(%error, "package update committed but journal cleanup remains for startup recovery");
            }
        }
        // Publish the already-running candidate only after its registry pointer is durable.
        // Dropping the replaced supervisors retires the previous component runtime.
        self.extension_package_services
            .insert(manifest.id.clone(), candidate_services);
        self.revoke_host_invocation_authority_for_package(&manifest.id);
        match self.installed_git_package_info(&manifest.id) {
            Ok(package) => encode_success(
                result.id,
                ResponseResult::ExtensionPackageChanged { package },
            ),
            Err(error) => encode_error(result.id, "extension_package_invalid", error),
        }
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

    fn start_installed_package_services(
        &self,
        installed: &crate::extension_package::InstalledPackageV1,
        artifact_generation: u64,
    ) -> Result<crate::extension_candidate::PackageCandidateSlot, String> {
        crate::extension_candidate::PackageCandidateSlot::spawn(
            installed.clone(),
            artifact_generation,
            self.event_tx.clone(),
        )
    }

    fn installed_package_from_record(
        &self,
        package_id: &str,
        pointer: &crate::extension_package::PackageArtifactPointerV1,
        source: &crate::extension_package::PackageProvenanceV1,
    ) -> Result<crate::extension_package::InstalledPackageV1, String> {
        let manifest_sha256 = match source {
            crate::extension_package::PackageProvenanceV1::Git {
                manifest_sha256, ..
            }
            | crate::extension_package::PackageProvenanceV1::Curated {
                manifest_sha256, ..
            }
            | crate::extension_package::PackageProvenanceV1::Link {
                manifest_sha256, ..
            } => manifest_sha256.clone(),
            crate::extension_package::PackageProvenanceV1::File { .. } => {
                let root = crate::extensions::package_storage_path()?
                    .join("sha256")
                    .join(&pointer.package_sha256);
                crate::extension_package::validate_package_file(
                    &root.join("package.vqrox"),
                    Some(&pointer.package_sha256),
                )?
                .descriptor
                .manifest_sha256
            }
        };
        Ok(crate::extension_package::InstalledPackageV1 {
            package_id: package_id.into(),
            version: pointer.version.clone(),
            package_sha256: pointer.package_sha256.clone(),
            manifest_sha256,
            immutable_root: crate::extensions::package_storage_path()?
                .join("sha256")
                .join(&pointer.package_sha256)
                .to_string_lossy()
                .into_owned(),
            provenance: source.clone(),
        })
    }

    pub(crate) fn activate_persisted_extension_packages(&mut self) {
        self.reconcile_service_binding_revocations();
        let package_ids = self
            .marketplace_state
            .packages
            .packages
            .iter()
            .filter(|(_, record)| {
                record.status == crate::extension_package::PackageLifecycleStatusV1::Enabled
                    && matches!(
                        record.source,
                        crate::extension_package::PackageProvenanceV1::Git { .. }
                    )
            })
            .map(|(package_id, _)| package_id.clone())
            .collect::<Vec<_>>();
        let mut active = std::collections::BTreeMap::new();
        for package_id in package_ids {
            let result = (|| {
                let record = &self.marketplace_state.packages.packages[&package_id];
                let pointer = record
                    .active
                    .as_ref()
                    .ok_or_else(|| "enabled package has no active artifact".to_string())?;
                let installed =
                    self.installed_package_from_record(&package_id, pointer, &record.source)?;
                self.start_installed_package_services(&installed, pointer.runtime_generation)
            })();
            match result {
                Ok(supervisors) => {
                    active.insert(package_id, supervisors);
                }
                Err(error) => {
                    tracing::warn!(%package_id, %error, "failed to restore extension package runtime")
                }
            }
        }
        self.extension_package_services = active;
    }

    fn retire_extension_package_runtime(&mut self, package_id: &str) {
        self.extension_package_services.remove(package_id);
        self.revoke_host_invocation_authority_for_package(package_id);
        if package_id == crate::collections_extension_contract::COLLECTIONS_PACKAGE_ID {
            self.disable_collections_package_authority_runtime();
        } else if package_id == crate::spaces_extension_contract::SPACES_PACKAGE_ID {
            self.disable_spaces_package_authority_runtime();
        }
    }

    pub(crate) fn handle_extension_package_candidate_ready(
        &mut self,
        result: crate::extension_candidate::PackageCandidateBootstrapResult,
    ) {
        let current = self
            .marketplace_state
            .packages
            .packages
            .get(&result.package_id)
            .filter(|record| {
                record.status == crate::extension_package::PackageLifecycleStatusV1::Enabled
            })
            .and_then(|record| record.active.as_ref())
            .is_some_and(|active| {
                active.package_sha256 == result.artifact_sha256
                    && active.runtime_generation == result.artifact_generation
            });
        let accepted = current
            && self
                .extension_package_services
                .get(&result.package_id)
                .is_some_and(|slot| slot.accepts(&result));
        if !accepted {
            return;
        }
        let mut became_ready = false;
        match result.result {
            Ok(candidate) => {
                if let Some(slot) = self.extension_package_services.get_mut(&result.package_id) {
                    slot.install_ready(candidate);
                    became_ready = true;
                    tracing::info!(
                        package_id = %result.package_id,
                        artifact_generation = result.artifact_generation,
                        "non-publishing package candidate is ready"
                    );
                }
            }
            Err(error) => {
                self.revoke_host_invocation_authority_for_package(&result.package_id);
                if result.package_id
                    == crate::collections_extension_contract::COLLECTIONS_PACKAGE_ID
                {
                    self.disable_collections_package_authority_runtime();
                } else if result.package_id == crate::spaces_extension_contract::SPACES_PACKAGE_ID {
                    self.disable_spaces_package_authority_runtime();
                }
                tracing::warn!(
                    package_id = %result.package_id,
                    artifact_generation = result.artifact_generation,
                    %error,
                    "package candidate bootstrap failed"
                );
            }
        }
        if became_ready
            && matches!(
                result.package_id.as_str(),
                crate::collections_extension_contract::COLLECTIONS_PACKAGE_ID
                    | crate::spaces_extension_contract::SPACES_PACKAGE_ID
            )
        {
            self.refresh_collection_documents();
        }
    }

    fn finish_service_fabric_invocation(
        &mut self,
        result: &crate::extension_candidate::PackageCandidateInvocationResult,
    ) -> bool {
        let Some(pending) = self
            .pending_service_fabric_invocations
            .remove(&result.attempt_id)
        else {
            return false;
        };
        let binding_current = self
            .marketplace_state
            .service_bindings
            .bindings
            .get(&pending.binding_id)
            .is_some_and(|binding| {
                binding.revision == pending.binding_revision
                    && binding.state == crate::service_fabric::ServiceBindingStateV1::Approved
                    && self.service_binding_info(binding).effective_status
                        == crate::service_fabric::EffectiveServiceBindingStatusV1::Ready
            });
        let exact_consumer = result.package_id == pending.consumer_package_id
            && result.service_id == pending.consumer_service_id
            && result.artifact_sha256 == pending.consumer_artifact_sha256
            && result.artifact_generation == pending.consumer_artifact_generation
            && result.runtime_generation == pending.consumer_runtime_generation;
        let exact_provider = self
            .extension_package_services
            .get(&pending.provider_package_id)
            .is_some_and(|slot| {
                slot.artifact_sha256() == pending.provider_artifact_sha256
                    && slot.artifact_generation() == pending.provider_artifact_generation
                    && slot.runtime_generation(&pending.provider_service_id)
                        == Some(pending.provider_runtime_generation)
            });
        let response = if !binding_current || !exact_consumer || !exact_provider {
            pending
                .canceled
                .store(true, std::sync::atomic::Ordering::Release);
            encode_service_fabric_error(
                pending.request_id,
                "stale_service_call_lease",
                "service binding or consumer generation changed before completion",
            )
        } else {
            match &result.result {
                Ok(value) => encode_success(
                    pending.request_id,
                    ResponseResult::ExtensionServiceBindingInvoked {
                        binding_id: pending.binding_id,
                        result: value.clone(),
                    },
                ),
                Err(_) => encode_service_fabric_error(
                    pending.request_id,
                    "service_provider_failed",
                    "service consumer invocation failed closed",
                ),
            }
        };
        let _ = pending.respond_to.send(response);
        true
    }

    pub(crate) fn handle_extension_service_fabric_timeout(&mut self, attempt_id: u64) {
        let Some(pending) = self.pending_service_fabric_invocations.remove(&attempt_id) else {
            return;
        };
        pending
            .canceled
            .store(true, std::sync::atomic::Ordering::Release);
        let _ = pending.respond_to.send(encode_service_fabric_error(
            pending.request_id,
            "service_call_timeout",
            "service consumer exceeded the host deadline",
        ));
    }

    pub(crate) fn handle_extension_package_candidate_finished(
        &mut self,
        result: crate::extension_candidate::PackageCandidateInvocationResult,
    ) {
        if self.finish_service_fabric_invocation(&result) {
            return;
        }
        let current = self
            .marketplace_state
            .packages
            .packages
            .get(&result.package_id)
            .and_then(|record| record.active.as_ref())
            .is_some_and(|active| {
                active.package_sha256 == result.artifact_sha256
                    && active.runtime_generation == result.artifact_generation
            });
        if !current {
            return;
        }
        if result.package_id == crate::spaces_extension_contract::SPACES_PACKAGE_ID {
            match self.finalize_spaces_candidate(result.attempt_id, result.result) {
                Ok(true) | Ok(false) => {}
                Err(error) => {
                    self.disable_spaces_package_authority_runtime();
                    tracing::warn!(
                        package_id = %result.package_id,
                        service_id = %result.service_id,
                        runtime_generation = result.runtime_generation,
                        %error,
                        "Spaces package candidate result was rejected"
                    );
                }
            }
            return;
        }
        if result.package_id != crate::collections_extension_contract::COLLECTIONS_PACKAGE_ID {
            return;
        }
        if self.finish_collections_create_stage(result.attempt_id, result.result.clone()) {
            return;
        }
        if self.finish_collections_action_plan(result.attempt_id, result.result.clone()) {
            return;
        }
        if let Err(error) = self.finalize_collections_candidate(result.attempt_id, result.result) {
            self.disable_collections_package_authority_runtime();
            tracing::warn!(
                package_id = %result.package_id,
                service_id = %result.service_id,
                runtime_generation = result.runtime_generation,
                %error,
                "package candidate result was rejected"
            );
        }
    }

    fn installed_git_package_info(
        &self,
        package_id: &str,
    ) -> Result<InstalledExtensionPackageInfo, String> {
        use crate::extension_package::{PackageLifecycleStatusV1, PackageProvenanceV1};

        let record = self
            .marketplace_state
            .packages
            .packages
            .get(package_id)
            .ok_or_else(|| "package is not installed".to_string())?;
        let PackageProvenanceV1::Git {
            url,
            source_commit,
            package_path,
            version,
            package_sha256,
            manifest_sha256,
            package_id: _,
        } = &record.source
        else {
            return Err("package is not a Git-installed extension".into());
        };
        let root = crate::extensions::package_storage_path()?
            .join("sha256")
            .join(package_sha256);
        let archive = root.join("package.vqrox");
        let validated =
            crate::extension_package::validate_package_file(&archive, Some(package_sha256))?;
        let mut requested_capabilities = validated
            .descriptor
            .manifest
            .services
            .iter()
            .flat_map(|service| service.capabilities.iter().cloned())
            .collect::<Vec<_>>();
        requested_capabilities.sort();
        requested_capabilities.dedup();
        let (provided_contracts, required_contracts) = contract_inspections(&validated.descriptor);
        Ok(InstalledExtensionPackageInfo {
            package_id: package_id.into(),
            version: version.clone(),
            source: crate::api::schema::ExtensionPackageSource {
                url: url.clone(),
                commit: source_commit.clone(),
                package_path: package_path.clone(),
            },
            package_sha256: package_sha256.clone(),
            manifest_sha256: manifest_sha256.clone(),
            enabled: record.status == PackageLifecycleStatusV1::Enabled,
            removed_retaining_data: record.status == PackageLifecycleStatusV1::RemovedRetainingData,
            load_order: self
                .marketplace_state
                .packages
                .load_order
                .iter()
                .position(|candidate| candidate == package_id)
                .unwrap_or(usize::MAX),
            requested_capabilities,
            provided_contracts,
            required_contracts,
            rollback_version: record
                .previous
                .as_ref()
                .map(|previous| previous.version.clone()),
            activation_error: record.activation_error.clone(),
        })
    }

    pub(super) fn handle_extension_package_list(&self, id: String) -> String {
        let mut packages = self
            .marketplace_state
            .packages
            .packages
            .iter()
            .filter(|(_, record)| {
                matches!(
                    record.source,
                    crate::extension_package::PackageProvenanceV1::Git { .. }
                )
            })
            .map(|(package_id, _)| self.installed_git_package_info(package_id))
            .collect::<Result<Vec<_>, _>>();
        match packages.as_mut() {
            Ok(packages) => {
                packages.sort_by_key(|package| package.load_order);
                encode_success(
                    id,
                    ResponseResult::ExtensionPackageList {
                        packages: std::mem::take(packages),
                    },
                )
            }
            Err(error) => encode_error(id, "extension_package_invalid", error.clone()),
        }
    }

    pub(super) fn handle_extension_package_enable(
        &mut self,
        id: String,
        params: ExtensionPackageMutationParams,
        enabled: bool,
    ) -> String {
        if !params.confirmed {
            return encode_error(
                id,
                "confirmation_required",
                "changing package enablement requires explicit confirmation",
            );
        }
        let lock = match self.lock_extension_state_for_mutation() {
            Ok(lock) => lock,
            Err(error) => return encode_error(id, "extension_state_busy", error),
        };
        let previous = self.marketplace_state.clone();
        let candidate_services = if enabled {
            let Some(record) = self
                .marketplace_state
                .packages
                .packages
                .get(&params.package_id)
            else {
                return encode_error(
                    id,
                    "extension_package_not_found",
                    "package is not installed",
                );
            };
            let Some(pointer) = record.active.as_ref() else {
                return encode_error(
                    id,
                    "extension_package_invalid",
                    "package has no active artifact",
                );
            };
            let installed = match self.installed_package_from_record(
                &params.package_id,
                pointer,
                &record.source,
            ) {
                Ok(installed) => installed,
                Err(error) => return encode_error(id, "extension_package_invalid", error),
            };
            match self.start_installed_package_services(&installed, pointer.runtime_generation) {
                Ok(services) => Some(services),
                Err(error) => {
                    return encode_error(id, "extension_package_activation_failed", error)
                }
            }
        } else {
            None
        };
        let Some(record) = self
            .marketplace_state
            .packages
            .packages
            .get_mut(&params.package_id)
        else {
            return encode_error(
                id,
                "extension_package_not_found",
                "package is not installed",
            );
        };
        record.status = if enabled {
            crate::extension_package::PackageLifecycleStatusV1::Enabled
        } else {
            crate::extension_package::PackageLifecycleStatusV1::Disabled
        };
        let Some(package_revision) = record.revision.checked_add(1) else {
            return encode_error(
                id,
                "extension_revision_exhausted",
                "package revision exhausted",
            );
        };
        record.revision = package_revision;
        let Some(registry_revision) = self.marketplace_state.packages.revision.checked_add(1)
        else {
            self.marketplace_state = previous;
            return encode_error(
                id,
                "extension_revision_exhausted",
                "registry revision exhausted",
            );
        };
        self.marketplace_state.packages.revision = registry_revision;
        if enabled
            && !self
                .marketplace_state
                .packages
                .load_order
                .contains(&params.package_id)
        {
            self.marketplace_state
                .packages
                .load_order
                .push(params.package_id.clone());
        }
        if let Err(error) = self.save_extension_state(lock.as_ref()) {
            self.marketplace_state = previous;
            return encode_error(id, "extension_state_save_failed", error);
        }
        if let Some(services) = candidate_services {
            self.extension_package_services
                .insert(params.package_id.clone(), services);
        } else {
            self.retire_extension_package_runtime(&params.package_id);
        }
        match self.installed_git_package_info(&params.package_id) {
            Ok(package) => encode_success(id, ResponseResult::ExtensionPackageChanged { package }),
            Err(error) => encode_error(id, "extension_package_invalid", error),
        }
    }

    pub(super) fn handle_extension_package_remove(
        &mut self,
        id: String,
        params: ExtensionPackageMutationParams,
    ) -> String {
        if !params.confirmed {
            return encode_error(
                id,
                "confirmation_required",
                "package removal requires explicit confirmation",
            );
        }
        let lock = match self.lock_extension_state_for_mutation() {
            Ok(lock) => lock,
            Err(error) => return encode_error(id, "extension_state_busy", error),
        };
        let previous = self.marketplace_state.clone();
        if let Err(error) = self
            .marketplace_state
            .packages
            .remove_retaining_data(&params.package_id)
        {
            return encode_error(id, "extension_package_remove_failed", error);
        }
        if let Err(error) = self.save_extension_state(lock.as_ref()) {
            self.marketplace_state = previous;
            return encode_error(id, "extension_state_save_failed", error);
        }
        self.retire_extension_package_runtime(&params.package_id);
        match self.installed_git_package_info(&params.package_id) {
            Ok(package) => encode_success(id, ResponseResult::ExtensionPackageChanged { package }),
            Err(error) => encode_error(id, "extension_package_invalid", error),
        }
    }

    pub(super) fn handle_extension_package_rollback(
        &mut self,
        id: String,
        params: ExtensionPackageMutationParams,
    ) -> String {
        if !params.confirmed {
            return encode_error(
                id,
                "confirmation_required",
                "package rollback requires explicit confirmation",
            );
        }
        let lock = match self.lock_extension_state_for_mutation() {
            Ok(lock) => lock,
            Err(error) => return encode_error(id, "extension_state_busy", error),
        };
        let previous = self.marketplace_state.clone();
        let candidate_services = {
            let Some(record) = self
                .marketplace_state
                .packages
                .packages
                .get(&params.package_id)
            else {
                return encode_error(
                    id,
                    "extension_package_not_found",
                    "package is not installed",
                );
            };
            let (Some(pointer), Some(source)) =
                (record.previous.as_ref(), record.previous_source.as_ref())
            else {
                return encode_error(
                    id,
                    "extension_package_rollback_failed",
                    "package has no retained rollback artifact",
                );
            };
            let installed =
                match self.installed_package_from_record(&params.package_id, pointer, source) {
                    Ok(installed) => installed,
                    Err(error) => {
                        return encode_error(id, "extension_package_rollback_failed", error)
                    }
                };
            match self.start_installed_package_services(&installed, pointer.runtime_generation) {
                Ok(services) => services,
                Err(error) => {
                    return encode_error(id, "extension_package_activation_failed", error)
                }
            }
        };
        if let Err(error) = self.marketplace_state.packages.rollback(&params.package_id) {
            return encode_error(id, "extension_package_rollback_failed", error);
        }
        if let Err(error) = self.save_extension_state(lock.as_ref()) {
            self.marketplace_state = previous;
            return encode_error(id, "extension_state_save_failed", error);
        }
        self.extension_package_services
            .insert(params.package_id.clone(), candidate_services);
        self.revoke_host_invocation_authority_for_package(&params.package_id);
        match self.installed_git_package_info(&params.package_id) {
            Ok(package) => encode_success(id, ResponseResult::ExtensionPackageChanged { package }),
            Err(error) => encode_error(id, "extension_package_invalid", error),
        }
    }

    fn reconcile_service_binding_revocations(&mut self) {
        use std::sync::atomic::Ordering;
        let live = self
            .marketplace_state
            .service_bindings
            .bindings
            .values()
            .filter(|binding| {
                binding.state == crate::service_fabric::ServiceBindingStateV1::Approved
            })
            .map(|binding| (binding.binding_id.clone(), binding.revision))
            .collect::<std::collections::BTreeSet<_>>();
        self.service_binding_revocations.retain(|key, token| {
            let retain = live.contains(key);
            if !retain {
                token.store(true, Ordering::Release);
            }
            retain
        });
        for key in live {
            self.service_binding_revocations
                .entry(key)
                .or_insert_with(|| Arc::new(std::sync::atomic::AtomicBool::new(false)));
        }
    }

    fn revalidate_binding_declarations(
        &self,
        binding: &crate::service_fabric::ServiceBindingV1,
    ) -> Result<(), String> {
        let consumer = self
            .extension_package_services
            .get(&binding.consent.consumer.package_id)
            .and_then(crate::extension_candidate::PackageCandidateSlot::validated_package)
            .ok_or_else(|| "service consumer declaration is not admitted".to_string())?;
        let provider = self
            .extension_package_services
            .get(&binding.consent.provider.package_id)
            .and_then(crate::extension_candidate::PackageCandidateSlot::validated_package)
            .ok_or_else(|| "service provider declaration is not admitted".to_string())?;
        let expected = crate::service_fabric::consent_for_binding(
            consumer,
            &binding.consent.consumer.service_id,
            &binding.consent.consumer.endpoint_id,
            provider,
            &binding.consent.provider.service_id,
            &binding.consent.provider.endpoint_id,
        )?;
        if expected != binding.consent {
            return Err(
                "persisted service consent does not match exact package declarations".into(),
            );
        }
        Ok(())
    }

    fn required_service_imports_ready(
        &self,
        package_id: &str,
        service_id: &str,
    ) -> Result<(), String> {
        let package = self
            .extension_package_services
            .get(package_id)
            .and_then(crate::extension_candidate::PackageCandidateSlot::validated_package)
            .ok_or_else(|| "service consumer declaration is not admitted".to_string())?;
        let service = package
            .manifest
            .services
            .iter()
            .find(|service| service.id == service_id)
            .ok_or_else(|| "service consumer declaration is unavailable".to_string())?;
        for import in service.imports.iter().filter(|import| import.required) {
            let ready = self
                .marketplace_state
                .service_bindings
                .bindings
                .values()
                .filter(|binding| {
                    binding.consent.consumer.package_id == package_id
                        && binding.consent.consumer.package_sha256 == package.package_sha256
                        && binding.consent.consumer.service_id == service_id
                        && binding.consent.consumer.endpoint_id == import.id
                })
                .any(|binding| {
                    self.service_binding_info(binding).effective_status
                        == crate::service_fabric::EffectiveServiceBindingStatusV1::Ready
                });
            if !ready {
                return Err(format!(
                    "required service import {} is not bound and ready",
                    import.id
                ));
            }
        }
        Ok(())
    }

    pub(crate) fn service_read_authorization(
        &mut self,
        binding_id: &str,
    ) -> Result<crate::service_fabric::ServiceReadInvocationAuthorization, String> {
        self.reconcile_service_binding_revocations();
        let binding = self
            .marketplace_state
            .service_bindings
            .bindings
            .get(binding_id)
            .cloned()
            .ok_or_else(|| "service binding does not exist".to_string())?;
        if binding.state != crate::service_fabric::ServiceBindingStateV1::Approved {
            return Err("service binding is revoked".into());
        }
        if self.service_binding_info(&binding).effective_status
            != crate::service_fabric::EffectiveServiceBindingStatusV1::Ready
        {
            return Err("service binding is suspended".into());
        }
        self.revalidate_binding_declarations(&binding)?;
        let consumer = self
            .extension_package_services
            .get(&binding.consent.consumer.package_id)
            .ok_or_else(|| "consumer package worker is unavailable".to_string())?;
        if consumer.artifact_sha256() != binding.consent.consumer.package_sha256 {
            return Err("consumer package artifact does not match the binding".into());
        }
        let consumer_runtime_generation = consumer
            .runtime_generation(&binding.consent.consumer.service_id)
            .ok_or_else(|| "consumer package service is unavailable".to_string())?;
        let provider = self
            .extension_package_services
            .get(&binding.consent.provider.package_id)
            .and_then(|slot| slot.service_read_endpoint(&binding.consent.provider.service_id))
            .ok_or_else(|| "provider package service is unavailable".to_string())?;
        let revoked = self
            .service_binding_revocations
            .get(&(binding.binding_id.clone(), binding.revision))
            .cloned()
            .ok_or_else(|| "service binding revocation authority is unavailable".to_string())?;
        crate::service_fabric::ServiceReadInvocationAuthorization::new(
            binding,
            self.marketplace_state.service_bindings.revision,
            consumer.artifact_sha256(),
            consumer.artifact_generation(),
            consumer_runtime_generation,
            provider,
            revoked,
        )
    }

    fn validated_active_package(
        &self,
        package_id: &str,
    ) -> Result<crate::extension_package::ValidatedPackageV1, String> {
        let record = self
            .marketplace_state
            .packages
            .packages
            .get(package_id)
            .ok_or_else(|| format!("package {package_id} is not installed"))?;
        if record.status == crate::extension_package::PackageLifecycleStatusV1::RemovedRetainingData
        {
            return Err(format!("package {package_id} is removed"));
        }
        let pointer = record
            .active
            .as_ref()
            .ok_or_else(|| format!("package {package_id} has no active artifact"))?;
        let installed = self.installed_package_from_record(package_id, pointer, &record.source)?;
        let root = std::path::Path::new(&installed.immutable_root);
        let archive = root.join("package.vqrox");
        let package = if archive.is_file() {
            crate::extension_package::validate_package_file(
                &archive,
                Some(&installed.package_sha256),
            )?
            .descriptor
        } else {
            crate::extension_package::validate_directory_package(root)?
        };
        if package.manifest.id != installed.package_id
            || package.manifest.version != installed.version
            || package.manifest_sha256 != installed.manifest_sha256
        {
            return Err(format!(
                "package {package_id} active artifact identity is inconsistent"
            ));
        }
        Ok(package)
    }

    fn validated_binding_package(
        &self,
        package_id: &str,
    ) -> Result<crate::extension_package::ValidatedPackageV1, String> {
        if let Some(package) = self
            .extension_package_services
            .get(package_id)
            .and_then(crate::extension_candidate::PackageCandidateSlot::validated_package)
        {
            let exact_active = self
                .marketplace_state
                .packages
                .packages
                .get(package_id)
                .is_some_and(|record| {
                    record.active.as_ref().is_some_and(|active| {
                        active.package_sha256 == package.package_sha256
                            && active.version == package.manifest.version
                    }) && matches!(
                        &record.source,
                        crate::extension_package::PackageProvenanceV1::Git {
                            version,
                            package_sha256,
                            manifest_sha256,
                            ..
                        } if version == &package.manifest.version
                            && package_sha256 == &package.package_sha256
                            && manifest_sha256 == &package.manifest_sha256
                    )
                });
            if exact_active {
                return Ok(package.clone());
            }
        }
        self.validated_active_package(package_id)
    }

    fn binding_consent(
        &self,
        consumer: &crate::api::schema::ExtensionServiceEndpointParams,
        provider: &crate::api::schema::ExtensionServiceEndpointParams,
    ) -> Result<crate::service_fabric::ServiceBindingConsentV1, String> {
        for package_id in [&consumer.package_id, &provider.package_id] {
            let record = self
                .marketplace_state
                .packages
                .packages
                .get(package_id)
                .ok_or_else(|| format!("package {package_id} is not installed"))?;
            if !matches!(
                record.source,
                crate::extension_package::PackageProvenanceV1::Git { .. }
            ) {
                return Err(
                    "R1 service bindings are available only to exact-Git component packages".into(),
                );
            }
        }
        let consumer_package = self.validated_binding_package(&consumer.package_id)?;
        let provider_package = self.validated_binding_package(&provider.package_id)?;
        crate::service_fabric::consent_for_binding(
            &consumer_package,
            &consumer.service_id,
            &consumer.endpoint_id,
            &provider_package,
            &provider.service_id,
            &provider.endpoint_id,
        )
    }

    fn service_binding_info(
        &self,
        binding: &crate::service_fabric::ServiceBindingV1,
    ) -> crate::api::schema::ExtensionServiceBindingInfo {
        let artifact_active = |endpoint: &crate::service_fabric::ServiceEndpointV1| {
            self.marketplace_state
                .packages
                .packages
                .get(&endpoint.package_id)
                .is_some_and(|record| {
                    record.status == crate::extension_package::PackageLifecycleStatusV1::Enabled
                        && record.active.as_ref().is_some_and(|active| {
                            active.version == endpoint.package_version
                                && active.package_sha256 == endpoint.package_sha256
                        })
                        && matches!(
                            &record.source,
                            crate::extension_package::PackageProvenanceV1::Git {
                                version,
                                package_sha256,
                                manifest_sha256,
                                ..
                            } if version == &endpoint.package_version
                                && package_sha256 == &endpoint.package_sha256
                                && manifest_sha256 == &endpoint.manifest_sha256
                        )
                })
        };
        let declarations_match = self.revalidate_binding_declarations(binding).is_ok();
        let consumer_artifact_active = artifact_active(&binding.consent.consumer);
        let provider_artifact_active =
            declarations_match && artifact_active(&binding.consent.provider);
        let consumer_worker_ready = self
            .extension_package_services
            .get(&binding.consent.consumer.package_id)
            .and_then(|slot| slot.binding(&binding.consent.consumer.service_id))
            .is_some_and(|runtime| {
                runtime.artifact_sha256() == binding.consent.consumer.package_sha256
            });
        let provider_worker_ready = self
            .extension_package_services
            .get(&binding.consent.provider.package_id)
            .and_then(|slot| slot.binding(&binding.consent.provider.service_id))
            .is_some_and(|runtime| {
                runtime.artifact_sha256() == binding.consent.provider.package_sha256
            });
        crate::api::schema::ExtensionServiceBindingInfo {
            effective_status: crate::service_fabric::effective_status(
                binding,
                consumer_artifact_active,
                provider_artifact_active,
                consumer_worker_ready,
                provider_worker_ready,
            ),
            binding: binding.clone(),
        }
    }

    pub(super) fn handle_extension_service_binding_inspect(
        &self,
        id: String,
        params: crate::api::schema::ExtensionServiceBindingInspectParams,
    ) -> String {
        let consent = match self.binding_consent(&params.consumer, &params.provider) {
            Ok(consent) => consent,
            Err(error) => {
                return encode_error(id, "extension_service_binding_invalid", error);
            }
        };
        let consent_sha256 = match crate::service_fabric::consent_sha256(&consent) {
            Ok(digest) => digest,
            Err(error) => return encode_error(id, "extension_service_binding_invalid", error),
        };
        let binding_id = match crate::service_fabric::binding_id(&consent.consumer) {
            Ok(binding_id) => binding_id,
            Err(error) => return encode_error(id, "extension_service_binding_invalid", error),
        };
        encode_success(
            id,
            ResponseResult::ExtensionServiceBindingInspected {
                inspection: crate::api::schema::ExtensionServiceBindingInspection {
                    registry_revision: self.marketplace_state.service_bindings.revision,
                    consent_sha256,
                    consent,
                    existing_binding: self
                        .marketplace_state
                        .service_bindings
                        .bindings
                        .get(&binding_id)
                        .cloned(),
                },
            },
        )
    }

    pub(super) fn handle_extension_service_binding_approve(
        &mut self,
        id: String,
        params: crate::api::schema::ExtensionServiceBindingApproveParams,
    ) -> String {
        let lock = match self.lock_extension_state_for_mutation() {
            Ok(lock) => lock,
            Err(error) => return encode_error(id, "extension_state_busy", error),
        };
        let consent = match self.binding_consent(&params.consumer, &params.provider) {
            Ok(consent) => consent,
            Err(error) => return encode_error(id, "extension_service_binding_invalid", error),
        };
        let previous = self.marketplace_state.clone();
        let binding = match self.marketplace_state.service_bindings.approve(
            consent,
            params.expected_registry_revision,
            params.expected_binding_revision,
            &params.expected_consent_sha256,
            params.confirmed,
        ) {
            Ok(binding) => binding.clone(),
            Err(error) => {
                return encode_error(id, "extension_service_binding_approval_failed", error)
            }
        };
        if let Err(error) = self.save_extension_state(lock.as_ref()) {
            self.marketplace_state = previous;
            return encode_error(id, "extension_state_save_failed", error);
        }
        self.reconcile_service_binding_revocations();
        encode_success(
            id,
            ResponseResult::ExtensionServiceBindingChanged {
                binding: self.service_binding_info(&binding),
            },
        )
    }

    pub(super) fn handle_extension_service_binding_list(&self, id: String) -> String {
        encode_success(
            id,
            ResponseResult::ExtensionServiceBindingList {
                registry_revision: self.marketplace_state.service_bindings.revision,
                bindings: self
                    .marketplace_state
                    .service_bindings
                    .bindings
                    .values()
                    .map(|binding| self.service_binding_info(binding))
                    .collect(),
            },
        )
    }

    pub(super) fn handle_extension_service_binding_revoke(
        &mut self,
        id: String,
        params: crate::api::schema::ExtensionServiceBindingRevokeParams,
    ) -> String {
        let lock = match self.lock_extension_state_for_mutation() {
            Ok(lock) => lock,
            Err(error) => return encode_error(id, "extension_state_busy", error),
        };
        let previous = self.marketplace_state.clone();
        let binding = match self.marketplace_state.service_bindings.revoke(
            &params.binding_id,
            params.expected_registry_revision,
            params.expected_binding_revision,
            params.confirmed,
        ) {
            Ok(binding) => binding.clone(),
            Err(error) => {
                return encode_error(id, "extension_service_binding_revoke_failed", error)
            }
        };
        if let Err(error) = self.save_extension_state(lock.as_ref()) {
            self.marketplace_state = previous;
            return encode_error(id, "extension_state_save_failed", error);
        }
        self.reconcile_service_binding_revocations();
        encode_success(
            id,
            ResponseResult::ExtensionServiceBindingChanged {
                binding: self.service_binding_info(&binding),
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
        if let Err(error) = self.begin_extension_state_mutation() {
            let code = match error {
                super::extension_state::ExtensionStateMutationError::Busy => "extension_state_busy",
                _ => "extension_state_invalid",
            };
            return encode_error(id, code, error.to_string());
        }
        let result = (|| -> Result<(), (&'static str, String)> {
            if !self.policy.persist_plugin_registry {
                crate::extensions::purge_extension_data(&data_id)
                    .map_err(|error| ("extension_data_delete_failed", error))?;
                self.marketplace_state
                    .clear_retained_data(&extension.repository)
                    .map_err(|error| ("extension_state_invalid", error))?;
                let mut candidate = self.extension_state.clone();
                let commit = candidate
                    .clear_namespace(&data_id, format!("marketplace-delete-{data_id}"))
                    .map_err(|error| {
                        (
                            "extension_state_invalid",
                            format!("clear authoritative extension state: {error:?}"),
                        )
                    })?;
                self.commit_extension_state_candidate_while_gated(candidate, commit.as_ref())
                    .map_err(|error| ("extension_state_invalid", error.to_string()))?;
                return Ok(());
            }
            let state_path = crate::extensions::state_storage_path()
                .map_err(|error| ("extension_state_path_failed", error))?;
            crate::extensions::begin_extension_data_deletion(
                &state_path,
                &extension.repository,
                &data_id,
            )
            .map_err(|error| ("extension_data_delete_failed", error))?;
            let mut marketplace_state = self.marketplace_state.clone();
            self.recover_extension_data_deletion_while_gated(&state_path, &mut marketplace_state)
                .map_err(|error| ("extension_data_delete_failed", error))?;
            self.marketplace_state = marketplace_state;
            Ok(())
        })();
        self.finish_extension_state_mutation();
        match result {
            Ok(()) => encode_success(
                id,
                ResponseResult::ExtensionDataDeleted {
                    repository: extension.repository,
                },
            ),
            Err((code, error)) => encode_error(id, code, error),
        }
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
        _params: ExtensionRepositoryOverrideParams,
    ) -> String {
        encode_error(
            id,
            "extension_repository_override_unsupported",
            "component packages do not use legacy repository-scoped overrides",
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
            ["Delegation", "Agent Status", "Pi", "Claude Code", "Codex"]
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
                    "Delegation" | "Agent Status" | "Pi" | "Claude Code" | "Codex"
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
    fn curated_inventory_distinguishes_bundled_and_agent_entries() {
        let mut app = test_app();
        let response = app.handle_api_request(Request {
            id: "extensions".into(),
            method: Method::ExtensionList(ExtensionListParams::default()),
        });
        let ResponseResult::ExtensionList { extensions } = result(&response) else {
            panic!("extension list response");
        };
        assert!(extensions
            .iter()
            .all(|extension| extension.id != "vqro.spaces"));
        assert!(extensions.iter().any(|extension| {
            extension.id == "vqro.delegation"
                && extension.package_kind == ExtensionPackageKind::Bundled
        }));
        assert!(extensions.iter().any(|extension| {
            extension.name == "Pi"
                && extension.package_kind == ExtensionPackageKind::AgentIntegration
        }));
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
                repository: "ttenneb/vqro-extension-delegation".into(),
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
                namespace: "vqro.delegation".into(),
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
                repository: "ttenneb/vqro-extension-delegation".into(),
                confirmed: true,
            }),
        }));
        let _ = result(&app.handle_api_request(Request {
            id: "delete".into(),
            method: Method::ExtensionDataDelete(ExtensionConfirmedParams {
                repository: "ttenneb/vqro-extension-delegation".into(),
                confirmed: true,
            }),
        }));
        assert!(app
            .extension_state
            .snapshot("vqro.delegation")
            .unwrap()
            .values
            .is_empty());
    }

    #[test]
    fn pending_transaction_prevents_all_extension_data_deletion_side_effects() {
        let _env = crate::config::test_config_env_lock().lock().unwrap();
        let root = std::env::temp_dir().join(format!(
            "vqro-pending-extension-delete-{}",
            std::process::id()
        ));
        let previous_config = std::env::var_os("XDG_CONFIG_HOME");
        let previous_state = std::env::var_os("XDG_STATE_HOME");
        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", root.join("config"));
            std::env::set_var("XDG_STATE_HOME", root.join("state"));
        }
        let mut app = test_app();
        app.extension_state
            .transact(&crate::extension_state::ExtensionStateTransactionV1 {
                contract: crate::extension_state::EXTENSION_STATE_CONTRACT_V1.into(),
                namespace: "vqro.collections".into(),
                expected_store_generation: 1,
                expected_revision: 0,
                idempotency_key: "seed-pending-delete".into(),
                operations: vec![crate::extension_state::ExtensionStateOperationV1 {
                    key: "retained".into(),
                    value: Some(serde_json::json!(true)),
                }],
            })
            .unwrap();
        let _ = result(&app.handle_api_request(Request {
            id: "remove".into(),
            method: Method::ExtensionRemove(ExtensionConfirmedParams {
                repository: "ttenneb/vqro-extension-delegation".into(),
                confirmed: true,
            }),
        }));
        let data_id = app.marketplace_state.entries["ttenneb/vqro-extension-delegation"]
            .data_id
            .clone();
        let config_marker = crate::plugin_paths::plugin_config_dir(&data_id).join("marker");
        let state_marker = crate::plugin_paths::plugin_state_dir(&data_id).join("marker");
        let artifact_marker = crate::extension_artifact::artifact_data_root(&data_id)
            .unwrap()
            .join("marker");
        std::fs::create_dir_all(config_marker.parent().unwrap()).unwrap();
        std::fs::create_dir_all(state_marker.parent().unwrap()).unwrap();
        std::fs::create_dir_all(artifact_marker.parent().unwrap()).unwrap();
        std::fs::write(&config_marker, "keep").unwrap();
        std::fs::write(&state_marker, "keep").unwrap();
        std::fs::write(&artifact_marker, "keep").unwrap();
        let before_marketplace = app.marketplace_state.clone();
        let before_state = app.extension_state.clone();
        app.extension_state_transaction_pending = true;
        let response: serde_json::Value = serde_json::from_str(&app.handle_api_request(Request {
            id: "delete".into(),
            method: Method::ExtensionDataDelete(ExtensionConfirmedParams {
                repository: "ttenneb/vqro-extension-delegation".into(),
                confirmed: true,
            }),
        }))
        .unwrap();
        assert_eq!(response["error"]["code"], "extension_state_busy");
        assert_eq!(app.marketplace_state, before_marketplace);
        assert_eq!(app.extension_state, before_state);
        assert_eq!(std::fs::read_to_string(&config_marker).unwrap(), "keep");
        assert_eq!(std::fs::read_to_string(&state_marker).unwrap(), "keep");
        assert_eq!(std::fs::read_to_string(&artifact_marker).unwrap(), "keep");
        match previous_config {
            Some(value) => unsafe { std::env::set_var("XDG_CONFIG_HOME", value) },
            None => unsafe { std::env::remove_var("XDG_CONFIG_HOME") },
        }
        match previous_state {
            Some(value) => unsafe { std::env::set_var("XDG_STATE_HOME", value) },
            None => unsafe { std::env::remove_var("XDG_STATE_HOME") },
        }
        let _ = std::fs::remove_dir_all(root);
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
        let repository = "ttenneb/vqro-extension-delegation".to_string();
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
        assert!(app.disabled_extensions.contains("vqro.delegation"));

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
        assert!(!app.disabled_extensions.contains("vqro.delegation"));
    }

    #[tokio::test]
    async fn package_retirement_revokes_extension_authority_without_destroying_host_ptys() {
        let mut app = test_app();
        app.state.workspaces = vec![crate::workspace::Workspace::test_new("retained-pty")];
        app.state.ensure_test_terminals();
        let terminal_ids = app.state.terminals.keys().cloned().collect::<Vec<_>>();
        let mut receivers = Vec::new();
        for terminal_id in &terminal_ids {
            let (runtime, receiver) =
                crate::terminal::TerminalRuntime::test_with_channel_capacity(80, 24, 1);
            app.terminal_runtimes.insert(terminal_id.clone(), runtime);
            receivers.push(receiver);
        }
        let runtime_count = app.terminal_runtimes.len();
        assert!(!terminal_ids.is_empty());
        assert!(terminal_ids
            .iter()
            .all(|terminal_id| app.terminal_runtimes.get(terminal_id).is_some()));

        app.marketplace_state
            .packages
            .register(
                crate::extension_package::PackageLifecycleRecordV1 {
                    package_id: "example.retained".into(),
                    source: crate::extension_package::PackageProvenanceV1::File {
                        package_sha256: "a".repeat(64),
                    },
                    previous_source: None,
                    namespaces: vec!["example.retained".into()],
                    revision: 1,
                    status: crate::extension_package::PackageLifecycleStatusV1::Enabled,
                    active: Some(crate::extension_package::PackageArtifactPointerV1 {
                        package_sha256: "a".repeat(64),
                        version: "1.0.0".into(),
                        runtime_generation: 1,
                    }),
                    previous: None,
                    activation_error: None,
                    pending_activation: None,
                    retained_data: true,
                },
                0,
                &std::collections::BTreeMap::new(),
            )
            .unwrap();
        app.marketplace_state
            .packages
            .remove_retaining_data("example.retained")
            .unwrap();
        app.retire_extension_package_runtime("example.retained");

        let record = &app.marketplace_state.packages.packages["example.retained"];
        assert!(record.retained_data);
        assert_eq!(
            record.status,
            crate::extension_package::PackageLifecycleStatusV1::RemovedRetainingData
        );
        assert!(app
            .marketplace_state
            .packages
            .namespace_owners()
            .contains_key("example.retained"));
        assert_eq!(app.terminal_runtimes.len(), runtime_count);
        assert!(terminal_ids
            .iter()
            .all(|terminal_id| app.terminal_runtimes.get(terminal_id).is_some()));
    }
}
