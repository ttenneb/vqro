use super::*;

fn key(code: KeyCode) -> RawInputEvent {
    RawInputEvent::Key(crate::input::TerminalKey::new(code, KeyModifiers::empty()))
}

#[test]
fn startup_codec_refusal_notice_is_client_local_and_survives_typing() {
    // The startup codec refusal is classified as endpoint attention
    // (`client::endpoint::supervisor::tests::codec_handshake_refusal_is_client_local_attention_while_network_failures_retry`).
    // The attention route presents it through `receive_endpoint_unavailable`, so
    // this covers that display and its persistence on the exact route: it is a
    // client-local notice, and typing in a pane does not dismiss it. (A supported
    // peer is unaffected; the real-predecessor probe proves that server side.)
    let mut state = state_with_two_terminals();
    let message = "client and server have no compatible endpoint core codecs";
    assert!(state.receive_endpoint_unavailable(message.to_owned()));
    let notice = state
        .visible_endpoint_notice
        .as_ref()
        .expect("codec refusal notice is shown");
    assert_eq!(
        notice.key.kind,
        super::super::state::ClientEndpointNoticeKind::Unavailable
    );
    assert_eq!(notice.title, "Endpoint unavailable");
    assert_eq!(notice.body, message);

    state.handle_raw_events(vec![key(KeyCode::Char('a'))]);
    assert!(
        state.visible_endpoint_notice.is_some(),
        "typing in a pane must not dismiss the client-local notice"
    );
}

fn state_recovery_row(
    state: &ClientShellState,
    index: usize,
) -> Option<&super::super::recovery_browser::RecoveryRow> {
    let Some(ClientShellOverlay::Recovery(recovery)) = state.overlay.as_ref() else {
        return None;
    };
    recovery.rows.get(index)
}

fn state_with_two_terminals() -> ClientShellState {
    let mut projected = snapshot();
    projected.panes.push(ClientShellPane {
        pane_id: "pane_2".into(),
        workspace_id: "ws_1".into(),
        tab_id: "tab_1".into(),
        label: Some("second terminal".into()),
        cwd: Some("/repo".into()),
        foreground_cwd: Some("/repo".into()),
        focused: false,
        right_click_passthrough: false,
    });
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(projected));
    state.set_pane_surface(surface());
    state
}

fn state_with_recovered_terminal() -> ClientShellState {
    let mut projected = snapshot();
    projected.panes[0].workspace_id = "missing-workspace".into();
    projected.panes[0].tab_id = "missing-tab".into();
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(projected));
    state.set_pane_surface(surface());
    state.open_recovery_browser();
    state.compose(80, 20).expect("recovery frame");
    state
}

fn assert_disabled_recovery_row_click_is_inert(
    mut state: ClientShellState,
    id: super::super::recovery_browser::RecoveryRowId,
) {
    let endpoint_id = state.active_endpoint_id.clone();
    let selected = state
        .recovery_presentations
        .get(&endpoint_id)
        .and_then(|presentation| presentation.selected.clone());
    let hit = state
        .hits
        .recovery_rows
        .iter()
        .find(|hit| state_recovery_row(&state, hit.row_index).is_some_and(|row| row.id == id))
        .expect("disabled structural row hit")
        .clone();

    let outcome = state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: hit.rect.x.saturating_add(1),
        row: hit.rect.y,
        modifiers: KeyModifiers::empty(),
    })]);

    assert!(outcome.actions.is_empty());
    assert!(!outcome.repaint);
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::Recovery(_))
    ));
    assert_eq!(
        state
            .recovery_presentations
            .get(&endpoint_id)
            .and_then(|presentation| presentation.selected.clone()),
        selected
    );
}

