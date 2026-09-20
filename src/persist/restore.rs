use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use ratatui::layout::Direction;
use tokio::sync::{mpsc, Notify};
use tracing::{error, warn};

use crate::detect::AgentState;
use crate::events::AppEvent;
#[cfg(test)]
use crate::layout::Node;
use crate::layout::{CollectionId, LayoutLeaf, PaneId, TileLayout, TypedNode};
use crate::legacy_collection_snapshot::LegacyCollectionSnapshot;
use crate::pane::{PaneLaunchEnv, PaneState};
use crate::render_signal::RenderSignal;
use crate::terminal::{TerminalId, TerminalRuntime, TerminalState};
use crate::terminal_container::{TerminalContainer, TerminalContainerId};
use crate::workspace::Workspace;

use super::snapshot::{
    CollectionSnapshot, LayoutLeafSnapshot, PaneAgentSessionSnapshot, PaneHistorySnapshot,
    TabHistorySnapshot, WorkspaceHistorySnapshot,
};
use super::{
    DirectionSnapshot, LayoutSnapshot, SessionHistorySnapshot, SessionSnapshot, TabSnapshot,
    WorkspaceSnapshot,
};

struct AgentRestoreState<'a> {
    enabled: bool,
    resumed_sessions: &'a mut HashSet<String>,
}

struct PaneRestoreStartup<'a> {
    restore_plan: Option<crate::agent_resume::AgentResumePlan>,
    initial_history_ansi: Option<&'a str>,
    duplicate_agent_session: bool,
    reserved_agent_session: Option<String>,
}

struct RestoreRuntimeContext<'a> {
    scrollback_limit_bytes: usize,
    shell_config: crate::pane::PaneShellConfig<'a>,
    resume_agents_on_restore: bool,
    events: mpsc::Sender<AppEvent>,
    render_notify: Arc<Notify>,
    render_dirty: Arc<RenderSignal>,
}

type RestoredSession = (
    Vec<Workspace>,
    HashMap<TerminalId, TerminalState>,
    HashMap<TerminalId, TerminalRuntime>,
);
type RestoredWorkspace = (
    Workspace,
    Vec<TerminalState>,
    HashMap<TerminalId, TerminalRuntime>,
);
type RestoredTab = (
    crate::workspace::Tab,
    Vec<TerminalState>,
    HashMap<TerminalId, TerminalRuntime>,
    HashMap<PaneId, u32>,
);
type RestoreFailures<T> = (T, usize);

struct RecoveredTabLayout {
    layout: TileLayout,
    legacy_collection_snapshots: HashMap<TerminalContainerId, LegacyCollectionSnapshot>,
}

/// Restore delegation provenance while remapping persisted pane IDs to fresh runtime IDs.
pub fn restore_delegations(
    snapshot: &SessionSnapshot,
    workspaces: &[Workspace],
) -> crate::delegation::Delegations {
    let mut pane_map = HashMap::new();
    for (workspace_snapshot, workspace) in snapshot.workspaces.iter().zip(workspaces) {
        for (&old, &public_number) in &workspace_snapshot.public_pane_numbers {
            if let Some((&pane, _)) = workspace
                .public_pane_numbers
                .iter()
                .find(|(_, number)| **number == public_number)
            {
                pane_map.insert(old, pane);
            }
        }
        // Pre-public-ID snapshots are deterministic when every pane survived restore.
        for (tab_snapshot, tab) in workspace_snapshot.tabs.iter().zip(&workspace.tabs) {
            let mut old: Vec<_> = tab_snapshot
                .panes
                .keys()
                .copied()
                .filter(|id| !pane_map.contains_key(id))
                .collect();
            old.sort_unstable();
            let mut new: Vec<_> = tab
                .panes
                .keys()
                .copied()
                .filter(|pane| !pane_map.values().any(|mapped| mapped == pane))
                .collect();
            new.sort_by_key(|pane| pane.raw());
            if old.len() == new.len() {
                pane_map.extend(old.into_iter().zip(new));
            }
        }
    }
    let mut records = snapshot
        .delegations
        .iter()
        .cloned()
        .map(|mut record| {
            if let Some(old) = record.pane_id {
                record.pane_id = pane_map.get(&old.raw()).copied();
                record.tombstone |= record.pane_id.is_none();
            }
            record
        })
        .collect::<Vec<_>>();
    records.sort_by_key(|record| (record.id, record.sibling_rank));
    records.truncate(crate::admission::MAX_RELATIONSHIPS);

    // Semantic relationship corruption must not prevent PTY restoration. Keep
    // a deterministic bounded prefix and root links that exceed current
    // fan-out/depth limits; repair_records handles duplicates and cycles.
    let repaired = crate::delegation::Delegations::repair_records(records);
    let mut records = repaired.records().values().cloned().collect::<Vec<_>>();
    records.sort_by_key(|record| record.id);
    let mut accepted_parents = HashMap::new();
    let mut child_counts = HashMap::new();
    for record in &mut records {
        let valid_parent = record.parent_id.filter(|parent| {
            if !accepted_parents.contains_key(parent)
                || child_counts.get(parent).copied().unwrap_or(0)
                    >= crate::admission::MAX_RELATIONSHIP_FAN_OUT
            {
                return false;
            }
            let mut depth = 1usize;
            let mut current = Some(*parent);
            while let Some(id) = current {
                if depth > crate::admission::MAX_RELATIONSHIP_DEPTH {
                    return false;
                }
                current = accepted_parents.get(&id).copied().flatten();
                depth += 1;
            }
            true
        });
        record.parent_id = valid_parent;
        if let Some(parent) = valid_parent {
            *child_counts.entry(parent).or_insert(0usize) += 1;
        }
        accepted_parents.insert(record.id, valid_parent);
    }
    let repaired = crate::delegation::Delegations::repair_records(records);
    debug_assert!(crate::admission::relationships(&repaired).is_ok());
    repaired
}

pub(crate) fn validate_snapshot_pane_admission(snapshot: &SessionSnapshot) -> Result<(), String> {
    let mut session_panes = 0usize;
    for workspace in &snapshot.workspaces {
        let mut workspace_panes = 0usize;
        for tab in &workspace.tabs {
            let tab_panes = restored_tab_identity_count(tab);
            crate::admission::panes(session_panes, workspace_panes, 0, tab_panes)
                .map_err(|error| error.to_string())?;
            session_panes = session_panes
                .checked_add(tab_panes)
                .ok_or_else(|| "session pane count overflow".to_string())?;
            workspace_panes = workspace_panes
                .checked_add(tab_panes)
                .ok_or_else(|| "workspace pane count overflow".to_string())?;
        }
    }
    Ok(())
}

pub(crate) fn validate_snapshot_admission(snapshot: &SessionSnapshot) -> Result<(), String> {
    validate_snapshot_pane_admission(snapshot)?;
    let mut workspace_ids = HashSet::new();
    let mut tab_ids = HashSet::new();
    let mut terminal_ids = HashSet::new();
    for workspace in &snapshot.workspaces {
        let workspace_id = workspace
            .id
            .as_deref()
            .ok_or_else(|| "snapshot workspace has no identity".to_string())?;
        let workspace_number = crate::workspace::public_workspace_number(workspace_id)
            .filter(|number| *number > 0)
            .ok_or_else(|| "snapshot workspace identity is invalid".to_string())?;
        if workspace_id
            != format!(
                "w{}",
                crate::workspace::encode_public_number(workspace_number)
            )
            || !workspace_ids.insert(workspace_id)
        {
            return Err("snapshot contains noncanonical or duplicate workspace identities".into());
        }
        if workspace.public_tab_numbers.len() != workspace.tabs.len() {
            return Err("snapshot public tab-number projection is incomplete".into());
        }
        let mut public_tab_numbers = HashSet::new();
        for number in &workspace.public_tab_numbers {
            if *number == 0 || !public_tab_numbers.insert(*number) {
                return Err("snapshot contains invalid or duplicate public tab numbers".into());
            }
        }
        let maximum_tab_number = public_tab_numbers.iter().copied().max().unwrap_or(0);
        if workspace.next_public_tab_number <= maximum_tab_number
            || workspace.next_public_tab_number == 0
        {
            return Err("snapshot next public tab number is reusable or invalid".into());
        }
        for tab in &workspace.tabs {
            let tab_id = super::snapshot::tab_id_for_snapshot(tab)?;
            if !tab_ids.insert(tab_id) {
                return Err("snapshot contains duplicate tab identities".into());
            }
            for pane in tab.panes.values() {
                let terminal_id = super::snapshot::terminal_id_for_pane_snapshot(pane)?;
                if !terminal_ids.insert(terminal_id) {
                    return Err("snapshot contains duplicate terminal identities".into());
                }
            }
        }
    }
    // Collection and relationship records are semantic projections. They are
    // repaired after pane identities/runtimes have been admitted so corrupt or
    // over-limit feature state cannot make otherwise valid terminals vanish.
    Ok(())
}

fn reserve_restored_collection_ids(snapshot: &SessionSnapshot) {
    fn reserve_layout(layout: &LayoutSnapshot) {
        match layout {
            LayoutSnapshot::Pane(_) => {}
            LayoutSnapshot::Collection(id) => {
                let _ = CollectionId::from_raw(id.raw());
            }
            LayoutSnapshot::Split { first, second, .. } => {
                reserve_layout(first);
                reserve_layout(second);
            }
        }
    }

    for tab in snapshot
        .workspaces
        .iter()
        .flat_map(|workspace| &workspace.tabs)
    {
        // Deserialization already reserves IDs, but restore also accepts
        // programmatically assembled snapshots in tests and handoff paths.
        reserve_layout(&tab.layout);
        for collection in &tab.collections {
            let _ = CollectionId::from_raw(collection.id.raw());
        }
        if let Some(LayoutLeafSnapshot::Collection(id)) = tab.focused_leaf {
            let _ = CollectionId::from_raw(id.raw());
        }
    }
}

fn restored_tab_identity_count(tab: &super::snapshot::TabSnapshot) -> usize {
    fn collect_layout_ids(layout: &super::snapshot::LayoutSnapshot, ids: &mut HashSet<u32>) {
        match layout {
            super::snapshot::LayoutSnapshot::Pane(id) => {
                ids.insert(*id);
            }
            super::snapshot::LayoutSnapshot::Collection(_) => {}
            super::snapshot::LayoutSnapshot::Split { first, second, .. } => {
                collect_layout_ids(first, ids);
                collect_layout_ids(second, ids);
            }
        }
    }

    let mut ids = tab.panes.keys().copied().collect::<HashSet<_>>();
    collect_layout_ids(&tab.layout, &mut ids);
    ids.len()
}

/// Restore workspaces from a snapshot. Each pane gets a fresh shell in its saved cwd.
pub fn restore(
    snapshot: &SessionSnapshot,
    history: Option<&SessionHistorySnapshot>,
    rows: u16,
    cols: u16,
    scrollback_limit_bytes: usize,
    default_shell: &str,
    shell_mode: crate::config::ShellModeConfig,
    resume_agents_on_restore: bool,
    events: mpsc::Sender<AppEvent>,
    render_notify: Arc<Notify>,
    render_dirty: Arc<RenderSignal>,
) -> Result<RestoredSession, String> {
    validate_snapshot_pane_admission(snapshot)?;
    let mut normalized = snapshot.clone();
    super::snapshot::normalize_terminal_identities(&mut normalized, false)?;
    validate_snapshot_admission(&normalized)?;
    let mut imported_panes = HashMap::new();
    Ok(restore_with_imports(
        &normalized,
        history,
        rows,
        cols,
        scrollback_limit_bytes,
        crate::pane::PaneShellConfig::new(default_shell, shell_mode),
        resume_agents_on_restore,
        &mut imported_panes,
        events,
        render_notify,
        render_dirty,
    ))
}

