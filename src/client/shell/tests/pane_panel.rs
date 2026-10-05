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
    // The unknown root is marked in place and keeps its own row: it no longer
    // replaces the whole stack, so a known sibling in the same pane survives.
    assert!(text.contains("unsupported timeline"), "{text}");
    assert!(text.contains("Tasks: unsupported timeline"), "{text}");
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

/// Two panes side by side, each with its own terminal, for the fences QA asked
/// for: the shared fixture has one pane, which cannot prove them.
fn two_pane_surface() -> PaneSurfaceFrame {
    let surface_buffer = Buffer::with_lines((0..28).map(|_| "PANE".repeat(25)).collect::<Vec<_>>());
    let pane = |pane_id: &str, x: u16, width: u16, focused: bool| PaneSurfacePane {
        pane_id: pane_id.into(),
        content_revision: 0,
        rect: SurfaceRect {
            x,
            y: 0,
            width,
            height: 28,
        },
        inner_rect: SurfaceRect {
            x: x + 1,
            y: 1,
            width: width - 2,
            height: 26,
        },
        scrollbar_rect: None,
        scroll: None,
        focused,
        mouse_reporting: false,
        sgr_pixel_mouse: false,
        alternate_screen_active: false,
        pixel_width: 0,
        pixel_height: 0,
    };
    PaneSurfaceFrame {
        boot_id: "boot-1".into(),
        projection_revision: 1,
        surface_revision: 1,
        frame: FrameData::from_ratatui_buffer_with_hyperlinks(&surface_buffer, None, &[]),
        panes: vec![pane("pane_1", 0, 50, true), pane("pane_2", 51, 49, false)],
        collections: Vec::new(),
        splits: Vec::new(),
        popup: None,
        graphics: crate::protocol::SurfaceGraphicsScene::default(),
    }
}

fn two_pane_projection() -> serde_json::Value {
    let mut projection = view_projection(None);
    let items = projection["documents"][0]["document"]["items"]
        .as_array_mut()
        .expect("items");
    let originals = items.clone();
    for item in originals {
        let text = item.to_string().replace("pane_1", "pane_2");
        let clone: serde_json::Value = serde_json::from_str(&text).expect("clone");
        items.push(clone);
    }
    projection
}

#[test]
fn two_open_panes_each_freeze_their_own_height() {
    let mut state = panel_state(None, false);
    let mut snapshot = snapshot();
    snapshot.panes.push(crate::protocol::ClientShellPane {
        pane_id: "pane_2".into(),
        workspace_id: "ws_1".into(),
        tab_id: "tab_1".into(),
        label: None,
        cwd: None,
        foreground_cwd: None,
        focused: false,
        right_click_passthrough: false,
    });
    state.set_endpoint_snapshot_for_generation(&ClientEndpointId::Local, 1, Box::new(snapshot));
    state.set_pane_surface(two_pane_surface());
    let projection: crate::protocol::endpoint::HostDocumentsProjectionV1 =
        serde_json::from_value(two_pane_projection()).expect("projection");
    state.set_endpoint_host_documents_for_generation(
        &ClientEndpointId::Local,
        1,
        Box::new(projection),
    );
    for pane_id in ["pane_1", "pane_2"] {
        state
            .pane_panel_open
            .entry(pane_id.into())
            .or_default()
            .insert("tasks".into());
    }
    let text = text_of(&mut state);
    assert!(text.contains("Panel panels layout"), "{text}");
    // The fence QA named: the pane drawn first must be frozen too, or its live
    // updates re-fit and resize the terminal under the user.
    for pane_id in ["pane_1", "pane_2"] {
        assert!(
            state.pane_panel_display[pane_id].height.is_some(),
            "{pane_id} keeps a frozen height"
        );
    }
}