#[test]
fn navigator_rows_keep_snapshot_order_across_many_workspaces_tabs_and_panes() {
    // Endpoint-local indexes must not reorder anything: workspaces, tabs and
    // panes keep snapshot order, and each tab still reports its own pane count.
    let mut projected = snapshot();
    projected.workspaces.clear();
    projected.tabs.clear();
    projected.panes.clear();
    projected.agents.clear();
    for workspace_index in 1..=3 {
        let workspace_id = format!("ws_{workspace_index}");
        projected.workspaces.push(ClientShellWorkspace {
            workspace_id: workspace_id.clone(),
            active_tab_id: format!("tab_{workspace_index}_1"),
            new_workspace_cwd: "/repo".into(),
            number: workspace_index,
            label: format!("workspace {workspace_index}"),
            custom_label: false,
            branch: None,
            git_ahead_behind: None,
            tokens: Vec::new(),
            worktree: None,
            focused: workspace_index == 1,
            agent_status: AgentStatus::Idle,
        });
        for tab_index in 1..=2 {
            let tab_id = format!("tab_{workspace_index}_{tab_index}");
            projected.tabs.push(ClientShellTab {
                tab_id: tab_id.clone(),
                workspace_id: workspace_id.clone(),
                number: tab_index,
                label: format!("tab {tab_index}"),
                custom_label: false,
                zoomed: false,
                focused: false,
                agent_status: AgentStatus::Idle,
            });
            for pane_index in 1..=3 {
                projected.panes.push(ClientShellPane {
                    pane_id: format!("pane_{workspace_index}_{tab_index}_{pane_index}"),
                    workspace_id: workspace_id.clone(),
                    tab_id: tab_id.clone(),
                    label: None,
                    cwd: Some("/repo".into()),
                    foreground_cwd: None,
                    focused: false,
                    right_click_passthrough: false,
                });
            }
        }
    }
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(projected));
    state.open_navigator_overlay();
    let ClientShellOverlay::Navigator(navigator) = state.overlay.as_ref().expect("navigator")
    else {
        panic!("expected navigator");
    };
    let rows =
        render::client_navigator_rows(&state.endpoints, &state.active_endpoint_id, navigator);
    let labels = rows
        .iter()
        .map(|row| row.label.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        labels,
        vec![
            "workspace 1",
            "tab 1",
            "pane 1",
            "pane 2",
            "pane 3",
            "tab 2",
            "pane 1",
            "pane 2",
            "pane 3",
            "workspace 2",
            "tab 1",
            "pane 1",
            "pane 2",
            "pane 3",
            "tab 2",
            "pane 1",
            "pane 2",
            "pane 3",
            "workspace 3",
            "tab 1",
            "pane 1",
            "pane 2",
            "pane 3",
            "tab 2",
            "pane 1",
            "pane 2",
            "pane 3",
        ]
    );
    for row in rows.iter().filter(|row| row.label.starts_with("tab ")) {
        assert_eq!(row.meta, "3 panes", "each tab counts only its own panes");
    }
}

#[test]
fn existing_feature_rich_navigator_rows_remain_unchanged() {
    let mut projected = snapshot();
    projected.agents.push(ClientShellAgent {
        pane_id: "pane_1".into(),
        workspace_id: "ws_1".into(),
        tab_id: "tab_1".into(),
        name: Some("agent label".into()),
        display_agent: None,
        agent: None,
        title: None,
        terminal_title: None,
        terminal_title_stripped: None,
        agent_status: AgentStatus::Blocked,
        state_change_seq: 1,
        state_labels: Vec::new(),
        tokens: Vec::new(),
        focused: true,
    });
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(projected));
    state.open_navigator_overlay();
    let ClientShellOverlay::Navigator(navigator) = state.overlay.as_ref().expect("navigator")
    else {
        panic!("expected navigator");
    };
    let rows =
        render::client_navigator_rows(&state.endpoints, &state.active_endpoint_id, navigator);
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].label, "client-shell");
    assert_eq!(rows[1].label, "1");
    assert_eq!(rows[2].label, "agent label");
    assert_eq!(rows[2].status, Some(AgentStatus::Blocked));
    assert!(matches!(rows[2].target, ClientNavigatorTarget::Pane { .. }));
}

