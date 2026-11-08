//! Actor-owned collapse edits and transient sizing-resume boundaries.
use std::collections::{HashMap, HashSet};

use super::App;
use crate::{
    layout::TypedNode,
    view_sizing::{ResumeFact, ResumeFence},
};

#[cfg(test)]
mod tests;

impl App {
    pub(crate) fn set_node_collapsed(
        &mut self,
        workspace: usize,
        tab_index: usize,
        path: &[usize],
        collapsed: bool,
    ) -> bool {
        let Some(tab) = self
            .state
            .workspaces
            .get(workspace)
            .and_then(|workspace| workspace.tabs.get(tab_index))
        else {
            return false;
        };
        let Some(node) = tab.layout.typed_root().at(path) else {
            return false;
        };
        if matches!(node, TypedNode::Collapsed { .. }) == collapsed {
            return false;
        }
        let box_views = tab.layout.auto_view_box_child_members(path);
        let refit_child = !box_views.is_empty()
            && box_views.iter().all(|pane| {
                self.state
                    .view_sizing
                    .active
                    .get(pane)
                    .is_some_and(|entry| entry.scope_valid)
            });
        // Child intent inside an armed designated box re-fits immediately. Do
        // not replace/remove a fence from an earlier enclosing collapse.
        if !collapsed && !refit_child {
            let tab_id = tab.id.clone();
            let parent = &path[..path.len().saturating_sub(1)];
            let Some(members) = tab.layout.node_members(parent) else {
                return false;
            };
            let Some(scope) = self.public_tab_id(workspace, tab_index) else {
                return false;
            };
            let mut namespaces = HashSet::new();
            let mut views = HashMap::new();
            for pane in &members {
                let Some(entry) = self
                    .state
                    .view_sizing
                    .active
                    .get(pane)
                    .filter(|entry| entry.scope_valid)
                else {
                    continue;
                };
                namespaces.insert(entry.extension.clone());
                let (measure, incarnation, _, available) =
                    self.cached_auto_measure(workspace, &scope, entry);
                views.insert(
                    *pane,
                    ResumeFact {
                        measure,
                        source: incarnation.map(|(generation, scope, _)| (generation, scope)),
                        available,
                    },
                );
            }
            if !views.is_empty() {
                let issued = self
                    .host_document_request_watermarks
                    .iter()
                    .filter(|((namespace, _), _)| namespaces.contains(namespace))
                    .map(|(key, stamp)| (key.clone(), *stamp))
                    .collect();
                let viewport = self
                    .state
                    .view_sizing
                    .viewports
                    .get(&tab_id)
                    .map(|(area, cells)| (area.width, area.height, *cells));
                self.state
                    .view_sizing
                    .resume_fences
                    .retain(|fence| !fence.views.keys().any(|pane| views.contains_key(pane)));
                self.state.view_sizing.resume_fences.push(ResumeFence {
                    tab: tab_id.clone(),
                    parent_members: members,
                    views,
                    issued,
                    viewport,
                });
                self.state.view_sizing.pending_geometry.remove(&tab_id);
            }
        }
        let tab = &mut self.state.workspaces[workspace].tabs[tab_index];
        let changed = tab.layout.set_collapsed(path, collapsed);
        if changed {
            // An expansion restores saved shares, not a latent zoom flag that
            // could have followed focus into this collapsed subtree.
            tab.zoomed = false;
        }
        changed
    }

    pub(crate) fn prune_resume_fences(&mut self) {
        let workspaces = &self.state.workspaces;
        let active = &self.state.view_sizing.active;
        self.state.view_sizing.resume_fences.retain(|fence| {
            if fence.views.keys().any(|pane| !active.contains_key(pane)) {
                return false;
            }
            let Some(anchor) = fence.views.keys().next() else {
                return false;
            };
            workspaces
                .iter()
                .flat_map(|workspace| &workspace.tabs)
                .find(|tab| tab.id == fence.tab)
                .is_some_and(|tab| {
                    tab.layout
                        .has_ancestor_members(*anchor, &fence.parent_members)
                })
        });
    }
}
