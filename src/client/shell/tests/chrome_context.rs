use super::*;

fn frame_symbol(frame: &FrameData, x: u16, y: u16) -> &str {
    &frame.cells[usize::from(y) * usize::from(frame.width) + usize::from(x)].symbol
}

fn assert_rect_within(rect: Rect, bounds: Rect) {
    assert!(
        rect.x >= bounds.x
            && rect.y >= bounds.y
            && rect.right() <= bounds.right()
            && rect.bottom() <= bounds.bottom(),
        "hit rect {rect:?} escaped frame {bounds:?}"
    );
}

fn assert_shell_hits_within_frame(state: &ClientShellState, frame: &FrameData) {
    assert_shell_hits_within_bounds(state, Rect::new(0, 0, frame.width, frame.height));
}

fn assert_shell_hits_within_bounds(state: &ClientShellState, bounds: Rect) {
    for rect in [
        state.hits.workspace_body,
        state.hits.workspace_scrollbar,
        state.hits.agent_body,
        state.hits.agent_scrollbar,
        state.hits.agent_sort_toggle,
        state.hits.sidebar_divider,
        state.hits.sidebar_section_divider,
        state.hits.sidebar_toggle,
        state.hits.new_workspace,
        state.hits.new_tab,
        state.hits.tab_scroll_left,
        state.hits.tab_scroll_right,
        state.hits.mobile_switch,
        state.hits.mobile_close,
        state.hits.global_launcher,
        state.hits.notification_toast,
        state.hits.overlay_primary,
        state.hits.overlay_clear,
        state.hits.overlay_cancel,
        state.hits.inspector_body,
        state.hits.inspector_close,
        state.hits.inspector_refresh,
        state.hits.navigator_popup,
        state.hits.navigator_search,
        state.hits.recovery_popup,
        state.hits.worktree_search,
        state.hits.help_popup,
        state.hits.help_scrollbar,
        state.hits.settings_popup,
        state.hits.product_announcement_scrollbar,
        state.hits.release_notes_scrollbar,
    ] {
        assert_rect_within(rect, bounds);
    }
    for workspace in &state.hits.workspaces {
        if let Some((rect, _)) = &workspace.group_toggle {
            assert_rect_within(*rect, bounds);
        }
    }
    for hit in &state.hits.panes {
        assert_rect_within(hit.rect, bounds);
        assert_rect_within(hit.inner_rect, bounds);
        assert_rect_within(hit.projected_inner_rect, bounds);
        if let Some(rect) = hit.scrollbar_rect {
            assert_rect_within(rect, bounds);
        }
    }
    if let Some(hit) = &state.hits.popup {
        assert_rect_within(hit.rect, bounds);
        assert_rect_within(hit.inner_rect, bounds);
        assert_rect_within(hit.projected_inner_rect, bounds);
        if let Some(rect) = hit.scrollbar_rect {
            assert_rect_within(rect, bounds);
        }
    }
    for hit in &state.hits.collections {
        for rect in [
            hit.rect,
            hit.previous,
            hit.next,
            hit.presentation_toggle,
            hit.inspect,
        ] {
            assert_rect_within(rect, bounds);
        }
    }
    for hit in &state.hits.collection_members {
        assert_rect_within(hit.rect, bounds);
        assert_rect_within(hit.disclosure, bounds);
        assert_rect_within(hit.preview, bounds);
        assert_rect_within(hit.resize_handle, bounds);
        if let Some(scrollbar) = hit.scrollbar {
            assert_rect_within(scrollbar, bounds);
            assert_eq!(scrollbar.height, hit.preview.height);
        }
    }
    for split in &state.hits.pane_splits {
        assert_rect_within(split.area, bounds);
        assert_rect_within(split.hit_rect, bounds);
    }
    for rect in state
        .hits
        .machines
        .iter()
        .map(|hit| hit.rect)
        .chain(state.hits.workspaces.iter().map(|hit| hit.rect))
        .chain(state.hits.space_links.iter().map(|hit| hit.rect))
        .chain(state.hits.tabs.iter().map(|(rect, _)| *rect))
        .chain(state.hits.agents.iter().map(|(rect, _)| *rect))
        .chain(state.hits.endpoint_agents.iter().map(|(rect, _, _)| *rect))
        .chain(state.hits.mobile_targets.iter().map(|(rect, _)| *rect))
        .chain(state.hits.global_menu_rows.iter().map(|(rect, _)| *rect))
        .chain(state.hits.context_menu_rows.iter().map(|(rect, _)| *rect))
        .chain(
            state
                .hits
                .contribution_actions
                .iter()
                .map(|(rect, _)| *rect),
        )
        .chain(state.hits.navigator_rows.iter().map(|(rect, _)| *rect))
        .chain(
            state
                .hits
                .recovery_rows
                .iter()
                .flat_map(|hit| [hit.rect, hit.close]),
        )
        .chain(state.hits.worktree_rows.iter().map(|(rect, _)| *rect))
        .chain(state.hits.settings_tabs.iter().map(|(rect, _)| *rect))
        .chain(state.hits.settings_choices.iter().map(|(rect, _)| *rect))
    {
        assert_rect_within(rect, bounds);
    }
}

fn capture_review_frame(name: &str, frame: &FrameData) {
    let Ok(directory) = std::env::var("VQRO_VISUAL_CAPTURE_DIR") else {
        return;
    };
    let directory = std::path::Path::new(&directory);
    std::fs::create_dir_all(directory).unwrap();
    let text = frame
        .cells
        .chunks(frame.width as usize)
        .map(|row| {
            row.iter()
                .map(|cell| cell.symbol.as_str())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(directory.join(format!("{name}.txt")), text).unwrap();
    let encoder = crate::protocol::render_ansi::BlitEncoder::new();
    let ansi = encoder.encode(frame, true);
    std::fs::write(directory.join(format!("{name}.ansi")), ansi.bytes).unwrap();
}

fn assert_no_nested_full_border(frame: &FrameData, area: Rect) {
    if area.width < 2 || area.height < 2 {
        return;
    }
    let complete = frame_symbol(frame, area.x, area.y) == "┌"
        && frame_symbol(frame, area.right() - 1, area.y) == "┐"
        && frame_symbol(frame, area.x, area.bottom() - 1) == "└"
        && frame_symbol(frame, area.right() - 1, area.bottom() - 1) == "┘"
        && (area.x + 1..area.right() - 1).all(|x| frame_symbol(frame, x, area.y) == "─")
        && (area.x + 1..area.right() - 1).all(|x| frame_symbol(frame, x, area.bottom() - 1) == "─")
        && (area.y + 1..area.bottom() - 1).all(|y| frame_symbol(frame, area.x, y) == "│")
        && (area.y + 1..area.bottom() - 1).all(|y| frame_symbol(frame, area.right() - 1, y) == "│");
    assert!(!complete, "contribution rendered as a nested window");
}

#[test]
fn tab_overflow_controls_scroll_the_client_owned_tab_bar() {
    let mut snapshot = snapshot();
    snapshot.tabs.extend((2..=8).map(|number| ClientShellTab {
        tab_id: format!("tab_{number}"),
        workspace_id: "ws_1".into(),
        number,
        label: number.to_string(),
        custom_label: false,
        zoomed: false,
        focused: false,
        agent_status: AgentStatus::Idle,
    }));
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot));
    state.set_pane_surface(surface());
    state.compose(80, 20).expect("overflow tab bar");

    assert!(state.hits.tab_scroll_right.width > 0);
    let scroll_right = state.hits.tab_scroll_right;
    let outcome =
        state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: scroll_right.x + 1,
            row: scroll_right.y,
            modifiers: KeyModifiers::empty(),
        })]);
    assert!(outcome.repaint);
    assert_eq!(state.tab_scroll, 1);

    let mut update = state.snapshot.as_deref().expect("snapshot").clone();
    update.focused_tab_id = Some("tab_8".into());
    for tab in &mut update.tabs {
        tab.focused = tab.tab_id == "tab_8";
    }
    state.set_snapshot(Box::new(update));
    state.compose(80, 20).expect("focused overflow tab");
    assert!(state.hits.tabs.iter().any(|(_, tab_id)| tab_id == "tab_8"));

    state.compose(300, 20).expect("tabs without overflow");
    assert_eq!(state.tab_scroll, 0);
    assert_eq!(state.hits.tabs.len(), 8);
    state.compose(80, 20).expect("focused tab after narrowing");
    assert!(state.hits.tabs.iter().any(|(_, tab_id)| tab_id == "tab_8"));
}

#[test]
fn focused_workspace_change_reveals_new_workspace_in_full_sidebar() {
    let mut initial = snapshot();
    let template = initial.workspaces[0].clone();
    initial.workspaces = (1..=12)
        .map(|number| ClientShellWorkspace {
            workspace_id: format!("ws_{number}"),
            number,
            label: format!("space-{number}"),
            branch: None,
            focused: number == 1,
            ..template.clone()
        })
        .collect();

    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(initial));
    state.set_pane_surface(surface());
    state.compose(106, 20).expect("full sidebar");
    assert!(state.hits.workspace_max_scroll > 0);
    assert!(state
        .hits
        .workspaces
        .iter()
        .all(|hit| hit.workspace_id != "ws_12"));

    let mut update = state.snapshot.as_deref().expect("snapshot").clone();
    update.revision = 2;
    update.focused_workspace_id = Some("ws_12".into());
    for workspace in &mut update.workspaces {
        workspace.focused = workspace.workspace_id == "ws_12";
    }
    let mut updated_surface = surface();
    updated_surface.projection_revision = 2;
    state.set_snapshot(Box::new(update));
    state.set_pane_surface(updated_surface);
    state.compose(106, 2).expect("zero-height workspace body");
    assert!(state.reveal_focused_workspace);
    state.compose(106, 20).expect("updated full sidebar");

    assert!(state
        .hits
        .workspaces
        .iter()
        .any(|hit| hit.workspace_id == "ws_12"));
}