#[test]
fn recovery_is_globally_discoverable_and_keyboard_focuses_a_terminal() {
    let mut state = state_with_two_terminals();
    let items = super::super::global_menu::global_menu_items(state.snapshot.as_deref().unwrap());
    let recovery_index = items
        .iter()
        .position(|(label, _)| *label == "terminals")
        .expect("recovery menu item");
    state.overlay = Some(ClientShellOverlay::GlobalMenu(ClientGlobalMenuOverlay {
        highlighted: recovery_index,
    }));
    let opened = state.handle_raw_events(vec![key(KeyCode::Enter)]);
    assert!(opened.repaint);
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::Recovery(_))
    ));

    state.move_recovery_selection(1);
    state.move_recovery_selection(1);
    state.move_recovery_selection(1);
    let focused = state.handle_raw_events(vec![key(KeyCode::Enter)]);
    let [ClientShellAction::Endpoint { request, .. }] = &focused.actions[..] else {
        panic!("terminal focus should use the endpoint API");
    };
    assert!(matches!(
        &request.method,
        crate::api::schema::Method::PaneFocus(crate::api::schema::PaneTarget { pane_id })
            if pane_id == "pane_2"
    ));
    assert!(state.overlay.is_none());
}

#[test]
fn recovery_mouse_and_keyboard_use_the_same_focus_action() {
    let mut keyboard = state_with_two_terminals();
    keyboard.open_recovery_browser();
    keyboard.move_recovery_selection(1);
    keyboard.move_recovery_selection(1);
    keyboard.move_recovery_selection(1);
    let keyboard = keyboard.handle_raw_events(vec![key(KeyCode::Enter)]);

    let mut mouse = state_with_two_terminals();
    mouse.open_recovery_browser();
    mouse.compose(106, 30).expect("recovery frame");
    let terminal = mouse
        .hits
        .recovery_rows
        .iter()
        .find(|hit| {
            matches!(
                state_recovery_row(&mouse, hit.row_index).map(|row| &row.id),
                Some(super::super::recovery_browser::RecoveryRowId::Terminal(pane_id))
                    if pane_id == "pane_2"
            )
        })
        .expect("pane row")
        .clone();
    let mouse = mouse.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: terminal.rect.x + 1,
        row: terminal.rect.y,
        modifiers: KeyModifiers::empty(),
    })]);

    let method = |outcome: &ClientShellInput| match &outcome.actions[..] {
        [ClientShellAction::Endpoint { request, .. }] => {
            crate::api::api_method_name(&request.method)
        }
        _ => "",
    };
    assert_eq!(method(&keyboard), "pane.focus");
    assert_eq!(method(&mouse), "pane.focus");
}

#[test]
fn recovery_close_confirmation_is_exact_snapshot_and_connection_fenced() {
    let mut state = state_with_two_terminals();
    state.open_recovery_browser();
    state.move_recovery_selection(1);
    state.move_recovery_selection(1);
    state.move_recovery_selection(1);
    state.handle_raw_events(vec![key(KeyCode::Char('x'))]);
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::ConfirmClose(
            ClientConfirmCloseOverlay {
                target: ClientConfirmCloseTarget::RecoveryPane(_),
                ..
            }
        ))
    ));

    let mut changed = state.snapshot.as_deref().unwrap().clone();
    changed.revision += 1;
    state.set_snapshot(Box::new(changed));
    let stale = state.handle_raw_events(vec![key(KeyCode::Enter)]);
    assert!(stale.actions.is_empty());
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::Recovery(_))
    ));
    assert_eq!(
        state
            .visible_endpoint_notice
            .as_ref()
            .map(|notice| notice.title.as_str()),
        Some("Terminal changed")
    );

    state.handle_raw_events(vec![key(KeyCode::Char('x'))]);
    let close = state.handle_raw_events(vec![key(KeyCode::Enter)]);
    let [ClientShellAction::Endpoint { request, .. }] = &close.actions[..] else {
        panic!("current confirmation should close through endpoint API");
    };
    assert!(matches!(
        &request.method,
        crate::api::schema::Method::PaneClose(crate::api::schema::PaneTarget { pane_id })
            if pane_id == "pane_2"
    ));
}

