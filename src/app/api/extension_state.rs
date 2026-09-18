use super::{responses, App};
use crate::api::schema::{ExtensionStateEventsParams, ResponseResult};

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ExtensionStateMutationError {
    Busy,
    Unavailable,
    Persist { detail: String, uncertain: bool },
}

impl std::fmt::Display for ExtensionStateMutationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Busy => {
                formatter.write_str("another durable extension-state mutation is pending")
            }
            Self::Unavailable => {
                formatter.write_str("durable extension state authority is unavailable")
            }
            Self::Persist { detail, .. } => formatter.write_str(detail),
        }
    }
}

#[derive(Debug)]
pub(crate) enum ExtensionStateResponseTarget {
    Native {
        request_id: String,
        respond_to: std::sync::mpsc::Sender<String>,
    },
    HostState {
        respond_to: std::sync::mpsc::SyncSender<
            Result<serde_json::Value, crate::extension_runtime::RuntimeErrorBodyV1>,
        >,
    },
}

#[derive(Debug)]
pub(crate) struct ExtensionStatePersistResult {
    pub(crate) candidate: crate::extension_state::DurableExtensionStateV1,
    pub(crate) commit: crate::extension_state::ExtensionStateCommitV1,
    pub(crate) persisted: Result<(), crate::persist::extension_state::ExtensionStateSaveError>,
    pub(crate) target: ExtensionStateResponseTarget,
}

impl App {
    pub(crate) fn begin_extension_state_mutation(
        &mut self,
    ) -> Result<(), ExtensionStateMutationError> {
        if self.extension_state_persistence_blocked {
            return Err(ExtensionStateMutationError::Unavailable);
        }
        if self.extension_state_transaction_pending {
            return Err(ExtensionStateMutationError::Busy);
        }
        self.extension_state_transaction_pending = true;
        Ok(())
    }

    pub(crate) fn finish_extension_state_mutation(&mut self) {
        self.extension_state_transaction_pending = false;
    }

    pub(crate) fn commit_extension_state_candidate_while_gated(
        &mut self,
        candidate: crate::extension_state::DurableExtensionStateV1,
        commit: Option<&crate::extension_state::ExtensionStateCommitV1>,
    ) -> Result<(), ExtensionStateMutationError> {
        if !self.extension_state_transaction_pending {
            return Err(ExtensionStateMutationError::Busy);
        }
        let persisted = self.extension_state_path.as_deref().map_or(Ok(()), |path| {
            crate::persist::extension_state::save_atomically(path, &candidate)
        });
        let result = match persisted {
            Ok(()) => {
                self.extension_state = candidate;
                if let Some(commit) = commit {
                    self.emit_extension_state_committed(commit);
                }
                Ok(())
            }
            Err(error) => {
                self.extension_state_persistence_blocked = error.replaced;
                let suffix = if error.replaced {
                    " (the file was replaced before durability failed)"
                } else {
                    ""
                };
                Err(ExtensionStateMutationError::Persist {
                    detail: format!("{}{suffix}", error.source),
                    uncertain: error.replaced,
                })
            }
        };
        result
    }

    pub(crate) fn commit_extension_state_candidate_sync(
        &mut self,
        candidate: crate::extension_state::DurableExtensionStateV1,
        commit: Option<&crate::extension_state::ExtensionStateCommitV1>,
    ) -> Result<(), ExtensionStateMutationError> {
        self.begin_extension_state_mutation()?;
        let result = self.commit_extension_state_candidate_while_gated(candidate, commit);
        self.finish_extension_state_mutation();
        result
    }

    pub(super) fn emit_extension_state_committed(
        &self,
        commit: &crate::extension_state::ExtensionStateCommitV1,
    ) {
        self.event_hub.push(crate::api::schema::EventEnvelope {
            event: crate::api::schema::EventKind::ExtensionStateCommitted,
            data: crate::api::schema::EventData::ExtensionStateCommitted {
                namespace: commit.namespace.clone(),
                store_id: commit.store_id.clone(),
                store_generation: commit.store_generation,
                namespace_revision: commit.revision,
                first_sequence: commit.sequence,
                last_sequence: commit.sequence,
            },
        });
    }

    pub(super) fn handle_extension_state_snapshot(
        &self,
        request_id: String,
        namespace: String,
    ) -> String {
        if self.extension_state_persistence_blocked {
            return responses::encode_error(
                request_id,
                "extension_state_unavailable",
                "durable extension state authority is unavailable until restart or repair",
            );
        }
        match self.extension_state.snapshot(&namespace) {
            Ok(snapshot) => responses::encode_success(
                request_id,
                ResponseResult::ExtensionStateSnapshot { snapshot },
            ),
            Err(error) => responses::encode_error(
                request_id,
                "invalid_extension_state_namespace",
                format!("{error:?}"),
            ),
        }
    }

    pub(super) fn handle_extension_state_transact(
        &mut self,
        request_id: String,
        transaction: crate::extension_state::ExtensionStateTransactionV1,
    ) -> String {
        if transaction.namespace == crate::collections_extension_contract::COLLECTIONS_NAMESPACE
            && self.collections_authority.phase
                == crate::collections_extension_contract::CollectionsAuthorityPhaseV1::Package
        {
            return responses::encode_error(
                request_id,
                "collections_action_required",
                "authoritative Collections state changes require a typed package action",
            );
        }
        if self.collections_mutations_blocked {
            return responses::encode_error(
                request_id,
                "extension_state_busy",
                "a Collections checkpoint is in progress",
            );
        }
        if self.extension_state_persistence_blocked {
            return responses::encode_error(
                request_id,
                "extension_state_persistence_blocked",
                "the persisted extension state was rejected; repair or remove it before writing",
            );
        }
        let mut candidate = self.extension_state.clone();
        let commit = match candidate.transact(&transaction) {
            Ok(commit) => commit,
            Err(error) => {
                return responses::encode_error(
                    request_id,
                    extension_state_error_code(&error),
                    format!("{error:?}"),
                );
            }
        };
        if let Err(error) = self.commit_extension_state_candidate_sync(candidate, Some(&commit)) {
            let code = match error {
                ExtensionStateMutationError::Busy => "extension_state_busy",
                ExtensionStateMutationError::Unavailable => "extension_state_persistence_blocked",
                ExtensionStateMutationError::Persist { .. } => "extension_state_persist_failed",
            };
            return responses::encode_error(request_id, code, error.to_string());
        }
        responses::encode_success(
            request_id,
            ResponseResult::ExtensionStateCommitted { commit },
        )
    }

