use super::*;
use crate::api::schema::{ExtensionServiceCapability, HostDocumentSourceKind};

fn fixture() -> (HeadlessServer, PrepareParams) {
    let mut server = super::super::tests::test_headless_server();
    server.app.state = crate::app::AppState::test_with_adversarial_identity_state();
    server.app.state.assert_invariants_for_test();
    let pane = server
        .app
        .state
        .workspaces
        .iter()
        .enumerate()
        .find_map(|(index, workspace)| {
            workspace.tabs.iter().find_map(|tab| {
                tab.panes
                    .keys()
                    .find_map(|pane| server.app.public_pane_id(index, *pane))
            })
        })
        .expect("pure pane");
    let selection = PrepareParams {
        namespace: "example.docs".into(),
        scope_id: "session".into(),
        pane_id: pane.clone(),
        generation: 7,
        revision: 8,
        item_id: "row".into(),
        option: Some("go".into()),
        operation_id: "original-op".into(),
    };
    let document: crate::extension_contract::HostDocumentV1 = serde_json::from_value(json!({"contract":crate::extension_contract::HOST_VIEW_CONTRACT_V1,"namespace":"example.docs","generation":7,"revision":8,"surface":"pane","target":{"kind":"session"},"items":[{"id":"root","role":"section","label":"Work","view":{"pane_id":pane,"view_id":"work"}},{"id":"row","parent_id":"root","role":"section","label":"Full question","view":{"kind":"row","affordance":{"kind":"choice","label":"Answer","detail":"Full question","subject":{"package_owned":42},"options":[{"key":"go","label":"Go"},{"key":"wait","label":"Wait"}],"default":"wait","reversible":false}}}],"actions":[],"required_capabilities":[]})).unwrap();
    let mut cache = crate::host_document_cache::HostDocumentCache::default();
    cache
        .publish_for_namespace("example.docs", 7, 1, &Default::default(), document.clone())
        .unwrap();
    server
        .app
        .host_document_caches
        .insert(("example.docs".into(), "session".into()), cache);
    server.app.host_document_services =
        crate::extension_service::HostDocumentServiceCatalog::with_test_action_identity(
            "example.docs",
            7,
            8,
            "example.actions",
        );
    assert_eq!(
        server.app.host_document_services.source("example.docs"),
        Some(HostDocumentSourceKind::Relationships)
    );
    let (events, _) = tokio::sync::mpsc::channel(8);
    server.app.extension_services = crate::extension_service::manager_for_test_spec(
        crate::extension_service::ExtensionServiceSpec {
            id: "example.actions".into(),
            command: vec!["unused-private-test-provider".into()],
            cwd: std::env::temp_dir(),
            priority: 1,
            granted_capabilities: vec![ExtensionServiceCapability::ActionInvoke],
        },
        events,
    );
    let mut client = ClientConnection::new(
        (80, 24),
        Default::default(),
        0,
        protocol::RenderEncoding::TerminalAnsi,
        None,
    );
    client.shell_host_views_enabled = true;
    client.shell_host_documents_enabled = true;
    client.shell_host_documents = vec![crate::protocol::endpoint::HostDocumentProjectionEntryV1 {
        provider_priority: 1,
        document,
        confirmation_tokens: Default::default(),
    }];
    server.clients.insert(1, client);
    (server, selection)
}
fn origin(server: &HeadlessServer, id: u64) -> api::ApiOrigin {
    api::ApiOrigin::NativeEndpoint {
        client_id: id,
        boot_id: server.client_shell_boot_id.clone(),
    }
}

#[tokio::test]
async fn session_view_known_provider_reply_is_preserved_when_later_audit_fails() {
    let (server, selection) = fixture();
    let binding = server.view_action_binding(1, &selection).unwrap();
    let caller = crate::action_invocation::InvocationCaller::Unknown;
    let provider = json!({"result":{"value":{"ok":true,"receipt":"original"}}});
    let response: Value = serde_json::from_str(&submitted_response(
        "request",
        &binding,
        &caller,
        provider.clone(),
        Some("disk full".into()),
    ))
    .unwrap();
    assert_eq!(response["result"]["provider_response"], provider);
    assert_eq!(response["result"]["audit_warning"], "disk full");
    assert_eq!(response["result"]["operation_id"], "original-op");
    assert!(response.get("error").is_none());
}