#[test]
fn client_owned_sidebar_dividers_resize_live() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    state.compose(106, 30).expect("expanded sidebar");
    assert!(state.hits.machines.is_empty());
    let workspace_body = state.hits.workspace_body;
    let needless_scroll =
        state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: workspace_body.x,
            row: workspace_body.y,
            modifiers: KeyModifiers::empty(),
        })]);
    assert_eq!(state.hits.workspace_max_scroll, 0);
    assert_eq!(state.workspace_scroll, 0);
    assert!(!needless_scroll.repaint);
    let width_divider = state.hits.sidebar_divider;
    state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: width_divider.x,
        row: width_divider.y + 2,
        modifiers: KeyModifiers::empty(),
    })]);
    let resize =
        state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column: 31,
            row: width_divider.y + 2,
            modifiers: KeyModifiers::empty(),
        })]);
    assert_eq!(state.sidebar_width, 32);
    assert!(state.sidebar_width_manual);
    assert!(resize.repaint);
    assert!(resize.resize);
    let waiting_frame = state.compose(106, 30).expect("waiting for resized surface");
    let waiting_text: String = waiting_frame
        .cells
        .iter()
        .map(|cell| cell.symbol.as_str())
        .collect();
    assert!(
        waiting_text.contains(" spaces"),
        "local sidebar must keep spaces while resizing: {waiting_text}"
    );
    assert!(!waiting_text.contains(" machines"));
    assert!(!waiting_text.contains("Select a connected machine"));
    assert!(!waiting_text.contains("LIVE"));
    assert!(waiting_frame.cursor.is_none());
    assert!(state.pane_surface.is_none());
    assert!(state.hits.panes.is_empty());
    assert!(state.hits.pane_splits.is_empty());
    assert!(state.hits.machines.is_empty());
    assert_eq!(state.hits.sidebar_divider.x, 31);
    assert_eq!(state.hits.workspaces[0].workspace_id, "ws_1");

    let next_resize =
        state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column: 32,
            row: width_divider.y + 2,
            modifiers: KeyModifiers::empty(),
        })]);
    assert!(next_resize.resize);
    state.compose(106, 30).expect("continued resize");
    assert_eq!(state.hits.sidebar_divider.x, 32);
    state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Up(MouseButton::Left),
        column: 32,
        row: width_divider.y + 2,
        modifiers: KeyModifiers::empty(),
    })]);
    assert!(state.chrome_drag.is_none());

    state.set_pane_surface(surface());
    let recovered_frame = state.compose(106, 30).expect("resized sidebar");
    let recovered_text: String = recovered_frame
        .cells
        .iter()
        .map(|cell| cell.symbol.as_str())
        .collect();
    assert!(recovered_text.contains(" spaces"));
    assert!(recovered_text.contains("LIVE"));
    assert!(!state.hits.panes.is_empty());
    let section_divider = state.hits.sidebar_section_divider;
    state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: section_divider.x + 2,
        row: section_divider.y,
        modifiers: KeyModifiers::empty(),
    })]);
    let split = state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Drag(MouseButton::Left),
        column: section_divider.x + 2,
        row: 20,
        modifiers: KeyModifiers::empty(),
    })]);
    assert!(state.sidebar_section_split > 0.6);
    assert!(split.repaint);
    assert!(!split.resize);
}

#[test]
fn context_menus_capture_stable_targets_and_route_actions() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    state.compose(106, 20).expect("composed frame");

    let workspace = state.hits.workspaces[0].rect;
    let open_workspace_menu =
        state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Right),
            column: workspace.x + 2,
            row: workspace.y,
            modifiers: KeyModifiers::empty(),
        })]);
    assert!(open_workspace_menu.actions.is_empty());
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::ContextMenu(ClientContextMenuOverlay {
            target: ClientContextMenuTarget::Workspace { ref workspace_id, .. },
            ..
        })) if workspace_id == "ws_1"
    ));
    let workspace_items = match state.overlay.as_ref() {
        Some(ClientShellOverlay::ContextMenu(menu)) => menu.items(),
        _ => panic!("workspace context menu"),
    };
    assert!(workspace_items
        .iter()
        .any(|item| item.action == ClientContextMenuAction::NewWorktree));
    state.compose(106, 20).expect("workspace context menu");
    let rename = state.hits.context_menu_rows[0].0;
    state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: rename.x + 1,
        row: rename.y,
        modifiers: KeyModifiers::empty(),
    })]);
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::Rename(ClientRenameOverlay {
            target: ClientRenameTarget::Workspace { ref workspace_id },
            ..
        })) if workspace_id == "ws_1"
    ));

    state.overlay = None;
    state.compose(106, 20).expect("composed frame");
    let workspace_body = state.hits.workspace_body;
    let blank_workspace_point = (workspace_body.y..workspace_body.bottom())
        .flat_map(|row| (workspace_body.x..workspace_body.right()).map(move |column| (column, row)))
        .find(|(column, row)| {
            !state
                .hits
                .workspaces
                .iter()
                .any(|workspace| super::contains(workspace.rect, (*column, *row)))
        })
        .expect("blank workspace body cell");
    state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Right),
        column: blank_workspace_point.0,
        row: blank_workspace_point.1,
        modifiers: KeyModifiers::empty(),
    })]);
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::ContextMenu(ClientContextMenuOverlay {
            target: ClientContextMenuTarget::Workspace { ref workspace_id, .. },
            ..
        })) if workspace_id == "ws_1"
    ));

    state.overlay = None;
    state.compose(106, 20).expect("composed frame");
    let pane = state.hits.panes[0].rect;
    state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Right),
        column: pane.x + 1,
        row: pane.y,
        modifiers: KeyModifiers::empty(),
    })]);
    state.compose(106, 20).expect("pane context menu");
    let split_index = match state.overlay.as_ref() {
        Some(ClientShellOverlay::ContextMenu(menu)) => menu
            .items()
            .iter()
            .position(|item| item.action == ClientContextMenuAction::SplitRight)
            .expect("split right item"),
        _ => panic!("pane context menu"),
    };
    let split = state.hits.context_menu_rows[split_index].0;
    let outcome =
        state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: split.x + 1,
            row: split.y,
            modifiers: KeyModifiers::empty(),
        })]);
    let [ClientShellAction::Endpoint { request, .. }] = &outcome.actions[..] else {
        panic!("pane split context action should use endpoint API");
    };
    assert!(matches!(
        &request.method,
        crate::api::schema::Method::PaneSplit(params)
            if params.target_pane_id.as_deref() == Some("pane_1")
                && params.direction == crate::api::schema::SplitDirection::Right
    ));
}

#[test]
fn structure_projection_applies_only_to_matching_generation_boot_and_revision() {
    fn structure(
        session_id: &str,
        boot_id: &str,
        revision: u64,
    ) -> Box<crate::protocol::endpoint::ClientShellStructureProjection> {
        Box::new(crate::protocol::endpoint::ClientShellStructureProjection {
            session_id: session_id.into(),
            boot_id: boot_id.into(),
            revision,
            collections: Vec::new(),
            delegations: Vec::new(),
            repositories: Vec::new(),
            space_order: Vec::new(),
        })
    }

    let endpoint = ClientEndpointId::Local;
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_endpoint_structure_for_generation(&endpoint, 7, structure("alpha", "boot-1", 1));
    assert!(
        state.structure.is_none(),
        "structure must wait for its snapshot"
    );

    state.set_endpoint_snapshot_for_generation(&endpoint, 7, Box::new(snapshot()));
    assert_eq!(
        state.structure.as_ref().map(|value| value.revision),
        Some(1)
    );

    state.set_endpoint_structure_for_generation(&endpoint, 7, structure("alpha", "boot-1", 0));
    state.set_endpoint_structure_for_generation(&endpoint, 7, structure("other", "boot-1", 2));
    assert_eq!(
        state.structure.as_ref().map(|value| value.revision),
        Some(1)
    );

    let mut successor = snapshot();
    successor.revision = 2;
    state.set_endpoint_snapshot_for_generation(&endpoint, 7, Box::new(successor));
    assert!(
        state.structure.is_none(),
        "old revision must not cross the snapshot boundary"
    );
    state.set_endpoint_structure_for_generation(&endpoint, 7, structure("alpha", "boot-1", 2));
    assert_eq!(
        state.structure.as_ref().map(|value| value.revision),
        Some(2)
    );

    let mut replacement = snapshot();
    replacement.boot_id = "boot-2".into();
    replacement.revision = 1;
    state.set_endpoint_snapshot_for_generation(&endpoint, 8, Box::new(replacement));
    assert!(
        state.structure.is_none(),
        "old boot/generation must be discarded"
    );
}

