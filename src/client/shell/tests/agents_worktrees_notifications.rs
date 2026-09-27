use super::*;

#[test]
fn mouse_hits_use_stable_workspace_tab_and_pane_ids() {
    let config = ClientShellConfig::from_config(&Config::default());
    let mut state = ClientShellState::new(config);
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    state.compose(106, 20).expect("composed frame");

    let workspace_down =
        state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 2,
            row: 2,
            modifiers: KeyModifiers::empty(),
        })]);
    assert!(workspace_down.actions.is_empty());
    let workspace =
        state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column: 2,
            row: 2,
            modifiers: KeyModifiers::empty(),
        })]);
    assert!(workspace.requests.is_empty());
    let [ClientShellAction::Endpoint { request, .. }] = &workspace.actions[..] else {
        panic!("workspace click should use the endpoint API");
    };
    assert!(matches!(
        &request.method,
        crate::api::schema::Method::WorkspaceFocus(target)
            if target.workspace_id == "ws_1"
    ));

    let pane = state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 27,
        row: 1,
        modifiers: KeyModifiers::empty(),
    })]);
    assert!(pane.requests.is_empty());
    let [ClientShellAction::Endpoint { request, .. }] = &pane.actions[..] else {
        panic!("pane click should use the endpoint API");
    };
    assert!(matches!(
        &request.method,
        crate::api::schema::Method::PaneFocus(target) if target.pane_id == "pane_1"
    ));
}

#[test]
fn collapsed_workspace_jitter_remains_a_click() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.sidebar_collapsed = true;
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    state.compose(106, 20).expect("collapsed sidebar");
    let workspace = state.hits.workspaces[0].rect;
    state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: workspace.x,
        row: workspace.y,
        modifiers: KeyModifiers::empty(),
    })]);
    state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Drag(MouseButton::Left),
        column: workspace.x + 1,
        row: workspace.y,
        modifiers: KeyModifiers::empty(),
    })]);
    assert!(state.chrome_drag.is_none());
    assert!(state.workspace_press.is_some());
    let release =
        state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column: workspace.x + 1,
            row: workspace.y,
            modifiers: KeyModifiers::empty(),
        })]);
    assert!(matches!(
        &release.actions[..],
        [ClientShellAction::Endpoint { request, .. }]
            if matches!(
                &request.method,
                crate::api::schema::Method::WorkspaceFocus(target)
                    if target.workspace_id == "ws_1"
            )
    ));
}

#[test]
fn grouped_worktrees_render_parent_branch_and_indented_child() {
    let config = ClientShellConfig::from_config(&Config::default());
    let mut state = ClientShellState::new(config);
    let mut snapshot = snapshot();
    snapshot.workspaces[0].worktree = Some(ClientShellWorktree {
        key: "repo".into(),
        label: "repo".into(),
        is_linked_worktree: false,
    });
    snapshot.workspaces.push(ClientShellWorkspace {
        workspace_id: "ws_2".into(),
        active_tab_id: "tab_ws2".into(),
        new_workspace_cwd: "/repo/feature".into(),
        number: 2,
        label: "repo-feature".into(),
        custom_label: false,
        branch: Some("worktree/feature".into()),
        git_ahead_behind: None,
        tokens: Vec::new(),
        worktree: Some(ClientShellWorktree {
            key: "repo".into(),
            label: "repo".into(),
            is_linked_worktree: true,
        }),
        focused: false,
        agent_status: AgentStatus::Idle,
    });
    state.set_snapshot(Box::new(snapshot));
    state.set_pane_surface(surface());
    let frame = state.compose(106, 20).expect("composed frame");
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
    assert!(text.contains("main"));
    assert!(text.contains("└─"));
    assert!(text.contains("feature"));

    let mut replacement = (**state.snapshot.as_ref().expect("snapshot")).clone();
    replacement.revision = 2;
    replacement.focused_workspace_id = Some("ws_2".into());
    replacement.workspaces[0].focused = false;
    replacement.workspaces[1].focused = true;
    replacement.workspaces[1].agent_status = AgentStatus::Blocked;
    let mut replacement_surface = surface();
    replacement_surface.projection_revision = 2;
    state.collapsed_groups.insert("repo".into());
    state.set_snapshot(Box::new(replacement));
    state.set_pane_surface(replacement_surface);
    let collapsed = state.compose(106, 20).expect("collapsed worktree group");
    let parent = state.hits.workspaces[0].rect;
    let status_cell = usize::from(parent.y) * usize::from(collapsed.width)
        + usize::from(parent.x.saturating_add(1));
    assert_eq!(
        collapsed.cells[status_cell].fg,
        crate::protocol::color_to_u32(state.config.palette.red)
    );

    let mut next = ClientShellInput::default();
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::NextWorkspace),
        &mut next,
    );
    assert!(matches!(
        &next.actions[..],
        [ClientShellAction::Endpoint { request, .. }]
            if matches!(
                &request.method,
                crate::api::schema::Method::WorkspaceFocus(target)
                    if target.workspace_id == "ws_1"
            )
    ));
}

