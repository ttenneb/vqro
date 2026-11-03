//! Actor/event adapters for neutral, transient sizing facts. Never called by render.
use super::App;
use crate::{
    layout::PaneId,
    ui::TabSurfaceTarget,
    view_sizing::{CachedDocument, DocumentMeasure, ViewMeasure},
};
use ratatui::layout::Rect;
use unicode_width::UnicodeWidthStr;

impl App {
    pub(crate) fn auto_view_geometry_blocked(&self) -> bool {
        self.collections_mutations_blocked || self.session_persistence_blocked
    }

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
        self.state
            .view_sizing
            .activate(pane, &content.extension, &content.view, scope[0]);
        self.prune_auto_view_sizing();
        if let Some(scope_id) = self.public_tab_id(workspace, tab_index) {
            self.refresh_auto_view_document(&content.extension, &scope_id, 0, 0);
        }
    }

    /// Topology invalidation, not a render/provider polling path. Keep valid
    /// hidden scopes, but never retain a history of moved/closed ownership.
    pub(crate) fn prune_auto_view_sizing(&mut self) {
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
        let mut keys = std::collections::HashSet::new();
        let mut tabs = std::collections::HashSet::new();
        let mut public_tabs = std::collections::HashSet::new();
        for (wi, workspace) in self.state.workspaces.iter().enumerate() {
            for (ti, tab) in workspace.tabs.iter().enumerate() {
                tabs.insert(tab.id.clone());
                if let Some(scope) = self.public_tab_id(wi, ti) {
                    public_tabs.insert(scope.clone());
                    for view in tab.layout.view_panes() {
                        if let Some(entry) = self.state.view_sizing.active.get(&view) {
                            keys.insert((entry.extension.clone(), scope.clone()));
                        }
                    }
                }
            }
        }
        let live_terminals = self
            .state
            .workspaces
            .iter()
            .flat_map(|ws| &ws.tabs)
            .flat_map(|tab| tab.layout.pane_ids())
            .collect::<std::collections::HashSet<_>>();
        let terminals = self
            .state
            .view_sizing
            .active
            .values()
            .map(|entry| entry.terminal)
            .collect::<std::collections::HashSet<_>>();
        self.state
            .view_sizing
            .documents
            .retain(|key, _| keys.contains(key));
        self.state
            .view_sizing
            .label_widths
            .retain(|pane, _| terminals.contains(pane) && live_terminals.contains(pane));
        self.state
            .view_sizing
            .viewports
            .retain(|tab, _| tabs.contains(tab));
        self.state
            .view_sizing
            .pending_geometry
            .retain(|tab| tabs.contains(tab));
        self.state
            .view_sizing
            .visible_tabs
            .retain(|tab| public_tabs.contains(tab));
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

    fn auto_scope_width(&self, workspace: usize, pane: PaneId) -> u16 {
        let public = self.public_pane_id(workspace, pane).unwrap_or_default();
        let terminal = self
            .state
            .workspaces
            .get(workspace)
            .and_then(|ws| ws.terminal_id(pane))
            .and_then(|id| self.state.terminals.get(id));
        let label = terminal
            .filter(|terminal| terminal.is_agent_terminal())
            .and_then(|terminal| {
                terminal
                    .effective_display_agent()
                    .or_else(|| terminal.agent_name.clone())
                    .or_else(|| terminal.effective_agent_label().map(str::to_owned))
            })
            .unwrap_or(public);
        label.width().min(4092) as u16
    }

    fn cached_auto_measure(
        &self,
        workspace: usize,
        scope: &str,
        entry: &crate::view_sizing::AutoView,
    ) -> (ViewMeasure, Option<(u64, String, u64)>, u16) {
        let width = self.auto_scope_width(workspace, entry.terminal);
        let public = self
            .public_pane_id(workspace, entry.terminal)
            .unwrap_or_default();
        let document = self
            .state
            .view_sizing
            .documents
            .get(&(entry.extension.clone(), scope.to_owned()));
        let measure = document
            .and_then(|document| document.measurements.get(&(entry.view.clone(), public)))
            .copied()
            .unwrap_or(DocumentMeasure {
                body: ViewMeasure::default(),
                header: entry.view.width().saturating_add(15).min(4092) as u16,
            });
        (
            measure.with_scope(width),
            document.map(|document| document.incarnation.clone()),
            width,
        )
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
        let Some((wi, ti)) = self.parse_tab_id(scope_id) else {
            if self
                .state
                .view_sizing
                .documents
                .contains_key(&(namespace.to_owned(), scope_id.to_owned()))
            {
                self.prune_auto_view_sizing();
            }
            return;
        };
        if !self.state.workspaces[wi].tabs[ti]
            .layout
            .view_panes()
            .iter()
            .any(|pane| {
                self.state
                    .view_sizing
                    .active
                    .get(pane)
                    .is_some_and(|entry| entry.extension == namespace)
            })
        {
            return;
        }
        let key = (namespace.to_owned(), scope_id.to_owned());
        let document = self
            .host_document_caches
            .get(&key)
            .and_then(|cache| cache.current())
            .filter(|document| document.namespace == namespace);
        let cached = document
            .map(|document| CachedDocument {
                incarnation: (document.generation, scope_id.to_owned(), document.revision),
                measurements: crate::view_sizing::measure_document(document),
            })
            .unwrap_or(CachedDocument {
                incarnation: (generation, scope_id.to_owned(), revision),
                measurements: Default::default(),
            });
        self.state.view_sizing.documents.insert(key, cached);
        self.refresh_cached_auto_target(wi, ti);
    }

    /// Metadata notifications only invalidate a target if the actual host label
    /// width changed. Status/age updates do not visit geometry or other tabs.
    pub(crate) fn refresh_auto_view_label(&mut self, workspace: usize, pane: PaneId) {
        let Some(before) = self.state.view_sizing.label_widths.get(&pane).copied() else {
            return;
        };
        if before == self.auto_scope_width(workspace, pane) {
            return;
        }
        if let Some(ti) = self
            .state
            .workspaces
            .get(workspace)
            .and_then(|ws| ws.find_tab_index_for_pane(pane))
        {
            self.refresh_cached_auto_target(workspace, ti);
        }
    }

    pub(crate) fn refresh_cached_auto_target(&mut self, workspace: usize, tab_index: usize) {
        let Some(scope) = self.public_tab_id(workspace, tab_index) else {
            return;
        };
        let tab = &self.state.workspaces[workspace].tabs[tab_index];
        let views = tab.layout.view_panes();
        let mut visible = std::collections::HashSet::new();
        if !tab.zoomed {
            tab.layout.any_visible_pane(|pane| {
                visible.insert(pane);
                false
            });
        }
        let mut dirty = false;
        for pane in views {
            let Some(entry) = self.state.view_sizing.active.get(&pane) else {
                continue;
            };
            if !entry.scope_valid {
                continue;
            }
            let terminal = entry.terminal;
            let (measure, incarnation, width) = self.cached_auto_measure(workspace, &scope, entry);
            if let Some(entry) = self.state.view_sizing.active.get_mut(&pane) {
                entry.dirty |= entry.measure != measure;
                entry.measure = measure;
                entry.incarnation = incarnation;
                dirty |= entry.dirty && visible.contains(&pane);
            }
            self.state.view_sizing.label_widths.insert(terminal, width);
        }
        if !dirty || !self.state.view_sizing.visible_tabs.contains(&scope) {
            return;
        }
        let tab = &self.state.workspaces[workspace].tabs[tab_index];
        if self.state.view_sizing.viewports.contains_key(&tab.id) {
            // The host geometry controller owns actual sizing, including
            // accessory/preview exclusions. A document event only invalidates
            // its affected target; it never resizes a PTY itself.
            self.state
                .view_sizing
                .pending_geometry
                .insert(tab.id.clone());
        }
    }

    /// Geometry-authority event only. Rebind placement against current complete
    /// membership and seed from accepted cached metrics, never a document scan.
    pub(crate) fn resize_auto_views(
        &mut self,
        target: TabSurfaceTarget,
        area: Rect,
        cells: crate::kitty_graphics::HostCellSize,
    ) -> bool {
        if self.auto_view_geometry_blocked() {
            return false;
        }
        let Some(scope_id) = self.public_tab_id(target.workspace_index, target.tab_index) else {
            return false;
        };
        let Some(tab) = self
            .state
            .workspaces
            .get(target.workspace_index)
            .and_then(|ws| ws.tabs.get(target.tab_index))
        else {
            return false;
        };
        if tab.zoomed {
            return false;
        }
        let tab_id = tab.id.clone();
        let bindings = tab.layout.single_terminal_view_scopes();
        let placed = tab.layout.view_panes();
        for pane in placed {
            if !bindings.contains_key(&pane) {
                if let Some(entry) = self.state.view_sizing.active.get_mut(&pane) {
                    entry.scope_valid = false;
                    entry.incarnation = None;
                    entry.dirty = false;
                }
            }
        }
        self.state.view_sizing.visible_tabs.insert(scope_id.clone());
        self.state
            .view_sizing
            .viewports
            .insert(tab_id, (area, cells));
        let mut measures = std::collections::HashMap::new();
        for (pane, terminal) in bindings {
            let Some(entry) = self.state.view_sizing.active.get_mut(&pane) else {
                continue;
            };
            entry.terminal = terminal;
            entry.scope_valid = true;
            let (measure, incarnation, width) = self.cached_auto_measure(
                target.workspace_index,
                &scope_id,
                &self.state.view_sizing.active[&pane],
            );
            if let Some(entry) = self.state.view_sizing.active.get_mut(&pane) {
                entry.measure = measure;
                entry.incarnation = incarnation;
                entry.dirty = false;
            }
            self.state.view_sizing.label_widths.insert(terminal, width);
            measures.insert(pane, measure);
        }
        let changed = self.state.workspaces[target.workspace_index].tabs[target.tab_index]
            .layout
            .auto_size_views(area, &measures);
        if changed {
            self.schedule_session_save();
            self.emit_layout_updated_event(target.workspace_index, target.tab_index);
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn hidden_stack_view_metrics_cache_without_pending_or_geometry() {
        let (_send, receive) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            receive,
            crate::api::EventHub::default(),
        );
        let mut workspace = crate::workspace::Workspace::test_new("hidden");
        let root = workspace.tabs[0].root_pane;
        let (_, view) = workspace
            .split_pane_with_view(
                root,
                "sample.views",
                "rows",
                ratatui::layout::Direction::Horizontal,
                0.5,
                false,
            )
            .unwrap();
        let outer = crate::terminal_container::TerminalContainerId::parse(91_001).unwrap();
        let empty = crate::terminal_container::TerminalContainerId::parse(91_002).unwrap();
        assert!(workspace.tabs[0].layout.new_stack_at(&[], outer));
        assert!(workspace.tabs[0].layout.insert_into_stack(
            outer,
            1,
            crate::layout::TypedNode::empty_stack(empty)
        ));
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.ensure_test_terminals();
        let target = TabSurfaceTarget {
            workspace_index: 0,
            tab_index: 0,
        };
        let area = Rect::new(0, 0, 200, 40);
        let cells = crate::kitty_graphics::HostCellSize::default();
        app.resize_auto_views(target, area, cells);
        app.activate_auto_view(0, 0, view);
        app.resize_auto_views(target, area, cells);
        app.state.view_sizing.pending_geometry.clear();
        assert!(app.state.workspaces[0].tabs[0]
            .layout
            .select_stack_child(outer, 1));
        let scope = app.public_tab_id(0, 0).unwrap();
        let public = app.public_pane_id(0, root).unwrap();
        app.state.view_sizing.documents.insert(
            ("sample.views".into(), scope.clone()),
            CachedDocument {
                incarnation: (1, scope, 2),
                measurements: std::collections::HashMap::from([(
                    ("rows".into(), public),
                    DocumentMeasure {
                        body: ViewMeasure { rows: 2, width: 80 },
                        header: 20,
                    },
                )]),
            },
        );
        let before = app.state.workspaces[0].tabs[0].layout.typed_root().clone();
        crate::layout::TileLayout::take_auto_gathers_for_test();
        crate::layout::TileLayout::take_geometry_gathers_for_test();
        app.refresh_cached_auto_target(0, 0);
        assert!(
            app.state.view_sizing.pending_geometry.is_empty(),
            "a hidden member in a visible tab cannot invalidate presentation"
        );
        assert_eq!(crate::layout::TileLayout::take_auto_gathers_for_test(), 0);
        assert_eq!(
            crate::layout::TileLayout::take_geometry_gathers_for_test(),
            0
        );
        assert_eq!(&before, app.state.workspaces[0].tabs[0].layout.typed_root());
        assert_eq!(
            app.state.view_sizing.active[&view].measure.width, 80,
            "hidden metrics are cached"
        );
        let live = app.public_tab_id(0, 0).unwrap();
        let closed = format!("{}:t2", app.state.workspaces[0].id);
        app.state.workspaces[0].next_public_tab_number = 3;
        app.state.view_sizing.documents.insert(
            ("sample.views".into(), closed.clone()),
            CachedDocument {
                incarnation: (1, closed.clone(), 1),
                measurements: Default::default(),
            },
        );
        app.refresh_auto_view_document("sample.views", &closed, 1, 1);
        assert!(
            app.state
                .view_sizing
                .documents
                .contains_key(&("sample.views".into(), live)),
            "topology cleanup preserves a valid hidden cache"
        );
        assert!(app.state.view_sizing.active.contains_key(&view));
        assert!(app.state.workspaces[0].tabs[0]
            .layout
            .select_stack_child(outer, 0));
        app.resize_auto_views(target, area, cells);
        assert_eq!(
            crate::layout::TileLayout::take_auto_gathers_for_test(),
            1,
            "selection applies cached facts"
        );
        app.state.assert_invariants_for_test();
    }

    #[tokio::test]
    async fn moving_the_same_terminal_and_view_to_another_tab_cannot_consume_old_scope_metrics() {
        let (_send, receive) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            receive,
            crate::api::EventHub::default(),
        );
        let mut workspace = crate::workspace::Workspace::test_new("move");
        let root = workspace.tabs[0].root_pane;
        let (_, view) = workspace
            .split_pane_with_view(
                root,
                "sample.views",
                "rows",
                ratatui::layout::Direction::Horizontal,
                0.5,
                false,
            )
            .unwrap();
        workspace.test_add_tab(Some("destination"));
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.ensure_test_terminals();
        let area = Rect::new(0, 0, 200, 40);
        let cells = crate::kitty_graphics::HostCellSize::default();
        app.resize_auto_views(
            TabSurfaceTarget {
                workspace_index: 0,
                tab_index: 0,
            },
            area,
            cells,
        );
        app.activate_auto_view(0, 0, view);
        let old = app.public_tab_id(0, 0).unwrap();
        let public = app.public_pane_id(0, root).unwrap();
        app.state.view_sizing.documents.insert(
            ("sample.views".into(), old.clone()),
            CachedDocument {
                incarnation: (7, old.clone(), 9),
                measurements: std::collections::HashMap::from([(
                    ("rows".into(), public),
                    DocumentMeasure {
                        body: ViewMeasure { rows: 7, width: 80 },
                        header: 20,
                    },
                )]),
            },
        );
        app.resize_auto_views(
            TabSurfaceTarget {
                workspace_index: 0,
                tab_index: 0,
            },
            area,
            cells,
        );
        assert_eq!(app.state.view_sizing.active[&view].measure.width, 80);
        // Move each complete content bundle while keeping the two host tab
        // identities/numbers: the terminal and view PaneIds stay unchanged.
        let tabs = &mut app.state.workspaces[0].tabs;
        let identities = [
            (tabs[0].id.clone(), tabs[0].number),
            (tabs[1].id.clone(), tabs[1].number),
        ];
        tabs.swap(0, 1);
        for (tab, (id, number)) in tabs.iter_mut().zip(identities) {
            tab.id = id;
            tab.number = number;
        }
        let new = app.public_tab_id(0, 1).unwrap();
        assert_ne!(old, new);
        app.resize_auto_views(
            TabSurfaceTarget {
                workspace_index: 0,
                tab_index: 1,
            },
            area,
            cells,
        );
        let policy = &app.state.view_sizing.active[&view];
        assert_eq!(policy.terminal, root);
        assert_eq!(policy.measure.rows, 1);
        assert_eq!(policy.measure.width, 32);
        assert!(
            policy.incarnation.is_none(),
            "old namespace/tab incarnation is not reused before a new scoped publication"
        );
        let retired = app.state.workspaces[0].tabs[0].root_pane;
        let retired_terminal = app.state.workspaces[0]
            .terminal_id(retired)
            .unwrap()
            .clone();
        app.state.workspaces[0].active_tab = 0;
        assert!(app.state.workspaces[0].close_active_tab());
        app.state.terminals.remove(&retired_terminal);
        app.refresh_auto_view_document("sample.views", &old, 7, 9);
        assert!(
            app.state.view_sizing.documents.is_empty(),
            "closed source scope is evicted without another activation"
        );
        let live = app.public_tab_id(0, 0).unwrap();
        app.state.view_sizing.documents.insert(
            ("sample.views".into(), live.clone()),
            CachedDocument {
                incarnation: (7, live.clone(), 10),
                measurements: Default::default(),
            },
        );
        app.state.workspaces[0].next_public_tab_number = 2000;
        for index in 0..32 {
            let closed = format!("{}:t{}", app.state.workspaces[0].id, 1000 + index);
            app.state.view_sizing.documents.insert(
                ("sample.views".into(), closed.clone()),
                CachedDocument {
                    incarnation: (7, closed.clone(), 1),
                    measurements: Default::default(),
                },
            );
            app.state
                .view_sizing
                .label_widths
                .insert(PaneId::from_raw(1_000_000 + index), 42);
            app.refresh_auto_view_document("sample.views", &closed, 7, 1);
            assert_eq!(
                app.state.view_sizing.documents.len(),
                1,
                "scope history cannot accumulate"
            );
            assert!(app
                .state
                .view_sizing
                .documents
                .contains_key(&("sample.views".into(), live.clone())));
            assert!(
                app.state.view_sizing.label_widths.len() <= 1,
                "closed-terminal history cannot accumulate"
            );
            assert!(app.state.view_sizing.active.contains_key(&view));
        }
        app.state.assert_invariants_for_test();
    }

    #[tokio::test]
    async fn cached_metrics_only_touch_the_changed_live_target_and_names_invalidate_width() {
        let (_send, receive) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            receive,
            crate::api::EventHub::default(),
        );
        let mut roots = Vec::new();
        let mut views = Vec::new();
        for index in 0..2 {
            let mut workspace = crate::workspace::Workspace::test_new(&format!("target {index}"));
            let root = workspace.tabs[0].root_pane;
            let (_, view) = workspace
                .split_pane_with_view(
                    root,
                    "sample.views",
                    "rows",
                    ratatui::layout::Direction::Horizontal,
                    0.5,
                    false,
                )
                .unwrap();
            roots.push(root);
            views.push(view);
            app.state.workspaces.push(workspace);
        }
        app.state.active = Some(0);
        app.state.ensure_test_terminals();
        let area = Rect::new(0, 0, 200, 40);
        let cells = crate::kitty_graphics::HostCellSize::default();
        for (index, &view) in views.iter().enumerate() {
            app.resize_auto_views(
                TabSurfaceTarget {
                    workspace_index: index,
                    tab_index: 0,
                },
                area,
                cells,
            );
            app.activate_auto_view(index, 0, view);
            app.resize_auto_views(
                TabSurfaceTarget {
                    workspace_index: index,
                    tab_index: 0,
                },
                area,
                cells,
            );
            app.state.view_sizing.pending_geometry.clear();
        }
        let scope_a = app.public_tab_id(0, 0).unwrap();
        let scope_b = app.public_tab_id(1, 0).unwrap();
        let public_a = app.public_pane_id(0, roots[0]).unwrap();
        app.state.view_sizing.documents.insert(
            ("sample.views".into(), scope_a.clone()),
            CachedDocument {
                incarnation: (1, scope_a, 1),
                measurements: std::collections::HashMap::from([(
                    ("rows".into(), public_a),
                    DocumentMeasure {
                        body: ViewMeasure { rows: 2, width: 70 },
                        header: 20,
                    },
                )]),
            },
        );
        let other = app.state.workspaces[1].tabs[0].layout.typed_root().clone();
        crate::layout::TileLayout::take_auto_gathers_for_test();
        app.refresh_cached_auto_target(0, 0);
        assert_eq!(
            crate::layout::TileLayout::take_auto_gathers_for_test(),
            0,
            "document callback never owns PTY/layout resize"
        );
        let pending = std::mem::take(&mut app.state.view_sizing.pending_geometry);
        assert_eq!(pending.len(), 1);
        assert!(pending.contains(&app.state.workspaces[0].tabs[0].id));
        app.resize_auto_views(
            TabSurfaceTarget {
                workspace_index: 0,
                tab_index: 0,
            },
            area,
            cells,
        );
        assert_eq!(
            crate::layout::TileLayout::take_auto_gathers_for_test(),
            1,
            "only changed target allocates at authority callback"
        );
        assert_eq!(
            &other,
            app.state.workspaces[1].tabs[0].layout.typed_root(),
            "unrelated target unchanged"
        );
        app.refresh_cached_auto_target(0, 0);
        assert_eq!(
            crate::layout::TileLayout::take_auto_gathers_for_test(),
            0,
            "equal facts do not gather or resize"
        );
        let terminal = app.state.workspaces[0]
            .terminal_id(roots[0])
            .unwrap()
            .clone();
        let metadata = app.state.terminals.get_mut(&terminal).unwrap();
        metadata.set_agent_name("a".repeat(64));
        assert!(
            metadata.effective_agent_label().is_none(),
            "regression uses a named-only terminal, not a detected agent"
        );
        app.refresh_auto_view_label(0, roots[0]);
        assert_eq!(crate::layout::TileLayout::take_auto_gathers_for_test(), 0);
        assert_eq!(
            std::mem::take(&mut app.state.view_sizing.pending_geometry).len(),
            1
        );
        app.resize_auto_views(
            TabSurfaceTarget {
                workspace_index: 0,
                tab_index: 0,
            },
            area,
            cells,
        );
        assert_eq!(
            crate::layout::TileLayout::take_auto_gathers_for_test(),
            1,
            "rename invalidates only its target"
        );
        let info = app.state.workspaces[0].tabs[0]
            .layout
            .panes(area)
            .into_iter()
            .find(|info| info.id == views[0])
            .unwrap();
        assert!(
            info.rect.width >= 84,
            "full permitted host scope label fits below the max cap"
        );
        app.refresh_auto_view_label(0, roots[0]);
        assert_eq!(
            crate::layout::TileLayout::take_auto_gathers_for_test(),
            0,
            "unchanged label/status tick does no geometry work"
        );
        let public_b = app.public_pane_id(1, roots[1]).unwrap();
        app.state.view_sizing.documents.insert(
            ("sample.views".into(), scope_b.clone()),
            CachedDocument {
                incarnation: (1, scope_b.clone(), 2),
                measurements: std::collections::HashMap::from([(
                    ("rows".into(), public_b),
                    DocumentMeasure {
                        body: ViewMeasure { rows: 2, width: 80 },
                        header: 20,
                    },
                )]),
            },
        );
        app.state.view_sizing.visible_tabs.remove(&scope_b);
        let tab = app.state.workspaces[1].tabs[0].id.clone();
        app.state.view_sizing.viewports.remove(&tab);
        let before = app.state.workspaces[1].tabs[0].layout.typed_root().clone();
        app.refresh_cached_auto_target(1, 0);
        assert_eq!(
            crate::layout::TileLayout::take_auto_gathers_for_test(),
            0,
            "hidden target caches facts, never replays an old viewport"
        );
        assert_eq!(&before, app.state.workspaces[1].tabs[0].layout.typed_root());
        app.resize_auto_views(
            TabSurfaceTarget {
                workspace_index: 1,
                tab_index: 0,
            },
            Rect::new(0, 0, 160, 40),
            cells,
        );
        assert_eq!(
            crate::layout::TileLayout::take_auto_gathers_for_test(),
            1,
            "selection applies cached facts at fresh controlling geometry"
        );
        app.state.assert_invariants_for_test();
    }
}