#[test]
fn collection_chrome_renders_members_delegation_and_routes_selection() {
    let mut projected = snapshot();
    projected.agents = vec![ClientShellAgent {
        pane_id: "pane_1".into(),
        workspace_id: "ws_1".into(),
        tab_id: "tab_1".into(),
        name: Some("coordinator".into()),
        display_agent: Some("Pi".into()),
        agent: Some("pi".into()),
        title: None,
        terminal_title: None,
        terminal_title_stripped: None,
        agent_status: AgentStatus::Working,
        state_change_seq: 1,
        state_labels: Vec::new(),
        tokens: Vec::new(),
        focused: true,
    }];
    projected.agent_order = vec!["pane_1".into()];
    projected.panes.push(ClientShellPane {
        pane_id: "pane_2".into(),
        workspace_id: "ws_1".into(),
        tab_id: "tab_1".into(),
        label: Some("plain terminal".into()),
        cwd: Some("/repo".into()),
        foreground_cwd: Some("/repo".into()),
        focused: false,
        right_click_passthrough: false,
    });
    projected.panes.push(ClientShellPane {
        pane_id: "pane_3".into(),
        workspace_id: "ws_1".into(),
        tab_id: "tab_1".into(),
        label: Some("parent terminal".into()),
        cwd: Some("/repo".into()),
        foreground_cwd: Some("/repo".into()),
        focused: false,
        right_click_passthrough: false,
    });
    let mut pane_surface = surface();
    pane_surface.panes[0].rect.width = 70;
    pane_surface.panes[0].rect.height = 20;
    pane_surface.panes[0].inner_rect.y = 1;
    pane_surface.panes[0].inner_rect.width = 70;
    pane_surface.panes[0].inner_rect.height = 19;
    pane_surface.collections = vec![crate::protocol::PaneSurfaceCollection {
        collection_id: "collection_1".into(),
        rect: crate::protocol::SurfaceRect {
            x: 0,
            y: 0,
            width: 70,
            height: 20,
        },
        inner_rect: crate::protocol::SurfaceRect {
            x: 0,
            y: 1,
            width: 70,
            height: 19,
        },
        focused: true,
    }];

    let structure = crate::protocol::endpoint::ClientShellStructureProjection {
        session_id: "alpha".into(),
        boot_id: "boot-1".into(),
        revision: 1,
        collections: vec![crate::protocol::endpoint::ClientShellCollection {
            collection_id: "collection_1".into(),
            workspace_id: "ws_1".into(),
            tab_id: "tab_1".into(),
            label: Some("Alpha workflow".into()),
            selected_pane_id: Some("pane_1".into()),
            members: vec![
                crate::protocol::endpoint::ClientShellCollectionMember {
                    pane_id: "pane_1".into(),
                    index: 0,
                    archived: false,
                },
                crate::protocol::endpoint::ClientShellCollectionMember {
                    pane_id: "pane_2".into(),
                    index: 1,
                    archived: false,
                },
            ],
        }],
        delegations: vec![
            crate::protocol::endpoint::ClientShellDelegation {
                delegation_id: "d1".into(),
                pane_id: Some("pane_1".into()),
                parent_id: None,
                purpose: Some("coordinate".into()),
                depth: 0,
                external_parent_id: None,
            },
            crate::protocol::endpoint::ClientShellDelegation {
                delegation_id: "d2".into(),
                pane_id: Some("pane_2".into()),
                parent_id: Some("d1".into()),
                purpose: Some("run tests".into()),
                depth: 1,
                external_parent_id: None,
            },
        ],
        repositories: Vec::new(),
        space_order: Vec::new(),
    };

    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_endpoint_capabilities_for(
        &ClientEndpointId::Local,
        vec![
            crate::protocol::endpoint::PANE_VIEW_CAPABILITY.into(),
            crate::protocol::endpoint::PANE_VIEWPORT_CAPABILITY.into(),
            crate::protocol::endpoint::TERMINAL_VIEWS_CAPABILITY.into(),
        ],
    );
    state.set_snapshot(Box::new(projected));
    state.structure = Some(Box::new(structure));
    state.set_pane_surface(pane_surface);
    super::super::document_navigation::reset_delegation_navigation_call_count();
    let frame = state.compose(106, 24).expect("collection frame");
    assert_eq!(
        super::super::document_navigation::delegation_navigation_call_count(),
        1,
        "delegation documents must be joined once per frame"
    );
    let text = frame
        .cells
        .chunks(frame.width as usize)
        .map(|row| {
            row.iter()
                .map(|cell| cell.symbol.as_str())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("Alpha workflow"), "{text}");
    assert!(text.contains("1/2"), "{text}");
    assert!(text.contains("[ ≡ list ]"), "{text}");
    assert!(text.contains("coordinate"), "{text}");
    let chrome = state.hits.collections[0].clone();
    let header_background = crate::protocol::color_to_u32(ratatui::style::Color::Reset);
    for x in chrome.rect.x..chrome.rect.right() {
        let index = usize::from(chrome.rect.y) * usize::from(frame.width) + usize::from(x);
        assert_eq!(
            frame.cells[index].bg, header_background,
            "the entire collection header must preserve the terminal background"
        );
    }
    let outcome =
        state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: chrome.next.x + 1,
            row: chrome.next.y,
            modifiers: KeyModifiers::empty(),
        })]);
    assert!(matches!(
        outcome.actions.first(),
        Some(ClientShellAction::Endpoint { request, .. })
            if matches!(
                &request.method,
                crate::api::schema::Method::CollectionSelect(params)
                    if params.collection_id == "collection_1" && params.pane_id == "pane_2"
            )
    ));

    let inspect = state.hits.collections[0].inspect;
    let open = state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: inspect.x,
        row: inspect.y,
        modifiers: KeyModifiers::empty(),
    })]);
    assert!(open.actions.iter().any(|action| matches!(
        action,
        ClientShellAction::Endpoint { request, .. }
            if matches!(request.method, crate::api::schema::Method::PaneTelemetry(_))
    )));
    let frame = state
        .compose(106, 24)
        .expect("embedded collection inspector");
    assert!(state.hits.panes[0].inner_rect.bottom() < state.hits.inspector_body.y);
    let text = frame
        .cells
        .chunks(frame.width as usize)
        .map(|row| {
            row.iter()
                .map(|cell| cell.symbol.as_str())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("Alpha workflow"));
    assert!(text.contains("Terminal inspector"));
    assert_no_nested_full_border(&frame, state.hits.inspector_body);

    state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: inspect.x,
        row: inspect.y,
        modifiers: KeyModifiers::empty(),
    })]);
    assert!(
        state.inspector.is_none(),
        "header inspect control toggles closed"
    );

    let presentation_toggle = state.hits.collections[0].presentation_toggle;
    state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: presentation_toggle.x,
        row: presentation_toggle.y,
        modifiers: KeyModifiers::empty(),
    })]);
    assert!(state
        .collection_expanded_members
        .get("collection_1")
        .is_some_and(|expanded| expanded.contains("pane_1")));
    let frame = state.compose(106, 24).expect("nested collection list");
    let text = frame
        .cells
        .chunks(frame.width as usize)
        .map(|row| {
            row.iter()
                .map(|cell| cell.symbol.as_str())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("active 2"), "{text}");
    assert!(text.contains("[ t term ]"), "{text}");
    assert!(text.contains("coordinate"), "{text}");
    assert!(text.contains("run tests"), "{text}");
    assert!(
        !text.contains("LIVE"),
        "list must replace the compact terminal surface"
    );
    assert_eq!(state.hits.collection_members.len(), 2);
    let pane_background = crate::protocol::color_to_u32(ratatui::style::Color::Reset);
    let selected_row = state.hits.collection_members[0].rect;
    for x in selected_row.x..selected_row.right() {
        let index = usize::from(selected_row.y) * usize::from(frame.width) + usize::from(x);
        assert_eq!(
            frame.cells[index].bg, pane_background,
            "list rows must explicitly reset to the normal terminal background"
        );
    }
    let selected_preview = state.hits.collection_members[0].preview;
    let preview_index = usize::from(selected_preview.y) * usize::from(frame.width)
        + usize::from(selected_preview.x);
    assert_eq!(
        frame.cells[preview_index].bg, pane_background,
        "empty preview space must reset stale colors to the normal terminal background"
    );
    assert_eq!(
        state
            .hits
            .panes
            .iter()
            .find(|pane| pane.pane_id == "pane_1")
            .map(|pane| pane.rect),
        Some(selected_preview),
        "the replaced full-leaf hit must not shadow nested preview hits"
    );
    let list_text = state.handle_raw_events(vec![RawInputEvent::Text(
        crate::input::TextCommit::new("must not leak"),
    )]);
    let list_paste = state.handle_raw_events(vec![RawInputEvent::Paste("must not leak".into())]);
    assert!(list_text.requests.is_empty());
    assert!(list_paste.requests.is_empty());
    state.collection_entered_pane = Some("pane_1".into());
    let entered_text = state.handle_raw_events(vec![RawInputEvent::Text(
        crate::input::TextCommit::new("ime"),
    )]);
    assert!(matches!(
        &entered_text.requests[..],
        [ClientMessage::ClientShellPaneInput { pane_id, events }]
            if pane_id == "pane_1"
                && matches!(&events[..], [ClientPaneInputEvent::TextCommit(text)] if text == "ime")
    ));
    state.collection_entered_pane = None;
    state.structure.as_mut().unwrap().collections[0].members[1].archived = true;
    let archived_frame = state.compose(106, 24).expect("archive section");
    let archived_text = archived_frame
        .cells
        .chunks(archived_frame.width as usize)
        .map(|row| {
            row.iter()
                .map(|cell| cell.symbol.as_str())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(archived_text.contains("archive 1"), "{archived_text}");
    state.structure.as_mut().unwrap().collections[0].members[1].archived = false;
    state.compose(106, 24).expect("restored active section");
    assert!(frame.cursor.is_none());
    let controls = state.take_pending_endpoint_controls();
    let interest = controls
        .iter()
        .rev()
        .find_map(|(_, message)| match message {
            ClientMessage::EndpointControl { kind, data }
                if kind == crate::protocol::endpoint::TERMINAL_VIEWS_INTEREST_KIND =>
            {
                serde_json::from_str::<crate::protocol::endpoint::ClientTerminalViewsInterest>(data)
                    .ok()
            }
            _ => None,
        })
        .expect("terminal preview interest");
    assert_eq!(interest.views.len(), 1);
    assert_eq!(interest.views[0].pane_id, "pane_1");
    let size = interest.views[0].size;
    let mut preview = Buffer::empty(Rect::new(0, 0, size.cols, size.rows));
    preview.set_string(0, 0, "CHILD PTY", Style::default());
    assert!(state.set_terminal_views_surface_for_generation(
        &ClientEndpointId::Local,
        0,
        crate::protocol::endpoint::ServerTerminalViewsSurface {
            boot_id: "boot-1".into(),
            projection_revision: 1,
            replace: true,
            views: vec![crate::protocol::endpoint::ServerTerminalViewSurface {
                pane_id: "pane_1".into(),
                frame: FrameData::from_ratatui_buffer_with_hyperlinks(
                    &preview,
                    Some(crate::protocol::CursorState {
                        x: 1,
                        y: 1,
                        visible: true,
                        shape: 2,
                    }),
                    &[],
                ),
                scroll: Some(crate::protocol::PaneSurfaceScrollMetrics {
                    offset_from_bottom: 2,
                    max_offset_from_bottom: 20,
                    viewport_rows: u64::from(size.rows),
                }),
                mouse_reporting: false,
                sgr_pixel_mouse: false,
                pixel_width: 0,
                pixel_height: 0,
            }],
        }
    ));
    let retained_surface = state.terminal_view_surfaces.clone();
    assert!(!state.set_terminal_views_surface_for_generation(
        &ClientEndpointId::Local,
        0,
        crate::protocol::endpoint::ServerTerminalViewsSurface {
            boot_id: "boot-1".into(),
            projection_revision: 1,
            replace: true,
            views: vec![crate::protocol::endpoint::ServerTerminalViewSurface {
                pane_id: "pane_1".into(),
                frame: FrameData {
                    width: size.cols,
                    height: size.rows,
                    cells: Vec::new(),
                    cursor: None,
                    hyperlinks: Vec::new(),
                    graphics: Vec::new(),
                },
                scroll: None,
                mouse_reporting: false,
                sgr_pixel_mouse: false,
                pixel_width: 0,
                pixel_height: 0,
            }],
        }
    ));
    assert_eq!(state.terminal_view_surfaces, retained_surface);
    let preview_frame = state.compose(106, 24).expect("inline child PTY");
    let preview_text = preview_frame
        .cells
        .chunks(preview_frame.width as usize)
        .map(|row| {
            row.iter()
                .map(|cell| cell.symbol.as_str())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(preview_text.contains("CHILD PTY"), "{preview_text}");
    assert!(preview_text.contains("drag height ↕"), "{preview_text}");
    let preview_hit = state.hits.collection_members[0].clone();
    assert_eq!(
        preview_hit.scrollbar.map(|scrollbar| scrollbar.height),
        Some(preview_hit.preview.height),
        "an inline terminal scrollbar must be bounded to that terminal"
    );
    assert!(!preview_hit.resize_handle.is_empty());
    let scrollbar = preview_hit.scrollbar.expect("scrollbar");
    for y in scrollbar.y..scrollbar.bottom() {
        let symbol = &preview_frame.cells
            [usize::from(y) * usize::from(preview_frame.width) + usize::from(scrollbar.x)]
        .symbol;
        assert!(
            matches!(symbol.as_str(), "│" | "┃"),
            "preview scrollbar must use box-drawing glyphs that join its corner: {symbol:?}"
        );
    }
    assert_eq!(
        preview_hit.resize_handle.x.saturating_add(1),
        preview_hit.preview.x,
        "the lower-left corner must align with the outer list edge"
    );
    assert_eq!(
        preview_hit.scrollbar.map(|scrollbar| scrollbar.x),
        Some(preview_hit.resize_handle.right().saturating_sub(1)),
        "the lower-right corner must sit directly beneath the scrollbar"
    );
    let handle_row = usize::from(preview_hit.resize_handle.y) * usize::from(preview_frame.width);
    assert_eq!(
        preview_frame.cells[handle_row + usize::from(preview_hit.resize_handle.x)].symbol,
        "└"
    );
    assert_eq!(
        preview_frame.cells[handle_row + usize::from(preview_hit.resize_handle.x + 1)].symbol,
        "─"
    );
    assert_eq!(
        preview_frame.cells
            [handle_row + usize::from(preview_hit.resize_handle.right().saturating_sub(1))]
        .symbol,
        "┘"
    );

    let collection_hit = state.hits.collections[0].clone();
    let collection_background = (preview_hit.resize_handle.bottom()
        ..collection_hit.surface_rect.bottom())
        .flat_map(|row| {
            (collection_hit.surface_rect.x..collection_hit.surface_rect.right())
                .map(move |column| (column, row))
        })
        .find(|(column, row)| {
            let point = (*column, *row);
            !state
                .hits
                .panes
                .iter()
                .any(|pane| super::contains(pane.rect, point))
                && !state.hits.collection_members.iter().any(|member| {
                    super::contains(member.rect, point)
                        || super::contains(member.preview, point)
                        || super::contains(member.resize_handle, point)
                })
        })
        .expect("collection background below preview");
    state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Right),
        column: collection_background.0,
        row: collection_background.1,
        modifiers: KeyModifiers::empty(),
    })]);
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::ContextMenu(ClientContextMenuOverlay {
            target: ClientContextMenuTarget::Collection { ref collection_id },
            ..
        })) if collection_id == "collection_1"
    ));

    state.overlay = None;
    state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Right),
        column: preview_hit.preview.x,
        row: preview_hit.preview.y,
        modifiers: KeyModifiers::empty(),
    })]);
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::ContextMenu(ClientContextMenuOverlay {
            target: ClientContextMenuTarget::Pane { ref pane_id, .. },
            ..
        })) if pane_id == "pane_1"
    ));
    state.overlay = None;

    state
        .collection_expanded_members
        .get_mut("collection_1")
        .unwrap()
        .insert("pane_2".into());
    state.compose(106, 24).expect("multiple expanded previews");
    let controls = state.take_pending_endpoint_controls();
    let multiple = controls
        .iter()
        .rev()
        .find_map(|(_, message)| match message {
            ClientMessage::EndpointControl { kind, data }
                if kind == crate::protocol::endpoint::TERMINAL_VIEWS_INTEREST_KIND =>
            {
                serde_json::from_str::<crate::protocol::endpoint::ClientTerminalViewsInterest>(data)
                    .ok()
            }
            _ => None,
        })
        .expect("multiple preview interest");
    assert_eq!(multiple.views.len(), 2);
    let second_size = multiple
        .views
        .iter()
        .find(|view| view.pane_id == "pane_2")
        .unwrap()
        .size;
    let mut second_preview = Buffer::empty(Rect::new(0, 0, second_size.cols, second_size.rows));
    second_preview.set_string(0, 0, "SECOND PTY", Style::default());
    assert!(state.set_terminal_views_surface_for_generation(
        &ClientEndpointId::Local,
        0,
        crate::protocol::endpoint::ServerTerminalViewsSurface {
            boot_id: "boot-1".into(),
            projection_revision: 1,
            replace: false,
            views: vec![crate::protocol::endpoint::ServerTerminalViewSurface {
                pane_id: "pane_2".into(),
                frame: FrameData::from_ratatui_buffer_with_hyperlinks(&second_preview, None, &[],),
                scroll: None,
                mouse_reporting: false,
                sgr_pixel_mouse: false,
                pixel_width: 0,
                pixel_height: 0,
            }],
        }
    ));
    assert_eq!(state.terminal_view_surfaces.len(), 2);
    state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: preview_hit.resize_handle.x,
        row: preview_hit.resize_handle.y,
        modifiers: KeyModifiers::empty(),
    })]);
    state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Drag(MouseButton::Left),
        column: preview_hit.resize_handle.x,
        row: preview_hit.resize_handle.y.saturating_sub(2),
        modifiers: KeyModifiers::empty(),
    })]);
    assert_eq!(
        state.collection_preview_heights.get("pane_1"),
        Some(&preview_hit.preview.height.saturating_sub(2).max(2))
    );
    state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Up(MouseButton::Left),
        column: preview_hit.resize_handle.x,
        row: preview_hit.resize_handle.y.saturating_sub(2),
        modifiers: KeyModifiers::empty(),
    })]);
    state.collection_preview_heights.remove("pane_1");
    state.compose(106, 24).expect("restored preview height");
    let _ = state.take_pending_endpoint_controls();
    let first_row = state.hits.collection_members[0].rect;
    state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Right),
        column: first_row.x,
        row: first_row.y,
        modifiers: KeyModifiers::empty(),
    })]);
    let archive_index = match state.overlay.as_ref() {
        Some(ClientShellOverlay::ContextMenu(menu)) => {
            assert!(menu
                .items()
                .iter()
                .any(|item| item.label == "Promote from collection"));
            menu.items()
                .iter()
                .position(|item| item.label == "Archive member")
                .unwrap()
        }
        _ => panic!("collection member context menu"),
    };
    let mut context_action = ClientShellInput::default();
    state.activate_context_menu_item(archive_index, &mut context_action);
    assert!(context_action.actions.iter().any(|action| matches!(
        action,
        ClientShellAction::Endpoint { request, .. }
            if matches!(request.method, crate::api::schema::Method::CollectionArchive(_))
    )));

    let swallowed = state.handle_input_bytes(b"x");
    assert!(swallowed.requests.is_empty(), "list input leaked to a PTY");
    let selected = state.handle_raw_events(vec![RawInputEvent::Key(
        crate::input::TerminalKey::new(KeyCode::Down, KeyModifiers::empty()),
    )]);
    assert!(selected.actions.iter().any(|action| matches!(
        action,
        ClientShellAction::Endpoint { request, .. }
            if matches!(
                &request.method,
                crate::api::schema::Method::CollectionSelect(params)
                    if params.collection_id == "collection_1" && params.pane_id == "pane_2"
            )
    )));
    let enter = state.handle_input_bytes(b"\r");
    assert!(enter.requests.is_empty());
    assert!(state.collection_list_views.contains("collection_1"));
    assert_eq!(state.collection_entered_pane.as_deref(), Some("pane_1"));
    let entered_frame = state.compose(106, 24).expect("entered inline terminal");
    assert!(entered_frame.cursor.is_some());
    let terminal_input = state.handle_input_bytes(b"x");
    assert!(matches!(
        terminal_input.requests.as_slice(),
        [ClientMessage::ClientShellPaneInput { pane_id, .. }] if pane_id == "pane_1"
    ));
    let bare_escape = state.handle_raw_events(vec![RawInputEvent::Key(
        crate::input::TerminalKey::new(KeyCode::Esc, KeyModifiers::empty()),
    )]);
    assert!(matches!(
        bare_escape.requests.as_slice(),
        [ClientMessage::ClientShellPaneInput { pane_id, .. }] if pane_id == "pane_1"
    ));
    state.handle_raw_events(vec![RawInputEvent::Key(crate::input::TerminalKey::new(
        KeyCode::Char('b'),
        KeyModifiers::CONTROL,
    ))]);
    state.handle_raw_events(vec![RawInputEvent::Key(crate::input::TerminalKey::new(
        KeyCode::Esc,
        KeyModifiers::empty(),
    ))]);
    assert!(state.collection_entered_pane.is_none());

    state.handle_input_bytes(b"z");
    assert_eq!(
        state.collection_maximized_pane.as_ref(),
        Some(&("collection_1".into(), "pane_1".into()))
    );
    let maximized = state.compose(106, 24).expect("maximized collection child");
    assert_shell_hits_within_frame(&state, &maximized);
    assert!(!state.hits.collection_members[0].preview.is_empty());
    let maximized_text = maximized
        .cells
        .chunks(maximized.width as usize)
        .map(|row| {
            row.iter()
                .map(|cell| cell.symbol.as_str())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(maximized_text.contains("[ ≡ list ]"), "{maximized_text}");
    let presentation_toggle = state.hits.collections[0].presentation_toggle;
    state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: presentation_toggle.x,
        row: presentation_toggle.y,
        modifiers: KeyModifiers::empty(),
    })]);
    assert!(state.collection_maximized_pane.is_none());
    assert!(state.collection_list_views.contains("collection_1"));

    state.handle_input_bytes(b"z");
    assert!(state.collection_maximized_pane.is_some());
    state.collection_entered_pane = None;
    state.handle_input_bytes(b"z");
    assert!(state.collection_maximized_pane.is_none());

    let promote = state.handle_input_bytes(b"p");
    assert!(promote.actions.iter().any(|action| matches!(
        action,
        ClientShellAction::Endpoint { request, .. }
            if matches!(request.method, crate::api::schema::Method::CollectionPromote(_))
    )));
    let archive = state.handle_input_bytes(b"a");
    assert!(archive.actions.iter().any(|action| matches!(
        action,
        ClientShellAction::Endpoint { request, .. }
            if matches!(request.method, crate::api::schema::Method::CollectionArchive(_))
    )));
    let preview = state.hits.collection_members[0].preview;
    let unfocused_scroll =
        state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: preview.x,
            row: preview.y,
            modifiers: KeyModifiers::empty(),
        })]);
    assert!(unfocused_scroll.requests.is_empty());
    assert!(unfocused_scroll.actions.is_empty());
    state.collection_entered_pane = Some("pane_1".into());
    let focused_scroll =
        state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: preview.x,
            row: preview.y,
            modifiers: KeyModifiers::empty(),
        })]);
    assert!(
        !focused_scroll.requests.is_empty() || !focused_scroll.actions.is_empty(),
        "wheel input must route only after the inline PTY is focused"
    );
    state.collection_entered_pane = None;
    let reorder = state.handle_raw_events(vec![RawInputEvent::Key(
        crate::input::TerminalKey::new(KeyCode::Down, KeyModifiers::CONTROL),
    )]);
    assert!(reorder.actions.iter().any(|action| matches!(
        action,
        ClientShellAction::Endpoint { request, .. }
            if matches!(request.method, crate::api::schema::Method::CollectionReorder(_))
    )));
    for (width, height) in [(106, 24), (72, 16), (40, 8), (20, 3), (1, 1), (106, 24)] {
        let frame = state.compose(width, height).expect("resized nested list");
        assert_shell_hits_within_frame(&state, &frame);
    }

    let header = state.hits.collections[0].rect;
    state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Right),
        column: header.x,
        row: header.y,
        modifiers: KeyModifiers::empty(),
    })]);
    let close_index = match state.overlay.as_ref() {
        Some(ClientShellOverlay::ContextMenu(menu)) => menu
            .items()
            .iter()
            .position(|item| item.label == "Close collection and members...")
            .unwrap(),
        _ => panic!("collection context menu"),
    };
    let mut close = ClientShellInput::default();
    state.activate_context_menu_item(close_index, &mut close);
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::ConfirmClose(
            ClientConfirmCloseOverlay {
                target: ClientConfirmCloseTarget::Collection { .. },
                ..
            }
        ))
    ));
    let confirmed = state.handle_input_bytes(b"\r");
    assert!(confirmed.actions.iter().any(|action| matches!(
        action,
        ClientShellAction::Endpoint { request, .. }
            if matches!(
                &request.method,
                crate::api::schema::Method::CollectionClose(params)
                    if params.disposition
                        == Some(crate::api::schema::CollectionCloseDisposition::CascadeClose)
            )
    )));
}