#[test]
fn typed_repository_projection_drives_space_identity_git_context_and_compact_count() {
    let mut projected = snapshot();
    projected.workspaces[0].label = "checkout-directory".into();
    projected.workspaces[0].branch = Some("main".into());
    projected.workspaces[0].git_ahead_behind = Some((1, 0));
    let mut child = projected.workspaces[0].clone();
    child.workspace_id = "ws_2".into();
    child.number = 2;
    child.label = "feature-directory".into();
    child.branch = Some("feature/sidebar".into());
    child.git_ahead_behind = Some((2, 1));
    child.focused = false;
    child.agent_status = AgentStatus::Blocked;
    projected.workspaces.push(child);
    let structure = Box::new(crate::protocol::endpoint::ClientShellStructureProjection {
        session_id: "alpha".into(),
        boot_id: projected.boot_id.clone(),
        revision: projected.revision,
        collections: Vec::new(),
        delegations: Vec::new(),
        repositories: vec![crate::protocol::endpoint::ClientShellRepository {
            repository_id: "repo_1".into(),
            label: "custom repository".into(),
            repository_path: Some("/repo/custom".into()),
            web_url: Some("https://github.com/example/custom".into()),
            durable_identity: Some("https://github.com/example/custom".into()),
            checkouts: vec![
                crate::protocol::endpoint::ClientShellRepositoryCheckout {
                    workspace_id: "ws_1".into(),
                    checkout_path: Some("/repo/custom".into()),
                    kind: crate::protocol::endpoint::ClientShellCheckoutKind::Primary,
                    upstream: Some("origin/main".into()),
                    upstream_web_url: Some("https://github.com/example/custom/tree/main".into()),
                    detached_head: None,
                },
                crate::protocol::endpoint::ClientShellRepositoryCheckout {
                    workspace_id: "ws_2".into(),
                    checkout_path: Some("/repo/linked".into()),
                    kind: crate::protocol::endpoint::ClientShellCheckoutKind::Linked,
                    upstream: None,
                    upstream_web_url: None,
                    detached_head: None,
                },
            ],
            last_focused_workspace_id: Some("ws_2".into()),
        }],
        space_order: vec![crate::protocol::endpoint::ClientShellSpaceRef {
            kind: crate::protocol::endpoint::ClientShellSpaceKind::Repository,
            id: "repo_1".into(),
        }],
    });
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(projected));
    state.structure = Some(structure.clone());
    state
        .endpoints
        .iter_mut()
        .find(|endpoint| endpoint.endpoint_id.is_local())
        .expect("local endpoint")
        .structure = Some(structure);
    state.set_pane_surface(surface());

    let frame = state.compose(106, 24).expect("repository projection");
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
    assert!(text.contains("custom repository"), "{text}");
    assert!(text.contains("main → origin/ma"), "{text}");
    assert!(text.contains("feature/sidebar"), "{text}");
    assert!(text.contains("└─  feature/sidebar"), "{text}");
    assert!(!text.contains("└─ · feature/sidebar"), "{text}");
    let child_hit = &state.hits.workspaces[1];
    let child_status = usize::from(child_hit.rect.y) * usize::from(frame.width)
        + usize::from(child_hit.rect.x.saturating_add(1));
    assert_eq!(
        frame.cells[child_status].symbol,
        status_icon(AgentStatus::Blocked, state.config.status_indicators),
        "linked checkouts keep their own base-depth attention bubble"
    );
    assert!(text.contains("↑2 ↓1"), "{text}");
    assert!(state.hits.workspaces[0].group_toggle.is_some());
    assert_eq!(
        state
            .hits
            .space_links
            .iter()
            .filter(|hit| matches!(hit.target, SpaceLinkTarget::LocalDirectory(_)))
            .count(),
        3,
        "the repository and both local branch labels open the checkout"
    );
    for hit in &state.hits.space_links {
        let index = usize::from(hit.rect.y) * usize::from(frame.width) + usize::from(hit.rect.x);
        let hyperlink = frame.cells[index].hyperlink.expect("OSC 8 link") as usize;
        let expected = match &hit.target {
            SpaceLinkTarget::LocalDirectory(path) => format!("file://{path}"),
            SpaceLinkTarget::WebUrl(_) => "https://github.com/example/custom/tree/main".to_string(),
        };
        assert_eq!(frame.hyperlinks[hyperlink], expected);
    }
    assert!(state.hits.space_links.iter().any(|hit| matches!(
        &hit.target,
        SpaceLinkTarget::LocalDirectory(path) if path == "/repo/linked"
    )));
    let repository_link = state
        .hits
        .space_links
        .iter()
        .find(|hit| matches!(hit.target, SpaceLinkTarget::LocalDirectory(_)))
        .expect("repository directory link");
    assert!(matches!(
        &repository_link.target,
        SpaceLinkTarget::LocalDirectory(path) if path == "/repo/custom"
    ));
    let upstream_link = state
        .hits
        .space_links
        .iter()
        .find(|hit| matches!(hit.target, SpaceLinkTarget::WebUrl(_)))
        .expect("upstream web link");
    assert!(matches!(
        &upstream_link.target,
        SpaceLinkTarget::WebUrl(url)
            if url == "https://github.com/example/custom/tree/main"
    ));
    let repository_rect = repository_link.rect;
    let upstream_rect = upstream_link.rect;
    for (rect, expected) in [
        (
            repository_rect,
            SpaceLinkTarget::LocalDirectory("/repo/custom".into()),
        ),
        (
            upstream_rect,
            SpaceLinkTarget::WebUrl("https://github.com/example/custom/tree/main".into()),
        ),
    ] {
        let click =
            state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: rect.x,
                row: rect.y,
                modifiers: KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            })]);
        assert!(
            matches!(
                (&click.actions[..], &expected),
                ([ClientShellAction::OpenLocalDirectory(actual)], SpaceLinkTarget::LocalDirectory(expected))
                    if actual == expected
            ) || matches!(
                (&click.actions[..], &expected),
                ([ClientShellAction::OpenSafeWebUrl(actual)], SpaceLinkTarget::WebUrl(expected))
                    if actual == expected
            )
        );
    }
    let ctrl_only_click =
        state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: upstream_rect.x,
            row: upstream_rect.y,
            modifiers: KeyModifiers::CONTROL,
        })]);
    assert!(matches!(
        &ctrl_only_click.actions[..],
        [ClientShellAction::OpenSafeWebUrl(url)]
            if url == "https://github.com/example/custom/tree/main"
    ));

    state.collapsed_groups.insert("repo_1".into());
    state.compose(106, 24).expect("collapsed repository space");
    assert_eq!(state.hits.workspaces[0].workspace_id, "ws_2");
    assert_eq!(state.hits.workspaces[0].move_workspace_id, "ws_1");
    let navigation_entries =
        state.navigation_workspace_entries(state.snapshot.as_deref().expect("snapshot"));
    assert_eq!(
        state.navigation_workspace_id(
            state.snapshot.as_deref().expect("snapshot"),
            &navigation_entries[0],
        ),
        "ws_2"
    );
    state.collapsed_groups.remove("repo_1");

    state.sidebar_collapsed = true;
    state.sidebar_collapsed_manual = true;
    let compact_snapshot = state.snapshot.as_deref().expect("snapshot");
    let compact_structure = state
        .endpoints
        .iter()
        .find(|endpoint| endpoint.endpoint_id.is_local())
        .and_then(|endpoint| endpoint.structure.as_deref())
        .expect("repository structure");
    assert_eq!(
        (
            compact_structure.boot_id.as_str(),
            compact_structure.revision
        ),
        (compact_snapshot.boot_id.as_str(), compact_snapshot.revision)
    );
    assert_eq!(
        super::super::sidebar::workspace_entries_with_structure(
            compact_snapshot,
            Some(compact_structure),
            None,
            &HashSet::from(["repo_1".to_owned()]),
        )
        .into_iter()
        .filter(|entry| !entry.indented)
        .count(),
        1
    );
    let compact = state.compose(106, 24).expect("compact repository spaces");
    assert_eq!(
        state.hits.workspaces.len(),
        1,
        "compact workspace hits: {:?}",
        state
            .hits
            .workspaces
            .iter()
            .map(|hit| hit.workspace_id.as_str())
            .collect::<Vec<_>>()
    );
    assert_eq!(state.hits.workspaces[0].workspace_id, "ws_2");
    let compact_status = usize::from(state.hits.workspaces[0].rect.y) * usize::from(compact.width)
        + usize::from(state.hits.workspaces[0].rect.x.saturating_add(2));
    assert_eq!(
        compact.cells[compact_status].fg,
        crate::protocol::color_to_u32(state.config.palette.red),
        "compact repository status must aggregate hidden checkout attention"
    );

    state.collapsed_groups.insert("repo_1".into());
    state.open_workspace_context_menu("ws_1".into(), 1, 1);
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::ContextMenu(ClientContextMenuOverlay {
            target: ClientContextMenuTarget::Workspace {
                has_worktree_children: true,
                collapsed: true,
                is_linked_worktree: false,
                ..
            },
            ..
        }))
    ));
}

#[test]
fn repository_collapse_keys_are_scoped_per_remote_endpoint() {
    let first = ClientEndpointId::Ssh(
        crate::client::endpoint::ProfileId::parse("11111111111111111111111111111111").unwrap(),
    );
    let second = ClientEndpointId::Ssh(
        crate::client::endpoint::ProfileId::parse("22222222222222222222222222222222").unwrap(),
    );
    assert_ne!(
        super::super::sidebar::repository_group_key(&first, "repo_1"),
        super::super::sidebar::repository_group_key(&second, "repo_1")
    );
}

#[test]
fn typed_single_checkout_repository_is_still_a_repository_space() {
    let mut projected = snapshot();
    projected.workspaces[0].branch = None;
    let mut structure = crate::protocol::endpoint::ClientShellStructureProjection {
        session_id: "alpha".into(),
        boot_id: projected.boot_id.clone(),
        revision: projected.revision,
        collections: Vec::new(),
        delegations: Vec::new(),
        repositories: vec![crate::protocol::endpoint::ClientShellRepository {
            repository_id: "repo_1".into(),
            label: "stable repository label".into(),
            repository_path: None,
            web_url: None,
            durable_identity: None,
            checkouts: vec![crate::protocol::endpoint::ClientShellRepositoryCheckout {
                workspace_id: "ws_1".into(),
                checkout_path: None,
                kind: crate::protocol::endpoint::ClientShellCheckoutKind::Primary,
                upstream: None,
                upstream_web_url: None,
                detached_head: Some("abc1234".into()),
            }],
            last_focused_workspace_id: Some("ws_1".into()),
        }],
        space_order: vec![crate::protocol::endpoint::ClientShellSpaceRef {
            kind: crate::protocol::endpoint::ClientShellSpaceKind::Repository,
            id: "repo_1".into(),
        }],
    };
    let entries = super::super::sidebar::workspace_entries_with_structure(
        &projected,
        Some(&structure),
        None,
        &HashSet::new(),
    );
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].repository_index, Some(0));
    assert!(!entries[0].repository_has_children);
    let rows = super::super::sidebar::projected_workspace_rows(
        &projected.workspaces[0],
        projected.workspaces[0].agent_status,
        &entries[0],
        Some(&structure),
        &Config::default().ui.sidebar.spaces,
    );
    assert!(rows.iter().flatten().any(|token| matches!(
        &token.kind,
        crate::ui::ResolvedTokenKind::Branch(branch) if branch == "detached@abc1234"
    )));
    structure.repositories[0].checkouts[0].detached_head = None;
    let unknown_rows = super::super::sidebar::projected_workspace_rows(
        &projected.workspaces[0],
        projected.workspaces[0].agent_status,
        &entries[0],
        Some(&structure),
        &Config::default().ui.sidebar.spaces,
    );
    assert!(!unknown_rows.iter().flatten().any(|token| matches!(
        &token.kind,
        crate::ui::ResolvedTokenKind::Branch(branch) if branch.contains("detached")
    )));
}