#[cfg(unix)]
pub fn restore_handoff(
    snapshot: &SessionSnapshot,
    scrollback_limit_bytes: usize,
    default_shell: &str,
    shell_mode: crate::config::ShellModeConfig,
    imports: &mut HashMap<u32, crate::handoff_runtime::ImportedHandoffRuntime>,
    events: mpsc::Sender<AppEvent>,
    render_notify: Arc<Notify>,
    render_dirty: Arc<RenderSignal>,
) -> std::io::Result<RestoredSession> {
    validate_snapshot_pane_admission(snapshot)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    let mut normalized = snapshot.clone();
    super::snapshot::normalize_terminal_identities(&mut normalized, false)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    validate_snapshot_admission(&normalized)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    restore_with_imports_strict(
        &normalized,
        None,
        24,
        80,
        scrollback_limit_bytes,
        crate::pane::PaneShellConfig::new(default_shell, shell_mode),
        true,
        imports,
        events,
        render_notify,
        render_dirty,
    )
}

#[cfg(unix)]
pub fn handoff_pane_aliases(
    snapshot: &SessionSnapshot,
    workspaces: &[Workspace],
) -> HashMap<u32, PaneId> {
    let mut aliases = HashMap::new();
    for (ws_snap, workspace) in snapshot.workspaces.iter().zip(workspaces) {
        for (tab_snap, tab) in ws_snap.tabs.iter().zip(&workspace.tabs) {
            let old_ids = collect_snapshot_pane_ids(&tab_snap.layout);
            let new_ids = tab.layout.pane_ids();
            for (old_id, new_id) in old_ids.into_iter().zip(new_ids) {
                if old_id != new_id.raw() {
                    aliases.insert(old_id, new_id);
                }
            }
        }
    }
    aliases
}

#[cfg(unix)]
fn collect_snapshot_pane_ids(node: &LayoutSnapshot) -> Vec<u32> {
    let mut ids = Vec::new();
    collect_snapshot_ids_inner(node, &mut ids);
    ids
}

#[cfg(unix)]
fn collect_snapshot_ids_inner(node: &LayoutSnapshot, ids: &mut Vec<u32>) {
    match node {
        LayoutSnapshot::Pane(id) => ids.push(*id),
        LayoutSnapshot::Collection(_) => {}
        LayoutSnapshot::Split { first, second, .. } => {
            collect_snapshot_ids_inner(first, ids);
            collect_snapshot_ids_inner(second, ids);
        }
    }
}

fn migrated_public_pane_numbers_by_old_raw(
    snap: &WorkspaceSnapshot,
    next_public_pane_number: &mut usize,
) -> HashMap<u32, usize> {
    let mut public_numbers = snap.public_pane_numbers.clone();
    for tab in &snap.tabs {
        let mut pane_ids: Vec<_> = tab.panes.keys().copied().collect();
        collect_layout_snapshot_pane_ids(&tab.layout, &mut pane_ids);
        pane_ids.sort_unstable();
        pane_ids.dedup();
        for old_raw in pane_ids {
            public_numbers.entry(old_raw).or_insert_with(|| {
                let number = *next_public_pane_number;
                *next_public_pane_number += 1;
                number
            });
        }
    }
    public_numbers
}

fn collect_layout_snapshot_pane_ids(node: &LayoutSnapshot, ids: &mut Vec<u32>) {
    match node {
        LayoutSnapshot::Pane(id) => ids.push(*id),
        LayoutSnapshot::Collection(_) => {}
        LayoutSnapshot::Split { first, second, .. } => {
            collect_layout_snapshot_pane_ids(first, ids);
            collect_layout_snapshot_pane_ids(second, ids);
        }
    }
}

#[cfg(unix)]
fn restore_with_imports_strict(
    snapshot: &SessionSnapshot,
    history: Option<&SessionHistorySnapshot>,
    rows: u16,
    cols: u16,
    scrollback_limit_bytes: usize,
    shell_config: crate::pane::PaneShellConfig<'_>,
    resume_agents_on_restore: bool,
    imported_panes: &mut HashMap<u32, crate::handoff_runtime::ImportedHandoffRuntime>,
    events: mpsc::Sender<AppEvent>,
    render_notify: Arc<Notify>,
    render_dirty: Arc<RenderSignal>,
) -> std::io::Result<RestoredSession> {
    let (restored, failed_imports) = restore_with_imports_and_failures(
        snapshot,
        history,
        rows,
        cols,
        scrollback_limit_bytes,
        shell_config,
        resume_agents_on_restore,
        imported_panes,
        events,
        render_notify,
        render_dirty,
    );
    if failed_imports > 0 {
        return Err(std::io::Error::other(format!(
            "handoff failed to restore {failed_imports} imported pane runtime(s)"
        )));
    }
    if !imported_panes.is_empty() {
        return Err(std::io::Error::other(format!(
            "handoff import did not consume {} pane runtime(s)",
            imported_panes.len()
        )));
    }
    Ok(restored)
}

fn restore_with_imports(
    snapshot: &SessionSnapshot,
    history: Option<&SessionHistorySnapshot>,
    rows: u16,
    cols: u16,
    scrollback_limit_bytes: usize,
    shell_config: crate::pane::PaneShellConfig<'_>,
    resume_agents_on_restore: bool,
    imported_panes: &mut HashMap<u32, crate::handoff_runtime::ImportedHandoffRuntime>,
    events: mpsc::Sender<AppEvent>,
    render_notify: Arc<Notify>,
    render_dirty: Arc<RenderSignal>,
) -> RestoredSession {
    restore_with_imports_and_failures(
        snapshot,
        history,
        rows,
        cols,
        scrollback_limit_bytes,
        shell_config,
        resume_agents_on_restore,
        imported_panes,
        events,
        render_notify,
        render_dirty,
    )
    .0
}

fn restore_with_imports_and_failures(
    snapshot: &SessionSnapshot,
    history: Option<&SessionHistorySnapshot>,
    rows: u16,
    cols: u16,
    scrollback_limit_bytes: usize,
    shell_config: crate::pane::PaneShellConfig<'_>,
    resume_agents_on_restore: bool,
    imported_panes: &mut HashMap<u32, crate::handoff_runtime::ImportedHandoffRuntime>,
    events: mpsc::Sender<AppEvent>,
    render_notify: Arc<Notify>,
    render_dirty: Arc<RenderSignal>,
) -> RestoreFailures<RestoredSession> {
    let mut workspaces = Vec::new();
    let mut terminals = HashMap::new();
    let mut terminal_runtimes = HashMap::new();
    let mut resumed_agent_sessions = HashSet::new();
    let mut failed_imports = 0;
    let mut restored_collection_ids = HashSet::new();
    reserve_restored_collection_ids(snapshot);
    for (idx, ws_snap) in snapshot.workspaces.iter().enumerate() {
        let runtime_context = RestoreRuntimeContext {
            scrollback_limit_bytes,
            shell_config,
            resume_agents_on_restore,
            events: events.clone(),
            render_notify: render_notify.clone(),
            render_dirty: render_dirty.clone(),
        };
        let (restored, workspace_failed_imports) = restore_workspace(
            ws_snap,
            history.and_then(|history| history.workspaces.get(idx)),
            rows,
            cols,
            &runtime_context,
            &mut resumed_agent_sessions,
            imported_panes,
            &mut restored_collection_ids,
        );
        failed_imports += workspace_failed_imports;
        if let Some((workspace, restored_terminals, restored_runtimes)) = restored {
            for terminal in restored_terminals {
                terminals.insert(terminal.id.clone(), terminal);
            }
            terminal_runtimes.extend(restored_runtimes);
            workspaces.push(workspace);
        }
    }
    crate::workspace::reserve_workspace_ids(&workspaces);
    ((workspaces, terminals, terminal_runtimes), failed_imports)
}

fn restore_workspace(
    snap: &WorkspaceSnapshot,
    history: Option<&WorkspaceHistorySnapshot>,
    rows: u16,
    cols: u16,
    runtime_context: &RestoreRuntimeContext<'_>,
    resumed_agent_sessions: &mut HashSet<String>,
    imported_panes: &mut HashMap<u32, crate::handoff_runtime::ImportedHandoffRuntime>,
    restored_collection_ids: &mut HashSet<TerminalContainerId>,
) -> RestoreFailures<Option<RestoredWorkspace>> {
    let mut tabs = Vec::new();
    let mut terminals = Vec::new();
    let mut terminal_runtimes = HashMap::new();
    let workspace_id = snap
        .id
        .clone()
        .unwrap_or_else(crate::workspace::generate_workspace_id);
    let mut next_public_pane_number = snap
        .public_pane_numbers
        .values()
        .copied()
        .max()
        .and_then(|max| max.checked_add(1))
        .unwrap_or(1)
        .max(snap.next_public_pane_number);
    let public_pane_numbers_by_old_raw =
        migrated_public_pane_numbers_by_old_raw(snap, &mut next_public_pane_number);
    let public_pane_ids_by_old_raw: HashMap<u32, String> = public_pane_numbers_by_old_raw
        .iter()
        .map(|(old_raw, public_number)| {
            (
                *old_raw,
                format!(
                    "{}:p{}",
                    workspace_id,
                    crate::workspace::encode_public_number(*public_number)
                ),
            )
        })
        .collect();
    let mut public_pane_numbers = HashMap::new();
    let mut next_public_tab_number = snap
        .public_tab_numbers
        .iter()
        .copied()
        .max()
        .and_then(|max| max.checked_add(1))
        .unwrap_or(1)
        .max(snap.next_public_tab_number);
    let mut failed_imports = 0;

    for (idx, tab_snap) in snap.tabs.iter().enumerate() {
        let tab_number = snap
            .public_tab_numbers
            .get(idx)
            .copied()
            .unwrap_or_else(|| idx.saturating_add(1));
        let (restored_tab, tab_failed_imports) = restore_tab(
            tab_snap,
            history.and_then(|history| history.tabs.get(idx)),
            tab_number,
            &workspace_id,
            rows,
            cols,
            runtime_context,
            resumed_agent_sessions,
            imported_panes,
            &public_pane_ids_by_old_raw,
            restored_collection_ids,
        );
        failed_imports += tab_failed_imports;
        let Some((mut tab, restored_terminals, restored_runtimes, reverse_id_map)) = restored_tab
        else {
            continue;
        };
        if let Some(public_tab_number) = snap.public_tab_numbers.get(idx).copied() {
            tab.number = public_tab_number;
        }
        if let Some(following_number) = tab.number.checked_add(1) {
            next_public_tab_number = next_public_tab_number.max(following_number);
        }
        for pane_id in tab.layout.pane_ids() {
            let public_number = public_pane_numbers_by_old_raw
                .get(
                    &reverse_id_map
                        .get(&pane_id)
                        .copied()
                        .unwrap_or(pane_id.raw()),
                )
                .copied()
                .unwrap_or_else(|| {
                    let number = next_public_pane_number;
                    next_public_pane_number += 1;
                    number
                });
            public_pane_numbers.insert(pane_id, public_number);
            next_public_pane_number = next_public_pane_number.max(public_number + 1);
        }
        terminals.extend(restored_terminals);
        terminal_runtimes.extend(restored_runtimes);
        tabs.push(tab);
    }

    if tabs.is_empty() {
        return (None, failed_imports);
    }

    let worktree_space = restored_worktree_space_membership(snap.worktree_space.clone());
    let (cached_git_space, cached_auto_label, cached_git_status_key) =
        crate::workspace::discover_workspace_git_identity(&snap.identity_cwd);

    (
        Some(Workspace {
            id: workspace_id,
            custom_name: snap.custom_name.clone(),
            identity_cwd: snap.identity_cwd.clone(),
            cached_identity_cwd: snap.identity_cwd.clone(),
            cached_auto_label,
            cached_git_status_key,
            cached_git_branch: crate::workspace::git_branch(&snap.identity_cwd),
            cached_git_ahead_behind: None,
            cached_git_space,
            checkout: snap.checkout.clone(),
            worktree_space,
            metadata_tokens: crate::metadata_tokens::MetadataTokens::default(),
            metadata_token_sequences: HashMap::new(),
            public_pane_numbers,
            next_public_pane_number,
            next_public_tab_number,
            active_tab: snap.active_tab.min(tabs.len().saturating_sub(1)),
            tabs,
            #[cfg(test)]
            test_runtimes: HashMap::new(),
        })
        .map(|workspace| (workspace, terminals, terminal_runtimes)),
        failed_imports,
    )
}

