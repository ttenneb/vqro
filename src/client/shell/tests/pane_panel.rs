//! Panel rendering: what Vqro draws from a published view, with no terminal.
use super::*;

fn view_projection(agent_state: Option<AgentStatus>) -> serde_json::Value {
    let mut agents = Vec::new();
    if let Some(status) = agent_state {
        agents.push(serde_json::json!({
            "pane_id": "pane_1", "workspace_id": "ws_1", "tab_id": "tab_1",
            "name": Some("pilot-owner"), "display_agent": serde_json::Value::Null, "agent": Some("pi"),
            "title": serde_json::Value::Null, "terminal_title": serde_json::Value::Null, "terminal_title_stripped": serde_json::Value::Null,
            "agent_status": status, "state_change_seq": 1,
            "state_labels": Vec::<(String, String)>::new(),
            "tokens": Vec::<(String, String)>::new(), "focused": true
        }));
    }
    serde_json::json!({
        "contract":"host.documents.v1", "boot_id":"boot-1", "revision":1,
        "documents":[{"provider_priority":1,"document":{
            "contract":"host.view.v1", "namespace":"example.view", "generation":7,"revision":2,
            "surface":"navigation","target":{"kind":"session"},
            "items":[
                {"id":"panel:pane_1:tasks","role":"section","label":"Tasks",
                 "detail":"0/3 done · 1 active · 1 blocked","flags":{"expanded":true},
                 "view":{"pane_id":"pane_1","view_id":"tasks","state":"in_progress"}},
                {"id":"panel:pane_1:root","parent_id":"panel:pane_1:tasks","role":"section",
                 "label":"Panel panels layout","terminal":{"terminal_id":"term_1",
                   "placement":{"pane_id":"pane_1","workspace_id":"ws_1","tab_id":"tab_1"}},
                 "view":{"state":"in_progress","since":1700000000}},
                {"id":"panel:pane_1:child","parent_id":"panel:pane_1:root","role":"section",
                 "label":"Spike fix","terminal":{"terminal_id":"term_1",
                   "placement":{"pane_id":"pane_1","workspace_id":"ws_1","tab_id":"tab_1"}},
                 "view":{"state":"blocked","waiting":"them","action":"unblock",
                   "since":1700000000}},
                {"id":"panel:pane_1:subtask","parent_id":"panel:pane_1:child","role":"section",
                 "label":"Nested check row",
                 "view":{"state":"open","waiting":"you","action":"report","since":1700000000}},
                {"id":"panel:pane_2:tasks","role":"section","label":"Other pane",
                 "view":{"pane_id":"pane_2","view_id":"tasks"}}
            ],
            "actions":[],"required_capabilities":[]
        }}]
    })
}

/// One pane big enough to hold a panel, unlike the shared 4x2 fixture.
fn panel_surface() -> PaneSurfaceFrame {
    let surface_buffer = Buffer::with_lines((0..28).map(|_| "PANE".repeat(25)).collect::<Vec<_>>());
    PaneSurfaceFrame {
        boot_id: "boot-1".into(),
        projection_revision: 1,
        surface_revision: 1,
        frame: FrameData::from_ratatui_buffer_with_hyperlinks(&surface_buffer, None, &[]),
        panes: vec![PaneSurfacePane {
            pane_id: "pane_1".into(),
            content_revision: 0,
            rect: SurfaceRect {
                x: 0,
                y: 0,
                width: 100,
                height: 28,
            },
            inner_rect: SurfaceRect {
                x: 1,
                y: 1,
                width: 98,
                height: 26,
            },
            scrollbar_rect: None,
            scroll: None,
            focused: true,
            mouse_reporting: false,
            sgr_pixel_mouse: false,
            alternate_screen_active: false,
            pixel_width: 0,
            pixel_height: 0,
        }],
        collections: Vec::new(),
        splits: Vec::new(),
        popup: None,
        graphics: crate::protocol::SurfaceGraphicsScene::default(),
    }
}

/// The same pane, with one row whose block kind Vqro does not know.
fn unsupported_projection() -> serde_json::Value {
    serde_json::json!({
        "contract":"host.documents.v1", "boot_id":"boot-1", "revision":1,
        "documents":[{"provider_priority":1,"document":{
            "contract":"host.view.v1", "namespace":"example.view", "generation":7,"revision":2,
            "surface":"navigation","target":{"kind":"session"},
            "items":[
                {"id":"panel:pane_1:tasks","role":"section","label":"Tasks","detail":"0/1 done",
                 "view":{"pane_id":"pane_1","view_id":"tasks","state":"in_progress"}},
                {"id":"panel:pane_1:chart","parent_id":"panel:pane_1:tasks","role":"section",
                 "label":"A future block","view":{"kind":"sparkline","state":"open"}}
            ],
            "actions":[],"required_capabilities":[]
        }}]
    })
}

