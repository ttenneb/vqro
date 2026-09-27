use crate::app::App;

impl App {
    /// Issues a staged read-only terminal authorization. Production activation does not call this.
    #[allow(dead_code)]
    pub(crate) fn stage_host_terminals_authorization(
        &mut self,
        binding: &crate::extension_host_state::HostProviderBinding,
        workspace_id: &str,
        tab_id: &crate::workspace::TabId,
        service_generation: u64,
        provider_generation: u64,
        runtime_generation: u64,
    ) -> Result<crate::extension_host_terminals::HostTerminalsInvocationAuthorization, String> {
        if service_generation == 0 || provider_generation == 0 || runtime_generation == 0 {
            return Err("host-terminal generations must be nonzero".into());
        }
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
            return Err("package does not own the requested artifact, service, namespace, or runtime generation".into());
        }
        let catalog_generation = registry.revision;
        let package_generation = record.revision;
        let artifact_generation = active.runtime_generation;
        let scope_exists = self.state.workspaces.iter().any(|workspace| {
            workspace.id == workspace_id && workspace.tabs.iter().any(|tab| &tab.id == tab_id)
        });
        if !scope_exists {
            return Err(
                "terminal snapshot scope does not identify an exact workspace and tab".into(),
            );
        }

        let authority = self.issue_staged_host_invocation_authority(
            binding.package_id(),
            binding.service_id(),
            service_generation,
            provider_generation,
            runtime_generation,
        );
        Ok(
            crate::extension_host_terminals::HostTerminalsInvocationAuthorization {
                package_id: binding.package_id().to_owned(),
                service_id: binding.service_id().to_owned(),
                namespace: binding.namespace().to_owned(),
                artifact_sha256: binding.artifact_sha256().to_owned(),
                artifact_version: binding.artifact_version().to_owned(),
                workspace_id: workspace_id.to_owned(),
                tab_id: tab_id.clone(),
                provisional_container: None,
                fence: crate::extension_host_terminals::HostTerminalsLeaseFenceV1 {
                    catalog_generation,
                    package_generation,
                    service_generation,
                    artifact_generation,
                    provider_generation,
                    runtime_generation,
                },
                authority,
                event_tx: self.event_tx.clone(),
            },
        )
    }

    fn host_terminals_lease_current(
        &self,
        lease: &crate::extension_host_terminals::HostTerminalsInvocationLease,
    ) -> bool {
        if lease.outer_request_id.is_empty() || !lease.authority_current() {
            return false;
        }
        let registry = &self.marketplace_state.packages;
        let Some(record) = registry.packages.get(&lease.package_id) else {
            return false;
        };
        registry.revision == lease.fence.catalog_generation
            && record.revision == lease.fence.package_generation
            && record.status == crate::extension_package::PackageLifecycleStatusV1::Enabled
            && record
                .namespaces
                .iter()
                .any(|namespace| namespace == &lease.namespace)
            && record.active.as_ref().is_some_and(|active| {
                active.runtime_generation == lease.fence.artifact_generation
                    && active.package_sha256 == lease.artifact_sha256
                    && active.version == lease.artifact_version
            })
            && self
                .host_invocation_authorities
                .get(&(lease.package_id.clone(), lease.service_id.clone()))
                .is_some_and(|authority| std::sync::Arc::ptr_eq(authority, &lease.authority))
            && self.state.workspaces.iter().any(|workspace| {
                workspace.id == lease.workspace_id
                    && workspace.tabs.iter().any(|tab| tab.id == lease.tab_id)
            })
    }

    pub(crate) fn handle_extension_host_terminals_call(
        &mut self,
        request: crate::extension_host_terminals::HostTerminalsActorRequest,
    ) {
        let send = |result| {
            let _ = request.respond_to.try_send(result);
        };
        if request
            .lease
            .canceled
            .load(std::sync::atomic::Ordering::Acquire)
        {
            send(Err(crate::extension_runtime::RuntimeErrorBodyV1 {
                code: "host_call_canceled".into(),
                message: "outer invocation was canceled".into(),
            }));
            return;
        }
        if std::time::Instant::now() >= request.lease.deadline {
            send(Err(crate::extension_runtime::RuntimeErrorBodyV1 {
                code: "host_call_timeout".into(),
                message: "outer invocation deadline expired".into(),
            }));
            return;
        }
        // Both capture and the final authorization gate revalidate package/service/artifact
        // and exact workspace/tab scope in their own bounded App-actor turn.
        if !self.host_terminals_lease_current(&request.lease) {
            send(Err(crate::extension_host_terminals::stale_error(
                "package, service, provider, artifact, runtime, workspace, or tab scope changed",
            )));
            return;
        }
        match request.method {
            crate::extension_host_terminals::HostTerminalsActorMethod::Capture => {
                let result = self
                    .state
                    .workspaces
                    .iter()
                    .find(|workspace| workspace.id == request.lease.workspace_id)
                    .and_then(|workspace| {
                        workspace
                            .tabs
                            .iter()
                            .find(|tab| tab.id == request.lease.tab_id)
                            .map(|tab| (workspace, tab))
                    })
                    .ok_or_else(|| {
                        crate::extension_host_terminals::stale_error(
                            "terminal snapshot scope disappeared",
                        )
                    })
                    .and_then(|(workspace, tab)| {
                        let mut snapshot = crate::extension_host_terminals::build_snapshot(
                            workspace,
                            tab,
                            &self.state.terminals,
                            request.lease.fence.clone(),
                        )?;
                        if let Some(container_id) = &request.lease.provisional_container {
                            crate::extension_host_terminals::append_provisional_container(
                                &mut snapshot,
                                container_id,
                            )?;
                        }
                        Ok(snapshot)
                    })
                    .and_then(|snapshot| {
                        serde_json::to_value(snapshot).map_err(|error| {
                            crate::extension_runtime::RuntimeErrorBodyV1 {
                                code: "host_call_unavailable".into(),
                                message: format!("cannot encode terminal snapshot: {error}"),
                            }
                        })
                    });
                send(result);
            }
            crate::extension_host_terminals::HostTerminalsActorMethod::Validate => {
                // Final authorization gate only. Snapshot data and fingerprint were
                // linearized by the earlier Capture turn and are not rebuilt here.
                send(Ok(serde_json::Value::Null));
            }
        }
    }
}