#[test]
fn pane_view_without_viewport_capability_uses_replacement_and_still_requests_content() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_endpoint_capabilities_for(
        &ClientEndpointId::Local,
        vec![crate::protocol::endpoint::PANE_VIEW_CAPABILITY.into()],
    );
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    state.compose(80, 20).unwrap();
    let mut outcome = ClientShellInput::default();
    state.open_terminal_inspector("pane_1".into(), &mut outcome);
    assert!(matches!(
        state
            .inspector
            .as_ref()
            .map(|inspector| inspector.placement),
        Some(crate::pane_contribution::PaneContributionPlacement::Replacement)
    ));
    assert!(outcome.actions.iter().any(|action| matches!(
        action,
        ClientShellAction::EndpointControl {
            message: ClientMessage::EndpointControl { kind, .. },
            ..
        } if kind == crate::protocol::endpoint::PANE_VIEW_REQUEST_KIND
    )));
    assert!(!outcome.actions.iter().any(|action| matches!(
        action,
        ClientShellAction::EndpointControl {
            message: ClientMessage::EndpointControl { kind, .. },
            ..
        } if kind == crate::protocol::endpoint::PANE_VIEWPORT_KIND
    )));
    let frame = state.compose(80, 20).unwrap();
    assert!(state.hits.panes[0].inner_rect.is_empty());
    assert!(!state.hits.panes[0].mouse_reporting);
    assert!(frame.cursor.is_none());
}

