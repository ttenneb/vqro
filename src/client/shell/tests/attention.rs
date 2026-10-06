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

/// The tab bar's own fixtures need a pane surface and a matching topology: without
/// the surface the bar's hits are never registered, which is what defeated my first
/// attempt at this fence.
fn badged_tab_state() -> ClientShellState {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_endpoint_status(&ClientEndpointId::Local, ClientEndpointStatus::Online);
    let mut projected = snapshot();
    let mut second_tab = projected.tabs[0].clone();
    second_tab.tab_id = "tab_2".into();
    second_tab.number = 2;
    second_tab.label = "2".into();
    second_tab.focused = false;
    projected.tabs.push(second_tab);
    state.set_snapshot(Box::new(projected));
    state.set_pane_surface(surface());
    let projection = serde_json::from_value(serde_json::json!({
        "contract":"host.documents.v1", "boot_id":"boot-1", "revision":1,
        "documents":[{"provider_priority":1,"document":{
            "contract":"host.view.v1", "namespace":"independent.attention", "generation":7,"revision":2,
            "surface":"navigation","target":{"kind":"session"},
            "items":[
                {"id":"context:attention.view:independent:read","role":"section","label":"Work"},
                {"id":"context:attention.badge:tab:tab_1:0:1:0","role":"section",
                 "label":"1 waiting on you"}
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

/// Real attention on the active endpoint, so a badge is actually drawn and its cells
/// can be clicked: two tabs and the workspace row, each badge carrying a count.
fn attention_badge_state(tab_count: u64, workspace_count: u64) -> ClientShellState {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_endpoint_status(&ClientEndpointId::Local, ClientEndpointStatus::Online);
    let mut projected = snapshot();
    let mut second_tab = projected.tabs[0].clone();
    second_tab.tab_id = "tab_2".into();
    second_tab.number = 2;
    second_tab.label = "2".into();
    second_tab.focused = false;
    projected.tabs.push(second_tab);
    // The endpoint's own snapshot and documents: attention is attached to the active
    // endpoint, which is what the tab bar and the agent sidebar draw from.
    state.set_endpoint_snapshot_for_generation(&ClientEndpointId::Local, 1, Box::new(projected));
    state.set_pane_surface(surface());
    // The workspace rows belong to the agent sidebar; the badge under test is on one.
    state.show_agents = true;
    let projection = serde_json::from_value(serde_json::json!({
        "contract":"host.documents.v1", "boot_id":"boot-1", "revision":1,
        "documents":[{"provider_priority":1,"document":{
            "contract":"host.view.v1", "namespace":"independent.attention", "generation":7,"revision":2,
            "surface":"navigation","target":{"kind":"session"},
            "items":[
                {"id":"context:attention.view:independent:read","role":"section","label":"Work"},
                {"id":"context:attention.badge:tab:tab_1:0:1:0","role":"section",
                 "label": format!("{tab_count} waiting on you")},
                {"id":"context:attention.badge:workspace:ws_1:1:0:0","role":"section",
                 "label": format!("{workspace_count} waiting on you")}
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

fn click(state: &mut ClientShellState, column: u16, row: u16) -> ClientShellInput {
    state.handle_raw_events(vec![
        RawInputEvent::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::empty(),
        }),
        RawInputEvent::Mouse(MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::empty(),
        }),
    ])
}

fn composed_rows(frame: &FrameData, width: u16) -> Vec<String> {
    frame
        .cells
        .chunks(usize::from(width))
        .map(|line| line.iter().map(|cell| cell.symbol.as_str()).collect())
        .collect()
}

#[test]
fn a_tab_badge_is_part_of_its_tab_and_shows_a_capped_count() {
    // QA's P2 at f0e31328: the earlier fence explicitly rendered no badge, so its
    // "no attention hit" assertion could not fail. Real attention, a real composed
    // badge, and a click on the badge's own cells.
    for (count, expected) in [(1_u64, "[1]"), (100, "[99+]")] {
        let mut state = attention_badge_state(count, 1);
        let frame = state.compose(106, 30).expect("frame");
        let bar_row = state.layout(106, 30).tab_bar.y;
        let bar = &composed_rows(&frame, 106)[usize::from(bar_row)];
        let column =
            bar.find(expected)
                .unwrap_or_else(|| panic!("the tab shows {expected}: {bar:?}")) as u16;
        let (tab_rect, tab_id) = state
            .hits
            .tabs
            .first()
            .cloned()
            .expect("the tab bar registered its tab");
        assert_eq!(tab_id, "tab_1");
        assert!(
            tab_rect.x <= column && column < tab_rect.right(),
            "the badge's cells are inside the tab: {tab_rect:?} at column {column}"
        );
        // A badge on a tab registers no hit of its own, so every hit the bar has is
        // the tab's: a click on the badge cells is a click on the tab.
        assert!(
            state.hits.attention.is_empty(),
            "a tab badge registers no attention hit: {:?}",
            state
                .hits
                .attention
                .iter()
                .map(|hit| hit.rect)
                .collect::<Vec<_>>()
        );
        let outcome = click(&mut state, column, bar_row);
        let focused = outcome
            .actions
            .iter()
            .filter_map(|action| match action {
                ClientShellAction::Endpoint { request, .. } => match &request.method {
                    crate::api::schema::Method::TabFocus(target) => Some(target.tab_id.clone()),
                    _ => None,
                },
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            focused,
            vec!["tab_1".to_string()],
            "a click on the badge's cells selects that tab"
        );
        assert!(
            state.pane_panel_open.is_empty(),
            "the badge has no action of its own: {:?}",
            state.pane_panel_open
        );
    }
}

#[test]
fn a_workspace_row_badge_is_part_of_its_row() {
    // The same supersession on the sidebar side: the badge is drawn inside the row,
    // and a click on its cells is a click on the row's workspace.
    let mut state = attention_badge_state(1, 1);
    let frame = state.compose(106, 30).expect("frame");
    let rows = composed_rows(&frame, 106);
    // The badge's text is the attention label in brackets, drawn inside the row: find
    // the row that carries it and the badge's own first cell, so a truncated badge
    // still yields the cells to click.
    let (row_index, column) = rows
        .iter()
        .enumerate()
        .find_map(|(index, row)| {
            row.contains("waiting on you")
                .then(|| row.find('['))
                .flatten()
                .map(|column| (index, column as u16))
        })
        .unwrap_or_else(|| panic!("the workspace row draws its badge: {rows:?}"));
    let workspace_rect = state
        .hits
        .workspaces
        .iter()
        .find(|hit| hit.workspace_id == "ws_1")
        .map(|hit| hit.rect)
        .expect("the sidebar registered its workspace row");
    assert!(
        super::contains(workspace_rect, (column, row_index as u16)),
        "the badge's cells are inside the row: {workspace_rect:?} at ({column}, {row_index})"
    );
    assert!(
        state.hits.attention.is_empty(),
        "a workspace badge registers no attention hit: {:?}",
        state
            .hits
            .attention
            .iter()
            .map(|hit| hit.rect)
            .collect::<Vec<_>>()
    );
    let outcome = click(&mut state, column, row_index as u16);
    let focused = outcome
        .actions
        .iter()
        .filter_map(|action| match action {
            ClientShellAction::Endpoint { request, .. } => match &request.method {
                crate::api::schema::Method::WorkspaceFocus(target) => {
                    Some(target.workspace_id.clone())
                }
                _ => None,
            },
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        focused,
        vec!["ws_1".to_string()],
        "a click on the badge's cells selects the row's workspace"
    );
    assert!(
        state.pane_panel_open.is_empty(),
        "the badge has no action of its own: {:?}",
        state.pane_panel_open
    );
}

#[test]
fn a_clipped_badged_tab_never_reaches_the_renderer_with_a_zero_width_badge() {
    // QA's P2 at 21407313: a tab clipped to a few cells with a badge split into
    // (label n, badge 0) but still drew the badge - a zero-width rect at the tab's own
    // first cell, over the label - which tripped the draw site's own assert. A compose
    // never clips a tab this far (the bar stops drawing them long before), so the
    // renderer is called directly with the narrow areas that do.
    let config = ClientShellConfig::from_config(&Config::default());
    let projected = snapshot();
    let mut state = attention_badge_state(1, 1);
    state.compose(106, 30).expect("frame");
    let nav = state.endpoints[0].attention.clone();
    for width in 1_u16..=8 {
        let area = Rect::new(0, 0, width, 1);
        let mut buffer = Buffer::empty(area);
        let mut hits = ShellHitMap::default();
        let mut tab_scroll = 0_usize;
        let mut reveal_focused_tab = false;
        render::tabs::render_tab_bar(
            &mut buffer,
            area,
            &projected,
            &config,
            &mut tab_scroll,
            &mut reveal_focused_tab,
            None,
            nav.as_ref(),
            &mut hits,
        );
    }
}

#[test]
fn badged_tabs_keep_their_own_hit_area_and_no_badge_hit() {
    let mut state = badged_tab_state();
    let _ = state.compose(106, 30).expect("frame");
    // No cell of the bar answers with a badge's own action: the badge registers no
    // hit, so every hit the bar has is the tab's.
    assert!(
        state.hits.attention.is_empty(),
        "a badge on a tab registers no attention hit: {:?}",
        state
            .hits
            .attention
            .iter()
            .map(|hit| hit.rect)
            .collect::<Vec<_>>()
    );
    if let Some((rect, tab_id)) = state.hits.tabs.first().cloned() {
        assert!(
            rect.width >= 6,
            "the whole tab is one hit area, badge included: {rect:?}"
        );
        assert_eq!(tab_id, "tab_1");
    }
}
