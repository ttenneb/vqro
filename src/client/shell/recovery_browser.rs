//! Neutral terminal recovery browser built only from cached shell snapshot facts.

use std::collections::{HashMap, HashSet};

use super::*;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) enum RecoveryRowId {
    Workspace(String),
    Tab(String),
    Terminal(String),
    RecoveredWorkspace,
    RecoveredTab,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum RecoveryAction {
    Focus(ClientEndpointFocusTarget),
    None,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct RecoveryRow {
    pub(super) id: RecoveryRowId,
    pub(super) depth: u8,
    pub(super) label: String,
    pub(super) current: bool,
    pub(super) action: RecoveryAction,
    pub(super) close_pane_id: Option<String>,
}

impl RecoveryRow {
    pub(super) fn enabled(&self) -> bool {
        !matches!(self.action, RecoveryAction::None)
    }
}

#[derive(Clone, Debug, Default)]
pub(super) struct RecoveryPresentationState {
    pub(super) selected: Option<RecoveryRowId>,
    pub(super) selected_index: Option<usize>,
    pub(super) scroll: usize,
}

#[derive(Clone, Debug)]
pub(super) struct ClientRecoveryOverlay {
    pub(super) endpoint_id: ClientEndpointId,
    pub(super) connection_generation: Option<u64>,
    pub(super) boot_id: String,
    pub(super) snapshot_revision: u64,
    pub(super) terminal_count: usize,
    /// Monotonic local token that rejects mouse hits rendered from an older projection.
    pub(super) projection_token: u64,
    pub(super) rows: Vec<RecoveryRow>,
}

#[derive(Clone, Debug)]
pub(super) struct RecoveryCloseLease {
    pub(super) endpoint_id: ClientEndpointId,
    pub(super) connection_generation: Option<u64>,
    pub(super) boot_id: String,
    pub(super) snapshot_revision: u64,
    pub(super) pane_id: String,
}

#[derive(Clone, Debug)]
pub(super) struct RecoveryRowHit {
    pub(super) rect: Rect,
    pub(super) close: Rect,
    pub(super) row_index: usize,
}

fn clean_label(label: &str, fallback: String) -> String {
    let label = label.trim();
    if label.is_empty() {
        fallback
    } else {
        label.to_owned()
    }
}

/// Preserve authoritative snapshot order while repairing duplicate and dangling identities.
/// First occurrence wins for duplicate public IDs; every remaining uniquely addressable pane is
/// retained, with structurally inconsistent panes appended under disabled synthetic parents.
pub(super) fn normalized_rows(snapshot: &ClientShellSnapshot) -> Vec<RecoveryRow> {
    let mut seen_workspaces = HashSet::new();
    let workspaces = snapshot
        .workspaces
        .iter()
        .filter(|workspace| {
            !workspace.workspace_id.is_empty()
                && seen_workspaces.insert(workspace.workspace_id.as_str())
        })
        .collect::<Vec<_>>();
    let workspace_ids = workspaces
        .iter()
        .map(|workspace| workspace.workspace_id.as_str())
        .collect::<HashSet<_>>();

    let mut seen_tabs = HashSet::new();
    let tabs = snapshot
        .tabs
        .iter()
        .filter(|tab| !tab.tab_id.is_empty() && seen_tabs.insert(tab.tab_id.as_str()))
        .collect::<Vec<_>>();
    let valid_tabs = tabs
        .iter()
        .filter(|tab| workspace_ids.contains(tab.workspace_id.as_str()))
        .map(|tab| (tab.tab_id.as_str(), tab.workspace_id.as_str()))
        .collect::<HashMap<_, _>>();

    let mut seen_panes = HashSet::new();
    let panes = snapshot
        .panes
        .iter()
        .filter(|pane| !pane.pane_id.is_empty() && seen_panes.insert(pane.pane_id.as_str()))
        .collect::<Vec<_>>();
    let mut panes_by_tab: HashMap<&str, Vec<&crate::protocol::ClientShellPane>> = HashMap::new();
    let mut recovered = Vec::new();
    for pane in panes {
        if valid_tabs
            .get(pane.tab_id.as_str())
            .is_some_and(|workspace_id| *workspace_id == pane.workspace_id)
        {
            panes_by_tab
                .entry(pane.tab_id.as_str())
                .or_default()
                .push(pane);
        } else {
            recovered.push(pane);
        }
    }

    let mut rows = Vec::new();
    for workspace in workspaces {
        rows.push(RecoveryRow {
            id: RecoveryRowId::Workspace(workspace.workspace_id.clone()),
            depth: 0,
            label: clean_label(&workspace.label, workspace.workspace_id.clone()),
            current: snapshot.focused_workspace_id.as_deref()
                == Some(workspace.workspace_id.as_str()),
            action: RecoveryAction::Focus(ClientEndpointFocusTarget::Workspace(
                workspace.workspace_id.clone(),
            )),
            close_pane_id: None,
        });
        for tab in tabs
            .iter()
            .copied()
            .filter(|tab| tab.workspace_id == workspace.workspace_id)
        {
            rows.push(RecoveryRow {
                id: RecoveryRowId::Tab(tab.tab_id.clone()),
                depth: 1,
                label: clean_label(&tab.label, tab.tab_id.clone()),
                current: snapshot.focused_tab_id.as_deref() == Some(tab.tab_id.as_str()),
                action: RecoveryAction::Focus(ClientEndpointFocusTarget::Tab(tab.tab_id.clone())),
                close_pane_id: None,
            });
            for pane in panes_by_tab.remove(tab.tab_id.as_str()).unwrap_or_default() {
                rows.push(terminal_row(snapshot, pane));
            }
        }
    }

    if !recovered.is_empty() {
        rows.push(RecoveryRow {
            id: RecoveryRowId::RecoveredWorkspace,
            depth: 0,
            label: "Unplaced terminals".into(),
            current: false,
            action: RecoveryAction::None,
            close_pane_id: None,
        });
        rows.push(RecoveryRow {
            id: RecoveryRowId::RecoveredTab,
            depth: 1,
            label: "Recovery".into(),
            current: false,
            action: RecoveryAction::None,
            close_pane_id: None,
        });
        rows.extend(
            recovered
                .into_iter()
                .map(|pane| terminal_row(snapshot, pane)),
        );
    }
    rows
}

fn terminal_row(
    snapshot: &ClientShellSnapshot,
    pane: &crate::protocol::ClientShellPane,
) -> RecoveryRow {
    RecoveryRow {
        id: RecoveryRowId::Terminal(pane.pane_id.clone()),
        depth: 2,
        label: pane
            .label
            .as_deref()
            .map(str::trim)
            .filter(|label| !label.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| format!("terminal {}", pane.pane_id)),
        current: snapshot.focused_pane_id.as_deref() == Some(pane.pane_id.as_str()),
        action: RecoveryAction::Focus(ClientEndpointFocusTarget::Pane(pane.pane_id.clone())),
        close_pane_id: Some(pane.pane_id.clone()),
    }
}

fn reconcile_presentation(presentation: &mut RecoveryPresentationState, rows: &[RecoveryRow]) {
    let selected_index = presentation
        .selected
        .as_ref()
        .and_then(|selected| {
            rows.iter()
                .position(|row| &row.id == selected && row.enabled())
        })
        .or_else(|| {
            rows.iter().position(|row| {
                row.current && row.enabled() && matches!(row.id, RecoveryRowId::Terminal(_))
            })
        })
        .or_else(|| rows.iter().position(|row| row.current && row.enabled()))
        .or_else(|| rows.iter().position(RecoveryRow::enabled));
    presentation.selected_index = selected_index;
    presentation.selected = selected_index.map(|index| rows[index].id.clone());
    presentation.scroll = presentation.scroll.min(rows.len().saturating_sub(1));
}

impl ClientShellState {
    pub(crate) fn open_recovery_browser(&mut self) {
        let endpoint_id = self.active_endpoint_id.clone();
        self.open_recovery_browser_for(&endpoint_id);
    }

    pub(super) fn open_recovery_browser_for(&mut self, endpoint_id: &ClientEndpointId) {
        let Some(endpoint) = self
            .endpoints
            .iter()
            .find(|endpoint| &endpoint.endpoint_id == endpoint_id)
        else {
            return;
        };
        let Some(snapshot) = endpoint.snapshot.as_deref() else {
            return;
        };
        let rows = normalized_rows(snapshot);
        let terminal_count = rows
            .iter()
            .filter(|row| matches!(row.id, RecoveryRowId::Terminal(_)))
            .count();
        let presentation = self
            .recovery_presentations
            .entry(endpoint.endpoint_id.clone())
            .or_default();
        reconcile_presentation(presentation, &rows);
        self.next_recovery_projection_token =
            self.next_recovery_projection_token.wrapping_add(1).max(1);
        self.overlay = Some(ClientShellOverlay::Recovery(ClientRecoveryOverlay {
            endpoint_id: endpoint.endpoint_id.clone(),
            connection_generation: endpoint.snapshot_generation,
            boot_id: snapshot.boot_id.clone(),
            snapshot_revision: snapshot.revision,
            terminal_count,
            projection_token: self.next_recovery_projection_token,
            rows,
        }));
    }

    pub(super) fn refresh_recovery_overlay(&mut self, endpoint_id: &ClientEndpointId) {
        if !self.overlay.as_ref().is_some_and(|overlay| {
            matches!(overlay, ClientShellOverlay::Recovery(recovery)
                if &recovery.endpoint_id == endpoint_id)
        }) {
            return;
        }
        if self
            .endpoints
            .iter()
            .find(|endpoint| &endpoint.endpoint_id == endpoint_id)
            .and_then(|endpoint| endpoint.snapshot.as_deref())
            .is_some()
        {
            self.open_recovery_browser_for(endpoint_id);
        } else {
            self.overlay = None;
        }
    }

    fn recovery_overlay_and_presentation(
        &self,
    ) -> Option<(&ClientRecoveryOverlay, &RecoveryPresentationState)> {
        let ClientShellOverlay::Recovery(overlay) = self.overlay.as_ref()? else {
            return None;
        };
        let presentation = self.recovery_presentations.get(&overlay.endpoint_id)?;
        Some((overlay, presentation))
    }

    fn recovery_overlay_is_current(&self, overlay: &ClientRecoveryOverlay) -> bool {
        self.endpoints
            .iter()
            .find(|endpoint| endpoint.endpoint_id == overlay.endpoint_id)
            .filter(|endpoint| endpoint.snapshot_generation == overlay.connection_generation)
            .and_then(|endpoint| endpoint.snapshot.as_deref())
            .is_some_and(|snapshot| {
                snapshot.boot_id == overlay.boot_id
                    && snapshot.revision == overlay.snapshot_revision
            })
    }

    pub(super) fn move_recovery_selection(&mut self, delta: isize) {
        let Some((overlay, presentation)) = self.recovery_overlay_and_presentation() else {
            return;
        };
        let endpoint_id = overlay.endpoint_id.clone();
        let rows = &overlay.rows;
        let Some(mut index) = presentation
            .selected_index
            .filter(|index| rows.get(*index).is_some_and(RecoveryRow::enabled))
            .or_else(|| rows.iter().position(RecoveryRow::enabled))
        else {
            return;
        };
        let direction = delta.signum();
        for _ in 0..delta.unsigned_abs() {
            let mut candidate = index as isize + direction;
            while candidate >= 0 && (candidate as usize) < rows.len() {
                if rows[candidate as usize].enabled() {
                    index = candidate as usize;
                    break;
                }
                candidate += direction;
            }
        }
        let next_id = rows[index].id.clone();
        if let Some(presentation) = self.recovery_presentations.get_mut(&endpoint_id) {
            presentation.selected = Some(next_id);
            presentation.selected_index = Some(index);
        }
    }

    pub(super) fn select_recovery_row(&mut self, row_index: usize) -> bool {
        let Some(ClientShellOverlay::Recovery(overlay)) = self.overlay.as_ref() else {
            return false;
        };
        let Some(row) = overlay.rows.get(row_index).filter(|row| row.enabled()) else {
            return false;
        };
        let presentation = self
            .recovery_presentations
            .entry(overlay.endpoint_id.clone())
            .or_default();
        presentation.selected = Some(row.id.clone());
        presentation.selected_index = Some(row_index);
        true
    }

    pub(super) fn activate_recovery_selection(&mut self, outcome: &mut ClientShellInput) {
        let Some((overlay, presentation)) = self.recovery_overlay_and_presentation() else {
            return;
        };
        let endpoint_id = overlay.endpoint_id.clone();
        let boot_id = overlay.boot_id.clone();
        let Some(row) = presentation
            .selected_index
            .and_then(|index| overlay.rows.get(index))
        else {
            return;
        };
        let RecoveryAction::Focus(target) = row.action.clone() else {
            return;
        };
        let current = self.recovery_overlay_is_current(overlay)
            && self.recovery_target_exists(&endpoint_id, &target);
        if !current {
            outcome.repaint |= self.push_recovery_notice(
                &endpoint_id,
                &boot_id,
                "recovery_focus_stale",
                "Terminal changed",
                "The recovery snapshot changed before focus. Reopen Terminal recovery and try again.",
            );
            return;
        }
        if self.focus_or_activate(endpoint_id, target, outcome) {
            self.overlay = None;
        }
        outcome.repaint = true;
    }

    fn recovery_target_exists(
        &self,
        endpoint_id: &ClientEndpointId,
        target: &ClientEndpointFocusTarget,
    ) -> bool {
        let Some(snapshot) = self
            .endpoints
            .iter()
            .find(|endpoint| &endpoint.endpoint_id == endpoint_id)
            .and_then(|endpoint| endpoint.snapshot.as_deref())
        else {
            return false;
        };
        match target {
            ClientEndpointFocusTarget::Workspace(id) => snapshot
                .workspaces
                .iter()
                .any(|workspace| workspace.workspace_id == *id),
            ClientEndpointFocusTarget::Tab(id) => snapshot.tabs.iter().any(|tab| tab.tab_id == *id),
            ClientEndpointFocusTarget::Pane(id) => {
                snapshot.panes.iter().any(|pane| pane.pane_id == *id)
            }
        }
    }

    pub(super) fn request_recovery_close(&mut self, outcome: &mut ClientShellInput) {
        let Some((overlay, presentation)) = self.recovery_overlay_and_presentation() else {
            return;
        };
        let Some(pane_id) = presentation
            .selected_index
            .and_then(|index| overlay.rows.get(index))
            .and_then(|row| row.close_pane_id.clone())
        else {
            return;
        };
        let endpoint_id = overlay.endpoint_id.clone();
        let connection_generation = overlay.connection_generation;
        let boot_id = overlay.boot_id.clone();
        let snapshot_revision = overlay.snapshot_revision;
        if !self.recovery_overlay_is_current(overlay)
            || !self.recovery_target_exists(
                &endpoint_id,
                &ClientEndpointFocusTarget::Pane(pane_id.clone()),
            )
        {
            outcome.repaint |= self.push_recovery_notice(
                &endpoint_id,
                &boot_id,
                "recovery_close_stale",
                "Terminal changed",
                "The recovery snapshot changed before close. Reopen Terminal recovery and try again.",
            );
            return;
        }
        let lease = RecoveryCloseLease {
            endpoint_id,
            connection_generation,
            boot_id,
            snapshot_revision,
            pane_id: pane_id.clone(),
        };
        self.overlay = Some(ClientShellOverlay::ConfirmClose(
            ClientConfirmCloseOverlay {
                target: ClientConfirmCloseTarget::RecoveryPane(lease),
                title: "Close terminal?".into(),
                detail: format!("Close {pane_id} and its running process. This cannot be undone."),
            },
        ));
        outcome.repaint = true;
    }

    pub(super) fn confirm_recovery_close(
        &mut self,
        lease: RecoveryCloseLease,
        outcome: &mut ClientShellInput,
    ) -> bool {
        let current = self
            .endpoints
            .iter()
            .find(|endpoint| endpoint.endpoint_id == lease.endpoint_id)
            .filter(|endpoint| endpoint.snapshot_generation == lease.connection_generation)
            .and_then(|endpoint| endpoint.snapshot.as_deref())
            .filter(|snapshot| {
                snapshot.boot_id == lease.boot_id && snapshot.revision == lease.snapshot_revision
            })
            .is_some_and(|snapshot| {
                snapshot
                    .panes
                    .iter()
                    .any(|pane| pane.pane_id == lease.pane_id)
            });
        if !current {
            outcome.repaint |= self.push_recovery_notice(
                &lease.endpoint_id,
                &lease.boot_id,
                "recovery_close_stale",
                "Terminal changed",
                "The recovery snapshot changed before confirmation. Reopen Terminal recovery and try again.",
            );
            return false;
        }
        self.push_endpoint_method_for_endpoint(
            &lease.endpoint_id,
            crate::api::schema::Method::PaneClose(crate::api::schema::PaneTarget {
                pane_id: lease.pane_id,
            }),
            outcome,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adversarial_normalization_keeps_first_identity_and_every_unique_terminal() {
        let mut first = super::super::tests::snapshot();
        first.workspaces.push(first.workspaces[0].clone());
        first.tabs.push(first.tabs[0].clone());
        first.panes.push(crate::protocol::ClientShellPane {
            pane_id: "orphan".into(),
            workspace_id: "missing-workspace".into(),
            tab_id: "missing-tab".into(),
            label: Some("orphan terminal".into()),
            cwd: None,
            foreground_cwd: None,
            focused: false,
            right_click_passthrough: false,
        });
        first.panes.push(crate::protocol::ClientShellPane {
            pane_id: "pane_1".into(),
            workspace_id: "missing-workspace".into(),
            tab_id: "missing-tab".into(),
            label: Some("duplicate must not replace first".into()),
            cwd: None,
            foreground_cwd: None,
            focused: false,
            right_click_passthrough: false,
        });
        let normalized = normalized_rows(&first);
        let terminals = normalized
            .iter()
            .filter(|row| matches!(row.id, RecoveryRowId::Terminal(_)))
            .collect::<Vec<_>>();
        assert_eq!(terminals.len(), 2);
        assert!(terminals.iter().any(|row| row.label == "orphan terminal"));
        assert!(terminals
            .iter()
            .all(|row| row.label != "duplicate must not replace first"));
    }

    #[test]
    fn frozen_generation_one_snapshot_alone_keeps_every_terminal_reachable() {
        let snapshot: ClientShellSnapshot = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/endpoint-snapshot-v1.json"
        )))
        .expect("published generation-one snapshot");
        let rows = normalized_rows(&snapshot);
        let reachable = rows
            .iter()
            .filter_map(|row| match &row.id {
                RecoveryRowId::Terminal(pane_id) => Some(pane_id.as_str()),
                _ => None,
            })
            .collect::<std::collections::BTreeSet<_>>();
        let projected = snapshot
            .panes
            .iter()
            .map(|pane| pane.pane_id.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(reachable, projected);
    }

    #[test]
    fn recovery_rows_use_only_snapshot_identity_and_labels() {
        let mut snapshot = super::super::tests::snapshot();
        snapshot.agents.push(crate::protocol::ClientShellAgent {
            pane_id: "pane_1".into(),
            workspace_id: "ws_1".into(),
            tab_id: "tab_1".into(),
            name: Some("feature agent".into()),
            display_agent: None,
            agent: None,
            title: None,
            terminal_title: None,
            terminal_title_stripped: None,
            agent_status: crate::api::schema::AgentStatus::Blocked,
            state_change_seq: 1,
            state_labels: Vec::new(),
            tokens: Vec::new(),
            focused: true,
        });
        let rows = normalized_rows(&snapshot);
        assert!(rows.iter().all(|row| !row.label.contains("agent")));
        assert!(rows.iter().any(|row| row.label == "terminal pane_1"));
    }
}
