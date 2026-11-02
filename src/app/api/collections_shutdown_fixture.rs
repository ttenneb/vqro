//! Test-only records at the already-admitted planning/publication boundary.
//! Package validation is covered separately; no external artifact or authority is installed here.
use super::*;

impl App {
    pub(crate) fn test_shutdown_read_authorization(
        &self,
    ) -> crate::extension_host_invocation::HostReadInvocationAuthorization {
        crate::extension_host_invocation::HostReadInvocationAuthorization {
            package_id: "vqro.collections".into(),
            service_id: "collections".into(),
            namespace: COLLECTIONS_NAMESPACE.into(),
            artifact_sha256: "a".repeat(64),
            artifact_version: "test".into(),
            catalog_generation: 1,
            package_generation: 1,
            service_generation: 1,
            artifact_generation: 1,
            provider_generation: 1,
            runtime_generation: 1,
            state: None,
            terminals: None,
            allowed_methods: Default::default(),
            authority: crate::extension_host_state::HostInvocationAuthority::new(1, 1, 1),
            terminal_observations: Default::default(),
        }
    }

    pub(crate) fn test_queue_shutdown_action(
        &mut self,
    ) -> (Arc<AtomicBool>, std::sync::mpsc::Receiver<String>) {
        assert!(!self.collections_mutations_blocked);
        let (respond_to, rx) = std::sync::mpsc::channel();
        let canceled = Arc::new(AtomicBool::new(false));
        let authorization = self.test_shutdown_read_authorization();
        let sequence = self.reserve_collections_action_sequence().unwrap();
        let pending = PendingCollectionsAction {
            request_id: "shutdown-action".into(),
            respond_to,
            invocation: CollectionsActionInvocationV1 {
                contract: COLLECTIONS_ACTION_INVOCATION_CONTRACT_V1.into(),
                ticket: format!("ticket:{sequence}"),
                idempotency_key: format!("shutdown-{sequence}"),
                document_revision: 1,
                actions_revision: 1,
                action_id: "archive".into(),
                expected_store_generation: self.extension_state.store_generation,
                expected_namespace_revision: 0,
                dependency: CollectionsActionDependencyV1::Container {
                    container_id: "container_0000000000000001".into(),
                },
                input: CollectionsActionInputV1::SetLabel {
                    container_id: "container_0000000000000001".into(),
                    label: None,
                },
            },
            observations: authorization.terminal_observations(),
            authorization,
            expected_state: self
                .extension_state
                .snapshot(COLLECTIONS_NAMESPACE)
                .unwrap()
                .into(),
            collection: crate::api::schema::CollectionInfo {
                collection_id: "collection_1".into(),
                workspace_id: self.state.workspaces[0].id.clone(),
                tab_id: self.state.workspaces[0].tabs[0].id.to_string(),
                label: None,
                focused: false,
                selected_pane_id: None,
                members: Vec::new(),
            },
            deadline: Instant::now() + Duration::from_secs(30),
            canceled: canceled.clone(),
        };
        self.collections_pending_actions.insert(77, pending);
        self.collections_mutations_blocked = true;
        (canceled, rx)
    }

    pub(crate) fn test_publish_shutdown_checkpoint(
        &mut self,
        paths: crate::persist::coordinator::PersistencePaths,
        published: std::sync::mpsc::Sender<()>,
        release: std::sync::mpsc::Receiver<()>,
    ) -> std::thread::JoinHandle<()> {
        assert!(self.collections_mutations_blocked);
        let pending = self.collections_pending_actions.remove(&77).unwrap();
        let state = self.extension_state.clone();
        let authority = self.collections_authority.clone();
        let session = self.capture_session_snapshot();
        let events = self.event_tx.clone();
        std::thread::spawn(move || {
            let result =
                crate::persist::coordinator::persist_checkpoint_paused_after_session_for_test(
                    &paths, &session, &state, published, release,
                )
                .map_err(|error| error.to_string());
            events
                .blocking_send(crate::events::AppEvent::CollectionsCheckpointPersisted(
                    Box::new(CollectionsCheckpointResult {
                        pending,
                        state,
                        authority,
                        result,
                    }),
                ))
                .unwrap();
        })
    }
}