#[test]
fn inspector_on_unfocused_visible_pane_captures_keys_without_leaking_to_focused_terminal() {
    let mut snapshot = snapshot();
    let mut second = snapshot.panes[0].clone();
    second.pane_id = "pane_2".into();
    second.focused = false;
    snapshot.panes.push(second);
    let mut pane_surface = surface();
    pane_surface.panes[0].rect.width = 42;
    pane_surface.panes[0].rect.height = 29;
    pane_surface.panes[0].inner_rect.width = 40;
    pane_surface.panes[0].inner_rect.height = 27;
    let mut second_surface = pane_surface.panes[0].clone();
    second_surface.pane_id = "pane_2".into();
    second_surface.focused = false;
    second_surface.rect.x = 44;
    second_surface.inner_rect.x = 45;
    pane_surface.panes.push(second_surface);

    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_endpoint_capabilities_for(
        &ClientEndpointId::Local,
        vec![
            crate::protocol::endpoint::PANE_VIEW_CAPABILITY.into(),
            crate::protocol::endpoint::PANE_VIEWPORT_CAPABILITY.into(),
        ],
    );
    state.set_snapshot(Box::new(snapshot));
    state.set_pane_surface(pane_surface);
    state.compose(160, 30).unwrap();
    state.open_terminal_inspector("pane_2".into(), &mut ClientShellInput::default());

    let swallowed = state.handle_input_bytes(b"x");
    assert!(swallowed.requests.is_empty());
    assert!(swallowed.actions.is_empty());
    assert!(state.inspector.is_some());

    state.compose(160, 30).expect("accessory focus frame");
    let terminal = state
        .hits
        .panes
        .iter()
        .find(|pane| pane.pane_id == "pane_2")
        .unwrap()
        .inner_rect;
    state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: terminal.x,
        row: terminal.y,
        modifiers: KeyModifiers::empty(),
    })]);
    assert!(!state.inspector.as_ref().unwrap().focused);
    let terminal_input = state.handle_input_bytes(b"x");
    assert!(matches!(
        terminal_input.requests.as_slice(),
        [ClientMessage::ClientShellPaneInput { pane_id, .. }] if pane_id == "pane_2"
    ));

    state.inspector.as_mut().unwrap().focused = true;
    let closed = state.handle_input_bytes(b"\x1b");
    assert!(closed.requests.is_empty());
    assert!(state.inspector.is_none());
}

#[test]
fn contribution_renderers_clip_partially_and_fully_offscreen_regions() {
    let mut buffer = Buffer::empty(Rect::new(0, 0, 10, 4));
    let palette = ClientShellConfig::from_config(&Config::default()).palette;
    let contribution = crate::pane_contribution::PaneContribution {
        owner_service_id: "clip.service".into(),
        target: crate::pane_contribution::PaneContributionTarget::Pane {
            pane_id: "pane_1".into(),
        },
        placement: crate::pane_contribution::PaneContributionPlacement::Accessory,
        revision: 1,
        document: crate::view_document::ViewDocument {
            title: "Clipped".into(),
            subtitle: None,
            sections: Vec::new(),
        },
        actions: Vec::new(),
    };
    let rendered = render::render_pane_contribution(
        &mut buffer,
        Rect::new(8, 2, 40, 20),
        &contribution,
        &palette,
    )
    .unwrap();
    assert!(rendered.cancel.is_empty());
    assert!(rendered.contribution_actions.is_empty());
    render::render_pane_contribution_divider(&mut buffer, Rect::new(9, 0, 20, 20), &palette);
    render::render_pane_contribution_divider(&mut buffer, Rect::new(100, 100, 20, 20), &palette);
}

#[test]
fn stale_large_surface_resize_clips_integrated_contributions_and_all_hits() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_endpoint_capabilities_for(
        &ClientEndpointId::Local,
        vec![
            crate::protocol::endpoint::PANE_VIEW_CAPABILITY.into(),
            crate::protocol::endpoint::PANE_VIEWPORT_CAPABILITY.into(),
        ],
    );
    state.set_snapshot(Box::new(snapshot()));
    state.structure = Some(Box::new(
        crate::protocol::endpoint::ClientShellStructureProjection {
            session_id: "alpha".into(),
            boot_id: "boot-1".into(),
            revision: 1,
            collections: vec![crate::protocol::endpoint::ClientShellCollection {
                collection_id: "resize-collection".into(),
                workspace_id: "ws_1".into(),
                tab_id: "tab_1".into(),
                label: Some("Resize safety".into()),
                selected_pane_id: Some("pane_1".into()),
                members: vec![crate::protocol::endpoint::ClientShellCollectionMember {
                    pane_id: "pane_1".into(),
                    index: 0,
                    archived: false,
                }],
            }],
            delegations: Vec::new(),
            repositories: Vec::new(),
            space_order: Vec::new(),
        },
    ));
    let stale_buffer = Buffer::empty(Rect::new(0, 0, 220, 90));
    state.set_pane_surface(crate::protocol::PaneSurfaceFrame {
        boot_id: "boot-1".into(),
        projection_revision: 1,
        surface_revision: 1,
        frame: FrameData::from_ratatui_buffer_with_hyperlinks(&stale_buffer, None, &[]),
        panes: vec![crate::protocol::PaneSurfacePane {
            pane_id: "pane_1".into(),
            content_revision: 1,
            rect: crate::protocol::SurfaceRect {
                x: 0,
                y: 0,
                width: 220,
                height: 90,
            },
            inner_rect: crate::protocol::SurfaceRect {
                x: 1,
                y: 1,
                width: 218,
                height: 88,
            },
            scrollbar_rect: Some(crate::protocol::SurfaceRect {
                x: 219,
                y: 1,
                width: 1,
                height: 88,
            }),
            scroll: None,
            focused: true,
            mouse_reporting: false,
            sgr_pixel_mouse: false,
            alternate_screen_active: false,
            pixel_width: 0,
            pixel_height: 0,
        }],
        collections: Vec::new(),
        splits: vec![crate::protocol::PaneSurfaceSplit {
            direction: crate::protocol::PaneSurfaceSplitDirection::Horizontal,
            pos: 219,
            area: crate::protocol::SurfaceRect {
                x: 0,
                y: 0,
                width: 220,
                height: 90,
            },
            hit_rect: crate::protocol::SurfaceRect {
                x: 218,
                y: 0,
                width: 2,
                height: 90,
            },
            path: Vec::new(),
        }],
        popup: None,
        graphics: crate::protocol::SurfaceGraphicsScene::default(),
    });
    state.compose(180, 50).expect("initial stale surface frame");
    state.open_terminal_inspector("pane_1".into(), &mut ClientShellInput::default());

    // Loading and collection chrome survive repeated shrink/grow transitions while the server
    // still projects the old, much larger terminal surface.
    for (width, height) in [
        (180, 50),
        (110, 32),
        (72, 22),
        (42, 12),
        (20, 3),
        (1, 1),
        (96, 28),
        (180, 50),
    ] {
        let frame = state.compose(width, height).expect("clipped loading frame");
        assert_shell_hits_within_frame(&state, &frame);
        assert_no_nested_full_border(&frame, state.hits.inspector_body);
    }

    let inspector = state.inspector.as_mut().unwrap();
    inspector.loading = false;
    inspector.error = Some("stale resize error".into());
    let error = state.compose(42, 8).expect("clipped inline error");
    assert_shell_hits_within_frame(&state, &error);

    let contribution = crate::pane_contribution::PaneContribution {
        owner_service_id: "resize.service".into(),
        target: crate::pane_contribution::PaneContributionTarget::Pane {
            pane_id: "pane_1".into(),
        },
        placement: crate::pane_contribution::PaneContributionPlacement::Accessory,
        revision: 1,
        document: crate::view_document::ViewDocument {
            title: "Resize contribution".into(),
            subtitle: Some("stale geometry remains clipped".into()),
            sections: Vec::new(),
        },
        actions: vec![crate::view_document::ViewAction {
            id: "safe.action".into(),
            label: "Safe action".into(),
            destructive: false,
        }],
    };
    let inspector = state.inspector.as_mut().unwrap();
    inspector.error = None;
    inspector.loading = false;
    inspector.request_revision = 1;
    inspector.view_request_id = Some("resize-view".into());
    inspector.service_generation = Some(7);
    inspector.contribution = Some(contribution);
    for (width, height) in [
        (180, 50),
        (100, 30),
        (71, 20),
        (40, 10),
        (20, 3),
        (1, 1),
        (72, 22),
        (180, 50),
    ] {
        let frame = state
            .compose(width, height)
            .expect("clipped accessory frame");
        assert_shell_hits_within_frame(&state, &frame);
        assert_no_nested_full_border(&frame, state.hits.inspector_body);
    }

    let inspector = state.inspector.as_mut().unwrap();
    inspector.placement = crate::pane_contribution::PaneContributionPlacement::Replacement;
    inspector.contribution.as_mut().unwrap().placement =
        crate::pane_contribution::PaneContributionPlacement::Replacement;
    for (width, height) in [(120, 35), (45, 9), (20, 3), (1, 1), (120, 35)] {
        let frame = state
            .compose(width, height)
            .expect("clipped replacement frame");
        assert_shell_hits_within_frame(&state, &frame);
        assert_no_nested_full_border(&frame, state.hits.inspector_body);
    }

    state.overlay = Some(ClientShellOverlay::PaneInterruptConfirm(
        ClientPaneInterruptConfirmOverlay {
            endpoint_id: ClientEndpointId::Local,
            boot_id: "boot-1".into(),
            projection_revision: 1,
            pane_id: "pane_1".into(),
            contribution_request_id: "resize-view".into(),
            contribution_revision: 1,
            owner_service_id: "resize.service".into(),
            service_generation: 7,
            action_request_id: "resize-action".into(),
            confirmation_token: "resize-token".into(),
        },
    ));
    for (width, height) in [(90, 24), (30, 5), (1, 1), (90, 24)] {
        if let Some(frame) = state.compose(width, height) {
            assert_shell_hits_within_frame(&state, &frame);
        } else {
            assert_shell_hits_within_bounds(&state, Rect::new(0, 0, width, height));
        }
    }
}