#[test]
fn recovery_focus_leaves_terminal_input_routing_unchanged() {
    let mut state = state_with_two_terminals();
    state.open_recovery_browser();
    state.move_recovery_selection(1);
    state.move_recovery_selection(1);
    state.move_recovery_selection(1);
    state.handle_raw_events(vec![key(KeyCode::Enter)]);

    let mut focused = state.snapshot.as_deref().unwrap().clone();
    focused.focused_pane_id = Some("pane_2".into());
    for pane in &mut focused.panes {
        pane.focused = pane.pane_id == "pane_2";
    }
    state.set_snapshot(Box::new(focused));
    let mut pane_surface = surface();
    pane_surface.panes[0].pane_id = "pane_2".into();
    pane_surface.panes[0].focused = true;
    state.set_pane_surface(pane_surface);

    let input = state.handle_raw_events(vec![key(KeyCode::Char('a'))]);
    assert!(matches!(
        input.requests.as_slice(),
        [crate::protocol::ClientMessage::ClientShellPaneInput { pane_id, .. }]
            if pane_id == "pane_2"
    ));

    assert!(matches!(
        state.endpoint_method_for_action(crate::input::KeybindAction::ResizePaneRight),
        Some(crate::api::schema::Method::PaneResize(
            crate::api::schema::PaneResizeParams { pane_id: Some(pane_id), .. }
        )) if pane_id == "pane_2"
    ));
    let mut scroll = ClientShellInput::default();
    state.push_pane_scroll_offset("pane_2".into(), 3, &mut scroll);
    assert!(matches!(
        scroll.actions.as_slice(),
        [ClientShellAction::Endpoint { request, .. }]
            if matches!(&request.method, crate::api::schema::Method::PaneScroll(params)
                if params.pane_id == "pane_2")
    ));
    assert!(
        state.copy_mode.is_none(),
        "recovery focus must not enter or alter copy mode"
    );
}

#[test]
fn recovery_binds_focus_and_close_to_overlay_endpoint_with_reused_pane_ids() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    let profile =
        SavedSshEndpoint::new("Build", "dev@build.example", "agents").expect("remote profile");
    let remote_id = ClientEndpointId::Ssh(profile.id.clone());
    state.set_endpoint_catalog(&[profile]);
    state.set_endpoint_status(&remote_id, ClientEndpointStatus::Online);
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    let mut remote = snapshot();
    remote.boot_id = "remote-boot".into();
    remote.revision = 9;
    remote.panes[0].label = Some("remote reused pane".into());
    state.set_endpoint_snapshot_for_generation(&remote_id, 7, Box::new(remote));

    state.open_recovery_browser_for(&remote_id);
    let frame = state.compose(80, 20).expect("remote recovery frame");
    let text = frame
        .cells
        .iter()
        .map(|cell| cell.symbol.as_str())
        .collect::<String>();
    assert!(text.contains("remote reused pane"));
    let focus = state.handle_raw_events(vec![key(KeyCode::Enter)]);
    assert!(matches!(
        focus.actions.as_slice(),
        [ClientShellAction::ActivateEndpoint { endpoint_id, target: Some(ClientEndpointFocusTarget::Pane(pane_id)) }]
            if endpoint_id == &remote_id && pane_id == "pane_1"
    ));

    state.open_recovery_browser_for(&remote_id);
    state.handle_raw_events(vec![key(KeyCode::Char('x'))]);
    let close = state.handle_raw_events(vec![key(KeyCode::Enter)]);
    assert!(matches!(
        close.actions.as_slice(),
        [ClientShellAction::Endpoint { endpoint_id, boot_id, request }]
            if endpoint_id == &remote_id
                && boot_id == "remote-boot"
                && matches!(&request.method, crate::api::schema::Method::PaneClose(
                    crate::api::schema::PaneTarget { pane_id }
                ) if pane_id == "pane_1")
    ));
}

