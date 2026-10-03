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
        document_key: ("independent.attention".into(), 7, 2),
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
    state.open_attention(hit.clone(), &mut outcome);
    assert!(outcome.actions.iter().any(|action| matches!(action, ClientShellAction::Endpoint {request,..} if matches!(&request.method, crate::api::schema::Method::PluginPaneOpen(params) if params.env.get("VQRO_ATTENTION_SCOPE").is_some_and(|value| value.contains("tab_1"))))));
    state.endpoints[0]
        .attention
        .as_mut()
        .expect("cached nav")
        .document_key
        .1 += 1;
    let mut stale = ClientShellInput::default();
    state.open_attention(hit, &mut stale);
    assert!(
        stale.actions.is_empty(),
        "replacement before redraw must reject old badge"
    );
}

#[test]
fn fresh_empty_tasks_document_is_healthy_zero_not_unavailable() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_endpoint_status(&ClientEndpointId::Local, ClientEndpointStatus::Online);
    state.set_endpoint_snapshot_for_generation(&ClientEndpointId::Local, 1, Box::new(snapshot()));
    state.set_pane_surface(surface());
    let projection = serde_json::from_value(serde_json::json!({
        "contract":"host.documents.v1", "boot_id":"boot-1", "revision":1,
        "documents":[{"provider_priority":1,"document":{
            "contract":"host.document.v1", "namespace":"vqro.tasks", "generation":7,"revision":2,
            "surface":"navigation","target":{"kind":"session"},
            "items":[
                {"id":"context:attention.view:vqro.tasks:tree", "role":"section","label":"Tasks"},
                {"id":"context:attention.measure:0", "role":"section","label":"waiting on you"},
                {"id":"context:attention.measure:1", "role":"section","label":"blocked"},
                {"id":"context:attention.measure:2", "role":"section","label":"unavailable"}
            ],
            "actions":[],"required_capabilities":[]
        }}]
    }))
    .expect("empty Tasks projection");
    state.set_endpoint_host_documents_for_generation(
        &ClientEndpointId::Local,
        1,
        Box::new(projection),
    );
    let frame = state.compose(106, 30).expect("empty Tasks frame");
    let text = frame
        .cells
        .iter()
        .map(|cell| cell.symbol.as_str())
        .collect::<String>();
    assert!(text.contains("No durable attention"), "{text}");
    assert!(!text.contains("Tasks unavailable"), "{text}");
    assert!(!state.show_agents);
}

#[test]
fn tasks_default_is_unavailable_not_zero_and_agents_toggle_keeps_recovery() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    let frame = state.compose(106, 30).expect("frame");
    let text = frame
        .cells
        .iter()
        .map(|cell| cell.symbol.as_str())
        .collect::<String>();
    assert!(text.contains("Tasks unavailable"));
    assert!(!text.contains("No durable attention"));
    assert!(!state.show_agents);
    let toggle = state.hits.attention_toggle;
    state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: toggle.x,
        row: toggle.y,
        modifiers: KeyModifiers::empty(),
    })]);
    assert!(state.show_agents);
    let frame = state.compose(106, 30).expect("agents frame");
    let text = frame
        .cells
        .iter()
        .map(|cell| cell.symbol.as_str())
        .collect::<String>();
    assert!(text.contains("agents"));
    assert!(text.contains("[tasks]"));
    assert!(state.snapshot.is_some());
}
