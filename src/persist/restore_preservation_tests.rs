use super::*;

fn nested_snapshot(cwd: &std::path::Path) -> SessionSnapshot {
    super::super::snapshot::parse_snapshot(&serde_json::json!({
        "version":8,"workspaces":[{"id":"wsaved","identity_cwd":cwd,
        "public_pane_numbers":{"1":7,"2":9,"3":12},"next_public_pane_number":19,
        "public_tab_numbers":[4],"next_public_tab_number":8,
        "tabs":[{"layout":{"Stack":{"id":"collection_7600001","selected":1,"children":[
            {"Pane":1},{"Stack":{"id":"collection_7600002","selected":1,"children":[{"Pane":2},{"Pane":3}]}}
        ]}},"collections":[{"id":"collection_7600002","label":"hidden policy","archived":[2]}],
        "panes":{"1":{"cwd":cwd,"label":"first"},"2":{"cwd":cwd,"label":"hidden"},
        "3":{"cwd":cwd,"label":"selected","agent_session":{"source":"vqro:pi","agent":"pi","kind":"path","value":"/missing/saved-pi.jsonl"}}},
        "zoomed":false,"focused":3,"root_pane":1,"focused_leaf":{"type":"stack","id":"collection_7600001"}}],
        "active_tab":0}],"active":0,"selected":0,
        "collections_authority":crate::collections_extension_contract::CollectionsAuthorityV1::default(),"collections_action_sequence":91
    }).to_string()).expect("reachable valid nested snapshot")
}

#[tokio::test]
async fn cold_v10_preserves_collapsed_root_hidden_children_focus_and_terminal_identity() {
    let cwd = std::env::current_dir().unwrap();
    let mut snapshot = nested_snapshot(&cwd);
    let tab = &mut snapshot.workspaces[0].tabs[0];
    let LayoutSnapshot::Stack { children, .. } = &mut tab.layout else {
        panic!("stack")
    };
    children[0] = LayoutSnapshot::Collapsed {
        node: Box::new(children[0].clone()),
    };
    tab.layout = LayoutSnapshot::Collapsed {
        node: Box::new(tab.layout.clone()),
    };
    let saved = &snapshot.workspaces[0];
    let saved_terminals: HashSet<_> = saved.tabs[0]
        .panes
        .values()
        .map(|pane| super::super::snapshot::terminal_id_for_pane_snapshot(pane).unwrap())
        .collect();
    let (events, _receive) = mpsc::channel(32);
    let (mut workspaces, terminals, runtimes) = restore(
        &snapshot,
        None,
        40,
        100,
        4096,
        "__vqro_missing_shell__",
        crate::config::ShellModeConfig::NonLogin,
        false,
        events,
        Arc::new(Notify::new()),
        Arc::new(RenderSignal::new()),
    )
    .unwrap();
    assert!(runtimes.is_empty());
    let workspace = &mut workspaces[0];
    workspace.assert_invariants_for_test();
    assert_eq!(workspace.tabs[0].layout.pane_count(), 3);
    assert_eq!(
        workspace.public_pane_number(workspace.tabs[0].root_pane),
        Some(7)
    );
    let actual_terminals: HashSet<_> = workspace.tabs[0]
        .panes
        .values()
        .map(|pane| pane.attached_terminal_id.clone())
        .collect();
    assert_eq!(actual_terminals, saved_terminals);
    assert_eq!(terminals.len(), 3);
    let geometry = workspace.tabs[0]
        .layout
        .geometry(ratatui::layout::Rect::new(0, 0, 100, 40));
    assert!(geometry.panes.is_empty());
    assert_eq!(geometry.collapsed.len(), 1);
    let focus = workspace.tabs[0].layout.focused_leaf();
    assert!(matches!(focus, LayoutLeaf::Stack(_)));
    assert!(workspace.tabs[0].layout.set_collapsed(&[], false));
    assert_eq!(workspace.tabs[0].layout.focused_leaf(), focus);
    let outer = TerminalContainerId::parse(7_600_001).unwrap();
    assert!(workspace.tabs[0].layout.select_stack_child(outer, 0));
    assert!(!workspace.tabs[0].layout.any_visible_pane(|_| true));
    assert_eq!(
        workspace.tabs[0]
            .layout
            .geometry(ratatui::layout::Rect::new(0, 0, 100, 40))
            .collapsed
            .len(),
        1
    );
    workspace.assert_invariants_for_test();
}

#[tokio::test]
async fn cold_v9_preserves_sub_ten_percent_actual_view_ratios_in_both_orders() {
    for ratio in [0.02, 0.98] {
        let cwd = std::env::current_dir().unwrap();
        let terminal = serde_json::json!({"Pane":1});
        let view = serde_json::json!({"View":{"pane":2,"extension":"sample.views","view":"rows"}});
        let (first, second) = if ratio < 0.5 {
            (view, terminal)
        } else {
            (terminal, view)
        };
        let snapshot = super::super::snapshot::parse_snapshot(&serde_json::json!({
            "version":9,"workspaces":[{"id":"wautosize","identity_cwd":cwd,"tabs":[{
            "layout":{"Split":{"direction":"Vertical","ratio":ratio,"first":first,"second":second}},
            "panes":{"1":{"cwd":cwd}},"root_pane":1,"focused":1,"zoomed":false}],"active_tab":0}],"active":0,"selected":0,"collections_authority":crate::collections_extension_contract::CollectionsAuthorityV1::default()
        }).to_string()).unwrap();
        let (events, _receive) = mpsc::channel(32);
        let (workspaces, _terminals, runtimes) = restore(
            &snapshot,
            None,
            200,
            100,
            4096,
            "__vqro_missing_shell__",
            crate::config::ShellModeConfig::NonLogin,
            false,
            events,
            Arc::new(Notify::new()),
            Arc::new(RenderSignal::new()),
        )
        .unwrap();
        assert!(runtimes.is_empty());
        let crate::layout::TypedNode::Split { ratio: actual, .. } =
            workspaces[0].tabs[0].layout.typed_root()
        else {
            panic!("saved split lost")
        };
        assert_eq!(
            *actual, ratio,
            "actual ratio cannot be normalized to a manual/API floor"
        );
        workspaces[0].assert_invariants_for_test();
        assert!(
            crate::app::AppState::test_new()
                .view_sizing
                .active
                .is_empty(),
            "restore never rearms transient auto sizing"
        );
    }
}