fn panel_state(agent_state: Option<AgentStatus>, open: bool) -> ClientShellState {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_endpoint_status(&ClientEndpointId::Local, ClientEndpointStatus::Online);
    state.set_endpoint_capabilities_for(
        &ClientEndpointId::Local,
        vec![
            crate::protocol::endpoint::PANE_VIEWPORT_CAPABILITY.into(),
            crate::protocol::endpoint::PANE_PANELS_CAPABILITY.into(),
        ],
    );
    let mut snapshot = snapshot();
    if let Some(status) = agent_state {
        snapshot.agents = serde_json::from_value(serde_json::json!([{
            "pane_id":"pane_1","workspace_id":"ws_1","tab_id":"tab_1",
            "name": Some("pilot-owner"), "display_agent": serde_json::Value::Null, "agent": Some("pi"),
            "title": serde_json::Value::Null, "terminal_title": serde_json::Value::Null, "terminal_title_stripped": serde_json::Value::Null,
            "agent_status": status, "state_change_seq": 1,
            "state_labels": [], "tokens": [], "focused": true
        }]))
        .expect("agents");
    }
    state.set_endpoint_snapshot_for_generation(&ClientEndpointId::Local, 1, Box::new(snapshot));
    state.set_pane_surface(panel_surface());
    let projection: crate::protocol::endpoint::HostDocumentsProjectionV1 =
        serde_json::from_value(view_projection(agent_state)).expect("projection");
    state.set_endpoint_host_documents_for_generation(
        &ClientEndpointId::Local,
        1,
        Box::new(projection),
    );
    if open {
        state
            .pane_panel_open
            .entry("pane_1".into())
            .or_default()
            .insert("tasks".into());
    }
    state
}

fn text_of(state: &mut ClientShellState) -> String {
    let frame = state.compose(106, 30).expect("frame");
    frame
        .cells
        .iter()
        .map(|cell| cell.symbol.as_str())
        .collect::<String>()
}

#[test]
fn a_view_draws_its_header_rows_glyphs_and_live_link() {
    let mut state = panel_state(Some(AgentStatus::Working), true);
    let text = text_of(&mut state);
    assert!(text.contains("Tasks"), "{text}");
    assert!(text.contains("⊟"), "header mark: {text}");
    assert!(text.contains("0/3 done · 1 active · 1 blocked"), "{text}");
    assert!(text.contains("Panel panels layout"), "{text}");
    assert!(text.contains("Spike fix"), "{text}");
    // Pi-style tree lanes: every level indents, and the lanes stay blank
    // because each row here is its parent's last child.
    assert!(text.contains("└─ ▾ ◐"), "{text}");
    assert!(text.contains("   └─ ▾ ⊘"), "{text}");
    assert!(text.contains("      └─   ○"), "{text}");
    assert!(text.contains('◐'), "{text}");
    assert!(text.contains('⊘'), "{text}");
    // The plain waiting sentence, with the live agent name Vqro joined.
    assert!(text.contains("waiting on you"), "{text}");
    assert!(text.contains("waiting on pilot-owner: unblock"), "{text}");
    // A row with no waiting sentence still names its live assignee.
    assert!(text.contains("● pilot-owner · "), "{text}");
    // A view published for another pane never draws here.
    assert!(!text.contains("Other pane"), "{text}");
}

#[test]
fn an_open_panel_claims_the_terminal_and_a_closed_one_claims_nothing() {
    let mut open = panel_state(None, true);
    let _ = text_of(&mut open);
    assert!(
        !open.hits.pane_panels.is_empty(),
        "the panel registers its cells"
    );
    assert!(
        open.hits
            .panes
            .iter()
            .any(|pane| pane.pane_id == "pane_1" && pane.inner_rect.y > 1),
        "the terminal starts below the panel"
    );
    let mut closed = panel_state(None, false);
    let text = text_of(&mut closed);
    assert!(!text.contains("⊟"), "no panel is drawn: {text}");
    // With views available and nothing open, the pane shows its own badge.
    assert!(text.contains('⚑'), "badge: {text}");
}

