use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use ratatui::layout::Direction;
use tokio::sync::{mpsc, Notify};

use crate::events::AppEvent;
#[cfg(test)]
use crate::layout::TypedNode;
use crate::layout::{CollectionId, LayoutLeaf, PaneId, PanePlacement, TileLayout};
use crate::legacy_collection_snapshot::LegacyCollectionSnapshot;
use crate::pane::{PaneLaunchEnv, PaneState};
use crate::render_signal::RenderSignal;
use crate::terminal::{TerminalId, TerminalRuntime, TerminalRuntimeRegistry, TerminalState};
use crate::terminal_container::TerminalContainerId;

pub(crate) type DetachedPane = (PaneId, TerminalId);

pub(crate) struct MovedPane {
    pub pane_id: PaneId,
    pub pane_state: PaneState,
}

pub struct NewPane {
    pub pane_id: PaneId,
    pub terminal: TerminalState,
    pub runtime: TerminalRuntime,
}

pub(crate) struct PaneCollection<'a> {
    container: crate::layout::StackRef<'a>,
    legacy_snapshot: Option<&'a LegacyCollectionSnapshot>,
}

impl PaneCollection<'_> {
    pub(crate) fn id(&self) -> CollectionId {
        self.container.id.into()
    }

    pub(crate) fn legacy_label(&self) -> Option<&str> {
        self.legacy_snapshot
            .and_then(|snapshot| snapshot.label.as_deref())
    }

    pub(crate) fn members(&self) -> Vec<PaneId> {
        self.container.members()
    }

    pub(crate) fn selected(&self) -> Option<PaneId> {
        self.container.selected()
    }

    pub(crate) fn member_count(&self) -> usize {
        self.container.member_count()
    }

    pub(crate) fn contains_member(&self, pane: PaneId) -> bool {
        self.container.contains_member(pane)
    }

    pub(crate) fn legacy_is_archived(&self, pane_id: PaneId) -> bool {
        self.legacy_snapshot
            .is_some_and(|snapshot| snapshot.is_archived(pane_id))
    }

    pub(crate) fn legacy_archived_members(&self) -> impl Iterator<Item = PaneId> + '_ {
        self.container
            .members()
            .into_iter()
            .filter(|pane| self.legacy_is_archived(*pane))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CollectionMutationError {
    CollectionNotFound,
    CollectionNotEmpty,
    PaneNotFound,
    PaneNotTiled,
    PaneNotMember,
    TargetNotTiled,
    LastLayoutLeaf,
    LayoutMutationFailed,
}

enum SplitCommand<'a> {
    Shell {
        command: &'a str,
        launch_env: &'a PaneLaunchEnv,
    },
    Argv {
        argv: &'a [String],
        launch_env: &'a PaneLaunchEnv,
    },
}

pub struct Tab {
    pub(crate) id: super::TabId,
    pub custom_name: Option<String>,
    pub number: usize,
    /// Identity source for this tab's pane tree.
    pub root_pane: PaneId,
    pub layout: TileLayout,
    pub(super) legacy_collection_snapshots: HashMap<TerminalContainerId, LegacyCollectionSnapshot>,
    /// Pane viewport state — always present, testable without PTYs.
    pub panes: HashMap<PaneId, PaneState>,
    #[cfg(test)]
    pub runtimes: HashMap<PaneId, TerminalRuntime>,
    pub zoomed: bool,
    pub events: mpsc::Sender<AppEvent>,
    pub(crate) render_notify: Arc<Notify>,
    pub(crate) render_dirty: Arc<RenderSignal>,
}