#[test]
fn typed_standalone_space_order_is_authoritative_without_repositories() {
    let mut projected = snapshot();
    let mut second = projected.workspaces[0].clone();
    second.workspace_id = "ws_2".into();
    second.focused = false;
    projected.workspaces.push(second);
    let structure = crate::protocol::endpoint::ClientShellStructureProjection {
        session_id: "alpha".into(),
        boot_id: projected.boot_id.clone(),
        revision: projected.revision,
        collections: Vec::new(),
        delegations: Vec::new(),
        repositories: Vec::new(),
        space_order: vec![
            crate::protocol::endpoint::ClientShellSpaceRef {
                kind: crate::protocol::endpoint::ClientShellSpaceKind::StandaloneWorkspace,
                id: "ws_2".into(),
            },
            crate::protocol::endpoint::ClientShellSpaceRef {
                kind: crate::protocol::endpoint::ClientShellSpaceKind::StandaloneWorkspace,
                id: "ws_1".into(),
            },
        ],
    };

    let entries = super::super::sidebar::workspace_entries_with_structure(
        &projected,
        Some(&structure),
        None,
        &HashSet::new(),
    );

    assert_eq!(
        entries
            .iter()
            .map(|entry| projected.workspaces[entry.index].workspace_id.as_str())
            .collect::<Vec<_>>(),
        vec!["ws_2", "ws_1"]
    );
}

#[test]
fn spaces_document_controls_workspace_order_labels_and_hierarchy() {
    use crate::extension_contract::{
        HostDocumentItem, HostDocumentItemFlags, HostDocumentV1, HostItemRole, HostSurface,
        HostTarget, HOST_DOCUMENT_CONTRACT_V1,
    };

    let mut projected = snapshot();
    let mut child = projected.workspaces[0].clone();
    child.workspace_id = "ws_2".into();
    child.focused = false;
    let mut standalone = projected.workspaces[0].clone();
    standalone.workspace_id = "ws_3".into();
    standalone.focused = false;
    projected.workspaces.extend([child, standalone]);
    let structure = crate::protocol::endpoint::ClientShellStructureProjection {
        session_id: "alpha".into(),
        boot_id: projected.boot_id.clone(),
        revision: projected.revision,
        collections: Vec::new(),
        delegations: Vec::new(),
        repositories: vec![crate::protocol::endpoint::ClientShellRepository {
            repository_id: "repo_1".into(),
            label: "native repository".into(),
            repository_path: None,
            web_url: None,
            durable_identity: None,
            checkouts: vec![
                crate::protocol::endpoint::ClientShellRepositoryCheckout {
                    workspace_id: "ws_1".into(),
                    checkout_path: None,
                    kind: crate::protocol::endpoint::ClientShellCheckoutKind::Primary,
                    upstream: None,
                    upstream_web_url: None,
                    detached_head: None,
                },
                crate::protocol::endpoint::ClientShellRepositoryCheckout {
                    workspace_id: "ws_2".into(),
                    checkout_path: None,
                    kind: crate::protocol::endpoint::ClientShellCheckoutKind::Linked,
                    upstream: None,
                    upstream_web_url: None,
                    detached_head: None,
                },
            ],
            last_focused_workspace_id: Some("ws_1".into()),
        }],
        space_order: vec![
            crate::protocol::endpoint::ClientShellSpaceRef {
                kind: crate::protocol::endpoint::ClientShellSpaceKind::Repository,
                id: "repo_1".into(),
            },
            crate::protocol::endpoint::ClientShellSpaceRef {
                kind: crate::protocol::endpoint::ClientShellSpaceKind::StandaloneWorkspace,
                id: "ws_3".into(),
            },
        ],
    };
    let item = |id: &str, parent_id: Option<&str>, role, label: &str| HostDocumentItem {
        id: id.into(),
        parent_id: parent_id.map(str::to_owned),
        role,
        label: label.into(),
        detail: None,
        terminal: None,
        action_ids: Vec::new(),
        flags: HostDocumentItemFlags::default(),
    };
    let document = HostDocumentV1 {
        contract: HOST_DOCUMENT_CONTRACT_V1.into(),
        namespace: "vqro.spaces".into(),
        generation: 1,
        revision: projected.revision,
        surface: HostSurface::Navigation,
        target: HostTarget::Session,
        items: vec![
            item("workspace:ws_3", None, HostItemRole::Section, "Scratch doc"),
            item(
                "repository:repo_1",
                None,
                HostItemRole::Group,
                "Repository doc",
            ),
            item(
                "workspace:ws_2",
                Some("repository:repo_1"),
                HostItemRole::Section,
                "Feature doc",
            ),
            item(
                "workspace:ws_1",
                Some("repository:repo_1"),
                HostItemRole::Section,
                "Main doc",
            ),
        ],
        actions: Vec::new(),
        required_capabilities: Vec::new(),
    };
    let documents = crate::protocol::endpoint::HostDocumentsProjectionV1 {
        boot_id: projected.boot_id.clone(),
        revision: projected.revision,
        documents: vec![crate::protocol::endpoint::HostDocumentProjectionEntryV1 {
            provider_priority: 100,
            document,
            confirmation_tokens: Default::default(),
        }],
    };

    let entries = super::super::sidebar::workspace_entries_with_structure(
        &projected,
        Some(&structure),
        Some(&documents),
        &HashSet::new(),
    );
    assert_eq!(
        entries
            .iter()
            .map(|entry| (
                projected.workspaces[entry.index].workspace_id.as_str(),
                entry.indented,
                entry.label.as_deref(),
            ))
            .collect::<Vec<_>>(),
        vec![
            ("ws_3", false, Some("Scratch doc")),
            ("ws_1", false, Some("Repository doc")),
            ("ws_2", true, Some("Feature doc")),
        ]
    );

    let mut incomplete = documents.clone();
    incomplete.documents[0]
        .document
        .items
        .retain(|item| item.id != "workspace:ws_3");
    let fallback = super::super::sidebar::workspace_entries_with_structure(
        &projected,
        Some(&structure),
        Some(&incomplete),
        &HashSet::new(),
    );
    assert_eq!(
        fallback
            .iter()
            .map(|entry| projected.workspaces[entry.index].workspace_id.as_str())
            .collect::<Vec<_>>(),
        vec!["ws_1", "ws_2", "ws_3"]
    );
    assert!(fallback.iter().all(|entry| entry.label.is_none()));
}

#[test]
fn highest_priority_spaces_document_wins_deterministically() {
    use crate::extension_contract::{
        HostDocumentItem, HostDocumentItemFlags, HostDocumentV1, HostItemRole, HostSurface,
        HostTarget, HOST_DOCUMENT_CONTRACT_V1,
    };

    let projected = snapshot();
    let structure = crate::protocol::endpoint::ClientShellStructureProjection {
        session_id: "alpha".into(),
        boot_id: projected.boot_id.clone(),
        revision: projected.revision,
        collections: Vec::new(),
        delegations: Vec::new(),
        repositories: Vec::new(),
        space_order: vec![crate::protocol::endpoint::ClientShellSpaceRef {
            kind: crate::protocol::endpoint::ClientShellSpaceKind::StandaloneWorkspace,
            id: "ws_1".into(),
        }],
    };
    let document = |label: &str| HostDocumentV1 {
        contract: HOST_DOCUMENT_CONTRACT_V1.into(),
        namespace: "vqro.spaces".into(),
        generation: 1,
        revision: projected.revision,
        surface: HostSurface::Navigation,
        target: HostTarget::Session,
        items: vec![HostDocumentItem {
            id: "workspace:ws_1".into(),
            parent_id: None,
            role: HostItemRole::Section,
            label: label.into(),
            detail: None,
            terminal: None,
            action_ids: Vec::new(),
            flags: HostDocumentItemFlags::default(),
        }],
        actions: Vec::new(),
        required_capabilities: Vec::new(),
    };
    let documents = crate::protocol::endpoint::HostDocumentsProjectionV1 {
        boot_id: projected.boot_id.clone(),
        revision: projected.revision,
        documents: vec![
            crate::protocol::endpoint::HostDocumentProjectionEntryV1 {
                provider_priority: 20,
                document: document("high first"),
                confirmation_tokens: Default::default(),
            },
            crate::protocol::endpoint::HostDocumentProjectionEntryV1 {
                provider_priority: 10,
                document: document("low"),
                confirmation_tokens: Default::default(),
            },
            crate::protocol::endpoint::HostDocumentProjectionEntryV1 {
                provider_priority: 20,
                document: document("high second"),
                confirmation_tokens: Default::default(),
            },
        ],
    };

    let entries = super::super::sidebar::workspace_entries_with_structure(
        &projected,
        Some(&structure),
        Some(&documents),
        &HashSet::new(),
    );
    assert_eq!(entries[0].label.as_deref(), Some("high first"));

    let mut malformed_winner = documents;
    malformed_winner.documents[0].document.items.clear();
    let fallback = super::super::sidebar::workspace_entries_with_structure(
        &projected,
        Some(&structure),
        Some(&malformed_winner),
        &HashSet::new(),
    );
    assert_eq!(fallback.len(), 1);
    assert_eq!(fallback[0].label, None);
}