    pub(crate) fn handle_deferred_extension_state_transact(
        &mut self,
        request_id: String,
        transaction: crate::extension_state::ExtensionStateTransactionV1,
        respond_to: std::sync::mpsc::Sender<String>,
    ) -> bool {
        if transaction.namespace == crate::collections_extension_contract::COLLECTIONS_NAMESPACE
            && self.collections_authority.phase
                == crate::collections_extension_contract::CollectionsAuthorityPhaseV1::Package
        {
            let _ = respond_to.send(responses::encode_error(
                request_id,
                "collections_action_required",
                "authoritative Collections state changes require a typed package action",
            ));
            return false;
        }
        if self.collections_mutations_blocked {
            let _ = respond_to.send(responses::encode_error(
                request_id,
                "extension_state_busy",
                "a Collections checkpoint is in progress",
            ));
            return false;
        }
        if self.extension_state_persistence_blocked {
            let _ = respond_to.send(responses::encode_error(
                request_id,
                "extension_state_persistence_blocked",
                "the persisted extension state was rejected; repair or remove it before writing",
            ));
            return false;
        }
        if self.extension_state_transaction_pending {
            let _ = respond_to.send(responses::encode_error(
                request_id,
                "extension_state_busy",
                "another extension state transaction is being durably committed",
            ));
            return false;
        }
        let mut candidate = self.extension_state.clone();
        let commit = match candidate.transact(&transaction) {
            Ok(commit) => commit,
            Err(error) => {
                let _ = respond_to.send(responses::encode_error(
                    request_id,
                    extension_state_error_code(&error),
                    format!("{error:?}"),
                ));
                return false;
            }
        };
        let Some(path) = self.extension_state_path.clone() else {
            if let Err(error) = self.begin_extension_state_mutation() {
                let _ = respond_to.send(responses::encode_error(
                    request_id,
                    "extension_state_busy",
                    error.to_string(),
                ));
                return false;
            }
            self.extension_state = candidate;
            self.emit_extension_state_committed(&commit);
            self.finish_extension_state_mutation();
            let _ = respond_to.send(responses::encode_success(
                request_id,
                ResponseResult::ExtensionStateCommitted { commit },
            ));
            return false;
        };
        let permit = match self.event_tx.clone().try_reserve_owned() {
            Ok(permit) => permit,
            Err(error) => {
                let _ = respond_to.send(responses::encode_error(
                    request_id,
                    "extension_state_busy",
                    format!("event queue unavailable: {error}"),
                ));
                return false;
            }
        };
        if let Err(error) = self.begin_extension_state_mutation() {
            let _ = respond_to.send(responses::encode_error(
                request_id,
                "extension_state_busy",
                error.to_string(),
            ));
            return false;
        }
        let spawn_request_id = request_id.clone();
        let spawn_respond_to = respond_to.clone();
        let spawn = std::thread::Builder::new()
            .name("vqro-extension-state".into())
            .spawn(move || {
                let persisted = crate::persist::extension_state::save_atomically(&path, &candidate);
                permit.send(crate::events::AppEvent::ExtensionStatePersisted(Box::new(
                    ExtensionStatePersistResult {
                        candidate,
                        commit,
                        persisted,
                        target: ExtensionStateResponseTarget::Native {
                            request_id,
                            respond_to,
                        },
                    },
                )));
            });
        match spawn {
            Ok(_) => {}
            Err(error) => {
                self.finish_extension_state_mutation();
                let _ = spawn_respond_to.send(responses::encode_error(
                    spawn_request_id,
                    "extension_state_persist_failed",
                    error.to_string(),
                ));
            }
        }
        false
    }

    pub(crate) fn handle_extension_state_persisted(&mut self, result: ExtensionStatePersistResult) {
        self.finish_extension_state_mutation();
        let outcome = match result.persisted {
            Ok(()) => {
                self.extension_state = result.candidate;
                self.emit_extension_state_committed(&result.commit);
                Ok(result.commit)
            }
            Err(error) => {
                self.extension_state_persistence_blocked = error.replaced;
                let detail = if error.replaced {
                    format!(
                        "{} (the file was replaced before durability failed)",
                        error.source
                    )
                } else {
                    error.source.to_string()
                };
                Err((error.replaced, detail))
            }
        };
        match result.target {
            ExtensionStateResponseTarget::Native {
                request_id,
                respond_to,
            } => {
                let response = match outcome {
                    Ok(commit) => responses::encode_success(
                        request_id,
                        ResponseResult::ExtensionStateCommitted { commit },
                    ),
                    Err((_, detail)) => responses::encode_error(
                        request_id,
                        "extension_state_persist_failed",
                        &detail,
                    ),
                };
                let _ = respond_to.send(response);
            }
            ExtensionStateResponseTarget::HostState { respond_to } => {
                let response = match outcome {
                    Ok(commit) => serde_json::to_value(
                        crate::extension_host_state::HostStateCommitV1::from(commit),
                    )
                    .map_err(|error| {
                        crate::extension_runtime::RuntimeErrorBodyV1 {
                            code: "host_call_unavailable".into(),
                            message: format!("cannot encode host-state commit: {error}"),
                        }
                    }),
                    Err((uncertain, detail)) => Err(crate::extension_runtime::RuntimeErrorBodyV1 {
                        code: if uncertain {
                            "state_persist_uncertain"
                        } else {
                            "state_persist_failed"
                        }
                        .into(),
                        message: detail,
                    }),
                };
                // Durability decides authority even when the invocation has canceled or gone away.
                let _ = respond_to.try_send(response);
            }
        }
    }