#[test]
fn recovery_rebuilds_rows_when_its_endpoint_snapshot_changes() {
    let mut state = state_with_two_terminals();
    state.open_recovery_browser();
    state.compose(80, 20).expect("initial recovery frame");
    let stale_hit = state
        .hits
        .recovery_rows
        .iter()
        .find(|hit| {
            matches!(
                state_recovery_row(&state, hit.row_index).map(|row| &row.id),
                Some(super::super::recovery_browser::RecoveryRowId::Terminal(pane_id))
                    if pane_id == "pane_2"
            )
        })
        .expect("old pane hit")
        .clone();

    let mut changed = state.snapshot.as_deref().unwrap().clone();
    changed.revision += 1;
    changed.panes.retain(|pane| pane.pane_id != "pane_2");
    changed.panes.push(ClientShellPane {
        pane_id: "pane_3".into(),
        workspace_id: "ws_1".into(),
        tab_id: "tab_1".into(),
        label: Some("replacement".into()),
        cwd: None,
        foreground_cwd: None,
        focused: false,
        right_click_passthrough: false,
    });
    state.set_snapshot(Box::new(changed));
    let Some(ClientShellOverlay::Recovery(recovery)) = state.overlay.as_ref() else {
        panic!("recovery remains open");
    };
    assert_eq!(recovery.snapshot_revision, 2);
    assert!(recovery.rows.iter().any(|row| matches!(
        &row.id,
        super::super::recovery_browser::RecoveryRowId::Terminal(id) if id == "pane_3"
    )));
    assert!(!recovery.rows.iter().any(|row| matches!(
        &row.id,
        super::super::recovery_browser::RecoveryRowId::Terminal(id) if id == "pane_2"
    )));

    let stale_click = state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: stale_hit.rect.x,
        row: stale_hit.rect.y,
        modifiers: KeyModifiers::empty(),
    })]);
    assert!(stale_click.actions.is_empty());
    assert!(stale_click.repaint);
}

#[test]
fn recovery_focus_rejects_stale_projection_and_notice_survives_typing() {
    let mut state = state_with_two_terminals();
    state.open_recovery_browser();
    state.move_recovery_selection(1);
    if let Some(endpoint) = state
        .endpoints
        .iter_mut()
        .find(|endpoint| endpoint.endpoint_id == state.active_endpoint_id)
    {
        endpoint.snapshot.as_mut().expect("snapshot").revision += 1;
    }
    let stale = state.handle_raw_events(vec![key(KeyCode::Enter)]);
    assert!(stale.actions.is_empty());
    assert_eq!(
        state
            .visible_endpoint_notice
            .as_ref()
            .map(|notice| notice.title.as_str()),
        Some("Terminal changed")
    );
    state.handle_raw_events(vec![key(KeyCode::Esc)]);
    let notice_key = state
        .visible_endpoint_notice
        .as_ref()
        .map(|notice| notice.key.clone());
    state.handle_raw_events(vec![key(KeyCode::Char('a'))]);
    assert_eq!(
        state
            .visible_endpoint_notice
            .as_ref()
            .map(|notice| &notice.key),
        notice_key.as_ref()
    );
}

#[test]
fn recovery_preserves_snapshot_workspace_tab_and_pane_order() {
    let mut projected = snapshot();
    projected.workspaces.insert(
        0,
        ClientShellWorkspace {
            workspace_id: "ws_2".into(),
            active_tab_id: "tab_2".into(),
            new_workspace_cwd: String::new(),
            number: 99,
            label: "second first".into(),
            custom_label: true,
            branch: None,
            git_ahead_behind: None,
            tokens: Vec::new(),
            worktree: None,
            focused: false,
            agent_status: AgentStatus::Unknown,
        },
    );
    projected.tabs.insert(
        0,
        ClientShellTab {
            tab_id: "tab_2".into(),
            workspace_id: "ws_2".into(),
            number: 99,
            label: "tab second first".into(),
            custom_label: true,
            zoomed: false,
            focused: false,
            agent_status: AgentStatus::Unknown,
        },
    );
    projected.panes = vec![
        ClientShellPane {
            pane_id: "pane_z".into(),
            workspace_id: "ws_2".into(),
            tab_id: "tab_2".into(),
            label: None,
            cwd: None,
            foreground_cwd: None,
            focused: false,
            right_click_passthrough: false,
        },
        ClientShellPane {
            pane_id: "pane_a".into(),
            workspace_id: "ws_2".into(),
            tab_id: "tab_2".into(),
            label: None,
            cwd: None,
            foreground_cwd: None,
            focused: false,
            right_click_passthrough: false,
        },
    ];
    let rows = super::super::recovery_browser::normalized_rows(&projected);
    let ids = rows.iter().map(|row| row.id.clone()).collect::<Vec<_>>();
    assert_eq!(
        ids[..4],
        [
            super::super::recovery_browser::RecoveryRowId::Workspace("ws_2".into()),
            super::super::recovery_browser::RecoveryRowId::Tab("tab_2".into()),
            super::super::recovery_browser::RecoveryRowId::Terminal("pane_z".into()),
            super::super::recovery_browser::RecoveryRowId::Terminal("pane_a".into()),
        ]
    );
}

