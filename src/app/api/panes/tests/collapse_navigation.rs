use super::*;
use crate::{config::Config, workspace::Workspace};
use ratatui::layout::{Direction, Rect};

#[test]
fn directional_focus_leaves_a_collapsed_leaf_or_box_via_its_header_geometry() {
    for (direction, navigation) in [
        (Direction::Horizontal, PaneDirection::Right),
        (Direction::Vertical, PaneDirection::Down),
    ] {
        for containing_box in [false, true] {
            let (_send, receive) = tokio::sync::mpsc::unbounded_channel();
            let mut app = App::new(
                &Config::default(),
                crate::app::AppPolicy::TEST,
                None,
                receive,
                crate::api::EventHub::default(),
            );
            app.state.workspaces = vec![Workspace::test_new("header-navigation")];
            app.state.active = Some(0);
            app.state.selected = 0;
            app.state.view.terminal_area = Rect::new(0, 0, 120, 40);
            let source = app.state.workspaces[0].tabs[0].root_pane;
            let sibling = app.state.workspaces[0].test_split(direction);
            if containing_box {
                app.state.workspaces[0]
                    .split_pane_with_view(
                        source,
                        "example.views",
                        "summary",
                        Direction::Vertical,
                        0.5,
                        false,
                    )
                    .unwrap();
            }
            app.state.ensure_test_terminals();
            app.state.focus_pane_in_workspace(0, source);
            assert!(app.set_node_collapsed(0, 0, &[0], true));
            assert_eq!(
                app.directional_pane_target(0, 0, source, navigation),
                Some(sibling),
                "header/strip must be the navigation origin"
            );
            let source_public = app.public_pane_id(0, source).unwrap();
            let response = app.handle_pane_focus_direction(
                "navigate-header".into(),
                PaneFocusDirectionParams {
                    pane_id: Some(source_public.clone()),
                    direction: navigation,
                },
            );
            assert!(serde_json::from_str::<serde_json::Value>(&response)
                .unwrap()
                .get("error")
                .is_none());
            assert_eq!(app.state.workspaces[0].tabs[0].layout.focused(), sibling);
            let projected = app.pane_layout_snapshot(0, 0).unwrap();
            assert!(
                projected
                    .panes
                    .iter()
                    .all(|pane| pane.pane_id != source_public),
                "navigation must not manufacture a body descriptor"
            );
            assert!(!app.state.workspaces[0].tabs[0]
                .layout
                .pane_is_expanded(source));
            app.state.assert_invariants_for_test();
        }
    }
}
