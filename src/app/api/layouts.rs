use std::path::PathBuf;

use ratatui::layout::Direction;

use crate::api::schema::{
    EventData, EventEnvelope, EventKind, LayoutApplyParams, LayoutDescription, LayoutExportParams,
    LayoutNode, LayoutPane, LayoutSetSplitRatioParams, LayoutTreeDescription, LayoutTreeFocus,
    LayoutTreeNode, ResponseResult, SplitDirection,
};
use crate::app::{App, Mode};
use crate::layout::{LayoutLeaf, PaneId, TileLayout, TypedNode};
use crate::workspace::NewPane;

use super::responses::{encode_error, encode_success};

const MAX_LAYOUT_PANES: usize = 24;
const MAX_LAYOUT_DEPTH: usize = 16;

impl App {
    pub(super) fn handle_layout_export(
        &mut self,
        id: String,
        params: LayoutExportParams,
    ) -> String {
        let Some((ws_idx, tab_idx)) = self.resolve_layout_export_target(&params) else {
            return encode_error(id, "layout_not_found", "layout target not found");
        };
        let Some(layout) = self.layout_description(ws_idx, tab_idx) else {
            return encode_error(id, "layout_not_found", "layout unavailable");
        };

        encode_success(id, ResponseResult::LayoutExport { layout })
    }

    pub(super) fn handle_layout_tree_export(
        &mut self,
        id: String,
        params: LayoutExportParams,
    ) -> String {
        let Some((ws_idx, tab_idx)) = self.resolve_layout_export_target(&params) else {
            return encode_error(id, "layout_not_found", "layout target not found");
        };
        let Some(layout) = self.layout_tree_description(ws_idx, tab_idx) else {
            return encode_error(id, "layout_not_found", "layout unavailable");
        };
        encode_success(id, ResponseResult::LayoutTreeExport { layout })
    }

    fn layout_tree_description(
        &self,
        ws_idx: usize,
        tab_idx: usize,
    ) -> Option<LayoutTreeDescription> {
        let tab = self.state.workspaces.get(ws_idx)?.tabs.get(tab_idx)?;
        let focused = match tab.layout.focused_leaf() {
            LayoutLeaf::Pane(pane) => LayoutTreeFocus::Pane {
                pane_id: self.public_pane_id(ws_idx, pane)?,
            },
            LayoutLeaf::Stack(id) => LayoutTreeFocus::Stack {
                box_id: crate::terminal_container::format_terminal_container_id(id),
            },
        };
        Some(LayoutTreeDescription {
            kind: "layout.tree.v1".into(),
            workspace_id: self.public_workspace_id(ws_idx),
            tab_id: self.public_tab_id(ws_idx, tab_idx)?,
            zoomed: tab.zoomed,
            focused,
            root: self.layout_tree_node(ws_idx, tab.layout.typed_root())?,
        })
    }

    fn layout_tree_node(&self, ws_idx: usize, node: &TypedNode) -> Option<LayoutTreeNode> {
        Some(match node {
            TypedNode::Leaf(pane) => LayoutTreeNode::Pane {
                pane_id: self.public_pane_id(ws_idx, *pane)?,
            },
            TypedNode::View {
                pane,
                extension,
                view,
            } => LayoutTreeNode::View {
                pane_id: self.public_pane_id(ws_idx, *pane)?,
                extension: extension.clone(),
                view: view.clone(),
            },
            TypedNode::Stack {
                id,
                selected,
                children,
            } => LayoutTreeNode::Stack {
                box_id: crate::terminal_container::format_terminal_container_id(*id),
                selected: *selected,
                children: children
                    .iter()
                    .map(|child| self.layout_tree_node(ws_idx, child))
                    .collect::<Option<Vec<_>>>()?,
            },
            TypedNode::Split {
                direction,
                ratio,
                first,
                second,
            } => LayoutTreeNode::Split {
                direction: match direction {
                    Direction::Horizontal => SplitDirection::Right,
                    Direction::Vertical => SplitDirection::Down,
                },
                ratio: *ratio,
                first: Box::new(self.layout_tree_node(ws_idx, first)?),
                second: Box::new(self.layout_tree_node(ws_idx, second)?),
            },
        })
    }

