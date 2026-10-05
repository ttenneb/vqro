use super::*;

#[test]
fn a_badge_click_opens_the_scopes_panels_and_never_needs_a_host_method() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_endpoint_status(&ClientEndpointId::Local, ClientEndpointStatus::Online);
    state.set_endpoint_snapshot_for_generation(&ClientEndpointId::Local, 1, Box::new(snapshot()));
    // Deliberately no host methods: opening a panel is the client's own state.
    state.set_endpoint_methods(Some(Vec::new()));
    let projection = serde_json::from_value(serde_json::json!({
        "contract":"host.documents.v1", "boot_id":"boot-1", "revision":1,
        "documents":[{"provider_priority":1,"document":{
            "contract":"host.view.v1", "namespace":"independent.attention", "generation":7,"revision":2,
            "surface":"navigation","target":{"kind":"session"},
            "items":[
                {"id":"context:attention.view:independent:read", "role":"section","label":"Work"},
                {"id":"panel:pane_1:tasks", "role":"section","label":"Tasks","detail":"0/1 done",
                 "view":{"pane_id":"pane_1","view_id":"tasks"}},
                {"id":"panel:pane_1:task:1", "parent_id":"panel:pane_1:tasks","role":"section",
                 "label":"A row","view":{"state":"open"}}
            ],
            "actions":[],"required_capabilities":[]
        }}]
    }))
    .expect("projection");
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
    // No host method, no action: the click opened the panel in this client.
    assert!(outcome.actions.is_empty());
    assert_eq!(
        state
            .pane_panel_open
            .get("pane_1")
            .map(|views| views.contains("tasks")),
        Some(true)
    );
    assert_eq!(state.endpoints[0].status, ClientEndpointStatus::Online);
    assert!(state.snapshot.is_some());
    // A stale document key is rejected before anything opens.
    let mut fresh = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    fresh.set_endpoint_status(&ClientEndpointId::Local, ClientEndpointStatus::Online);
    fresh.set_endpoint_snapshot_for_generation(&ClientEndpointId::Local, 1, Box::new(snapshot()));
    let mut stale = ClientShellInput::default();
    fresh.open_attention(hit, &mut stale);
    assert!(stale.actions.is_empty());
    assert!(fresh.pane_panel_open.is_empty());
}

#[test]
fn a_badge_with_no_views_notices_instead_of_launching_a_tab() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_endpoint_status(&ClientEndpointId::Local, ClientEndpointStatus::Online);
    state.set_endpoint_snapshot_for_generation(&ClientEndpointId::Local, 1, Box::new(snapshot()));
    state.set_endpoint_methods(Some(vec!["plugin.pane.open".into()]));
    let projection = serde_json::from_value(serde_json::json!({
        "contract":"host.documents.v1", "boot_id":"boot-1", "revision":1,
        "documents":[{"provider_priority":1,"document":{
            "contract":"host.document.v1", "namespace":"independent.attention", "generation":7,"revision":2,
            "surface":"navigation","target":{"kind":"session"},
            "items":[{"id":"context:attention.view:independent:read", "role":"section","label":"Work"}],
            "actions":[],"required_capabilities":[]
        }}]
    }))
    .expect("projection");
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
    state.open_attention(hit, &mut outcome);
    // The extension's own tab view is retired: nothing is launched, the click
    // reports that this scope has no panel to open.
    assert!(outcome.actions.is_empty());
    assert_eq!(state.endpoints[0].status, ClientEndpointStatus::Online);
    // The client says why nothing opened, and stays connected.
    assert!(state.endpoint_error.is_some());
    assert!(state.pane_panel_open.is_empty());
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

/// A sidebar attention (Tasks) list with one row the extension gave a terminal link
/// for, and one whose agent has no live pane.
fn attention_rows_state() -> ClientShellState {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_endpoint_status(&ClientEndpointId::Local, ClientEndpointStatus::Online);
    state.set_endpoint_snapshot_for_generation(&ClientEndpointId::Local, 1, Box::new(snapshot()));
    let projection = serde_json::from_value(serde_json::json!({
        "contract":"host.documents.v1", "boot_id":"boot-1", "revision":1,
        "documents":[{"provider_priority":1,"document":{
            "contract":"host.view.v1", "namespace":"independent.attention", "generation":7,"revision":2,
            "surface":"navigation","target":{"kind":"session"},
            "items":[
                {"id":"context:attention.view:independent:read","role":"section","label":"Work"},
                {"id":"context:attention.row:task-1","role":"section","label":"A live task",
                 "detail":"pi @1111 · start",
                 "terminal":{"terminal_id":"term_1","placement":{"pane_id":"pane_1",
                   "workspace_id":"ws_1","tab_id":"tab_1"}}},
                {"id":"context:attention.row:task-2","role":"section","label":"Yesterday's smoke test",
                 "detail":"pi @2222 · accept"}
            ],
            "actions":[],"required_capabilities":[]
        }}]
    }))
    .expect("projection");
    state.set_endpoint_host_documents_for_generation(
        &ClientEndpointId::Local,
        1,
        Box::new(projection),
    );
    state
}

#[test]
fn a_sidebar_task_entry_jumps_to_its_pane_and_a_stale_one_says_so() {
    let mut state = attention_rows_state();
    let rows = state
        .endpoints
        .iter()
        .find(|endpoint| endpoint.endpoint_id == ClientEndpointId::Local)
        .and_then(|endpoint| endpoint.attention.as_ref())
        .map(|nav| nav.rows.clone())
        .expect("rows");
    assert_eq!(rows.len(), 2, "both entries are listed");
    assert_eq!(rows[0].pane_id.as_deref(), Some("pane_1"));
    assert!(
        rows[1].pane_id.is_none(),
        "an entry whose agent has no live pane has nowhere to jump"
    );

    // The stale entry says so in the rendered row.
    let frame = state.compose(106, 30).expect("frame");
    let text = frame
        .cells
        .iter()
        .map(|cell| cell.symbol.as_str())
        .collect::<String>();
    assert!(
        text.contains("not running"),
        "the stale entry must say it is not running: {text}"
    );
    assert!(
        !text.contains("@2222"),
        "and it names the agent, not a raw id: {text}"
    );

    // Clicking the live row focuses its pane.
    let hit = state
        .hits
        .attention
        .iter()
        .find(|hit| hit.kind == "attention_row" && hit.ids == vec!["pane_1".to_string()])
        .cloned()
        .expect("the live row is clickable");
    let mut outcome = ClientShellInput::default();
    state.open_attention(hit, &mut outcome);
    let focused: Vec<&str> = outcome
        .actions
        .iter()
        .filter_map(|action| match action {
            ClientShellAction::Endpoint { request, .. } => match &request.method {
                crate::api::schema::Method::PaneFocus(target) => Some(target.pane_id.as_str()),
                _ => None,
            },
            _ => None,
        })
        .collect();
    assert_eq!(focused, vec!["pane_1"], "the entry jumps to its pane");

    // And clicking the stale row reports why nothing happened.
    let stale = state
        .hits
        .attention
        .iter()
        .find(|hit| hit.kind == "attention_row" && hit.ids.is_empty())
        .cloned()
        .expect("the stale row is clickable too");
    let mut outcome = ClientShellInput::default();
    state.open_attention(stale, &mut outcome);
    assert!(
        outcome.actions.is_empty(),
        "a stale entry must not push a focus for a pane that is gone"
    );
    assert!(
        state.endpoint_error.is_some(),
        "a stale entry tells the user instead of doing nothing"
    );
}
