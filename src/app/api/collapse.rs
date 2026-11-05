//! Neutral desired-state collapse APIs. Anonymous paths never imply identity.
use super::responses::{encode_error, encode_success};
use crate::{
    api::schema::{
        LayoutCollapseDescription, LayoutCollapseTarget, LayoutExportParams, LayoutPathStep,
        LayoutSplitChild, PaneTarget, ResponseResult,
    },
    app::App,
    layout::TypedNode,
};

#[cfg(test)]
mod tests;

impl App {
    pub(super) fn handle_layout_collapse_describe(
        &mut self,
        id: String,
        params: LayoutExportParams,
    ) -> String {
        let Some((workspace, tab_index)) = self.resolve_layout_export_target(&params) else {
            return encode_error(id, "layout_not_found", "layout target not found");
        };
        let Some(layout) = self.layout_tree_description(workspace, tab_index) else {
            return encode_error(id, "layout_not_found", "layout unavailable");
        };
        let tree_fingerprint = self.state.workspaces[workspace].tabs[tab_index]
            .layout
            .tree_fingerprint()
            .to_owned();
        encode_success(
            id,
            ResponseResult::LayoutCollapseDescription {
                collapse: LayoutCollapseDescription {
                    kind: "layout.collapse.v1".into(),
                    boot_id: self.runtime_incarnation.clone(),
                    tree_fingerprint,
                    layout,
                },
            },
        )
    }

    pub(crate) fn check_collapse_target(
        &self,
        target: &LayoutCollapseTarget,
    ) -> Result<(usize, usize, Vec<usize>), (&'static str, &'static str)> {
        if target.boot_id != self.runtime_incarnation || target.tree_fingerprint.len() != 64 {
            return Err((
                "stale_layout_target",
                "layout target belongs to another incarnation or tree",
            ));
        }
        if target.path.len() > 128 {
            return Err((
                "invalid_layout_path",
                "layout path exceeds the structural bound",
            ));
        }
        let Some((workspace, tab_index)) = self.parse_tab_id(&target.tab_id) else {
            return Err(("layout_not_found", "layout target not found"));
        };
        let layout = &self.state.workspaces[workspace].tabs[tab_index].layout;
        // Deliberately independent of the cached projection fingerprint: a missed
        // invalidation must fail closed, not authorize a different subtree.
        if target.tree_fingerprint != layout.authoritative_fingerprint() {
            return Err((
                "stale_layout_target",
                "layout changed; obtain a fresh target",
            ));
        }
        let mut node = layout.typed_root();
        let mut path = Vec::with_capacity(target.path.len());
        for step in &target.path {
            let (next, index) = match (step, node.content()) {
                (
                    LayoutPathStep::Split(LayoutSplitChild::First),
                    TypedNode::Split { first, .. },
                ) => (first.as_ref(), 0),
                (
                    LayoutPathStep::Split(LayoutSplitChild::Second),
                    TypedNode::Split { second, .. },
                ) => (second.as_ref(), 1),
                (LayoutPathStep::StackChild(index), TypedNode::Stack { children, .. }) => {
                    let Some(child) = children.get(*index) else {
                        return Err(("invalid_layout_path", "stack child no longer exists"));
                    };
                    (child, *index)
                }
                _ => {
                    return Err((
                        "invalid_layout_path",
                        "path step does not match the current container",
                    ))
                }
            };
            path.push(index);
            node = next;
        }
        Ok((workspace, tab_index, path))
    }

    pub(super) fn handle_layout_collapse(
        &mut self,
        id: String,
        target: LayoutCollapseTarget,
        collapsed: bool,
    ) -> String {
        let (workspace, tab_index, path) = match self.check_collapse_target(&target) {
            Ok(resolved) => resolved,
            Err((code, message)) => return encode_error(id, code, message),
        };
        self.finish_collapse_edit(id, workspace, tab_index, &path, collapsed)
    }

    pub(super) fn handle_pane_collapse(
        &mut self,
        id: String,
        target: PaneTarget,
        collapsed: bool,
    ) -> String {
        let Some((workspace, pane)) = self.parse_pane_id(&target.pane_id) else {
            return encode_error(id, "pane_not_found", "pane not found");
        };
        let Some(tab_index) = self.state.workspaces[workspace].find_tab_index_for_pane(pane) else {
            return encode_error(id, "pane_not_found", "pane not found");
        };
        let Some(path) = self.state.workspaces[workspace].tabs[tab_index]
            .layout
            .pane_path(pane)
        else {
            return encode_error(id, "pane_not_found", "pane not found");
        };
        self.finish_collapse_edit(id, workspace, tab_index, &path, collapsed)
    }

    fn finish_collapse_edit(
        &mut self,
        id: String,
        workspace: usize,
        tab_index: usize,
        path: &[usize],
        collapsed: bool,
    ) -> String {
        if self.auto_view_geometry_blocked() {
            return encode_error(
                id,
                "layout_mutation_blocked",
                "layout is fenced by pending persistence",
            );
        }
        if self.set_node_collapsed(workspace, tab_index, path, collapsed) {
            self.schedule_session_save();
            self.emit_layout_updated_event(workspace, tab_index);
        }
        encode_success(id, ResponseResult::Ok {})
    }
}
