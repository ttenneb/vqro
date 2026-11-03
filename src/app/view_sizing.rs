//! Actor/event adapters for the neutral transient sizing facts.
use super::App;
use crate::{layout::PaneId, ui::TabSurfaceTarget};
use ratatui::layout::Rect;

impl App {
    pub(crate) fn activate_auto_view(&mut self, workspace: usize, tab_index: usize, pane: PaneId) {
        let Some(tab) = self
            .state
            .workspaces
            .get(workspace)
            .and_then(|ws| ws.tabs.get(tab_index))
        else {
            return;
        };
        let Some(content) = tab.layout.view_at(pane) else {
            return;
        };
        let scope = tab.layout.view_scope(pane);
        if scope.len() != 1 {
            return;
        }
        let live = self
            .state
            .workspaces
            .iter()
            .flat_map(|ws| &ws.tabs)
            .flat_map(|tab| tab.layout.view_panes())
            .collect::<std::collections::HashSet<_>>();
        self.state
            .view_sizing
            .active
            .retain(|pane, _| live.contains(pane));
        self.state
            .view_sizing
            .activate(pane, &content.extension, &content.view, scope[0]);
        if let Some(scope_id) = self.public_tab_id(workspace, tab_index) {
            self.refresh_auto_view_document(&content.extension, &scope_id, 0, 0);
        }
    }

    pub(crate) fn manual_view_sizing_for_pane(
        &mut self,
        workspace: usize,
        tab_index: usize,
        pane: PaneId,
    ) {
        if let Some(tab) = self
            .state
            .workspaces
            .get(workspace)
            .and_then(|ws| ws.tabs.get(tab_index))
        {
            let terminal = if tab.layout.view_at(pane).is_some() {
                tab.layout.view_scope(pane).first().copied().unwrap_or(pane)
            } else {
                pane
            };
            let affected = tab
                .layout
                .view_panes()
                .into_iter()
                .filter(|view| *view == pane || tab.layout.view_scope(*view).contains(&terminal))
                .collect::<Vec<_>>();
            self.state.view_sizing.manual(affected);
        }
    }

    pub(crate) fn manual_view_sizing_for_split(
        &mut self,
        workspace: usize,
        tab_index: usize,
        path: &[bool],
    ) {
        if let Some(tab) = self
            .state
            .workspaces
            .get(workspace)
            .and_then(|ws| ws.tabs.get(tab_index))
        {
            let path = path
                .iter()
                .map(|second| usize::from(*second))
                .collect::<Vec<_>>();
            if let Some(crate::layout::TypedNode::Split { first, second, .. }) =
                tab.layout.typed_root().at(&path)
            {
                let views = [first.as_ref(), second.as_ref()]
                    .into_iter()
                    .filter_map(|node| {
                        if let crate::layout::TypedNode::View { pane, .. } = node {
                            Some(*pane)
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>();
                self.state.view_sizing.manual(views);
            }
        }
    }

    pub(crate) fn refresh_auto_view_document(
        &mut self,
        namespace: &str,
        scope_id: &str,
        generation: u64,
        revision: u64,
    ) {
        if self.state.view_sizing.active.is_empty() {
            return;
        }
        let key = (namespace.to_owned(), scope_id.to_owned());
        let (metrics, incarnation) = self
            .host_document_caches
            .get(&key)
            .and_then(|cache| cache.current())
            .filter(|document| document.namespace == namespace)
            .map(|document| {
                (
                    crate::view_sizing::measure_document(document),
                    (document.generation, scope_id.to_owned(), document.revision),
                )
            })
            .unwrap_or_else(|| {
                (
                    Default::default(),
                    (generation, scope_id.to_owned(), revision),
                )
            });
        let mut updates = Vec::new();
        for (&pane, entry) in &self.state.view_sizing.active {
            if entry.extension != namespace {
                continue;
            }
            let Some((wi, ti)) = self
                .state
                .workspaces
                .iter()
                .enumerate()
                .find_map(|(wi, ws)| ws.find_tab_index_for_pane(pane).map(|ti| (wi, ti)))
            else {
                continue;
            };
            if self.public_tab_id(wi, ti).as_deref() != Some(scope_id) {
                continue;
            }
            let scope = self.state.workspaces[wi].tabs[ti].layout.view_scope(pane);
            if scope.len() != 1 {
                continue;
            }
            let Some(public) = self.public_pane_id(wi, scope[0]) else {
                continue;
            };
            updates.push((
                pane,
                scope[0],
                metrics
                    .get(&(entry.view.clone(), public))
                    .copied()
                    .unwrap_or_default(),
            ));
        }
        for (pane, terminal, measure) in updates {
            if let Some(entry) = self.state.view_sizing.active.get_mut(&pane) {
                entry.terminal = terminal;
                entry.measure = measure;
                entry.incarnation = Some(incarnation.clone());
            }
        }
        let mut targets = Vec::new();
        for (wi, ws) in self.state.workspaces.iter().enumerate() {
            for (ti, tab) in ws.tabs.iter().enumerate() {
                if let Some(viewport) = self.state.view_sizing.viewports.get(&tab.id).copied() {
                    targets.push((wi, ti, viewport));
                }
            }
        }
        for (wi, ti, (area, cells)) in targets {
            if self.resize_auto_views(
                TabSurfaceTarget {
                    workspace_index: wi,
                    tab_index: ti,
                },
                area,
                cells,
            ) {
                crate::ui::resize_tab_surface(
                    &self.state,
                    &self.terminal_runtimes,
                    wi,
                    ti,
                    area,
                    cells,
                );
            }
        }
    }

    /// Geometry-authority event only, never a per-client render call.
    pub(crate) fn resize_auto_views(
        &mut self,
        target: TabSurfaceTarget,
        area: Rect,
        cells: crate::kitty_graphics::HostCellSize,
    ) -> bool {
        if self.collections_mutations_blocked || self.session_persistence_blocked {
            return false;
        }
        if self.state.view_sizing.viewports.len() >= crate::admission::MAX_SESSION_PANES {
            let tabs = self
                .state
                .workspaces
                .iter()
                .flat_map(|ws| &ws.tabs)
                .map(|tab| tab.id.clone())
                .collect::<std::collections::HashSet<_>>();
            self.state
                .view_sizing
                .viewports
                .retain(|id, _| tabs.contains(id));
        }
        let Some(tab) = self
            .state
            .workspaces
            .get_mut(target.workspace_index)
            .and_then(|ws| ws.tabs.get_mut(target.tab_index))
        else {
            return false;
        };
        if tab.zoomed {
            return false;
        }
        self.state
            .view_sizing
            .viewports
            .insert(tab.id.clone(), (area, cells));
        let mut measures = std::collections::HashMap::new();
        for (&pane, entry) in &mut self.state.view_sizing.active {
            if tab.layout.view_at(pane).is_none() {
                continue;
            }
            let scope = tab.layout.view_scope(pane);
            if scope.len() != 1 {
                entry.incarnation = None;
                continue;
            }
            if entry.terminal != scope[0] {
                entry.terminal = scope[0];
                entry.incarnation = None;
                entry.measure = Default::default();
            }
            measures.insert(pane, entry.measure);
        }
        let changed = tab.layout.auto_size_views(area, &measures);
        if changed {
            self.schedule_session_save();
            self.emit_layout_updated_event(target.workspace_index, target.tab_index);
        }
        changed
    }
}