fn restored_worktree_space_membership(
    space: Option<crate::workspace::WorktreeSpaceMembership>,
) -> Option<crate::workspace::WorktreeSpaceMembership> {
    space.filter(|space| {
        space.checkout_path.exists()
            && crate::workspace::git_space_metadata(&space.checkout_path)
                .is_some_and(|current| current.key == space.key)
    })
}

fn restore_tab(
    snap: &TabSnapshot,
    history: Option<&TabHistorySnapshot>,
    number: usize,
    workspace_id: &str,
    rows: u16,
    cols: u16,
    runtime_context: &RestoreRuntimeContext<'_>,
    resumed_agent_sessions: &mut HashSet<String>,
    imported_panes: &mut HashMap<u32, crate::handoff_runtime::ImportedHandoffRuntime>,
    public_pane_ids_by_old_raw: &HashMap<u32, String>,
    restored_collection_ids: &mut HashSet<TerminalContainerId>,
) -> RestoreFailures<Option<RestoredTab>> {
    let tab_id = match super::snapshot::tab_id_for_snapshot(snap) {
        Ok(id) => id,
        Err(error) => {
            error!(%error, "restored tab identity is invalid");
            return (None, 0);
        }
    };
    let (typed_node, id_map) = restore_typed_node_remapped(snap);
    let reverse_id_map: HashMap<PaneId, u32> = id_map
        .iter()
        .map(|(&old_id, &new_id)| (new_id, old_id))
        .collect();
    let mut pane_ids: Vec<_> = id_map.iter().map(|(old, new)| (*old, *new)).collect();
    pane_ids.sort_by_key(|(old, _)| *old);
    let pane_ids: Vec<_> = pane_ids.into_iter().map(|(_, new)| new).collect();

    let mut panes = HashMap::new();
    let mut terminals = Vec::new();
    let mut terminal_runtimes = HashMap::new();
    let mut failed_imports = 0;
    for id in &pane_ids {
        let old_id = reverse_id_map.get(id);
        let saved_pane = old_id.and_then(|old_id| snap.panes.get(old_id));
        let terminal_id = match saved_pane
            .map(super::snapshot::terminal_id_for_pane_snapshot)
            .transpose()
        {
            Ok(Some(terminal_id)) => terminal_id,
            Ok(None) => {
                error!(
                    pane_id = id.raw(),
                    "restored pane has no persisted terminal identity"
                );
                return (None, failed_imports);
            }
            Err(error) => {
                error!(pane_id = id.raw(), %error, "restored pane terminal identity is invalid");
                return (None, failed_imports);
            }
        };
        let saved_cwd = saved_pane
            .map(|p| p.cwd.clone())
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| "/".into()));

        let cwd = if saved_cwd.exists() {
            saved_cwd
        } else {
            warn!(
                cwd = %saved_cwd.display(),
                "saved pane cwd does not exist, falling back to HOME"
            );
            let home = std::env::var("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("/"));
            if home.exists() {
                home
            } else {
                PathBuf::from("/")
            }
        };

        let saved_label = saved_pane.and_then(|p| p.label.clone());
        let saved_agent_name = saved_pane.and_then(|p| p.agent_name.clone());
        let saved_managed_agent = saved_pane
            .and_then(|pane| pane.managed_agent_kind.as_deref())
            .and_then(crate::detect::parse_canonical_agent_label);
        let saved_launch_argv = saved_pane.and_then(|p| p.launch_argv.clone());
        let saved_agent_session = saved_pane.and_then(|p| p.agent_session.as_ref());
        let saved_history =
            old_id.and_then(|old_id| history.and_then(|history| history.panes.get(old_id)));
        let startup = {
            let mut agent_restore = AgentRestoreState {
                enabled: runtime_context.resume_agents_on_restore,
                resumed_sessions: resumed_agent_sessions,
            };
            pane_restore_startup(saved_agent_session, saved_history, &mut agent_restore)
        };
        let restored_agent_session =
            restored_terminal_agent_session(saved_agent_session, startup.duplicate_agent_session);
        let initial_restore_agent = startup
            .restore_plan
            .as_ref()
            .and_then(|plan| crate::detect::parse_agent_label(&plan.agent));

        let old_pane_id = reverse_id_map.get(id).copied();
        let public_pane_id = old_pane_id
            .and_then(|old_id| public_pane_ids_by_old_raw.get(&old_id))
            .map(String::as_str);
        let launch_env = public_pane_id
            .map(|pane_id| {
                PaneLaunchEnv::from_extra(Vec::new()).with_identity(
                    workspace_id.to_string(),
                    crate::workspace::public_tab_id_for_number(workspace_id, number),
                    pane_id.to_string(),
                )
            })
            .unwrap_or_default();
        let imported_runtime = old_pane_id.and_then(|old_id| imported_panes.remove(&old_id));
        let was_imported = imported_runtime.is_some();
        let pending_native_agent_restore = if was_imported {
            None
        } else {
            startup.restore_plan.clone()
        };
        if let Some(plan) = pending_native_agent_restore {
            let mut terminal = TerminalState::new(terminal_id.clone(), cwd.clone())
                .with_pending_agent_resume_plan(plan);
            if let Some(label) = saved_label {
                terminal.set_manual_label(label);
            }
            if let Some(session) = restored_agent_session {
                terminal.set_persisted_agent_session(session);
            }
            match (saved_agent_name, saved_managed_agent) {
                (Some(agent_name), Some(agent)) => {
                    terminal.restore_managed_agent(agent_name, agent)
                }
                (Some(_), None) => {}
                (None, _) => {}
            }
            if let Some(agent) = initial_restore_agent {
                let _ = terminal.set_detected_state_with_screen_signals_at(
                    Some(agent),
                    AgentState::Idle,
                    false,
                    false,
                    false,
                    false,
                    std::time::Instant::now(),
                );
            }
            panes.insert(*id, PaneState::new(terminal_id));
            terminals.push(terminal);
            continue;
        }

        #[cfg(not(unix))]
        if imported_runtime.is_some() {
            failed_imports += 1;
            continue;
        }

        let runtime_result = {
            #[cfg(unix)]
            if let Some(imported) = imported_runtime {
                TerminalRuntime::from_handoff_fd(
                    crate::handoff_runtime::ImportedHandoffRuntime {
                        master_fd: imported.master_fd,
                        state: imported.state.with_pane_id(*id),
                    },
                    runtime_context.scrollback_limit_bytes,
                    crate::terminal_theme::TerminalTheme::default(),
                    None,
                    runtime_context.events.clone(),
                    runtime_context.render_notify.clone(),
                    runtime_context.render_dirty.clone(),
                )
            } else {
                TerminalRuntime::spawn_with_initial_history(
                    *id,
                    rows,
                    cols,
                    cwd.clone(),
                    runtime_context.scrollback_limit_bytes,
                    crate::terminal_theme::TerminalTheme::default(),
                    None,
                    runtime_context.shell_config,
                    &launch_env,
                    startup.initial_history_ansi,
                    runtime_context.events.clone(),
                    runtime_context.render_notify.clone(),
                    runtime_context.render_dirty.clone(),
                )
            }

            #[cfg(not(unix))]
            {
                TerminalRuntime::spawn_with_initial_history(
                    *id,
                    rows,
                    cols,
                    cwd.clone(),
                    runtime_context.scrollback_limit_bytes,
                    crate::terminal_theme::TerminalTheme::default(),
                    None,
                    runtime_context.shell_config,
                    &launch_env,
                    startup.initial_history_ansi,
                    runtime_context.events.clone(),
                    runtime_context.render_notify.clone(),
                    runtime_context.render_dirty.clone(),
                )
            }
        };

        match runtime_result {
            Ok(runtime) => {
                let mut terminal = TerminalState::new(terminal_id.clone(), cwd.clone());
                if was_imported {
                    if let Some(argv) = saved_launch_argv {
                        terminal = terminal.with_launch_argv(argv).with_respawn_shell_on_exit();
                    }
                }
                if let Some(label) = saved_label {
                    terminal.set_manual_label(label);
                }
                if let Some(session) = restored_agent_session {
                    terminal.set_persisted_agent_session(session);
                }
                match (saved_agent_name, saved_managed_agent) {
                    (Some(agent_name), Some(agent)) if was_imported => {
                        terminal.restore_managed_agent(agent_name, agent)
                    }
                    (Some(_), Some(_)) => {}
                    (Some(agent_name), None) if was_imported => terminal.set_agent_name(agent_name),
                    (Some(_), None) => {}
                    (None, _) => {}
                }
                if let Some(agent) = initial_restore_agent {
                    let _ = terminal.set_detected_state_with_screen_signals_at(
                        Some(agent),
                        AgentState::Idle,
                        false,
                        false,
                        false,
                        false,
                        std::time::Instant::now(),
                    );
                }
                panes.insert(*id, PaneState::new(terminal_id.clone()));
                terminal_runtimes.insert(terminal_id, runtime);
                terminals.push(terminal);
            }
            Err(e) => {
                if let Some(key) = startup.reserved_agent_session.as_deref() {
                    resumed_agent_sessions.remove(key);
                }
                if was_imported {
                    failed_imports += 1;
                    error!(
                        tab = ?snap.custom_name,
                        pane_id = id.raw(),
                        err = %e,
                        "failed to restore imported pane"
                    );
                }
                error!(
                    tab = ?snap.custom_name,
                    pane_id = id.raw(),
                    err = %e,
                    "failed to restore pane, skipping"
                );
            }
        }
    }

    if panes.is_empty() {
        warn!(
            tab = ?snap.custom_name,
            "no panes could be restored for tab, dropping it"
        );
        return (None, failed_imports);
    }

    let surviving: HashSet<PaneId> = panes.keys().copied().collect();
    let Some(recovered) = recover_typed_layout(
        snap,
        typed_node,
        &id_map,
        &surviving,
        restored_collection_ids,
    ) else {
        warn!(tab = ?snap.custom_name, "restored tab lost all panes while recovering collection placement");
        return (None, failed_imports);
    };
    let pane_ids = recovered.layout.pane_ids();
    let Some(root_pane) = resolve_restored_pane(snap.root_pane, &id_map, &surviving, &pane_ids)
    else {
        return (None, failed_imports);
    };

    (
        Some((
            match crate::workspace::Tab::from_restored(
                tab_id,
                snap.custom_name.clone(),
                number,
                root_pane,
                recovered.layout,
                recovered.legacy_collection_snapshots,
                panes,
                snap.zoomed,
                runtime_context.events.clone(),
                runtime_context.render_notify.clone(),
                runtime_context.render_dirty.clone(),
            ) {
                Some(tab) => tab,
                None => return (None, failed_imports),
            },
            terminals,
            terminal_runtimes,
            reverse_id_map,
        )),
        failed_imports,
    )
}