#[test]
fn the_header_collapses_the_panel_and_returns_the_rows() {
    let mut state = panel_state(None, true);
    let expanded = text_of(&mut state);
    assert!(expanded.contains("Panel panels layout"));
    // The header line is the panel's own toggle.
    let header = state
        .hits
        .pane_panels
        .iter()
        .find(|hit| {
            matches!(
                hit.action,
                super::super::pane_panel::PanePanelAction::ToggleView(_)
            )
        })
        .cloned()
        .expect("header hit");
    state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: header.rect.x,
        row: header.rect.y,
        modifiers: KeyModifiers::empty(),
    })]);
    let collapsed = text_of(&mut state);
    assert!(collapsed.contains('▸'), "collapsed marker: {collapsed}");
    assert!(collapsed.contains("Tasks"), "{collapsed}");
    assert!(
        !collapsed.contains("Panel panels layout"),
        "rows are hidden: {collapsed}"
    );
    // The terminal got every row back.
    let pane = state
        .hits
        .panes
        .iter()
        .find(|pane| pane.pane_id == "pane_1")
        .expect("pane");
    // Tab bar, header line, divider: the terminal starts under those.
    assert_eq!(
        pane.inner_rect.y, 4,
        "the terminal sits right under the header"
    );
}

#[test]
fn a_row_marker_collapses_only_that_rows_children() {
    let mut state = panel_state(None, true);
    let _ = text_of(&mut state);
    let marker = state
        .hits
        .pane_panels
        .iter()
        .find(|hit| {
            matches!(&hit.action, super::super::pane_panel::PanePanelAction::ToggleRow(id) if id == "panel:pane_1:root")
        })
        .cloned()
        .expect("row marker");
    state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: marker.rect.x,
        row: marker.rect.y,
        modifiers: KeyModifiers::empty(),
    })]);
    let text = text_of(&mut state);
    assert!(text.contains("Panel panels layout"), "{text}");
    assert!(!text.contains("Spike fix"), "child hidden: {text}");
    assert!(text.contains('▸'), "{text}");
}

#[test]
fn an_empty_slice_says_so_instead_of_drawing_nothing() {
    let mut state = panel_state(None, true);
    // The same pane, with the row items removed from the document.
    let projection: crate::protocol::endpoint::HostDocumentsProjectionV1 =
        serde_json::from_value(serde_json::json!({
            "contract":"host.documents.v1", "boot_id":"boot-1", "revision":1,
            "documents":[{"provider_priority":1,"document":{
                "contract":"host.view.v1", "namespace":"example.view", "generation":7,"revision":2,
                "surface":"navigation","target":{"kind":"session"},
                "items":[{"id":"panel:pane_1:tasks","role":"section","label":"Tasks","detail":"settled",
                          "view":{"pane_id":"pane_1","view_id":"tasks"}}],
                "actions":[],"required_capabilities":[]
            }}]
        }))
        .expect("projection");
    state.set_endpoint_host_documents_for_generation(
        &ClientEndpointId::Local,
        1,
        Box::new(projection),
    );
    let text = text_of(&mut state);
    assert!(text.contains("nothing to show"), "{text}");
}

fn state_with_projection(projection: serde_json::Value, open: bool) -> ClientShellState {
    let mut state = panel_state(None, false);
    let projection: crate::protocol::endpoint::HostDocumentsProjectionV1 =
        serde_json::from_value(projection).expect("projection");
    state.set_endpoint_host_documents_for_generation(
        &ClientEndpointId::Local,
        1,
        Box::new(projection),
    );
    if open {
        state
            .pane_panel_open
            .entry("pane_1".into())
            .or_default()
            .insert("tasks".into());
    }
    state
}

#[test]
fn an_unknown_block_renders_as_unsupported_in_place() {
    let mut state = state_with_projection(unsupported_projection(), true);
    let text = text_of(&mut state);
    // The section still draws, and the unknown row keeps its place and label.
    assert!(text.contains("Tasks"), "{text}");
    assert!(text.contains("A future block"), "{text}");
    // The row says what Vqro cannot draw instead of vanishing.
    assert!(text.contains('?'), "unsupported marker: {text}");
    // The known row still renders beside it.
    assert!(text.contains("└─"), "{text}");
}

#[test]
fn a_stack_keeps_its_known_sections_beside_a_placeholder() {
    // One known section and one whose kind Vqro does not know: both draw.
    let mut projection = unsupported_projection();
    let items = projection["documents"][0]["document"]["items"]
        .as_array_mut()
        .expect("items");
    let mut known = items[1].clone();
    known["id"] = serde_json::Value::String("panel:pane_1:known".into());
    known["label"] = serde_json::Value::String("A known row".into());
    known["view"] = serde_json::json!({"state": "open"});
    items.push(known);
    let mut state = state_with_projection(projection, true);
    let text = text_of(&mut state);
    assert!(text.contains("Tasks"), "{text}");
    assert!(text.contains("A future block"), "{text}");
    assert!(text.contains('?'), "the unknown block is marked: {text}");
    assert!(text.contains("A known row"), "{text}");
}

