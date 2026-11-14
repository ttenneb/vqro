use super::*;
use crate::api::schema::{
    CollectionAddParams, CollectionPromoteParams, CollectionReorderParams, PaneSwapParams,
    SplitDirection,
};
use ratatui::layout::Direction;

fn collection_fixture(collected: bool) -> (App, String, TerminalId, UnixStream, String, String) {
    let (mut app, pane, terminal, peer) = fixture();
    let raw = app.parse_current_public_pane_id(&pane).unwrap().1;
    let workspace = &mut app.state.workspaces[0];
    workspace.switch_tab(0);
    let existing = workspace.test_split(Direction::Horizontal);
    let target = workspace.test_split(Direction::Vertical);
    let collection = workspace.tabs[0]
        .create_collection_near(
            crate::layout::LayoutLeaf::Pane(target),
            Direction::Vertical,
            0.5,
            None,
        )
        .unwrap();
    workspace.tabs[0]
        .collect_tiled_pane(existing, collection)
        .unwrap();
    if collected {
        workspace.tabs[0]
            .collect_tiled_pane(raw, collection)
            .unwrap();
    }
    app.state.ensure_test_terminals();
    let collection_id = format!("collection_{}", collection.raw());
    let tab_id = app.public_tab_id(0, 0).unwrap();
    app.collections_authority.phase =
        crate::collections_extension_contract::CollectionsAuthorityPhaseV1::Package;
    app.collections_package_projections.insert(
        tab_id.clone(),
        crate::collections_extension_contract::CollectionsCompatibilityProjectionV1 {
            document: crate::extension_contract::HostDocumentV1 {
                contract: "host.document.v1".into(),
                namespace: crate::collections_extension_contract::COLLECTIONS_NAMESPACE.into(),
                generation: 1,
                revision: 1,
                surface: crate::extension_contract::HostSurface::Navigation,
                target: crate::extension_contract::HostTarget::Tab {
                    tab_id: tab_id.clone(),
                },
                items: Vec::new(),
                actions: Vec::new(),
                required_capabilities: Vec::new(),
            },
            collections: vec![crate::protocol::endpoint::ClientShellCollection {
                collection_id: collection_id.clone(),
                workspace_id: app.public_workspace_id(0),
                tab_id,
                label: None,
                selected_pane_id: None,
                members: Vec::new(),
            }],
        },
    );
    let target = app.public_pane_id(0, target).unwrap();
    assert!(app
        .terminal_runtimes
        .get(&terminal)
        .unwrap()
        .arm_fresh_split_cleanup());
    app.state.assert_invariants_for_test();
    (app, pane, terminal, peer, collection_id, target)
}

fn retained_and_usable(app: &mut App, pane: &str, terminal: &TerminalId, response: String) {
    let response: serde_json::Value = serde_json::from_str(&response).unwrap();
    assert!(response.get("error").is_none(), "{response}");
    assert_eq!(
        close(app, pane, terminal)["error"]["code"],
        "pristine_close_rejected",
        "relocation must disqualify cleanup"
    );
    assert!(app
        .terminal_runtimes
        .get(terminal)
        .unwrap()
        .try_send_bytes(bytes::Bytes::from_static(b"still usable"))
        .is_ok());
    app.state.assert_invariants_for_test();
}

#[tokio::test]
async fn pristine_relocation_collection_add_retains_the_same_terminal() {
    let (mut app, pane, terminal, _peer, collection, _) = collection_fixture(false);
    let response = app.handle_collection_add(
        "add".into(),
        CollectionAddParams {
            collection_id: collection,
            pane_id: pane.clone(),
        },
    );
    retained_and_usable(&mut app, &pane, &terminal, response);
}

#[tokio::test]
async fn pristine_relocation_collection_promote_retains_the_same_terminal() {
    let (mut app, pane, terminal, _peer, _, target) = collection_fixture(true);
    let response = app.handle_collection_promote(
        "promote".into(),
        CollectionPromoteParams {
            pane_id: pane.clone(),
            target_pane_id: target,
            direction: SplitDirection::Right,
            ratio: None,
            focus: false,
        },
    );
    retained_and_usable(&mut app, &pane, &terminal, response);
}

#[tokio::test]
async fn pristine_relocation_collection_reorder_retains_the_same_terminal() {
    let (mut app, pane, terminal, _peer, collection, _) = collection_fixture(true);
    let response = app.handle_collection_reorder(
        "reorder".into(),
        CollectionReorderParams {
            collection_id: collection,
            pane_id: pane.clone(),
            index: 0,
        },
    );
    retained_and_usable(&mut app, &pane, &terminal, response);
}

#[tokio::test]
async fn pristine_relocation_selection_alone_is_not_a_move() {
    let (mut app, pane, terminal, _peer, collection, _) = collection_fixture(true);
    let selected = app.handle_collection_select(
        "select".into(),
        crate::api::schema::CollectionSelectParams {
            collection_id: collection,
            pane_id: pane.clone(),
            focus: false,
        },
    );
    assert!(serde_json::from_str::<serde_json::Value>(&selected)
        .unwrap()
        .get("error")
        .is_none());
    let result = close(&mut app, &pane, &terminal);
    assert!(result.get("error").is_none(), "{result}");
    app.state.assert_invariants_for_test();
}

#[tokio::test]
async fn pristine_relocation_in_place_layout_replacement_disqualifies_reused_terminal() {
    let (mut app, pane, terminal, _peer) = fixture();
    let current: usize = app
        .state
        .workspaces
        .iter()
        .flat_map(|workspace| &workspace.tabs)
        .map(|tab| tab.layout.pane_count())
        .sum();
    app.state.workspaces.extend(
        (current..crate::admission::MAX_SESSION_PANES)
            .map(|_| crate::workspace::Workspace::test_new("capacity")),
    );
    app.state.ensure_test_terminals();
    let tab_id = app.public_tab_id(0, 0).unwrap();
    assert!(app
        .terminal_runtimes
        .get(&terminal)
        .unwrap()
        .arm_fresh_split_cleanup());
    let replaced = app.handle_layout_apply(
        "replace".into(),
        crate::api::schema::LayoutApplyParams {
            workspace_id: None,
            tab_id: Some(tab_id),
            tab_label: None,
            focus: false,
            root: crate::api::schema::LayoutNode::Pane {
                pane: crate::api::schema::LayoutPane::default(),
            },
        },
    );
    retained_and_usable(&mut app, &pane, &terminal, replaced);
}

#[tokio::test]
async fn pristine_relocation_swap_disqualifies_both_source_and_target() {
    for source in [false, true] {
        let (mut app, pane, terminal, _peer) = fixture();
        app.state.workspaces[0].switch_tab(0);
        let other = app.state.workspaces[0].test_split(Direction::Horizontal);
        app.state.ensure_test_terminals();
        let other = app.public_pane_id(0, other).unwrap();
        assert!(app
            .terminal_runtimes
            .get(&terminal)
            .unwrap()
            .arm_fresh_split_cleanup());
        let (from, to) = if source {
            (pane.clone(), other)
        } else {
            (other, pane.clone())
        };
        let response = app.handle_pane_swap(
            "swap".into(),
            PaneSwapParams {
                source_pane_id: Some(from),
                target_pane_id: Some(to),
                ..Default::default()
            },
        );
        retained_and_usable(&mut app, &pane, &terminal, response);
    }
}