#[test]
fn typed_repository_drag_moves_all_checkouts_when_visual_root_is_linked() {
    let mut projected = snapshot();
    let mut sibling = projected.workspaces[0].clone();
    sibling.workspace_id = "ws_2".into();
    sibling.focused = false;
    let mut standalone = projected.workspaces[0].clone();
    standalone.workspace_id = "ws_3".into();
    standalone.focused = false;
    projected.workspaces.extend([sibling, standalone]);
    let structure = Box::new(crate::protocol::endpoint::ClientShellStructureProjection {
        session_id: "alpha".into(),
        boot_id: projected.boot_id.clone(),
        revision: projected.revision,
        collections: Vec::new(),
        delegations: Vec::new(),
        repositories: vec![crate::protocol::endpoint::ClientShellRepository {
            repository_id: "repo_1".into(),
            label: "repo".into(),
            repository_path: None,
            web_url: None,
            durable_identity: None,
            checkouts: vec![
                crate::protocol::endpoint::ClientShellRepositoryCheckout {
                    workspace_id: "ws_1".into(),
                    checkout_path: None,
                    kind: crate::protocol::endpoint::ClientShellCheckoutKind::Linked,
                    upstream: None,
                    upstream_web_url: None,
                    detached_head: None,
                },
                crate::protocol::endpoint::ClientShellRepositoryCheckout {
                    workspace_id: "ws_2".into(),
                    checkout_path: None,
                    kind: crate::protocol::endpoint::ClientShellCheckoutKind::Linked,
                    upstream: None,
                    upstream_web_url: None,
                    detached_head: None,
                },
            ],
            last_focused_workspace_id: Some("ws_1".into()),
        }],
        space_order: vec![
            crate::protocol::endpoint::ClientShellSpaceRef {
                kind: crate::protocol::endpoint::ClientShellSpaceKind::Repository,
                id: "repo_1".into(),
            },
            crate::protocol::endpoint::ClientShellSpaceRef {
                kind: crate::protocol::endpoint::ClientShellSpaceKind::StandaloneWorkspace,
                id: "ws_3".into(),
            },
        ],
    });
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(projected));
    state.structure = Some(structure);

    assert!(matches!(
        state.workspace_move_method("ws_1", None),
        Some(crate::api::schema::Method::WorkspaceMoveBlock(
            crate::api::schema::WorkspaceMoveBlockParams { workspace_ids, .. }
        )) if workspace_ids == ["ws_1", "ws_2"]
    ));
}

#[test]
fn repository_group_survives_without_a_primary_checkout_projection() {
    let mut projected = snapshot();
    projected.workspaces[0].worktree = Some(ClientShellWorktree {
        key: "repo".into(),
        label: "repo".into(),
        is_linked_worktree: true,
    });
    let mut sibling = projected.workspaces[0].clone();
    sibling.workspace_id = "ws_2".into();
    sibling.number = 2;
    sibling.focused = false;
    projected.workspaces.push(sibling);

    let entries = super::super::sidebar::workspace_entries(&projected, &HashSet::new());

    assert_eq!(entries.len(), 2);
    assert!(!entries[0].indented);
    assert!(entries[1].indented);
    assert_eq!(
        super::super::sidebar::parent_group_key(&projected, entries[0].index).as_deref(),
        Some("repo")
    );
}

#[test]
fn workspace_click_waits_for_release_and_drag_reorders_by_stable_id() {
    let mut projected = snapshot();
    for index in 2..=3 {
        let mut workspace = projected.workspaces[0].clone();
        workspace.workspace_id = format!("ws_{index}");
        workspace.number = index;
        workspace.label = format!("workspace-{index}");
        workspace.focused = false;
        projected.workspaces.push(workspace);
    }
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(projected));
    state.set_pane_surface(surface());
    state.compose(106, 24).expect("three workspaces");
    let first = state.hits.workspaces[0].rect;
    let third = state.hits.workspaces[2].rect;

    let down = state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: first.x + 2,
        row: first.y,
        modifiers: KeyModifiers::empty(),
    })]);
    assert!(down.actions.is_empty());
    let drag = state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Drag(MouseButton::Left),
        column: third.x + 2,
        row: third.bottom(),
        modifiers: KeyModifiers::empty(),
    })]);
    assert!(drag.repaint);
    assert!(matches!(
        state.chrome_drag,
        Some(ClientChromeDrag::Workspace {
            ref source_workspace_id,
            target: Some((None, _)),
        }) if source_workspace_id == "ws_1"
    ));
    let frame = state.compose(106, 24).expect("workspace drop indicator");
    assert!(frame
        .cells
        .chunks(frame.width as usize)
        .any(|row| row.iter().take(20).any(|cell| cell.symbol == "─")));

    let release =
        state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column: third.x + 2,
            row: third.bottom(),
            modifiers: KeyModifiers::empty(),
        })]);
    assert!(matches!(
        &release.actions[..],
        [ClientShellAction::Endpoint { request, .. }]
            if matches!(
                &request.method,
                crate::api::schema::Method::WorkspaceMove(params)
                    if params.workspace_id == "ws_1" && params.insert_index == 3
            )
    ));

    state.compose(106, 24).expect("workspaces after drag");
    let second = state.hits.workspaces[1].rect;
    state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: second.x + 2,
        row: second.y,
        modifiers: KeyModifiers::empty(),
    })]);
    let click = state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Up(MouseButton::Left),
        column: second.x + 2,
        row: second.y,
        modifiers: KeyModifiers::empty(),
    })]);
    assert!(matches!(
        &click.actions[..],
        [ClientShellAction::Endpoint { request, .. }]
            if matches!(
                &request.method,
                crate::api::schema::Method::WorkspaceFocus(target)
                    if target.workspace_id == "ws_2"
            )
    ));
}

#[test]
fn workspace_drag_moves_parent_worktree_as_one_block_and_rejects_child() {
    let mut projected = snapshot();
    projected.workspaces[0].worktree = Some(ClientShellWorktree {
        key: "repo".into(),
        label: "repo".into(),
        is_linked_worktree: false,
    });
    let mut child = projected.workspaces[0].clone();
    child.workspace_id = "ws_child".into();
    child.number = 2;
    child.label = "feature".into();
    child.focused = false;
    child.worktree = Some(ClientShellWorktree {
        key: "repo".into(),
        label: "repo".into(),
        is_linked_worktree: true,
    });
    let mut other = projected.workspaces[0].clone();
    other.workspace_id = "ws_other".into();
    other.number = 3;
    other.label = "other".into();
    other.focused = false;
    other.worktree = None;
    projected.workspaces.extend([child, other]);

    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(projected));
    state.set_pane_surface(surface());
    state.compose(106, 24).expect("worktree workspaces");
    assert!(state.hits.workspaces[1].indented);
    let parent = state.hits.workspaces[0].rect;
    let child = state.hits.workspaces[1].rect;
    let other = state.hits.workspaces[2].rect;

    state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: parent.x + 2,
        row: parent.y,
        modifiers: KeyModifiers::empty(),
    })]);
    state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Drag(MouseButton::Left),
        column: other.x + 2,
        row: other.bottom(),
        modifiers: KeyModifiers::empty(),
    })]);
    let moved = state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Up(MouseButton::Left),
        column: other.x + 2,
        row: other.bottom(),
        modifiers: KeyModifiers::empty(),
    })]);
    assert!(matches!(
        &moved.actions[..],
        [ClientShellAction::Endpoint { request, .. }]
            if matches!(
                &request.method,
                crate::api::schema::Method::WorkspaceMoveBlock(params)
                    if params.workspace_ids == ["ws_1", "ws_child"]
                        && params.before_workspace_id.is_none()
            )
    ));

    state.compose(106, 24).expect("worktree child");
    state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: child.x + 2,
        row: child.y,
        modifiers: KeyModifiers::empty(),
    })]);
    let dragging_child =
        state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column: other.x + 2,
            row: other.bottom(),
            modifiers: KeyModifiers::empty(),
        })]);
    assert!(dragging_child.actions.is_empty());
    assert!(state.chrome_drag.is_none());
}