#[test]
fn popup_keyboard_target_precedes_open_inspector() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    state.compose(106, 30).unwrap();
    state.open_terminal_inspector("pane_1".into(), &mut ClientShellInput::default());
    let mut popup_surface = surface();
    popup_surface.popup = Some(Box::new(crate::protocol::ClientShellPopupSurface {
        terminal_id: "popup-terminal".into(),
        title: "Popup".into(),
        width: None,
        height: None,
        frame: FrameData::from_ratatui_buffer_with_hyperlinks(
            &Buffer::with_lines(["popup"]),
            None,
            &[],
        ),
        mouse_reporting: false,
        sgr_pixel_mouse: false,
        pixel_width: 0,
        pixel_height: 0,
    }));
    state.set_pane_surface(popup_surface);
    state.compose(106, 30).unwrap();

    let input = state.handle_input_bytes(b"x");
    assert!(matches!(
        input.requests.as_slice(),
        [ClientMessage::ClientShellPopupInput { terminal_id, .. }]
            if terminal_id == "popup-terminal"
    ));
    assert!(state.inspector.is_some());
}

#[test]
fn graphics_active_target_forces_accessory_to_safe_replacement() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_endpoint_capabilities_for(
        &ClientEndpointId::Local,
        vec![
            crate::protocol::endpoint::PANE_VIEW_CAPABILITY.into(),
            crate::protocol::endpoint::PANE_VIEWPORT_CAPABILITY.into(),
        ],
    );
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    let asset = crate::protocol::SurfaceGraphicsAsset {
        key: crate::protocol::SurfaceGraphicsAssetKey {
            source: crate::protocol::SurfaceGraphicsSource::Terminal {
                target: crate::protocol::SurfaceGraphicsTarget::Pane {
                    pane_id: "pane_1".into(),
                },
                image_id: 1,
            },
            image_width: 1,
            image_height: 1,
            format: crate::protocol::SurfaceGraphicsFormat::Rgba,
            data_len: 4,
            data_fingerprint: 7,
        },
        data: vec![1, 2, 3, 4],
    };
    state
        .graphics
        .set_scene(crate::protocol::SurfaceGraphicsScene {
            placements: vec![crate::protocol::SurfaceGraphicsPlacement {
                asset: asset.key.clone(),
                logical_placement_id: 1,
                x: 0,
                y: 0,
                cols: 1,
                rows: 1,
                source_x: 0,
                source_y: 0,
                source_width: 1,
                source_height: 1,
                x_offset: 0,
                y_offset: 0,
                z: 0,
                scrollback_offset: 0,
            }],
            assets: vec![asset],
            retained_assets: Vec::new(),
        });
    state.compose(106, 30).unwrap();
    state.open_terminal_inspector("pane_1".into(), &mut ClientShellInput::default());
    assert!(matches!(
        state
            .inspector
            .as_ref()
            .map(|inspector| inspector.placement),
        Some(crate::pane_contribution::PaneContributionPlacement::Replacement)
    ));
    state.compose(106, 30).unwrap();
    assert!(state.hits.panes[0].inner_rect.is_empty());
    assert!(state
        .last_pane_viewport_request
        .as_ref()
        .is_some_and(|request| request.size.is_none()));
}

