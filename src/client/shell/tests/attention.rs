use super::*;

#[test]
fn missing_attention_pane_method_disables_only_click() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_endpoint_status(&ClientEndpointId::Local, ClientEndpointStatus::Online);
    state.set_endpoint_snapshot_for_generation(&ClientEndpointId::Local, 1, Box::new(snapshot()));
    state.set_endpoint_methods(Some(vec!["pane.focus".into()]));
    let projection = serde_json::from_value(serde_json::json!({
        "contract":"host.documents.v1", "boot_id":"boot-1", "revision":1,
        "documents":[{"provider_priority":1,"document":{
            "contract":"host.document.v1", "namespace":"independent.attention", "generation":7,"revision":2,
            "surface":"navigation","target":{"kind":"session"},
            "items":[{"id":"context:attention.view:independent:read", "role":"section","label":"Work"}],
            "actions":[],"required_capabilities":[]
        }}]
    })).expect("projection");
    state.set_endpoint_host_documents_for_generation(
        &ClientEndpointId::Local,
        1,
        Box::new(projection),
    );
    let hit = AttentionHit {
        rect: Rect::new(0, 0, 5, 1),
        plugin: "independent".into(),
        entrypoint: "read".into(),
        kind: "tab".into(),
        ids: vec!["tab_1".into()],
    };
    let mut outcome = ClientShellInput::default();
    state.open_attention(hit.clone(), &mut outcome);
    assert!(outcome.actions.is_empty());
    assert_eq!(state.endpoints[0].status, ClientEndpointStatus::Online);
    assert!(state.snapshot.is_some());
    assert_eq!(
        state
            .visible_endpoint_notice
            .as_ref()
            .expect("feature-local notice")
            .key
            .code,
        "plugin.pane.open"
    );
    state.set_endpoint_methods(Some(vec!["pane.focus".into(), "plugin.pane.open".into()]));
    state.open_attention(hit, &mut outcome);
    assert!(outcome.actions.iter().any(|action| matches!(action, ClientShellAction::Endpoint {request,..} if matches!(&request.method, crate::api::schema::Method::PluginPaneOpen(params) if params.env.get("VQRO_ATTENTION_SCOPE").is_some_and(|value| value.contains("tab_1"))))));
}