#[test]
fn pane_cycle_last_and_agent_actions_resolve_to_stable_pane_ids() {
    let mut initial = snapshot();
    let mut second = initial.panes[0].clone();
    second.pane_id = "pane_2".into();
    second.focused = false;
    initial.panes.push(second);
    initial.agents = vec![
        ClientShellAgent {
            pane_id: "pane_1".into(),
            workspace_id: "ws_1".into(),
            tab_id: "tab_1".into(),
            name: Some("first".into()),
            display_agent: None,
            agent: None,
            title: None,
            terminal_title: None,
            terminal_title_stripped: None,
            agent_status: AgentStatus::Idle,
            state_change_seq: 1,
            state_labels: Vec::new(),
            tokens: Vec::new(),
            focused: true,
        },
        ClientShellAgent {
            pane_id: "pane_2".into(),
            workspace_id: "ws_1".into(),
            tab_id: "tab_1".into(),
            name: Some("second".into()),
            display_agent: None,
            agent: None,
            title: None,
            terminal_title: None,
            terminal_title_stripped: None,
            agent_status: AgentStatus::Idle,
            state_change_seq: 2,
            state_labels: Vec::new(),
            tokens: Vec::new(),
            focused: false,
        },
    ];
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(initial.clone()));

    let mut cycle = ClientShellInput::default();
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::CyclePaneNext),
        &mut cycle,
    );
    let [ClientShellAction::Endpoint { request, .. }] = &cycle.actions[..] else {
        panic!("pane cycle should use endpoint API");
    };
    assert!(matches!(
        &request.method,
        crate::api::schema::Method::PaneFocus(target) if target.pane_id == "pane_2"
    ));

    let mut replacement = initial;
    replacement.revision = 2;
    replacement.focused_pane_id = Some("pane_2".into());
    replacement.panes[0].focused = false;
    replacement.panes[1].focused = true;
    state.set_snapshot(Box::new(replacement));
    let mut last = ClientShellInput::default();
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::LastPane),
        &mut last,
    );
    let [ClientShellAction::Endpoint { request, .. }] = &last.actions[..] else {
        panic!("last pane should use endpoint API");
    };
    assert!(matches!(
        &request.method,
        crate::api::schema::Method::PaneFocus(target) if target.pane_id == "pane_1"
    ));

    let mut agent = ClientShellInput::default();
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::FocusAgent(1)),
        &mut agent,
    );
    let [ClientShellAction::Endpoint { request, .. }] = &agent.actions[..] else {
        panic!("agent focus should use endpoint API");
    };
    assert!(matches!(
        &request.method,
        crate::api::schema::Method::PaneFocus(target) if target.pane_id == "pane_2"
    ));
}

#[test]
fn agent_sidebar_honors_priority_symbols_tokens_and_stable_hits() {
    let mut projected = snapshot();
    let mut second_pane = projected.panes[0].clone();
    second_pane.pane_id = "pane_2".into();
    second_pane.focused = false;
    projected.panes.push(second_pane);
    projected.agents = vec![
        ClientShellAgent {
            pane_id: "pane_1".into(),
            workspace_id: "ws_1".into(),
            tab_id: "tab_1".into(),
            name: Some("pi one".into()),
            display_agent: None,
            agent: Some("pi".into()),
            title: None,
            terminal_title: Some("first title".into()),
            terminal_title_stripped: Some("first".into()),
            agent_status: AgentStatus::Done,
            state_change_seq: 10,
            state_labels: Vec::new(),
            tokens: vec![("summary".into(), "review complete".into())],
            focused: true,
        },
        ClientShellAgent {
            pane_id: "pane_2".into(),
            workspace_id: "ws_1".into(),
            tab_id: "tab_1".into(),
            name: Some("pi two".into()),
            display_agent: None,
            agent: Some("pi".into()),
            title: None,
            terminal_title: Some("second title".into()),
            terminal_title_stripped: Some("second".into()),
            agent_status: AgentStatus::Blocked,
            state_change_seq: 20,
            state_labels: vec![("blocked".into(), "needs input".into())],
            tokens: vec![("summary".into(), "waiting for Can".into())],
            focused: false,
        },
    ];
    let mut config = Config::default();
    config.ui.agent_panel_sort = crate::config::AgentPanelSortConfig::Priority;
    config.ui.status_indicators = crate::config::StatusIndicatorStyle::Symbols;
    config.ui.sidebar.agents.rows = vec![vec![crate::config::AgentSidebarToken::Agent]];
    config.ui.sidebar.agents.rows_by_agent.insert(
        "pi".into(),
        vec![
            vec![
                crate::config::AgentSidebarToken::StateIcon,
                crate::config::AgentSidebarToken::StateText,
            ],
            vec![
                crate::config::AgentSidebarToken::Agent,
                crate::config::AgentSidebarToken::Custom("summary".into()),
            ],
        ],
    );
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&config));
    state.set_snapshot(Box::new(projected));
    state.set_pane_surface(surface());

    let frame = state.compose(106, 30).expect("agent sidebar frame");
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
    assert!(text.contains("× needs input"), "frame: {text}");
    assert!(text.contains("pi two"), "frame: {text}");
    assert!(text.contains("waiting for"), "frame: {text}");
    assert_eq!(
        state
            .hits
            .agents
            .first()
            .map(|(_, pane_id)| pane_id.as_str()),
        Some("pane_2")
    );

    let first = state.hits.agents[0].0;
    let click = state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: first.x,
        row: first.y,
        modifiers: KeyModifiers::empty(),
    })]);
    let [ClientShellAction::Endpoint { request, .. }] = &click.actions[..] else {
        panic!("agent row should focus through endpoint API");
    };
    assert!(matches!(
        &request.method,
        crate::api::schema::Method::PaneFocus(target) if target.pane_id == "pane_2"
    ));

    state.compose(106, 10).expect("short agent sidebar frame");
    assert_eq!(
        state
            .hits
            .agents
            .first()
            .map(|(_, pane_id)| pane_id.as_str()),
        Some("pane_2")
    );
    let body = state.hits.agent_body;
    state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::ScrollDown,
        column: body.x,
        row: body.y,
        modifiers: KeyModifiers::empty(),
    })]);
    state
        .compose(106, 10)
        .expect("scrolled agent sidebar frame");
    assert_eq!(
        state
            .hits
            .agents
            .first()
            .map(|(_, pane_id)| pane_id.as_str()),
        Some("pane_1")
    );

    state.sidebar_collapsed = true;
    let compact = state.compose(106, 30).expect("compact agent sidebar frame");
    let blocked = state
        .hits
        .agents
        .iter()
        .find(|(_, pane_id)| pane_id == "pane_2")
        .expect("blocked compact agent")
        .0;
    let row_start = blocked.y as usize * compact.width as usize + blocked.x as usize;
    assert_ne!(compact.cells[row_start].fg, compact.cells[row_start + 2].fg);
    assert_eq!(compact.cells[row_start].bg, compact.cells[row_start + 2].bg);
}

#[test]
fn muted_agent_sidebar_rows_do_not_stack_terminal_faint() {
    let mut projected = snapshot();
    projected.tabs[0].label = "second".into();
    projected.tabs[0].custom_label = true;
    projected.agents = vec![ClientShellAgent {
        pane_id: "pane_1".into(),
        workspace_id: "ws_1".into(),
        tab_id: "tab_1".into(),
        name: Some("reviewer".into()),
        display_agent: None,
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
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(projected));
    state.set_pane_surface(surface());
    let frame = state.compose(106, 30).expect("agent sidebar frame");
    let row = state.hits.agents.first().expect("agent row hit").0;
    let buffer = frame.to_ratatui_buffer().expect("agent sidebar buffer");

    for (label, needle) in [("tab", "second"), ("agent", "reviewer"), ("separator", "·")] {
        let (x, y) = cell_symbol_position(&frame, row, needle);
        let cell = buffer.cell((x, y)).expect("muted sidebar cell");
        assert!(
            !cell.modifier.contains(Modifier::DIM),
            "{label} cell at ({x},{y}) should not stack terminal faint: {cell:?}"
        );
    }
}

#[test]
fn workspace_state_text_does_not_stack_terminal_faint() {
    use crate::config::SpaceSidebarToken;

    let mut config = Config::default();
    config.ui.sidebar.spaces.rows = vec![
        vec![SpaceSidebarToken::StateIcon, SpaceSidebarToken::Workspace],
        vec![SpaceSidebarToken::StateText],
    ];
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&config));
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    let frame = state.compose(106, 30).expect("workspace sidebar frame");
    let rect = state.hits.workspaces.first().expect("workspace hit").rect;
    let buffer = frame.to_ratatui_buffer().expect("workspace sidebar buffer");
    let (x, y) = cell_symbol_position(&frame, rect, "idle");
    let cell = buffer.cell((x, y)).expect("workspace state text cell");
    assert!(
        !cell.modifier.contains(Modifier::DIM),
        "workspace state text at ({x},{y}) should not stack terminal faint: {cell:?}"
    );
}