#[test]
fn terminal_inspector_opens_from_pane_menu_and_renders_safe_telemetry() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_endpoint_capabilities_for(
        &ClientEndpointId::Local,
        vec![
            crate::protocol::endpoint::PANE_VIEW_CAPABILITY.into(),
            crate::protocol::endpoint::PANE_VIEWPORT_CAPABILITY.into(),
        ],
    );
    state.set_snapshot(Box::new(snapshot()));
    state.set_graphics_cell_size(8, 16);
    let mut pane_surface = surface();
    pane_surface.panes[0].rect.width = 42;
    pane_surface.panes[0].rect.height = 29;
    pane_surface.panes[0].inner_rect.width = 40;
    pane_surface.panes[0].inner_rect.height = 27;
    state.set_pane_surface(pane_surface);
    state.compose(106, 30).expect("composed frame");
    let pane = state.hits.panes[0].rect;
    state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Right),
        column: pane.x + 1,
        row: pane.y,
        modifiers: KeyModifiers::empty(),
    })]);
    state.compose(106, 30).expect("pane menu");
    let inspect_index = match state.overlay.as_ref() {
        Some(ClientShellOverlay::ContextMenu(menu)) => menu
            .items()
            .iter()
            .position(|item| item.action == ClientContextMenuAction::InspectTerminal)
            .expect("inspector item"),
        _ => panic!("pane menu"),
    };
    let inspect = state.hits.context_menu_rows[inspect_index].0;
    let open = state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: inspect.x + 1,
        row: inspect.y,
        modifiers: KeyModifiers::empty(),
    })]);
    let viewport = open.actions.iter().find_map(|action| match action {
        ClientShellAction::EndpointControl {
            message: ClientMessage::EndpointControl { kind, data },
            ..
        } if kind == crate::protocol::endpoint::PANE_VIEWPORT_KIND => {
            serde_json::from_str::<crate::protocol::endpoint::ClientPaneViewportRequest>(data).ok()
        }
        _ => None,
    });
    assert_eq!(
        viewport.and_then(|request| request.size),
        Some(crate::protocol::ClientSurfaceSize { cols: 40, rows: 13 })
    );
    let view_request = open
        .actions
        .iter()
        .find_map(|action| match action {
            ClientShellAction::EndpointControl {
                message: ClientMessage::EndpointControl { kind, data },
                ..
            } if kind == crate::protocol::endpoint::PANE_VIEW_REQUEST_KIND => {
                serde_json::from_str::<crate::protocol::endpoint::ClientPaneViewRequest>(data).ok()
            }
            _ => None,
        })
        .expect("async pane view request");
    let request = open
        .actions
        .iter()
        .find_map(|action| match action {
            ClientShellAction::Endpoint { request, .. } => Some(request),
            _ => None,
        })
        .expect("inspector should request endpoint telemetry");
    let request_id = request.id.clone();
    assert!(matches!(
        &request.method,
        crate::api::schema::Method::PaneTelemetry(params)
            if params.pane_id.as_deref() == Some("pane_1")
    ));
    assert!(matches!(
        state.inspector,
        Some(ref inspector) if inspector.loading
    ));
    assert!(state.overlay.is_none());

    let telemetry = crate::api::schema::PaneTelemetry {
        pane_id: "pane_1".into(),
        availability: crate::api::schema::PaneTelemetryAvailability::Available,
        detail: None,
        cache_age_ms: 0,
        shell_pid: Some(10),
        foreground_process_group_id: Some(20),
        truncated: false,
        processes: vec![crate::api::schema::PaneTelemetryProcess {
            pid: 20,
            name: "cargo".into(),
            state: Some("R".into()),
            cpu_time_ms: Some(100),
            cpu_tenths_percent: Some(125),
            resident_bytes: Some(4 * 1024 * 1024),
            thread_count: Some(8),
            read_bytes: Some(0),
            write_bytes: Some(1024),
        }],
        agent: Some("pi".into()),
        display_agent: Some("Pi".into()),
        agent_status: crate::api::schema::AgentStatus::Working,
        state_labels: std::collections::HashMap::new(),
        document_owner: None,
        document: None,
    };
    assert!(
        state
            .handle_endpoint_result(
                "boot-1",
                &request_id,
                Ok(crate::api::schema::ResponseResult::PaneTelemetry { telemetry }),
            )
            .0
    );
    let frame = state.compose(106, 30).expect("inspector frame");
    let text = frame
        .cells
        .chunks(frame.width as usize)
        .map(|row| {
            row.iter()
                .map(|cell| cell.symbol.as_str())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("Terminal inspector"), "{text}");
    assert!(text.contains("Pi  ·  working"));
    assert!(text.contains("cargo"));
    assert!(text.contains("12.5%"));
    assert!(text.contains("argv/env never collected"));
    assert!(
        text.contains("client-shell"),
        "sidebar should remain visible"
    );
    assert_eq!(state.hits.panes[0].inner_rect.height, 13);
    assert_eq!(state.hits.panes[0].pixel_width, 40 * 8);
    assert_eq!(state.hits.panes[0].pixel_height, 13 * 16);
    assert_eq!(state.hits.inspector_body.height, 13);
    assert!(state.hits.panes[0].inner_rect.bottom() < state.hits.inspector_body.y);
    assert_no_nested_full_border(&frame, state.hits.inspector_body);
    let horizontal_divider_y = state.hits.inspector_body.y.saturating_sub(1);
    assert!(
        (state.hits.inspector_body.x..state.hits.inspector_body.right()).all(|x| frame_symbol(
            &frame,
            x,
            horizontal_divider_y
        ) == "─")
    );
    assert!(frame.cursor.is_none());

    let extension_result = crate::protocol::endpoint::ServerPaneViewResult {
        boot_id: "boot-1".into(),
        projection_revision: 1,
        request_id: view_request.request_id.clone(),
        pane_id: "pane_1".into(),
        contribution_revision: view_request.contribution_revision,
        owner_service_id: "vqro.terminal-inspector.telemetry".into(),
        service_generation: 7,
        document: Some(crate::view_document::ViewDocument {
            title: "Terminal intelligence".into(),
            subtitle: Some("supervised extension".into()),
            sections: vec![crate::view_document::ViewSection {
                id: "activity".into(),
                title: Some("Agent activity".into()),
                blocks: vec![crate::view_document::ViewBlock::Badge {
                    label: "Agent".into(),
                    value: "Pi".into(),
                    tone: crate::view_document::ViewTone::Accent,
                }],
            }],
        }),
        actions: vec![
            crate::view_document::ViewAction {
                id: "terminal.interrupt".into(),
                label: "Interrupt process with deliberately wide confirmation".into(),
                destructive: true,
            },
            crate::view_document::ViewAction {
                id: "future.action".into(),
                label: "Hidden narrow action".into(),
                destructive: false,
            },
        ],
        error: None,
    };
    let mut stale = extension_result.clone();
    stale.contribution_revision = stale.contribution_revision.saturating_add(1);
    assert!(!state.handle_pane_view_result(&ClientEndpointId::Local, 0, stale));
    let mut unowned = extension_result.clone();
    unowned.owner_service_id.clear();
    assert!(state.handle_pane_view_result(&ClientEndpointId::Local, 0, unowned));
    assert!(state.inspector.as_ref().unwrap().error.is_some());
    assert!(state.handle_pane_view_result(&ClientEndpointId::Local, 0, extension_result.clone()));
    state.pane_surface.as_mut().unwrap().panes[0].rect.width = 102;
    state.pane_surface.as_mut().unwrap().panes[0]
        .inner_rect
        .width = 100;
    let wide = state.compose(160, 30).expect("wide declarative inspector");
    assert!(state.hits.panes[0].inner_rect.right() < state.hits.inspector_body.x);
    assert_no_nested_full_border(&wide, state.hits.inspector_body);
    let vertical_divider_x = state.hits.inspector_body.x.saturating_sub(1);
    assert!(
        (state.hits.inspector_body.y..state.hits.inspector_body.bottom()).all(|y| frame_symbol(
            &wide,
            vertical_divider_x,
            y
        ) == "│")
    );

    state.pane_surface.as_mut().unwrap().panes[0].rect.width = 42;
    state.pane_surface.as_mut().unwrap().panes[0]
        .inner_rect
        .width = 40;
    let narrow = state.compose(72, 30).expect("narrow declarative inspector");
    let narrow_text = narrow
        .cells
        .chunks(narrow.width as usize)
        .map(|row| {
            row.iter()
                .map(|cell| cell.symbol.as_str())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(narrow_text.contains("Terminal intelligence"));
    assert!(narrow_text.contains("supervised extension"));
    assert!(narrow_text.contains("Agent activity"));
    assert!(narrow_text.contains("Interrupt"));
    assert!(narrow_text.contains("+1 more"), "{narrow_text}");
    assert_no_nested_full_border(&narrow, state.hits.inspector_body);
    let horizontal_divider_y = state.hits.inspector_body.y.saturating_sub(1);
    assert!(
        (state.hits.inspector_body.x..state.hits.inspector_body.right()).all(|x| frame_symbol(
            &narrow,
            x,
            horizontal_divider_y
        ) == "─")
    );
    assert_eq!(state.hits.contribution_actions.len(), 1);
    let hidden_action = state.handle_input_bytes(b"2");
    assert!(!hidden_action.actions.iter().any(|action| matches!(
        action,
        ClientShellAction::EndpointControl {
            message: ClientMessage::EndpointControl { kind, .. },
            ..
        } if kind == crate::protocol::endpoint::PANE_VIEW_ACTION_KIND
    )));
    let action_rect = state.hits.contribution_actions[0].0;
    let action =
        state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: action_rect.x,
            row: action_rect.y,
            modifiers: KeyModifiers::empty(),
        })]);
    let action_request = action.actions.iter().find_map(|action| match action {
        ClientShellAction::EndpointControl {
            message: ClientMessage::EndpointControl { kind, data },
            ..
        } if kind == crate::protocol::endpoint::PANE_VIEW_ACTION_KIND => {
            serde_json::from_str::<crate::protocol::endpoint::ClientPaneViewActionRequest>(data)
                .ok()
        }
        _ => None,
    });
    assert!(action_request.as_ref().is_some_and(|request| {
        request.action_id == "terminal.interrupt"
            && request.owner_service_id == "vqro.terminal-inspector.telemetry"
            && request.service_generation == 7
            && request.pane_id == "pane_1"
    }));
    assert!(state.handle_pane_view_action_result(
        &ClientEndpointId::Local,
        0,
        crate::protocol::endpoint::ServerPaneViewActionResult {
            request_id: action_request.unwrap().request_id,
            pane_id: "pane_1".into(),
            contribution_revision: view_request.contribution_revision,
            action_id: "terminal.interrupt".into(),
            confirmation_token: Some("confirm-token".into()),
            error: None,
        }
    ));
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::PaneInterruptConfirm(_))
    ));
    let confirmation = state.compose(72, 30).expect("interrupt confirmation");
    capture_review_frame("confirmation", &confirmation);
    let canceled = state.handle_input_bytes(b"\x1b");
    assert!(canceled.actions.is_empty());
    assert!(state.overlay.is_none());
    assert!(
        state.inspector.is_some(),
        "Esc cancels only the modal overlay"
    );

    let keyboard_action = state.handle_input_bytes(b"1");
    let keyboard_request = keyboard_action
        .actions
        .iter()
        .find_map(|action| match action {
            ClientShellAction::EndpointControl {
                message: ClientMessage::EndpointControl { kind, data },
                ..
            } if kind == crate::protocol::endpoint::PANE_VIEW_ACTION_KIND => {
                serde_json::from_str::<crate::protocol::endpoint::ClientPaneViewActionRequest>(data)
                    .ok()
            }
            _ => None,
        })
        .expect("visible numeric action");
    assert!(state.handle_pane_view_action_result(
        &ClientEndpointId::Local,
        0,
        crate::protocol::endpoint::ServerPaneViewActionResult {
            request_id: keyboard_request.request_id,
            pane_id: "pane_1".into(),
            contribution_revision: view_request.contribution_revision,
            action_id: "terminal.interrupt".into(),
            confirmation_token: Some("keyboard-confirm-token".into()),
            error: None,
        }
    ));
    let confirmed = state.handle_input_bytes(b"\r");
    assert!(confirmed.actions.iter().any(|action| matches!(
        action,
        ClientShellAction::EndpointControl {
            message: ClientMessage::EndpointControl { kind, .. },
            ..
        } if kind == crate::protocol::endpoint::PANE_VIEW_CONFIRM_KIND
    )));
    assert!(state.overlay.is_none());

    let replacement = state.handle_input_bytes(b"v");
    assert!(replacement.actions.iter().any(|action| match action {
        ClientShellAction::EndpointControl {
            message: ClientMessage::EndpointControl { data, .. },
            ..
        } => serde_json::from_str::<crate::protocol::endpoint::ClientPaneViewportRequest>(data)
            .is_ok_and(|request| request.size.is_none()),
        _ => false,
    }));
    let replacement_frame = state.compose(72, 30).expect("replacement contribution");
    assert_eq!(state.hits.inspector_body.height, 27);
    assert_no_nested_full_border(&replacement_frame, state.hits.inspector_body);
    assert_eq!(
        state.hits.inspector_body,
        state.hits.panes[0].projected_inner_rect
    );
    let accessory = state.handle_input_bytes(b"v");
    assert!(accessory.actions.iter().any(|action| match action {
        ClientShellAction::EndpointControl {
            message: ClientMessage::EndpointControl { data, .. },
            ..
        } => serde_json::from_str::<crate::protocol::endpoint::ClientPaneViewportRequest>(data)
            .is_ok_and(|request| request.size.is_some()),
        _ => false,
    }));

    state.pending_endpoint_controls.clear();
    state.pane_surface.as_mut().unwrap().panes[0]
        .inner_rect
        .height = 20;
    state.compose(72, 30).expect("resized accessory");
    let reissued = state.take_pending_endpoint_controls();
    assert!(reissued.iter().any(|(_, message)| match message {
        ClientMessage::EndpointControl { kind, data }
            if kind == crate::protocol::endpoint::PANE_VIEWPORT_KIND =>
        {
            serde_json::from_str::<crate::protocol::endpoint::ClientPaneViewportRequest>(data)
                .is_ok_and(|request| {
                    request.size == Some(crate::protocol::ClientSurfaceSize { cols: 40, rows: 9 })
                })
        }
        _ => false,
    }));

    state.inspector.as_mut().unwrap().pending_action =
        Some(crate::protocol::endpoint::ClientPaneViewActionRequest {
            boot_id: "boot-1".into(),
            projection_revision: 1,
            request_id: "pending-refresh-action".into(),
            pane_id: "pane_1".into(),
            contribution_revision: view_request.contribution_revision,
            owner_service_id: "vqro.terminal-inspector.telemetry".into(),
            service_generation: 7,
            action_id: "terminal.interrupt".into(),
        });
    state.overlay = Some(ClientShellOverlay::PaneInterruptConfirm(
        ClientPaneInterruptConfirmOverlay {
            endpoint_id: ClientEndpointId::Local,
            boot_id: "boot-1".into(),
            projection_revision: 1,
            pane_id: "pane_1".into(),
            contribution_request_id: view_request.request_id.clone(),
            contribution_revision: view_request.contribution_revision,
            owner_service_id: "vqro.terminal-inspector.telemetry".into(),
            service_generation: 7,
            action_request_id: "pending-refresh-action".into(),
            confirmation_token: "pending-confirmation".into(),
        },
    ));
    let mut refresh = ClientShellInput::default();
    state.refresh_terminal_inspector("pane_1".into(), &mut refresh);
    assert!(refresh.actions.iter().any(|action| matches!(
        action,
        ClientShellAction::Endpoint { request, .. }
            if matches!(request.method, crate::api::schema::Method::PaneTelemetry(_))
    )));
    let inspector = state.inspector.as_ref().unwrap();
    assert!(inspector.loading);
    assert!(inspector.contribution.is_none());
    assert!(inspector.service_generation.is_none());
    assert!(inspector.pending_action.is_none());
    assert!(inspector.telemetry.is_none());
    assert!(state.overlay.is_none());
    assert!(state.hits.contribution_actions.is_empty());
    let loading_frame = state.compose(72, 30).expect("inline loading state");
    assert_no_nested_full_border(&loading_frame, state.hits.inspector_body);
    let loading_text = loading_frame
        .cells
        .chunks(loading_frame.width as usize)
        .map(|row| {
            row.iter()
                .map(|cell| cell.symbol.as_str())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(loading_text.contains("loading"), "{loading_text}");
    let replacement_request = refresh
        .actions
        .iter()
        .find_map(|action| match action {
            ClientShellAction::EndpointControl {
                message: ClientMessage::EndpointControl { kind, data },
                ..
            } if kind == crate::protocol::endpoint::PANE_VIEW_REQUEST_KIND => {
                serde_json::from_str::<crate::protocol::endpoint::ClientPaneViewRequest>(data).ok()
            }
            _ => None,
        })
        .expect("replacement view request");
    assert!(!state.handle_pane_view_result(&ClientEndpointId::Local, 0, extension_result));
    assert!(state.inspector.as_ref().unwrap().contribution.is_none());
    assert!(state.handle_pane_view_result(
        &ClientEndpointId::Local,
        0,
        crate::protocol::endpoint::ServerPaneViewResult {
            boot_id: "boot-1".into(),
            projection_revision: 1,
            request_id: replacement_request.request_id,
            pane_id: "pane_1".into(),
            contribution_revision: replacement_request.contribution_revision,
            owner_service_id: "vqro.terminal-inspector.telemetry".into(),
            service_generation: 8,
            document: None,
            actions: Vec::new(),
            error: Some("replacement failed".into()),
        }
    ));
    let inspector = state.inspector.as_ref().unwrap();
    assert!(!inspector.loading);
    assert!(inspector.contribution.is_none());
    assert!(inspector.service_generation.is_none());
    assert_eq!(inspector.error.as_deref(), Some("replacement failed"));
    assert!(state.hits.contribution_actions.is_empty());
    let error_frame = state.compose(72, 30).expect("inline error state");
    assert_no_nested_full_border(&error_frame, state.hits.inspector_body);
    let error_text = error_frame
        .cells
        .chunks(error_frame.width as usize)
        .map(|row| {
            row.iter()
                .map(|cell| cell.symbol.as_str())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(error_text.contains("replacement failed"), "{error_text}");
    state.handle_input_bytes(b"\x1b");
    assert!(state.inspector.is_none());
}

#[test]
fn projection_revision_and_disconnect_close_pane_contribution_state() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_endpoint_capabilities_for(
        &ClientEndpointId::Local,
        vec![crate::protocol::endpoint::PANE_VIEW_CAPABILITY.into()],
    );
    state.set_snapshot(Box::new(snapshot()));
    let mut open = ClientShellInput::default();
    state.open_terminal_inspector("pane_1".into(), &mut open);
    state.overlay = Some(ClientShellOverlay::PaneInterruptConfirm(
        ClientPaneInterruptConfirmOverlay {
            endpoint_id: ClientEndpointId::Local,
            boot_id: "boot-1".into(),
            projection_revision: 1,
            pane_id: "pane_1".into(),
            contribution_request_id: "view-1".into(),
            contribution_revision: 1,
            owner_service_id: "test.service".into(),
            service_generation: 1,
            action_request_id: "action-1".into(),
            confirmation_token: "token".into(),
        },
    ));
    let mut revised = snapshot();
    revised.revision = 2;
    state.set_snapshot(Box::new(revised));
    assert!(state.inspector.is_none());
    assert!(!matches!(
        state.overlay,
        Some(ClientShellOverlay::PaneInterruptConfirm(_))
    ));

    state.open_terminal_inspector("pane_1".into(), &mut ClientShellInput::default());
    state.overlay = Some(ClientShellOverlay::PaneInterruptConfirm(
        ClientPaneInterruptConfirmOverlay {
            endpoint_id: ClientEndpointId::Local,
            boot_id: "boot-1".into(),
            projection_revision: 2,
            pane_id: "pane_1".into(),
            contribution_request_id: "view-2".into(),
            contribution_revision: 2,
            owner_service_id: "test.service".into(),
            service_generation: 2,
            action_request_id: "action-2".into(),
            confirmation_token: "token-2".into(),
        },
    ));
    state.set_endpoint_status(
        &ClientEndpointId::Local,
        crate::client::endpoint::ClientEndpointStatus::Attention,
    );
    assert!(state.inspector.is_none());
    assert!(!matches!(
        state.overlay,
        Some(ClientShellOverlay::PaneInterruptConfirm(_))
    ));
}

#[test]
fn global_menu_opens_from_sidebar_and_routes_client_actions() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    state.compose(106, 30).expect("shell frame");
    let launcher = state.hits.global_launcher;
    assert_ne!(launcher, Rect::default());

    let open = state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: launcher.x,
        row: launcher.y,
        modifiers: KeyModifiers::empty(),
    })]);
    assert!(open.repaint);
    let menu = state.compose(106, 30).expect("global menu");
    let text = menu
        .cells
        .chunks(menu.width as usize)
        .map(|row| {
            row.iter()
                .map(|cell| cell.symbol.as_str())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("settings"));
    assert!(text.contains("keybinds"));
    assert!(text.contains("reload config"));
    assert!(text.contains("extensions"));
    assert!(text.contains("detach"));

    let keybinds = state.hits.global_menu_rows[1].0;
    let help = state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: keybinds.x,
        row: keybinds.y,
        modifiers: KeyModifiers::empty(),
    })]);
    assert!(help.actions.is_empty());
    assert!(matches!(state.overlay, Some(ClientShellOverlay::Help(_))));

    let detach_index =
        super::super::global_menu::global_menu_items(state.snapshot.as_deref().expect("snapshot"))
            .iter()
            .position(|(label, _)| *label == "detach")
            .expect("detach menu item");
    state.overlay = Some(ClientShellOverlay::GlobalMenu(ClientGlobalMenuOverlay {
        highlighted: detach_index,
    }));
    let detach = state.handle_input_bytes(b"\r");
    assert!(detach.detach);
    assert!(state.overlay.is_none());
}