    /// Issues or supersedes the shared package/service invocation authority.
    /// Future package lifecycle activation must own this call and revoke the same
    /// authority on disable, replacement, or retirement; production does not yet call it.
    #[allow(dead_code)]
    pub(crate) fn issue_staged_host_invocation_authority(
        &mut self,
        package_id: &str,
        service_id: &str,
        service_generation: u64,
        provider_generation: u64,
        runtime_generation: u64,
    ) -> std::sync::Arc<crate::extension_host_state::HostInvocationAuthority> {
        let authority_key = (package_id.to_owned(), service_id.to_owned());
        match self.host_invocation_authorities.entry(authority_key) {
            std::collections::hash_map::Entry::Occupied(mut entry) => {
                if entry.get().is_revoked() {
                    let replacement = crate::extension_host_state::HostInvocationAuthority::new(
                        service_generation,
                        provider_generation,
                        runtime_generation,
                    );
                    entry.insert(replacement.clone());
                    replacement
                } else {
                    entry.get().supersede(
                        service_generation,
                        provider_generation,
                        runtime_generation,
                    );
                    entry.get().clone()
                }
            }
            std::collections::hash_map::Entry::Vacant(entry) => entry
                .insert(crate::extension_host_state::HostInvocationAuthority::new(
                    service_generation,
                    provider_generation,
                    runtime_generation,
                ))
                .clone(),
        }
    }

    /// Issues staged package-v1 authorization. No production catalog currently invokes this.
    #[allow(dead_code)] // Removed when package activation begins issuing these leases.
    pub(crate) fn stage_host_state_authorization(
        &mut self,
        binding: &crate::extension_host_state::HostStateProviderBinding,
        manager_generation: u64,
        provider_generation: u64,
        runtime_generation: u64,
    ) -> Result<crate::extension_host_state::HostStateInvocationAuthorization, String> {
        if self.extension_state_persistence_blocked {
            return Err("durable extension state authority is unavailable".into());
        }
        if manager_generation == 0 || provider_generation == 0 || runtime_generation == 0 {
            return Err("host-state generations must be nonzero".into());
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
            return Err(
                "package does not own the requested artifact, service, namespace, or runtime generation"
                    .into(),
            );
        }
        let catalog_revision = registry.revision;
        let package_revision = record.revision;
        let artifact_generation = active.runtime_generation;
        let authority = self.issue_staged_host_invocation_authority(
            binding.package_id(),
            binding.service_id(),
            manager_generation,
            provider_generation,
            runtime_generation,
        );
        Ok(
            crate::extension_host_state::HostStateInvocationAuthorization {
                source: crate::extension_host_invocation::HostStateReadSource::Durable,
                package_id: binding.package_id().to_owned(),
                service_id: binding.service_id().to_owned(),
                namespace: binding.namespace().to_owned(),
                artifact_sha256: binding.artifact_sha256().to_owned(),
                artifact_version: binding.artifact_version().to_owned(),
                catalog_revision,
                package_revision,
                manager_generation,
                provider_generation,
                artifact_generation,
                scope_id: self.extension_state.store_id.clone(),
                scope_revision: package_revision,
                store_id: self.extension_state.store_id.clone(),
                store_generation: self.extension_state.store_generation,
                authority,
                event_tx: self.event_tx.clone(),
            },
        )
    }

    pub(crate) fn revoke_host_invocation_authority(&mut self, package_id: &str, service_id: &str) {
        if let Some(authority) = self
            .host_invocation_authorities
            .remove(&(package_id.into(), service_id.into()))
        {
            authority.revoke();
        }
    }

    pub(crate) fn revoke_host_invocation_authority_for_package(&mut self, package_id: &str) {
        let service_ids = self
            .host_invocation_authorities
            .keys()
            .filter(|(candidate, _)| candidate == package_id)
            .map(|(_, service_id)| service_id.clone())
            .collect::<Vec<_>>();
        for service_id in service_ids {
            self.revoke_host_invocation_authority(package_id, &service_id);
        }
    }

    #[allow(dead_code)]
    pub(crate) fn revoke_host_state_authorization(&mut self, package_id: &str, service_id: &str) {
        self.revoke_host_invocation_authority(package_id, service_id);
    }

