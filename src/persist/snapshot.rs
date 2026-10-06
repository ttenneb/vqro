use std::collections::HashMap;
use std::path::PathBuf;

use ratatui::layout::Direction;
use serde::{Deserialize, Serialize};

#[cfg(test)]
use crate::layout::Node;
use crate::terminal::TerminalRuntimeRegistry;
use crate::workspace::Workspace;

/// Current structural session snapshot format version.
/// Maximum snapshot version this binary can read.
pub(crate) const SNAPSHOT_VERSION: u32 = 8;
/// Admitted input version from pre-tree legacy-authority writers. Never written.
pub(crate) const LEGACY_SNAPSHOT_WRITER_VERSION: u32 = 6;
/// Pane history has no terminal-identity fields and remains wire-compatible at v4.
pub(super) const HISTORY_SNAPSHOT_VERSION: u32 = 4;
pub(super) const MAX_SESSION_SNAPSHOT_BYTES: u64 = 16 * 1024 * 1024;
pub(super) const MAX_HISTORY_SNAPSHOT_BYTES: u64 = 64 * 1024 * 1024;
const MAX_COLLECTIONS_ACTION_SEQUENCE: u64 = (1 << 60) - 1;

/// Serializable snapshot of the entire herdr session.
#[derive(Clone, Serialize, Deserialize)]
pub struct SessionSnapshot {
    /// Format version — used to detect incompatible changes.
    #[serde(default)]
    pub version: u32,
    pub workspaces: Vec<WorkspaceSnapshot>,
    /// Shared repository identities and repository-local checkout ordering.
    #[serde(default)]
    pub repositories: Vec<crate::repository::Repository>,
    /// Shared top-level Spaces ordering. Older snapshots derive this from workspaces.
    #[serde(default)]
    pub space_order: Vec<crate::repository::SpaceRef>,
    pub active: Option<usize>,
    pub selected: usize,
    #[serde(default)]
    pub sidebar_width: Option<u16>,
    #[serde(default)]
    pub sidebar_section_split: Option<f32>,
    #[serde(default)]
    pub collapsed_space_keys: std::collections::HashSet<String>,
    /// Session-wide delegation provenance. Pane IDs refer to pre-restore IDs.
    #[serde(default)]
    pub delegations: Vec<crate::delegation::DelegationRecord>,
    /// Single durable Collections feature-authority owner. Required since version 7.
    #[serde(default)]
    pub collections_authority: crate::collections_extension_contract::CollectionsAuthorityV1,
    /// Durable high-water mark for package action tickets and state idempotency keys.
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub collections_action_sequence: u64,
}

fn is_zero_u64(value: &u64) -> bool {
    *value == 0
}

