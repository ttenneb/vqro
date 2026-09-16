//! Immutable method policy for the published generation-1 Herdr endpoint bridge.
//!
//! New vqro API methods travel through negotiated named controls and must never
//! expand this list.

pub const METHODS: &[&str] = &[
    "client_shell.surface.set",
    "command.invoke",
    "integration.install",
    "integration.list",
    "layout.set_split_ratio",
    "pane.close",
    "pane.copy_motion",
    "pane.copy_search",
    "pane.edit_scrollback",
    "pane.focus",
    "pane.focus_direction",
    "pane.input.set",
    "pane.link.activate",
    "pane.rename",
    "pane.resize",
    "pane.scroll",
    "pane.selection.read",
    "pane.split",
    "pane.swap",
    "pane.zoom",
    "product_announcement.dismiss",
    "release_notes.dismiss",
    "server.reload_config",
    "tab.close",
    "tab.create",
    "tab.focus",
    "tab.move",
    "tab.rename",
    "workspace.close",
    "workspace.create",
    "workspace.focus",
    "workspace.move",
    "workspace.move_block",
    "workspace.rename",
    "worktree.create",
    "worktree.list",
    "worktree.open",
    "worktree.remove",
];

pub fn supports_name(method: &str) -> bool {
    METHODS.contains(&method)
}

pub fn supports(method: &crate::api::schema::Method) -> bool {
    supports_name(crate::api::api_method_name(method))
}

#[cfg(test)]
mod tests {
    #[test]
    fn frozen_method_names_match_the_published_fixture_exactly() {
        let fixture: std::collections::BTreeMap<String, String> =
            serde_json::from_str(include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/endpoint-method-shapes-v1.json"
            )))
            .unwrap();
        assert_eq!(
            fixture.len(),
            38,
            "generation-1 method fixture is append-closed"
        );
        assert_eq!(super::METHODS.len(), 38);
        assert_eq!(
            super::METHODS,
            fixture.keys().map(String::as_str).collect::<Vec<_>>()
        );
    }
}