    pub(crate) fn handle_extension_host_state_call(
        &mut self,
        request: crate::extension_host_state::HostStateActorRequest,
    ) {
        let send_error = |code: &str, message: String| {
            let _ =
                request
                    .respond_to
                    .try_send(Err(crate::extension_runtime::RuntimeErrorBodyV1 {
                        code: code.into(),
                        message,
                    }));
        };
        if request
            .lease
            .canceled
            .load(std::sync::atomic::Ordering::Acquire)
        {
            send_error("host_call_canceled", "outer invocation was canceled".into());
            return;
        }
        if std::time::Instant::now() >= request.lease.deadline {
            send_error(
                "host_call_timeout",
                "outer invocation deadline expired".into(),
            );
            return;
        }
        if !request.lease.authority.permits(&request.lease) {
            send_error(
                "stale_host_call_lease",
                "host call lease was revoked".into(),
            );
            return;
        }
        let registry = &self.marketplace_state.packages;
        let Some(record) = registry.packages.get(&request.lease.package_id) else {
            send_error(
                "stale_host_call_lease",
                "package catalog ownership disappeared".into(),
            );
            return;
        };
        let state_store = match request.lease.source {
            crate::extension_host_invocation::HostStateReadSource::None => None,
            crate::extension_host_invocation::HostStateReadSource::Durable => {
                Some(&self.extension_state)
            }
            crate::extension_host_invocation::HostStateReadSource::CollectionsShadow => {
                Some(self.collections_import_overlay.state())
            }
        };
        let ownership_current = !request.lease.outer_request_id.is_empty()
            && registry.revision == request.lease.catalog_revision
            && record.revision == request.lease.package_revision
            && request.lease.scope_revision == record.revision
            && record.status == crate::extension_package::PackageLifecycleStatusV1::Enabled
            && record.active.as_ref().is_some_and(|active| {
                active.runtime_generation == request.lease.artifact_generation
                    && active.package_sha256 == request.lease.artifact_sha256
                    && active.version == request.lease.artifact_version
            })
            && self
                .host_invocation_authorities
                .get(&(
                    request.lease.package_id.clone(),
                    request.lease.service_id.clone(),
                ))
                .is_some_and(|authority| {
                    std::sync::Arc::ptr_eq(authority, &request.lease.authority)
                })
            && record
                .namespaces
                .iter()
                .any(|namespace| namespace == &request.lease.namespace)
            && state_store.is_some_and(|state| {
                request.lease.scope_id == state.store_id
                    && request.lease.store_id == state.store_id
                    && request.lease.store_generation == state.store_generation
            });
        if !ownership_current {
            send_error(
                "stale_host_call_lease",
                "package ownership, scope, or state generation changed".into(),
            );
            return;
        }
        if self.extension_state_persistence_blocked {
            send_error(
                "state_persist_uncertain",
                "durable extension state authority is unavailable".into(),
            );
            return;
        }
        match request.method {
            crate::extension_host_state::HostStateActorMethod::Snapshot => {
                let result = state_store
                    .expect("current host-state lease selected a store")
                    .snapshot(&request.lease.namespace)
                    .map(crate::extension_host_state::HostStateSnapshotV1::from)
                    .map_err(|error| crate::extension_host_state::state_error(&error))
                    .and_then(|snapshot| {
                        serde_json::to_value(snapshot).map_err(|error| {
                            crate::extension_runtime::RuntimeErrorBodyV1 {
                                code: "host_call_unavailable".into(),
                                message: format!("cannot encode host-state snapshot: {error}"),
                            }
                        })
                    });
                let _ = request.respond_to.try_send(result);
            }
            crate::extension_host_state::HostStateActorMethod::Transact(transaction) => {
                if request.lease.source
                    != crate::extension_host_invocation::HostStateReadSource::Durable
                {
                    send_error(
                        "capability_denied",
                        "non-authoritative shadow state is read-only".into(),
                    );
                    return;
                }
                if self.extension_state_transaction_pending {
                    send_error(
                        "host_call_busy",
                        "another durable mutation is pending".into(),
                    );
                    return;
                }
                let mut candidate = self.extension_state.clone();
                let commit = match candidate.transact(&transaction) {
                    Ok(commit) => commit,
                    Err(error) => {
                        let _ = request
                            .respond_to
                            .try_send(Err(crate::extension_host_state::state_error(&error)));
                        return;
                    }
                };
                let Some(path) = self.extension_state_path.clone() else {
                    if let Err(error) = self.begin_extension_state_mutation() {
                        send_error("host_call_busy", error.to_string());
                        return;
                    }
                    self.extension_state = candidate;
                    self.emit_extension_state_committed(&commit);
                    self.finish_extension_state_mutation();
                    let result = serde_json::to_value(
                        crate::extension_host_state::HostStateCommitV1::from(commit),
                    )
                    .map_err(|error| {
                        crate::extension_runtime::RuntimeErrorBodyV1 {
                            code: "host_call_unavailable".into(),
                            message: format!("cannot encode host-state commit: {error}"),
                        }
                    });
                    let _ = request.respond_to.try_send(result);
                    return;
                };
                let permit = match self.event_tx.clone().try_reserve_owned() {
                    Ok(permit) => permit,
                    Err(error) => {
                        send_error(
                            "host_call_busy",
                            format!("event queue unavailable: {error}"),
                        );
                        return;
                    }
                };
                if let Err(error) = self.begin_extension_state_mutation() {
                    send_error("host_call_busy", error.to_string());
                    return;
                }
                let target = ExtensionStateResponseTarget::HostState {
                    respond_to: request.respond_to,
                };
                let spawn = std::thread::Builder::new()
                    .name("vqro-extension-state".into())
                    .spawn(move || {
                        let persisted =
                            crate::persist::extension_state::save_atomically(&path, &candidate);
                        permit.send(crate::events::AppEvent::ExtensionStatePersisted(Box::new(
                            ExtensionStatePersistResult {
                                candidate,
                                commit,
                                persisted,
                                target,
                            },
                        )));
                    });
                if let Err(error) = spawn {
                    self.finish_extension_state_mutation();
                    // The response target moved into the failed closure and disconnects here.
                    tracing::warn!(%error, "failed to spawn staged host-state persistence");
                }
            }
        }
    }

    pub(super) fn handle_extension_state_events(
        &self,
        request_id: String,
        params: ExtensionStateEventsParams,
    ) -> String {
        if self.extension_state_persistence_blocked {
            return responses::encode_error(
                request_id,
                "extension_state_unavailable",
                "durable extension state authority is unavailable until restart or repair",
            );
        }
        if params.store_id != self.extension_state.store_id
            || params.store_generation != self.extension_state.store_generation
        {
            return responses::encode_error(
                request_id,
                "extension_state_store_mismatch",
                "the extension state store identity changed; request a new snapshot",
            );
        }
        let page = self
            .extension_state
            .events_after(params.after_sequence, params.limit.unwrap_or(128));
        responses::encode_success(request_id, ResponseResult::ExtensionStateEvents { page })
    }
}

fn extension_state_error_code(error: &crate::extension_state::ExtensionStateError) -> &'static str {
    match error {
        crate::extension_state::ExtensionStateError::StoreGenerationConflict { .. } => {
            "extension_state_generation_conflict"
        }
        crate::extension_state::ExtensionStateError::RevisionConflict { .. } => {
            "extension_state_revision_conflict"
        }
        crate::extension_state::ExtensionStateError::IdempotencyConflict => {
            "extension_state_idempotency_conflict"
        }
        crate::extension_state::ExtensionStateError::LimitExceeded(_) => "resource_limit",
        _ => "invalid_extension_state_transaction",
    }
}

#[cfg(test)]
mod tests {
    use crate::api::schema::{Method, Request, ResponseResult, SuccessResponse};
    use crate::config::Config;
    use crate::extension_state::{
        ExtensionStateOperationV1, ExtensionStateTransactionV1, EXTENSION_STATE_CONTRACT_V1,
    };

    fn app() -> crate::app::App {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        crate::app::App::new(
            &Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        )
    }

    fn install_staged_package(app: &mut crate::app::App, runtime_generation: u64) {
        app.marketplace_state.packages.revision = 1;
        app.marketplace_state.packages.packages.insert(
            "example.package".into(),
            crate::extension_package::PackageLifecycleRecordV1 {
                package_id: "example.package".into(),
                source: crate::extension_package::PackageProvenanceV1::File {
                    package_sha256: "a".repeat(64),
                },
                previous_source: None,
                namespaces: vec!["example.extension".into()],
                revision: 3,
                status: crate::extension_package::PackageLifecycleStatusV1::Enabled,
                active: Some(crate::extension_package::PackageArtifactPointerV1 {
                    package_sha256: "a".repeat(64),
                    version: "1.0.0".into(),
                    runtime_generation,
                }),
                previous: None,
                activation_error: None,
                pending_activation: None,
                retained_data: false,
            },
        );
    }