#[derive(Clone, Serialize, Deserialize)]
pub struct SessionHistorySnapshot {
    /// History remains independently versioned because v5 changes only the
    /// structural session snapshot.
    #[serde(default)]
    pub version: u32,
    pub workspaces: Vec<WorkspaceHistorySnapshot>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct WorkspaceHistorySnapshot {
    pub tabs: Vec<TabHistorySnapshot>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct TabHistorySnapshot {
    pub panes: HashMap<u32, PaneHistorySnapshot>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct WorkspaceSnapshot {
    #[serde(default, deserialize_with = "deserialize_raw_workspace_id")]
    pub id: Option<String>,
    #[serde(default)]
    pub custom_name: Option<String>,
    pub identity_cwd: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkout: Option<crate::repository::CheckoutProvenance>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_space: Option<crate::workspace::WorktreeSpaceMembership>,
    #[serde(default)]
    pub public_pane_numbers: HashMap<u32, usize>,
    #[serde(default)]
    pub next_public_pane_number: usize,
    #[serde(default)]
    pub public_tab_numbers: Vec<usize>,
    #[serde(default)]
    pub next_public_tab_number: usize,
    pub tabs: Vec<TabSnapshot>,
    #[serde(default)]
    pub active_tab: usize,
}

#[derive(Deserialize)]
struct LegacyWorkspaceSnapshot {
    #[serde(default)]
    custom_name: Option<String>,
    layout: LayoutSnapshot,
    panes: HashMap<u32, PaneSnapshot>,
    zoomed: bool,
    #[serde(default)]
    focused: Option<u32>,
    #[serde(default)]
    root_pane: Option<u32>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct TabSnapshot {
    /// Added in snapshot v6. Older and malformed values are normalized before restore.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<RawTabId>,
    #[serde(default)]
    pub custom_name: Option<String>,
    pub layout: LayoutSnapshot,
    #[serde(default)]
    pub collections: Vec<CollectionSnapshot>,
    pub panes: HashMap<u32, PaneSnapshot>,
    pub zoomed: bool,
    #[serde(default)]
    pub focused: Option<u32>,
    #[serde(default)]
    pub focused_leaf: Option<LayoutLeafSnapshot>,
    #[serde(default)]
    pub root_pane: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CollectionSnapshot {
    pub id: crate::layout::CollectionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default)]
    pub archived: Vec<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "id", rename_all = "snake_case")]
pub enum LayoutLeafSnapshot {
    Pane(u32),
    Stack(crate::layout::CollectionId),
}

/// Bounded persistence DTO. Validation into a logical `TerminalId` happens in
/// the deterministic snapshot-normalization pass, never during JSON allocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawTerminalId(Option<String>);

impl RawTerminalId {
    fn validated(&self) -> Option<crate::terminal::TerminalId> {
        self.0
            .as_deref()
            .and_then(|raw| crate::terminal::TerminalId::parse(raw).ok())
    }

    fn from_validated(id: &crate::terminal::TerminalId) -> Self {
        Self(Some(id.as_str().to_owned()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawTabId(Option<String>);

impl RawTabId {
    fn validated(&self) -> Option<crate::workspace::TabId> {
        self.0
            .as_deref()
            .and_then(|raw| crate::workspace::TabId::parse(raw).ok())
    }

    fn from_validated(id: &crate::workspace::TabId) -> Self {
        Self(Some(id.as_str().to_owned()))
    }
}

impl Serialize for RawTabId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.0.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for RawTabId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = serde_json::Value::deserialize(deserializer)?;
        let candidate = match raw {
            serde_json::Value::String(value)
                if value.len() <= crate::workspace::MAX_TAB_ID_BYTES =>
            {
                Some(value)
            }
            _ => None,
        };
        Ok(Self(candidate))
    }
}

const MAX_WORKSPACE_ID_BYTES: usize = 32;

fn deserialize_raw_workspace_id<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = serde_json::Value::deserialize(deserializer)?;
    Ok(match raw {
        serde_json::Value::String(value) if value.len() <= MAX_WORKSPACE_ID_BYTES => Some(value),
        _ => None,
    })
}

fn canonical_workspace_id(raw: &str) -> bool {
    crate::workspace::public_workspace_number(raw).is_some_and(|number| {
        number > 0 && raw == format!("w{}", crate::workspace::encode_public_number(number))
    })
}

impl Serialize for RawTerminalId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.0.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for RawTerminalId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        // The containing session read is bounded. Keep only a bounded string
        // candidate; every other JSON shape is repairable malformed input.
        let raw = serde_json::Value::deserialize(deserializer)?;
        let candidate = match raw {
            serde_json::Value::String(value)
                if value.len() <= crate::terminal::MAX_TERMINAL_ID_BYTES =>
            {
                Some(value)
            }
            _ => None,
        };
        Ok(Self(candidate))
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct PaneSnapshot {
    /// Added in snapshot v5. Older snapshots are normalized before restoration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_id: Option<RawTerminalId>,
    pub cwd: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub managed_agent_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_session: Option<PaneAgentSessionSnapshot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch_argv: Option<Vec<String>>,
}

impl PaneSnapshot {
    pub(crate) fn without_terminal_id(cwd: PathBuf) -> Self {
        Self {
            terminal_id: None,
            cwd,
            label: None,
            agent_name: None,
            managed_agent_kind: None,
            agent_session: None,
            launch_argv: None,
        }
    }

    fn legacy_layout_only() -> Self {
        Self::without_terminal_id(std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneAgentSessionSnapshot {
    pub source: String,
    pub agent: String,
    pub kind: crate::agent_resume::AgentSessionRefKind,
    pub value: String,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct PaneHistorySnapshot {
    pub ansi: String,
    pub lines: usize,
}

/// Canonical durable tree. Membership and selection have no parallel record.
#[derive(Clone, Serialize, Deserialize)]
pub enum LayoutSnapshot {
    Pane(u32),
    Stack {
        id: crate::layout::CollectionId,
        selected: usize,
        children: Vec<LayoutSnapshot>,
    },
    Split {
        direction: DirectionSnapshot,
        ratio: f32,
        first: Box<LayoutSnapshot>,
        second: Box<LayoutSnapshot>,
    },
}

#[derive(Clone, Serialize, Deserialize)]
pub enum DirectionSnapshot {
    Horizontal,
    Vertical,
}

impl From<LegacyWorkspaceSnapshot> for WorkspaceSnapshot {
    fn from(snap: LegacyWorkspaceSnapshot) -> Self {
        let identity_cwd = legacy_identity_cwd(&snap);
        let tab = TabSnapshot {
            id: None,
            custom_name: None,
            layout: snap.layout,
            collections: Vec::new(),
            panes: snap.panes,
            zoomed: snap.zoomed,
            focused: snap.focused,
            focused_leaf: None,
            root_pane: snap.root_pane,
        };

        Self {
            id: None,
            custom_name: snap.custom_name,
            identity_cwd,
            checkout: None,
            worktree_space: None,
            public_pane_numbers: HashMap::new(),
            next_public_pane_number: 0,
            public_tab_numbers: Vec::new(),
            next_public_tab_number: 0,
            tabs: vec![tab],
            active_tab: 0,
        }
    }
}

#[derive(Deserialize)]
struct RawSessionSnapshot {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    workspaces: Vec<serde_json::Value>,
    #[serde(default)]
    repositories: Vec<crate::repository::Repository>,
    #[serde(default)]
    space_order: Vec<crate::repository::SpaceRef>,
    #[serde(default)]
    active: Option<usize>,
    #[serde(default)]
    selected: usize,
    #[serde(default)]
    sidebar_width: Option<u16>,
    #[serde(default)]
    sidebar_section_split: Option<f32>,
    #[serde(default)]
    collapsed_space_keys: std::collections::HashSet<String>,
    #[serde(default)]
    delegations: Vec<crate::delegation::DelegationRecord>,
    #[serde(default)]
    collections_authority: Option<crate::collections_extension_contract::CollectionsAuthorityV1>,
    #[serde(default)]
    collections_action_sequence: u64,
}

fn migrate_snapshot(
    raw: RawSessionSnapshot,
    normalize: bool,
) -> Result<(SessionSnapshot, bool), String> {
    let original_version = raw.version;
    let workspaces = raw
        .workspaces
        .into_iter()
        .enumerate()
        .map(|(index, mut workspace)| {
            if original_version <= 7 {
                super::convert_v7::convert_workspace(&mut workspace)
                    .map_err(|error| format!("workspace {index}: {error}"))?;
            }
            migrate_workspace(workspace)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let (migrated_repositories, migrated_space_order) =
        crate::repository::migrate_workspace_projection(&workspaces);
    let repositories = if raw.repositories.is_empty() {
        migrated_repositories
    } else {
        raw.repositories
    };
    let space_order = if raw.space_order.is_empty() {
        migrated_space_order
    } else {
        raw.space_order
    };
    let collections_authority = match raw.collections_authority {
        Some(authority) if original_version >= 7 => authority,
        Some(_) => {
            return Err("Collections authority cannot appear before snapshot version 7".into())
        }
        None if original_version >= 7 => {
            return Err("snapshot version 7 requires Collections authority".into());
        }
        None => crate::collections_extension_contract::CollectionsAuthorityV1::default(),
    };
    collections_authority
        .validate()
        .map_err(|error| format!("invalid Collections authority: {error}"))?;
    if original_version < 7 && raw.collections_action_sequence != 0 {
        return Err("Collections action sequence cannot appear before snapshot version 7".into());
    }
    if raw.collections_action_sequence > MAX_COLLECTIONS_ACTION_SEQUENCE {
        return Err("Collections action sequence exceeds the supported range".into());
    }
    let mut snapshot = SessionSnapshot {
        version: original_version,
        workspaces,
        repositories,
        space_order,
        active: raw.active,
        selected: raw.selected,
        sidebar_width: raw.sidebar_width,
        sidebar_section_split: raw.sidebar_section_split,
        collapsed_space_keys: raw.collapsed_space_keys,
        delegations: raw.delegations,
        collections_authority,
        collections_action_sequence: raw.collections_action_sequence,
    };
    super::restore::validate_snapshot_pane_admission(&snapshot)?;
    let normalized = normalize && normalize_terminal_identities(&mut snapshot, false)?;
    Ok((snapshot, normalized))
}

/// Normalize all persisted session identities before any PTY, runtime, or
/// handoff lease is constructed. Structural vector order decides duplicate
/// survivors; pane maps are visited by sorted pane ID.
pub(crate) fn normalize_terminal_identities(
    snapshot: &mut SessionSnapshot,
    regenerate_tab_and_terminal_ids: bool,
) -> Result<bool, String> {
    normalize_identity_changes(snapshot, regenerate_tab_and_terminal_ids)
        .map(|changes| changes.changed)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct IdentityNormalization {
    pub changed: bool,
    pub schema_version_changed: bool,
    pub internal_identities_changed: bool,
    /// A generation-1 workspace handle changed. Handoff cannot repair this
    /// because an already-running PTY may contain the original public handle.
    pub public_identities_changed: bool,
    pub projections_changed: bool,
    pub structural_state_changed: bool,
}

pub(crate) fn normalize_identity_changes(
    snapshot: &mut SessionSnapshot,
    regenerate_tab_and_terminal_ids: bool,
) -> Result<IdentityNormalization, String> {
    normalize_identities_with(
        snapshot,
        regenerate_tab_and_terminal_ids,
        crate::workspace::TabId::alloc,
        crate::terminal::TerminalId::alloc,
    )
}

fn normalize_identities_with<T, U>(
    snapshot: &mut SessionSnapshot,
    regenerate_tab_and_terminal_ids: bool,
    mut generate_tab: T,
    mut generate_terminal: U,
) -> Result<IdentityNormalization, String>
where
    T: FnMut() -> std::io::Result<crate::workspace::TabId>,
    U: FnMut() -> std::io::Result<crate::terminal::TerminalId>,
{
    let target_version = SNAPSHOT_VERSION;
    let schema_version_changed = snapshot.version != target_version;
    let mut changed = schema_version_changed;
    let mut internal_identities_changed = false;
    let mut public_identities_changed = false;
    let mut structural_state_changed = false;

    fn collect_layout_panes(layout: &LayoutSnapshot, panes: &mut Vec<u32>) {
        match layout {
            LayoutSnapshot::Pane(id) => panes.push(*id),
            LayoutSnapshot::Stack { children, .. } => {
                for child in children {
                    collect_layout_panes(child, panes);
                }
            }
            LayoutSnapshot::Split { first, second, .. } => {
                collect_layout_panes(first, panes);
                collect_layout_panes(second, panes);
            }
        }
    }

    for workspace in &mut snapshot.workspaces {
        for tab in &mut workspace.tabs {
            let mut layout_panes = Vec::new();
            collect_layout_panes(&tab.layout, &mut layout_panes);
            layout_panes.sort_unstable();
            layout_panes.dedup();
            for pane_id in layout_panes {
                if let std::collections::hash_map::Entry::Vacant(entry) = tab.panes.entry(pane_id) {
                    entry.insert(PaneSnapshot::legacy_layout_only());
                    changed = true;
                    structural_state_changed = true;
                }
            }
        }
    }

    // Reserve every valid unique workspace handle before assigning the lowest
    // unused canonical handle to malformed and duplicate entries.
    let mut occupied_workspaces = std::collections::HashSet::new();
    let mut old_workspace_ids = Vec::with_capacity(snapshot.workspaces.len());
    let mut workspace_rewrites = std::collections::HashMap::new();
    for workspace in &mut snapshot.workspaces {
        let old = workspace.id.clone();
        let accepted = old
            .as_deref()
            .filter(|id| canonical_workspace_id(id))
            .filter(|id| occupied_workspaces.insert((*id).to_owned()));
        if let Some(id) = accepted {
            workspace_rewrites
                .entry(id.to_owned())
                .or_insert_with(|| id.to_owned());
        } else {
            workspace.id = None;
            changed = true;
            public_identities_changed = true;
        }
        old_workspace_ids.push(old);
    }
    let mut next_workspace_number = 1usize;
    for (index, workspace) in snapshot.workspaces.iter_mut().enumerate() {
        if workspace.id.is_some() {
            continue;
        }
        let allocated = loop {
            let candidate = format!(
                "w{}",
                crate::workspace::encode_public_number(next_workspace_number),
            );
            next_workspace_number = next_workspace_number
                .checked_add(1)
                .ok_or_else(|| "workspace identity space is exhausted".to_string())?;
            if occupied_workspaces.insert(candidate.clone()) {
                break candidate;
            }
        };
        if let Some(old) = old_workspace_ids[index].as_ref() {
            // Duplicate references intentionally remain bound to the first
            // structural survivor through `or_insert`.
            workspace_rewrites
                .entry(old.clone())
                .or_insert_with(|| allocated.clone());
        }
        workspace.id = Some(allocated);
    }

    // Generation-1 tab handles are persisted separately from opaque TabId.
    // Keep the first positive unique number in structural order, reserve all
    // such survivors, then fill rejected/missing slots with the lowest unused
    // positive numbers. A high historical next value is never reduced.
    for workspace in &mut snapshot.workspaces {
        let original_numbers = workspace.public_tab_numbers.clone();
        let original_next = workspace.next_public_tab_number;
        let mut occupied_numbers = std::collections::HashSet::new();
        let mut normalized_numbers = Vec::with_capacity(workspace.tabs.len());
        for tab_idx in 0..workspace.tabs.len() {
            let accepted = original_numbers
                .get(tab_idx)
                .copied()
                .filter(|number| *number > 0)
                .filter(|number| occupied_numbers.insert(*number));
            normalized_numbers.push(accepted);
        }
        let mut next_candidate = 1usize;
        for number in &mut normalized_numbers {
            if number.is_some() {
                continue;
            }
            while occupied_numbers.contains(&next_candidate) {
                next_candidate = next_candidate
                    .checked_add(1)
                    .ok_or_else(|| "public tab-number space is exhausted".to_string())?;
            }
            *number = Some(next_candidate);
            occupied_numbers.insert(next_candidate);
        }
        let public_tab_numbers = normalized_numbers
            .into_iter()
            .map(|number| {
                number.ok_or_else(|| "public tab-number normalization failed".to_string())
            })
            .collect::<Result<Vec<_>, _>>()?;
        let minimum_next = public_tab_numbers
            .iter()
            .copied()
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or_else(|| "public tab-number space is exhausted".to_string())?;
        let next_public_tab_number = original_next.max(minimum_next).max(1);
        if next_public_tab_number == usize::MAX {
            return Err("public tab-number space is exhausted".to_string());
        }
        if public_tab_numbers != original_numbers || next_public_tab_number != original_next {
            workspace.public_tab_numbers = public_tab_numbers;
            workspace.next_public_tab_number = next_public_tab_number;
            changed = true;
            public_identities_changed = true;
        }
    }

    // Rebuild projections from workspace provenance after identity repair.
    // This is what makes every repaired duplicate reachable at runtime rather
    // than merely assigning it an ID that no Space references.
    let old_repositories = snapshot.repositories.clone();
    let old_space_order = snapshot.space_order.clone();
    let repository_ids = snapshot
        .repositories
        .iter()
        .map(|repository| repository.id.clone())
        .collect::<std::collections::HashSet<_>>();
    if repository_ids.len() != snapshot.repositories.len() {
        return Err("snapshot contains duplicate repository identities".to_string());
    }
    let workspace_memberships = snapshot
        .workspaces
        .iter()
        .map(|workspace| {
            workspace
                .checkout
                .as_ref()
                .map(|checkout| checkout.repository_id.clone())
                .or_else(|| {
                    workspace.worktree_space.as_ref().map(|space| {
                        crate::repository::stable_repository_id(std::path::Path::new(&space.key))
                    })
                })
        })
        .collect::<Vec<_>>();
    for repository_id in workspace_memberships.iter().flatten() {
        if !repository_ids.contains(repository_id) {
            return Err(format!(
                "workspace checkout references missing repository {repository_id}"
            ));
        }
    }

    for repository in &mut snapshot.repositories {
        if let Some(workspace_id) = &mut repository.last_focused_workspace_id {
            if let Some(replacement) = workspace_rewrites.get(workspace_id) {
                workspace_id.clone_from(replacement);
            }
        }
        repository.checkout_workspace_ids.clear();
    }
    for (workspace, membership) in snapshot.workspaces.iter().zip(&workspace_memberships) {
        let Some(repository_id) = membership else {
            continue;
        };
        let workspace_id = workspace
            .id
            .as_ref()
            .ok_or_else(|| "normalized workspace identity disappeared".to_string())?;
        let repository = snapshot
            .repositories
            .iter_mut()
            .find(|repository| &repository.id == repository_id)
            .ok_or_else(|| format!("workspace repository {repository_id} disappeared"))?;
        repository.checkout_workspace_ids.push(workspace_id.clone());
    }
    for repository in &mut snapshot.repositories {
        if repository
            .last_focused_workspace_id
            .as_ref()
            .is_some_and(|workspace_id| !repository.checkout_workspace_ids.contains(workspace_id))
        {
            repository.last_focused_workspace_id = None;
        }
        if repository.last_focused_workspace_id.is_none() {
            repository.last_focused_workspace_id =
                repository.checkout_workspace_ids.first().cloned();
        }
    }

    for space in &mut snapshot.space_order {
        if let crate::repository::SpaceRef::StandaloneWorkspace(workspace_id) = space {
            if let Some(replacement) = workspace_rewrites.get(workspace_id) {
                workspace_id.clone_from(replacement);
            }
        }
    }
    let standalone_ids = snapshot
        .workspaces
        .iter()
        .zip(&workspace_memberships)
        .filter_map(|(workspace, membership)| membership.is_none().then_some(workspace.id.clone()))
        .flatten()
        .collect::<std::collections::HashSet<_>>();
    let mut seen_spaces = std::collections::HashSet::new();
    snapshot.space_order.retain(|space| {
        let (valid, key) = match space {
            crate::repository::SpaceRef::Repository(repository_id) => (
                repository_ids.contains(repository_id),
                format!("repository:{repository_id}"),
            ),
            crate::repository::SpaceRef::StandaloneWorkspace(workspace_id) => (
                standalone_ids.contains(workspace_id),
                format!("workspace:{workspace_id}"),
            ),
        };
        valid && seen_spaces.insert(key)
    });
    for (workspace, membership) in snapshot.workspaces.iter().zip(&workspace_memberships) {
        let space = if let Some(repository_id) = membership {
            crate::repository::SpaceRef::Repository(repository_id.clone())
        } else {
            crate::repository::SpaceRef::StandaloneWorkspace(
                workspace
                    .id
                    .clone()
                    .ok_or_else(|| "normalized workspace identity disappeared".to_string())?,
            )
        };
        let key = match &space {
            crate::repository::SpaceRef::Repository(repository_id) => {
                format!("repository:{repository_id}")
            }
            crate::repository::SpaceRef::StandaloneWorkspace(workspace_id) => {
                format!("workspace:{workspace_id}")
            }
        };
        if seen_spaces.insert(key) {
            snapshot.space_order.push(space);
        }
    }
    let projections_changed =
        snapshot.repositories != old_repositories || snapshot.space_order != old_space_order;
    changed |= projections_changed;

    let mut occupied_tabs = std::collections::HashSet::new();
    for workspace in &mut snapshot.workspaces {
        for tab in &mut workspace.tabs {
            let accepted = (!regenerate_tab_and_terminal_ids)
                .then(|| tab.id.as_ref().and_then(RawTabId::validated))
                .flatten()
                .filter(|id| occupied_tabs.insert(id.clone()));
            if accepted.is_none() {
                tab.id = None;
                changed = true;
                internal_identities_changed = true;
            }
        }
    }
    for workspace in &mut snapshot.workspaces {
        for tab in &mut workspace.tabs {
            if tab.id.is_some() {
                continue;
            }
            let allocated =
                crate::workspace::TabId::alloc_unique_with(&occupied_tabs, &mut generate_tab)
                    .map_err(|error| format!("allocate tab identity: {error}"))?;
            occupied_tabs.insert(allocated.clone());
            tab.id = Some(RawTabId::from_validated(&allocated));
        }
    }

    let mut occupied_terminals = std::collections::HashSet::new();
    for workspace in &mut snapshot.workspaces {
        for tab in &mut workspace.tabs {
            let mut pane_ids = tab.panes.keys().copied().collect::<Vec<_>>();
            pane_ids.sort_unstable();
            for pane_id in pane_ids {
                let pane = tab
                    .panes
                    .get_mut(&pane_id)
                    .ok_or_else(|| "pane disappeared during identity normalization".to_string())?;
                let accepted = (!regenerate_tab_and_terminal_ids)
                    .then(|| pane.terminal_id.as_ref().and_then(RawTerminalId::validated))
                    .flatten()
                    .filter(|id| occupied_terminals.insert(id.clone()));
                if accepted.is_none() {
                    pane.terminal_id = None;
                    changed = true;
                    internal_identities_changed = true;
                }
            }
        }
    }
    for workspace in &mut snapshot.workspaces {
        for tab in &mut workspace.tabs {
            let mut pane_ids = tab.panes.keys().copied().collect::<Vec<_>>();
            pane_ids.sort_unstable();
            for pane_id in pane_ids {
                let pane = tab
                    .panes
                    .get_mut(&pane_id)
                    .ok_or_else(|| "pane disappeared during identity allocation".to_string())?;
                if pane.terminal_id.is_some() {
                    continue;
                }
                let allocated = crate::terminal::TerminalId::alloc_unique_with(
                    &occupied_terminals,
                    &mut generate_terminal,
                )
                .map_err(|error| format!("allocate terminal identity: {error}"))?;
                occupied_terminals.insert(allocated.clone());
                pane.terminal_id = Some(RawTerminalId::from_validated(&allocated));
            }
        }
    }
    snapshot.version = target_version;
    Ok(IdentityNormalization {
        changed,
        schema_version_changed,
        internal_identities_changed,
        public_identities_changed,
        projections_changed,
        structural_state_changed,
    })
}

pub(crate) fn tab_id_for_snapshot(tab: &TabSnapshot) -> Result<crate::workspace::TabId, String> {
    tab.id
        .as_ref()
        .and_then(RawTabId::validated)
        .ok_or_else(|| "snapshot tab has no validated tab identity".to_string())
}

pub(crate) fn terminal_id_for_pane_snapshot(
    pane: &PaneSnapshot,
) -> Result<crate::terminal::TerminalId, String> {
    pane.terminal_id
        .as_ref()
        .and_then(RawTerminalId::validated)
        .ok_or_else(|| "snapshot pane has no validated terminal identity".to_string())
}

fn migrate_workspace(raw: serde_json::Value) -> Result<WorkspaceSnapshot, String> {
    if raw.get("identity_cwd").is_some() {
        return serde_json::from_value(raw).map_err(|e| e.to_string());
    }

    if raw.get("layout").is_some() {
        let legacy =
            serde_json::from_value::<LegacyWorkspaceSnapshot>(raw).map_err(|e| e.to_string())?;
        return Ok(legacy.into());
    }

    Err("workspace snapshot is neither current nor legacy format".to_string())
}

fn legacy_identity_cwd(snap: &LegacyWorkspaceSnapshot) -> PathBuf {
    let root_pane = snap
        .root_pane
        .or_else(|| first_pane_id_in_layout(&snap.layout));

    root_pane
        .and_then(|pane_id| snap.panes.get(&pane_id))
        .map(|pane| pane.cwd.clone())
        .or_else(|| {
            first_pane_id_in_layout(&snap.layout)
                .and_then(|pane_id| snap.panes.get(&pane_id))
                .map(|pane| pane.cwd.clone())
        })
        .or_else(|| {
            snap.panes
                .keys()
                .min()
                .and_then(|pane_id| snap.panes.get(pane_id))
                .map(|pane| pane.cwd.clone())
        })
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| "/".into()))
}

fn first_pane_id_in_layout(layout: &LayoutSnapshot) -> Option<u32> {
    match layout {
        LayoutSnapshot::Pane(id) => Some(*id),
        LayoutSnapshot::Stack {
            selected, children, ..
        } => children.get(*selected).and_then(first_pane_id_in_layout),
        LayoutSnapshot::Split { first, second, .. } => {
            first_pane_id_in_layout(first).or_else(|| first_pane_id_in_layout(second))
        }
    }
}

pub(crate) fn reserve_empty_collection(
    snapshot: &mut SessionSnapshot,
    workspace_index: usize,
    tab_index: usize,
    target_pane: u32,
    collection_id: crate::layout::CollectionId,
    direction: ratatui::layout::Direction,
    ratio: f32,
    focus: bool,
) -> Result<(), String> {
    fn insert(
        node: &mut LayoutSnapshot,
        target: u32,
        collection_id: crate::layout::CollectionId,
        direction: &DirectionSnapshot,
        ratio: f32,
    ) -> bool {
        match node {
            LayoutSnapshot::Pane(pane) if *pane == target => {
                let old = std::mem::replace(node, LayoutSnapshot::Pane(target));
                *node = LayoutSnapshot::Split {
                    direction: direction.clone(),
                    ratio,
                    first: Box::new(old),
                    second: Box::new(LayoutSnapshot::Stack {
                        id: collection_id,
                        selected: 0,
                        children: Vec::new(),
                    }),
                };
                true
            }
            LayoutSnapshot::Split { first, second, .. } => {
                insert(first, target, collection_id, direction, ratio)
                    || insert(second, target, collection_id, direction, ratio)
            }
            _ => false,
        }
    }
    let tab = snapshot
        .workspaces
        .get_mut(workspace_index)
        .and_then(|workspace| workspace.tabs.get_mut(tab_index))
        .ok_or("reserved Collections tab disappeared")?;
    if tab
        .collections
        .iter()
        .any(|entry| entry.id == collection_id)
    {
        return Err("reserved Collections identity already exists".into());
    }
    let direction = match direction {
        ratatui::layout::Direction::Horizontal => DirectionSnapshot::Horizontal,
        ratatui::layout::Direction::Vertical => DirectionSnapshot::Vertical,
    };
    let ratio = if ratio.is_finite() {
        ratio.clamp(0.1, 0.9)
    } else {
        0.5
    };
    if !insert(
        &mut tab.layout,
        target_pane,
        collection_id,
        &direction,
        ratio,
    ) {
        return Err("reserved Collections target disappeared".into());
    }
    tab.collections.push(CollectionSnapshot {
        id: collection_id,
        label: None,
        archived: Vec::new(),
    });
    if focus {
        tab.focused_leaf = Some(LayoutLeafSnapshot::Stack(collection_id));
    }
    Ok(())
}

/// Capture the current app state into a serializable snapshot.
pub fn capture(
    workspaces: &[Workspace],
    repositories: &[crate::repository::Repository],
    space_order: &[crate::repository::SpaceRef],
    terminals: &std::collections::HashMap<
        crate::terminal::TerminalId,
        crate::terminal::TerminalState,
    >,
    terminal_runtimes: &TerminalRuntimeRegistry,
    active: Option<usize>,
    selected: usize,
) -> SessionSnapshot {
    SessionSnapshot {
        version: SNAPSHOT_VERSION,
        workspaces: workspaces
            .iter()
            .map(|workspace| capture_workspace(workspace, terminals, terminal_runtimes))
            .collect(),
        repositories: repositories.to_vec(),
        space_order: space_order.to_vec(),
        active,
        selected,
        sidebar_width: None,
        sidebar_section_split: None,
        collapsed_space_keys: std::collections::HashSet::new(),
        delegations: Vec::new(),
        collections_authority:
            crate::collections_extension_contract::CollectionsAuthorityV1::default(),
        collections_action_sequence: 0,
    }
}

fn capture_workspace(
    ws: &Workspace,
    terminals: &std::collections::HashMap<
        crate::terminal::TerminalId,
        crate::terminal::TerminalState,
    >,
    terminal_runtimes: &TerminalRuntimeRegistry,
) -> WorkspaceSnapshot {
    WorkspaceSnapshot {
        id: Some(ws.id.clone()),
        custom_name: ws.custom_name.clone(),
        identity_cwd: ws
            .checkout
            .as_ref()
            .map(|checkout| checkout.checkout_path.clone())
            .or_else(|| ws.resolved_identity_cwd_from(terminals, terminal_runtimes))
            .unwrap_or_else(|| ws.identity_cwd.clone()),
        checkout: ws.checkout.clone(),
        worktree_space: ws.worktree_space.clone(),
        public_pane_numbers: ws
            .public_pane_numbers
            .iter()
            .map(|(pane_id, number)| (pane_id.raw(), *number))
            .collect(),
        next_public_pane_number: ws.next_public_pane_number,
        public_tab_numbers: ws.tabs.iter().map(|tab| tab.number).collect(),
        next_public_tab_number: ws.next_public_tab_number,
        tabs: ws
            .tabs
            .iter()
            .map(|tab| capture_tab(tab, terminals, terminal_runtimes))
            .collect(),
        active_tab: ws.active_tab,
    }
}

fn capture_tab(
    tab: &crate::workspace::Tab,
    terminals: &std::collections::HashMap<
        crate::terminal::TerminalId,
        crate::terminal::TerminalState,
    >,
    terminal_runtimes: &TerminalRuntimeRegistry,
) -> TabSnapshot {
    let mut panes = HashMap::new();
    for id in tab.panes.keys() {
        let cwd = tab
            .cwd_for_pane(*id, terminals, terminal_runtimes)
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| "/".into()));
        let terminal = tab
            .panes
            .get(id)
            .and_then(|pane| terminals.get(&pane.attached_terminal_id));
        let label = terminal.and_then(|terminal| terminal.manual_label.clone());
        let (agent_name, managed_agent_kind) = terminal
            .filter(|terminal| !terminal.managed_agent_launch_pending())
            .map(|terminal| {
                (
                    terminal.agent_name.clone(),
                    terminal
                        .managed_agent_kind()
                        .map(|agent| crate::detect::agent_label(agent).to_string()),
                )
            })
            .unwrap_or_default();
        let launch_argv = terminal.and_then(|terminal| terminal.launch_argv.clone());
        let agent_session = terminal.and_then(|terminal| {
            if let Some(authority) = terminal.hook_authority.as_ref() {
                if let Some(session_ref) = authority.session_ref.as_ref() {
                    return Some(PaneAgentSessionSnapshot {
                        source: authority.source.clone(),
                        agent: authority.agent_label.clone(),
                        kind: session_ref.kind,
                        value: session_ref.value.clone(),
                    });
                }
            }
            terminal
                .persisted_agent_session
                .as_ref()
                .map(|session| PaneAgentSessionSnapshot {
                    source: session.source.clone(),
                    agent: session.agent.clone(),
                    kind: session.session_ref.kind,
                    value: session.session_ref.value.clone(),
                })
        });
        panes.insert(
            id.raw(),
            PaneSnapshot {
                terminal_id: Some(RawTerminalId::from_validated(
                    &tab.panes[id].attached_terminal_id,
                )),
                cwd,
                label,
                agent_name,
                managed_agent_kind,
                agent_session,
                launch_argv,
            },
        );
    }
    // Only product policy is persisted here; stack placement is in the tree.
    let mut collections: Vec<_> = tab
        .collections()
        .map(|collection| CollectionSnapshot {
            id: collection.id(),
            label: collection.legacy_label().map(str::to_owned),
            archived: collection
                .legacy_archived_members()
                .map(|id| id.raw())
                .collect(),
        })
        .collect();
    collections.sort_by_key(|collection| collection.id.raw());
    TabSnapshot {
        id: Some(RawTabId::from_validated(&tab.id)),
        custom_name: tab.custom_name.clone(),
        layout: capture_typed_node(tab.layout.typed_root()),
        collections,
        panes,
        zoomed: tab.zoomed,
        focused: Some(tab.layout.focused().raw()),
        focused_leaf: Some(capture_layout_leaf(tab.layout.focused_leaf())),
        root_pane: Some(tab.root_pane.raw()),
    }
}

/// Capture pane screen history separately from the structural session snapshot.
pub fn capture_history(
    workspaces: &[Workspace],
    terminal_runtimes: &TerminalRuntimeRegistry,
) -> SessionHistorySnapshot {
    SessionHistorySnapshot {
        version: HISTORY_SNAPSHOT_VERSION,
        workspaces: workspaces
            .iter()
            .map(|workspace| WorkspaceHistorySnapshot {
                tabs: workspace
                    .tabs
                    .iter()
                    .map(|tab| TabHistorySnapshot {
                        panes: capture_tab_history(tab, terminal_runtimes),
                    })
                    .collect(),
            })
            .collect(),
    }
}

fn capture_tab_history(
    tab: &crate::workspace::Tab,
    terminal_runtimes: &TerminalRuntimeRegistry,
) -> HashMap<u32, PaneHistorySnapshot> {
    let mut panes = HashMap::new();
    for (id, pane) in &tab.panes {
        if let Some(history) = capture_pane_history(Some(pane), terminal_runtimes) {
            panes.insert(id.raw(), history);
        }
    }
    panes
}

fn capture_pane_history(
    pane: Option<&crate::pane::PaneState>,
    terminal_runtimes: &TerminalRuntimeRegistry,
) -> Option<PaneHistorySnapshot> {
    let ansi = terminal_runtimes
        .get(&pane?.attached_terminal_id)?
        .snapshot_history()?;
    let lines = ansi.lines().count();
    Some(PaneHistorySnapshot { ansi, lines })
}

fn capture_layout_leaf(leaf: crate::layout::LayoutLeaf) -> LayoutLeafSnapshot {
    match leaf {
        crate::layout::LayoutLeaf::Pane(id) => LayoutLeafSnapshot::Pane(id.raw()),
        crate::layout::LayoutLeaf::Stack(id) => LayoutLeafSnapshot::Stack(id.into()),
    }
}

fn capture_typed_node(node: &crate::layout::TypedNode) -> LayoutSnapshot {
    match node {
        crate::layout::TypedNode::Leaf(id) => LayoutSnapshot::Pane(id.raw()),
        crate::layout::TypedNode::Stack {
            id,
            selected,
            children,
        } => LayoutSnapshot::Stack {
            id: (*id).into(),
            selected: *selected,
            children: children.iter().map(capture_typed_node).collect(),
        },
        crate::layout::TypedNode::Split {
            direction,
            ratio,
            first,
            second,
        } => LayoutSnapshot::Split {
            direction: match direction {
                Direction::Horizontal => DirectionSnapshot::Horizontal,
                Direction::Vertical => DirectionSnapshot::Vertical,
            },
            ratio: *ratio,
            first: Box::new(capture_typed_node(first)),
            second: Box::new(capture_typed_node(second)),
        },
    }
}

#[cfg(test)]
pub(super) fn capture_node(node: &Node) -> LayoutSnapshot {
    match node {
        Node::Pane(id) => LayoutSnapshot::Pane(id.raw()),
        Node::Split {
            direction,
            ratio,
            first,
            second,
        } => LayoutSnapshot::Split {
            direction: match direction {
                Direction::Horizontal => DirectionSnapshot::Horizontal,
                Direction::Vertical => DirectionSnapshot::Vertical,
            },
            ratio: *ratio,
            first: Box::new(capture_node(first)),
            second: Box::new(capture_node(second)),
        },
    }
}

pub(crate) fn validate_snapshot(content: &str) -> Result<(), String> {
    let snapshot = parse_snapshot(content)?;
    super::restore::validate_snapshot_admission(&snapshot)
}

pub(super) fn parse_snapshot_with_status(content: &str) -> Result<(SessionSnapshot, bool), String> {
    let raw = serde_json::from_str::<RawSessionSnapshot>(content).map_err(|e| e.to_string())?;
    if raw.version > SNAPSHOT_VERSION {
        return Err(format!(
            "snapshot version {} is newer than supported {}",
            raw.version, SNAPSHOT_VERSION
        ));
    }
    migrate_snapshot(raw, true)
}

/// Handoff converts legacy layout DTOs without repairing identity. The manifest
/// validator must still prove canonical identities before accepting descriptors.
#[cfg(unix)]
pub(crate) fn deserialize_handoff_snapshot<'de, D>(
    deserializer: D,
) -> Result<SessionSnapshot, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = RawSessionSnapshot::deserialize(deserializer)?;
    if raw.version > SNAPSHOT_VERSION {
        return Err(serde::de::Error::custom(
            "downgrade unsupported: future handoff snapshot",
        ));
    }
    migrate_snapshot(raw, false)
        .map(|(snapshot, _)| snapshot)
        .map_err(serde::de::Error::custom)
}

pub(crate) fn parse_snapshot(content: &str) -> Result<SessionSnapshot, String> {
    parse_snapshot_with_status(content).map(|(snapshot, _)| snapshot)
}

pub(super) fn parse_history_snapshot(content: &str) -> Result<SessionHistorySnapshot, String> {
    let snapshot =
        serde_json::from_str::<SessionHistorySnapshot>(content).map_err(|e| e.to_string())?;
    if snapshot.version > HISTORY_SNAPSHOT_VERSION {
        return Err(format!(
            "history snapshot version {} is newer than supported {}",
            snapshot.version, HISTORY_SNAPSHOT_VERSION
        ));
    }
    Ok(snapshot)
}

pub(super) fn snapshot_file_version(content: &str) -> Option<u32> {
    serde_json::from_str::<RawSessionSnapshot>(content)
        .ok()
        .map(|raw| raw.version)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::PathBuf;

    use ratatui::layout::{Direction, Rect};

    use super::*;
    use crate::app::{AppState, Mode};
    use crate::layout::NavDirection;
    use crate::workspace::Workspace;

    fn session_fixture(name: &str) -> &'static str {
        match name {
            "current-herdr" => {
                include_str!("../../tests/fixtures/session/current-herdr-session.json")
            }
            "current-herdr-dev" => {
                include_str!("../../tests/fixtures/session/current-herdr-dev-session.json")
            }
            "legacy-pre-tabs-v2" => {
                include_str!("../../tests/fixtures/session/legacy-pre-tabs-v2.json")
            }
            other => panic!("unknown session fixture: {other}"),
        }
    }

    fn test_session_path(name: &str) -> String {
        std::env::current_dir()
            .unwrap()
            .join(name)
            .display()
            .to_string()
    }

    fn state_with_workspaces(names: &[&str]) -> AppState {
        let mut state = AppState::test_new();
        state.workspaces = names.iter().map(|name| Workspace::test_new(name)).collect();
        state.ensure_test_terminals();
        if !state.workspaces.is_empty() {
            state.active = Some(0);
            state.selected = 0;
            state.mode = Mode::Terminal;
        }
        state
    }

    fn capture_from_state(state: &AppState) -> SessionSnapshot {
        let terminal_runtimes = TerminalRuntimeRegistry::new();
        capture_from_state_with_runtimes(state, &terminal_runtimes)
    }

    fn capture_from_state_with_runtimes(
        state: &AppState,
        terminal_runtimes: &TerminalRuntimeRegistry,
    ) -> SessionSnapshot {
        capture(
            &state.workspaces,
            &state.repositories,
            &state.space_order,
            &state.terminals,
            terminal_runtimes,
            state.active,
            state.selected,
        )
    }

    fn capture_history_from_state_with_runtimes(
        state: &AppState,
        terminal_runtimes: &TerminalRuntimeRegistry,
    ) -> SessionHistorySnapshot {
        capture_history(&state.workspaces, terminal_runtimes)
    }

    fn root_split_ratio(tab: &TabSnapshot) -> Option<f32> {
        match &tab.layout {
            LayoutSnapshot::Split { ratio, .. } => Some(*ratio),
            LayoutSnapshot::Pane(_) | LayoutSnapshot::Stack { .. } => None,
        }
    }

    #[test]
    fn managed_agent_snapshot_omits_pending_and_persists_active_ownership() {
        let mut state = state_with_workspaces(&["managed-snapshot"]);
        let root = state.workspaces[0].tabs[0].root_pane;
        let terminal_id = state.workspaces[0].tabs[0].panes[&root]
            .attached_terminal_id
            .clone();
        let now = std::time::Instant::now();
        state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .begin_managed_agent(
                "reviewer".into(),
                crate::detect::Agent::Pi,
                now,
                std::time::Duration::ZERO,
                std::time::Duration::from_secs(1),
            );

        let pending = capture_from_state(&state);
        let pending_pane = &pending.workspaces[0].tabs[0].panes[&root.raw()];
        assert_eq!(pending_pane.agent_name, None);
        assert_eq!(pending_pane.managed_agent_kind, None);

        let terminal = state.terminals.get_mut(&terminal_id).unwrap();
        terminal.set_detected_state(
            Some(crate::detect::Agent::Pi),
            crate::detect::AgentState::Idle,
        );
        assert!(terminal.reconcile_managed_agent_at(now, false));
        let active = capture_from_state(&state);
        let active_pane = &active.workspaces[0].tabs[0].panes[&root.raw()];
        assert_eq!(active_pane.agent_name.as_deref(), Some("reviewer"));
        assert_eq!(active_pane.managed_agent_kind.as_deref(), Some("pi"));
    }

    #[test]
    fn checkout_identity_persists_independently_from_live_workspace_cwd() {
        let mut state = state_with_workspaces(&["checkout"]);
        let repository_id = "r-checkout".to_owned();
        let checkout_path = PathBuf::from("/repos/app");
        state.workspaces[0].identity_cwd = PathBuf::from("/tmp/live-pane-cwd");
        state.workspaces[0].checkout = Some(crate::repository::CheckoutProvenance {
            repository_id: repository_id.clone(),
            checkout_path: checkout_path.clone(),
            kind: crate::repository::CheckoutKind::Primary,
        });
        state.repositories = vec![crate::repository::Repository {
            id: repository_id.clone(),
            git_common_dir: PathBuf::from("/repos/app/.git"),
            label: "app".into(),
            custom_name: None,
            preferred_base: None,
            checkout_workspace_ids: vec![state.workspaces[0].id.clone()],
            last_focused_workspace_id: Some(state.workspaces[0].id.clone()),
        }];
        state.space_order = vec![crate::repository::SpaceRef::Repository(repository_id)];

        let snapshot = capture_from_state(&state);

        assert_eq!(snapshot.workspaces[0].identity_cwd, checkout_path);
        assert_eq!(
            snapshot.workspaces[0].checkout,
            state.workspaces[0].checkout
        );
        assert_eq!(snapshot.repositories, state.repositories);
        assert_eq!(snapshot.space_order, state.space_order);
    }

    #[test]
    fn reserved_empty_collection_changes_only_candidate_snapshot() {
        let state = state_with_workspaces(&["atomic-create"]);
        let pane_id = state.workspaces[0].tabs[0].root_pane;
        let collection_id = crate::layout::CollectionId::alloc().unwrap();
        let mut candidate = capture_from_state(&state);
        reserve_empty_collection(
            &mut candidate,
            0,
            0,
            pane_id.raw(),
            collection_id,
            ratatui::layout::Direction::Horizontal,
            0.5,
            true,
        )
        .unwrap();
        assert_eq!(candidate.workspaces[0].tabs[0].collections.len(), 1);
        assert!(matches!(
            candidate.workspaces[0].tabs[0].focused_leaf,
            Some(LayoutLeafSnapshot::Stack(id)) if id == collection_id
        ));
        assert!(state.workspaces[0].tabs[0]
            .collection(collection_id)
            .is_none());

        let mut rejected = capture_from_state(&state);
        let before = serde_json::to_vec(&rejected).unwrap();
        assert!(reserve_empty_collection(
            &mut rejected,
            0,
            0,
            u32::MAX,
            crate::layout::CollectionId::alloc().unwrap(),
            ratatui::layout::Direction::Horizontal,
            0.5,
            false,
        )
        .is_err());
        assert_eq!(serde_json::to_vec(&rejected).unwrap(), before);
    }

    #[test]
    fn round_trip_empty_session() {
        let snap = SessionSnapshot {
            delegations: Vec::new(),
            collections_authority: Default::default(),
            collections_action_sequence: 0,
            version: SNAPSHOT_VERSION,
            repositories: Vec::new(),
            space_order: Vec::new(),
            workspaces: vec![],
            active: None,
            selected: 0,
            sidebar_width: Some(26),
            sidebar_section_split: Some(0.5),
            collapsed_space_keys: std::collections::HashSet::new(),
        };
        let json = serde_json::to_string(&snap).unwrap();
        let restored = parse_snapshot(&json).unwrap();
        assert!(restored.workspaces.is_empty());
        assert_eq!(restored.active, None);
        assert_eq!(restored.sidebar_width, Some(26));
        assert_eq!(restored.sidebar_section_split, Some(0.5));
    }

    #[test]
    fn round_trip_layout_snapshot() {
        let layout = LayoutSnapshot::Split {
            direction: DirectionSnapshot::Horizontal,
            ratio: 0.6,
            first: Box::new(LayoutSnapshot::Pane(0)),
            second: Box::new(LayoutSnapshot::Split {
                direction: DirectionSnapshot::Vertical,
                ratio: 0.5,
                first: Box::new(LayoutSnapshot::Pane(1)),
                second: Box::new(LayoutSnapshot::Pane(2)),
            }),
        };
        let json = serde_json::to_string(&layout).unwrap();
        let restored: LayoutSnapshot = serde_json::from_str(&json).unwrap();

        match restored {
            LayoutSnapshot::Split { ratio, .. } => assert!((ratio - 0.6).abs() < 0.01),
            _ => panic!("expected split"),
        }
    }

    #[test]
    fn round_trip_full_workspace_snapshot() {
        let mut panes = HashMap::new();
        panes.insert(
            0,
            PaneSnapshot {
                terminal_id: None,
                cwd: PathBuf::from("/home/can/Projects/herdr"),
                label: None,
                agent_name: None,
                managed_agent_kind: None,
                agent_session: None,
                launch_argv: None,
            },
        );
        panes.insert(
            1,
            PaneSnapshot {
                terminal_id: None,
                cwd: PathBuf::from("/home/can/Projects/website"),
                label: Some("website".into()),
                agent_name: None,
                managed_agent_kind: None,
                agent_session: None,
                launch_argv: None,
            },
        );

        let snap = SessionSnapshot {
            repositories: Vec::new(),
            space_order: Vec::new(),
            delegations: Vec::new(),
            collections_authority: Default::default(),
            collections_action_sequence: 0,
            workspaces: vec![WorkspaceSnapshot {
                id: Some("w1".to_string()),
                custom_name: Some("pi-mono".to_string()),
                identity_cwd: PathBuf::from("/home/can/Projects/herdr"),
                checkout: None,
                worktree_space: None,
                public_pane_numbers: HashMap::from([(0, 1), (1, 2)]),
                next_public_pane_number: 3,
                public_tab_numbers: vec![1],
                next_public_tab_number: 2,
                tabs: vec![TabSnapshot {
                    id: None,
                    collections: Vec::new(),
                    focused_leaf: None,
                    custom_name: Some("api".to_string()),
                    layout: LayoutSnapshot::Split {
                        direction: DirectionSnapshot::Horizontal,
                        ratio: 0.5,
                        first: Box::new(LayoutSnapshot::Pane(0)),
                        second: Box::new(LayoutSnapshot::Pane(1)),
                    },
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
            collapsed_space_keys: std::collections::HashSet::new(),
            version: SNAPSHOT_VERSION,
        };

        let json = serde_json::to_string_pretty(&snap).unwrap();
        let restored = parse_snapshot(&json).unwrap();

        assert_eq!(restored.workspaces.len(), 1);
        assert_eq!(restored.workspaces[0].id.as_deref(), Some("w1"));
        assert_eq!(
            restored.workspaces[0].custom_name.as_deref(),
            Some("pi-mono")
        );
        assert_eq!(restored.workspaces[0].tabs.len(), 1);
        assert_eq!(restored.workspaces[0].tabs[0].panes.len(), 2);
        assert_eq!(
            restored.workspaces[0].tabs[0].panes[&0].cwd,
            PathBuf::from("/home/can/Projects/herdr")
        );
        assert_eq!(
            restored.workspaces[0].tabs[0].panes[&1].label.as_deref(),
            Some("website")
        );
        assert_eq!(restored.sidebar_width, Some(26));
        assert_eq!(restored.sidebar_section_split, Some(0.5));
    }

    #[test]
    fn current_session_fixture_parses() {
        let snap = parse_snapshot(session_fixture("current-herdr")).unwrap();

        assert_eq!(snap.version, SNAPSHOT_VERSION);
        assert_eq!(snap.workspaces.len(), 2);
        assert_eq!(snap.active, Some(0));
        assert_eq!(snap.selected, 0);
        assert_eq!(snap.sidebar_width, None);
        assert_eq!(snap.sidebar_section_split, None);
        assert_eq!(snap.workspaces[0].tabs.len(), 2);
        assert_eq!(
            snap.workspaces[1].identity_cwd,
            PathBuf::from("/home/test/projects/project-b")
        );
    }

    #[test]
    fn current_dev_session_fixture_parses_additive_fields() {
        let snap = parse_snapshot(session_fixture("current-herdr-dev")).unwrap();

        assert_eq!(snap.version, SNAPSHOT_VERSION);
        assert_eq!(snap.workspaces.len(), 2);
        assert_eq!(snap.sidebar_section_split, Some(0.4));
        assert_eq!(snap.workspaces[0].active_tab, 1);
        assert_eq!(snap.workspaces[1].tabs[0].panes.len(), 2);
    }

    #[test]
    fn old_snapshot_defaults_sidebar_fields() {
        let json = serde_json::json!({
            "version": LEGACY_SNAPSHOT_WRITER_VERSION,
            "workspaces": [],
            "active": null,
            "selected": 0
        })
        .to_string();

        let restored = parse_snapshot(&json).unwrap();

        assert_eq!(restored.sidebar_width, None);
        assert_eq!(restored.sidebar_section_split, None);
    }

    #[test]
    fn old_pane_snapshot_with_embedded_history_is_ignored() {
        let json = serde_json::json!({
            "version": LEGACY_SNAPSHOT_WRITER_VERSION,
            "workspaces": [{
                "id": "wtest",
                "identity_cwd": "/tmp",
                "tabs": [{
                    "layout": { "Pane": 0 },
                    "panes": {
                        "0": {
                            "cwd": "/tmp",
                            "history": {
                                "ansi": "legacy-secret",
                                "lines": 1
                            }
                        }
                    },
                    "zoomed": false,
                    "focused": 0,
                    "root_pane": 0
                }],
                "active_tab": 0
            }],
            "active": 0,
            "selected": 0
        })
        .to_string();

        let restored = parse_snapshot(&json).unwrap();

        let encoded = serde_json::to_string(&restored).unwrap();
        assert!(!encoded.contains("legacy-secret"));
        assert!(!encoded.contains("\"history\""));
    }

    #[test]
    fn legacy_workspace_snapshot_migrates_to_single_tab() {
        let snap = parse_snapshot(session_fixture("legacy-pre-tabs-v2")).unwrap();
        let ws = &snap.workspaces[0];

        assert_eq!(snap.version, SNAPSHOT_VERSION);
        assert_eq!(snap.workspaces.len(), 1);
        assert_eq!(ws.custom_name.as_deref(), Some("legacy"));
        assert_eq!(ws.identity_cwd, PathBuf::from("/tmp/pion"));
        assert_eq!(ws.active_tab, 0);
        assert_eq!(ws.tabs.len(), 1);
        assert_eq!(ws.tabs[0].focused, Some(1));
        assert_eq!(ws.tabs[0].root_pane, Some(0));
        assert_eq!(ws.tabs[0].panes[&0].cwd, PathBuf::from("/tmp/pion"));
        assert_eq!(ws.tabs[0].panes[&1].cwd, PathBuf::from("/tmp/herdr"));
    }

    #[test]
    fn capture_contract_tracks_workspace_order_active_and_selected() {
        let mut state = state_with_workspaces(&["a", "b", "c"]);
        state.active = Some(1);
        state.selected = 2;

        state.move_workspace(1, 0);

        let snapshot = capture_from_state(&state);
        let ids: Vec<_> = state.workspaces.iter().map(|ws| ws.id.clone()).collect();
        let captured_ids: Vec<_> = snapshot
            .workspaces
            .iter()
            .map(|ws| ws.id.clone().unwrap())
            .collect();
        assert_eq!(captured_ids, ids);
        assert_eq!(snapshot.active, state.active);
        assert_eq!(snapshot.selected, state.selected);
    }

    #[test]
    fn capture_contract_tracks_workspace_and_tab_names_and_active_tab() {
        let mut state = state_with_workspaces(&["one"]);
        state.workspaces[0].set_custom_name("renamed-workspace".into());
        let second_tab = state.workspaces[0].test_add_tab(Some("logs"));
        state.workspaces[0].switch_tab(second_tab);
        state.workspaces[0].tabs[0].set_custom_name("main".into());

        let snapshot = capture_from_state(&state);
        let workspace = &snapshot.workspaces[0];
        assert_eq!(workspace.custom_name.as_deref(), Some("renamed-workspace"));
        assert_eq!(workspace.active_tab, second_tab);
        assert_eq!(workspace.tabs[0].custom_name.as_deref(), Some("main"));
        assert_eq!(workspace.tabs[1].custom_name.as_deref(), Some("logs"));
    }

    #[test]
    fn capture_contract_tracks_workspace_closure() {
        let mut state = state_with_workspaces(&["one", "two"]);
        state.selected = 1;
        state.active = Some(1);

        state.close_selected_workspace();

        let snapshot = capture_from_state(&state);
        assert_eq!(snapshot.workspaces.len(), 1);
        assert_eq!(snapshot.workspaces[0].custom_name.as_deref(), Some("one"));
        assert_eq!(snapshot.active, Some(0));
        assert_eq!(snapshot.selected, 0);
    }

    #[test]
    fn capture_contract_omits_legacy_server_chrome_state() {
        let state = state_with_workspaces(&["one"]);

        let snapshot = capture_from_state(&state);
        assert_eq!(snapshot.sidebar_width, None);
        assert_eq!(snapshot.sidebar_section_split, None);
        assert!(snapshot.collapsed_space_keys.is_empty());
    }

    #[test]
    fn capture_contract_tracks_worktree_space_membership() {
        let mut state = state_with_workspaces(&["main"]);
        state.workspaces[0].worktree_space = Some(crate::workspace::WorktreeSpaceMembership {
            key: "repo-key".into(),
            label: "herdr".into(),
            repo_root: PathBuf::from("/repo/herdr"),
            checkout_path: PathBuf::from("/repo/herdr/worktree-a"),
            is_linked_worktree: true,
        });

        let snapshot = capture_from_state(&state);

        assert_eq!(
            snapshot.workspaces[0].worktree_space,
            state.workspaces[0].worktree_space
        );
    }

    #[test]
    fn capture_contract_tracks_layout_focus_zoom_and_root_pane() {
        let mut state = state_with_workspaces(&["one"]);
        let root = state.workspaces[0].tabs[0].root_pane;
        let second = state.workspaces[0].test_split(Direction::Horizontal);
        state.workspaces[0].tabs[0].layout.focus_pane(second);
        state.toggle_zoom();

        let snapshot = capture_from_state(&state);
        let tab = &snapshot.workspaces[0].tabs[0];
        assert!(matches!(tab.layout, LayoutSnapshot::Split { .. }));
        assert_eq!(tab.focused, Some(second.raw()));
        assert_eq!(tab.root_pane, Some(root.raw()));
        assert!(tab.zoomed);
        assert_eq!(tab.panes.len(), 2);
    }

    #[test]
    fn capture_contract_tracks_focus_navigation() {
        let mut state = state_with_workspaces(&["one"]);
        let root = state.workspaces[0].tabs[0].root_pane;
        let second = state.workspaces[0].test_split(Direction::Horizontal);
        crate::ui::compute_view_with_runtime_registry(
            &mut state,
            &crate::terminal::TerminalRuntimeRegistry::new(),
            Rect::new(0, 0, 106, 20),
        );

        state.navigate_pane(NavDirection::Right);

        let snapshot = capture_from_state(&state);
        assert_eq!(snapshot.workspaces[0].tabs[0].focused, Some(second.raw()));
        assert_ne!(snapshot.workspaces[0].tabs[0].focused, Some(root.raw()));
    }

    #[test]
    fn maximal_admitted_legacy_split_chain_survives_v8_reader_roundtrip() {
        let last = crate::admission::MAX_TAB_PANES as u32;
        let mut layout = serde_json::json!({"Collection":"collection_900001"});
        for id in 1..last {
            layout = serde_json::json!({"Split":{
                "direction":"Horizontal","ratio":0.5,
                "first":{"Pane":id},"second":layout
            }});
        }
        let panes: serde_json::Map<String, serde_json::Value> = (1..=last)
            .map(|id| {
                (
                    id.to_string(),
                    serde_json::json!({
                        "cwd":"/", "terminal_id":format!("term_{id:032x}")
                    }),
                )
            })
            .collect();
        let original = serde_json::json!({
            "version":6,"repositories":[],"space_order":[],"active":0,"selected":0,
            "workspaces":[{"id":"w1","identity_cwd":"/","active_tab":0,
                "public_tab_numbers":[1],"next_public_tab_number":2,
                "tabs":[{"id":"tab_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "layout":layout,"panes":panes,"focused":last,"root_pane":last,
                "zoomed":false,"collections":[{"id":"collection_900001",
                    "members":[last],"selected":last,"archived":[]}]
            }]}]
        });
        let legacy = serde_json::to_string(&original).unwrap();
        let converted = parse_snapshot(&legacy).expect("legacy source must be reader-valid");
        let current = serde_json::to_string(&converted).unwrap();
        assert!(
            parse_snapshot(&current).is_ok(),
            "a successful conversion must not publish an unreadable v8 file: {:?}",
            parse_snapshot(&current).err()
        );
    }

    #[test]
    fn capture_contract_tracks_resize_ratio_changes() {
        let mut state = state_with_workspaces(&["one"]);
        let root = state.workspaces[0].tabs[0].root_pane;
        state.workspaces[0].test_split(Direction::Horizontal);
        state.workspaces[0].layout.focus_pane(root);
        crate::ui::compute_view_with_runtime_registry(
            &mut state,
            &crate::terminal::TerminalRuntimeRegistry::new(),
            Rect::new(0, 0, 106, 20),
        );
        let before = capture_from_state(&state);

        state.resize_pane(NavDirection::Right);

        let after = capture_from_state(&state);
        let before_ratio = root_split_ratio(&before.workspaces[0].tabs[0]).unwrap();
        let after_ratio = root_split_ratio(&after.workspaces[0].tabs[0]).unwrap();
        assert_ne!(before_ratio, after_ratio);
    }

    #[test]
    fn capture_contract_tracks_tab_closure() {
        let mut state = state_with_workspaces(&["one"]);
        let second_tab = state.workspaces[0].test_add_tab(Some("logs"));
        state.switch_tab(second_tab);

        state.close_tab();

        let snapshot = capture_from_state(&state);
        let workspace = &snapshot.workspaces[0];
        assert_eq!(workspace.tabs.len(), 1);
        assert_eq!(workspace.active_tab, 0);
        assert!(workspace.tabs[0].custom_name.is_none());
    }

    #[test]
    fn capture_contract_tracks_pane_closure() {
        let mut state = state_with_workspaces(&["one"]);
        state.workspaces[0].test_split(Direction::Horizontal);

        state.close_pane();

        let snapshot = capture_from_state(&state);
        let tab = &snapshot.workspaces[0].tabs[0];
        assert_eq!(tab.panes.len(), 1);
        assert!(matches!(tab.layout, LayoutSnapshot::Pane(_)));
        assert!(!tab.zoomed);
    }

    #[test]
    fn capture_contract_tracks_public_id_counters() {
        let mut state = state_with_workspaces(&["one"]);
        let second = state.workspaces[0].test_split(Direction::Horizontal);
        let third = state.workspaces[0].test_split(Direction::Vertical);
        let second_tab = state.workspaces[0].test_add_tab(None);

        state.workspaces[0].close_pane(second);

        let snapshot = capture_from_state(&state);
        let workspace = &snapshot.workspaces[0];
        assert_eq!(
            workspace.public_pane_numbers,
            HashMap::from([
                (state.workspaces[0].tabs[0].root_pane.raw(), 1),
                (third.raw(), 3),
                (state.workspaces[0].tabs[second_tab].root_pane.raw(), 4),
            ])
        );
        assert_eq!(workspace.next_public_pane_number, 5);
        assert_eq!(workspace.public_tab_numbers, vec![1, 2]);
        assert_eq!(workspace.next_public_tab_number, 3);
    }

    #[test]
    fn capture_contract_tracks_workspace_identity_and_pane_cwds() {
        let mut state = state_with_workspaces(&["one"]);
        let root = state.workspaces[0].tabs[0].root_pane;
        state.workspaces[0].identity_cwd = PathBuf::from("/tmp/pion");
        let second = state.workspaces[0].test_split(Direction::Horizontal);
        state.ensure_test_terminals();
        let root_terminal_id = state.workspaces[0].tabs[0].panes[&root]
            .attached_terminal_id
            .clone();
        state.terminals.get_mut(&root_terminal_id).unwrap().cwd = PathBuf::from("/tmp/pion");
        let second_terminal_id = state.workspaces[0].tabs[0].panes[&second]
            .attached_terminal_id
            .clone();
        state.terminals.get_mut(&second_terminal_id).unwrap().cwd = PathBuf::from("/tmp/herdr");

        let snapshot = capture_from_state(&state);
        let workspace = &snapshot.workspaces[0];
        let tab = &workspace.tabs[0];
        assert_eq!(workspace.identity_cwd, PathBuf::from("/tmp/pion"));
        assert_eq!(tab.panes[&root.raw()].cwd, PathBuf::from("/tmp/pion"));
        assert_eq!(tab.panes[&second.raw()].cwd, PathBuf::from("/tmp/herdr"));
    }

    #[tokio::test]
    async fn capture_contract_tracks_pane_history_from_runtime() {
        let state = state_with_workspaces(&["one"]);
        let root = state.workspaces[0].tabs[0].root_pane;
        let terminal_id = state.workspaces[0].tabs[0].panes[&root]
            .attached_terminal_id
            .clone();
        let mut terminal_runtimes = TerminalRuntimeRegistry::new();
        terminal_runtimes.insert(
            terminal_id,
            crate::terminal::TerminalRuntime::test_with_scrollback_bytes(
                20,
                3,
                4096,
                b"alpha\r\nbeta\r\ngamma\r\n",
            ),
        );

        let snapshot = capture_from_state_with_runtimes(&state, &terminal_runtimes);
        let encoded = serde_json::to_string(&snapshot).unwrap();
        assert!(!encoded.contains("beta"));
        assert!(!encoded.contains("gamma"));
        assert!(!encoded.contains("\"history\""));

        let history_snapshot = capture_history_from_state_with_runtimes(&state, &terminal_runtimes);
        let history = &history_snapshot.workspaces[0].tabs[0].panes[&root.raw()];

        assert!(history.ansi.contains("alpha"));
        assert!(history.ansi.contains("gamma"));
        assert!(history.lines >= 3);
    }

    #[tokio::test]
    async fn capture_contract_tracks_history_for_each_pane() {
        let mut state = state_with_workspaces(&["one"]);
        let first = state.workspaces[0].tabs[0].root_pane;
        let second = state.workspaces[0].test_split(Direction::Horizontal);
        let first_terminal_id = state.workspaces[0].tabs[0].panes[&first]
            .attached_terminal_id
            .clone();
        let second_terminal_id = state.workspaces[0].tabs[0].panes[&second]
            .attached_terminal_id
            .clone();
        let mut terminal_runtimes = TerminalRuntimeRegistry::new();
        terminal_runtimes.insert(
            first_terminal_id,
            crate::terminal::TerminalRuntime::test_with_scrollback_bytes(
                20,
                3,
                4096,
                b"first-pane-history\r\n",
            ),
        );
        terminal_runtimes.insert(
            second_terminal_id,
            crate::terminal::TerminalRuntime::test_with_scrollback_bytes(
                20,
                3,
                4096,
                b"second-pane-history\r\n",
            ),
        );

        let snapshot = capture_from_state_with_runtimes(&state, &terminal_runtimes);
        let encoded = serde_json::to_string(&snapshot).unwrap();
        assert!(!encoded.contains("first-pane-history"));
        assert!(!encoded.contains("second-pane-history"));

        let history_snapshot = capture_history_from_state_with_runtimes(&state, &terminal_runtimes);
        let tab = &history_snapshot.workspaces[0].tabs[0];
        let first_history = &tab.panes[&first.raw()];
        let second_history = &tab.panes[&second.raw()];

        assert!(first_history.ansi.contains("first-pane-history"));
        assert!(second_history.ansi.contains("second-pane-history"));
    }

    #[test]
    fn capture_contract_tracks_hook_authority_agent_session() {
        let mut state = state_with_workspaces(&["one"]);
        let session_path = test_session_path("pi-session.jsonl");
        let root = state.workspaces[0].tabs[0].root_pane;
        state.ensure_test_terminals();
        let terminal_id = state.workspaces[0].tabs[0].panes[&root]
            .attached_terminal_id
            .clone();
        let terminal = state.terminals.get_mut(&terminal_id).unwrap();
        terminal.set_detected_state(
            Some(crate::detect::Agent::Pi),
            crate::detect::AgentState::Idle,
        );
        terminal.set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
            source: "herdr:pi".into(),
            agent: "pi".into(),
            session_ref: crate::agent_resume::AgentSessionRef::path(session_path.clone()).unwrap(),
        });
        terminal.set_hook_authority_with_session_ref(
            "herdr:pi".into(),
            "pi".into(),
            crate::detect::AgentState::Working,
            None,
            crate::agent_resume::AgentSessionRef::path(session_path.clone()),
            Some(20),
        );

        let snapshot = capture_from_state(&state);
        let agent_session = snapshot.workspaces[0].tabs[0].panes[&root.raw()]
            .agent_session
            .as_ref()
            .expect("agent session should be captured");

        assert_eq!(agent_session.source, "herdr:pi");
        assert_eq!(agent_session.agent, "pi");
        assert_eq!(
            agent_session.kind,
            crate::agent_resume::AgentSessionRefKind::Path
        );
        assert_eq!(agent_session.value, session_path);
    }

    #[test]
    fn capture_contract_preserves_restored_agent_session() {
        let mut state = state_with_workspaces(&["one"]);
        let root = state.workspaces[0].tabs[0].root_pane;
        state.ensure_test_terminals();
        let terminal_id = state.workspaces[0].tabs[0].panes[&root]
            .attached_terminal_id
            .clone();
        state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
                source: "herdr:opencode".into(),
                agent: "opencode".into(),
                session_ref: crate::agent_resume::AgentSessionRef::id("opencode-session").unwrap(),
            });

        let snapshot = capture_from_state(&state);
        let agent_session = snapshot.workspaces[0].tabs[0].panes[&root.raw()]
            .agent_session
            .as_ref()
            .expect("persisted agent session should be captured");

        assert_eq!(agent_session.source, "herdr:opencode");
        assert_eq!(agent_session.agent, "opencode");
        assert_eq!(
            agent_session.kind,
            crate::agent_resume::AgentSessionRefKind::Id
        );
        assert_eq!(agent_session.value, "opencode-session");
    }

    #[test]
    fn old_unversioned_snapshot_normalizes_to_current_version() {
        let json = r#"{"workspaces":[],"active":null,"selected":0}"#;
        let snap = parse_snapshot(json).unwrap();
        assert_eq!(snap.version, SNAPSHOT_VERSION);
    }

    #[test]
    fn v4_to_v6_normalization_is_two_pass_and_round_trip_stable() {
        let json = r#"{
            "version":4,
            "workspaces":[{
                "id":"w1",
                "identity_cwd":"/tmp",
                "tabs":[{
                    "layout":{"Split":{"direction":"Horizontal","ratio":0.5,"first":{"Pane":2},"second":{"Pane":1}}},
                    "panes":{"2":{"cwd":"/tmp"},"1":{"cwd":"/tmp"}},
                    "zoomed":false
                }]
            }]
        }"#;
        let (snapshot, changed) = parse_snapshot_with_status(json).unwrap();
        assert!(changed);
        assert_eq!(snapshot.version, SNAPSHOT_VERSION);
        let tab_id = tab_id_for_snapshot(&snapshot.workspaces[0].tabs[0]).unwrap();
        let first =
            terminal_id_for_pane_snapshot(&snapshot.workspaces[0].tabs[0].panes[&1]).unwrap();
        let second =
            terminal_id_for_pane_snapshot(&snapshot.workspaces[0].tabs[0].panes[&2]).unwrap();
        assert_ne!(first, second);

        let encoded = serde_json::to_string(&snapshot).unwrap();
        let (round_trip, changed) = parse_snapshot_with_status(&encoded).unwrap();
        assert!(!changed);
        assert_eq!(
            tab_id_for_snapshot(&round_trip.workspaces[0].tabs[0]).unwrap(),
            tab_id
        );
        assert_eq!(
            terminal_id_for_pane_snapshot(&round_trip.workspaces[0].tabs[0].panes[&1]).unwrap(),
            first
        );
        assert_eq!(
            terminal_id_for_pane_snapshot(&round_trip.workspaces[0].tabs[0].panes[&2]).unwrap(),
            second
        );
    }

    #[test]
    fn v5_normalization_repairs_invalid_and_duplicate_ids_without_dropping_panes() {
        let duplicate = "term_00000000000000000000000000000001";
        let json = format!(
            r#"{{"version":5,"workspaces":[{{"id":"w1","identity_cwd":"/tmp","tabs":[{{"layout":{{"Pane":1}},"panes":{{"1":{{"terminal_id":"{duplicate}","cwd":"/tmp"}},"2":{{"terminal_id":"{duplicate}","cwd":"/tmp"}},"3":{{"terminal_id":"bad","cwd":"/tmp"}}}},"zoomed":false}}]}}]}}"#
        );
        let (snapshot, changed) = parse_snapshot_with_status(&json).unwrap();
        assert!(changed);
        let panes = &snapshot.workspaces[0].tabs[0].panes;
        assert_eq!(panes.len(), 3);
        assert_eq!(
            terminal_id_for_pane_snapshot(&panes[&1]).unwrap().as_str(),
            duplicate
        );
        let ids = panes
            .values()
            .map(terminal_id_for_pane_snapshot)
            .collect::<Result<std::collections::HashSet<_>, _>>()
            .unwrap();
        assert_eq!(ids.len(), 3);
    }

    #[test]
    fn malformed_terminal_id_json_shapes_are_repaired_without_dropping_panes() {
        let oversized = "x".repeat(crate::terminal::MAX_TERMINAL_ID_BYTES + 1);
        for malformed in [
            serde_json::Value::String(oversized),
            serde_json::json!(42),
            serde_json::json!({"unexpected": ["shape"]}),
            serde_json::Value::Null,
        ] {
            let json = serde_json::json!({
                "version": 5,
                "workspaces": [{
                    "id": "w1",
                    "identity_cwd": "/tmp",
                    "tabs": [{
                        "layout": {"Pane": 1},
                        "panes": {"1": {"terminal_id": malformed, "cwd": "/tmp"}},
                        "zoomed": false
                    }]
                }]
            });
            let (snapshot, changed) = parse_snapshot_with_status(&json.to_string()).unwrap();
            assert!(changed);
            assert_eq!(snapshot.workspaces[0].tabs[0].panes.len(), 1);
            terminal_id_for_pane_snapshot(&snapshot.workspaces[0].tabs[0].panes[&1]).unwrap();
        }
    }

    #[test]
    fn identity_normalization_repairs_all_families_in_structural_order_and_rewrites_references() {
        let tab_a = "tab_00000000000000000000000000000001";
        let tab_b = "tab_00000000000000000000000000000002";
        let terminal_a = "term_00000000000000000000000000000001";
        let terminal_b = "term_00000000000000000000000000000002";
        let json = serde_json::json!({
            "version": 5,
            "workspaces": [
                {"id":"w1","identity_cwd":"/a","checkout":{"repository_id":"repo_1","checkout_path":"/a","kind":"primary"},"tabs":[{"id":tab_a,"layout":{"Pane":1},"panes":{"1":{"terminal_id":terminal_a,"cwd":"/a"}},"zoomed":false}]},
                {"id":"w1","identity_cwd":"/b","checkout":{"repository_id":"repo_1","checkout_path":"/b","kind":"linked"},"tabs":[{"id":tab_a,"layout":{"Pane":2},"panes":{"2":{"terminal_id":terminal_a,"cwd":"/b"}},"zoomed":false}]},
                {"id":"w2","identity_cwd":"/c","checkout":{"repository_id":"repo_1","checkout_path":"/c","kind":"linked"},"tabs":[{"id":tab_b,"layout":{"Pane":3},"panes":{"3":{"terminal_id":terminal_b,"cwd":"/c"}},"zoomed":false}]},
                {"id":"malformed","identity_cwd":"/d","checkout":{"repository_id":"repo_1","checkout_path":"/d","kind":"linked"},"tabs":[]}
            ],
            "repositories": [{
                "id":"repo_1","git_common_dir":"/repo/.git","label":"repo",
                "checkout_workspace_ids":["w1","w1","malformed"],
                "last_focused_workspace_id":"malformed"
            }],
            "space_order": [
                {"kind":"repository","id":"repo_1"},
                {"kind":"repository","id":"repo_1"},
                {"kind":"standalone_workspace","id":"w1"},
                {"kind":"standalone_workspace","id":"malformed"}
            ]
        });

        let (snapshot, changed) = parse_snapshot_with_status(&json.to_string()).unwrap();
        assert!(changed);
        assert_eq!(snapshot.version, SNAPSHOT_VERSION);
        assert_eq!(snapshot.workspaces[0].id.as_deref(), Some("w1"));
        assert_eq!(snapshot.workspaces[1].id.as_deref(), Some("w3"));
        assert_eq!(snapshot.workspaces[2].id.as_deref(), Some("w2"));
        assert_eq!(snapshot.workspaces[3].id.as_deref(), Some("w4"));
        assert_eq!(
            snapshot.repositories[0].checkout_workspace_ids,
            ["w1", "w3", "w2", "w4"]
        );
        assert_eq!(
            snapshot.repositories[0]
                .last_focused_workspace_id
                .as_deref(),
            Some("w4")
        );
        assert_eq!(
            snapshot.space_order,
            [crate::repository::SpaceRef::Repository("repo_1".into())]
        );

        assert_eq!(
            tab_id_for_snapshot(&snapshot.workspaces[0].tabs[0])
                .unwrap()
                .as_str(),
            tab_a
        );
        assert_eq!(
            tab_id_for_snapshot(&snapshot.workspaces[2].tabs[0])
                .unwrap()
                .as_str(),
            tab_b
        );
        let tab_ids = snapshot
            .workspaces
            .iter()
            .flat_map(|workspace| &workspace.tabs)
            .map(|tab| tab_id_for_snapshot(tab).unwrap())
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(tab_ids.len(), 3);
        let terminal_ids = snapshot
            .workspaces
            .iter()
            .flat_map(|workspace| &workspace.tabs)
            .flat_map(|tab| tab.panes.values())
            .map(|pane| terminal_id_for_pane_snapshot(pane).unwrap())
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(terminal_ids.len(), 3);

        let persisted = serde_json::to_string(&snapshot).unwrap();
        let restored = parse_snapshot(&persisted).unwrap();
        assert_eq!(restored.repositories, snapshot.repositories);
        assert_eq!(restored.space_order, snapshot.space_order);
    }

    #[test]
    fn public_tab_numbers_are_repaired_deterministically_and_v6_is_idempotent() {
        let tabs = (1..=4)
            .map(|number| {
                serde_json::json!({
                    "id": format!("tab_{number:032x}"),
                    "layout": {"Pane": number},
                    "panes": {
                        number.to_string(): {
                            "terminal_id": format!("term_{number:032x}"),
                            "cwd": "/tmp"
                        }
                    },
                    "zoomed": false
                })
            })
            .collect::<Vec<_>>();
        let json = serde_json::json!({
            "version": 6,
            "workspaces": [{
                "id":"w1",
                "identity_cwd":"/tmp",
                "public_tab_numbers":[7, 0, 7],
                "next_public_tab_number":2,
                "tabs":tabs
            }],
            "repositories":[],
            "space_order":[{"kind":"standalone_workspace","id":"w1"}],
            "selected":0
        });

        let (snapshot, changed) = parse_snapshot_with_status(&json.to_string()).unwrap();
        assert!(changed);
        assert_eq!(snapshot.workspaces[0].public_tab_numbers, [7, 1, 2, 3]);
        assert_eq!(snapshot.workspaces[0].next_public_tab_number, 8);
        let public_ids = snapshot.workspaces[0]
            .public_tab_numbers
            .iter()
            .map(|number| crate::workspace::public_tab_id_for_number("w1", *number))
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(public_ids.len(), 4);

        let persisted = serde_json::to_string(&snapshot).unwrap();
        let (round_trip, changed) = parse_snapshot_with_status(&persisted).unwrap();
        assert!(!changed);
        assert_eq!(
            round_trip.workspaces[0].public_tab_numbers,
            snapshot.workspaces[0].public_tab_numbers
        );
        assert_eq!(
            round_trip.workspaces[0].next_public_tab_number,
            snapshot.workspaces[0].next_public_tab_number
        );
    }

    #[test]
    fn v6_reserved_public_tab_boundary_round_trips_without_rewrite() {
        let json = serde_json::json!({
            "version": 6,
            "workspaces": [{
                "id":"w1",
                "identity_cwd":"/tmp",
                "public_tab_numbers":[1],
                "next_public_tab_number": usize::MAX - 1,
                "tabs":[{
                    "id":"tab_00000000000000000000000000000001",
                    "layout":{"Pane":1},
                    "panes":{"1":{"terminal_id":"term_00000000000000000000000000000001","cwd":"/tmp"}},
                    "zoomed":false
                }]
            }],
            "space_order":[{"kind":"standalone_workspace","id":"w1"}],
            "selected":0
        });
        let (snapshot, changed) = parse_snapshot_with_status(&json.to_string()).unwrap();
        assert!(changed, "legacy schema must roll forward");
        assert_eq!(snapshot.version, SNAPSHOT_VERSION);
        assert_eq!(
            snapshot.workspaces[0].tabs[0]
                .id
                .as_ref()
                .unwrap()
                .validated()
                .unwrap()
                .as_str(),
            json["workspaces"][0]["tabs"][0]["id"].as_str().unwrap()
        );
        assert_eq!(
            snapshot.workspaces[0].next_public_tab_number,
            usize::MAX - 1
        );
        let persisted = serde_json::to_string(&snapshot).unwrap();
        let (_, changed) = parse_snapshot_with_status(&persisted).unwrap();
        assert!(!changed);
    }

    #[test]
    fn tab_identity_allocation_failure_rejects_normalization_before_admission() {
        let mut snapshot: SessionSnapshot = serde_json::from_value(serde_json::json!({
            "version": 5,
            "selected": 0,
            "workspaces": [{
                "id":"w1",
                "identity_cwd":"/tmp",
                "tabs":[{"layout":{"Pane":1},"panes":{"1":{"cwd":"/tmp"}},"zoomed":false}]
            }]
        }))
        .unwrap();
        let error = normalize_identities_with(
            &mut snapshot,
            false,
            || Err(std::io::Error::other("injected tab entropy failure")),
            crate::terminal::TerminalId::alloc,
        )
        .unwrap_err();
        assert!(error.contains("injected tab entropy failure"));
        assert_eq!(snapshot.version, 5);
    }

    #[test]
    fn malformed_tab_and_workspace_json_shapes_are_bounded_and_repaired() {
        let oversized_tab = "x".repeat(crate::workspace::MAX_TAB_ID_BYTES + 1);
        let oversized_workspace = "x".repeat(MAX_WORKSPACE_ID_BYTES + 1);
        for (tab_id, workspace_id) in [
            (
                serde_json::Value::String(oversized_tab),
                serde_json::json!(42),
            ),
            (
                serde_json::json!({"bad": true}),
                serde_json::Value::String(oversized_workspace),
            ),
            (serde_json::Value::Null, serde_json::Value::Null),
        ] {
            let json = serde_json::json!({
                "version": 6,
                "workspaces": [{
                    "id": workspace_id,
                    "identity_cwd":"/tmp",
                    "tabs":[{"id":tab_id,"layout":{"Pane":1},"panes":{"1":{"cwd":"/tmp"}},"zoomed":false}]
                }]
            });
            let (snapshot, changed) = parse_snapshot_with_status(&json.to_string()).unwrap();
            assert!(changed);
            let workspace_id = snapshot.workspaces[0].id.as_deref().unwrap();
            assert!(canonical_workspace_id(workspace_id));
            assert_eq!(
                snapshot.space_order,
                [crate::repository::SpaceRef::StandaloneWorkspace(
                    workspace_id.into()
                )]
            );
            tab_id_for_snapshot(&snapshot.workspaces[0].tabs[0]).unwrap();
        }
    }

    #[test]
    fn identity_normalization_is_independent_of_hashmap_insertion_order() {
        fn normalize(json: &str) -> Vec<(u32, String)> {
            let mut snapshot: SessionSnapshot = serde_json::from_str(json).unwrap();
            let generated = [
                "term_00000000000000000000000000000011",
                "term_00000000000000000000000000000012",
                "term_00000000000000000000000000000013",
            ];
            let mut generated = generated
                .into_iter()
                .map(|raw| crate::terminal::TerminalId::parse(raw).unwrap());
            let mut generated_tabs = ["tab_00000000000000000000000000000011"]
                .into_iter()
                .map(|raw| crate::workspace::TabId::parse(raw).unwrap());
            normalize_identities_with(
                &mut snapshot,
                false,
                || Ok(generated_tabs.next().unwrap()),
                || Ok(generated.next().unwrap()),
            )
            .unwrap();
            let mut ids = snapshot.workspaces[0].tabs[0]
                .panes
                .iter()
                .map(|(&pane_id, pane)| {
                    (
                        pane_id,
                        terminal_id_for_pane_snapshot(pane).unwrap().to_string(),
                    )
                })
                .collect::<Vec<_>>();
            ids.sort_unstable();
            ids
        }
        let prefix = r#"{"version":4,"workspaces":[{"id":"w1","identity_cwd":"/tmp","tabs":[{"layout":{"Pane":1},"panes":{"#;
        let suffix = r#"},"zoomed":false}]}],"active":null,"selected":0}"#;
        let ascending = format!(
            "{prefix}{}{}",
            r#""1":{"cwd":"/tmp"},"2":{"cwd":"/tmp"},"3":{"cwd":"/tmp"}"#, suffix
        );
        let descending = format!(
            "{prefix}{}{}",
            r#""3":{"cwd":"/tmp"},"2":{"cwd":"/tmp"},"1":{"cwd":"/tmp"}"#, suffix
        );

        assert_eq!(normalize(&ascending), normalize(&descending));
    }

    #[test]
    fn pane_history_format_remains_explicitly_version_four() {
        let history = capture_history(&[], &TerminalRuntimeRegistry::new());
        assert_eq!(history.version, HISTORY_SNAPSHOT_VERSION);
        assert_eq!(history.version, 4);
    }

    #[test]
    fn package_authority_requires_and_round_trips_snapshot_v7() {
        let missing = serde_json::json!({
            "version": 7,
            "workspaces": [],
            "selected": 0
        });
        let Err(error) = parse_snapshot(&missing.to_string()) else {
            panic!("version 7 without authority was accepted");
        };
        assert!(error.contains("requires Collections authority"));

        let mut snapshot = capture(
            &[],
            &[],
            &[],
            &std::collections::HashMap::new(),
            &crate::terminal::TerminalRuntimeRegistry::new(),
            None,
            0,
        );
        snapshot.version = 7;
        snapshot.collections_authority =
            crate::collections_extension_contract::CollectionsAuthorityV1 {
                contract: crate::collections_extension_contract::COLLECTIONS_AUTHORITY_CONTRACT_V1
                    .into(),
                phase: crate::collections_extension_contract::CollectionsAuthorityPhaseV1::Package,
                authority_epoch: 2,
                package: Some(crate::collections_extension_contract::CollectionsAuthorityPackageV1 {
                    package_id: crate::collections_extension_contract::COLLECTIONS_PACKAGE_ID.into(),
                    service_id: "collections".into(),
                    package_sha256: "a".repeat(64),
                    component_sha256: "b".repeat(64),
                    profile_contract: crate::collections_extension_contract::COLLECTIONS_DOCUMENT_PROFILE_CONTRACT_V1.into(),
                }),
                state: Some(crate::collections_extension_contract::CollectionsAuthorityStateV1 {
                    store_generation: 2,
                    namespace_revision: 3,
                }),
                migration_id: Some("collections_r3_test".into()),
                minimum_reader_version: 7,
            };
        snapshot.collections_action_sequence = 9;
        let encoded = serde_json::to_string(&snapshot).unwrap();
        let restored = parse_snapshot(&encoded).unwrap();
        assert_eq!(restored.version, SNAPSHOT_VERSION);
        assert_eq!(
            restored.collections_authority,
            snapshot.collections_authority
        );
        assert_eq!(restored.collections_action_sequence, 9);

        let mut prior_v7: serde_json::Value = serde_json::from_str(&encoded).unwrap();
        prior_v7
            .as_object_mut()
            .unwrap()
            .remove("collections_action_sequence");
        assert_eq!(
            parse_snapshot(&prior_v7.to_string())
                .unwrap()
                .collections_action_sequence,
            0,
            "snapshot-v7 images written before the high-water field must remain readable"
        );
    }

    #[test]
    fn future_version_is_rejected() {
        let json = r#"{"version":999,"workspaces":[],"active":null,"selected":0}"#;
        assert!(parse_snapshot(json).is_err());
    }

    #[test]
    fn active_tab_default_is_zero() {
        let json = r#"{"custom_name":"test","identity_cwd":"/tmp","tabs":[]}"#;
        let ws: WorkspaceSnapshot = serde_json::from_str(json).unwrap();
        assert_eq!(ws.active_tab, 0);
    }

    #[test]
    fn adversarial_identity_capture_preserves_tab_order_public_numbers_and_hidden_members() {
        let mut state = AppState::test_with_adversarial_identity_state();
        let workspace = &mut state.workspaces[0];
        let tab = workspace
            .tabs
            .iter_mut()
            .find(|tab| tab.panes.len() > 1)
            .unwrap();
        let root = tab.root_pane;
        let mut members: Vec<_> = tab.panes.keys().copied().filter(|id| *id != root).collect();
        members.sort_by_key(|id| id.raw());
        let collection = tab
            .create_collection_near(
                crate::layout::LayoutLeaf::Pane(root),
                Direction::Vertical,
                0.35,
                Some("hidden identities".into()),
            )
            .unwrap();
        for member in &members {
            tab.collect_tiled_pane(*member, collection).unwrap();
        }
        tab.select_collection_member(collection, members[1])
            .unwrap();
        tab.set_collection_member_archived(collection, members[0], true)
            .unwrap();
        tab.layout
            .focus_leaf(crate::layout::LayoutLeaf::Stack(collection.into()));
        tab.zoomed = true;
        state.assert_invariants_for_test();

        let snapshot = capture_from_state(&state);
        let decoded = parse_snapshot(&serde_json::to_string(&snapshot).unwrap()).unwrap();
        let saved = &decoded.workspaces[0];
        let workspace = &state.workspaces[0];
        assert_eq!(saved.id.as_deref(), Some(workspace.id.as_str()));
        assert_eq!(saved.active_tab, workspace.active_tab);
        assert_eq!(
            saved.public_tab_numbers,
            workspace
                .tabs
                .iter()
                .map(|tab| tab.number)
                .collect::<Vec<_>>()
        );
        assert_eq!(
            saved.next_public_tab_number,
            workspace.next_public_tab_number
        );
        assert_eq!(
            saved.next_public_pane_number,
            workspace.next_public_pane_number
        );
        assert_eq!(
            saved.public_pane_numbers,
            workspace
                .public_pane_numbers
                .iter()
                .map(|(id, number)| (id.raw(), *number))
                .collect()
        );
        for (saved_tab, tab) in saved.tabs.iter().zip(&workspace.tabs) {
            assert_eq!(saved_tab.id.as_ref().unwrap().validated().unwrap(), tab.id);
            assert_eq!(saved_tab.root_pane, Some(tab.root_pane.raw()));
            assert_eq!(saved_tab.focused, Some(tab.layout.focused().raw()));
            assert_eq!(saved_tab.zoomed, tab.zoomed);
            assert_eq!(saved_tab.panes.len(), tab.panes.len());
            for (id, pane) in &tab.panes {
                assert_eq!(
                    terminal_id_for_pane_snapshot(&saved_tab.panes[&id.raw()]).unwrap(),
                    pane.attached_terminal_id
                );
            }
        }
        let saved_collection = saved
            .tabs
            .iter()
            .flat_map(|tab| &tab.collections)
            .find(|saved| saved.id == collection)
            .unwrap();
        let saved_stack = saved
            .tabs
            .iter()
            .find_map(|tab| stack_in_snapshot(&tab.layout, collection))
            .unwrap();
        assert_eq!(saved_stack.0, 1);
        assert_eq!(
            serde_json::to_value(saved_stack.1).unwrap(),
            serde_json::json!([{"Pane": members[0].raw()}, {"Pane": members[1].raw()}])
        );
        assert_eq!(saved_collection.archived, vec![members[0].raw()]);
        assert!(
            !normalize_identity_changes(&mut decoded.clone(), false)
                .unwrap()
                .changed
        );
        state.assert_invariants_for_test();
    }

    #[test]
    fn collection_saved_layout_round_trip_preserves_ids_members_selection_archive_and_focus() {
        let mut state = state_with_workspaces(&["collections"]);
        let root = state.workspaces[0].tabs[0].root_pane;
        let first = state.workspaces[0].test_split(Direction::Horizontal);
        let second = state.workspaces[0].test_split(Direction::Vertical);
        let collection = state.workspaces[0].tabs[0]
            .create_collection_near(
                crate::layout::LayoutLeaf::Pane(root),
                Direction::Horizontal,
                0.5,
                Some("helpers".into()),
            )
            .expect("collection");
        for pane in [first, second] {
            state.workspaces[0].tabs[0]
                .collect_tiled_pane(pane, collection)
                .unwrap();
        }
        state.workspaces[0].tabs[0]
            .select_collection_member(collection, second)
            .unwrap();
        state.workspaces[0].tabs[0]
            .set_collection_member_archived(collection, first, true)
            .unwrap();
        assert!(state.workspaces[0].tabs[0]
            .layout
            .focus_leaf(crate::layout::LayoutLeaf::Stack(collection.into())));

        let snapshot = capture_from_state(&state);
        let json = serde_json::to_string(&snapshot).expect("serialize");
        let shape: serde_json::Value = serde_json::from_str(&json).expect("snapshot json");
        assert_eq!(shape["version"], SNAPSHOT_VERSION);
        assert!(shape["workspaces"][0]["tabs"][0]
            .get("collections")
            .is_some());
        assert!(shape["workspaces"][0]["tabs"][0]
            .get("containers")
            .is_none());
        let decoded = parse_snapshot(&json).expect("deserialize");
        let tab = &decoded.workspaces[0].tabs[0];
        let saved = &tab.collections[0];
        assert_eq!(saved.id, collection);
        assert_eq!(saved.label.as_deref(), Some("helpers"));
        let (selected, children) = stack_in_snapshot(&tab.layout, collection).unwrap();
        assert_eq!(selected, 1);
        assert_eq!(
            serde_json::to_value(children).unwrap(),
            serde_json::json!([{"Pane": first.raw()}, {"Pane": second.raw()}])
        );
        assert_eq!(saved.archived, vec![first.raw()]);
        assert_eq!(
            tab.focused_leaf,
            Some(LayoutLeafSnapshot::Stack(collection))
        );
        assert!(matches!(
            tab.layout,
            LayoutSnapshot::Split { ref second, .. }
                if matches!(second.as_ref(), LayoutSnapshot::Stack { id, .. } if *id == collection)
        ));
    }

    fn stack_in_snapshot(
        node: &LayoutSnapshot,
        target: crate::layout::CollectionId,
    ) -> Option<(usize, &[LayoutSnapshot])> {
        match node {
            LayoutSnapshot::Stack {
                id,
                selected,
                children,
            } if *id == target => Some((*selected, children)),
            LayoutSnapshot::Stack { children, .. } => children
                .iter()
                .find_map(|child| stack_in_snapshot(child, target)),
            LayoutSnapshot::Split { first, second, .. } => {
                stack_in_snapshot(first, target).or_else(|| stack_in_snapshot(second, target))
            }
            LayoutSnapshot::Pane(_) => None,
        }
    }

    #[test]
    fn v8_capture_round_trip_is_total_for_nested_split_stack_and_empty_children() {
        use crate::layout::{LayoutLeaf, TileLayout, TypedNode};
        let mut state = state_with_workspaces(&["nested"]);
        let root = state.workspaces[0].tabs[0].root_pane;
        let member = state.workspaces[0].test_split(Direction::Horizontal);
        let outside = state.workspaces[0].test_split(Direction::Vertical);
        state.ensure_test_terminals();
        let outer = crate::layout::CollectionId::from_raw(7_500_001).unwrap();
        let inner = crate::layout::CollectionId::from_raw(7_500_002).unwrap();
        let empty = crate::layout::CollectionId::from_raw(7_500_003).unwrap();
        let tree = TypedNode::Stack {
            id: outer.into(),
            selected: 1,
            children: vec![
                TypedNode::Leaf(root),
                TypedNode::Split {
                    direction: Direction::Vertical,
                    ratio: 0.37,
                    first: Box::new(TypedNode::Stack {
                        id: inner.into(),
                        selected: 1,
                        children: vec![
                            TypedNode::empty_stack(empty.into()),
                            TypedNode::Leaf(member),
                        ],
                    }),
                    second: Box::new(TypedNode::Leaf(outside)),
                },
            ],
        };
        state.workspaces[0].tabs[0].layout =
            TileLayout::from_tree(tree, LayoutLeaf::Stack(outer.into())).unwrap();
        assert!(!state.workspaces[0].tabs[0].layout.is_pane_visible(root));
        let captured = capture_from_state(&state);
        let expected = serde_json::json!({"Stack":{"id":outer,"selected":1,"children":[
            {"Pane":root.raw()},
            {"Split":{"direction":"Vertical","ratio":0.37_f32,"first":{
                "Stack":{"id":inner,"selected":1,"children":[{"Stack":{"id":empty,"selected":0,"children":[]}},{"Pane":member.raw()}]}
            },"second":{"Pane":outside.raw()}}}
        ]}});
        assert_eq!(
            serde_json::to_value(&captured.workspaces[0].tabs[0].layout).unwrap(),
            expected
        );
        let decoded = parse_snapshot(&serde_json::to_string(&captured).unwrap()).unwrap();
        assert_eq!(
            serde_json::to_value(&decoded.workspaces[0].tabs[0].layout).unwrap(),
            expected
        );
        assert_eq!(
            decoded.workspaces[0].tabs[0].focused_leaf,
            Some(LayoutLeafSnapshot::Stack(outer))
        );
        assert_eq!(decoded.workspaces[0].tabs[0].panes.len(), 3);
        state.assert_invariants_for_test();
    }

    #[test]
    fn frozen_v4_collection_writer_shape_converts_to_canonical_tree() {
        let snapshot = parse_snapshot(include_str!(
            "../../tests/fixtures/session/collection-layout-v4.json"
        ))
        .unwrap();
        assert_eq!(snapshot.version, SNAPSHOT_VERSION);
        let tab = &snapshot.workspaces[0].tabs[0];
        let policy = &tab.collections[0];
        assert_eq!(policy.id.raw(), 7_300_001);
        assert_eq!(policy.label.as_deref(), Some("helpers"));
        assert_eq!(policy.archived, vec![11]);
        let (selected, children) = stack_in_snapshot(&tab.layout, policy.id).unwrap();
        assert_eq!(selected, 1);
        assert_eq!(
            serde_json::to_value(children).unwrap(),
            serde_json::json!([{"Pane":11},{"Pane":12}])
        );
        assert_eq!(tab.focused_leaf, Some(LayoutLeafSnapshot::Stack(policy.id)));
    }

    #[test]
    fn restore_falls_back_to_home_when_cwd_missing() {
        let mut panes = HashMap::new();
        panes.insert(
            0,
            PaneSnapshot {
                terminal_id: None,
                cwd: PathBuf::from("/tmp/this-directory-does-not-exist-for-herdr-test"),
                label: None,
                agent_name: None,
                managed_agent_kind: None,
                agent_session: None,
                launch_argv: None,
            },
        );
        panes.insert(
            1,
            PaneSnapshot {
                terminal_id: None,
                cwd: std::env::var("HOME")
                    .map(PathBuf::from)
                    .unwrap_or_else(|_| PathBuf::from("/tmp")),
                label: None,
                agent_name: None,
                managed_agent_kind: None,
                agent_session: None,
                launch_argv: None,
            },
        );

        let snap = SessionSnapshot {
            delegations: Vec::new(),
            collections_authority: Default::default(),
            collections_action_sequence: 0,
            version: SNAPSHOT_VERSION,
            repositories: Vec::new(),
            space_order: Vec::new(),
            workspaces: vec![WorkspaceSnapshot {
                id: Some("test-ws".to_string()),
                custom_name: Some("fallback test".to_string()),
                identity_cwd: PathBuf::from("/tmp"),
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
                        direction: DirectionSnapshot::Horizontal,
                        ratio: 0.5,
                        first: Box::new(LayoutSnapshot::Pane(0)),
                        second: Box::new(LayoutSnapshot::Pane(1)),
                    },
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
            collapsed_space_keys: std::collections::HashSet::new(),
        };

        let json = serde_json::to_string(&snap).unwrap();
        let restored = parse_snapshot(&json).unwrap();
        assert_eq!(restored.workspaces.len(), 1);
        assert_eq!(
            restored.workspaces[0].tabs[0].panes[&0].cwd,
            PathBuf::from("/tmp/this-directory-does-not-exist-for-herdr-test")
        );
    }
}