    pub(super) fn handle_layout_apply(&mut self, id: String, params: LayoutApplyParams) -> String {
        let replace_target = match params.tab_id.as_deref() {
            Some(tab_id) => match self.parse_tab_id(tab_id) {
                Some(target) => Some(target),
                None => {
                    return encode_error(id, "tab_not_found", format!("tab {tab_id} not found"))
                }
            },
            None => None,
        };
        if replace_target.is_some() && params.workspace_id.is_some() {
            return encode_error(
                id,
                "invalid_target",
                "use either tab_id or workspace_id, not both",
            );
        }

        let ws_idx = if let Some((ws_idx, _)) = replace_target {
            ws_idx
        } else if let Some(workspace_id) = params.workspace_id.as_deref() {
            let Some(ws_idx) = self.parse_workspace_id(workspace_id) else {
                return encode_error(
                    id,
                    "workspace_not_found",
                    format!("workspace {workspace_id} not found"),
                );
            };
            ws_idx
        } else if let Some(active) = self.state.active {
            active
        } else {
            return encode_error(id, "workspace_not_found", "no active workspace");
        };
        if let Err(message) = validate_layout_tree(&params.root) {
            return encode_error(id, "invalid_layout", message);
        }
        let requested_panes = layout_pane_count(&params.root);
        let replaced_panes = replace_target.map_or(0, |(_, tab_idx)| {
            self.state.workspaces[ws_idx].tabs[tab_idx].panes.len()
        });
        let additional_panes = replacement_pane_increase(requested_panes, replaced_panes);
        let destination_tab = replace_target.map(|(_, tab_idx)| tab_idx);
        if let Err(error) = self.admit_new_panes(Some(ws_idx), destination_tab, additional_panes) {
            return encode_error(id, "resource_limit", error.to_string());
        }
        let transient_capacity_available = self
            .admit_new_panes(Some(ws_idx), None, requested_panes)
            .is_ok();
        if let Some(target) = replace_target.filter(|_| !transient_capacity_available) {
            return self.handle_in_place_layout_replacement(id, params, target);
        }
        if let Err(error) = self.state.workspaces[ws_idx].ensure_public_tab_numbers_available(1) {
            return encode_error(id, "layout_apply_failed", error.to_string());
        }

        let replacement_label = params.tab_label.clone().or_else(|| {
            let (_, tab_idx) = replace_target?;
            self.state
                .workspaces
                .get(ws_idx)?
                .tabs
                .get(tab_idx)?
                .custom_name
                .clone()
        });
        let replace_was_active = replace_target.is_some_and(|(target_ws, target_tab)| {
            self.state.active == Some(target_ws)
                && self
                    .state
                    .workspaces
                    .get(target_ws)
                    .is_some_and(|ws| ws.active_tab_index() == target_tab)
        });
        let root_leaf = first_layout_leaf(&params.root);
        let first_cwd = self.layout_root_cwd(ws_idx, replace_target, root_leaf);
        let (rows, cols) = self.state.estimate_pane_size();
        let default_shell = self.state.default_shell.clone();
        let scrollback_limit_bytes = self.state.pane_scrollback_limit_bytes;
        let host_terminal_theme = self.state.host_terminal_theme;
        let host_terminal_appearance = self.state.host_terminal_appearance;
        let extra_env = match super::env::normalize_launch_env(root_leaf.env.clone()) {
            Ok(env) => env,
            Err((code, message)) => return encode_error(id, &code, message),
        };
        let command = match layout_command(root_leaf) {
            Ok(command) => command,
            Err(message) => return encode_error(id, "invalid_layout", message),
        };

        let tab_id = match self.allocate_tab_id() {
            Ok(tab_id) => tab_id,
            Err(error) => return encode_error(id, "layout_apply_failed", error.to_string()),
        };
        let terminal_id = match self.allocate_terminal_id() {
            Ok(id) => id,
            Err(error) => return encode_error(id, "layout_apply_failed", error.to_string()),
        };
        let created = {
            let Some(ws) = self.state.workspaces.get_mut(ws_idx) else {
                return encode_error(id, "workspace_not_found", "workspace not found");
            };
            if let Some(argv) = command.as_deref() {
                ws.create_tab_argv_command(
                    tab_id,
                    terminal_id,
                    rows,
                    cols,
                    first_cwd,
                    argv,
                    extra_env,
                    scrollback_limit_bytes,
                    host_terminal_theme,
                    host_terminal_appearance,
                )
            } else {
                ws.create_tab(
                    tab_id,
                    terminal_id,
                    rows,
                    cols,
                    first_cwd,
                    scrollback_limit_bytes,
                    host_terminal_theme,
                    host_terminal_appearance,
                    crate::pane::PaneShellConfig::new(&default_shell, self.state.shell_mode),
                    extra_env,
                )
            }
        };

        let (new_tab_idx, terminal, runtime) = match created {
            Ok(result) => result,
            Err(err) => return encode_error(id, "layout_apply_failed", err.to_string()),
        };
        let new_root_pane = self.state.workspaces[ws_idx].tabs[new_tab_idx].root_pane;
        self.terminal_runtimes.insert(terminal.id.clone(), runtime);
        self.state.remove_alias_shadowed_by_new_pane(new_root_pane);
        self.state.terminals.insert(terminal.id.clone(), terminal);
        if let Some(label) = replacement_label {
            self.state.workspaces[ws_idx].tabs[new_tab_idx].set_custom_name(label);
        }
        self.apply_layout_pane_label(ws_idx, new_root_pane, root_leaf);

        if let Err(message) = self.apply_layout_node_to_pane(ws_idx, new_root_pane, &params.root) {
            self.rollback_layout_tab(ws_idx, new_root_pane);
            return encode_error(id, "layout_apply_failed", message);
        }

        if let Some((target_ws_idx, target_tab_idx)) = replace_target {
            let closed_tab_id = self
                .public_tab_id(target_ws_idx, target_tab_idx)
                .unwrap_or_else(|| {
                    crate::workspace::public_tab_id_for_number(
                        &self.public_workspace_id(target_ws_idx),
                        target_tab_idx + 1,
                    )
                });
            let terminal_ids = self
                .state
                .terminal_ids_for_tab(target_ws_idx, target_tab_idx);
            let plugin_pane_ids = self.state.pane_ids_for_tab(target_ws_idx, target_tab_idx);
            let Some(ws) = self.state.workspaces.get_mut(target_ws_idx) else {
                return encode_error(id, "tab_not_found", "tab not found");
            };
            if ws.close_tab(target_tab_idx) {
                for pane_id in &plugin_pane_ids {
                    self.state.delegations.tombstone_pane(*pane_id);
                }
                self.state.delegations.gc_tombstones();
                self.state.remove_plugin_pane_records(plugin_pane_ids);
                self.state.remove_unattached_terminal_ids(terminal_ids);
                self.shutdown_detached_terminal_runtimes();
                self.emit_event(EventEnvelope {
                    event: EventKind::TabClosed,
                    data: EventData::TabClosed {
                        tab_id: closed_tab_id,
                        workspace_id: self.public_workspace_id(target_ws_idx),
                    },
                });
            }
        }

        let Some(new_tab_idx) = self.state.workspaces[ws_idx]
            .tabs
            .iter()
            .position(|tab| tab.root_pane == new_root_pane)
        else {
            return encode_error(id, "layout_apply_failed", "new layout tab disappeared");
        };

        if params.focus || replace_was_active {
            self.state.switch_workspace_tab(ws_idx, new_tab_idx);
            self.apply_bundled_extension_state();
            self.state.mode = Mode::Terminal;
        }
        self.schedule_session_save();
        if let Some(tab) = self.tab_info(ws_idx, new_tab_idx) {
            self.emit_event(EventEnvelope {
                event: EventKind::TabCreated,
                data: EventData::TabCreated { tab },
            });
        }
        for pane_id in self.state.workspaces[ws_idx].tabs[new_tab_idx]
            .layout
            .pane_ids()
        {
            if let Some(pane) = self.pane_info(ws_idx, pane_id) {
                self.emit_event(EventEnvelope {
                    event: EventKind::PaneCreated,
                    data: EventData::PaneCreated { pane },
                });
            }
        }
        self.emit_layout_updated_event(ws_idx, new_tab_idx);

        let Some(layout) = self.layout_description(ws_idx, new_tab_idx) else {
            return encode_error(id, "layout_apply_failed", "new layout unavailable");
        };
        self.refresh_collection_documents();
        encode_success(id, ResponseResult::LayoutApply { layout })
    }