#[test]
fn active_agent_view_controls_sidebar_order_and_focus_indices() {
    let mut projected = snapshot();
    let mut second_pane = projected.panes[0].clone();
    second_pane.pane_id = "pane_2".into();
    second_pane.focused = false;
    projected.panes.push(second_pane.clone());
    let mut third_pane = second_pane;
    third_pane.pane_id = "pane_3".into();
    projected.panes.push(third_pane);
    projected.agents = vec![
        ClientShellAgent {
            pane_id: "pane_1".into(),
            workspace_id: "ws_1".into(),
            tab_id: "tab_1".into(),
            name: Some("first".into()),
            display_agent: None,
            agent: Some("pi".into()),
            title: None,
            terminal_title: None,
            terminal_title_stripped: None,
            agent_status: AgentStatus::Idle,
            state_change_seq: 1,
            state_labels: Vec::new(),
            tokens: Vec::new(),
            focused: true,
        },
        ClientShellAgent {
            pane_id: "pane_2".into(),
            workspace_id: "ws_1".into(),
            tab_id: "tab_1".into(),
            name: Some("second".into()),
            display_agent: None,
            agent: Some("pi".into()),
            title: None,
            terminal_title: None,
            terminal_title_stripped: None,
            agent_status: AgentStatus::Blocked,
            state_change_seq: 2,
            state_labels: Vec::new(),
            tokens: Vec::new(),
            focused: false,
        },
        ClientShellAgent {
            pane_id: "pane_3".into(),
            workspace_id: "ws_1".into(),
            tab_id: "tab_1".into(),
            name: Some("third".into()),
            display_agent: None,
            agent: Some("pi".into()),
            title: None,
            terminal_title: None,
            terminal_title_stripped: None,
            agent_status: AgentStatus::Idle,
            state_change_seq: 3,
            state_labels: Vec::new(),
            tokens: Vec::new(),
            focused: false,
        },
    ];
    projected.agent_view_label = Some("review".into());
    projected.agent_order = vec!["pane_2".into(), "pane_3".into()];
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(projected));
    state.set_pane_surface(surface());
    state.compose(106, 30).expect("filtered agent sidebar");
    assert_eq!(
        state
            .hits
            .agents
            .iter()
            .map(|(_, pane_id)| pane_id.as_str())
            .collect::<Vec<_>>(),
        vec!["pane_2", "pane_3"]
    );
    assert_eq!(state.hits.agent_sort_toggle, Rect::default());

    let mut focus = ClientShellInput::default();
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::FocusAgent(0)),
        &mut focus,
    );
    assert!(matches!(
        &focus.actions[..],
        [ClientShellAction::Endpoint { request, .. }]
            if matches!(
                &request.method,
                crate::api::schema::Method::PaneFocus(target) if target.pane_id == "pane_2"
            )
    ));

    let mut next = ClientShellInput::default();
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::NextAgent),
        &mut next,
    );
    assert!(matches!(
        &next.actions[..],
        [ClientShellAction::Endpoint { request, .. }]
            if matches!(
                &request.method,
                crate::api::schema::Method::PaneFocus(target) if target.pane_id == "pane_2"
            )
    ));
}

#[test]
fn agent_sort_toggle_is_client_local_and_persists_per_endpoint() {
    let path = std::env::temp_dir().join(format!(
        "herdr-shell-agent-sort-{}-{}.json",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos()
    ));
    let mut projected = snapshot();
    projected.agents.push(ClientShellAgent {
        pane_id: "pane_1".into(),
        workspace_id: "ws_1".into(),
        tab_id: "tab_1".into(),
        name: Some("pi".into()),
        display_agent: None,
        agent: Some("pi".into()),
        title: None,
        terminal_title: None,
        terminal_title_stripped: None,
        agent_status: AgentStatus::Working,
        state_change_seq: 1,
        state_labels: Vec::new(),
        tokens: Vec::new(),
        focused: true,
    });
    let config =
        ClientShellConfig::from_config(&Config::default()).with_preferences_path(path.clone());
    let mut state = ClientShellState::new(config);
    state.set_snapshot(Box::new(projected));
    state.set_pane_surface(surface());
    state.compose(106, 30).expect("agent sidebar frame");
    let toggle = state.hits.agent_sort_toggle;

    let click = state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: toggle.x,
        row: toggle.y,
        modifiers: KeyModifiers::empty(),
    })]);

    assert_eq!(
        state.config.agent_panel_sort,
        crate::config::AgentPanelSortConfig::Priority
    );
    assert!(click.actions.is_empty());
    let reloaded_config =
        ClientShellConfig::from_config(&Config::default()).with_preferences_path(path.clone());
    let reloaded = ClientShellState::new(reloaded_config);
    assert_eq!(
        reloaded.config.agent_panel_sort,
        crate::config::AgentPanelSortConfig::Priority
    );
    assert!(reloaded.agent_panel_sort_manual);
    std::fs::remove_file(path).expect("remove agent sort preferences");
}

#[test]
fn workspace_actions_preserve_selected_target_and_client_confirmation() {
    let mut snapshot = snapshot();
    let mut second = snapshot.workspaces[0].clone();
    second.workspace_id = "ws_2".into();
    second.number = 2;
    second.label = "second".into();
    second.focused = false;
    snapshot.workspaces.push(second);
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot));
    state.mode = ClientShellMode::Navigate;
    state.navigate_workspace_id = state.navigation_target(&ClientEndpointId::Local, "ws_2");

    let rename = state.handle_raw_events(vec![RawInputEvent::Key(crate::input::TerminalKey::new(
        KeyCode::Char('w'),
        KeyModifiers::SHIFT,
    ))]);
    assert!(rename.actions.is_empty());
    assert!(matches!(
        state.overlay.as_ref(),
        Some(ClientShellOverlay::Rename(ClientRenameOverlay {
            target: ClientRenameTarget::Workspace { workspace_id },
            ..
        })) if workspace_id == "ws_2"
    ));
    assert!(state.handle_input_bytes(&[0x15]).actions.is_empty());
    assert!(state.handle_input_bytes(b"renamed").actions.is_empty());
    let save = state.handle_input_bytes(b"\r");
    let [ClientShellAction::Endpoint { request, .. }] = &save.actions[..] else {
        panic!("workspace rename should use endpoint API");
    };
    assert!(matches!(
        &request.method,
        crate::api::schema::Method::WorkspaceRename(params)
            if params.workspace_id == "ws_2" && params.label == "renamed"
    ));

    state.navigate_workspace_id = state.navigation_target(&ClientEndpointId::Local, "ws_2");
    let mut close = ClientShellInput::default();
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::CloseWorkspace),
        &mut close,
    );
    assert!(close.actions.is_empty());
    assert!(matches!(
        state.overlay.as_ref(),
        Some(ClientShellOverlay::ConfirmClose(ClientConfirmCloseOverlay {
            target: ClientConfirmCloseTarget::Workspace { workspace_id },
            ..
        })) if workspace_id == "ws_2"
    ));
    let confirm = state.handle_input_bytes(b"\r");
    let [ClientShellAction::Endpoint { request, .. }] = &confirm.actions[..] else {
        panic!("workspace confirmation should use endpoint API");
    };
    assert!(matches!(
        &request.method,
        crate::api::schema::Method::WorkspaceClose(params)
            if params.workspace_id == "ws_2" && params.close_group
    ));
}

#[test]
fn desktop_workspace_navigation_reveals_overflowing_selection() {
    let mut projected = snapshot();
    for index in 2..=8 {
        let mut workspace = projected.workspaces[0].clone();
        workspace.workspace_id = format!("ws_{index}");
        workspace.number = index;
        workspace.label = format!("workspace-{index}");
        workspace.focused = false;
        projected.workspaces.push(workspace);
    }
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(projected));
    state.set_pane_surface(surface());
    state.mode = ClientShellMode::Navigate;
    state.navigate_workspace_id = state.navigation_target(&ClientEndpointId::Local, "ws_1");
    state.compose(106, 12).expect("overflowing sidebar");

    for _ in 0..6 {
        state.handle_input_bytes(b"\x1b[B");
        state.compose(106, 12).expect("revealed workspace");
        let selected = state
            .navigate_workspace_id
            .as_ref()
            .expect("selection")
            .workspace_id
            .as_str();
        assert!(
            state
                .hits
                .workspaces
                .iter()
                .any(|hit| hit.workspace_id == selected),
            "{selected} should remain visible"
        );
    }
}

