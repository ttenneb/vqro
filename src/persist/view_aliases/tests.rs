use super::*;
use crate::persist::snapshot::{normalize_identity_changes, parse_snapshot, DirectionSnapshot};

#[test]
fn ambiguous_aliases_cannot_grow_admission_at_tab_workspace_or_session_capacity() {
    let mut state = crate::app::AppState::test_new();
    state.workspaces = vec![crate::workspace::Workspace::test_new("capacity")];
    state.active = Some(0);
    state.ensure_test_terminals();
    let base = crate::persist::capture(
        &state.workspaces,
        &state.repositories,
        &state.space_order,
        &state.terminals,
        &crate::terminal::TerminalRuntimeRegistry::new(),
        state.active,
        state.selected,
    );
    for count in [
        crate::admission::MAX_TAB_PANES,
        crate::admission::MAX_WORKSPACE_PANES,
        crate::admission::MAX_SESSION_PANES,
    ] {
        let mut saved = base.clone();
        saved.workspaces.clear();
        saved.space_order.clear();
        let mut next_raw = 1u32;
        let mut next_box = 50_000u64;
        let mut remaining = count;
        while remaining > 0 {
            let workspace_count = remaining.min(crate::admission::MAX_WORKSPACE_PANES);
            remaining -= workspace_count;
            let mut workspace = base.workspaces[0].clone();
            workspace.id = None;
            workspace.tabs.clear();
            workspace.public_pane_numbers.clear();
            workspace.public_tab_numbers.clear();
            let mut in_workspace = workspace_count;
            while in_workspace > 0 {
                let tab_count = in_workspace.min(crate::admission::MAX_TAB_PANES);
                in_workspace -= tab_count;
                let mut tab = base.workspaces[0].tabs[0].clone();
                tab.id = None;
                tab.panes.clear();
                let mut children = Vec::new();
                for _ in 0..tab_count {
                    let mut terminal = base.workspaces[0].tabs[0]
                        .panes
                        .values()
                        .next()
                        .unwrap()
                        .clone();
                    terminal.terminal_id = None;
                    tab.panes.insert(next_raw, terminal);
                    children.push(LayoutSnapshot::Pane(next_raw));
                    next_raw += 1;
                }
                tab.root_pane = children.first().and_then(|node| match node {
                    LayoutSnapshot::Pane(id) => Some(*id),
                    _ => None,
                });
                tab.focused = tab.root_pane;
                tab.layout = LayoutSnapshot::Stack {
                    id: crate::terminal_container::TerminalContainerId::from_validated(next_box)
                        .into(),
                    selected: 0,
                    children,
                };
                next_box += 1;
                workspace.tabs.push(tab);
            }
            saved.workspaces.push(workspace);
        }
        normalize_identity_changes(&mut saved, false).unwrap();
        crate::persist::restore::validate_snapshot_admission(&saved)
            .expect("valid terminal records at capacity");
        let terminal_ids = saved
            .workspaces
            .iter()
            .flat_map(|workspace| &workspace.tabs)
            .flat_map(|tab| tab.panes.values())
            .map(|pane| crate::persist::snapshot::terminal_id_for_pane_snapshot(pane).unwrap())
            .collect::<HashSet<_>>();
        assert_eq!(terminal_ids.len(), count);
        for workspace in &mut saved.workspaces {
            for tab in &mut workspace.tabs {
                let raw_id = *tab.panes.keys().min().unwrap();
                let original = std::mem::replace(&mut tab.layout, LayoutSnapshot::Pane(raw_id));
                let aliases = (0..16)
                    .map(|_| LayoutSnapshot::View {
                        pane: raw_id,
                        extension: "vqro.tasks".into(),
                        view: "tasks".into(),
                    })
                    .collect();
                tab.layout = LayoutSnapshot::Split {
                    direction: DirectionSnapshot::Horizontal,
                    ratio: 0.5,
                    first: Box::new(original),
                    second: Box::new(LayoutSnapshot::Stack {
                        id: crate::terminal_container::TerminalContainerId::from_validated(
                            next_box,
                        )
                        .into(),
                        selected: 0,
                        children: aliases,
                    }),
                };
                next_box += 1;
            }
        }
        crate::persist::restore::validate_snapshot_admission(&saved)
            .expect("aliases have not added an identity yet");
        let repaired = parse_snapshot(&serde_json::to_string(&saved).unwrap()).unwrap();
        crate::persist::restore::validate_snapshot_admission(&repaired)
            .expect("repair cannot exceed a capacity boundary");
        let restored_ids = repaired
            .workspaces
            .iter()
            .flat_map(|workspace| &workspace.tabs)
            .flat_map(|tab| tab.panes.values())
            .map(|pane| crate::persist::snapshot::terminal_id_for_pane_snapshot(pane).unwrap())
            .collect::<HashSet<_>>();
        assert_eq!(
            restored_ids, terminal_ids,
            "no admitted terminal is replaced or fabricated"
        );
    }
}