    fn handle_in_place_layout_replacement(
        &mut self,
        id: String,
        params: LayoutApplyParams,
        (ws_idx, tab_idx): (usize, usize),
    ) -> String {
        let removed_collections = self.collection_infos(None, Some((ws_idx, tab_idx)));
        let tab = &self.state.workspaces[ws_idx].tabs[tab_idx];
        let existing_panes = tab.layout.pane_ids();
        if layout_pane_count(&params.root) != existing_panes.len()
            || layout_has_launch_changes(&params.root)
        {
            return encode_error(
                id,
                "resource_limit",
                "transactional replacement requires temporary pane capacity; at capacity only same-size layout-only replacements are allowed",
            );
        }
        let mut pane_iter = existing_panes.iter().copied();
        let mut labels = Vec::new();
        let Some(root) = remap_layout_to_existing_panes(&params.root, &mut pane_iter, &mut labels)
        else {
            return encode_error(id, "invalid_layout", "layout pane mapping failed");
        };
        let Some(first_pane) = existing_panes.first().copied() else {
            return encode_error(id, "invalid_layout", "replacement layout is empty");
        };
        let Some(layout) = TileLayout::from_tree(root, LayoutLeaf::Pane(first_pane)) else {
            return encode_error(id, "invalid_layout", "replacement layout is invalid");
        };
        {
            let tab = &mut self.state.workspaces[ws_idx].tabs[tab_idx];
            tab.replace_layout_without_collections(layout, first_pane);
            if let Some(label) = params.tab_label {
                tab.set_custom_name(label);
            }
        }
        for (pane_id, leaf) in labels {
            self.apply_layout_pane_label(ws_idx, pane_id, leaf);
        }
        if params.focus {
            self.state.switch_workspace_tab(ws_idx, tab_idx);
            self.apply_bundled_extension_state();
            self.state.mode = Mode::Terminal;
        }
        self.state.mark_session_dirty();
        self.schedule_session_save();
        for collection in removed_collections {
            self.emit_collection_event("closed", collection);
        }
        self.emit_layout_updated_event(ws_idx, tab_idx);
        let Some(layout) = self.layout_description(ws_idx, tab_idx) else {
            return encode_error(id, "layout_apply_failed", "replacement layout unavailable");
        };
        encode_success(id, ResponseResult::LayoutApply { layout })
    }

    pub(super) fn handle_layout_set_split_ratio(
        &mut self,
        id: String,
        params: LayoutSetSplitRatioParams,
    ) -> String {
        if !params.ratio.is_finite() {
            return encode_error(id, "invalid_ratio", "ratio must be finite");
        }
        let Some((ws_idx, tab_idx)) = self.resolve_layout_export_target(&LayoutExportParams {
            tab_id: params.tab_id,
            pane_id: params.pane_id,
        }) else {
            return encode_error(id, "layout_not_found", "layout target not found");
        };

        let changed = self
            .state
            .workspaces
            .get_mut(ws_idx)
            .and_then(|ws| ws.tabs.get_mut(tab_idx))
            .is_some_and(|tab| tab.set_layout_split_ratio(&params.path, params.ratio));
        if !changed {
            return encode_error(id, "split_not_found", "split path not found");
        }

        self.schedule_session_save();
        let Some(layout) = self.layout_description(ws_idx, tab_idx) else {
            return encode_error(id, "layout_not_found", "layout unavailable");
        };
        self.emit_layout_updated_event(ws_idx, tab_idx);
        encode_success(id, ResponseResult::LayoutSplitRatioSet { layout })
    }

    fn resolve_layout_export_target(&self, params: &LayoutExportParams) -> Option<(usize, usize)> {
        match (params.tab_id.as_deref(), params.pane_id.as_deref()) {
            (Some(_), Some(_)) => None,
            (Some(tab_id), None) => self.parse_tab_id(tab_id),
            (None, Some(pane_id)) => {
                let (ws_idx, pane_id) = self.parse_pane_id(pane_id)?;
                let tab_idx = self
                    .state
                    .workspaces
                    .get(ws_idx)?
                    .find_tab_index_for_pane(pane_id)?;
                Some((ws_idx, tab_idx))
            }
            (None, None) => {
                let ws_idx = self.state.active?;
                let tab_idx = self.state.workspaces.get(ws_idx)?.active_tab_index();
                Some((ws_idx, tab_idx))
            }
        }
    }

    fn layout_description(&self, ws_idx: usize, tab_idx: usize) -> Option<LayoutDescription> {
        let ws = self.state.workspaces.get(ws_idx)?;
        let tab = ws.tabs.get(tab_idx)?;
        Some(LayoutDescription {
            workspace_id: self.public_workspace_id(ws_idx),
            tab_id: self.public_tab_id(ws_idx, tab_idx)?,
            zoomed: tab.zoomed,
            focused_pane_id: self.public_pane_id(ws_idx, tab.layout.focused())?,
            root: if tab.layout.typed_root().selected_pane().is_some() {
                self.layout_node_description(ws_idx, tab_idx, tab.layout.typed_root())?
            } else {
                self.layout_node_description(
                    ws_idx,
                    tab_idx,
                    &TypedNode::Leaf(tab.layout.focused()),
                )?
            },
        })
    }

    fn layout_node_description(
        &self,
        ws_idx: usize,
        tab_idx: usize,
        node: &TypedNode,
    ) -> Option<LayoutNode> {
        // Published pane-only API semantics stay here, not in the canonical layout.
        match node {
            TypedNode::Leaf(pane_id) => Some(LayoutNode::Pane {
                pane: self.layout_pane_description(ws_idx, tab_idx, *pane_id)?,
            }),
            // The legacy pane-only export has no view semantics; a view branch
            // has no terminal description and collapses like an empty branch.
            TypedNode::View { .. } => None,
            TypedNode::Stack {
                selected, children, ..
            } => children
                .get(*selected)
                .and_then(|child| self.layout_node_description(ws_idx, tab_idx, child)),
            TypedNode::Split {
                direction,
                ratio,
                first,
                second,
            } => {
                // Only branches with no terminal may collapse. A missing pane
                // description is an export error, not an empty layout branch.
                if !subtree_has_terminal(first) {
                    return self.layout_node_description(ws_idx, tab_idx, second);
                }
                if !subtree_has_terminal(second) {
                    return self.layout_node_description(ws_idx, tab_idx, first);
                }
                Some(LayoutNode::Split {
                    direction: match direction {
                        Direction::Horizontal => SplitDirection::Right,
                        Direction::Vertical => SplitDirection::Down,
                    },
                    ratio: *ratio,
                    first: Box::new(self.layout_node_description(ws_idx, tab_idx, first)?),
                    second: Box::new(self.layout_node_description(ws_idx, tab_idx, second)?),
                })
            }
        }
    }