#[tokio::test]
async fn session_view_binding_uses_exact_publisher_choice_and_opaque_subject() {
    let (server, mut selection) = fixture();
    let binding = server.view_action_binding(1, &selection).unwrap();
    assert_eq!(binding.service_id, "example.actions");
    assert_eq!(binding.affordance.subject(), &json!({"package_owned":42}));
    selection.option = None;
    assert!(server.view_action_binding(1, &selection).is_err());
    selection.option = Some("unlisted".into());
    assert!(server.view_action_binding(1, &selection).is_err());
    selection.scope_id = "w1:t1".into();
    assert!(server.view_action_binding(1, &selection).is_err());
}
#[tokio::test]
async fn session_view_binding_rejects_stale_client_document_and_closed_source() {
    let (mut server, mut selection) = fixture();
    selection.revision += 1;
    assert!(server.view_action_binding(1, &selection).is_err());
    selection.revision -= 1;
    server
        .clients
        .get_mut(&1)
        .unwrap()
        .shell_host_documents
        .clear();
    assert!(server.view_action_binding(1, &selection).is_err());
    let (mut server, selection) = fixture();
    server.app.state.workspaces.clear();
    assert!(server.view_action_binding(1, &selection).is_err());
}
#[tokio::test]
async fn session_view_binding_rejects_missing_grant_or_replaced_publisher() {
    let (mut server, selection) = fixture();
    let (events, _) = tokio::sync::mpsc::channel(8);
    server.app.extension_services =
        crate::extension_service::manager_for_test_specs(Vec::new(), events);
    assert!(server.view_action_binding(1, &selection).is_err());
    let (mut server, selection) = fixture();
    server.app.host_document_services =
        crate::extension_service::HostDocumentServiceCatalog::with_test_action_identity(
            "example.docs",
            9,
            8,
            "example.actions",
        );
    assert!(server.view_action_binding(1, &selection).is_err());
}
#[tokio::test]
async fn session_view_origin_does_not_borrow_foreground_or_payload_identity() {
    let (mut server, _) = fixture();
    server.foreground_client_id = Some(999);
    assert_eq!(server.view_action_client(&origin(&server, 1)).unwrap(), 1);
    assert!(server
        .view_action_client(&api::ApiOrigin::LocalSocket)
        .is_err());
    assert!(server
        .view_action_client(&api::ApiOrigin::NativeEndpoint {
            client_id: 1,
            boot_id: "other-boot".into()
        })
        .is_err());
    assert!(serde_json::from_value::<PrepareParams>(json!({"namespace":"example.docs","scope_id":"session","pane_id":"w1:p1","generation":7,"revision":8,"item_id":"row","operation_id":"op","client_id":1})).is_err());
}
#[tokio::test]
async fn session_view_cancel_consumes_only_this_connections_exact_token_without_proof_or_write() {
    let (mut server, selection) = fixture();
    let binding = server.view_action_binding(1, &selection).unwrap();
    let identity = crate::client_invocation::ClientIdentity {
        boot_id: "boot".into(),
        pid: 10,
        birth: 20,
        tty: crate::client_invocation::ClientTtyDescriptor {
            kind: "unix".into(),
            device: "1:2:3".into(),
        },
    };
    let prepared = Prepared::new(binding, identity).unwrap();
    let token = prepared.token.clone();
    server.clients.get_mut(&1).unwrap().view_actions.prepared = Some(prepared);
    let (tx, rx) = std::sync::mpsc::channel();
    server
        .start_view_action_inner(api::ApiRequestMessage {
            origin: origin(&server, 1),
            request: api::schema::Request {
                id: "cancel".into(),
                method: api::schema::Method::HostViewActionSubmit(
                    crate::view_action::SubmitParams {
                        token: token.clone(),
                        confirmation_token: None,
                        cancel: true,
                    },
                ),
            },
            respond_to: tx,
            response_write_complete: None,
        })
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&rx.recv().unwrap()).unwrap()["result"]["canceled"],
        true
    );
    assert!(server.clients[&1].view_actions.prepared.is_none());
    let (tx, _) = std::sync::mpsc::channel();
    assert!(server
        .start_view_action_inner(api::ApiRequestMessage {
            origin: origin(&server, 1),
            request: api::schema::Request {
                id: "replay".into(),
                method: api::schema::Method::HostViewActionSubmit(
                    crate::view_action::SubmitParams {
                        token,
                        confirmation_token: None,
                        cancel: false
                    }
                )
            },
            respond_to: tx,
            response_write_complete: None
        })
        .is_err());
}