fn pane_restore_startup<'a>(
    session: Option<&PaneAgentSessionSnapshot>,
    history: Option<&'a PaneHistorySnapshot>,
    agent_restore: &mut AgentRestoreState<'_>,
) -> PaneRestoreStartup<'a> {
    // Native agent resume owns the conversation history. If a pane has a
    // resumable agent session and resume is enabled, do not replay saved pane
    // presentation history into that terminal, even when this pane is a
    // duplicate suppressed by session de-duplication.
    let restore_plan =
        session.and_then(|session| restore_plan_for_snapshot(session, agent_restore.enabled));
    let has_native_agent_restore = restore_plan.is_some();
    // Reserve before spawning so later panes in the same restore pass cannot
    // launch the same native agent session. The caller rolls this reservation
    // back if runtime spawn fails before any agent process is started.
    let mut reserved_agent_session = None;
    let duplicate_agent_session = restore_plan.as_ref().is_some_and(|plan| {
        if agent_restore
            .resumed_sessions
            .insert(plan.dedupe_key.clone())
        {
            reserved_agent_session = Some(plan.dedupe_key.clone());
            false
        } else {
            true
        }
    });
    let restore_plan = if duplicate_agent_session {
        None
    } else {
        restore_plan
    };

    PaneRestoreStartup {
        restore_plan,
        initial_history_ansi: if has_native_agent_restore {
            None
        } else {
            history.map(|history| history.ansi.as_str())
        },
        duplicate_agent_session,
        reserved_agent_session,
    }
}

fn restore_plan_for_snapshot(
    session: &PaneAgentSessionSnapshot,
    resume_agents_on_restore: bool,
) -> Option<crate::agent_resume::AgentResumePlan> {
    if !resume_agents_on_restore {
        return None;
    }
    let persisted = persisted_agent_session_from_snapshot(session)?;
    crate::agent_resume::plan(&persisted.source, &persisted.agent, &persisted.session_ref)
}

fn persisted_agent_session_from_snapshot(
    session: &PaneAgentSessionSnapshot,
) -> Option<crate::agent_resume::PersistedAgentSession> {
    let source = match (session.source.as_str(), session.agent.as_str()) {
        ("herdr:pi", "pi") => "vqro:pi",
        ("herdr:claude", "claude") => "vqro:claude",
        ("herdr:codex", "codex") => "vqro:codex",
        _ => &session.source,
    };
    crate::agent_resume::session_ref_from_snapshot(
        source,
        &session.agent,
        session.kind,
        &session.value,
    )
}

fn restored_terminal_agent_session(
    session: Option<&PaneAgentSessionSnapshot>,
    duplicate_agent_session: bool,
) -> Option<crate::agent_resume::PersistedAgentSession> {
    if duplicate_agent_session {
        return None;
    }
    session.and_then(persisted_agent_session_from_snapshot)
}

#[cfg(test)]
fn take_restore_plan_for_snapshot(
    session: &PaneAgentSessionSnapshot,
    resume_agents_on_restore: bool,
    resumed_agent_sessions: &mut HashSet<String>,
) -> Option<crate::agent_resume::AgentResumePlan> {
    restore_plan_for_snapshot(session, resume_agents_on_restore)
        .filter(|plan| resumed_agent_sessions.insert(plan.dedupe_key.clone()))
}

#[cfg(test)]
pub(super) fn prune_restored_node(node: Node, surviving: &HashSet<PaneId>) -> Option<Node> {
    match node {
        Node::Pane(id) => surviving.contains(&id).then_some(Node::Pane(id)),
        Node::Split {
            direction,
            ratio,
            first,
            second,
        } => {
            let first = prune_restored_node(*first, surviving);
            let second = prune_restored_node(*second, surviving);
            match (first, second) {
                (Some(first), Some(second)) => Some(Node::Split {
                    direction,
                    ratio,
                    first: Box::new(first),
                    second: Box::new(second),
                }),
                (Some(remaining), None) | (None, Some(remaining)) => Some(remaining),
                (None, None) => None,
            }
        }
    }
}

pub(super) fn resolve_restored_pane(
    saved_old_id: Option<u32>,
    id_map: &HashMap<u32, PaneId>,
    surviving: &HashSet<PaneId>,
    pane_ids: &[PaneId],
) -> Option<PaneId> {
    saved_old_id
        .and_then(|old_id| id_map.get(&old_id).copied())
        .filter(|pane_id| surviving.contains(pane_id))
        .or_else(|| pane_ids.first().copied())
}

fn restore_typed_node_remapped(snap: &TabSnapshot) -> (TypedNode, HashMap<u32, PaneId>) {
    let mut old_ids: HashSet<u32> = snap.panes.keys().copied().collect();
    fn collect(node: &LayoutSnapshot, ids: &mut HashSet<u32>) {
        match node {
            LayoutSnapshot::Pane(id) => {
                ids.insert(*id);
            }
            LayoutSnapshot::Collection(_) => {}
            LayoutSnapshot::Split { first, second, .. } => {
                collect(first, ids);
                collect(second, ids);
            }
        }
    }
    collect(&snap.layout, &mut old_ids);
    let mut ordered: Vec<_> = old_ids.into_iter().collect();
    ordered.sort_unstable();
    let id_map: HashMap<_, _> = ordered
        .into_iter()
        .map(|old| (old, PaneId::alloc()))
        .collect();
    fn remap(node: &LayoutSnapshot, ids: &HashMap<u32, PaneId>) -> TypedNode {
        match node {
            LayoutSnapshot::Pane(old) => TypedNode::Leaf(LayoutLeaf::Pane(ids[old])),
            LayoutSnapshot::Collection(id) => TypedNode::Leaf(LayoutLeaf::Collection((*id).into())),
            LayoutSnapshot::Split {
                direction,
                ratio,
                first,
                second,
            } => TypedNode::Split {
                direction: match direction {
                    DirectionSnapshot::Horizontal => Direction::Horizontal,
                    DirectionSnapshot::Vertical => Direction::Vertical,
                },
                ratio: *ratio,
                first: Box::new(remap(first, ids)),
                second: Box::new(remap(second, ids)),
            },
        }
    }
    (remap(&snap.layout, &id_map), id_map)
}

fn recover_typed_layout(
    snap: &TabSnapshot,
    node: TypedNode,
    id_map: &HashMap<u32, PaneId>,
    surviving: &HashSet<PaneId>,
    restored_collection_ids: &mut HashSet<TerminalContainerId>,
) -> Option<RecoveredTabLayout> {
    fn count_collection_leaves(node: &TypedNode, counts: &mut HashMap<TerminalContainerId, usize>) {
        match node {
            TypedNode::Leaf(LayoutLeaf::Pane(_)) => {}
            TypedNode::Leaf(LayoutLeaf::Collection(id)) => {
                *counts.entry(*id).or_default() += 1;
            }
            TypedNode::Split { first, second, .. } => {
                count_collection_leaves(first, counts);
                count_collection_leaves(second, counts);
            }
        }
    }
    let mut leaf_counts = HashMap::new();
    count_collection_leaves(&node, &mut leaf_counts);
    let valid_leaf_ids: HashSet<_> = leaf_counts
        .into_iter()
        .filter_map(|(id, count)| (count == 1).then_some(id))
        .collect();

    let mut saved_records = snap.collections.iter().collect::<Vec<_>>();
    saved_records.sort_by_key(|record| record.id.raw());
    saved_records.dedup_by_key(|record| record.id);
    let candidates: Vec<_> = saved_records
        .into_iter()
        .filter_map(|record| {
            let id = TerminalContainerId::from(record.id);
            (valid_leaf_ids.contains(&id) && !restored_collection_ids.contains(&id))
                .then_some((id, record))
        })
        .take(crate::admission::MAX_GROUPS_PER_TAB)
        .collect();
    for (id, _) in &candidates {
        restored_collection_ids.insert(*id);
    }
    let records: HashMap<TerminalContainerId, &CollectionSnapshot> =
        candidates.into_iter().collect();
    fn prune(
        node: TypedNode,
        surviving: &HashSet<PaneId>,
        records: &HashMap<TerminalContainerId, &CollectionSnapshot>,
        seen: &mut HashSet<TerminalContainerId>,
    ) -> Option<TypedNode> {
        match node {
            TypedNode::Leaf(LayoutLeaf::Pane(id)) => surviving
                .contains(&id)
                .then_some(TypedNode::Leaf(LayoutLeaf::Pane(id))),
            TypedNode::Leaf(LayoutLeaf::Collection(id)) => (records.contains_key(&id)
                && seen.insert(id))
            .then_some(TypedNode::Leaf(LayoutLeaf::Collection(id))),
            TypedNode::Split {
                direction,
                ratio,
                first,
                second,
            } => match (
                prune(*first, surviving, records, seen),
                prune(*second, surviving, records, seen),
            ) {
                (Some(first), Some(second)) => Some(TypedNode::Split {
                    direction,
                    ratio,
                    first: Box::new(first),
                    second: Box::new(second),
                }),
                (Some(node), None) | (None, Some(node)) => Some(node),
                (None, None) => None,
            },
        }
    }
    let mut collection_ids = HashSet::new();
    let mut root = prune(node, surviving, &records, &mut collection_ids);
    let mut placed = HashSet::new();
    fn collect_tiled(node: &TypedNode, placed: &mut HashSet<PaneId>) {
        match node {
            TypedNode::Leaf(LayoutLeaf::Pane(id)) => {
                placed.insert(*id);
            }
            TypedNode::Leaf(LayoutLeaf::Collection(_)) => {}
            TypedNode::Split { first, second, .. } => {
                collect_tiled(first, placed);
                collect_tiled(second, placed);
            }
        }
    }
    if let Some(node) = root.as_ref() {
        collect_tiled(node, &mut placed);
    }
    let mut collections = Vec::new();
    let mut legacy_collection_snapshots = HashMap::new();
    let mut ids: Vec<_> = collection_ids.into_iter().collect();
    ids.sort_by_key(|id| id.raw());
    for id in ids {
        let saved = records[&id];
        let mut members_seen = HashSet::new();
        let members: Vec<_> = saved
            .members
            .iter()
            .filter_map(|old| id_map.get(old).copied())
            .filter(|pane| {
                surviving.contains(pane) && !placed.contains(pane) && members_seen.insert(*pane)
            })
            .take(crate::admission::MAX_GROUP_MEMBERS)
            .collect();
        placed.extend(members.iter().copied());
        let selected = saved
            .selected
            .and_then(|old| id_map.get(&old).copied())
            .filter(|pane| members.contains(pane))
            .or_else(|| members.first().copied());
        let archived = saved
            .archived
            .iter()
            .filter_map(|old| id_map.get(old).copied())
            .filter(|pane| members.contains(pane))
            .collect();
        if let Some(collection) = TerminalContainer::from_legacy(id, members, selected) {
            legacy_collection_snapshots.insert(
                id,
                LegacyCollectionSnapshot::from_legacy(saved.label.clone(), archived),
            );
            collections.push(collection);
        }
    }
    let mut unplaced: Vec<_> = surviving
        .iter()
        .copied()
        .filter(|pane| !placed.contains(pane))
        .collect();
    unplaced.sort_by_key(|pane| pane.raw());
    for pane in unplaced {
        let leaf = TypedNode::Leaf(LayoutLeaf::Pane(pane));
        root = Some(match root {
            Some(existing) => TypedNode::Split {
                direction: Direction::Horizontal,
                ratio: 0.5,
                first: Box::new(existing),
                second: Box::new(leaf),
            },
            None => leaf,
        });
    }
    let root = root?;
    let saved_focus = snap
        .focused_leaf
        .and_then(|leaf| match leaf {
            LayoutLeafSnapshot::Pane(old) => id_map.get(&old).copied().map(LayoutLeaf::Pane),
            LayoutLeafSnapshot::Collection(id) => Some(LayoutLeaf::Collection(id.into())),
        })
        .or_else(|| {
            snap.focused
                .and_then(|old| id_map.get(&old).copied())
                .map(LayoutLeaf::Pane)
        });
    let mut leaves = Vec::new();
    fn collect_leaves(node: &TypedNode, leaves: &mut Vec<LayoutLeaf>) {
        match node {
            TypedNode::Leaf(leaf) => leaves.push(*leaf),
            TypedNode::Split { first, second, .. } => {
                collect_leaves(first, leaves);
                collect_leaves(second, leaves);
            }
        }
    }
    collect_leaves(&root, &mut leaves);
    let focus = saved_focus
        .filter(|focus| leaves.contains(focus))
        .or_else(|| leaves.first().copied())?;
    let layout = TileLayout::from_typed_saved(root, focus, collections);
    let exact_survivors = |layout: &TileLayout| {
        let placed = layout.pane_ids();
        let unique: HashSet<_> = placed.iter().copied().collect();
        placed.len() == unique.len() && unique == *surviving
    };
    if let Some(layout) = layout.filter(exact_survivors) {
        return Some(RecoveredTabLayout {
            layout,
            legacy_collection_snapshots,
        });
    }

    // Semantic recovery must never discard an already restored runtime. If a
    // repaired container projection is still inconsistent, dissolve all
    // containers and place every survivor exactly once in raw-ID order.
    let mut panes: Vec<_> = surviving.iter().copied().collect();
    panes.sort_by_key(|pane| pane.raw());
    let first = panes.first().copied()?;
    let root =
        panes
            .into_iter()
            .skip(1)
            .fold(TypedNode::Leaf(LayoutLeaf::Pane(first)), |root, pane| {
                TypedNode::Split {
                    direction: Direction::Horizontal,
                    ratio: 0.5,
                    first: Box::new(root),
                    second: Box::new(TypedNode::Leaf(LayoutLeaf::Pane(pane))),
                }
            });
    let layout = TileLayout::from_typed_saved(root, LayoutLeaf::Pane(first), Vec::new())?;
    exact_survivors(&layout).then_some(RecoveredTabLayout {
        layout,
        legacy_collection_snapshots: HashMap::new(),
    })
}