#[test]
fn named_workspace_overlay_targets_projected_source_workspace() {
    let mut config = Config::default();
    config.ui.prompt_new_workspace_name = true;
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&config));
    state.set_snapshot(Box::new(snapshot()));
    let mut open = ClientShellInput::default();
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::NewWorkspace),
        &mut open,
    );
    assert!(matches!(
        state.overlay.as_ref(),
        Some(ClientShellOverlay::Rename(ClientRenameOverlay {
            input: value,
            target: ClientRenameTarget::NewWorkspace {
                source_workspace_id,
                ..
            },
            ..
        })) if value == "repo" && source_workspace_id.as_deref() == Some("ws_1")
    ));
    let create = state.handle_input_bytes(b"\r");
    let [ClientShellAction::Endpoint { request, .. }] = &create.actions[..] else {
        panic!("named workspace should use endpoint API");
    };
    assert!(matches!(
        &request.method,
        crate::api::schema::Method::WorkspaceCreate(params)
            if params.source_workspace_id.as_deref() == Some("ws_1")
                && params.cwd.as_deref() == Some("/repo")
                && params.label.is_none()
    ));
}

#[test]
fn navigate_mode_selects_workspace_locally_then_focuses_by_stable_id() {
    let mut snapshot = snapshot();
    let mut second = snapshot.workspaces[0].clone();
    second.workspace_id = "ws_2".into();
    second.number = 2;
    second.label = "second".into();
    second.focused = false;
    snapshot.workspaces.push(second);
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot));
    state.set_pane_surface(surface());

    assert!(state.handle_input_bytes(&[0x02]).actions.is_empty());
    let enter_navigate = state.handle_input_bytes(b"w");
    assert!(enter_navigate.repaint);
    assert_eq!(state.mode, ClientShellMode::Navigate);
    assert_eq!(
        state.navigate_workspace_id,
        state.navigation_target(&ClientEndpointId::Local, "ws_1")
    );

    let invalid = state.handle_input_bytes(b"9");
    assert!(invalid.actions.is_empty());
    assert_eq!(state.mode, ClientShellMode::Navigate);
    assert_eq!(
        state.navigate_workspace_id,
        state.navigation_target(&ClientEndpointId::Local, "ws_1")
    );

    let move_selection = state.handle_input_bytes(b"\x1b[B");
    assert!(move_selection.actions.is_empty());
    assert_eq!(
        state.navigate_workspace_id,
        state.navigation_target(&ClientEndpointId::Local, "ws_2")
    );
    let frame = state.compose(106, 20).expect("navigate frame");
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
    assert!(text.contains("second"));
    assert!(text.contains("NAVIGATE"));

    let focus = state.handle_input_bytes(b"\r");
    let [ClientShellAction::Endpoint { request, .. }] = &focus.actions[..] else {
        panic!("selected workspace should use endpoint API");
    };
    assert!(matches!(
        &request.method,
        crate::api::schema::Method::WorkspaceFocus(target)
            if target.workspace_id == "ws_2"
    ));
    assert_eq!(state.mode, ClientShellMode::Terminal);
}

#[test]
fn worktree_create_previews_the_endpoint_owned_checkout_path() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    let mut prepare = ClientShellInput::default();
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::NewWorktree),
        &mut prepare,
    );
    let [ClientShellAction::Endpoint { request, .. }] = &prepare.actions[..] else {
        panic!("new worktree should prepare through worktree.list");
    };
    assert!(matches!(
        &request.method,
        crate::api::schema::Method::WorktreeList(params)
            if params.workspace_id.as_deref() == Some("ws_1")
    ));
    let request_id = request.id.clone();
    assert!(
        state
            .handle_endpoint_result("boot-1", &request_id, Ok(worktree_list_result(None)))
            .0
    );
    let frame = state.compose(106, 30).expect("new worktree modal");
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
    assert!(text.contains("new worktree"));
    assert!(text.contains("create and open"));
    assert!(frame.cursor.as_ref().is_some_and(|cursor| cursor.visible));

    assert!(state
        .handle_input_bytes(b"feature/client-shell")
        .actions
        .is_empty());
    assert!(matches!(
        &state.overlay,
        Some(ClientShellOverlay::WorktreeCreate(create))
            if create.checkout_path
                == "/tmp/herdr-worktrees/repo/feature-client-shell"
    ));
    let submit = state.handle_input_bytes(b"\r");
    let [ClientShellAction::Endpoint { request, .. }] = &submit.actions[..] else {
        panic!("worktree create should use endpoint API");
    };
    assert!(matches!(
        &request.method,
        crate::api::schema::Method::WorktreeCreate(params)
            if params.workspace_id.as_deref() == Some("ws_1")
                && params.branch.as_deref() == Some("feature/client-shell")
                && params.path.is_none()
                && !params.focus
    ));
}

#[test]
fn unavailable_worktree_create_does_not_wedge_the_overlay() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    let mut prepare = ClientShellInput::default();
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::NewWorktree),
        &mut prepare,
    );
    let [ClientShellAction::Endpoint { request, .. }] = &prepare.actions[..] else {
        panic!("new worktree should prepare through worktree.list");
    };
    state.handle_endpoint_result("boot-1", &request.id, Ok(worktree_list_result(None)));
    state.set_endpoint_methods(Some(vec!["worktree.list".into()]));
    state.handle_input_bytes(b"feature/unavailable");

    let submit = state.handle_input_bytes(b"\r");

    assert!(submit.actions.is_empty());
    assert!(matches!(
        &state.overlay,
        Some(ClientShellOverlay::WorktreeCreate(create)) if !create.creating
    ));
    assert!(state
        .visible_endpoint_notice
        .as_ref()
        .is_some_and(|notice| notice.key.code == "worktree.create"));
}

#[test]
fn worktree_action_errors_expire_without_more_input() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());

    let mut guard = ClientShellInput::default();
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::RemoveWorktree),
        &mut guard,
    );
    let message = "This workspace is not a vqro-managed worktree checkout.";
    assert_eq!(state.endpoint_error.as_deref(), Some(message));

    let deadline = state.endpoint_error_deadline.expect("deadline");
    assert!(!state.tick_endpoint_error(deadline - std::time::Duration::from_secs(1)));
    assert_eq!(state.endpoint_error.as_deref(), Some(message));

    assert!(state.tick_endpoint_error(deadline + std::time::Duration::from_millis(1)));
    assert!(state.endpoint_error.is_none());

    // A repeated identical message must start a fresh lifetime instead of
    // inheriting the earlier deadline.
    let before_repeat = std::time::Instant::now();
    state.set_endpoint_error(message);
    assert!(
        state.endpoint_error_deadline.expect("deadline")
            >= before_repeat + std::time::Duration::from_secs(5)
    );
}

#[test]
fn worktree_prepare_rejection_notice_expires() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    let mut prepare = ClientShellInput::default();
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::NewWorktree),
        &mut prepare,
    );
    let [ClientShellAction::Endpoint { request, .. }] = &prepare.actions[..] else {
        panic!("new worktree should prepare through worktree.list");
    };
    let request_id = request.id.clone();
    state.handle_endpoint_result(
        "boot-1",
        &request_id,
        Err(ClientShellEndpointError {
            code: Some("not_git_worktree".into()),
            message: "vqro worktree actions require a workspace inside a Git work tree".into(),
        }),
    );
    let notice = state
        .visible_endpoint_notice
        .as_ref()
        .expect("rejection notice");
    assert!(notice.key.code.contains("not_git_worktree"));
    let deadline = notice.deadline;

    let (_, repaint) = state.tick_notifications(deadline + std::time::Duration::from_millis(1));
    assert!(repaint);
    assert!(state.visible_endpoint_notice.is_none());
}

#[test]
fn worktree_open_filters_and_clicks_a_stable_public_entry() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    let mut prepare = ClientShellInput::default();
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::OpenWorktree),
        &mut prepare,
    );
    let [ClientShellAction::Endpoint { request, .. }] = &prepare.actions[..] else {
        panic!("open worktree should prepare through worktree.list");
    };
    let request_id = request.id.clone();
    state.handle_endpoint_result("boot-1", &request_id, Ok(worktree_list_result(None)));
    let frame = state.compose(106, 30).expect("open worktree modal");
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
    assert!(text.contains("feature"));
    let row = state.hits.worktree_rows[0].0;
    let open = state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: row.x + 2,
        row: row.y,
        modifiers: KeyModifiers::empty(),
    })]);
    let [ClientShellAction::Endpoint { request, .. }] = &open.actions[..] else {
        panic!("worktree row should open through endpoint API");
    };
    assert!(matches!(
        &request.method,
        crate::api::schema::Method::WorktreeOpen(params)
            if params.workspace_id.as_deref() == Some("ws_1")
                && params.path.as_deref() == Some("/repo-feature")
                && params.focus
    ));
}