#[tokio::test]
async fn missing_cwd_and_failed_shell_keep_hidden_nested_leaves_and_resume_facts() {
    let missing = std::env::temp_dir().join(format!(
        "vqro-missing-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    assert!(!missing.exists());
    for cwd in [&missing, &std::env::current_dir().unwrap()] {
        let snapshot = nested_snapshot(cwd);
        let saved = &snapshot.workspaces[0];
        let saved_terminal_ids: HashSet<_> = saved.tabs[0]
            .panes
            .values()
            .map(|pane| super::super::snapshot::terminal_id_for_pane_snapshot(pane).unwrap())
            .collect();
        assert_eq!(saved_terminal_ids.len(), 3);
        let (events, _rx) = mpsc::channel(32);
        let (workspaces, terminals, runtimes) = restore(
            &snapshot,
            None,
            24,
            80,
            4096,
            "__vqro_missing_shell__",
            crate::config::ShellModeConfig::NonLogin,
            false,
            events,
            Arc::new(Notify::new()),
            Arc::new(RenderSignal::new()),
        )
        .unwrap();
        assert_eq!(workspaces.len(), 1);
        assert_eq!(workspaces[0].tabs.len(), 1);
        workspaces[0].assert_invariants_for_test();
        assert!(runtimes.is_empty(), "no fallback execution in HOME");
        assert_eq!(terminals.len(), 3);
        assert_eq!(
            terminals.keys().cloned().collect::<HashSet<_>>(),
            saved_terminal_ids
        );
        assert!(terminals
            .values()
            .all(|terminal| terminal.cwd == *cwd && terminal.restore_error.is_some()));
        let selected = terminals
            .values()
            .find(|terminal| terminal.manual_label.as_deref() == Some("selected"))
            .unwrap();
        assert_eq!(
            selected
                .persisted_agent_session
                .as_ref()
                .unwrap()
                .session_ref
                .value,
            "/missing/saved-pi.jsonl"
        );
        let tab = &workspaces[0].tabs[0];
        assert_eq!(tab.number, 4);
        assert_eq!(tab.layout.pane_ids().len(), 3);
        assert_eq!(
            tab.layout
                .pane_ids()
                .iter()
                .filter(|id| tab.layout.is_pane_visible(**id))
                .count(),
            1
        );
        assert_eq!(workspaces[0].next_public_pane_number, 19);
        assert_eq!(workspaces[0].next_public_tab_number, 8);
        assert_eq!(
            workspaces[0]
                .public_pane_numbers
                .values()
                .copied()
                .collect::<HashSet<_>>(),
            HashSet::from([7, 9, 12])
        );
        let captured = super::super::capture(
            &workspaces,
            &[],
            &[],
            &terminals,
            &crate::terminal::TerminalRuntimeRegistry::new(),
            Some(0),
            0,
        );
        assert_eq!(captured.workspaces[0].tabs[0].panes.len(), 3);
        match &captured.workspaces[0].tabs[0].layout {
            LayoutSnapshot::Stack {
                selected, children, ..
            } => {
                assert_eq!(*selected, 1);
                assert!(
                    matches!(&children[1], LayoutSnapshot::Stack { selected: 1, children, .. } if children.len() == 2)
                );
            }
            _ => panic!("nested stack was flattened"),
        }
        assert!(captured.workspaces[0].tabs[0]
            .panes
            .values()
            .all(|pane| pane.cwd == *cwd));
    }
}

#[test]
fn fingerprint_normalizes_maps_and_sets_and_binds_hidden_tree_and_high_water() {
    let mut image = nested_snapshot(std::path::Path::new("/"));
    image.collapsed_space_keys = ["z", "a", "m"].map(str::to_string).into();
    let expected = super::super::snapshot::layout_fingerprint(&image).unwrap();
    for _ in 0..16 {
        image = super::super::snapshot::parse_snapshot(&serde_json::to_string(&image).unwrap())
            .unwrap();
        assert_eq!(
            super::super::snapshot::layout_fingerprint(&image).as_deref(),
            Some(expected.as_str())
        );
    }
    let mut changed = image.clone();
    let LayoutSnapshot::Stack { children, .. } = &mut changed.workspaces[0].tabs[0].layout else {
        panic!()
    };
    let LayoutSnapshot::Stack { selected, .. } = &mut children[1] else {
        panic!()
    };
    *selected = 0;
    assert_ne!(
        super::super::snapshot::layout_fingerprint(&changed).as_deref(),
        Some(expected.as_str())
    );
    image.collections_action_sequence += 1;
    assert_ne!(
        super::super::snapshot::layout_fingerprint(&image).as_deref(),
        Some(expected.as_str())
    );
}
