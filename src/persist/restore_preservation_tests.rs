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