#[test]
fn worktree_remove_escalates_recoverable_failure_to_force_confirmation() {
    for (code, message, expect_force) in [
        ("dirty_worktree_requires_force", "dirty worktree", true),
        (
            "worktree_remove_failed",
            "fatal: '/repo-feature' is not a working tree",
            true,
        ),
        ("worktree_remove_failed", "Permission denied", false),
        ("server_unavailable", "is not a working tree", false),
    ] {
        let mut snapshot = snapshot();
        snapshot.workspaces[0].worktree = Some(ClientShellWorktree {
            key: "repo-key".into(),
            label: "repo".into(),
            is_linked_worktree: true,
        });
        let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
        state.set_snapshot(Box::new(snapshot));
        state.set_pane_surface(surface());
        let mut prepare = ClientShellInput::default();
        state.record_binding(
            crate::input::KeybindMatch::Action(crate::input::KeybindAction::RemoveWorktree),
            &mut prepare,
        );
        let [ClientShellAction::Endpoint { request, .. }] = &prepare.actions[..] else {
            panic!("remove worktree should prepare through worktree.list");
        };
        let request_id = request.id.clone();
        state.handle_endpoint_result(
            "boot-1",
            &request_id,
            Ok(worktree_list_result(Some("ws_1"))),
        );
        let remove = state.handle_input_bytes(b"\r");
        let [ClientShellAction::Endpoint { request, .. }] = &remove.actions[..] else {
            panic!("worktree remove should use endpoint API");
        };
        assert!(matches!(
            &request.method,
            crate::api::schema::Method::WorktreeRemove(params)
                if params.workspace_id == "ws_1" && !params.force
        ));
        let request_id = request.id.clone();
        let (_, actions) = state.handle_endpoint_result(
            "boot-1",
            &request_id,
            Err(ClientShellEndpointError {
                code: Some(code.into()),
                message: message.into(),
            }),
        );
        assert!(actions.is_empty(), "failure must not retry automatically");
        let Some(ClientShellOverlay::WorktreeRemove(remove)) = &state.overlay else {
            panic!("failed remove should keep its confirmation");
        };
        assert!(!remove.removing);
        assert_eq!(remove.force_confirmation, expect_force);
        if !expect_force {
            assert_eq!(remove.error.as_deref(), Some(message));
            continue;
        }
        assert!(remove.error.is_none());
        let frame = state.compose(106, 30).expect("force remove modal");
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
        assert!(text.contains("delete anyway"));
        assert!(text.contains("permanently deleted"));
        let force = state.handle_input_bytes(b"\r");
        let [ClientShellAction::Endpoint { request, .. }] = &force.actions[..] else {
            panic!("forced worktree remove should use endpoint API");
        };
        assert!(matches!(
            &request.method,
            crate::api::schema::Method::WorktreeRemove(params)
                if params.workspace_id == "ws_1" && params.force
        ));
        let request_id = request.id.clone();
        let (_, actions) = state.handle_endpoint_result(
            "boot-1",
            &request_id,
            Err(ClientShellEndpointError {
                code: Some("worktree_remove_failed".into()),
                message: "fatal: '/repo-feature' is not a working tree".into(),
            }),
        );
        assert!(actions.is_empty());
        let Some(ClientShellOverlay::WorktreeRemove(remove)) = &state.overlay else {
            panic!("forced failure should keep its confirmation");
        };
        assert!(!remove.removing);
        assert_eq!(
            remove.error.as_deref(),
            Some("fatal: '/repo-feature' is not a working tree")
        );
        state.handle_input_bytes(b"\x1b");
        assert!(state.overlay.is_none());
    }
}

#[test]
fn semantic_notifications_use_client_policy_and_stable_navigation_targets() {
    let mut config = ClientShellConfig::from_config(&Config::default());
    config.toast_delivery = crate::config::ToastDelivery::Herdr;
    config.toast_delay_seconds = 0;
    let mut state = ClientShellState::new(config);
    let mut projected = snapshot();
    projected.agents.push(ClientShellAgent {
        pane_id: "pane_2".into(),
        workspace_id: "ws_2".into(),
        tab_id: "tab_2".into(),
        name: None,
        display_agent: Some("codex".into()),
        agent: Some("codex".into()),
        title: None,
        terminal_title: None,
        terminal_title_stripped: None,
        agent_status: AgentStatus::Blocked,
        state_change_seq: 1,
        state_labels: Vec::new(),
        tokens: Vec::new(),
        focused: false,
    });
    state.set_snapshot(Box::new(projected));
    state.set_pane_surface(surface());
    let now = std::time::Instant::now();
    let (effects, repaint) = state.receive_notification(
        &ClientEndpointId::Local,
        SemanticNotification {
            kind: SemanticNotificationKind::NeedsAttention,
            title: "codex needs attention".into(),
            body: Some("other · 2".into()),
            sound: Some(SemanticNotificationSound::Request),
            agent: Some("codex".into()),
            workspace_id: Some("ws_2".into()),
            tab_id: Some("tab_2".into()),
            pane_id: Some("pane_2".into()),
            position: None,
        },
        now,
    );
    assert!(repaint);
    assert!(matches!(
        effects.as_slice(),
        [ClientShellNotificationEffect::Sound {
            sound: crate::sound::Sound::Request,
            ..
        }]
    ));
    let frame = state.compose(100, 28).expect("notification frame");
    let rendered = frame
        .cells
        .chunks(frame.width as usize)
        .map(|row| {
            row.iter()
                .map(|cell| cell.symbol.as_str())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(rendered.contains("codex needs attention"));
    let hit = state.hits.notification_toast;
    let click = || {
        RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: hit.x,
            row: hit.y,
            modifiers: KeyModifiers::empty(),
        })
    };
    state.mode = ClientShellMode::Navigate;
    let ignored = state.handle_raw_events(vec![click()]);
    assert!(ignored.actions.is_empty());
    assert!(state.visible_notification.is_some());

    state.mode = ClientShellMode::Terminal;
    let outcome = state.handle_raw_events(vec![click()]);
    assert!(outcome.actions.iter().any(|action| matches!(
        action,
        ClientShellAction::Endpoint { request, .. }
            if matches!(
                &request.method,
                crate::api::schema::Method::PaneFocus(params)
                    if params.pane_id == "pane_2"
            )
    )));
    assert!(state.visible_notification.is_none());

    state.receive_notification(
        &ClientEndpointId::Local,
        SemanticNotification {
            kind: SemanticNotificationKind::NeedsAttention,
            title: "codex needs attention".into(),
            body: None,
            sound: None,
            agent: Some("codex".into()),
            workspace_id: Some("ws_2".into()),
            tab_id: Some("tab_2".into()),
            pane_id: Some("pane_2".into()),
            position: None,
        },
        now,
    );
    let mut keybind = ClientShellInput::default();
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::OpenNotificationTarget),
        &mut keybind,
    );
    assert!(keybind.actions.iter().any(|action| matches!(
        action,
        ClientShellAction::Endpoint { request, .. }
            if matches!(
                &request.method,
                crate::api::schema::Method::PaneFocus(params)
                    if params.pane_id == "pane_2"
            )
    )));
    assert!(state.visible_notification.is_none());

    state.receive_notification(
        &ClientEndpointId::Local,
        SemanticNotification {
            kind: SemanticNotificationKind::NeedsAttention,
            title: "first".into(),
            body: None,
            sound: None,
            agent: Some("codex".into()),
            workspace_id: Some("ws_2".into()),
            tab_id: Some("tab_2".into()),
            pane_id: Some("pane_2".into()),
            position: None,
        },
        now,
    );
    assert!(state.visible_notification.is_some());
    state.config.toast_delay_seconds = 1;
    let (_, repaint) = state.receive_notification(
        &ClientEndpointId::Local,
        SemanticNotification {
            kind: SemanticNotificationKind::NeedsAttention,
            title: "replacement".into(),
            body: None,
            sound: None,
            agent: Some("codex".into()),
            workspace_id: Some("ws_2".into()),
            tab_id: Some("tab_2".into()),
            pane_id: Some("pane_2".into()),
            position: None,
        },
        now,
    );
    assert!(repaint);
    assert!(state.visible_notification.is_none());
    assert_eq!(state.pending_notifications.len(), 1);
}
