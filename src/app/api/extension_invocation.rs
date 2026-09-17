use std::collections::BTreeMap;

use crate::app::App;

impl App {
    /// Atomically stages one package-bound read context. Production package activation does not
    /// call this method; callers must explicitly select the bounded read purpose.
    #[allow(dead_code)]
    pub(crate) fn stage_host_read_authorization(
        &mut self,
        binding: &crate::extension_host_state::HostProviderBinding,
        purpose: crate::extension_host_invocation::HostReadInvocationPurpose,
        service_generation: u64,
        provider_generation: u64,
        runtime_generation: u64,
    ) -> Result<crate::extension_host_invocation::HostReadInvocationAuthorization, String> {
        if service_generation == 0 || provider_generation == 0 || runtime_generation == 0 {
            return Err("composite host-read generations must be nonzero".into());
        }
        if !purpose.state_snapshot && purpose.terminals.is_none() {
            return Err("composite host-read purpose must select at least one method".into());
        }
        if purpose.state_snapshot && self.extension_state_persistence_blocked {
            return Err("durable extension state authority is unavailable".into());
        }
        let observes_state_then_terminals = purpose.state_snapshot && purpose.terminals.is_some();

        let state_capability = crate::extension_host_state::HOST_STATE_READ_CAPABILITY;
        let terminals_capability = crate::extension_host_terminals::HOST_TERMINALS_READ_CAPABILITY;
        if (purpose.state_snapshot && !binding.capabilities().contains(state_capability))
            || (purpose.terminals.is_some()
                && !binding.capabilities().contains(terminals_capability))
        {
            return Err("composite host-read purpose exceeds installed package grants".into());
        }

        // Validate every requested package and scope fact before issuing or superseding the one
        // App-owned authority. Failure above or in this block leaves authority untouched.
        let registry = &self.marketplace_state.packages;
        let record = registry
            .packages
            .get(binding.package_id())
            .ok_or_else(|| "package is not registered in the package-v1 catalog".to_string())?;
        let active = record
            .active
            .as_ref()
            .filter(|_| {
                record.status == crate::extension_package::PackageLifecycleStatusV1::Enabled
            })
            .ok_or_else(|| "package is not enabled with an active artifact".to_string())?;
        if record.package_id != binding.package_id()
            || !record
                .namespaces
                .iter()
                .any(|owned| owned == binding.namespace())
            || active.runtime_generation != runtime_generation
            || active.package_sha256 != binding.artifact_sha256()
            || active.version != binding.artifact_version()
        {
            return Err(
                "package does not own the requested artifact, service, namespace, or runtime generation"
                    .into(),
            );
        }
        if let Some((workspace_id, tab_id)) = &purpose.terminals {
            let exact_scope = self.state.workspaces.iter().any(|workspace| {
                workspace.id == *workspace_id && workspace.tabs.iter().any(|tab| tab.id == *tab_id)
            });
            if !exact_scope {
                return Err(
                    "terminal snapshot scope does not identify an exact workspace and tab".into(),
                );
            }
        }

        let catalog_generation = registry.revision;
        let package_generation = record.revision;
        let artifact_generation = active.runtime_generation;
        let state_store_id = self.extension_state.store_id.clone();
        let state_store_generation = self.extension_state.store_generation;
        let event_tx = self.event_tx.clone();
        let authority = self.issue_staged_host_invocation_authority(
            binding.package_id(),
            binding.service_id(),
            service_generation,
            provider_generation,
            runtime_generation,
        );

        let state = purpose.state_snapshot.then(|| {
            crate::extension_host_state::HostStateInvocationAuthorization {
                package_id: binding.package_id().to_owned(),
                service_id: binding.service_id().to_owned(),
                namespace: binding.namespace().to_owned(),
                artifact_sha256: binding.artifact_sha256().to_owned(),
                artifact_version: binding.artifact_version().to_owned(),
                catalog_revision: catalog_generation,
                package_revision: package_generation,
                manager_generation: service_generation,
                provider_generation,
                scope_id: state_store_id.clone(),
                scope_revision: package_generation,
                store_id: state_store_id,
                store_generation: state_store_generation,
                authority: authority.clone(),
                event_tx: event_tx.clone(),
            }
        });
        let terminals = purpose.terminals.map(|(workspace_id, tab_id)| {
            crate::extension_host_terminals::HostTerminalsInvocationAuthorization {
                package_id: binding.package_id().to_owned(),
                service_id: binding.service_id().to_owned(),
                namespace: binding.namespace().to_owned(),
                artifact_sha256: binding.artifact_sha256().to_owned(),
                artifact_version: binding.artifact_version().to_owned(),
                workspace_id,
                tab_id,
                fence: crate::extension_host_terminals::HostTerminalsLeaseFenceV1 {
                    catalog_generation,
                    package_generation,
                    service_generation,
                    artifact_generation,
                    provider_generation,
                    runtime_generation,
                },
                authority: authority.clone(),
                event_tx,
            }
        });
        let mut allowed_methods = BTreeMap::new();
        if state.is_some() {
            allowed_methods.insert(
                crate::extension_host_state::HOST_STATE_SNAPSHOT_METHOD.into(),
                state_capability.into(),
            );
        }
        if terminals.is_some() {
            allowed_methods.insert(
                crate::extension_host_terminals::HOST_TERMINALS_SNAPSHOT_METHOD.into(),
                terminals_capability.into(),
            );
        }

        Ok(
            crate::extension_host_invocation::HostReadInvocationAuthorization {
                package_id: binding.package_id().to_owned(),
                service_id: binding.service_id().to_owned(),
                namespace: binding.namespace().to_owned(),
                artifact_sha256: binding.artifact_sha256().to_owned(),
                artifact_version: binding.artifact_version().to_owned(),
                catalog_generation,
                package_generation,
                service_generation,
                artifact_generation,
                provider_generation,
                runtime_generation,
                state,
                terminals,
                allowed_methods,
                authority,
                terminal_observations: if observes_state_then_terminals {
                    crate::extension_host_invocation::TrustedTerminalSnapshotObservations::state_then_terminals()
                } else {
                    crate::extension_host_invocation::TrustedTerminalSnapshotObservations::default()
                },
            },
        )
    }
}