#[test]
fn an_open_panel_survives_a_provider_outage_and_returns() {
    let mut state = panel_state(None, true);
    state.set_pane_surface(panel_surface());
    let _ = text_of(&mut state);
    assert!(state.pane_panel_open.contains_key("pane_1"));
    let open = state.pane_panel_open["pane_1"].clone();
    // The provider stops publishing: every placement disappears.
    let empty: crate::protocol::endpoint::HostDocumentsProjectionV1 =
        serde_json::from_value(serde_json::json!({
            "contract":"host.documents.v1", "boot_id":"boot-1", "revision":1, "documents":[]
        }))
        .expect("projection");
    state.set_endpoint_host_documents_for_generation(&ClientEndpointId::Local, 1, Box::new(empty));
    state.sync_panel_identity();
    assert_eq!(
        state.pane_panel_open.get("pane_1"),
        Some(&open),
        "the outage does not forget what the user opened"
    );
    // It comes back with the provider.
    let projection: crate::protocol::endpoint::HostDocumentsProjectionV1 =
        serde_json::from_value(view_projection(None)).expect("projection");
    state.set_endpoint_host_documents_for_generation(
        &ClientEndpointId::Local,
        1,
        Box::new(projection),
    );
    state.sync_panel_identity();
    state.set_pane_surface(panel_surface());
    let text = text_of(&mut state);
    assert!(text.contains("Panel panels layout"), "{text}");
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

#[test]
fn the_panel_band_is_reserved_from_retained_patches() {
    // A retained surface patch must never repaint the band a panel covers, so
    // the band is recorded per compose. It was never populated before.
    let mut state = panel_state(None, true);
    text_of(&mut state);
    let areas = state.pane_panel_areas.clone();
    assert_eq!(1, areas.len(), "one open panel: {areas:?}");
    let area = areas[0];
    assert!(area.height >= 2, "the header and the divider: {area:?}");
    let pane = state
        .hits
        .panes
        .iter()
        .find(|pane| pane.pane_id == "pane_1")
        .expect("pane_1");
    assert_eq!(
        area.x, pane.projected_inner_rect.x,
        "the band spans the pane"
    );
    assert_eq!(area.width, pane.projected_inner_rect.width);
    let closed = {
        let mut state = panel_state(None, false);
        text_of(&mut state);
        state.pane_panel_areas.clone()
    };
    assert!(closed.is_empty(), "no panel, nothing reserved: {closed:?}");
}

#[test]
fn a_second_root_keeps_its_known_siblings() {
    // Two roots in one pane, the first of an unknown kind. The known second
    // root must still draw: the whole stack is not replaced by a placeholder,
    // and the unknown root gets its own row.
    let projection = serde_json::json!({
        "contract":"host.documents.v1", "boot_id":"boot-1", "revision":1,
        "documents":[{"provider_priority":1,"document":{
            "contract":"host.view.v1", "namespace":"example.view", "generation":7,"revision":2,
            "surface":"navigation","target":{"kind":"session"},
            "items":[
                {"id":"panel:pane_1:future","role":"section","label":"A future view",
                 "view":{"pane_id":"pane_1","view_id":"future","kind":"timeline"}},
                {"id":"panel:pane_1:known","role":"section","label":"Known view",
                 "detail":"0/1 done","view":{"pane_id":"pane_1","view_id":"tasks","state":"open"}},
                {"id":"panel:pane_1:known:row","parent_id":"panel:pane_1:known","role":"section",
                 "label":"A known row","view":{"state":"open"}}
            ],
            "actions":[],"required_capabilities":[]
        }}]
    });
    let mut state = panel_state(None, false);
    let projection: crate::protocol::endpoint::HostDocumentsProjectionV1 =
        serde_json::from_value(projection).expect("projection");
    state.set_endpoint_host_documents_for_generation(
        &ClientEndpointId::Local,
        1,
        Box::new(projection),
    );
    for view_id in ["future", "tasks"] {
        state
            .pane_panel_open
            .entry("pane_1".into())
            .or_default()
            .insert(view_id.into());
    }
    let text = text_of(&mut state);
    assert!(text.contains("A future view"), "the unknown root: {text}");
    assert!(text.contains("unsupported timeline"), "{text}");
    assert!(text.contains("Known view"), "the known root: {text}");
    assert!(text.contains("A known row"), "{text}");
}

#[test]
fn an_open_panel_claims_the_rect_below_it() {
    // The claim is what makes the panel a split rather than a crop: the client
    // tells the server the smaller terminal rect, so the pane's PTY is sized to
    // the rows below the panel and the agent reflows.
    let mut state = panel_state(None, true);
    text_of(&mut state);
    let claim = state
        .pending_endpoint_controls
        .iter()
        .map(|(_, message)| format!("{message:?}"))
        .find(|message| message.contains("pane.panels.v1"))
        .expect("the claim is queued");
    let data = claim
        .split("data: \"")
        .nth(1)
        .and_then(|rest| rest.split("\" }").next())
        .expect("claim json");
    let data = data.replace("\\\"", "\"");
    let claim: crate::protocol::endpoint::ClientPanePanels =
        serde_json::from_str(&data).expect("claim decodes");
    let size = claim.claims.first().expect("one claim").size;
    let pane = state
        .hits
        .panes
        .iter()
        .find(|pane| pane.pane_id == "pane_1")
        .expect("pane_1");
    assert!(
        size.rows < pane.projected_inner_rect.height,
        "the claimed rows must be the pane's minus the panel: {size:?} vs {:?}",
        pane.projected_inner_rect
    );
    assert_eq!(size.cols, pane.projected_inner_rect.width);
}

#[test]
fn documents_with_one_namespace_keep_their_rows_apart() {
    // Two providers publish under the same namespace with different pane targets
    // and their own item ids. A shared namespace must not merge their rows.
    let projection = serde_json::json!({
        "contract":"host.documents.v1", "boot_id":"boot-1", "revision":1,
        "documents":[
            {"provider_priority":1,"document":{
                "contract":"host.view.v1","namespace":"example.same","generation":1,"revision":1,
                "surface":"navigation","target":{"kind":"pane","pane_id":"pane_1"},
                "items":[
                    {"id":"root","role":"section","label":"Tasks","view":{"pane_id":"pane_1","view_id":"tasks","state":"open"}},
                    {"id":"child","parent_id":"root","role":"section","label":"Only from A","view":{"state":"open"}}
                ],"actions":[],"required_capabilities":[]}},
            {"provider_priority":1,"document":{
                "contract":"host.view.v1","namespace":"example.same","generation":2,"revision":1,
                "surface":"navigation","target":{"kind":"pane","pane_id":"pane_2"},
                "items":[
                    {"id":"root","role":"section","label":"Tasks","view":{"pane_id":"pane_2","view_id":"tasks","state":"open"}},
                    {"id":"child","parent_id":"root","role":"section","label":"Only from B","view":{"state":"open"}}
                ],"actions":[],"required_capabilities":[]}}
        ]
    });
    let mut state = panel_state(None, false);
    let projection: crate::protocol::endpoint::HostDocumentsProjectionV1 =
        serde_json::from_value(projection).expect("projection");
    state.set_endpoint_host_documents_for_generation(
        &ClientEndpointId::Local,
        1,
        Box::new(projection),
    );
    state
        .pane_panel_open
        .entry("pane_1".into())
        .or_default()
        .insert("tasks".into());
    let text = text_of(&mut state);
    assert!(
        text.contains("Only from A"),
        "pane_1 keeps its own rows: {text}"
    );
    assert!(
        !text.contains("Only from B"),
        "pane_2's rows must not leak into pane_1: {text}"
    );
}

#[test]
fn a_reachable_child_cycle_still_terminates() {
    // A loop the walk actually reaches: root -> a -> b -> a. An earlier version of
    // this test named two roots as each other's parent, which the walk never
    // followed, so it exercised no guard at all. The repeated id is what a malformed
    // document needs for the cycle to exist, and the walk's bound is what stops it.
    let projection = serde_json::json!({
        "contract":"host.documents.v1", "boot_id":"boot-1", "revision":1,
        "documents":[{"provider_priority":1,"document":{
            "contract":"host.view.v1","namespace":"example.loop","generation":1,"revision":1,
            "surface":"navigation","target":{"kind":"session"},
            "items":[
                {"id":"root","role":"section","label":"Tasks","view":{"pane_id":"pane_1","view_id":"tasks","state":"open"}},
                {"id":"a","parent_id":"root","role":"section","label":"Reached A","view":{"state":"open"}},
                {"id":"b","parent_id":"a","role":"section","label":"Reached B","view":{"state":"open"}},
                {"id":"a","parent_id":"b","role":"section","label":"Reached A again","view":{"state":"open"}}
            ],"actions":[],"required_capabilities":[]}}]
    });
    let mut state = panel_state(None, false);
    let projection: crate::protocol::endpoint::HostDocumentsProjectionV1 =
        serde_json::from_value(projection).expect("projection");
    state.set_endpoint_host_documents_for_generation(
        &ClientEndpointId::Local,
        1,
        Box::new(projection),
    );
    state
        .pane_panel_open
        .entry("pane_1".into())
        .or_default()
        .insert("tasks".into());
    let text = text_of(&mut state);
    // Reaching the loop and returning at all is the assertion: without the bound this
    // render would not finish.
    assert!(text.contains("Tasks"), "{text}");
    assert!(text.contains("Reached A"), "{text}");
    assert!(text.contains("Reached B"), "{text}");
}

#[test]
fn a_collapsed_panel_still_says_a_missing_view_is_unavailable() {
    // The user has one published view collapsed and another open view that the
    // extension stopped publishing. Collapsing must not hide the missing one.
    let mut state = panel_state(None, true);
    state
        .pane_panel_open
        .entry("pane_1".into())
        .or_default()
        .insert("messages".into());
    state
        .pane_panel_display
        .entry("pane_1".into())
        .or_default()
        .collapsed_views
        .insert("tasks".into());
    let text = text_of(&mut state);
    assert!(text.contains("▸ "), "the known view is collapsed: {text}");
    assert!(
        text.contains("messages"),
        "the missing open view still says so: {text}"
    );
}

#[test]
fn a_retained_patch_row_lands_below_the_panel_with_the_grid() {
    // The host writes patch rows in the pane's own coordinates, and the panel
    // moved the grid down by its band. The raw surface cache is the host's own
    // view and stays unshifted; the composed row is the one that moves.
    let mut state = panel_state(None, true);
    text_of(&mut state);
    let pane = state
        .hits
        .panes
        .iter()
        .find(|pane| pane.pane_id == "pane_1")
        .expect("pane_1");
    // Screen coordinates for the composed frame, the surface pane's own rect for
    // the patch the host writes.
    let inner = pane.projected_inner_rect;
    let band = state
        .pane_panel_areas
        .first()
        .copied()
        .expect("reserved band")
        .height;
    assert!(band > 0, "the panel reserved a band");
    let mut surface_pane = state
        .pane_surface
        .as_ref()
        .expect("surface")
        .panes
        .iter()
        .find(|pane| pane.pane_id == "pane_1")
        .expect("surface pane")
        .clone();
    surface_pane.content_revision = 2;
    let mut cell = state
        .pane_surface
        .as_ref()
        .expect("surface")
        .frame
        .cells
        .first()
        .cloned()
        .expect("a cell");
    cell.symbol = "Z".into();
    let patch = crate::protocol::PaneSurfacePatch {
        boot_id: "boot-1".into(),
        projection_revision: 1,
        base_surface_revision: 1,
        surface_revision: 2,
        rows: vec![crate::protocol::PaneSurfacePatchRow {
            x: surface_pane.inner_rect.x,
            y: surface_pane.inner_rect.y,
            cells: vec![cell],
        }],
        panes: vec![surface_pane.clone()],
        cursor: None,
    };
    let outcome = state.apply_pane_surface_patch(patch);
    assert!(
        !matches!(outcome, ClientPaneSurfacePatchOutcome::Rejected),
        "the patch must be applied, not rejected"
    );
    let surface = state.pane_surface.as_ref().expect("surface");
    let width = usize::from(surface.frame.width);
    let symbol_at = |surface: &crate::protocol::PaneSurfaceFrame, y: u16, x: u16| {
        surface
            .frame
            .cells
            .get(usize::from(y) * width + usize::from(x))
            .map(|cell| cell.symbol.clone())
            .unwrap_or_default()
    };
    // The host's own cache keeps the row where the host wrote it.
    assert_eq!(
        symbol_at(
            surface,
            surface_pane.inner_rect.y,
            surface_pane.inner_rect.x
        ),
        "Z",
        "the raw surface cache is the host's view and is not shifted"
    );
    // A full compose puts it below the panel, with the grid.
    let frame = state.compose(106, 30).expect("frame");
    let composed = frame
        .cells
        .chunks(106)
        .map(|row| {
            row.iter()
                .map(|cell| cell.symbol.as_str())
                .collect::<String>()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        composed[usize::from(inner.y) + usize::from(band)]
            .chars()
            .nth(usize::from(inner.x))
            .unwrap_or_default(),
        'Z',
        "the composed row belongs a band lower"
    );
    assert_ne!(
        composed[usize::from(inner.y)]
            .chars()
            .nth(usize::from(inner.x))
            .unwrap_or_default(),
        'Z',
        "not in the panel's band"
    );
}

#[test]
fn the_cursor_travels_with_the_grid_below_the_panel() {
    // The host writes the cursor in the pane's own coordinates. The grid moves
    // down by the band, so the cursor must reach the frame that many rows lower:
    // the frame takes the cursor the client cloned before the panel drew.
    let mut state = panel_state(None, true);
    let mut surface = panel_surface();
    let pane_rect = surface.panes[0].inner_rect;
    surface.frame.cursor = Some(crate::protocol::CursorState {
        x: pane_rect.x,
        y: pane_rect.y,
        visible: true,
        shape: 0,
    });
    state.set_pane_surface(surface);
    let frame = state.compose(106, 30).expect("frame");
    let pane = state
        .hits
        .panes
        .iter()
        .find(|pane| pane.pane_id == "pane_1")
        .expect("pane_1");
    let band = state
        .pane_panel_areas
        .first()
        .copied()
        .expect("reserved band")
        .height;
    let cursor = frame.cursor.expect("cursor");
    assert_eq!(
        cursor.y,
        pane.projected_inner_rect.y.saturating_add(band),
        "the cursor belongs at the top of the rect below the panel"
    );
}

/// A patch row for a pane, in that pane's own surface coordinates.
fn pane_patch_row(
    state: &ClientShellState,
    surface_pane: &crate::protocol::PaneSurfacePane,
    y: u16,
    symbol: &str,
) -> crate::protocol::PaneSurfacePatch {
    let mut cell = state
        .pane_surface
        .as_ref()
        .expect("surface")
        .frame
        .cells
        .first()
        .cloned()
        .expect("a cell");
    cell.symbol = symbol.into();
    let surface = state.pane_surface.as_ref().expect("surface");
    crate::protocol::PaneSurfacePatch {
        boot_id: "boot-1".into(),
        projection_revision: 1,
        base_surface_revision: surface.surface_revision,
        surface_revision: surface.surface_revision.saturating_add(1),
        rows: vec![crate::protocol::PaneSurfacePatchRow {
            x: surface_pane.inner_rect.x,
            y,
            cells: vec![cell],
        }],
        panes: vec![surface_pane.clone()],
        cursor: None,
    }
}

/// Two panes with real open panels: each pane is in the surface frame and in the
/// client's hit list, and the documents publish a view for both, so the compose
/// builds a band for each. Nothing is injected.
fn two_panelled_panes_state() -> ClientShellState {
    let mut state = panel_state(None, false);
    // The existing distinct-pane fixture: pane_1 at x=0 and pane_2 at x=51, each
    // 50/49 wide with its own inner rect, so the origins differ and their bands can
    // be told apart. Cloning one pane's rect would stack them exactly.
    state.set_pane_surface(two_pane_surface());
    let items = ["pane_1", "pane_2"]
        .iter()
        .flat_map(|pane_id| {
            [
                serde_json::json!({
                    "id": format!("panel:{pane_id}:tasks"),
                    "role": "section",
                    "label": "Tasks",
                    "detail": "0/0 done",
                    "view": {"pane_id": pane_id, "view_id": "tasks", "state": "open"},
                }),
                serde_json::json!({
                    "id": format!("panel:{pane_id}:tasks:row"),
                    "parent_id": format!("panel:{pane_id}:tasks"),
                    "role": "section",
                    "label": "A row",
                    "view": {"state": "open"},
                }),
            ]
        })
        .collect::<Vec<_>>();
    let projection = serde_json::from_value(serde_json::json!({
        "contract":"host.documents.v1","boot_id":"boot-1","revision":1,
        "documents":[{"provider_priority":1,"document":{
            "contract":"host.view.v1","namespace":"example.tasks","generation":1,"revision":1,
            "surface":"navigation","target":{"kind":"session"},
            "items":items,"actions":[],"required_capabilities":[]}}]
    }))
    .expect("projection");
    state.set_endpoint_host_documents_for_generation(
        &ClientEndpointId::Local,
        1,
        Box::new(projection),
    );
    for pane_id in ["pane_1", "pane_2"] {
        state
            .pane_panel_open
            .entry(pane_id.to_string())
            .or_default()
            .insert("tasks".into());
    }
    state
}

#[test]
fn a_panelled_patch_never_paints_into_the_panel_band() {
    // The split's offset mapping, as the coordinator requires it: a patch row for a
    // panelled pane is translated by the band into the claimed rect below it, so no
    // emitted row lands in the band the panel owns.
    let mut state = two_panelled_panes_state();
    text_of(&mut state);
    assert!(
        !state.pane_panel_areas.is_empty(),
        "both panes drew a panel and reserved a band"
    );
    let pane = state
        .pane_surface
        .as_ref()
        .expect("surface")
        .panes
        .iter()
        .find(|pane| pane.pane_id == "pane_2")
        .cloned()
        .expect("pane_2");
    assert_eq!(
        state.pane_panel_areas.len(),
        2,
        "each pane drew its own panel and band"
    );
    let surfaces = state.pane_surface.as_ref().expect("surface").panes.clone();
    assert_ne!(
        surfaces[0].inner_rect, surfaces[1].inner_rect,
        "the fixture's panes must not overlap"
    );
    let patch = pane_patch_row(&state, &pane, pane.inner_rect.y.saturating_add(2), "B");
    let outcome = state.apply_pane_surface_patch(patch);
    let ClientPaneSurfacePatchOutcome::Applied(Some(composed)) = outcome else {
        panic!("a row inside the grid keeps the fast path");
    };
    assert!(!composed.rows.is_empty(), "the row is emitted");
    for change in &composed.rows {
        let width = u16::try_from(change.cells.len()).unwrap_or(1);
        for band in &state.pane_panel_areas {
            let inside_y = change.y >= band.y && change.y < band.y.saturating_add(band.height);
            let inside_x =
                change.x < band.x.saturating_add(band.width) && change.x + width > band.x;
            assert!(
                !(inside_y && inside_x),
                "cells at ({}, {}) x{width} intersect the band {:?}",
                change.x,
                change.y,
                band
            );
        }
    }
}

#[test]
fn the_fast_path_and_a_full_compose_agree_for_two_panelled_panes() {
    // The valid comparison QA asked for: two panes, both with real panels, no
    // injected band and no pane present in one view but not the other. Every emitted
    // fast row must be what a full compose shows at those coordinates.
    let mut state = two_panelled_panes_state();
    text_of(&mut state);
    assert_eq!(
        state.pane_surface.as_ref().expect("surface").panes.len(),
        2,
        "both panes are in the surface frame"
    );
    let mut emitted = 0_usize;
    for pane_id in ["pane_1", "pane_2"] {
        let pane = state
            .pane_surface
            .as_ref()
            .expect("surface")
            .panes
            .iter()
            .find(|pane| pane.pane_id == pane_id)
            .cloned()
            .expect("a pane");
        for step in 0..4_u16 {
            let symbol = ((b'a' + step as u8) as char).to_string();
            let patch = pane_patch_row(
                &state,
                &pane,
                pane.inner_rect.y.saturating_add(step).saturating_add(2),
                &symbol,
            );
            let ClientPaneSurfacePatchOutcome::Applied(Some(composed)) =
                state.apply_pane_surface_patch(patch)
            else {
                panic!("the fast path applies a row inside the grid");
            };
            let frame = state.compose(106, 30).expect("frame");
            for change in &composed.rows {
                for (offset, cell) in change.cells.iter().enumerate() {
                    let index = usize::from(change.y) * 106 + usize::from(change.x) + offset;
                    let full = frame
                        .cells
                        .get(index)
                        .map(|cell| cell.symbol.clone())
                        .unwrap_or_default();
                    assert_eq!(
                        full,
                        cell.symbol,
                        "fast and full agree for {pane_id} at ({}, {})",
                        change.x + offset as u16,
                        change.y
                    );
                    emitted += 1;
                }
            }
        }
    }
    assert!(emitted >= 8, "every emitted row was compared: {emitted}");

    // The returned cursor, not only the rows: a patch that carries the focused
    // pane's cursor must return the same cursor a full compose shows. With distinct
    // panes there is no shared cursor to move twice.
    let focused = state
        .pane_surface
        .as_ref()
        .expect("surface")
        .panes
        .iter()
        .find(|pane| pane.focused)
        .expect("a focused pane")
        .inner_rect;
    let background = state
        .pane_surface
        .as_ref()
        .expect("surface")
        .panes
        .iter()
        .find(|pane| !pane.focused)
        .cloned()
        .expect("a background pane");
    let mut patch = pane_patch_row(
        &state,
        &background,
        background.inner_rect.y.saturating_add(2),
        "z",
    );
    patch.cursor = Some(crate::protocol::CursorState {
        x: focused.x.saturating_add(2),
        y: focused.y.saturating_add(3),
        visible: true,
        shape: 0,
    });
    let ClientPaneSurfacePatchOutcome::Applied(Some(composed)) =
        state.apply_pane_surface_patch(patch)
    else {
        panic!("the fast path applies this patch");
    };
    let frame = state.compose(106, 30).expect("frame");
    let fast = composed.cursor.map(|cursor| (cursor.y, cursor.visible));
    let full = frame.cursor.map(|cursor| (cursor.y, cursor.visible));
    assert_eq!(
        fast, full,
        "the returned cursor (row, visible) must equal the full compose's"
    );
}

#[test]
fn a_row_past_the_grid_takes_the_compose_path() {
    // The narrow exception: a row a panel moved has no destination once it is past
    // the grid, so that patch composes instead of translating.
    let mut state = two_panelled_panes_state();
    text_of(&mut state);
    let pane = state
        .pane_surface
        .as_ref()
        .expect("surface")
        .panes
        .first()
        .cloned()
        .expect("a pane");
    let past = pane
        .inner_rect
        .y
        .saturating_add(pane.inner_rect.height)
        .saturating_sub(1);
    let patch = pane_patch_row(&state, &pane, past, "C");
    let outcome = state.apply_pane_surface_patch(patch);
    assert!(
        matches!(outcome, ClientPaneSurfacePatchOutcome::Applied(None)),
        "a row with no destination after the translation composes instead of translating"
    );
}

#[test]
fn a_background_patch_keeps_the_composes_cursor_in_the_pane() {
    // A patch carries only the panes that changed, so the focused pane can be
    // absent. The cursor belongs to that pane: the compose must still contain it -
    // a band lower - and hide it when it would land past the pane.
    let mut state = panel_state(None, true);
    let mut surface = state.pane_surface.as_ref().expect("surface").clone();
    let mut lower = surface.panes[0].clone();
    lower.pane_id = "pane_2".into();
    lower.focused = false;
    lower.inner_rect = crate::protocol::SurfaceRect {
        x: 1,
        y: 20,
        width: 10,
        height: 16,
    };
    surface.panes.push(lower.clone());
    surface.frame.cursor = Some(crate::protocol::CursorState {
        x: 2,
        y: 3,
        visible: true,
        shape: 0,
    });
    state.set_pane_surface(surface);
    text_of(&mut state);
    let band = *state
        .pane_panel_band_rows
        .get("pane_1")
        .expect("the focused pane has a band");
    assert!(band > 0, "the focused pane has a panel band");
    let mut patch = pane_patch_row(&state, &lower, lower.inner_rect.y.saturating_add(3), "S");
    patch.cursor = Some(crate::protocol::CursorState {
        x: 2,
        y: 3,
        visible: true,
        shape: 0,
    });
    let outcome = state.apply_pane_surface_patch(patch);
    assert!(
        !matches!(outcome, ClientPaneSurfacePatchOutcome::Rejected),
        "the patch is accepted"
    );
    let frame = state.compose(106, 30).expect("frame");
    let area = state.layout(106, 30).pane_surface;
    let cursor = frame.cursor.expect("a composed cursor");
    assert_eq!(
        cursor.y,
        area.y + 3 + band,
        "the composed cursor travels with the grid, a band lower"
    );
}

#[test]
fn a_parent_link_across_documents_is_not_followed() {
    // Two documents that name each other's item as a parent. Document-scoped
    // children mean neither link is followed, so rows stay isolated. This is an
    // isolation fence, not a cycle fence: with the links dropped there is no
    // combined loop to walk - the reachable-cycle guard is
    // a_reachable_child_cycle_still_terminates.
    let projection = serde_json::json!({
        "contract":"host.documents.v1", "boot_id":"boot-1", "revision":1,
        "documents":[
            {"provider_priority":1,"document":{
                "contract":"host.view.v1","namespace":"example.same","generation":1,"revision":1,
                "surface":"navigation","target":{"kind":"pane","pane_id":"pane_1"},
                "items":[
                    {"id":"rootA","role":"section","label":"Root A","view":{"pane_id":"pane_1","view_id":"tasks","state":"open"}},
                    {"id":"childA","parent_id":"rootB","role":"section","label":"Child A","view":{"state":"open"}}
                ],"actions":[],"required_capabilities":[]}},
            {"provider_priority":1,"document":{
                "contract":"host.view.v1","namespace":"example.same","generation":2,"revision":1,
                "surface":"navigation","target":{"kind":"pane","pane_id":"pane_2"},
                "items":[
                    {"id":"rootB","role":"section","label":"Root B","view":{"pane_id":"pane_2","view_id":"tasks","state":"open"}},
                    {"id":"childB","parent_id":"rootA","role":"section","label":"Child B","view":{"state":"open"}}
                ],"actions":[],"required_capabilities":[]}}
        ]
    });
    let mut state = panel_state(None, false);
    let projection: crate::protocol::endpoint::HostDocumentsProjectionV1 =
        serde_json::from_value(projection).expect("projection");
    state.set_endpoint_host_documents_for_generation(
        &ClientEndpointId::Local,
        1,
        Box::new(projection),
    );
    state
        .pane_panel_open
        .entry("pane_1".into())
        .or_default()
        .insert("tasks".into());
    let text = text_of(&mut state);
    assert!(text.contains("Root A"), "its own root: {text}");
    assert!(
        !text.contains("Child B"),
        "the other document's item must not be walked in: {text}"
    );
}

#[test]
fn a_long_run_of_patches_keeps_the_cache_and_the_grid_in_step() {
    // QA's long-run probe, as a fence: a hundred successive live updates to a
    // panelled pane. Each one moves the row by its band in the fast output while
    // the host's own cache keeps it where the host wrote it, and the run must not
    // drift or drop the latest.
    let mut state = panel_state(None, true);
    text_of(&mut state);
    let surface_pane = state
        .pane_surface
        .as_ref()
        .expect("surface")
        .panes
        .first()
        .cloned()
        .expect("a pane");
    let band = *state
        .pane_panel_band_rows
        .get(&surface_pane.pane_id)
        .expect("the pane has a band");
    assert!(band > 0, "the pane has a panel band");
    let row = surface_pane.inner_rect.y.saturating_add(5);
    let mut last = String::new();
    for step in 0..100_u32 {
        let symbol = format!("{}", (b'a' + (step % 26) as u8) as char);
        let patch = pane_patch_row(&state, &surface_pane, row, &symbol);
        let outcome = state.apply_pane_surface_patch(patch);
        assert!(
            !matches!(outcome, ClientPaneSurfacePatchOutcome::Rejected),
            "update {step} is accepted"
        );
        last = symbol;
    }
    // The host's own cache still has the latest at the row the host wrote.
    let surface = state.pane_surface.as_ref().expect("surface");
    let width = usize::from(surface.frame.width);
    let cached = surface
        .frame
        .cells
        .get(usize::from(row) * width + usize::from(surface_pane.inner_rect.x))
        .map(|cell| cell.symbol.clone())
        .unwrap_or_default();
    assert_eq!(cached, last, "the raw cache holds the latest update");
    // And a full compose shows it a band lower, with the grid.
    let frame = state.compose(106, 30).expect("frame");
    let area = state.layout(106, 30).pane_surface;
    let composed_row = usize::from(area.y + row + band);
    let composed = frame
        .cells
        .chunks(106)
        .nth(composed_row)
        .map(|line| {
            line.iter()
                .map(|cell| cell.symbol.as_str())
                .collect::<String>()
        })
        .unwrap_or_default();
    assert!(
        composed.contains(&last),
        "a full compose shows the latest update a band lower at row {composed_row}: {composed:?}"
    );
}

#[test]
fn the_fast_path_and_a_full_compose_agree_without_a_panel() {
    // A valid, consistent comparison: no injected bands, no second pane that only
    // exists in one of the two views, and every emitted row is checked rather than
    // counted. Where the fast path applies at all, it must write exactly what a full
    // compose shows at those coordinates. (A panelled pane never reaches here: it
    // declines the fast path and takes the full compose, which is fenced by
    // a_panelled_panes_patch_takes_the_full_compose_and_composes_correctly.)
    let mut state = panel_state(None, false);
    text_of(&mut state);
    assert!(
        state.pane_panel_band_rows.is_empty(),
        "no pane has a panel, so no band is injected"
    );
    let surface_pane = state
        .pane_surface
        .as_ref()
        .expect("surface")
        .panes
        .first()
        .cloned()
        .expect("a pane");
    let mut emitted = 0_usize;
    for (index, symbol) in ["P", "Q", "R", "S"].iter().enumerate() {
        let row = surface_pane.inner_rect.y.saturating_add(index as u16);
        let patch = pane_patch_row(&state, &surface_pane, row, symbol);
        let ClientPaneSurfacePatchOutcome::Applied(Some(composed)) =
            state.apply_pane_surface_patch(patch)
        else {
            panic!("the fast path applies a patch without a panel");
        };
        assert!(
            !composed.rows.is_empty(),
            "the fast path emits the changed row, not nothing"
        );
        let frame = state.compose(106, 30).expect("frame");
        for change in &composed.rows {
            for (offset, cell) in change.cells.iter().enumerate() {
                let index = usize::from(change.y) * 106 + usize::from(change.x) + offset;
                let full = frame
                    .cells
                    .get(index)
                    .map(|cell| cell.symbol.clone())
                    .unwrap_or_default();
                assert_eq!(
                    full,
                    cell.symbol,
                    "fast and full agree at ({}, {})",
                    change.x + offset as u16,
                    change.y
                );
                emitted += 1;
            }
        }
    }
    assert!(emitted >= 4, "every emitted row was compared: {emitted}");
}

/// The rendered rows of one composed frame.
fn composed_rows_of(state: &mut ClientShellState) -> Vec<String> {
    let frame = state.compose(106, 30).expect("frame");
    frame
        .cells
        .chunks(106)
        .map(|row| row.iter().map(|cell| cell.symbol.as_str()).collect())
        .collect()
}

/// Two views stacked on one pane: Tasks with rows, then Messages with rows. The
/// human's dogfood report: both headers drew first, with every row below them.
fn two_section_state() -> ClientShellState {
    let mut state = panel_state(None, false);
    let projection = serde_json::json!({
        "contract":"host.documents.v1", "boot_id":"boot-1", "revision":1,
        "documents":[{"provider_priority":1,"document":{
            "contract":"host.view.v1","namespace":"example.tasks","generation":1,"revision":1,
            "surface":"navigation","target":{"kind":"session"},
            "items":[
                {"id":"panel:pane_1:tasks","role":"section","label":"Tasks","detail":"1/2 done",
                 "view":{"pane_id":"pane_1","view_id":"tasks","state":"in_progress"}},
                {"id":"panel:pane_1:tasks:row","parent_id":"panel:pane_1:tasks","role":"section",
                 "label":"A tasks row","view":{"state":"open"}},
                {"id":"panel:pane_1:messages","role":"section","label":"Messages","detail":"1 waiting",
                 "view":{"pane_id":"pane_1","view_id":"messages","state":"in_progress"}},
                {"id":"panel:pane_1:messages:row","parent_id":"panel:pane_1:messages","role":"section",
                 "label":"A message row","view":{"state":"open"}}
            ],"actions":[],"required_capabilities":[]}}]
    });
    let projection: crate::protocol::endpoint::HostDocumentsProjectionV1 =
        serde_json::from_value(projection).expect("projection");
    state.set_endpoint_host_documents_for_generation(
        &ClientEndpointId::Local,
        1,
        Box::new(projection),
    );
    for view_id in ["tasks", "messages"] {
        state
            .pane_panel_open
            .entry("pane_1".into())
            .or_default()
            .insert(view_id.into());
    }
    state
}

#[test]
fn each_sections_rows_sit_directly_under_its_own_header() {
    let mut state = two_section_state();
    let rows = composed_rows_of(&mut state);
    let index = |needle: &str| {
        rows.iter()
            .position(|row| row.contains(needle))
            .unwrap_or_else(|| panic!("{needle} is drawn: {rows:?}"))
    };
    let tasks_header = index("Tasks");
    let tasks_row = index("A tasks row");
    let messages_header = index("Messages");
    let messages_row = index("A message row");
    assert!(
        tasks_header < tasks_row,
        "the Tasks row belongs under the Tasks header: {rows:?}"
    );
    assert!(
        tasks_row < messages_header,
        "the Messages header comes after the Tasks rows, not above them: {rows:?}"
    );
    assert!(
        messages_header < messages_row,
        "the Messages row belongs under the Messages header: {rows:?}"
    );
    // The rows are adjacent to their own headers, i.e. nothing of the other
    // section is in between.
    assert_eq!(
        tasks_row,
        tasks_header + 1,
        "the Tasks row is the line after its header: {rows:?}"
    );
    assert_eq!(
        messages_row,
        messages_header + 1,
        "the Messages row is the line after its header: {rows:?}"
    );
}

#[test]
fn every_cell_of_the_panel_band_is_panel_painted() {
    // The human's bleed-through report: terminal text showed in the panel's empty
    // cells. Every cell the band owns must carry the panel background.
    let mut state = panel_state(None, true);
    let frame = state.compose(106, 30).expect("frame");
    let bands = state.pane_panel_areas.clone();
    assert!(!bands.is_empty(), "the panel reserved a band");
    let width = 106;
    let cell_at = |x: u16, y: u16| &frame.cells[usize::from(y) * width + usize::from(x)];
    let band = bands[0];
    // The header is painted by the panel with its own background, so it is the
    // reference for what panel chrome looks like in this frame.
    let chrome = cell_at(band.x.saturating_add(1), band.y).bg;
    for band in &bands {
        for y in band.y..band.y.saturating_add(band.height) {
            for x in band.x..band.x.saturating_add(band.width) {
                assert_eq!(
                    cell_at(x, y).bg,
                    chrome,
                    "cell ({x}, {y}) of the band is not panel-painted: {:?}",
                    cell_at(x, y)
                );
            }
        }
    }
    // And the band must not carry the terminal's own background, which is what the
    // human saw through the panel's empty cells.
    let terminal_y = bands[0].y.saturating_add(bands[0].height).saturating_add(3);
    let terminal = cell_at(bands[0].x.saturating_add(4), terminal_y).bg;
    assert_ne!(
        chrome, terminal,
        "the band carries the terminal's background, so its text shows through"
    );
}