#[test]
fn a_missing_view_in_a_stack_says_unavailable_on_its_own_row() {
    let mut state = panel_state(None, true);
    // Two views open; only one is published.
    state
        .pane_panel_open
        .entry("pane_1".into())
        .or_default()
        .insert("messages".into());
    let text = text_of(&mut state);
    assert!(text.contains("Panel panels layout"), "{text}");
    assert!(text.contains("messages"), "{text}");
}

#[test]
fn an_unknown_section_kind_renders_unsupported_and_keeps_its_edge() {
    let mut projection = unsupported_projection();
    projection["documents"][0]["document"]["items"][0]["view"]["kind"] =
        serde_json::Value::String("timeline".into());
    let mut state = state_with_projection(projection, true);
    let text = text_of(&mut state);
    assert!(text.contains("unsupported timeline"), "{text}");
    assert!(text.contains("unsupported block timeline"), "{text}");
    let claimed = state
        .hits
        .panes
        .iter()
        .find(|pane| pane.pane_id == "pane_1")
        .map(|pane| pane.inner_rect.y)
        .unwrap_or(0);
    assert!(claimed > 3, "the edge stays reserved: {claimed}");
}

#[test]
fn a_view_that_stops_being_published_says_unavailable() {
    let mut state = panel_state(None, true);
    let healthy = text_of(&mut state);
    assert!(healthy.contains("Panel panels layout"), "{healthy}");
    // The provider is gone: the same document, with nothing for this pane.
    let empty: crate::protocol::endpoint::HostDocumentsProjectionV1 =
        serde_json::from_value(serde_json::json!({
            "contract":"host.documents.v1", "boot_id":"boot-1", "revision":1, "documents":[]
        }))
        .expect("projection");
    state.set_endpoint_host_documents_for_generation(&ClientEndpointId::Local, 1, Box::new(empty));
    let text = text_of(&mut state);
    assert!(text.contains("unavailable"), "{text}");
    assert!(text.contains("extension unavailable"), "{text}");
    assert!(!text.contains("Panel panels layout"), "{text}");
}

#[test]
fn dragging_the_edge_resizes_the_panel_and_never_toggles_it() {
    let mut state = panel_state(None, true);
    let _ = text_of(&mut state);
    let edge = state
        .hits
        .pane_panels
        .iter()
        .find(|hit| {
            matches!(
                hit.action,
                super::super::pane_panel::PanePanelAction::ResizePanel
            )
        })
        .cloned()
        .expect("edge hit");
    assert!(state.pane_panel_display["pane_1"].user_height.is_none());
    fn mouse(kind: MouseEventKind, column: u16, row: u16) -> RawInputEvent {
        RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::empty(),
        })
    }
    // Press on the edge, drag down two rows, release.
    state.handle_raw_events(vec![mouse(
        MouseEventKind::Down(MouseButton::Left),
        edge.rect.x,
        edge.rect.y,
    )]);
    state.handle_raw_events(vec![mouse(
        MouseEventKind::Drag(MouseButton::Left),
        edge.rect.x,
        edge.rect.y + 2,
    )]);
    let grown = state.pane_panel_display["pane_1"]
        .user_height
        .expect("the drag set the user's height");
    assert!(grown > 4, "the drag grew the panel: {grown}");
    state.handle_raw_events(vec![mouse(
        MouseEventKind::Up(MouseButton::Left),
        edge.rect.x,
        edge.rect.y + 2,
    )]);
    // Releasing a drag is not a click: the section stays open.
    assert!(
        state.pane_panel_display["pane_1"]
            .collapsed_views
            .is_empty(),
        "the drag must not collapse the section"
    );
    // A release invalidates the cached pane surface (the server re-sends it),
    // so the test hands one back before reading what is drawn.
    state.set_pane_surface(panel_surface());
    let text = text_of(&mut state);
    assert!(text.contains("Panel panels layout"), "{text}");
    let pane = state
        .hits
        .panes
        .iter()
        .find(|pane| pane.pane_id == "pane_1")
        .expect("pane");
    assert!(
        pane.inner_rect.y > edge.rect.y - 3,
        "the terminal follows the taller panel: {}",
        pane.inner_rect.y
    );
}