impl Tab {
    // Tab construction threads pane runtime geometry, host context, and render hooks.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: super::TabId,
        number: usize,
        terminal_id: TerminalId,
        initial_cwd: PathBuf,
        rows: u16,
        cols: u16,
        scrollback_limit_bytes: usize,
        host_terminal_theme: crate::terminal_theme::TerminalTheme,
        host_terminal_appearance: Option<crate::terminal_theme::HostAppearance>,
        shell_config: crate::pane::PaneShellConfig<'_>,
        launch_env: &PaneLaunchEnv,
        events: mpsc::Sender<AppEvent>,
        render_notify: Arc<Notify>,
        render_dirty: Arc<RenderSignal>,
    ) -> std::io::Result<(Self, TerminalState, TerminalRuntime)> {
        Self::new_with_runtime(
            id,
            number,
            initial_cwd,
            rows,
            cols,
            scrollback_limit_bytes,
            host_terminal_theme,
            host_terminal_appearance,
            shell_config,
            launch_env,
            events,
            render_notify,
            render_dirty,
            terminal_id,
            None,
        )
    }

    // Command tab construction mirrors the shell tab runtime arguments.
    #[allow(clippy::too_many_arguments)]
    pub fn new_argv_command(
        id: super::TabId,
        number: usize,
        terminal_id: TerminalId,
        initial_cwd: PathBuf,
        rows: u16,
        cols: u16,
        argv: &[String],
        scrollback_limit_bytes: usize,
        host_terminal_theme: crate::terminal_theme::TerminalTheme,
        host_terminal_appearance: Option<crate::terminal_theme::HostAppearance>,
        launch_env: &PaneLaunchEnv,
        events: mpsc::Sender<AppEvent>,
        render_notify: Arc<Notify>,
        render_dirty: Arc<RenderSignal>,
    ) -> std::io::Result<(Self, TerminalState, TerminalRuntime)> {
        Self::new_with_runtime(
            id,
            number,
            initial_cwd,
            rows,
            cols,
            scrollback_limit_bytes,
            host_terminal_theme,
            host_terminal_appearance,
            crate::pane::PaneShellConfig::new("", crate::config::ShellModeConfig::NonLogin),
            launch_env,
            events,
            render_notify,
            render_dirty,
            terminal_id,
            Some(argv),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn new_with_runtime(
        id: super::TabId,
        number: usize,
        initial_cwd: PathBuf,
        rows: u16,
        cols: u16,
        scrollback_limit_bytes: usize,
        host_terminal_theme: crate::terminal_theme::TerminalTheme,
        host_terminal_appearance: Option<crate::terminal_theme::HostAppearance>,
        shell_config: crate::pane::PaneShellConfig<'_>,
        launch_env: &PaneLaunchEnv,
        events: mpsc::Sender<AppEvent>,
        render_notify: Arc<Notify>,
        render_dirty: Arc<RenderSignal>,
        terminal_id: TerminalId,
        argv: Option<&[String]>,
    ) -> std::io::Result<(Self, TerminalState, TerminalRuntime)> {
        let (layout, root_id) = TileLayout::new();
        let runtime = if let Some(argv) = argv {
            TerminalRuntime::spawn_argv_command(
                root_id,
                rows,
                cols,
                initial_cwd.clone(),
                argv,
                launch_env,
                crate::pane::AgentDetection::Enabled,
                scrollback_limit_bytes,
                host_terminal_theme,
                host_terminal_appearance,
                events.clone(),
                render_notify.clone(),
                render_dirty.clone(),
            )?
        } else {
            TerminalRuntime::spawn(
                root_id,
                rows,
                cols,
                initial_cwd.clone(),
                scrollback_limit_bytes,
                host_terminal_theme,
                host_terminal_appearance,
                shell_config,
                launch_env,
                events.clone(),
                render_notify.clone(),
                render_dirty.clone(),
            )?
        };

        let terminal = match argv {
            Some(argv) => {
                TerminalState::new(terminal_id.clone(), initial_cwd).with_launch_argv(argv.to_vec())
            }
            None => TerminalState::new(terminal_id.clone(), initial_cwd),
        };
        let mut panes = HashMap::new();
        panes.insert(root_id, PaneState::new(terminal_id));

        Ok((
            Self {
                id,
                custom_name: None,
                number,
                root_pane: root_id,
                layout,
                legacy_collection_snapshots: HashMap::new(),
                panes,
                #[cfg(test)]
                runtimes: HashMap::new(),
                zoomed: false,
                events,
                render_notify,
                render_dirty,
            },
            terminal,
            runtime,
        ))
    }

    pub fn pane_placement(&self, pane_id: PaneId) -> Option<PanePlacement> {
        self.panes
            .contains_key(&pane_id)
            .then(|| self.layout.placement(pane_id))
            .flatten()
    }

    pub(crate) fn collection(&self, collection_id: CollectionId) -> Option<PaneCollection<'_>> {
        let container_id = TerminalContainerId::from(collection_id);
        Some(PaneCollection {
            container: self.layout.collection(container_id)?,
            legacy_snapshot: self.legacy_collection_snapshots.get(&container_id),
        })
    }

    pub(crate) fn collections(&self) -> impl Iterator<Item = PaneCollection<'_>> {
        self.layout.collections().map(|container| PaneCollection {
            legacy_snapshot: self.legacy_collection_snapshots.get(&container.id),
            container,
        })
    }

    #[cfg(test)]
    pub(super) fn assert_collection_invariants_for_test(&self) {
        assert!(
            self.layout.typed_root().valid_structure(),
            "canonical tree ownership and selection must be valid"
        );
        let layout_ids: HashSet<_> = self.layout.collection_ids().into_iter().collect();
        let policy_ids: HashSet<_> = self.legacy_collection_snapshots.keys().copied().collect();
        assert!(
            policy_ids.is_subset(&layout_ids),
            "legacy snapshots may refer only to live neutral containers"
        );
        for container_id in layout_ids {
            let container = self
                .layout
                .collection(container_id)
                .expect("layout ID must resolve to a container");
            let policy = self.legacy_collection_snapshots.get(&container_id);
            let members: HashSet<_> = container.members().iter().copied().collect();
            assert_eq!(members.len(), container.members().len());
            if policy.is_some() {
                assert_eq!(
                    container.selected().is_some(),
                    !members.is_empty(),
                    "legacy flat stacks select a pane if and only if non-empty"
                );
            }
            assert!(container
                .selected()
                .is_none_or(|pane| members.contains(&pane)));
            assert!(
                policy.is_none_or(|snapshot| snapshot.archived().is_subset(&members)),
                "raw archived state must be a subset of container members"
            );
        }
    }

    pub(crate) fn focus_layout_leaf(&mut self, leaf: LayoutLeaf) -> bool {
        self.layout.focus_leaf(leaf)
    }

    pub(crate) fn focus_pane(&mut self, pane_id: PaneId) {
        self.layout.focus_pane(pane_id);
    }

    pub(crate) fn swap_panes(&mut self, first: PaneId, second: PaneId) -> bool {
        let mut layout = self.layout.clone();
        if !layout.swap_panes(first, second) {
            return false;
        }
        self.layout = layout;
        true
    }

    #[cfg(test)]
    pub(crate) fn resize_focused(
        &mut self,
        direction: crate::layout::NavDirection,
        delta: f32,
        area: ratatui::layout::Rect,
    ) {
        let mut layout = self.layout.clone();
        layout.resize_focused(direction, delta, area);
        self.layout = layout;
    }

    pub(crate) fn resize_pane(
        &mut self,
        pane_id: PaneId,
        direction: crate::layout::NavDirection,
        delta: f32,
        area: ratatui::layout::Rect,
    ) -> bool {
        let mut layout = self.layout.clone();
        if !layout.resize_pane(pane_id, direction, delta, area) {
            return false;
        }
        self.layout = layout;
        true
    }

    pub(crate) fn set_layout_split_ratio(&mut self, path: &[bool], ratio: f32) -> bool {
        let mut layout = self.layout.clone();
        if !layout.set_ratio_at(path, ratio) {
            return false;
        }
        self.layout = layout;
        true
    }

    pub(crate) fn replace_layout_without_collections(
        &mut self,
        layout: TileLayout,
        root_pane: PaneId,
    ) {
        self.layout = layout;
        self.legacy_collection_snapshots.clear();
        self.root_pane = root_pane;
        self.zoomed = false;
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn from_restored(
        id: super::TabId,
        custom_name: Option<String>,
        number: usize,
        root_pane: PaneId,
        layout: TileLayout,
        policies: HashMap<TerminalContainerId, LegacyCollectionSnapshot>,
        panes: HashMap<PaneId, PaneState>,
        zoomed: bool,
        events: mpsc::Sender<AppEvent>,
        render_notify: Arc<Notify>,
        render_dirty: Arc<RenderSignal>,
    ) -> Option<Self> {
        let ids: HashSet<_> = layout.collection_ids().into_iter().collect();
        if policies.len() != ids.len() || !policies.keys().all(|id| ids.contains(id)) {
            return None;
        }
        for id in ids {
            let container = layout.collection(id)?;
            let policy = policies.get(&id)?;
            let members: HashSet<_> = container.members().iter().copied().collect();
            let selection_presence_is_valid = match (members.is_empty(), container.selected()) {
                (true, None) | (false, Some(_)) => true,
                (true, Some(_)) | (false, None) => false,
            };
            if members.len() != container.members().len()
                || !selection_presence_is_valid
                || container
                    .selected()
                    .is_some_and(|pane| !members.contains(&pane))
                || !policy.archived().is_subset(&members)
            {
                return None;
            }
        }
        Some(Self {
            id,
            custom_name,
            number,
            root_pane,
            layout,
            legacy_collection_snapshots: policies,
            panes,
            #[cfg(test)]
            runtimes: HashMap::new(),
            zoomed,
            events,
            render_notify,
            render_dirty,
        })
    }

    #[cfg(test)]
    pub fn create_collection_near(
        &mut self,
        target: LayoutLeaf,
        direction: Direction,
        ratio: f32,
        label: Option<String>,
    ) -> Result<CollectionId, CollectionMutationError> {
        let collection_id =
            CollectionId::alloc().map_err(|_| CollectionMutationError::LayoutMutationFailed)?;
        let created =
            self.create_collection_near_reserved(target, direction, ratio, collection_id)?;
        self.legacy_collection_snapshots.insert(
            TerminalContainerId::from(created),
            LegacyCollectionSnapshot::from_legacy(label, HashSet::new()),
        );
        Ok(created)
    }

    pub(crate) fn create_collection_near_reserved(
        &mut self,
        target: LayoutLeaf,
        direction: Direction,
        ratio: f32,
        collection_id: CollectionId,
    ) -> Result<CollectionId, CollectionMutationError> {
        // Preflight before publishing the caller-reserved identity.
        if !self.layout.can_insert_collection_near(target) {
            return Err(CollectionMutationError::LayoutMutationFailed);
        }
        let container_id = TerminalContainerId::from(collection_id);
        let mut layout = self.layout.clone();
        if !layout.insert_collection_near(target, container_id, direction, ratio) {
            return Err(CollectionMutationError::LayoutMutationFailed);
        }
        self.layout = layout;
        self.zoomed = false;
        Ok(collection_id)
    }

    pub fn remove_empty_collection(
        &mut self,
        collection_id: CollectionId,
    ) -> Result<(), CollectionMutationError> {
        let container_id = TerminalContainerId::from(collection_id);
        let collection = self
            .layout
            .collection(container_id)
            .ok_or(CollectionMutationError::CollectionNotFound)?;
        if collection.member_count() != 0 {
            return Err(CollectionMutationError::CollectionNotEmpty);
        }
        if self.layout.leaf_count() <= 1 {
            return Err(CollectionMutationError::LastLayoutLeaf);
        }
        let mut layout = self.layout.clone();
        if !layout.remove_collection(container_id) {
            return Err(CollectionMutationError::LayoutMutationFailed);
        }
        self.legacy_collection_snapshots.remove(&container_id);
        self.layout = layout;
        self.zoomed = false;
        Ok(())
    }

    pub fn collect_tiled_pane(
        &mut self,
        pane_id: PaneId,
        collection_id: CollectionId,
    ) -> Result<(), CollectionMutationError> {
        if !self.panes.contains_key(&pane_id) {
            return Err(CollectionMutationError::PaneNotFound);
        }
        let container_id = TerminalContainerId::from(collection_id);
        if self.layout.collection(container_id).is_none() {
            return Err(CollectionMutationError::CollectionNotFound);
        }
        if self.layout.placement(pane_id) != Some(PanePlacement::Tiled) {
            return Err(CollectionMutationError::PaneNotTiled);
        }
        if self.layout.leaf_count() <= 1 {
            return Err(CollectionMutationError::LastLayoutLeaf);
        }
        let mut layout = self.layout.clone();
        if !layout.remove_tiled_pane_for_collection(pane_id)
            || !layout.add_collection_member(container_id, pane_id)
        {
            return Err(CollectionMutationError::LayoutMutationFailed);
        }
        self.layout = layout;
        self.zoomed = false;
        Ok(())
    }

    pub fn promote_collection_member_near(
        &mut self,
        pane_id: PaneId,
        collection_id: CollectionId,
        target_pane_id: PaneId,
        direction: Direction,
        ratio: f32,
    ) -> Result<(), CollectionMutationError> {
        let container_id = TerminalContainerId::from(collection_id);
        if self.layout.placement(pane_id) != Some(PanePlacement::Stack(container_id)) {
            return Err(CollectionMutationError::PaneNotMember);
        }
        if self.layout.placement(target_pane_id) != Some(PanePlacement::Tiled) {
            return Err(CollectionMutationError::TargetNotTiled);
        }
        let mut layout = self.layout.clone();
        if !layout.remove_collection_member(container_id, pane_id)
            || !layout.insert_pane_near(target_pane_id, pane_id, direction, ratio, true)
        {
            return Err(CollectionMutationError::LayoutMutationFailed);
        }
        self.layout = layout;
        self.zoomed = false;
        Ok(())
    }

    pub fn select_collection_member(
        &mut self,
        collection_id: CollectionId,
        pane_id: PaneId,
    ) -> Result<(), CollectionMutationError> {
        let mut layout = self.layout.clone();
        let container_id = TerminalContainerId::from(collection_id);
        if !layout.select_collection_member(container_id, pane_id) {
            return Err(CollectionMutationError::PaneNotMember);
        }
        self.layout = layout;
        Ok(())
    }

    pub fn reorder_collection_member(
        &mut self,
        collection_id: CollectionId,
        pane_id: PaneId,
        index: usize,
    ) -> Result<(), CollectionMutationError> {
        let mut layout = self.layout.clone();
        let container_id = TerminalContainerId::from(collection_id);
        if !layout.reorder_collection_member(container_id, pane_id, index) {
            return Err(CollectionMutationError::PaneNotMember);
        }
        self.layout = layout;
        Ok(())
    }

    #[cfg(test)]
    pub fn set_collection_member_archived(
        &mut self,
        collection_id: CollectionId,
        pane_id: PaneId,
        archived: bool,
    ) -> Result<(), CollectionMutationError> {
        let layout = self.layout.clone();
        let mut policies = self.legacy_collection_snapshots.clone();
        let container_id = TerminalContainerId::from(collection_id);
        let Some(collection) = layout.collection(container_id) else {
            return Err(CollectionMutationError::CollectionNotFound);
        };
        if !collection.contains_member(pane_id) {
            return Err(CollectionMutationError::PaneNotMember);
        }
        let policy = policies
            .get_mut(&container_id)
            .ok_or(CollectionMutationError::CollectionNotFound)?;
        policy.set_archived(pane_id, archived);
        self.commit_collection_state(layout, policies);
        Ok(())
    }

    #[cfg(test)]
    fn commit_collection_state(
        &mut self,
        layout: TileLayout,
        policies: HashMap<TerminalContainerId, LegacyCollectionSnapshot>,
    ) {
        self.layout = layout;
        self.legacy_collection_snapshots = policies;
    }

    pub fn is_auto_named(&self) -> bool {
        self.custom_name.is_none()
    }

    pub fn set_custom_name(&mut self, name: String) {
        self.custom_name = Some(name);
    }

    pub fn split_focused_command(
        &mut self,
        terminal_id: TerminalId,
        direction: Direction,
        rows: u16,
        cols: u16,
        cwd: Option<PathBuf>,
        command: &str,
        launch_env: &PaneLaunchEnv,
        scrollback_limit_bytes: usize,
        host_terminal_theme: crate::terminal_theme::TerminalTheme,
        host_terminal_appearance: Option<crate::terminal_theme::HostAppearance>,
    ) -> std::io::Result<NewPane> {
        self.split_pane_with_runtime(
            self.layout.focused(),
            terminal_id,
            true,
            direction,
            None,
            rows,
            cols,
            cwd,
            scrollback_limit_bytes,
            host_terminal_theme,
            host_terminal_appearance,
            crate::pane::PaneShellConfig::new("", crate::config::ShellModeConfig::NonLogin),
            launch_env,
            Some(SplitCommand::Shell {
                command,
                launch_env,
            }),
        )
    }

    /// Split `target` with a shell pane. Focus moves to the new pane only when
    /// `focus_new_pane` is set; a spawn failure rolls the layout back without
    /// touching focus or its history.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn split_pane_shell(
        &mut self,
        target: PaneId,
        terminal_id: TerminalId,
        focus_new_pane: bool,
        direction: Direction,
        ratio: Option<f32>,
        rows: u16,
        cols: u16,
        cwd: Option<PathBuf>,
        scrollback_limit_bytes: usize,
        host_terminal_theme: crate::terminal_theme::TerminalTheme,
        host_terminal_appearance: Option<crate::terminal_theme::HostAppearance>,
        shell_config: crate::pane::PaneShellConfig<'_>,
        launch_env: &PaneLaunchEnv,
    ) -> std::io::Result<NewPane> {
        self.split_pane_with_runtime(
            target,
            terminal_id,
            focus_new_pane,
            direction,
            ratio,
            rows,
            cols,
            cwd,
            scrollback_limit_bytes,
            host_terminal_theme,
            host_terminal_appearance,
            shell_config,
            launch_env,
            None,
        )
    }

    /// Split `target` with an argv-command pane. Same focus contract as
    /// `split_pane_shell`.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn split_pane_argv(
        &mut self,
        target: PaneId,
        terminal_id: TerminalId,
        focus_new_pane: bool,
        direction: Direction,
        ratio: Option<f32>,
        rows: u16,
        cols: u16,
        cwd: Option<PathBuf>,
        argv: &[String],
        launch_env: &PaneLaunchEnv,
        scrollback_limit_bytes: usize,
        host_terminal_theme: crate::terminal_theme::TerminalTheme,
        host_terminal_appearance: Option<crate::terminal_theme::HostAppearance>,
    ) -> std::io::Result<NewPane> {
        self.split_pane_with_runtime(
            target,
            terminal_id,
            focus_new_pane,
            direction,
            ratio,
            rows,
            cols,
            cwd,
            scrollback_limit_bytes,
            host_terminal_theme,
            host_terminal_appearance,
            crate::pane::PaneShellConfig::new("", crate::config::ShellModeConfig::NonLogin),
            launch_env,
            Some(SplitCommand::Argv { argv, launch_env }),
        )
    }

    // Split construction threads geometry, host context, launch policy, and command state.
    #[allow(clippy::too_many_arguments)]
    fn split_pane_with_runtime(
        &mut self,
        target: PaneId,
        terminal_id: TerminalId,
        focus_new_pane: bool,
        direction: Direction,
        ratio: Option<f32>,
        rows: u16,
        cols: u16,
        cwd: Option<PathBuf>,
        scrollback_limit_bytes: usize,
        host_terminal_theme: crate::terminal_theme::TerminalTheme,
        host_terminal_appearance: Option<crate::terminal_theme::HostAppearance>,
        shell_config: crate::pane::PaneShellConfig<'_>,
        launch_env: &PaneLaunchEnv,
        command: Option<SplitCommand<'_>>,
    ) -> std::io::Result<NewPane> {
        if self.panes.len() >= crate::admission::MAX_TAB_PANES {
            return Err(std::io::Error::other("tab pane limit reached"));
        }
        let mut layout = self.layout.clone();
        let Some(new_id) = layout.split_pane(target, direction, ratio.unwrap_or(0.5)) else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "split target pane is not in the layout",
            ));
        };
        let actual_cwd =
            cwd.unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| "/".into()));
        let launch_argv = if let Some(SplitCommand::Argv { argv, .. }) = &command {
            Some((*argv).to_vec())
        } else {
            None
        };
        let runtime = match command {
            Some(SplitCommand::Shell {
                command,
                launch_env,
            }) => TerminalRuntime::spawn_shell_command(
                new_id,
                rows,
                cols,
                actual_cwd.clone(),
                command,
                launch_env,
                crate::pane::AgentDetection::Enabled,
                scrollback_limit_bytes,
                host_terminal_theme,
                host_terminal_appearance,
                self.events.clone(),
                self.render_notify.clone(),
                self.render_dirty.clone(),
            ),
            Some(SplitCommand::Argv { argv, launch_env }) => TerminalRuntime::spawn_argv_command(
                new_id,
                rows,
                cols,
                actual_cwd.clone(),
                argv,
                launch_env,
                crate::pane::AgentDetection::Enabled,
                scrollback_limit_bytes,
                host_terminal_theme,
                host_terminal_appearance,
                self.events.clone(),
                self.render_notify.clone(),
                self.render_dirty.clone(),
            ),
            None => TerminalRuntime::spawn(
                new_id,
                rows,
                cols,
                actual_cwd.clone(),
                scrollback_limit_bytes,
                host_terminal_theme,
                host_terminal_appearance,
                shell_config,
                launch_env,
                self.events.clone(),
                self.render_notify.clone(),
                self.render_dirty.clone(),
            ),
        };
        let runtime = runtime?;
        let terminal = match launch_argv {
            Some(argv) => {
                TerminalState::new(terminal_id.clone(), actual_cwd).with_launch_argv(argv)
            }
            None => TerminalState::new(terminal_id.clone(), actual_cwd),
        };
        if focus_new_pane {
            layout.focus_pane(new_id);
        }
        self.panes.insert(new_id, PaneState::new(terminal_id));
        self.layout = layout;
        self.zoomed = false;
        Ok(NewPane {
            pane_id: new_id,
            terminal,
            runtime,
        })
    }

    #[cfg(test)]
    pub fn close_focused(&mut self) -> Option<DetachedPane> {
        let pane_id = self.layout.focused();
        self.detach_pane(pane_id)
    }

    pub fn close_pane(&mut self, pane_id: PaneId) -> Option<DetachedPane> {
        self.detach_pane(pane_id)
    }

    pub fn remove_pane(&mut self, pane_id: PaneId) -> Option<DetachedPane> {
        self.detach_pane(pane_id)
    }

    pub(crate) fn from_existing_pane(
        id: super::TabId,
        number: usize,
        custom_name: Option<String>,
        moved: MovedPane,
        events: mpsc::Sender<AppEvent>,
        render_notify: Arc<Notify>,
        render_dirty: Arc<RenderSignal>,
    ) -> Self {
        let mut panes = HashMap::new();
        let pane_id = moved.pane_id;
        panes.insert(pane_id, moved.pane_state);
        Self {
            id,
            custom_name,
            number,
            root_pane: pane_id,
            layout: TileLayout::for_pane(pane_id),
            legacy_collection_snapshots: HashMap::new(),
            panes,
            #[cfg(test)]
            runtimes: HashMap::new(),
            zoomed: false,
            events,
            render_notify,
            render_dirty,
        }
    }

    pub(crate) fn take_pane_for_move(&mut self, pane_id: PaneId) -> Option<MovedPane> {
        if !self.panes.contains_key(&pane_id) {
            return None;
        }

        if self.layout.pane_count() > 1 {
            let next_root = self.promoted_root_if_needed(pane_id);
            self.layout = self.remove_layout_pane_candidate(pane_id)?;
            if let Some(next_root) = next_root {
                self.root_pane = next_root;
            }
        }

        let pane_state = self.panes.remove(&pane_id)?;
        self.zoomed = false;
        Some(MovedPane {
            pane_id,
            pane_state,
        })
    }

    pub(crate) fn insert_existing_pane(
        &mut self,
        target_pane_id: PaneId,
        moved: MovedPane,
        direction: Direction,
        ratio: f32,
        focus: bool,
    ) -> Result<PaneId, MovedPane> {
        let mut layout = self.layout.clone();
        if !layout.insert_pane_near(target_pane_id, moved.pane_id, direction, ratio, focus) {
            return Err(moved);
        }
        let pane_id = moved.pane_id;
        self.panes.insert(pane_id, moved.pane_state);
        self.layout = layout;
        self.zoomed = false;
        Ok(pane_id)
    }

    fn detach_pane(&mut self, pane_id: PaneId) -> Option<DetachedPane> {
        if self.layout.pane_count() <= 1 {
            return None;
        }

        let next_root = self.promoted_root_if_needed(pane_id);

        let layout = self.remove_layout_pane_candidate(pane_id)?;
        let pane = self.panes.remove(&pane_id)?;
        let terminal_id = pane.attached_terminal_id;
        self.layout = layout;
        self.zoomed = false;
        if let Some(next_root) = next_root {
            self.root_pane = next_root;
        }
        Some((pane_id, terminal_id))
    }

    fn remove_layout_pane_candidate(&self, pane_id: PaneId) -> Option<TileLayout> {
        self.layout.placement(pane_id)?;
        let mut layout = self.layout.clone();
        layout.close_pane(pane_id).then_some(layout)
    }

    fn promoted_root_if_needed(&self, closing: PaneId) -> Option<PaneId> {
        if self.root_pane != closing {
            return None;
        }
        self.layout.pane_ids().into_iter().find(|id| *id != closing)
    }

    pub fn terminal_id(&self, pane_id: PaneId) -> Option<&TerminalId> {
        self.panes
            .get(&pane_id)
            .map(|pane| &pane.attached_terminal_id)
    }

    pub fn cwd_for_pane(
        &self,
        pane_id: PaneId,
        terminals: &HashMap<TerminalId, TerminalState>,
        terminal_runtimes: &TerminalRuntimeRegistry,
    ) -> Option<PathBuf> {
        let terminal_id = self.terminal_id(pane_id)?;
        terminal_runtimes
            .get(terminal_id)
            .and_then(|rt| rt.cwd())
            .or_else(|| {
                terminals
                    .get(terminal_id)
                    .map(|terminal| terminal.cwd.clone())
            })
    }

    pub fn foreground_cwd_for_pane(
        &self,
        pane_id: PaneId,
        terminal_runtimes: &TerminalRuntimeRegistry,
    ) -> Option<PathBuf> {
        let terminal_id = self.terminal_id(pane_id)?;
        terminal_runtimes
            .get(terminal_id)
            .and_then(|rt| rt.foreground_cwd())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn deep_legacy_identity_state() -> (crate::app::AppState, usize, PaneId) {
        let mut state = crate::app::AppState::test_with_adversarial_identity_state();
        let workspace = &mut state.workspaces[0];
        let index = workspace
            .tabs
            .iter()
            .position(|tab| tab.panes.len() > 1)
            .unwrap();
        workspace.switch_tab(index);
        let root = workspace.tabs[index].root_pane;
        for _ in 0..6 {
            workspace.tabs[index].layout.focus_pane(root);
            assert_eq!(workspace.tabs[index].layout.focused(), root);
            workspace.test_split_with_ratio(Direction::Horizontal, 0.8);
        }
        state.ensure_test_terminals();
        state.assert_invariants_for_test();
        assert!(legacy_split_depth(&state.workspaces[0].tabs[index]) > 4);
        (state, index, root)
    }

    fn legacy_split_depth(tab: &Tab) -> usize {
        tab.layout
            .splits(ratatui::layout::Rect::new(0, 0, 512, 128))
            .iter()
            .map(|split| split.path.len() + 1)
            .max()
            .unwrap_or(0)
    }

    fn saved_legacy_identities(state: &crate::app::AppState) -> Vec<(PaneId, usize, TerminalId)> {
        state.workspaces[0]
            .tabs
            .iter()
            .flat_map(|tab| &tab.panes)
            .map(|(id, pane)| {
                (
                    *id,
                    state.workspaces[0].public_pane_number(*id).unwrap(),
                    pane.attached_terminal_id.clone(),
                )
            })
            .collect()
    }

    fn assert_legacy_identities(
        state: &crate::app::AppState,
        identities: &[(PaneId, usize, TerminalId)],
    ) {
        state.assert_invariants_for_test();
        for (pane, number, terminal) in identities {
            assert_eq!(state.workspaces[0].public_pane_number(*pane), Some(*number));
            assert_eq!(state.workspaces[0].terminal_id(*pane), Some(terminal));
        }
    }

    #[cfg(unix)]
    fn assert_deep_legacy_runtime_split(use_argv: bool) {
        let (mut state, index, root) = deep_legacy_identity_state();
        let identities = saved_legacy_identities(&state);
        let depth = legacy_split_depth(&state.workspaces[0].tabs[index]);
        let terminal_id = TerminalId::alloc().unwrap();
        let private_root = std::env::temp_dir().join(format!(
            "vqro-deep-split-{}-{}",
            std::process::id(),
            root.raw()
        ));
        std::fs::create_dir(&private_root).unwrap();
        let launch_env = PaneLaunchEnv::from_extra(vec![
            ("HOME".into(), private_root.display().to_string()),
            ("XDG_CONFIG_HOME".into(), private_root.display().to_string()),
            ("XDG_STATE_HOME".into(), private_root.display().to_string()),
        ]);
        let argv = vec!["/bin/sh".into(), "-c".into(), "exit 0".into()];
        let tab = &mut state.workspaces[0].tabs[index];
        tab.zoomed = true;
        let result = if use_argv {
            tab.split_pane_argv(
                root,
                terminal_id.clone(),
                true,
                Direction::Vertical,
                Some(0.8),
                8,
                40,
                Some(private_root.clone()),
                &argv,
                &launch_env,
                0,
                crate::terminal_theme::TerminalTheme::default(),
                None,
            )
        } else {
            tab.split_pane_shell(
                root,
                terminal_id.clone(),
                true,
                Direction::Vertical,
                Some(0.8),
                8,
                40,
                Some(private_root.clone()),
                0,
                crate::terminal_theme::TerminalTheme::default(),
                None,
                crate::pane::PaneShellConfig::new(
                    "/bin/sh",
                    crate::config::ShellModeConfig::NonLogin,
                ),
                &launch_env,
            )
        };
        let NewPane {
            pane_id,
            terminal,
            runtime,
        } = result.expect("deep legacy split must still launch a PTY");
        runtime.shutdown();
        std::fs::remove_dir_all(&private_root).unwrap();
        state.workspaces[0].register_new_pane(pane_id);
        state.terminals.insert(terminal.id.clone(), terminal);
        assert_eq!(
            state.workspaces[0].tabs[index].panes[&pane_id].attached_terminal_id,
            terminal_id
        );
        assert_eq!(state.workspaces[0].tabs[index].layout.focused(), pane_id);
        assert!(!state.workspaces[0].tabs[index].zoomed);
        assert_eq!(
            legacy_split_depth(&state.workspaces[0].tabs[index]),
            depth + 1
        );
        assert_eq!(
            state.terminals[&terminal_id].launch_argv,
            use_argv.then_some(argv)
        );
        assert_legacy_identities(&state, &identities);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn legacy_tab_split_pane_shell_accepts_a_deep_branch() {
        assert_deep_legacy_runtime_split(false);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn legacy_tab_split_pane_argv_accepts_a_deep_branch() {
        assert_deep_legacy_runtime_split(true);
    }

    #[test]
    fn legacy_tab_insert_existing_pane_reparents_into_a_deep_branch() {
        let (mut state, index, root) = deep_legacy_identity_state();
        let identities = saved_legacy_identities(&state);
        let tab = &mut state.workspaces[0].tabs[index];
        let moving = tab
            .layout
            .pane_ids()
            .into_iter()
            .find(|pane| *pane != root)
            .unwrap();
        let moved = tab.take_pane_for_move(moving).unwrap();
        let depth = legacy_split_depth(tab);
        tab.zoomed = true;
        let result = tab.insert_existing_pane(root, moved, Direction::Vertical, 0.8, true);
        assert!(matches!(result, Ok(id) if id == moving));
        assert_eq!(tab.layout.focused(), moving);
        assert!(!tab.zoomed);
        assert_eq!(legacy_split_depth(tab), depth + 1);
        assert_legacy_identities(&state, &identities);
    }

    #[test]
    fn legacy_deep_insert_rejection_returns_the_same_pane_without_mutation() {
        let (mut state, index, root) = deep_legacy_identity_state();
        let identities = saved_legacy_identities(&state);
        let tab = &mut state.workspaces[0].tabs[index];
        let moving = tab
            .layout
            .pane_ids()
            .into_iter()
            .find(|id| *id != root)
            .unwrap();
        let terminal = tab.panes[&moving].attached_terminal_id.clone();
        let moved = tab.take_pane_for_move(moving).unwrap();
        let missing = PaneId::alloc();
        tab.zoomed = true;
        let tree = tab.layout.typed_root().clone();
        let focus = tab.layout.focused_leaf();
        let next_pane = PaneId::next_for_test();
        let returned =
            match tab.insert_existing_pane(missing, moved, Direction::Vertical, 0.8, true) {
                Err(returned) => returned,
                Ok(_) => panic!("missing target accepted a moved pane"),
            };
        assert_eq!(returned.pane_id, moving);
        assert_eq!(returned.pane_state.attached_terminal_id, terminal);
        assert_eq!(tab.layout.typed_root(), &tree);
        assert_eq!(tab.layout.focused_leaf(), focus);
        assert!(tab.zoomed);
        assert!(!tab.panes.contains_key(&moving));
        assert_eq!(PaneId::next_for_test(), next_pane);
        assert!(tab
            .insert_existing_pane(root, returned, Direction::Vertical, 0.8, true)
            .is_ok());
        assert_legacy_identities(&state, &identities);
    }

    #[test]
    fn legacy_create_collection_near_accepts_a_deep_branch() {
        let (mut state, index, root) = deep_legacy_identity_state();
        let identities = saved_legacy_identities(&state);
        let tab = &mut state.workspaces[0].tabs[index];
        let depth = legacy_split_depth(tab);
        tab.zoomed = true;
        let id = tab
            .create_collection_near(
                LayoutLeaf::Pane(root),
                Direction::Vertical,
                0.8,
                Some("deep".into()),
            )
            .unwrap();
        assert_eq!(legacy_split_depth(tab), depth + 1);
        assert!(!tab.zoomed);
        let collection = tab.collection(id).unwrap();
        assert!(collection.members().is_empty());
        assert_eq!(collection.selected(), None);
        assert_eq!(collection.legacy_label(), Some("deep"));
        assert_legacy_identities(&state, &identities);
    }

    #[test]
    fn legacy_promote_collection_member_near_accepts_a_deep_branch() {
        let (mut state, index, root) = deep_legacy_identity_state();
        let identities = saved_legacy_identities(&state);
        let tab = &mut state.workspaces[0].tabs[index];
        let member = tab
            .layout
            .pane_ids()
            .into_iter()
            .find(|pane| *pane != root)
            .unwrap();
        let id = tab
            .create_collection_near(
                LayoutLeaf::Pane(root),
                Direction::Vertical,
                0.8,
                Some("deep".into()),
            )
            .unwrap();
        tab.collect_tiled_pane(member, id).unwrap();
        let depth = legacy_split_depth(tab);
        tab.zoomed = true;
        tab.promote_collection_member_near(member, id, root, Direction::Horizontal, 0.8)
            .unwrap();
        assert_eq!(legacy_split_depth(tab), depth + 1);
        assert_eq!(tab.layout.placement(member), Some(PanePlacement::Tiled));
        assert_eq!(tab.layout.focused(), member);
        assert!(!tab.zoomed);
        assert!(tab.collection(id).unwrap().members().is_empty());
        assert_legacy_identities(&state, &identities);
    }

    #[test]
    fn legacy_tab_split_limit_rejection_preserves_deep_layout_before_pane_allocation() {
        let (mut state, index, root) = deep_legacy_identity_state();
        while state.workspaces[0].tabs[index].panes.len() < crate::admission::MAX_TAB_PANES {
            state.workspaces[0].tabs[index].layout.focus_pane(root);
            state.workspaces[0].test_split_with_ratio(Direction::Horizontal, 0.95);
        }
        state.ensure_test_terminals();
        let identities = saved_legacy_identities(&state);
        let tab = &mut state.workspaces[0].tabs[index];
        tab.zoomed = true;
        let tree = tab.layout.typed_root().clone();
        let focus = tab.layout.focused_leaf();
        let next_pane = PaneId::next_for_test();
        let launch_env = PaneLaunchEnv::default();
        let argv = vec!["vqro-missing-command-for-limit-test".into()];
        for use_argv in [false, true] {
            let result = if use_argv {
                tab.split_pane_argv(
                    root,
                    TerminalId::alloc().unwrap(),
                    true,
                    Direction::Vertical,
                    Some(0.8),
                    8,
                    40,
                    None,
                    &argv,
                    &launch_env,
                    0,
                    crate::terminal_theme::TerminalTheme::default(),
                    None,
                )
            } else {
                tab.split_pane_shell(
                    root,
                    TerminalId::alloc().unwrap(),
                    true,
                    Direction::Vertical,
                    Some(0.8),
                    8,
                    40,
                    None,
                    0,
                    crate::terminal_theme::TerminalTheme::default(),
                    None,
                    crate::pane::PaneShellConfig::new(
                        "vqro-missing-shell-for-limit-test",
                        crate::config::ShellModeConfig::NonLogin,
                    ),
                    &launch_env,
                )
            };
            let error = match result {
                Err(error) => error,
                Ok(new) => {
                    new.runtime.shutdown();
                    panic!("tab limit admitted another pane");
                }
            };
            assert_eq!(error.to_string(), "tab pane limit reached");
            assert_eq!(tab.layout.typed_root(), &tree);
            assert_eq!(tab.layout.focused_leaf(), focus);
            assert!(tab.zoomed);
            assert_eq!(PaneId::next_for_test(), next_pane);
        }
        assert_legacy_identities(&state, &identities);
    }

    #[test]
    fn legacy_promote_rejects_non_tiled_target_without_reparenting_or_allocating() {
        let (mut state, index, root) = deep_legacy_identity_state();
        let identities = saved_legacy_identities(&state);
        let tab = &mut state.workspaces[0].tabs[index];
        let members: Vec<_> = tab
            .layout
            .pane_ids()
            .into_iter()
            .filter(|id| *id != root)
            .take(2)
            .collect();
        let source = tab
            .create_collection_near(
                LayoutLeaf::Pane(root),
                Direction::Vertical,
                0.8,
                Some("source".into()),
            )
            .unwrap();
        tab.collect_tiled_pane(members[0], source).unwrap();
        let target = tab
            .create_collection_near(
                LayoutLeaf::Pane(root),
                Direction::Horizontal,
                0.8,
                Some("target".into()),
            )
            .unwrap();
        tab.collect_tiled_pane(members[1], target).unwrap();
        tab.zoomed = true;
        let tree = tab.layout.typed_root().clone();
        let focus = tab.layout.focused_leaf();
        let next_pane = PaneId::next_for_test();
        assert_eq!(
            tab.promote_collection_member_near(
                members[0],
                source,
                members[1],
                Direction::Vertical,
                0.5
            ),
            Err(CollectionMutationError::TargetNotTiled)
        );
        assert_eq!(tab.layout.typed_root(), &tree);
        assert_eq!(tab.layout.focused_leaf(), focus);
        assert!(tab.zoomed);
        assert_eq!(PaneId::next_for_test(), next_pane);
        for (id, member, label) in [
            (source, members[0], "source"),
            (target, members[1], "target"),
        ] {
            let collection = tab.collection(id).unwrap();
            assert_eq!(collection.members(), &[member]);
            assert_eq!(collection.selected(), Some(member));
            assert_eq!(collection.legacy_label(), Some(label));
        }
        assert_legacy_identities(&state, &identities);
    }

    #[test]
    #[should_panic(expected = "raw archived state must be a subset")]
    fn runtime_invariant_checks_raw_archive_state_without_filtering() {
        let mut workspace = crate::workspace::Workspace::test_new("raw-archive-invariant");
        let root = workspace.tabs[0].root_pane;
        let collection = workspace.tabs[0]
            .create_collection_near(
                LayoutLeaf::Pane(root),
                Direction::Vertical,
                0.5,
                Some("legacy".into()),
            )
            .unwrap();
        let container_id = TerminalContainerId::from(collection);
        workspace.tabs[0]
            .legacy_collection_snapshots
            .get_mut(&container_id)
            .expect("compatibility policy")
            .set_archived(root, true);

        workspace.assert_invariants_for_test();
    }

    #[test]
    fn layout_replacement_clears_compat_policy_without_losing_panes() {
        let mut workspace = crate::workspace::Workspace::test_new("replacement");
        let root = workspace.tabs[0].root_pane;
        let member = workspace.test_split(Direction::Horizontal);
        let collection = workspace.tabs[0]
            .create_collection_near(
                LayoutLeaf::Pane(root),
                Direction::Vertical,
                0.5,
                Some("legacy".into()),
            )
            .unwrap();
        workspace.tabs[0]
            .collect_tiled_pane(member, collection)
            .unwrap();

        let replacement = TileLayout::from_tree(
            TypedNode::Split {
                direction: Direction::Horizontal,
                ratio: 0.5,
                first: Box::new(TypedNode::Leaf(root)),
                second: Box::new(TypedNode::Leaf(member)),
            },
            LayoutLeaf::Pane(root),
        )
        .unwrap();
        workspace.tabs[0].replace_layout_without_collections(replacement, root);

        assert!(workspace.tabs[0].legacy_collection_snapshots.is_empty());
        assert_eq!(workspace.tabs[0].layout.pane_ids(), vec![root, member]);
        workspace.assert_invariants_for_test();
    }
}