    fn layout_pane_description(
        &self,
        ws_idx: usize,
        tab_idx: usize,
        pane_id: PaneId,
    ) -> Option<LayoutPane> {
        let ws = self.state.workspaces.get(ws_idx)?;
        let tab = ws.tabs.get(tab_idx)?;
        let terminal_id = tab.terminal_id(pane_id)?;
        let terminal = self.state.terminals.get(terminal_id);
        Some(LayoutPane {
            pane_id: Some(self.public_pane_id(ws_idx, pane_id)?),
            label: terminal.and_then(|terminal| terminal.manual_label.clone()),
            cwd: tab
                .cwd_for_pane(pane_id, &self.state.terminals, &self.terminal_runtimes)
                .map(|cwd| cwd.display().to_string()),
            command: terminal.and_then(|terminal| terminal.launch_argv.clone()),
            env: Default::default(),
        })
    }

    fn layout_root_cwd(
        &self,
        ws_idx: usize,
        replace_target: Option<(usize, usize)>,
        pane: &LayoutPane,
    ) -> PathBuf {
        if let Some(cwd) = pane.cwd.as_ref() {
            return PathBuf::from(cwd);
        }
        let follow_cwd = replace_target.and_then(|(_, tab_idx)| {
            let pane_id = self
                .state
                .workspaces
                .get(ws_idx)?
                .tabs
                .get(tab_idx)?
                .layout
                .focused();
            self.launch_cwd_for_pane_in_workspace(ws_idx, pane_id)
        });
        self.resolve_new_terminal_cwd(
            follow_cwd.or_else(|| self.focused_pane_cwd_in_workspace(ws_idx)),
        )
    }

    fn apply_layout_node_to_pane(
        &mut self,
        ws_idx: usize,
        pane_id: PaneId,
        node: &LayoutNode,
    ) -> Result<(), String> {
        match node {
            LayoutNode::Pane { pane } => {
                self.apply_layout_pane_label(ws_idx, pane_id, pane);
                Ok(())
            }
            LayoutNode::Split {
                direction,
                ratio,
                first,
                second,
            } => {
                let second_leaf = first_layout_leaf(second);
                let new_pane = self.layout_split_pane(
                    ws_idx,
                    pane_id,
                    direction.clone(),
                    *ratio,
                    second_leaf,
                )?;
                self.apply_layout_node_to_pane(ws_idx, pane_id, first)?;
                self.apply_layout_node_to_pane(ws_idx, new_pane, second)
            }
        }
    }

    fn layout_split_pane(
        &mut self,
        ws_idx: usize,
        target_pane_id: PaneId,
        direction: SplitDirection,
        ratio: f32,
        pane: &LayoutPane,
    ) -> Result<PaneId, String> {
        let (rows, cols) = self.state.estimate_pane_size();
        let default_shell = self.state.default_shell.clone();
        let scrollback_limit_bytes = self.state.pane_scrollback_limit_bytes;
        let host_terminal_theme = self.state.host_terminal_theme;
        let host_terminal_appearance = self.state.host_terminal_appearance;
        let cwd = pane
            .cwd
            .as_ref()
            .map(PathBuf::from)
            .or_else(|| self.launch_cwd_for_pane_in_workspace(ws_idx, target_pane_id));
        let extra_env = super::env::normalize_launch_env(pane.env.clone())
            .map_err(|(_, message)| message.to_string())?;
        let direction = match direction {
            SplitDirection::Right => Direction::Horizontal,
            SplitDirection::Down => Direction::Vertical,
        };
        let command = layout_command(pane)?;
        let terminal_id = self
            .allocate_terminal_id()
            .map_err(|error| error.to_string())?;
        let result = {
            let Some(ws) = self.state.workspaces.get_mut(ws_idx) else {
                return Err("workspace not found".into());
            };
            if let Some(argv) = command.as_deref() {
                ws.split_pane_argv_command_with_ratio(
                    terminal_id,
                    target_pane_id,
                    direction,
                    ratio,
                    rows,
                    cols,
                    cwd,
                    argv,
                    extra_env,
                    scrollback_limit_bytes,
                    host_terminal_theme,
                    host_terminal_appearance,
                    false,
                )
            } else {
                ws.split_pane_with_ratio(
                    terminal_id,
                    target_pane_id,
                    direction,
                    ratio,
                    rows,
                    cols,
                    cwd,
                    scrollback_limit_bytes,
                    host_terminal_theme,
                    host_terminal_appearance,
                    crate::pane::PaneShellConfig::new(&default_shell, self.state.shell_mode),
                    extra_env,
                    false,
                )
            }
        };
        let (_, new_pane) = result
            .ok_or_else(|| "pane not found".to_string())?
            .map_err(|err| err.to_string())?;
        let new_pane_id = new_pane.pane_id;
        self.attach_new_layout_pane(new_pane);
        self.apply_layout_pane_label(ws_idx, new_pane_id, pane);
        Ok(new_pane_id)
    }

    fn attach_new_layout_pane(&mut self, new_pane: NewPane) {
        self.terminal_runtimes
            .insert(new_pane.terminal.id.clone(), new_pane.runtime);
        self.state
            .remove_alias_shadowed_by_new_pane(new_pane.pane_id);
        self.state
            .terminals
            .insert(new_pane.terminal.id.clone(), new_pane.terminal);
    }

    fn apply_layout_pane_label(&mut self, ws_idx: usize, pane_id: PaneId, pane: &LayoutPane) {
        let Some(label) = pane
            .label
            .as_ref()
            .map(|label| label.trim())
            .filter(|label| !label.is_empty())
        else {
            return;
        };
        let Some(terminal_id) = self
            .state
            .workspaces
            .get(ws_idx)
            .and_then(|ws| ws.terminal_id(pane_id))
            .cloned()
        else {
            return;
        };
        if let Some(terminal) = self.state.terminals.get_mut(&terminal_id) {
            terminal.set_manual_label(label.to_string());
        }
    }