    fn staged_handler(
        app: &mut crate::app::App,
        runtime_generation: u64,
        capabilities: &[&str],
        canceled: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> (
        crate::extension_host_state::HostStateCallHandler,
        std::sync::Arc<crate::extension_host_state::HostStateLeaseAuthority>,
    ) {
        let binding = crate::extension_host_state::HostStateProviderBinding::test_new(
            "example.package",
            "state",
            "example.extension",
            "a".repeat(64),
            "1.0.0",
        );
        let authorization = app
            .stage_host_state_authorization(&binding, 5, 7, runtime_generation)
            .unwrap();
        let authority = authorization.authority.clone();
        let identity = crate::extension_runtime::RuntimeIdentityV1 {
            package_id: "example.package".into(),
            namespace: "example.extension".into(),
            service_id: "state".into(),
            generation: runtime_generation,
        };
        let lease = authorization.bind(
            "outer-1".into(),
            identity.clone(),
            std::time::Instant::now() + std::time::Duration::from_secs(2),
            canceled,
        );
        (
            crate::extension_host_state::HostStateCallHandler::new(
                identity,
                capabilities
                    .iter()
                    .map(|value| (*value).to_string())
                    .collect(),
                lease,
                authorization.event_tx,
            ),
            authority,
        )
    }

    fn host_call(
        generation: u64,
        capability: &str,
        method: &str,
        namespace: &str,
        params: serde_json::Value,
    ) -> crate::extension_runtime::RuntimeHostCallV1 {
        crate::extension_runtime::RuntimeHostCallV1 {
            kind: "host_call".into(),
            call_id: "call-1".into(),
            identity: crate::extension_runtime::RuntimeIdentityV1 {
                package_id: "example.package".into(),
                namespace: "example.extension".into(),
                service_id: "state".into(),
                generation,
            },
            depth: 1,
            capability: capability.into(),
            method: method.into(),
            namespace: namespace.into(),
            params,
        }
    }

    fn transaction(generation: u64) -> ExtensionStateTransactionV1 {
        ExtensionStateTransactionV1 {
            contract: EXTENSION_STATE_CONTRACT_V1.into(),
            namespace: "example.extension".into(),
            expected_store_generation: generation,
            expected_revision: 0,
            idempotency_key: "request-1".into(),
            operations: vec![ExtensionStateOperationV1 {
                key: "mode".into(),
                value: Some(serde_json::json!("fast")),
            }],
        }
    }

    #[test]
    fn api_transaction_commits_atomically_and_events_are_replayable() {
        let mut app = app();
        let generation = app.extension_state.store_generation;
        let response = app.handle_api_request(Request {
            id: "transact".into(),
            method: Method::ExtensionStateTransactV1(transaction(generation)),
        });
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::ExtensionStateCommitted { commit } = success.result else {
            panic!("expected extension state commit");
        };
        assert_eq!(commit.revision, 1);
        let live_events = app.event_hub.events_after(0);
        assert!(live_events.iter().any(|(_, event)| {
            matches!(
                &event.data,
                crate::api::schema::EventData::ExtensionStateCommitted {
                    namespace,
                    namespace_revision: 1,
                    ..
                } if namespace == "example.extension"
            )
        }));

        let page = app.extension_state.events_after(0, 10);
        assert_eq!(page.events.len(), 1);
        assert_eq!(
            app.extension_state
                .snapshot("example.extension")
                .unwrap()
                .values["mode"],
            serde_json::json!("fast")
        );
    }

    #[test]
    fn restart_preserves_store_identity_events_and_idempotency_receipts() {
        let _guard = crate::config::test_config_env_lock().lock().unwrap();
        let directory = std::env::temp_dir().join(format!(
            "vqro-extension-state-restart-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::env::set_var("XDG_CONFIG_HOME", &directory);
        std::env::remove_var(crate::session::SESSION_ENV_VAR);
        let policy = crate::app::AppPolicy {
            restore_session: false,
            persist_session: true,
            persist_plugin_registry: false,
            background_updates: false,
        };
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut first = crate::app::App::new(
            &Config::default(),
            policy,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        let generation = first.extension_state.store_generation;
        let transaction = transaction(generation);
        let response = first.handle_extension_state_transact("first".into(), transaction.clone());
        let first_response: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::ExtensionStateCommitted {
            commit: first_commit,
        } = first_response.result
        else {
            panic!("expected first commit");
        };
        let store_id = first.extension_state.store_id.clone();
        drop(first);

        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut restored = crate::app::App::new(
            &Config::default(),
            policy,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        assert_eq!(restored.extension_state.store_id, store_id);
        assert_eq!(restored.extension_state.store_generation, generation);
        assert_eq!(restored.extension_state.events_after(0, 10).events.len(), 1);
        assert_eq!(
            restored
                .extension_state
                .snapshot("example.extension")
                .unwrap()
                .values["mode"],
            serde_json::json!("fast")
        );
        let replay = restored.handle_extension_state_transact("replay".into(), transaction);
        let replay: SuccessResponse = serde_json::from_str(&replay).unwrap();
        let ResponseResult::ExtensionStateCommitted { commit } = replay.result else {
            panic!("expected replayed commit");
        };
        assert!(commit.replayed);
        assert_eq!(commit.sequence, first_commit.sequence);

        std::env::remove_var("XDG_CONFIG_HOME");
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn staged_snapshot_is_package_namespace_and_lease_bound() {
        use crate::extension_runtime::RuntimeHostCallHandler;

        let mut app = app();
        install_staged_package(&mut app, 11);
        for binding in [
            crate::extension_host_state::HostStateProviderBinding::test_new(
                "example.package",
                "other-service",
                "example.extension",
                "b".repeat(64),
                "1.0.0",
            ),
            crate::extension_host_state::HostStateProviderBinding::test_new(
                "example.package",
                "state",
                "example.extension",
                "a".repeat(64),
                "2.0.0",
            ),
        ] {
            assert!(app
                .stage_host_state_authorization(&binding, 5, 7, 11)
                .is_err());
        }
        let canceled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (handler, authority) = staged_handler(
            &mut app,
            11,
            &[crate::extension_host_state::HOST_STATE_READ_CAPABILITY],
            canceled,
        );
        let call = host_call(
            11,
            crate::extension_host_state::HOST_STATE_READ_CAPABILITY,
            crate::extension_host_state::HOST_STATE_SNAPSHOT_METHOD,
            "example.extension",
            serde_json::json!({"contract": "host.state.v1"}),
        );
        let join = std::thread::spawn(move || handler.handle_host_call(call));
        let event = app.event_rx.blocking_recv().unwrap();
        app.handle_internal_event(event);
        let response = join.join().unwrap();
        assert!(response.error.is_none());
        assert_eq!(response.result.unwrap()["namespace"], "example.extension");

        let (catalog_handler, _) = staged_handler(
            &mut app,
            11,
            &[crate::extension_host_state::HOST_STATE_READ_CAPABILITY],
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        );
        app.marketplace_state.packages.revision += 1;
        let catalog_call = host_call(
            11,
            crate::extension_host_state::HOST_STATE_READ_CAPABILITY,
            crate::extension_host_state::HOST_STATE_SNAPSHOT_METHOD,
            "example.extension",
            serde_json::json!({"contract": "host.state.v1"}),
        );
        let catalog_join =
            std::thread::spawn(move || catalog_handler.handle_host_call(catalog_call));
        let event = app.event_rx.blocking_recv().unwrap();
        app.handle_internal_event(event);
        assert_eq!(
            catalog_join.join().unwrap().error.unwrap().code,
            "stale_host_call_lease"
        );
        app.marketplace_state.packages.revision = 1;

        let (artifact_handler, _) = staged_handler(
            &mut app,
            11,
            &[crate::extension_host_state::HOST_STATE_READ_CAPABILITY],
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        );
        app.marketplace_state
            .packages
            .packages
            .get_mut("example.package")
            .unwrap()
            .active
            .as_mut()
            .unwrap()
            .package_sha256 = "c".repeat(64);
        let artifact_call = host_call(
            11,
            crate::extension_host_state::HOST_STATE_READ_CAPABILITY,
            crate::extension_host_state::HOST_STATE_SNAPSHOT_METHOD,
            "example.extension",
            serde_json::json!({"contract": "host.state.v1"}),
        );
        let artifact_join =
            std::thread::spawn(move || artifact_handler.handle_host_call(artifact_call));
        let event = app.event_rx.blocking_recv().unwrap();
        app.handle_internal_event(event);
        assert_eq!(
            artifact_join.join().unwrap().error.unwrap().code,
            "stale_host_call_lease"
        );
        app.marketplace_state
            .packages
            .packages
            .get_mut("example.package")
            .unwrap()
            .active
            .as_mut()
            .unwrap()
            .package_sha256 = "a".repeat(64);

        authority.revoke();
        let (revoked_handler, revoked_authority) = staged_handler(
            &mut app,
            11,
            &[crate::extension_host_state::HOST_STATE_READ_CAPABILITY],
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        );
        revoked_authority.revoke();
        let revoked = revoked_handler.handle_host_call(host_call(
            11,
            crate::extension_host_state::HOST_STATE_READ_CAPABILITY,
            crate::extension_host_state::HOST_STATE_SNAPSHOT_METHOD,
            "example.extension",
            serde_json::json!({"contract": "host.state.v1"}),
        ));
        assert_eq!(revoked.error.unwrap().code, "stale_host_call_lease");

        let (superseded_handler, superseded_authority) = staged_handler(
            &mut app,
            11,
            &[crate::extension_host_state::HOST_STATE_READ_CAPABILITY],
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        );
        superseded_authority.supersede(6, 8, 12);
        let superseded = superseded_handler.handle_host_call(host_call(
            11,
            crate::extension_host_state::HOST_STATE_READ_CAPABILITY,
            crate::extension_host_state::HOST_STATE_SNAPSHOT_METHOD,
            "example.extension",
            serde_json::json!({"contract": "host.state.v1"}),
        ));
        assert_eq!(superseded.error.unwrap().code, "stale_host_call_lease");

        let (handler, _) = staged_handler(
            &mut app,
            11,
            &[crate::extension_host_state::HOST_STATE_READ_CAPABILITY],
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        );
        // A namespace mismatch is rejected before it can enter the App queue.
        let response = handler.handle_host_call(host_call(
            11,
            crate::extension_host_state::HOST_STATE_READ_CAPABILITY,
            crate::extension_host_state::HOST_STATE_SNAPSHOT_METHOD,
            "another.namespace",
            serde_json::json!({"contract": "host.state.v1"}),
        ));
        assert_eq!(response.error.unwrap().code, "invalid_host_call");
    }

    #[test]
    fn staged_success_is_rechecked_for_cancellation_deadline_and_revocation() {
        use crate::extension_runtime::RuntimeHostCallHandler;

        enum Race {
            Cancel,
            Deadline,
            Revoke,
        }
        for race in [Race::Cancel, Race::Deadline, Race::Revoke] {
            let mut app = app();
            install_staged_package(&mut app, 18);
            let canceled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let (handler, authority) = staged_handler(
                &mut app,
                18,
                &[crate::extension_host_state::HOST_STATE_READ_CAPABILITY],
                canceled.clone(),
            );
            let handler = if matches!(race, Race::Deadline) {
                handler
                    .with_deadline(std::time::Instant::now() + std::time::Duration::from_millis(20))
            } else {
                handler
            };
            let gate = crate::extension_host_state::HostStateSuccessGate::new();
            let handler = handler.with_success_gate(gate.clone());
            let call = host_call(
                18,
                crate::extension_host_state::HOST_STATE_READ_CAPABILITY,
                crate::extension_host_state::HOST_STATE_SNAPSHOT_METHOD,
                "example.extension",
                serde_json::json!({"contract": "host.state.v1"}),
            );
            let join = std::thread::spawn(move || handler.handle_host_call(call));
            let event = app.event_rx.blocking_recv().unwrap();
            app.handle_internal_event(event);
            gate.received.wait();
            let expected = match race {
                Race::Cancel => {
                    canceled.store(true, std::sync::atomic::Ordering::Release);
                    "host_call_canceled"
                }
                Race::Deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(25));
                    "host_call_timeout"
                }
                Race::Revoke => {
                    app.revoke_host_state_authorization("example.package", "state");
                    assert!(authority.is_revoked());
                    "stale_host_call_lease"
                }
            };
            gate.release.wait();
            assert_eq!(join.join().unwrap().error.unwrap().code, expected);
        }
    }

    #[test]
    fn staged_transaction_persists_json_null_before_success_and_replays() {
        use crate::extension_runtime::RuntimeHostCallHandler;

        let mut app = app();
        install_staged_package(&mut app, 12);
        let directory = std::env::temp_dir().join(format!(
            "vqro-host-state-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("state.json");
        app.extension_state_path = Some(path.clone());
        let generation = app.extension_state.store_generation;
        let params = serde_json::json!({
            "contract": "host.state.v1",
            "expected_store_generation": generation,
            "expected_revision": 0,
            "idempotency_key": "null-1",
            "operations": [{"op": "set", "key": "nullable", "value": null}]
        });
        let run = |app: &mut crate::app::App| {
            let (handler, _) = staged_handler(
                app,
                12,
                &[crate::extension_host_state::HOST_STATE_WRITE_CAPABILITY],
                std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            );
            let call = host_call(
                12,
                crate::extension_host_state::HOST_STATE_WRITE_CAPABILITY,
                crate::extension_host_state::HOST_STATE_TRANSACT_METHOD,
                "example.extension",
                params.clone(),
            );
            let join = std::thread::spawn(move || handler.handle_host_call(call));
            let request_event = app.event_rx.blocking_recv().unwrap();
            app.handle_internal_event(request_event);
            assert!(app.extension_state_transaction_pending);
            let persisted_event = app.event_rx.blocking_recv().unwrap();
            app.handle_internal_event(persisted_event);
            join.join().unwrap()
        };
        let first = run(&mut app);
        assert!(first.error.is_none());
        assert_eq!(
            app.extension_state
                .snapshot("example.extension")
                .unwrap()
                .values["nullable"],
            serde_json::Value::Null
        );
        assert!(matches!(
            crate::persist::extension_state::load(&path),
            crate::persist::extension_state::ExtensionStateLoad::Loaded(_)
        ));
        let replay = run(&mut app);
        assert_eq!(replay.result.unwrap()["replayed"], true);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn one_mutation_gate_rejects_native_host_and_namespace_candidates() {
        use crate::extension_runtime::RuntimeHostCallHandler;

        let mut app = app();
        install_staged_package(&mut app, 17);
        app.extension_state_transaction_pending = true;

        let (native_tx, native_rx) = std::sync::mpsc::channel();
        app.handle_deferred_extension_state_transact(
            "native".into(),
            transaction(app.extension_state.store_generation),
            native_tx,
        );
        assert!(native_rx.recv().unwrap().contains("extension_state_busy"));

        let candidate = app.extension_state.clone();
        assert_eq!(
            app.commit_extension_state_candidate_sync(candidate, None)
                .unwrap_err(),
            super::ExtensionStateMutationError::Busy
        );

        let (handler, _) = staged_handler(
            &mut app,
            17,
            &[crate::extension_host_state::HOST_STATE_WRITE_CAPABILITY],
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        );
        let call = host_call(
            17,
            crate::extension_host_state::HOST_STATE_WRITE_CAPABILITY,
            crate::extension_host_state::HOST_STATE_TRANSACT_METHOD,
            "example.extension",
            serde_json::json!({
                "contract": "host.state.v1",
                "expected_store_generation": app.extension_state.store_generation,
                "expected_revision": 0,
                "idempotency_key": "busy",
                "operations": [{"op": "set", "key": "x", "value": 1}]
            }),
        );
        let join = std::thread::spawn(move || handler.handle_host_call(call));
        let event = app.event_rx.blocking_recv().unwrap();
        app.handle_internal_event(event);
        assert_eq!(join.join().unwrap().error.unwrap().code, "host_call_busy");
    }

    #[test]
    fn staged_host_state_queue_saturation_is_nonblocking_and_allocates_nothing() {
        use crate::extension_runtime::RuntimeHostCallHandler;

        let mut app = app();
        install_staged_package(&mut app, 16);
        let (handler, _) = staged_handler(
            &mut app,
            16,
            &[crate::extension_host_state::HOST_STATE_READ_CAPABILITY],
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        );
        while app
            .event_tx
            .try_send(crate::events::AppEvent::UpdateReady {
                version: "test".into(),
                install_command: "test".into(),
            })
            .is_ok()
        {}
        let response = handler.handle_host_call(host_call(
            16,
            crate::extension_host_state::HOST_STATE_READ_CAPABILITY,
            crate::extension_host_state::HOST_STATE_SNAPSHOT_METHOD,
            "example.extension",
            serde_json::json!({"contract": "host.state.v1"}),
        ));
        assert_eq!(response.error.unwrap().code, "host_call_busy");
        assert_eq!(
            app.extension_state
                .snapshot("example.extension")
                .unwrap()
                .revision,
            0
        );
    }

    #[test]
    fn cancellation_after_persistence_starts_does_not_cancel_authority_commit() {
        use crate::extension_runtime::RuntimeHostCallHandler;

        let mut app = app();
        install_staged_package(&mut app, 14);
        let directory = std::env::temp_dir().join(format!(
            "vqro-host-state-cancel-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        app.extension_state_path = Some(directory.join("state.json"));
        let canceled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (handler, _) = staged_handler(
            &mut app,
            14,
            &[crate::extension_host_state::HOST_STATE_WRITE_CAPABILITY],
            canceled.clone(),
        );
        let call = host_call(
            14,
            crate::extension_host_state::HOST_STATE_WRITE_CAPABILITY,
            crate::extension_host_state::HOST_STATE_TRANSACT_METHOD,
            "example.extension",
            serde_json::json!({
                "contract": "host.state.v1",
                "expected_store_generation": app.extension_state.store_generation,
                "expected_revision": 0,
                "idempotency_key": "cancel-1",
                "operations": [{"op": "set", "key": "committed", "value": true}]
            }),
        );
        let join = std::thread::spawn(move || handler.handle_host_call(call));
        let request_event = app.event_rx.blocking_recv().unwrap();
        app.handle_internal_event(request_event);
        canceled.store(true, std::sync::atomic::Ordering::Release);
        assert_eq!(
            join.join().unwrap().error.unwrap().code,
            "host_call_canceled"
        );
        let persisted_event = app.event_rx.blocking_recv().unwrap();
        app.handle_internal_event(persisted_event);
        assert_eq!(
            app.extension_state
                .snapshot("example.extension")
                .unwrap()
                .values["committed"],
            true
        );
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn staged_transaction_bounds_and_method_capabilities_fail_closed() {
        use crate::extension_runtime::RuntimeHostCallHandler;

        let mut app = app();
        install_staged_package(&mut app, 15);
        let (handler, authority) = staged_handler(
            &mut app,
            15,
            &[crate::extension_host_state::HOST_STATE_READ_CAPABILITY],
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        );
        let denied = handler.handle_host_call(host_call(
            15,
            crate::extension_host_state::HOST_STATE_READ_CAPABILITY,
            crate::extension_host_state::HOST_STATE_TRANSACT_METHOD,
            "example.extension",
            serde_json::json!({
                "contract": "host.state.v1",
                "expected_store_generation": app.extension_state.store_generation,
                "expected_revision": 0,
                "idempotency_key": "denied",
                "operations": []
            }),
        ));
        assert_eq!(denied.error.unwrap().code, "capability_denied");
        authority.revoke();
        let stale = handler.handle_host_call(host_call(
            15,
            crate::extension_host_state::HOST_STATE_READ_CAPABILITY,
            crate::extension_host_state::HOST_STATE_SNAPSHOT_METHOD,
            "example.extension",
            serde_json::json!({"contract": "host.state.v1"}),
        ));
        assert_eq!(stale.error.unwrap().code, "stale_host_call_lease");

        let (handler, _) = staged_handler(
            &mut app,
            15,
            &[crate::extension_host_state::HOST_STATE_WRITE_CAPABILITY],
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        );
        let operations = (0..=crate::extension_state::MAX_STATE_OPERATIONS)
            .map(|index| serde_json::json!({"op": "set", "key": format!("k{index}"), "value": index}))
            .collect::<Vec<_>>();
        let call = host_call(
            15,
            crate::extension_host_state::HOST_STATE_WRITE_CAPABILITY,
            crate::extension_host_state::HOST_STATE_TRANSACT_METHOD,
            "example.extension",
            serde_json::json!({
                "contract": "host.state.v1",
                "expected_store_generation": app.extension_state.store_generation,
                "expected_revision": 0,
                "idempotency_key": "too-many",
                "operations": operations
            }),
        );
        let join = std::thread::spawn(move || handler.handle_host_call(call));
        let event = app.event_rx.blocking_recv().unwrap();
        app.handle_internal_event(event);
        assert_eq!(join.join().unwrap().error.unwrap().code, "resource_limit");
    }

    #[test]
    fn persistence_faults_distinguish_safe_failure_from_uncertain_replacement() {
        let mut safe_app = app();
        let mut safe_candidate = safe_app.extension_state.clone();
        let safe_commit = safe_candidate
            .transact(&transaction(safe_candidate.store_generation))
            .unwrap();
        let (safe_tx, safe_rx) = std::sync::mpsc::sync_channel(1);
        safe_app.extension_state_transaction_pending = true;
        safe_app.handle_extension_state_persisted(super::ExtensionStatePersistResult {
            candidate: safe_candidate,
            commit: safe_commit,
            persisted: Err(crate::persist::extension_state::ExtensionStateSaveError {
                source: std::io::Error::other("injected write failure"),
                replaced: false,
            }),
            target: super::ExtensionStateResponseTarget::HostState {
                respond_to: safe_tx,
            },
        });
        assert_eq!(
            safe_rx.recv().unwrap().unwrap_err().code,
            "state_persist_failed"
        );
        assert!(!safe_app.extension_state_persistence_blocked);
        assert_eq!(
            safe_app
                .extension_state
                .snapshot("example.extension")
                .unwrap()
                .revision,
            0
        );

        let mut app = app();
        install_staged_package(&mut app, 13);
        let mut candidate = app.extension_state.clone();
        let commit = candidate
            .transact(&transaction(candidate.store_generation))
            .unwrap();
        let (respond_to, response) = std::sync::mpsc::sync_channel(1);
        app.extension_state_transaction_pending = true;
        app.handle_extension_state_persisted(super::ExtensionStatePersistResult {
            candidate,
            commit,
            persisted: Err(crate::persist::extension_state::ExtensionStateSaveError {
                source: std::io::Error::other("injected directory sync failure"),
                replaced: true,
            }),
            target: super::ExtensionStateResponseTarget::HostState { respond_to },
        });
        assert_eq!(
            response.recv().unwrap().unwrap_err().code,
            "state_persist_uncertain"
        );
        assert!(app.extension_state_persistence_blocked);
        assert!(app
            .stage_host_state_authorization(
                &crate::extension_host_state::HostStateProviderBinding::test_new(
                    "example.package",
                    "state",
                    "example.extension",
                    "a".repeat(64),
                    "1.0.0",
                ),
                5,
                7,
                13,
            )
            .is_err());
    }

    #[test]
    fn deferred_transaction_persists_off_actor_before_publication() {
        let mut app = app();
        let directory = std::env::temp_dir().join(format!(
            "vqro-extension-state-api-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("extension-state-v1.json");
        app.extension_state_path = Some(path.clone());
        let generation = app.extension_state.store_generation;
        let (respond_to, response) = std::sync::mpsc::channel();

        assert!(!app.handle_deferred_extension_state_transact(
            "deferred".into(),
            transaction(generation),
            respond_to,
        ));
        assert!(app.extension_state_transaction_pending);
        assert_eq!(
            app.extension_state
                .snapshot("example.extension")
                .unwrap()
                .revision,
            0
        );
        let event = app.event_rx.blocking_recv().unwrap();
        app.handle_internal_event(event);
        let response: SuccessResponse = serde_json::from_str(&response.recv().unwrap()).unwrap();
        assert!(matches!(
            response.result,
            ResponseResult::ExtensionStateCommitted { .. }
        ));
        assert_eq!(
            app.extension_state
                .snapshot("example.extension")
                .unwrap()
                .revision,
            1
        );
        assert!(matches!(
            crate::persist::extension_state::load(&path),
            crate::persist::extension_state::ExtensionStateLoad::Loaded(_)
        ));
        std::fs::remove_dir_all(directory).unwrap();
    }
}