/// Restore a layout tree, remapping every pane ID to a fresh globally unique one.
/// Returns the new tree and a map of old_raw_id → new PaneId.
#[cfg(test)]
pub(super) fn restore_node_remapped(snap: &LayoutSnapshot) -> (Node, HashMap<u32, PaneId>) {
    let mut id_map = HashMap::new();
    let node = remap_inner(snap, &mut id_map);
    (node, id_map)
}

#[cfg(test)]
fn remap_inner(snap: &LayoutSnapshot, id_map: &mut HashMap<u32, PaneId>) -> Node {
    match snap {
        LayoutSnapshot::Pane(old_id) => {
            let new_id = PaneId::alloc();
            id_map.insert(*old_id, new_id);
            Node::Pane(new_id)
        }
        LayoutSnapshot::Collection(_) => Node::Pane(PaneId::alloc()),
        LayoutSnapshot::Split {
            direction,
            ratio,
            first,
            second,
        } => {
            let first_node = remap_inner(first, id_map);
            let second_node = remap_inner(second, id_map);
            let dir = match direction {
                DirectionSnapshot::Horizontal => Direction::Horizontal,
                DirectionSnapshot::Vertical => Direction::Vertical,
            };
            Node::Split {
                direction: dir,
                ratio: *ratio,
                first: Box::new(first_node),
                second: Box::new(second_node),
            }
        }
    }
}

#[cfg(test)]
pub(super) fn collect_pane_ids(node: &Node) -> Vec<PaneId> {
    let mut ids = Vec::new();
    collect_ids_inner(node, &mut ids);
    ids
}