    fn rollback_layout_tab(&mut self, ws_idx: usize, root_pane: PaneId) {
        let Some(tab_idx) = self
            .state
            .workspaces
            .get(ws_idx)
            .and_then(|ws| ws.tabs.iter().position(|tab| tab.root_pane == root_pane))
        else {
            return;
        };
        let terminal_ids = self.state.terminal_ids_for_tab(ws_idx, tab_idx);
        let plugin_pane_ids = self.state.pane_ids_for_tab(ws_idx, tab_idx);
        if self
            .state
            .workspaces
            .get_mut(ws_idx)
            .is_some_and(|ws| ws.close_tab(tab_idx))
        {
            self.state.remove_plugin_pane_records(plugin_pane_ids);
            self.state.remove_unattached_terminal_ids(terminal_ids);
            self.shutdown_detached_terminal_runtimes();
        }
    }
}

fn first_layout_leaf(node: &LayoutNode) -> &LayoutPane {
    match node {
        LayoutNode::Pane { pane } => pane,
        LayoutNode::Split { first, .. } => first_layout_leaf(first),
    }
}

fn layout_command(pane: &LayoutPane) -> Result<Option<Vec<String>>, String> {
    match pane.command.as_ref() {
        Some(command) if command.is_empty() => Err("pane command must not be empty".into()),
        Some(command) => Ok(Some(command.clone())),
        None => Ok(None),
    }
}

fn layout_has_launch_changes(node: &LayoutNode) -> bool {
    match node {
        LayoutNode::Pane { pane } => {
            pane.command.is_some() || pane.cwd.is_some() || !pane.env.is_empty()
        }
        LayoutNode::Split { first, second, .. } => {
            layout_has_launch_changes(first) || layout_has_launch_changes(second)
        }
    }
}

fn remap_layout_to_existing_panes<'a>(
    node: &'a LayoutNode,
    panes: &mut impl Iterator<Item = PaneId>,
    labels: &mut Vec<(PaneId, &'a LayoutPane)>,
) -> Option<TypedNode> {
    match node {
        LayoutNode::Pane { pane } => {
            let pane_id = panes.next()?;
            labels.push((pane_id, pane));
            Some(TypedNode::Leaf(pane_id))
        }
        LayoutNode::Split {
            direction: split_direction,
            ratio,
            first,
            second,
        } => Some(TypedNode::Split {
            direction: match split_direction {
                SplitDirection::Right => Direction::Horizontal,
                SplitDirection::Down => Direction::Vertical,
            },
            ratio: *ratio,
            first: Box::new(remap_layout_to_existing_panes(first, panes, labels)?),
            second: Box::new(remap_layout_to_existing_panes(second, panes, labels)?),
        }),
    }
}

fn replacement_pane_increase(requested: usize, replaced: usize) -> usize {
    requested.saturating_sub(replaced)
}

fn layout_pane_count(node: &LayoutNode) -> usize {
    match node {
        LayoutNode::Pane { .. } => 1,
        LayoutNode::Split { first, second, .. } => {
            layout_pane_count(first).saturating_add(layout_pane_count(second))
        }
    }
}

fn subtree_has_terminal(node: &TypedNode) -> bool {
    match node {
        TypedNode::Leaf(_) => true,
        TypedNode::View { .. } => false,
        TypedNode::Stack {
            selected, children, ..
        } => children.get(*selected).is_some_and(subtree_has_terminal),
        TypedNode::Split { first, second, .. } => {
            subtree_has_terminal(first) || subtree_has_terminal(second)
        }
    }
}

fn validate_layout_tree(root: &LayoutNode) -> Result<(), String> {
    let mut stats = LayoutTreeStats {
        panes: 0,
        max_depth: 0,
    };
    validate_layout_node(root, 1, &mut stats)?;
    if stats.panes > MAX_LAYOUT_PANES {
        return Err(format!(
            "layout has {} panes; maximum is {}",
            stats.panes, MAX_LAYOUT_PANES
        ));
    }
    if stats.max_depth > MAX_LAYOUT_DEPTH {
        return Err(format!(
            "layout depth is {}; maximum is {}",
            stats.max_depth, MAX_LAYOUT_DEPTH
        ));
    }
    Ok(())
}

struct LayoutTreeStats {
    panes: usize,
    max_depth: usize,
}