#[test]
fn recovered_workspace_row_is_not_mouse_actionable() {
    assert_disabled_recovery_row_click_is_inert(
        state_with_recovered_terminal(),
        super::super::recovery_browser::RecoveryRowId::RecoveredWorkspace,
    );
}

#[test]
fn recovered_tab_row_is_not_mouse_actionable() {
    assert_disabled_recovery_row_click_is_inert(
        state_with_recovered_terminal(),
        super::super::recovery_browser::RecoveryRowId::RecoveredTab,
    );
}

#[test]
fn synthetic_recovery_ids_cannot_collide_with_real_snapshot_ids() {
    let mut projected = snapshot();
    projected.workspaces[0].workspace_id = "host.recovery.workspace".into();
    projected.tabs[0].workspace_id = "host.recovery.workspace".into();
    projected.tabs[0].tab_id = "host.recovery.tab".into();
    projected.panes[0].workspace_id = "missing".into();
    projected.panes[0].tab_id = "missing".into();
    let rows = super::super::recovery_browser::normalized_rows(&projected);
    assert!(rows.iter().any(|row| row.id
        == super::super::recovery_browser::RecoveryRowId::Workspace(
            "host.recovery.workspace".into()
        )));
    assert!(rows
        .iter()
        .any(|row| row.id == super::super::recovery_browser::RecoveryRowId::RecoveredWorkspace));
    assert!(rows
        .iter()
        .any(|row| row.id == super::super::recovery_browser::RecoveryRowId::RecoveredTab));
}

#[test]
fn recovery_mouse_close_cancel_and_confirm_keep_exact_target() {
    let mut state = state_with_two_terminals();
    state.open_recovery_browser();
    state.compose(80, 20).expect("recovery frame");
    let close = state
        .hits
        .recovery_rows
        .iter()
        .find(|hit| !hit.close.is_empty())
        .expect("close hit")
        .close;
    state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: close.x,
        row: close.y,
        modifiers: KeyModifiers::empty(),
    })]);
    state.compose(80, 20).expect("confirmation frame");
    let cancel = state.hits.overlay_cancel;
    let cancelled = state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: cancel.x,
        row: cancel.y,
        modifiers: KeyModifiers::empty(),
    })]);
    assert!(cancelled.actions.is_empty());
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::Recovery(_))
    ));

    state.compose(80, 20).expect("recovery frame again");
    let close = state
        .hits
        .recovery_rows
        .iter()
        .find(|hit| !hit.close.is_empty())
        .expect("close hit")
        .close;
    state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: close.x,
        row: close.y,
        modifiers: KeyModifiers::empty(),
    })]);
    state.compose(80, 20).expect("confirmation frame again");
    let confirm = state.hits.overlay_primary;
    let confirmed = state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: confirm.x,
        row: confirm.y,
        modifiers: KeyModifiers::empty(),
    })]);
    assert!(matches!(
        confirmed.actions.as_slice(),
        [ClientShellAction::Endpoint { request, .. }]
            if matches!(request.method, crate::api::schema::Method::PaneClose(_))
    ));
}

#[test]
fn recovery_overlay_clips_safely_at_narrow_geometry() {
    let mut state = state_with_two_terminals();
    state.open_recovery_browser();
    let frame = state.compose(8, 4).expect("narrow recovery frame");
    assert_eq!((frame.width, frame.height), (8, 4));
    let bounds = Rect::new(0, 0, 8, 4);
    assert_eq!(
        state.hits.recovery_popup.intersection(bounds),
        state.hits.recovery_popup
    );
    for hit in &state.hits.recovery_rows {
        assert!(hit.rect.intersection(bounds) == hit.rect);
        assert!(hit.close.is_empty() || hit.close.intersection(bounds) == hit.close);
    }
}
