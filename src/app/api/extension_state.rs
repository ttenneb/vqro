use super::{responses, App};
use crate::api::schema::{ExtensionStateEventsParams, ResponseResult};

#[derive(Debug)]
pub(crate) struct ExtensionStatePersistResult {
    pub(crate) request_id: String,
    pub(crate) candidate: crate::extension_state::DurableExtensionStateV1,
    pub(crate) commit: crate::extension_state::ExtensionStateCommitV1,
    pub(crate) persisted: Result<(), crate::persist::extension_state::ExtensionStateSaveError>,
    pub(crate) respond_to: std::sync::mpsc::Sender<String>,
}

impl App {
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
        if let Some(path) = self.extension_state_path.as_deref() {
            if let Err(error) = crate::persist::extension_state::save_atomically(path, &candidate) {
                // Only a post-replacement durability failure creates uncertain authority.
                self.extension_state_persistence_blocked = error.replaced;
                let detail = if error.replaced {
                    format!(
                        "{} (the file was replaced before durability failed)",
                        error.source
                    )
                } else {
                    error.source.to_string()
                };
                return responses::encode_error(
                    request_id,
                    "extension_state_persist_failed",
                    &detail,
                );
            }
        }
        self.extension_state = candidate;
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
            self.extension_state = candidate;
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
        let spawn_request_id = request_id.clone();
        let spawn_respond_to = respond_to.clone();
        let spawn = std::thread::Builder::new()
            .name("vqro-extension-state".into())
            .spawn(move || {
                let persisted = crate::persist::extension_state::save_atomically(&path, &candidate);
                permit.send(crate::events::AppEvent::ExtensionStatePersisted(Box::new(
                    ExtensionStatePersistResult {
                        request_id,
                        candidate,
                        commit,
                        persisted,
                        respond_to,
                    },
                )));
            });
        match spawn {
            Ok(_) => self.extension_state_transaction_pending = true,
            Err(error) => {
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
        self.extension_state_transaction_pending = false;
        let response = match result.persisted {
            Ok(()) => {
                self.extension_state = result.candidate;
                responses::encode_success(
                    result.request_id,
                    ResponseResult::ExtensionStateCommitted {
                        commit: result.commit,
                    },
                )
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
                responses::encode_error(
                    result.request_id,
                    "extension_state_persist_failed",
                    &detail,
                )
            }
        };
        let _ = result.respond_to.send(response);
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