fn validate_layout_node(
    node: &LayoutNode,
    depth: usize,
    stats: &mut LayoutTreeStats,
) -> Result<(), String> {
    stats.max_depth = stats.max_depth.max(depth);
    if depth > MAX_LAYOUT_DEPTH {
        return Err(format!(
            "layout depth is {}; maximum is {}",
            depth, MAX_LAYOUT_DEPTH
        ));
    }
    match node {
        LayoutNode::Pane { pane } => {
            stats.panes += 1;
            if stats.panes > MAX_LAYOUT_PANES {
                return Err(format!("layout has more than {} panes", MAX_LAYOUT_PANES));
            }
            layout_command(pane)?;
            super::env::normalize_launch_env(pane.env.clone())
                .map_err(|(_, message)| message.to_string())?;
            Ok(())
        }
        LayoutNode::Split {
            first,
            second,
            ratio,
            ..
        } => {
            if !ratio.is_finite() {
                return Err("split ratio must be finite".into());
            }
            validate_layout_node(first, depth + 1, stats)?;
            validate_layout_node(second, depth + 1, stats)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{exiting_test_command, shutdown_test_runtimes};
    use super::*;
    use crate::{
        api::schema::{ErrorResponse, ResponseResult, SuccessResponse},
        config::{Config, ShellModeConfig},
        workspace::Workspace,
    };

    fn app_with_workspace() -> App {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.default_shell = exiting_test_command().into();
        app.state.shell_mode = ShellModeConfig::NonLogin;
        app.state.workspaces = vec![Workspace::test_new("layout")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.ensure_test_terminals();
        app
    }

    #[test]
    fn layout_export_does_not_hide_an_undescribable_nonempty_pane() {
        let mut app = app_with_workspace();
        let root = app.state.workspaces[0].tabs[0].root_pane;
        let right = app.state.workspaces[0].test_split(Direction::Horizontal);
        app.state.ensure_test_terminals();
        app.state.workspaces[0].tabs[0].layout.focus_pane(root);
        app.state.workspaces[0].tabs[0].panes.remove(&right);
        assert!(app.layout_description(0, 0).is_none());
    }

    #[test]
    fn layout_export_returns_portable_tree() {
        let mut app = app_with_workspace();
        let root = app.state.workspaces[0].tabs[0].root_pane;
        let right = app.state.workspaces[0].test_split(Direction::Horizontal);
        app.state.ensure_test_terminals();
        app.state.workspaces[0].tabs[0].layout.focus_pane(root);
        app.state.workspaces[0].tabs[0]
            .layout
            .set_ratio_at(&[], 0.65);
        let right_terminal_id = app.state.workspaces[0].tabs[0]
            .terminal_id(right)
            .cloned()
            .unwrap();
        app.state
            .terminals
            .get_mut(&right_terminal_id)
            .unwrap()
            .set_manual_label("tests".into());

        let response = app.handle_layout_export(
            "req".into(),
            LayoutExportParams {
                tab_id: None,
                pane_id: None,
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::LayoutExport { layout } = success.result else {
            panic!("expected layout export response");
        };
        assert_eq!(layout.workspace_id, app.public_workspace_id(0));
        assert_eq!(layout.focused_pane_id, app.public_pane_id(0, root).unwrap());
        let LayoutNode::Split {
            direction,
            ratio,
            second,
            ..
        } = layout.root
        else {
            panic!("expected split layout root");
        };
        assert_eq!(direction, SplitDirection::Right);
        assert!((ratio - 0.65).abs() < f32::EPSILON);
        let LayoutNode::Pane { pane } = *second else {
            panic!("expected second pane");
        };
        assert_eq!(pane.label.as_deref(), Some("tests"));
        assert_eq!(pane.pane_id, Some(app.public_pane_id(0, right).unwrap()));
    }

    #[test]
    fn tree_export_includes_hidden_members_without_reinterpreting_legacy_layout() {
        let mut app = app_with_workspace();
        let root = app.state.workspaces[0].tabs[0].root_pane;
        let selected_pane = app.state.workspaces[0].test_split(Direction::Horizontal);
        let hidden = app.state.workspaces[0].test_split(Direction::Vertical);
        let collection = app.state.workspaces[0].tabs[0]
            .create_collection_near(LayoutLeaf::Pane(root), Direction::Horizontal, 0.37, None)
            .unwrap();
        app.state.workspaces[0].tabs[0]
            .collect_tiled_pane(selected_pane, collection)
            .unwrap();
        app.state.workspaces[0].tabs[0]
            .collect_tiled_pane(hidden, collection)
            .unwrap();
        assert!(!app.state.workspaces[0].tabs[0]
            .layout
            .is_pane_visible(hidden));
        app.state.ensure_test_terminals();
        let legacy = app.layout_description(0, 0).unwrap();
        let LayoutNode::Split { first, second, .. } = &legacy.root else {
            panic!("legacy split")
        };
        assert!(
            matches!(first.as_ref(), LayoutNode::Pane { pane } if pane.pane_id == app.public_pane_id(0, root))
        );
        assert!(
            matches!(second.as_ref(), LayoutNode::Pane { pane } if pane.pane_id == app.public_pane_id(0, selected_pane))
        );
        let response = app.handle_layout_tree_export(
            "req".into(),
            LayoutExportParams {
                tab_id: None,
                pane_id: None,
            },
        );
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::LayoutTreeExport { layout } = success.result else {
            panic!("tree export")
        };
        assert_eq!(layout.kind, "layout.tree.v1");
        let LayoutTreeNode::Split { second, .. } = layout.root else {
            panic!("tree split")
        };
        let LayoutTreeNode::Stack {
            box_id,
            selected,
            children,
        } = *second
        else {
            panic!("tree stack")
        };
        assert_eq!(
            box_id,
            crate::terminal_container::format_terminal_container_id(collection.into())
        );
        assert_eq!(selected, 0);
        assert_eq!(
            children,
            vec![
                LayoutTreeNode::Pane {
                    pane_id: app.public_pane_id(0, selected_pane).unwrap()
                },
                LayoutTreeNode::Pane {
                    pane_id: app.public_pane_id(0, hidden).unwrap()
                },
            ]
        );
        let unknown: LayoutTreeNode =
            serde_json::from_str(r#"{"type":"future_box","children":[]}"#).unwrap();
        assert_eq!(unknown, LayoutTreeNode::Unknown);
        assert_eq!(app.layout_description(0, 0).unwrap(), legacy);
        app.state.assert_invariants_for_test();
    }

    #[test]
    fn legacy_export_ignores_hidden_terminals_behind_a_selected_view() {
        let mut app = app_with_workspace();
        let root = app.state.workspaces[0].tabs[0].root_pane;
        let hidden = app.state.workspaces[0].test_split(Direction::Horizontal);
        let (_, view_pane) = app.state.workspaces[0]
            .split_pane_with_view(
                root,
                "vqro.tasks",
                "tasks",
                Direction::Horizontal,
                0.5,
                false,
            )
            .unwrap();
        let id = crate::terminal_container::TerminalContainerId::parse(9_777).unwrap();
        app.state.workspaces[0].tabs[0]
            .layout
            .replace_typed_root_for_test(TypedNode::Split {
                direction: Direction::Horizontal,
                ratio: 0.5,
                first: Box::new(TypedNode::Stack {
                    id,
                    selected: 0,
                    children: vec![
                        TypedNode::view(view_pane, "vqro.tasks", "tasks"),
                        TypedNode::Leaf(hidden),
                    ],
                }),
                second: Box::new(TypedNode::Leaf(root)),
            });
        app.state.ensure_test_terminals();
        let legacy = app
            .layout_description(0, 0)
            .expect("visible terminal remains exportable");
        assert!(
            matches!(legacy.root, LayoutNode::Pane { pane } if pane.pane_id == app.public_pane_id(0, root))
        );
        assert_eq!(app.state.workspaces[0].tabs[0].layout.pane_count(), 3);
        app.state.assert_invariants_for_test();
    }

    #[test]
    fn layout_set_split_ratio_updates_existing_split() {
        let mut app = app_with_workspace();
        app.state.workspaces[0].test_split(Direction::Horizontal);

        let response = app.handle_layout_set_split_ratio(
            "req".into(),
            LayoutSetSplitRatioParams {
                tab_id: None,
                pane_id: None,
                path: vec![],
                ratio: 0.72,
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::LayoutSplitRatioSet { layout } = success.result else {
            panic!("expected layout split ratio set response");
        };
        let LayoutNode::Split { ratio, .. } = layout.root else {
            panic!("expected split layout root");
        };
        assert!((ratio - 0.72).abs() < f32::EPSILON);
        assert!(matches!(
            &app.event_hub.events_after(0).last().expect("layout event").1.data,
            EventData::LayoutUpdated { layout }
                if layout.tab_id == app.public_tab_id(0, 0).unwrap()
                    && (layout.splits[0].ratio - 0.72).abs() < f32::EPSILON
        ));
    }

    #[test]
    fn layout_set_split_ratio_rejects_missing_split() {
        let mut app = app_with_workspace();

        let response = app.handle_layout_set_split_ratio(
            "req".into(),
            LayoutSetSplitRatioParams {
                tab_id: None,
                pane_id: None,
                path: vec![],
                ratio: 0.72,
            },
        );

        let error: ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(error.error.code, "split_not_found");
    }

    #[tokio::test]
    async fn layout_apply_replaces_tab_with_requested_tree() {
        let mut app = app_with_workspace();
        let original_tab_id = app.public_tab_id(0, 0).unwrap();

        let response = app.handle_layout_apply(
            "req".into(),
            LayoutApplyParams {
                workspace_id: None,
                tab_id: Some(original_tab_id),
                tab_label: Some("dev".into()),
                focus: true,
                root: LayoutNode::Split {
                    direction: SplitDirection::Right,
                    ratio: 0.7,
                    first: Box::new(LayoutNode::Pane {
                        pane: LayoutPane {
                            label: Some("editor".into()),
                            ..Default::default()
                        },
                    }),
                    second: Box::new(LayoutNode::Pane {
                        pane: LayoutPane {
                            label: Some("tests".into()),
                            command: Some(vec![exiting_test_command().into()]),
                            env: std::collections::HashMap::from([(
                                "HERDR_ROLE".into(),
                                "tests".into(),
                            )]),
                            ..Default::default()
                        },
                    }),
                },
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::LayoutApply { layout } = success.result else {
            panic!("expected layout apply response");
        };
        assert_eq!(app.state.workspaces[0].tabs.len(), 1);
        assert_eq!(
            app.state.workspaces[0].tab_display_name(0).as_deref(),
            Some("dev")
        );
        let LayoutNode::Split {
            direction,
            ratio,
            first,
            second,
        } = layout.root
        else {
            panic!("expected split layout root");
        };
        assert_eq!(direction, SplitDirection::Right);
        assert!((ratio - 0.7).abs() < f32::EPSILON);
        let LayoutNode::Pane { pane: first_pane } = *first else {
            panic!("expected first pane");
        };
        let LayoutNode::Pane { pane: second_pane } = *second else {
            panic!("expected second pane");
        };
        assert_eq!(first_pane.label.as_deref(), Some("editor"));
        assert_eq!(second_pane.label.as_deref(), Some("tests"));
        assert_eq!(
            second_pane.command,
            Some(vec![exiting_test_command().into()])
        );
        assert!(matches!(
            &app.event_hub.events_after(0).last().expect("layout event").1.data,
            EventData::LayoutUpdated { layout }
                if layout.tab_id == app.public_tab_id(0, 0).unwrap()
                    && layout.panes.len() == 2
        ));
        shutdown_test_runtimes(&mut app);
    }

    #[tokio::test]
    async fn failed_replacement_restores_a_reachable_generic_terminal() {
        let mut app = app_with_workspace();
        let original_tab_id = app.public_tab_id(0, 0).unwrap();
        let original_pane = app.state.workspaces[0].tabs[0].root_pane;
        let original_terminal = app.state.workspaces[0].tabs[0].panes[&original_pane]
            .attached_terminal_id
            .clone();
        let runtime_count = app.terminal_runtimes.len();

        let response = app.handle_layout_apply(
            "req".into(),
            LayoutApplyParams {
                workspace_id: None,
                tab_id: Some(original_tab_id),
                tab_label: None,
                focus: true,
                root: LayoutNode::Pane {
                    pane: LayoutPane {
                        command: Some(vec!["/definitely/not/a/vqro-command".into()]),
                        ..Default::default()
                    },
                },
            },
        );

        let error: ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(error.error.code, "layout_apply_failed");
        assert_eq!(app.state.workspaces[0].tabs.len(), 1);
        assert_eq!(app.state.workspaces[0].structural_pane_count(), 1);
        assert_eq!(app.state.terminals.len(), 1);
        assert_eq!(app.state.workspaces[0].tabs[0].root_pane, original_pane);
        assert_eq!(
            app.state.workspaces[0].tabs[0].panes[&original_pane].attached_terminal_id,
            original_terminal
        );
        assert_eq!(app.terminal_runtimes.len(), runtime_count);
    }

    #[tokio::test]
    async fn later_split_failure_recovers_replacement_without_false_close_event() {
        let mut app = app_with_workspace();
        let original_tab_id = app.public_tab_id(0, 0).unwrap();
        let response = app.handle_layout_apply(
            "req".into(),
            LayoutApplyParams {
                workspace_id: None,
                tab_id: Some(original_tab_id),
                tab_label: None,
                focus: true,
                root: LayoutNode::Split {
                    direction: SplitDirection::Right,
                    ratio: 0.5,
                    first: Box::new(LayoutNode::Pane {
                        pane: LayoutPane::default(),
                    }),
                    second: Box::new(LayoutNode::Pane {
                        pane: LayoutPane {
                            command: Some(vec!["/definitely/not/a/vqro-command".into()]),
                            ..Default::default()
                        },
                    }),
                },
            },
        );

        let error: ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(error.error.code, "layout_apply_failed");
        assert_eq!(app.state.workspaces[0].tabs.len(), 1);
        assert_eq!(app.state.workspaces[0].structural_pane_count(), 1);
        assert!(!app
            .event_hub
            .events_after(0)
            .iter()
            .any(|(_, event)| event.event == EventKind::TabClosed));
    }

    #[tokio::test]
    async fn layout_apply_new_tab_follows_cached_focused_pane_cwd_without_runtime() {
        let mut app = app_with_workspace();
        let focused_pane = app.state.workspaces[0].tabs[0].root_pane;
        let cached_cwd = std::env::temp_dir();
        let terminal_id = app.state.workspaces[0]
            .terminal_id(focused_pane)
            .cloned()
            .unwrap();
        app.state.terminals.get_mut(&terminal_id).unwrap().cwd = cached_cwd.clone();

        let response = app.handle_layout_apply(
            "req".into(),
            LayoutApplyParams {
                workspace_id: None,
                tab_id: None,
                tab_label: Some("cached".into()),
                focus: false,
                root: LayoutNode::Pane {
                    pane: LayoutPane::default(),
                },
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert!(matches!(success.result, ResponseResult::LayoutApply { .. }));
        let created = &app.state.workspaces[0].tabs[1];
        let created_terminal_id = created.terminal_id(created.root_pane).unwrap();
        let created_cwd = &app.state.terminals.get(created_terminal_id).unwrap().cwd;
        assert_eq!(
            crate::worktree::canonical_or_original(created_cwd),
            crate::worktree::canonical_or_original(&cached_cwd)
        );
        shutdown_test_runtimes(&mut app);
    }

    #[tokio::test]
    async fn layout_apply_replace_drops_plugin_pane_records_of_replaced_tab() {
        let mut app = app_with_workspace();
        let original_tab_id = app.public_tab_id(0, 0).unwrap();
        let replaced_pane = app.state.workspaces[0].tabs[0].root_pane;
        app.state.plugin_panes.insert(
            replaced_pane,
            crate::app::state::PluginPaneRecord {
                plugin_id: "example.layout".into(),
                entrypoint: "board".into(),
            },
        );
        let parent = app
            .state
            .delegations
            .create(Some(replaced_pane), None, Some("parent".into()))
            .unwrap();
        let child = app
            .state
            .delegations
            .create(None, Some(parent), Some("child".into()))
            .unwrap();

        let response = app.handle_layout_apply(
            "req".into(),
            LayoutApplyParams {
                workspace_id: None,
                tab_id: Some(original_tab_id),
                tab_label: Some("dev".into()),
                focus: true,
                root: LayoutNode::Pane {
                    pane: LayoutPane {
                        label: Some("editor".into()),
                        ..Default::default()
                    },
                },
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert!(matches!(success.result, ResponseResult::LayoutApply { .. }));
        assert!(!app.state.plugin_panes.contains_key(&replaced_pane));
        let retained = app.state.delegations.get(parent).unwrap();
        assert!(retained.tombstone);
        assert_eq!(retained.pane_id, None);
        assert_eq!(
            app.state.delegations.get(child).unwrap().parent_id,
            Some(parent)
        );
        app.state.assert_invariants_for_test();
    }

    #[tokio::test]
    async fn layout_apply_rejects_invalid_deep_leaf_without_creating_tab() {
        let mut app = app_with_workspace();
        let original_tab_count = app.state.workspaces[0].tabs.len();

        let response = app.handle_layout_apply(
            "req".into(),
            LayoutApplyParams {
                workspace_id: Some(app.public_workspace_id(0)),
                tab_id: None,
                tab_label: Some("bad".into()),
                focus: false,
                root: LayoutNode::Split {
                    direction: SplitDirection::Right,
                    ratio: 0.5,
                    first: Box::new(LayoutNode::Pane {
                        pane: LayoutPane {
                            label: Some("editor".into()),
                            ..Default::default()
                        },
                    }),
                    second: Box::new(LayoutNode::Pane {
                        pane: LayoutPane {
                            command: Some(Vec::new()),
                            ..Default::default()
                        },
                    }),
                },
            },
        );

        let error: ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(error.error.code, "invalid_layout");
        assert_eq!(app.state.workspaces[0].tabs.len(), original_tab_count);
    }

    #[tokio::test]
    async fn same_size_replacement_at_session_capacity_is_in_place_and_transactional() {
        let mut app = app_with_workspace();
        app.state.workspaces.extend(
            (1..crate::admission::MAX_SESSION_PANES)
                .map(|_| crate::workspace::Workspace::test_new("capacity")),
        );
        let tab_id = app.public_tab_id(0, 0).unwrap();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let next_pane_id = PaneId::next_for_test();

        let response = app.handle_layout_apply(
            "req".into(),
            LayoutApplyParams {
                workspace_id: None,
                tab_id: Some(tab_id.clone()),
                tab_label: Some("rearranged".into()),
                focus: true,
                root: LayoutNode::Pane {
                    pane: LayoutPane {
                        label: Some("same terminal".into()),
                        ..Default::default()
                    },
                },
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert!(matches!(success.result, ResponseResult::LayoutApply { .. }));
        assert_eq!(app.public_tab_id(0, 0).unwrap(), tab_id);
        assert_eq!(app.state.workspaces[0].tabs[0].root_pane, pane_id);
        assert_eq!(
            app.state.workspaces[0].tabs[0].panes[&pane_id].attached_terminal_id,
            terminal_id
        );
        assert_eq!(PaneId::next_for_test(), next_pane_id);
        assert!(!app
            .event_hub
            .events_after(0)
            .iter()
            .any(|(_, event)| matches!(event.event, EventKind::TabClosed | EventKind::TabCreated)));
    }

    #[test]
    fn same_size_replacement_is_admitted_at_every_pane_limit() {
        let additional = replacement_pane_increase(MAX_LAYOUT_PANES, MAX_LAYOUT_PANES);
        assert_eq!(additional, 0);
        assert!(crate::admission::panes(
            crate::admission::MAX_SESSION_PANES,
            crate::admission::MAX_WORKSPACE_PANES,
            crate::admission::MAX_TAB_PANES,
            additional,
        )
        .is_ok());
    }

    #[test]
    fn layout_validation_rejects_too_many_panes() {
        let mut root = LayoutNode::Pane {
            pane: LayoutPane::default(),
        };
        for _ in 0..MAX_LAYOUT_PANES {
            root = LayoutNode::Split {
                direction: SplitDirection::Right,
                ratio: 0.5,
                first: Box::new(root),
                second: Box::new(LayoutNode::Pane {
                    pane: LayoutPane::default(),
                }),
            };
        }

        let err = validate_layout_tree(&root).unwrap_err();
        assert!(err.contains("maximum"));
    }
}