#[test]
fn the_wheel_scrolls_the_panel_without_touching_the_terminal() {
    // A slice taller than the panel's cap, so it has somewhere to scroll.
    let mut projection = view_projection(None);
    let items = projection["documents"][0]["document"]["items"]
        .as_array_mut()
        .expect("items");
    let template = items[2].clone();
    for index in 0..20 {
        let mut row = template.clone();
        row["id"] = serde_json::Value::String(format!("panel:pane_1:extra:{index}"));
        row["label"] = serde_json::Value::String(format!("Extra row {index}"));
        items.push(row);
    }
    let mut state = state_with_projection(projection, true);
    let _ = text_of(&mut state);
    let header = state
        .hits
        .pane_panels
        .iter()
        .find(|hit| {
            matches!(
                hit.action,
                super::super::pane_panel::PanePanelAction::ToggleView(_)
            )
        })
        .cloned()
        .expect("header hit");
    assert_eq!(state.pane_panel_display["pane_1"].scroll, 0);
    state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::ScrollDown,
        column: header.rect.x + 10,
        row: header.rect.y,
        modifiers: KeyModifiers::empty(),
    })]);
    let scrolled = state.pane_panel_display["pane_1"].scroll;
    assert!(scrolled > 0, "the wheel scrolled the panel: {scrolled}");
    state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::ScrollUp,
        column: header.rect.x + 10,
        row: header.rect.y,
        modifiers: KeyModifiers::empty(),
    })]);
    assert_eq!(state.pane_panel_display["pane_1"].scroll, 0);
    // The terminal was never resized by scrolling.
    assert!(state.pane_panel_display["pane_1"].height.is_some());
}

#[test]
fn persisted_panels_follow_the_terminal_when_the_pane_moves() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.config.preferences.panels = vec![super::super::preferences::ClientPanelPreference {
        terminal_id: "term_moved".into(),
        views: vec!["tasks".into()],
        height: Some(6),
        collapsed_views: Vec::new(),
        collapsed_rows: Vec::new(),
    }];
    let mut moved = snapshot();
    moved.panes[0].pane_id = "pane_9".into();
    state.set_endpoint_status(&ClientEndpointId::Local, ClientEndpointStatus::Online);
    state.set_endpoint_capabilities_for(
        &ClientEndpointId::Local,
        vec![crate::protocol::endpoint::PANE_PANELS_CAPABILITY.into()],
    );
    state.set_endpoint_snapshot_for_generation(&ClientEndpointId::Local, 1, Box::new(moved));
    state.set_pane_surface(panel_surface());
    // The same terminal, now placed in the pane's new identity.
    let projection: crate::protocol::endpoint::HostDocumentsProjectionV1 =
        serde_json::from_value(serde_json::json!({
            "contract":"host.documents.v1", "boot_id":"boot-1", "revision":1,
            "documents":[{"provider_priority":1,"document":{
                "contract":"host.view.v1", "namespace":"example.view", "generation":7,"revision":2,
                "surface":"navigation","target":{"kind":"session"},
                "items":[{"id":"panel:pane_9:tasks","role":"section","label":"Tasks",
                          "view":{"pane_id":"pane_9","view_id":"tasks"},
                          "terminal":{"terminal_id":"term_moved",
                            "placement":{"pane_id":"pane_9","workspace_id":"ws_1","tab_id":"tab_1"}}}],
                "actions":[],"required_capabilities":[]
            }}]
        }))
        .expect("projection");
    state.set_endpoint_host_documents_for_generation(
        &ClientEndpointId::Local,
        1,
        Box::new(projection),
    );
    state.sync_panel_identity();
    assert!(
        state
            .pane_panel_open
            .get("pane_9")
            .is_some_and(|views| views.contains("tasks")),
        "the panel followed its terminal to the pane's new identity"
    );
    assert_eq!(
        state
            .pane_panel_display
            .get("pane_9")
            .and_then(|display| display.user_height),
        Some(6)
    );
    // A terminal that is not in this snapshot claims nothing.
    assert!(!state.pane_panel_open.contains_key("pane_1"));
}

#[test]
fn views_open_by_default_when_an_agent_starts_and_the_user_can_opt_out() {
    let config = ClientShellConfig::from_config(&Config::default());
    assert!(config.show_panels_on_agent_start, "on by default");
    let mut on = panel_state(Some(AgentStatus::Working), false);
    assert!(on.config.show_panels_on_agent_start);
    assert!(on.apply_show_panels_on_agent_start());
    assert!(
        on.pane_panel_open
            .get("pane_1")
            .is_some_and(|views| views.contains("tasks")),
        "an agent start opens the pane's first view"
    );
    let mut off = panel_state(Some(AgentStatus::Working), false);
    off.config.show_panels_on_agent_start = false;
    assert!(!off.apply_show_panels_on_agent_start());
    assert!(off.pane_panel_open.is_empty(), "opting out opens nothing");
}
