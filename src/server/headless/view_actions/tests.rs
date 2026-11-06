use super::*;
use crate::api::schema::{ExtensionServiceCapability, HostDocumentSourceKind};

fn fixture() -> (HeadlessServer, PrepareParams) {
    let mut server = super::super::tests::test_headless_server();
    server.app.state = crate::app::AppState::test_with_adversarial_identity_state();
    server.app.state.assert_invariants_for_test();
    let (workspace_index, source) = server
        .app
        .state
        .workspaces
        .iter()
        .enumerate()
        .flat_map(|(index, workspace)| workspace.tabs.iter().map(move |tab| (index, tab)))
        .max_by_key(|(_, tab)| tab.panes.len())
        .and_then(|(index, tab)| tab.panes.keys().next().copied().map(|pane| (index, pane)))
        .expect("pure source");
    let pane = server.app.public_pane_id(workspace_index, source).unwrap();
    let (tab_index, consumer) = server.app.state.workspaces[workspace_index]
        .split_pane_with_view(
            source,
            "example.docs",
            "work",
            ratatui::layout::Direction::Horizontal,
            0.5,
            false,
        )
        .unwrap();
    let consumer_pane_id = server
        .app
        .public_pane_id(workspace_index, consumer)
        .unwrap();
    let terminal_scope = server.app.state.workspaces[workspace_index].tabs[tab_index]
        .layout
        .view_scope(consumer)
        .into_iter()
        .map(|pane| server.app.public_pane_id(workspace_index, pane).unwrap())
        .collect::<Vec<_>>();
    let tab_id = server
        .app
        .public_tab_id(workspace_index, tab_index)
        .unwrap();
    let workspace_id = server.app.public_workspace_id(workspace_index);
    server.app.state.assert_invariants_for_test();
    let selection = PrepareParams {
        namespace: "example.docs".into(),
        scope_id: "session".into(),
        pane_id: pane.clone(),
        consumer_pane_id: consumer_pane_id.clone(),
        terminal_scope: terminal_scope.clone(),
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
    client.shell_location = Some(crate::server::clients::ClientShellLocation {
        focused_workspace_id: Some(workspace_id.clone()),
        active_tab_ids: HashMap::from([(workspace_id, tab_id)]),
    });
    client.render_state = crate::server::render_stream::ClientRenderState::Semantic {
        last_surface: None,
        last_views: vec![crate::protocol::PaneSurfaceViewEntryV2 {
            pane_id: consumer_pane_id,
            extension: "example.docs".into(),
            view: "work".into(),
            scope: terminal_scope,
        }],
        surface_revision: 1,
    };
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

fn publish_test_document(
    server: &mut HeadlessServer,
    document: crate::extension_contract::HostDocumentV1,
) {
    let namespace = document.namespace.clone();
    let generation = document.generation;
    let revision = document.revision;
    server
        .app
        .host_document_caches
        .get_mut(&(namespace.clone(), "session".into()))
        .unwrap()
        .publish_for_namespace(
            &namespace,
            generation,
            1,
            &Default::default(),
            document.clone(),
        )
        .unwrap();
    server
        .app
        .host_document_services
        .set_desired_revision_for_test(&namespace, "session".into(), generation, revision);
    server.clients.get_mut(&1).unwrap().shell_host_documents[0].document = document;
}

#[tokio::test]
async fn admission_scope_rejects_other_tab_and_workspace_roots_in_the_same_session_document() {
    let (mut server, selection) = fixture();
    server
        .app
        .state
        .workspaces
        .push(crate::workspace::Workspace::test_new("other-workspace"));
    server.app.state.ensure_test_terminals();
    server.app.state.assert_invariants_for_test();
    let (wi, consumer) = server
        .app
        .parse_pane_id(&selection.consumer_pane_id)
        .unwrap();
    let ti = server.app.state.workspaces[wi]
        .find_tab_index_for_pane(consumer)
        .unwrap();
    let other_tab = server.app.state.workspaces[wi]
        .tabs
        .iter()
        .enumerate()
        .find(|(index, _)| *index != ti)
        .and_then(|(_, tab)| tab.panes.keys().next())
        .and_then(|pane| server.app.public_pane_id(wi, *pane))
        .unwrap();
    let other_workspace = server.app.state.workspaces[1].tabs[0]
        .panes
        .keys()
        .next()
        .and_then(|pane| server.app.public_pane_id(1, *pane))
        .unwrap();
    for (index, pane) in [other_tab, other_workspace].into_iter().enumerate() {
        let mut document = server.app.host_document_caches
            [&("example.docs".into(), "session".into())]
            .current()
            .unwrap()
            .clone();
        document.revision += 1;
        let mut root = document.items[0].clone();
        root.id = format!("other-root-{index}");
        root.view.as_mut().unwrap().pane_id = Some(pane.clone());
        let mut row = document.items[1].clone();
        row.id = format!("other-row-{index}");
        row.parent_id = Some(root.id.clone());
        document.items.extend([root, row.clone()]);
        let mut attack = selection.clone();
        attack.revision = document.revision;
        attack.item_id = row.id;
        attack.pane_id = pane.clone();
        attack.terminal_scope = vec![pane];
        publish_test_document(&mut server, document);
        assert!(
            server.view_action_binding(1, &attack).is_err(),
            "global delivery is not scope authority"
        );
    }
}

#[tokio::test]
async fn admission_scope_rejects_a_background_consumer_and_wrong_view_identity() {
    let (mut server, selection) = fixture();
    let (wi, consumer) = server
        .app
        .parse_pane_id(&selection.consumer_pane_id)
        .unwrap();
    let ti = server.app.state.workspaces[wi]
        .find_tab_index_for_pane(consumer)
        .unwrap();
    let other = server.app.state.workspaces[wi]
        .tabs
        .iter()
        .enumerate()
        .find(|(index, _)| *index != ti)
        .map(|(index, _)| server.app.public_tab_id(wi, index).unwrap())
        .unwrap();
    let workspace = server.app.public_workspace_id(wi);
    server
        .clients
        .get_mut(&1)
        .unwrap()
        .shell_location
        .as_mut()
        .unwrap()
        .active_tab_ids
        .insert(workspace, other);
    assert!(
        server.view_action_binding(1, &selection).is_err(),
        "cannot borrow global/previous surface location"
    );
    let (mut server, selection) = fixture();
    if let crate::server::render_stream::ClientRenderState::Semantic { last_views, .. } =
        &mut server.clients.get_mut(&1).unwrap().render_state
    {
        last_views[0].view = "different-view".into();
    }
    assert!(server.view_action_binding(1, &selection).is_err());
}

#[tokio::test]
async fn admission_scope_rejects_a_rescoped_consumer_with_unchanged_document_and_identity() {
    let (mut server, selection) = fixture();
    let (wi, consumer) = server
        .app
        .parse_pane_id(&selection.consumer_pane_id)
        .unwrap();
    let ti = server.app.state.workspaces[wi]
        .find_tab_index_for_pane(consumer)
        .unwrap();
    let source = server.app.parse_pane_id(&selection.pane_id).unwrap().1;
    let terminals = server.app.state.workspaces[wi].tabs[ti]
        .layout
        .pane_ids()
        .into_iter()
        .filter(|pane| {
            server.app.state.workspaces[wi].tabs[ti]
                .layout
                .view_at(*pane)
                .is_none()
        })
        .collect::<Vec<_>>();
    let other = *terminals.iter().find(|pane| **pane != source).unwrap();
    let mut remainder = terminals
        .into_iter()
        .filter(|pane| *pane != other)
        .map(crate::layout::TypedNode::Leaf);
    let first = remainder.next().unwrap();
    let retained = remainder.fold(first, |first, second| crate::layout::TypedNode::Split {
        direction: ratatui::layout::Direction::Horizontal,
        ratio: 0.5,
        first: Box::new(first),
        second: Box::new(second),
    });
    let moved = crate::layout::TypedNode::Split {
        direction: ratatui::layout::Direction::Horizontal,
        ratio: 0.5,
        first: Box::new(crate::layout::TypedNode::Leaf(other)),
        second: Box::new(crate::layout::TypedNode::View {
            pane: consumer,
            extension: "example.docs".into(),
            view: "work".into(),
        }),
    };
    server.app.state.workspaces[wi].tabs[ti]
        .layout
        .replace_typed_root_for_test(crate::layout::TypedNode::Split {
            direction: ratatui::layout::Direction::Vertical,
            ratio: 0.5,
            first: Box::new(retained),
            second: Box::new(moved),
        });
    server.app.state.assert_invariants_for_test();
    assert!(server.view_action_binding(1, &selection).is_err());
}

#[tokio::test]
async fn admission_scope_keeps_hidden_members_and_rejects_a_selected_only_scope() {
    let (mut server, mut selection) = fixture();
    let (wi, consumer) = server
        .app
        .parse_pane_id(&selection.consumer_pane_id)
        .unwrap();
    let ti = server.app.state.workspaces[wi]
        .find_tab_index_for_pane(consumer)
        .unwrap();
    let terminals = server.app.state.workspaces[wi].tabs[ti]
        .layout
        .pane_ids()
        .into_iter()
        .filter(|pane| {
            server.app.state.workspaces[wi].tabs[ti]
                .layout
                .view_at(*pane)
                .is_none()
        })
        .collect::<Vec<_>>();
    assert!(terminals.len() > 1);
    let scope = terminals
        .iter()
        .map(|pane| server.app.public_pane_id(wi, *pane).unwrap())
        .collect::<Vec<_>>();
    let hidden = crate::layout::TypedNode::Stack {
        id: crate::terminal_container::TerminalContainerId::alloc().unwrap(),
        selected: 0,
        children: terminals
            .into_iter()
            .map(crate::layout::TypedNode::Leaf)
            .collect(),
    };
    server.app.state.workspaces[wi].tabs[ti]
        .layout
        .replace_typed_root_for_test(crate::layout::TypedNode::Split {
            direction: ratatui::layout::Direction::Horizontal,
            ratio: 0.5,
            first: Box::new(hidden),
            second: Box::new(crate::layout::TypedNode::View {
                pane: consumer,
                extension: "example.docs".into(),
                view: "work".into(),
            }),
        });
    if let crate::server::render_stream::ClientRenderState::Semantic { last_views, .. } =
        &mut server.clients.get_mut(&1).unwrap().render_state
    {
        last_views[0].scope = scope.clone();
    }
    server.app.state.assert_invariants_for_test();
    selection.terminal_scope = scope;
    assert!(server.view_action_binding(1, &selection).is_ok());
    selection.terminal_scope = vec![selection.pane_id.clone()];
    assert!(server.view_action_binding(1, &selection).is_err());
}

fn ready_sentinel(
    server: &mut HeadlessServer,
    selection: &PrepareParams,
    deadline: Instant,
) -> ProofReady {
    let binding = server.view_action_binding(1, selection).unwrap();
    let client = server.clients.get_mut(&1).unwrap();
    client.view_actions.busy = Some("proof-job".into());
    let (respond_to, _) = std::sync::mpsc::channel();
    ProofReady {
        client_id: 1,
        nonce: client.view_actions.nonce.clone(),
        job: "proof-job".into(),
        binding,
        previous: None,
        request_id: "proof-request".into(),
        respond_to,
        result: Err("proof sentinel: must not be considered".into()),
        deadline,
        slot: crate::extension_service::ExtensionWorkSlotGuard::acquire().unwrap(),
    }
}

#[tokio::test]
async fn admission_rejects_root_without_a_matching_placed_consumer() {
    let (mut server, selection) = fixture();
    let (workspace, pane) = server
        .app
        .parse_pane_id(&selection.consumer_pane_id)
        .unwrap();
    server.app.state.workspaces[workspace].close_pane(pane);
    assert!(
        server.view_action_binding(1, &selection).is_err(),
        "global SESSION root and terminal existence are not a placed consumer scope"
    );
}

#[tokio::test]
async fn admission_rejects_a_delayed_ready_before_reading_its_proof() {
    let (mut server, selection) = fixture();
    let ready = ready_sentinel(
        &mut server,
        &selection,
        Instant::now() - Duration::from_secs(1),
    );
    let message = server.finish_view_action_inner(ready).unwrap_err();
    assert!(message.contains("expired"), "{message}");
    assert!(server.clients[&1].view_actions.prepared.is_none());
}

#[tokio::test]
async fn admission_rejects_stop_intent_before_reading_its_proof() {
    let (mut server, selection) = fixture();
    let ready = ready_sentinel(
        &mut server,
        &selection,
        Instant::now() + Duration::from_secs(3),
    );
    server.should_quit.store(true, Ordering::Release);
    let message = server.finish_view_action_inner(ready).unwrap_err();
    assert!(message.contains("stopping"), "{message}");
    assert!(server.clients[&1].view_actions.prepared.is_none());
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
    let (mut server, selection) = fixture();
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
    let mut forged = serde_json::to_value(selection).unwrap();
    forged["client_id"] = json!(1);
    assert!(serde_json::from_value::<PrepareParams>(forged).is_err());
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