#[cfg(test)]
fn collect_ids_inner(node: &Node, ids: &mut Vec<PaneId>) {
    match node {
        Node::Pane(id) => ids.push(*id),
        Node::Split { first, second, .. } => {
            collect_ids_inner(first, ids);
            collect_ids_inner(second, ids);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_session_path(name: &str) -> String {
        std::env::current_dir()
            .unwrap()
            .join(name)
            .display()
            .to_string()
    }

    #[cfg(windows)]
    fn test_restore_shell() -> &'static str {
        "C:\\Windows\\System32\\whoami.exe"
    }

    #[cfg(not(windows))]
    fn test_restore_shell() -> &'static str {
        "/bin/sh"
    }

    #[test]
    fn capture_and_restore_node_round_trip() {
        let node = Node::Split {
            direction: Direction::Horizontal,
            ratio: 0.5,
            first: Box::new(Node::Pane(PaneId::from_raw(0))),
            second: Box::new(Node::Split {
                direction: Direction::Vertical,
                ratio: 0.3,
                first: Box::new(Node::Pane(PaneId::from_raw(1))),
                second: Box::new(Node::Pane(PaneId::from_raw(2))),
            }),
        };

        let snap = super::super::snapshot::capture_node(&node);
        let (restored, id_map) = restore_node_remapped(&snap);

        assert_eq!(id_map.len(), 3);
        let ids = collect_pane_ids(&restored);
        assert_eq!(ids.len(), 3);
        let unique: std::collections::HashSet<u32> = ids.iter().map(|id| id.raw()).collect();
        assert_eq!(unique.len(), 3);
    }

    #[test]
    fn prune_restored_node_collapses_missing_branch() {
        let keep = PaneId::from_raw(11);
        let missing = PaneId::from_raw(12);
        let node = Node::Split {
            direction: Direction::Horizontal,
            ratio: 0.5,
            first: Box::new(Node::Pane(keep)),
            second: Box::new(Node::Pane(missing)),
        };
        let surviving = std::collections::HashSet::from([keep]);

        let pruned = prune_restored_node(node, &surviving).expect("remaining pane should survive");

        assert!(matches!(pruned, Node::Pane(id) if id == keep));
    }

    #[test]
    fn resolve_restored_pane_prefers_surviving_saved_id_and_falls_back_to_first_remaining() {
        let first = PaneId::from_raw(21);
        let second = PaneId::from_raw(22);
        let id_map = HashMap::from([(0_u32, first), (1_u32, second)]);
        let surviving = std::collections::HashSet::from([first]);
        let pane_ids = vec![first];

        assert_eq!(
            resolve_restored_pane(Some(0), &id_map, &surviving, &pane_ids),
            Some(first)
        );
        assert_eq!(
            resolve_restored_pane(Some(1), &id_map, &surviving, &pane_ids),
            Some(first)
        );
    }

    #[test]
    fn restored_worktree_space_membership_drops_missing_checkout() {
        let missing =
            std::env::temp_dir().join(format!("herdr-missing-worktree-{}", std::process::id()));
        let membership = crate::workspace::WorktreeSpaceMembership {
            key: "repo-key".into(),
            label: "herdr".into(),
            repo_root: missing.join("repo"),
            checkout_path: missing.join("checkout"),
            is_linked_worktree: true,
        };

        assert_eq!(restored_worktree_space_membership(Some(membership)), None);
    }

    #[test]
    fn restore_plan_respects_opt_in_and_allowlist() {
        let pi_session_path = test_session_path("pi-session.jsonl");
        let session = super::super::snapshot::PaneAgentSessionSnapshot {
            source: "vqro:pi".into(),
            agent: "pi".into(),
            kind: crate::agent_resume::AgentSessionRefKind::Path,
            value: pi_session_path.clone(),
        };

        assert!(restore_plan_for_snapshot(&session, false).is_none());
        assert_eq!(
            restore_plan_for_snapshot(&session, true).unwrap().argv,
            vec!["pi", "--session", pi_session_path.as_str()]
        );
        let inherited_session = super::super::snapshot::PaneAgentSessionSnapshot {
            source: "herdr:pi".into(),
            ..session.clone()
        };
        let migrated = restore_plan_for_snapshot(&inherited_session, true)
            .expect("legacy snapshot source should migrate during restore");
        assert_eq!(
            migrated.argv,
            vec!["pi", "--session", pi_session_path.as_str()]
        );
        assert!(migrated.dedupe_key.starts_with("vqro:pi\0"));

        let unsupported_path = super::super::snapshot::PaneAgentSessionSnapshot {
            source: "vqro:claude".into(),
            agent: "claude".into(),
            kind: crate::agent_resume::AgentSessionRefKind::Path,
            value: test_session_path("claude-session"),
        };
        assert!(restore_plan_for_snapshot(&unsupported_path, true).is_none());
    }

    #[test]
    fn restore_plan_selection_suppresses_duplicates() {
        let pi_session_path = test_session_path("pi-session.jsonl");
        let session = super::super::snapshot::PaneAgentSessionSnapshot {
            source: "vqro:pi".into(),
            agent: "pi".into(),
            kind: crate::agent_resume::AgentSessionRefKind::Path,
            value: pi_session_path.clone(),
        };
        let mut resumed = HashSet::new();

        assert!(take_restore_plan_for_snapshot(&session, false, &mut resumed).is_none());
        assert!(resumed.is_empty());

        let first = take_restore_plan_for_snapshot(&session, true, &mut resumed)
            .expect("first restore should get a plan");
        assert_eq!(
            first.argv,
            vec!["pi", "--session", pi_session_path.as_str()]
        );
        assert!(take_restore_plan_for_snapshot(&session, true, &mut resumed).is_none());
    }

    #[test]
    fn pane_restore_startup_suppresses_history_for_native_agent_resume() {
        let session = super::super::snapshot::PaneAgentSessionSnapshot {
            source: "vqro:pi".into(),
            agent: "pi".into(),
            kind: crate::agent_resume::AgentSessionRefKind::Path,
            value: test_session_path("pi-session.jsonl"),
        };
        let history = super::super::snapshot::PaneHistorySnapshot {
            ansi: "RESTORED_HISTORY\r\n".into(),
            lines: 1,
        };
        let mut resumed = HashSet::new();
        let mut agent_restore = AgentRestoreState {
            enabled: true,
            resumed_sessions: &mut resumed,
        };

        let startup = pane_restore_startup(Some(&session), Some(&history), &mut agent_restore);

        assert!(startup.restore_plan.is_some());
        assert!(startup.initial_history_ansi.is_none());
        assert!(!startup.duplicate_agent_session);
    }

    #[test]
    fn pane_restore_startup_suppresses_history_for_duplicate_native_agent_session() {
        let session = super::super::snapshot::PaneAgentSessionSnapshot {
            source: "vqro:pi".into(),
            agent: "pi".into(),
            kind: crate::agent_resume::AgentSessionRefKind::Path,
            value: test_session_path("pi-session.jsonl"),
        };
        let history = super::super::snapshot::PaneHistorySnapshot {
            ansi: "RESTORED_HISTORY\r\n".into(),
            lines: 1,
        };
        let mut resumed = HashSet::new();
        let mut agent_restore = AgentRestoreState {
            enabled: true,
            resumed_sessions: &mut resumed,
        };

        let first = pane_restore_startup(Some(&session), Some(&history), &mut agent_restore);
        let duplicate = pane_restore_startup(Some(&session), Some(&history), &mut agent_restore);

        assert!(first.restore_plan.is_some());
        assert!(first.initial_history_ansi.is_none());
        assert!(duplicate.restore_plan.is_none());
        assert!(duplicate.initial_history_ansi.is_none());
        assert!(duplicate.duplicate_agent_session);
    }

    #[test]
    fn restore_admission_is_pty_first_and_defers_semantic_overflow_for_repair() {
        use super::super::snapshot::{
            CollectionSnapshot, LayoutSnapshot, PaneSnapshot, TabSnapshot, WorkspaceSnapshot,
        };

        let collection = CollectionId::from_raw(6_000_001).unwrap();
        let collections = (0..=crate::admission::MAX_GROUPS_PER_TAB)
            .map(|offset| CollectionSnapshot {
                id: CollectionId::from_raw(collection.raw() + offset as u64).unwrap(),
                label: None,
                members: vec![1; crate::admission::MAX_GROUP_MEMBERS + 1],
                selected: Some(1),
                archived: Vec::new(),
            })
            .collect();
        let mut snapshot = SessionSnapshot {
            version: super::super::snapshot::SNAPSHOT_VERSION,
            workspaces: vec![WorkspaceSnapshot {
                id: Some("pty-first".into()),
                custom_name: None,
                identity_cwd: std::path::PathBuf::from("/tmp"),
                checkout: None,
                worktree_space: None,
                public_pane_numbers: HashMap::new(),
                next_public_pane_number: 2,
                public_tab_numbers: vec![1],
                next_public_tab_number: 2,
                tabs: vec![TabSnapshot {
                    id: None,
                    custom_name: None,
                    layout: LayoutSnapshot::Collection(collection),
                    collections,
                    panes: HashMap::from([(
                        1,
                        PaneSnapshot {
                            terminal_id: None,
                            cwd: std::path::PathBuf::from("/tmp"),
                            label: None,
                            agent_name: None,
                            managed_agent_kind: None,
                            agent_session: None,
                            launch_argv: None,
                        },
                    )]),
                    zoomed: false,
                    focused: Some(1),
                    focused_leaf: Some(LayoutLeafSnapshot::Collection(collection)),
                    root_pane: Some(1),
                }],
                active_tab: 0,
            }],
            repositories: Vec::new(),
            space_order: Vec::new(),
            active: Some(0),
            selected: 0,
            sidebar_width: None,
            sidebar_section_split: None,
            collapsed_space_keys: HashSet::new(),
            delegations: Vec::new(),
            collections_authority: Default::default(),
            collections_action_sequence: 0,
        };

        super::super::snapshot::normalize_terminal_identities(&mut snapshot, false)
            .expect("identity normalization");
        validate_snapshot_admission(&snapshot).expect("semantic overflow is repaired after PTYs");
    }

    #[test]
    fn legacy_collection_restore_repairs_duplicate_missing_and_over_limit_members_without_losing_panes(
    ) {
        use super::super::snapshot::{CollectionSnapshot, LayoutSnapshot, TabSnapshot};

        let collection = CollectionId::from_raw(7_000_001).unwrap();
        let first = PaneId::from_raw(101);
        let second = PaneId::from_raw(102);
        let unplaced = PaneId::from_raw(103);
        let id_map = HashMap::from([(1, first), (2, second), (3, unplaced)]);
        let surviving = HashSet::from([first, second, unplaced]);
        let mut members = Vec::new();
        for _ in 0..=crate::admission::MAX_GROUP_MEMBERS {
            members.extend([1, 2, 999]);
        }
        let mut collections = vec![CollectionSnapshot {
            id: collection,
            label: Some("legacy".into()),
            members,
            selected: Some(999),
            archived: vec![2, 999],
        }];
        for offset in 1..=crate::admission::MAX_GROUPS_PER_TAB {
            collections.push(CollectionSnapshot {
                id: CollectionId::from_raw(collection.raw() + offset as u64).unwrap(),
                label: None,
                members: vec![3],
                selected: Some(3),
                archived: Vec::new(),
            });
        }
        let snap = TabSnapshot {
            id: None,
            custom_name: None,
            layout: LayoutSnapshot::Collection(collection),
            collections,
            panes: HashMap::new(),
            zoomed: false,
            focused: None,
            focused_leaf: Some(LayoutLeafSnapshot::Collection(collection)),
            root_pane: Some(1),
        };
        let typed = TypedNode::Leaf(LayoutLeaf::Collection(collection.into()));

        let mut restored_ids = HashSet::new();
        let recovered =
            recover_typed_layout(&snap, typed, &id_map, &surviving, &mut restored_ids).unwrap();
        let container_id = TerminalContainerId::from(collection);
        let restored = recovered
            .layout
            .collection(container_id)
            .expect("valid members retain the legacy container");
        assert_eq!(restored.members(), &[first, second]);
        assert_eq!(restored.selected(), Some(first));
        let policy = &recovered.legacy_collection_snapshots[&container_id];
        assert_eq!(policy.label.as_deref(), Some("legacy"));
        assert_eq!(policy.archived(), &HashSet::from([second]));
        assert_eq!(
            recovered.layout.placement(unplaced),
            Some(crate::layout::PanePlacement::Tiled)
        );
        for pane in [first, second, unplaced] {
            assert_eq!(
                recovered
                    .layout
                    .pane_ids()
                    .iter()
                    .filter(|candidate| **candidate == pane)
                    .count(),
                1
            );
        }
    }

    #[test]
    fn orphan_record_does_not_claim_session_global_container_id() {
        use super::super::snapshot::{CollectionSnapshot, LayoutSnapshot, TabSnapshot};

        let id = CollectionId::from_raw(7_050_001).unwrap();
        let pane = PaneId::from_raw(151);
        let record = CollectionSnapshot {
            id,
            label: None,
            members: vec![1],
            selected: Some(1),
            archived: Vec::new(),
        };
        let snapshot = |collections| TabSnapshot {
            id: None,
            custom_name: None,
            layout: LayoutSnapshot::Collection(id),
            collections,
            panes: HashMap::new(),
            zoomed: false,
            focused: Some(1),
            focused_leaf: Some(LayoutLeafSnapshot::Collection(id)),
            root_pane: Some(1),
        };
        let id_map = HashMap::from([(1, pane)]);
        let surviving = HashSet::from([pane]);
        let mut restored_ids = HashSet::new();

        recover_typed_layout(
            &snapshot(vec![record.clone()]),
            TypedNode::Leaf(LayoutLeaf::Pane(pane)),
            &id_map,
            &surviving,
            &mut restored_ids,
        )
        .unwrap();
        let recovered = recover_typed_layout(
            &snapshot(vec![record]),
            TypedNode::Leaf(LayoutLeaf::Collection(id.into())),
            &id_map,
            &surviving,
            &mut restored_ids,
        )
        .unwrap();

        assert!(recovered.layout.collection(id.into()).is_some());
    }

    #[test]
    fn over_limit_orphan_records_do_not_consume_container_capacity() {
        use super::super::snapshot::{CollectionSnapshot, LayoutSnapshot, TabSnapshot};

        let valid_id = CollectionId::from_raw(7_060_000).unwrap();
        let pane = PaneId::from_raw(161);
        let mut collections = (1..=crate::admission::MAX_GROUPS_PER_TAB)
            .map(|raw| CollectionSnapshot {
                id: CollectionId::from_raw(raw as u64).unwrap(),
                label: None,
                members: vec![1],
                selected: Some(1),
                archived: Vec::new(),
            })
            .collect::<Vec<_>>();
        collections.push(CollectionSnapshot {
            id: valid_id,
            label: None,
            members: vec![1],
            selected: Some(1),
            archived: Vec::new(),
        });
        let snap = TabSnapshot {
            id: None,
            custom_name: None,
            layout: LayoutSnapshot::Collection(valid_id),
            collections,
            panes: HashMap::new(),
            zoomed: false,
            focused: Some(1),
            focused_leaf: Some(LayoutLeafSnapshot::Collection(valid_id)),
            root_pane: Some(1),
        };

        let recovered = recover_typed_layout(
            &snap,
            TypedNode::Leaf(LayoutLeaf::Collection(valid_id.into())),
            &HashMap::from([(1, pane)]),
            &HashSet::from([pane]),
            &mut HashSet::new(),
        )
        .unwrap();

        assert!(recovered.layout.collection(valid_id.into()).is_some());
    }

    #[test]
    fn session_global_duplicate_collection_ids_are_dissolved_after_first_placement() {
        use super::super::snapshot::{CollectionSnapshot, LayoutSnapshot, TabSnapshot};

        let id = CollectionId::from_raw(7_100_001).unwrap();
        let make_tab = |old_pane| TabSnapshot {
            id: None,
            custom_name: None,
            layout: LayoutSnapshot::Collection(id),
            collections: vec![CollectionSnapshot {
                id,
                label: Some("duplicate".into()),
                members: vec![old_pane],
                selected: Some(old_pane),
                archived: Vec::new(),
            }],
            panes: HashMap::new(),
            zoomed: false,
            focused: Some(old_pane),
            focused_leaf: Some(LayoutLeafSnapshot::Collection(id)),
            root_pane: Some(old_pane),
        };
        let mut restored_ids = HashSet::new();
        let first_pane = PaneId::from_raw(201);
        let first = recover_typed_layout(
            &make_tab(1),
            TypedNode::Leaf(LayoutLeaf::Collection(id.into())),
            &HashMap::from([(1, first_pane)]),
            &HashSet::from([first_pane]),
            &mut restored_ids,
        )
        .unwrap();
        assert!(first.layout.collection(id.into()).is_some());

        let second_pane = PaneId::from_raw(202);
        let second = recover_typed_layout(
            &make_tab(2),
            TypedNode::Leaf(LayoutLeaf::Collection(id.into())),
            &HashMap::from([(2, second_pane)]),
            &HashSet::from([second_pane]),
            &mut restored_ids,
        )
        .unwrap();
        assert!(second.layout.collection(id.into()).is_none());
        assert_eq!(second.layout.pane_ids(), vec![second_pane]);
        assert_eq!(
            second.layout.placement(second_pane),
            Some(crate::layout::PanePlacement::Tiled)
        );
    }

    #[test]
    fn every_restored_runtime_survivor_is_placed_exactly_once_after_semantic_repair() {
        use super::super::snapshot::{CollectionSnapshot, LayoutSnapshot, TabSnapshot};

        let id = CollectionId::from_raw(7_200_001).unwrap();
        let panes = [
            PaneId::from_raw(301),
            PaneId::from_raw(302),
            PaneId::from_raw(303),
        ];
        let snap = TabSnapshot {
            id: None,
            custom_name: None,
            layout: LayoutSnapshot::Collection(id),
            collections: vec![CollectionSnapshot {
                id,
                label: None,
                members: vec![1, 1, 999],
                selected: Some(999),
                archived: vec![999],
            }],
            panes: HashMap::new(),
            zoomed: false,
            focused: None,
            focused_leaf: Some(LayoutLeafSnapshot::Collection(id)),
            root_pane: Some(1),
        };
        let recovered = recover_typed_layout(
            &snap,
            TypedNode::Leaf(LayoutLeaf::Collection(id.into())),
            &HashMap::from([(1, panes[0]), (2, panes[1]), (3, panes[2])]),
            &HashSet::from(panes),
            &mut HashSet::new(),
        )
        .unwrap();
        let placed = recovered.layout.pane_ids();
        assert_eq!(placed.len(), panes.len());
        assert_eq!(
            placed.iter().copied().collect::<HashSet<_>>(),
            HashSet::from(panes)
        );
    }

    #[test]
    fn oversized_snapshot_is_rejected_before_restore() {
        use super::super::snapshot::{PaneSnapshot, TabSnapshot, WorkspaceSnapshot};

        let panes = HashMap::from([(
            1,
            PaneSnapshot {
                terminal_id: None,
                cwd: std::path::PathBuf::from("/tmp"),
                label: None,
                agent_name: None,
                managed_agent_kind: None,
                agent_session: None,
                launch_argv: None,
            },
        )]);
        let mut layout = super::super::snapshot::LayoutSnapshot::Pane(1);
        for id in 2..=crate::admission::MAX_TAB_PANES as u32 + 1 {
            layout = super::super::snapshot::LayoutSnapshot::Split {
                direction: super::super::snapshot::DirectionSnapshot::Horizontal,
                ratio: 0.5,
                first: Box::new(layout),
                second: Box::new(super::super::snapshot::LayoutSnapshot::Pane(id)),
            };
        }
        let snapshot = SessionSnapshot {
            version: super::super::snapshot::SNAPSHOT_VERSION,
            workspaces: vec![WorkspaceSnapshot {
                id: Some("oversized".into()),
                custom_name: None,
                identity_cwd: std::path::PathBuf::from("/tmp"),
                checkout: None,
                worktree_space: None,
                public_pane_numbers: HashMap::new(),
                next_public_pane_number: 1,
                public_tab_numbers: vec![1],
                next_public_tab_number: 2,
                tabs: vec![TabSnapshot {
                    id: None,
                    custom_name: None,
                    layout,
                    collections: Vec::new(),
                    panes,
                    zoomed: false,
                    focused: Some(1),
                    focused_leaf: None,
                    root_pane: Some(1),
                }],
                active_tab: 0,
            }],
            repositories: Vec::new(),
            space_order: Vec::new(),
            active: Some(0),
            selected: 0,
            sidebar_width: None,
            sidebar_section_split: None,
            collapsed_space_keys: HashSet::new(),
            delegations: Vec::new(),
            collections_authority: Default::default(),
            collections_action_sequence: 0,
        };
        let (events, _event_rx) = mpsc::channel(4);

        let error = match restore(
            &snapshot,
            None,
            24,
            80,
            0,
            test_restore_shell(),
            crate::config::ShellModeConfig::NonLogin,
            false,
            events,
            Arc::new(Notify::new()),
            Arc::new(RenderSignal::new()),
        ) {
            Err(error) => error,
            Ok(_) => panic!("oversized restore unexpectedly succeeded"),
        };

        assert!(error.contains("pane limit"));
    }

    #[test]
    fn pane_restore_startup_keeps_history_without_native_agent_resume() {
        let session = super::super::snapshot::PaneAgentSessionSnapshot {
            source: "vqro:pi".into(),
            agent: "pi".into(),
            kind: crate::agent_resume::AgentSessionRefKind::Path,
            value: test_session_path("pi-session.jsonl"),
        };
        let history = super::super::snapshot::PaneHistorySnapshot {
            ansi: "RESTORED_HISTORY\r\n".into(),
            lines: 1,
        };
        let mut resumed = HashSet::new();
        let mut agent_restore = AgentRestoreState {
            enabled: false,
            resumed_sessions: &mut resumed,
        };

        let startup = pane_restore_startup(Some(&session), Some(&history), &mut agent_restore);

        assert!(startup.restore_plan.is_none());
        assert_eq!(startup.initial_history_ansi, Some("RESTORED_HISTORY\r\n"));
        assert!(!startup.duplicate_agent_session);
        assert!(resumed.is_empty());
    }

    #[test]
    fn restore_rehydrates_agent_session_metadata() {
        let session = super::super::snapshot::PaneAgentSessionSnapshot {
            source: "herdr:hermes".into(),
            agent: "hermes".into(),
            kind: crate::agent_resume::AgentSessionRefKind::Id,
            value: "hermes-session".into(),
        };

        let preserved = restored_terminal_agent_session(Some(&session), false)
            .expect("restore should preserve metadata");
        assert_eq!(preserved.source, "herdr:hermes");
        assert_eq!(preserved.agent, "hermes");
        assert_eq!(preserved.session_ref.value, "hermes-session");
    }

    #[test]
    fn restore_does_not_rehydrate_duplicate_agent_session_metadata() {
        let session = super::super::snapshot::PaneAgentSessionSnapshot {
            source: "vqro:pi".into(),
            agent: "pi".into(),
            kind: crate::agent_resume::AgentSessionRefKind::Path,
            value: test_session_path("pi-session.jsonl"),
        };
        let mut resumed = HashSet::new();
        assert!(take_restore_plan_for_snapshot(&session, true, &mut resumed).is_some());
        assert!(take_restore_plan_for_snapshot(&session, true, &mut resumed).is_none());

        assert!(restored_terminal_agent_session(Some(&session), true).is_none());
    }

    #[tokio::test]
    async fn restore_carries_persisted_agent_session_metadata() {
        let cwd = std::env::current_dir().unwrap();
        let snapshot = SessionSnapshot {
            delegations: Vec::new(),
            collections_authority: Default::default(),
            collections_action_sequence: 0,
            version: super::super::snapshot::SNAPSHOT_VERSION,
            repositories: Vec::new(),
            space_order: Vec::new(),
            workspaces: vec![WorkspaceSnapshot {
                id: Some("workspace".into()),
                custom_name: None,
                identity_cwd: cwd.clone(),
                checkout: None,
                worktree_space: None,
                public_pane_numbers: HashMap::new(),
                next_public_pane_number: 0,
                public_tab_numbers: Vec::new(),
                next_public_tab_number: 0,
                tabs: vec![TabSnapshot {
                    id: None,
                    collections: Vec::new(),
                    focused_leaf: None,
                    custom_name: None,
                    layout: LayoutSnapshot::Pane(0),
                    panes: HashMap::from([(
                        0,
                        super::super::snapshot::PaneSnapshot {
                            terminal_id: None,
                            cwd,
                            label: Some("reviewer".into()),
                            agent_name: Some("reviewer".into()),
                            managed_agent_kind: Some("opencode".into()),
                            agent_session: Some(super::super::snapshot::PaneAgentSessionSnapshot {
                                source: "herdr:opencode".into(),
                                agent: "opencode".into(),
                                kind: crate::agent_resume::AgentSessionRefKind::Id,
                                value: "opencode-session".into(),
                            }),
                            launch_argv: None,
                        },
                    )]),
                    zoomed: false,
                    focused: Some(0),
                    root_pane: Some(0),
                }],
                active_tab: 0,
            }],
            active: Some(0),
            selected: 0,
            sidebar_width: None,
            sidebar_section_split: None,
            collapsed_space_keys: Default::default(),
        };
        let (events, _event_rx) = mpsc::channel(4);

        let (_workspaces, terminals, _runtimes) = restore(
            &snapshot,
            None,
            24,
            80,
            0,
            test_restore_shell(),
            crate::config::ShellModeConfig::NonLogin,
            false,
            events,
            Arc::new(Notify::new()),
            Arc::new(RenderSignal::new()),
        )
        .unwrap();

        let terminal = terminals
            .values()
            .next()
            .expect("restored terminal should exist");
        assert!(
            !terminal.respawn_shell_on_exit,
            "agent sessions should not use native restore lifecycle when resume_agents_on_restore is disabled"
        );
        assert_eq!(terminal.agent_name, None);
        assert_eq!(terminal.manual_label.as_deref(), Some("reviewer"));
        let session = terminal
            .persisted_agent_session
            .as_ref()
            .expect("persisted agent session should survive restore");
        assert_eq!(session.source, "herdr:opencode");
        assert_eq!(session.agent, "opencode");
        assert_eq!(session.session_ref.value, "opencode-session");
    }

    #[tokio::test]
    async fn restore_preserves_public_id_mapping_and_terminal_identity_after_pane_id_remap() {
        let cwd = std::env::current_dir().unwrap();
        let mut snapshot = SessionSnapshot {
            delegations: Vec::new(),
            collections_authority: Default::default(),
            collections_action_sequence: 0,
            version: super::super::snapshot::SNAPSHOT_VERSION,
            repositories: Vec::new(),
            space_order: Vec::new(),
            workspaces: vec![WorkspaceSnapshot {
                id: Some("w1".into()),
                custom_name: None,
                identity_cwd: cwd.clone(),
                checkout: None,
                worktree_space: None,
                public_pane_numbers: HashMap::from([(10, 1), (20, 3)]),
                next_public_pane_number: 4,
                public_tab_numbers: vec![5],
                next_public_tab_number: 6,
                tabs: vec![TabSnapshot {
                    id: None,
                    collections: Vec::new(),
                    focused_leaf: None,
                    custom_name: None,
                    layout: LayoutSnapshot::Split {
                        direction: super::super::snapshot::DirectionSnapshot::Horizontal,
                        ratio: 0.5,
                        first: Box::new(LayoutSnapshot::Pane(10)),
                        second: Box::new(LayoutSnapshot::Pane(20)),
                    },
                    panes: HashMap::from([
                        (
                            10,
                            super::super::snapshot::PaneSnapshot {
                                terminal_id: None,
                                cwd: cwd.clone(),
                                label: None,
                                agent_name: None,
                                managed_agent_kind: None,
                                agent_session: None,
                                launch_argv: None,
                            },
                        ),
                        (
                            20,
                            super::super::snapshot::PaneSnapshot {
                                terminal_id: None,
                                cwd: cwd.clone(),
                                label: None,
                                agent_name: None,
                                managed_agent_kind: None,
                                agent_session: None,
                                launch_argv: None,
                            },
                        ),
                    ]),
                    zoomed: false,
                    focused: Some(10),
                    root_pane: Some(10),
                }],
                active_tab: 0,
            }],
            active: Some(0),
            selected: 0,
            sidebar_width: None,
            sidebar_section_split: None,
            collapsed_space_keys: Default::default(),
        };
        super::super::snapshot::normalize_terminal_identities(&mut snapshot, false).unwrap();
        let expected_tab_id =
            super::super::snapshot::tab_id_for_snapshot(&snapshot.workspaces[0].tabs[0]).unwrap();
        let expected = snapshot.workspaces[0].tabs[0]
            .panes
            .iter()
            .map(|(&pane, saved)| {
                (
                    pane,
                    super::super::snapshot::terminal_id_for_pane_snapshot(saved).unwrap(),
                )
            })
            .collect::<HashMap<_, _>>();
        let (events, _event_rx) = mpsc::channel(4);

        let (workspaces, _terminals, _runtimes) = restore(
            &snapshot,
            None,
            24,
            80,
            0,
            test_restore_shell(),
            crate::config::ShellModeConfig::NonLogin,
            false,
            events,
            Arc::new(Notify::new()),
            Arc::new(RenderSignal::new()),
        )
        .unwrap();

        let workspace = workspaces.first().expect("workspace should restore");
        let mut public_numbers: Vec<_> = workspace.public_pane_numbers.values().copied().collect();
        public_numbers.sort_unstable();
        assert_eq!(public_numbers, vec![1, 3]);
        assert_eq!(workspace.next_public_pane_number, 4);
        assert_eq!(workspace.tabs[0].id, expected_tab_id);
        assert_eq!(workspace.tabs[0].number, 5);
        assert_eq!(workspace.next_public_tab_number, 6);
        for (old_pane, public_number) in [(10, 1), (20, 3)] {
            let restored_pane = workspace
                .public_pane_numbers
                .iter()
                .find_map(|(pane, number)| (*number == public_number).then_some(*pane))
                .unwrap();
            assert_eq!(
                workspace.terminal_id(restored_pane),
                Some(&expected[&old_pane])
            );
        }
    }

    #[tokio::test]
    async fn cold_restore_uses_persisted_repaired_public_tab_numbers() {
        let cwd = std::env::current_dir().unwrap();
        let pane_snap = |id: &str| {
            (
                id.parse::<u32>().unwrap(),
                super::super::snapshot::PaneSnapshot {
                    terminal_id: None,
                    cwd: cwd.clone(),
                    label: None,
                    agent_name: None,
                    managed_agent_kind: None,
                    agent_session: None,
                    launch_argv: None,
                },
            )
        };
        let final_pane = super::super::snapshot::PaneSnapshot {
            terminal_id: None,
            cwd: cwd.clone(),
            label: Some("planner".into()),
            agent_name: Some("planner".into()),
            managed_agent_kind: None,
            agent_session: Some(super::super::snapshot::PaneAgentSessionSnapshot {
                source: "vqro:codex".into(),
                agent: "codex".into(),
                kind: crate::agent_resume::AgentSessionRefKind::Id,
                value: "codex-session".into(),
            }),
            launch_argv: None,
        };
        let mut snapshot = SessionSnapshot {
            delegations: Vec::new(),
            collections_authority: Default::default(),
            collections_action_sequence: 0,
            version: super::super::snapshot::SNAPSHOT_VERSION,
            repositories: Vec::new(),
            space_order: Vec::new(),
            workspaces: vec![WorkspaceSnapshot {
                id: Some("w1".into()),
                custom_name: None,
                identity_cwd: cwd.clone(),
                checkout: None,
                worktree_space: None,
                public_pane_numbers: HashMap::from([(10, 1), (11, 2), (12, 3), (13, 4)]),
                next_public_pane_number: 5,
                public_tab_numbers: vec![7, 0, 7],
                next_public_tab_number: 2,
                tabs: vec![
                    TabSnapshot {
                        id: None,
                        collections: Vec::new(),
                        focused_leaf: None,
                        custom_name: None,
                        layout: LayoutSnapshot::Pane(10),
                        panes: HashMap::from([pane_snap("10")]),
                        zoomed: false,
                        focused: Some(10),
                        root_pane: Some(10),
                    },
                    TabSnapshot {
                        id: None,
                        collections: Vec::new(),
                        focused_leaf: None,
                        custom_name: None,
                        layout: LayoutSnapshot::Pane(11),
                        panes: HashMap::from([pane_snap("11")]),
                        zoomed: false,
                        focused: Some(11),
                        root_pane: Some(11),
                    },
                    TabSnapshot {
                        id: None,
                        collections: Vec::new(),
                        focused_leaf: None,
                        custom_name: None,
                        layout: LayoutSnapshot::Pane(12),
                        panes: HashMap::from([pane_snap("12")]),
                        zoomed: false,
                        focused: Some(12),
                        root_pane: Some(12),
                    },
                    TabSnapshot {
                        id: None,
                        collections: Vec::new(),
                        focused_leaf: None,
                        custom_name: None,
                        layout: LayoutSnapshot::Pane(13),
                        panes: HashMap::from([(13, final_pane)]),
                        zoomed: false,
                        focused: Some(13),
                        root_pane: Some(13),
                    },
                ],
                active_tab: 3,
            }],
            active: Some(0),
            selected: 0,
            sidebar_width: None,
            sidebar_section_split: None,
            collapsed_space_keys: Default::default(),
        };
        super::super::snapshot::normalize_terminal_identities(&mut snapshot, false).unwrap();
        let persisted_numbers = snapshot.workspaces[0].public_tab_numbers.clone();
        let persisted_next = snapshot.workspaces[0].next_public_tab_number;
        assert_eq!(persisted_numbers, [7, 1, 2, 3]);
        assert_eq!(persisted_next, 8);
        let (events, _event_rx) = mpsc::channel(4);

        let (workspaces, terminals, _runtimes) = restore(
            &snapshot,
            None,
            24,
            80,
            0,
            test_restore_shell(),
            crate::config::ShellModeConfig::NonLogin,
            false,
            events,
            Arc::new(Notify::new()),
            Arc::new(RenderSignal::new()),
        )
        .unwrap();

        let workspace = workspaces.first().expect("workspace should restore");
        assert_eq!(workspace.active_tab, 3);
        assert_eq!(
            workspace
                .tabs
                .iter()
                .map(|tab| tab.number)
                .collect::<Vec<_>>(),
            persisted_numbers
        );
        assert_eq!(workspace.next_public_tab_number, persisted_next);
        assert_eq!(workspace.tabs[3].number, 3);
        let agent_pane = workspace.tabs[3].root_pane;
        let terminal_id = &workspace.tabs[3].panes[&agent_pane].attached_terminal_id;
        assert!(terminals[terminal_id].agent_name.is_none());
        assert_eq!(terminals[terminal_id].managed_agent_kind(), None);
        assert!(workspace
            .pane_details(&terminals)
            .into_iter()
            .all(|detail| detail.pane_id != agent_pane));
    }

    #[test]
    fn legacy_restore_precomputes_missing_public_pane_numbers() {
        let cwd = std::env::current_dir().unwrap();
        let snapshot = WorkspaceSnapshot {
            id: Some("w1".into()),
            custom_name: None,
            identity_cwd: cwd,
            checkout: None,
            worktree_space: None,
            public_pane_numbers: HashMap::new(),
            next_public_pane_number: 0,
            public_tab_numbers: Vec::new(),
            next_public_tab_number: 0,
            tabs: vec![TabSnapshot {
                id: None,
                collections: Vec::new(),
                focused_leaf: None,
                custom_name: None,
                layout: LayoutSnapshot::Split {
                    direction: super::super::snapshot::DirectionSnapshot::Horizontal,
                    ratio: 0.5,
                    first: Box::new(LayoutSnapshot::Pane(10)),
                    second: Box::new(LayoutSnapshot::Pane(20)),
                },
                panes: HashMap::new(),
                zoomed: false,
                focused: Some(10),
                root_pane: Some(10),
            }],
            active_tab: 0,
        };
        let mut next_public_pane_number = 1;

        let public_numbers =
            migrated_public_pane_numbers_by_old_raw(&snapshot, &mut next_public_pane_number);

        assert_eq!(public_numbers, HashMap::from([(10, 1), (20, 2)]));
        assert_eq!(next_public_pane_number, 3);
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn native_agent_restore_defers_runtime_launch() {
        let cwd = std::env::current_dir().unwrap();
        let snapshot = SessionSnapshot {
            delegations: Vec::new(),
            collections_authority: Default::default(),
            collections_action_sequence: 0,
            version: super::super::snapshot::SNAPSHOT_VERSION,
            repositories: Vec::new(),
            space_order: Vec::new(),
            workspaces: vec![WorkspaceSnapshot {
                id: Some("workspace".into()),
                custom_name: None,
                identity_cwd: cwd.clone(),
                checkout: None,
                worktree_space: None,
                public_pane_numbers: HashMap::new(),
                next_public_pane_number: 0,
                public_tab_numbers: Vec::new(),
                next_public_tab_number: 0,
                tabs: vec![TabSnapshot {
                    id: None,
                    collections: Vec::new(),
                    focused_leaf: None,
                    custom_name: None,
                    layout: LayoutSnapshot::Pane(0),
                    panes: HashMap::from([(
                        0,
                        super::super::snapshot::PaneSnapshot {
                            terminal_id: None,
                            cwd,
                            label: None,
                            agent_name: None,
                            managed_agent_kind: None,
                            agent_session: Some(super::super::snapshot::PaneAgentSessionSnapshot {
                                source: "vqro:codex".into(),
                                agent: "codex".into(),
                                kind: crate::agent_resume::AgentSessionRefKind::Id,
                                value: "codex-session".into(),
                            }),
                            launch_argv: None,
                        },
                    )]),
                    zoomed: false,
                    focused: Some(0),
                    root_pane: Some(0),
                }],
                active_tab: 0,
            }],
            active: Some(0),
            selected: 0,
            sidebar_width: None,
            sidebar_section_split: None,
            collapsed_space_keys: Default::default(),
        };
        let (events, _event_rx) = mpsc::channel(4);

        let (_workspaces, terminals, runtimes) = restore(
            &snapshot,
            None,
            24,
            80,
            0,
            test_restore_shell(),
            crate::config::ShellModeConfig::NonLogin,
            true,
            events,
            Arc::new(Notify::new()),
            Arc::new(RenderSignal::new()),
        )
        .unwrap();

        let terminal = terminals
            .values()
            .next()
            .expect("native agent restore should create terminal state");
        assert!(
            terminal.pending_agent_resume_plan.is_some(),
            "restored native agent panes should defer resume until client terminal context is known"
        );
        assert!(
            !terminal.respawn_shell_on_exit,
            "deferred agent resume should not use native restore lifecycle before launch"
        );
        assert!(
            runtimes.is_empty(),
            "native agent restore should not spawn a fallback-size runtime during snapshot restore"
        );
        let mut imports = HashMap::new();
        let (_handoff_workspaces, handoff_terminals, handoff_runtimes) = restore_handoff(
            &snapshot,
            0,
            test_restore_shell(),
            crate::config::ShellModeConfig::NonLogin,
            &mut imports,
            mpsc::channel(4).0,
            Arc::new(Notify::new()),
            Arc::new(RenderSignal::new()),
        )
        .expect("handoff restore should preserve pending native agent resume");
        let handoff_terminal = handoff_terminals
            .values()
            .next()
            .expect("handoff restore should create terminal state");
        assert!(
            handoff_terminal.pending_agent_resume_plan.is_some(),
            "handoff restore should preserve pending native agent resume intent"
        );
        assert!(
            handoff_runtimes.is_empty(),
            "handoff restore should not replace pending native agent resume with a shell runtime"
        );
    }

    #[tokio::test]
    async fn restore_seeds_saved_pane_history_into_runtime() {
        let (snapshot, history) = snapshot_with_saved_pane_history();
        let (events, _events_rx) = mpsc::channel(8);
        let render_notify = Arc::new(Notify::new());
        let render_dirty = Arc::new(RenderSignal::new());

        let (_workspaces, _terminals, runtimes) = restore(
            &snapshot,
            Some(&history),
            5,
            40,
            4096,
            test_restore_shell(),
            crate::config::ShellModeConfig::NonLogin,
            false,
            events,
            render_notify,
            render_dirty,
        )
        .unwrap();
        let runtime = runtimes
            .values()
            .next()
            .expect("restored runtime should exist");

        let restored_text = runtime.recent_unwrapped_text(10);
        assert!(
            restored_text.contains("RESTORED_HISTORY 👨‍👩‍👧 LINK"),
            "styled Unicode and hyperlink text should survive history replay"
        );

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while runtime.cwd().is_none() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let _ = runtime.try_send_bytes(bytes::Bytes::from_static(b"exit\n"));
    }

    #[tokio::test]
    async fn restore_without_history_snapshot_keeps_pane_contents_empty() {
        let (snapshot, _history) = snapshot_with_saved_pane_history();
        let (events, _events_rx) = mpsc::channel(8);
        let render_notify = Arc::new(Notify::new());
        let render_dirty = Arc::new(RenderSignal::new());

        let (_workspaces, _terminals, runtimes) = restore(
            &snapshot,
            None,
            5,
            40,
            4096,
            test_restore_shell(),
            crate::config::ShellModeConfig::NonLogin,
            false,
            events,
            render_notify,
            render_dirty,
        )
        .unwrap();
        let runtime = runtimes
            .values()
            .next()
            .expect("restored runtime should exist");

        assert!(
            !runtime
                .recent_unwrapped_text(10)
                .contains("RESTORED_HISTORY"),
            "pane history should not restore unless a history snapshot is supplied"
        );

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while runtime.cwd().is_none() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let _ = runtime.try_send_bytes(bytes::Bytes::from_static(b"exit\n"));
    }

    fn snapshot_with_saved_pane_history() -> (SessionSnapshot, SessionHistorySnapshot) {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
        let mut panes = HashMap::new();
        panes.insert(
            0,
            super::super::snapshot::PaneSnapshot {
                terminal_id: None,
                cwd: cwd.clone(),
                label: None,
                agent_name: None,
                managed_agent_kind: None,
                agent_session: None,
                launch_argv: None,
            },
        );
        let history = SessionHistorySnapshot {
            version: super::super::snapshot::SNAPSHOT_VERSION,
            workspaces: vec![WorkspaceHistorySnapshot {
                tabs: vec![super::super::snapshot::TabHistorySnapshot {
                    panes: HashMap::from([(
                        0,
                        super::super::snapshot::PaneHistorySnapshot {
                            ansi: concat!(
                                "\x1b[31mRESTORED_HISTORY 👨‍👩‍👧\x1b[0m ",
                                "\x1b]8;;https://example.com\x1b\\LINK\x1b]8;;\x1b\\\r\n"
                            )
                            .to_string(),
                            lines: 1,
                        },
                    )]),
                }],
            }],
        };
        let snapshot = SessionSnapshot {
            delegations: Vec::new(),
            collections_authority: Default::default(),
            collections_action_sequence: 0,
            version: super::super::snapshot::SNAPSHOT_VERSION,
            repositories: Vec::new(),
            space_order: Vec::new(),
            workspaces: vec![WorkspaceSnapshot {
                id: Some("workspace".into()),
                custom_name: None,
                identity_cwd: cwd,
                checkout: None,
                worktree_space: None,
                public_pane_numbers: HashMap::new(),
                next_public_pane_number: 0,
                public_tab_numbers: Vec::new(),
                next_public_tab_number: 0,
                tabs: vec![TabSnapshot {
                    id: None,
                    collections: Vec::new(),
                    focused_leaf: None,
                    custom_name: None,
                    layout: LayoutSnapshot::Pane(0),
                    panes,
                    zoomed: false,
                    focused: Some(0),
                    root_pane: Some(0),
                }],
                active_tab: 0,
            }],
            active: Some(0),
            selected: 0,
            sidebar_width: Some(26),
            sidebar_section_split: Some(0.5),
            collapsed_space_keys: Default::default(),
        };
        (snapshot, history)
    }
}