#[test]
fn new_tab_overlay_owns_text_cursor_and_submits_public_api_request() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    let mut open = ClientShellInput::default();
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::NewTab),
        &mut open,
    );
    assert!(open.actions.is_empty());
    let frame = state.compose(106, 20).expect("new tab overlay");
    let text = frame
        .cells
        .chunks(frame.width as usize)
        .map(|row| {
            row.iter()
                .map(|cell| cell.symbol.as_str())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("new tab"));
    assert!(text.contains("save"));
    let restored = frame.to_ratatui_buffer().expect("overlay frame");
    assert!(!restored
        .cell((26, 7))
        .expect("overlay title cell")
        .modifier
        .contains(Modifier::DIM));
    assert!(frame.cursor.as_ref().is_some_and(|cursor| cursor.visible));

    assert!(state.handle_input_bytes(b"logs").actions.is_empty());
    let create = state.handle_input_bytes(b"\r");
    let [ClientShellAction::Endpoint { request, .. }] = &create.actions[..] else {
        panic!("new tab save should use endpoint API");
    };
    assert!(matches!(
        &request.method,
        crate::api::schema::Method::TabCreate(params)
            if params.workspace_id.as_deref() == Some("ws_1")
                && params.label.as_deref() == Some("logs")
    ));
    assert!(state.overlay.is_none());
}

#[test]
fn close_confirmation_error_becomes_client_owned_overlay_and_stable_group_close() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    let mut close = ClientShellInput::default();
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::ClosePane),
        &mut close,
    );
    let [ClientShellAction::Endpoint { request, .. }] = &close.actions[..] else {
        panic!("pane close should use endpoint API");
    };
    let request_id = request.id.clone();
    assert!(
        state
            .handle_endpoint_result(
                "boot-1",
                &request_id,
                Err(ClientShellEndpointError {
                    code: Some("confirmation_required".into()),
                    message: "confirmation required".into(),
                }),
            )
            .0
    );
    let frame = state.compose(106, 20).expect("confirmation overlay");
    let text = frame
        .cells
        .chunks(frame.width as usize)
        .map(|row| {
            row.iter()
                .map(|cell| cell.symbol.as_str())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("Close workspace?"));
    assert!(text.contains("1 pane"));

    let confirm = state.handle_input_bytes(b"\r");
    let [ClientShellAction::Endpoint { request, .. }] = &confirm.actions[..] else {
        panic!("confirmation should use endpoint API");
    };
    assert!(matches!(
        &request.method,
        crate::api::schema::Method::WorkspaceClose(params)
            if params.workspace_id == "ws_1" && params.close_group
    ));
}

#[test]
fn empty_collection_surface_renders_and_owns_blank_space_context_menu() {
    let projected = snapshot();
    let structure = crate::protocol::endpoint::ClientShellStructureProjection {
        session_id: "alpha".into(),
        boot_id: "boot-1".into(),
        revision: 1,
        collections: vec![crate::protocol::endpoint::ClientShellCollection {
            collection_id: "collection_empty".into(),
            workspace_id: "ws_1".into(),
            tab_id: "tab_1".into(),
            label: Some("Empty workflow".into()),
            selected_pane_id: None,
            members: Vec::new(),
        }],
        delegations: Vec::new(),
        repositories: Vec::new(),
        space_order: Vec::new(),
    };
    let mut pane_surface = surface();
    let blank = Buffer::empty(Rect::new(0, 0, 80, 19));
    pane_surface.frame = FrameData::from_ratatui_buffer_with_hyperlinks(&blank, None, &[]);
    pane_surface.panes[0].rect = crate::protocol::SurfaceRect {
        x: 0,
        y: 0,
        width: 36,
        height: 19,
    };
    pane_surface.panes[0].inner_rect = pane_surface.panes[0].rect;
    pane_surface.collections = vec![crate::protocol::PaneSurfaceCollection {
        collection_id: "collection_empty".into(),
        rect: crate::protocol::SurfaceRect {
            x: 36,
            y: 0,
            width: 70,
            height: 19,
        },
        inner_rect: crate::protocol::SurfaceRect {
            x: 36,
            y: 1,
            width: 70,
            height: 18,
        },
        focused: true,
    }];

    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(projected));
    state.structure = Some(Box::new(structure));
    state.set_pane_surface(pane_surface);
    let frame = state.compose(106, 20).expect("empty collection frame");
    let text = frame
        .cells
        .chunks(frame.width as usize)
        .map(|row| {
            row.iter()
                .map(|cell| cell.symbol.as_str())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    // Same frame and header as a collection with terminals: a border around the
    // surface, the label with 0/0, the list/terminal control, and a Vqro-drawn
    // empty state naming the existing command that adds a pane.
    // The header owns the top row, as it does for a collection with terminals, so
    // the sides and the bottom corners are what a frame shows around it.
    let cell = |x: u16, y: u16| {
        frame.cells[y as usize * frame.width as usize + x as usize]
            .symbol
            .clone()
    };
    let surface_rect = state.hits.collections[0].surface_rect;
    let left = surface_rect.x;
    let right = surface_rect.right().saturating_sub(1);
    let bottom = surface_rect.bottom().saturating_sub(1);
    assert_eq!(cell(left, surface_rect.y + 5), "│", "left edge");
    assert_eq!(cell(right, surface_rect.y + 5), "│", "right edge");
    assert_eq!(cell(left, bottom), "└", "bottom-left corner");
    assert_eq!(cell(right, bottom), "┘", "bottom-right corner");
    // The label and count share the header with the controls, so the count fits
    // while the longer suffix truncates; the body states the rest.
    assert!(text.contains("Empty workflow 0/0"), "{text}");
    assert!(text.contains("[ ≡ list ]"), "{text}");
    assert!(text.contains(" i "), "{text}");
    assert!(text.contains("No terminals in this collection"), "{text}");
    assert!(text.contains("vqro collection add"), "{text}");
    // Clicking the drawn list/terminal control opens the list presentation,
    // which keeps its own empty state and must not unframe the collection.
    let toggle = state.hits.collections[0].presentation_toggle;
    assert!(!toggle.is_empty(), "the control must be a real hit target");
    state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: toggle.x + 1,
        row: toggle.y,
        modifiers: KeyModifiers::empty(),
    })]);
    assert!(
        state.collection_list_views.contains("collection_empty"),
        "clicking the list control must open the list presentation"
    );
    let listed = state.compose(106, 20).expect("empty collection list frame");
    let listed_text = listed
        .cells
        .chunks(listed.width as usize)
        .map(|row| {
            row.iter()
                .map(|cell| cell.symbol.as_str())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(listed_text.contains("No members"), "{listed_text}");
    let listed_cell = |x: u16, y: u16| {
        listed.cells[y as usize * listed.width as usize + x as usize]
            .symbol
            .clone()
    };
    assert_eq!(
        listed_cell(left, surface_rect.y + 5),
        "│",
        "left edge in list"
    );
    assert_eq!(
        listed_cell(right, bottom),
        "┘",
        "bottom-right corner in list"
    );

    let collection = state.hits.collections[0].clone();
    let point = (
        collection.surface_rect.x + 2,
        collection.surface_rect.bottom().saturating_sub(2),
    );
    state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Right),
        column: point.0,
        row: point.1,
        modifiers: KeyModifiers::empty(),
    })]);
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::ContextMenu(ClientContextMenuOverlay {
            target: ClientContextMenuTarget::Collection { ref collection_id },
            ..
        })) if collection_id == "collection_empty"
    ));
}
