use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use ratatui::layout::Direction;
use tokio::sync::{mpsc, Notify};

use crate::collection_compat::CollectionCompatibilityPolicy;
use crate::events::AppEvent;
#[cfg(test)]
use crate::layout::TypedNode;
use crate::layout::{CollectionId, LayoutLeaf, Node, PaneId, PanePlacement, TileLayout};
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
    container: &'a crate::terminal_container::TerminalContainer,
    policy: &'a CollectionCompatibilityPolicy,
}

impl PaneCollection<'_> {
    pub(crate) fn id(&self) -> CollectionId {
        self.container.id.into()
    }

    pub(crate) fn label(&self) -> Option<&str> {
        self.policy.label.as_deref()
    }

    pub(crate) fn members(&self) -> &[PaneId] {
        self.container.members()
    }

    pub(crate) fn selected(&self) -> Option<PaneId> {
        self.container.selected()
    }

    pub(crate) fn is_archived(&self, pane_id: PaneId) -> bool {
        self.policy.is_archived(pane_id)
    }

    pub(crate) fn archived_members(&self) -> impl Iterator<Item = PaneId> + '_ {
        self.policy.archived_members(self.container.members())
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
    pub(super) collection_compat: HashMap<TerminalContainerId, CollectionCompatibilityPolicy>,
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
                collection_compat: HashMap::new(),
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
            policy: self.collection_compat.get(&container_id)?,
        })
    }

    pub(crate) fn collections(&self) -> impl Iterator<Item = PaneCollection<'_>> {
        self.layout
            .collection_ids()
            .into_iter()
            .filter_map(|id| self.collection(id.into()))
    }

    #[cfg(test)]
    pub(super) fn assert_collection_invariants_for_test(&self) {
        let layout_ids: HashSet<_> = self.layout.collection_ids().into_iter().collect();
        let policy_ids: HashSet<_> = self.collection_compat.keys().copied().collect();
        assert_eq!(
            layout_ids, policy_ids,
            "container leaves and compatibility policy must have a one-to-one identity mapping"
        );
        for container_id in layout_ids {
            let container = self
                .layout
                .collection(container_id)
                .expect("layout ID must resolve to a container");
            let policy = &self.collection_compat[&container_id];
            let members: HashSet<_> = container.members().iter().copied().collect();
            assert_eq!(members.len(), container.members().len());
            assert_eq!(
                container.selected().is_some(),
                !members.is_empty(),
                "selection must exist if and only if the container is non-empty"
            );
            assert!(container
                .selected()
                .is_none_or(|pane| members.contains(&pane)));
            assert!(
                policy.archived().is_subset(&members),
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
        self.collection_compat.clear();
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
        policies: HashMap<TerminalContainerId, CollectionCompatibilityPolicy>,
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
            collection_compat: policies,
            panes,
            #[cfg(test)]
            runtimes: HashMap::new(),
            zoomed,
            events,
            render_notify,
            render_dirty,
        })
    }

    pub fn create_collection_near(
        &mut self,
        target: LayoutLeaf,
        direction: Direction,
        ratio: f32,
        label: Option<String>,
    ) -> Result<CollectionId, CollectionMutationError> {
        self.create_collection_near_with_allocator(target, direction, ratio, label, || {
            CollectionId::alloc()
        })
    }

    fn create_collection_near_with_allocator<F>(
        &mut self,
        target: LayoutLeaf,
        direction: Direction,
        ratio: f32,
        label: Option<String>,
        allocate: F,
    ) -> Result<CollectionId, CollectionMutationError>
    where
        F: FnOnce() -> Result<CollectionId, crate::layout::CollectionIdError>,
    {
        // Preflight before allocation so invalid placement cannot consume IDs.
        if !self.layout.can_insert_collection_near(target) {
            return Err(CollectionMutationError::LayoutMutationFailed);
        }
        let collection_id =
            allocate().map_err(|_| CollectionMutationError::LayoutMutationFailed)?;
        let container_id = TerminalContainerId::from(collection_id);
        let mut layout = self.layout.clone();
        let mut policies = self.collection_compat.clone();
        if !layout.insert_collection_near(target, container_id, direction, ratio) {
            return Err(CollectionMutationError::LayoutMutationFailed);
        }
        policies.insert(
            container_id,
            CollectionCompatibilityPolicy::from_legacy(label, HashSet::new()),
        );
        self.commit_collection_state(layout, policies);
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
        if !collection.members().is_empty() {
            return Err(CollectionMutationError::CollectionNotEmpty);
        }
        if self.layout.leaf_count() <= 1 {
            return Err(CollectionMutationError::LastLayoutLeaf);
        }
        let mut layout = self.layout.clone();
        let mut policies = self.collection_compat.clone();
        if !layout.remove_collection(container_id) {
            return Err(CollectionMutationError::LayoutMutationFailed);
        }
        policies.remove(&container_id);
        self.commit_collection_state(layout, policies);
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
        let mut policies = self.collection_compat.clone();
        if !layout.remove_tiled_pane_for_collection(pane_id)
            || !layout.add_collection_member(container_id, pane_id)
        {
            return Err(CollectionMutationError::LayoutMutationFailed);
        }
        policies
            .get_mut(&container_id)
            .ok_or(CollectionMutationError::CollectionNotFound)?
            .bump_revision();
        self.commit_collection_state(layout, policies);
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
        if self.layout.placement(pane_id) != Some(PanePlacement::Collection(container_id)) {
            return Err(CollectionMutationError::PaneNotMember);
        }
        if self.layout.placement(target_pane_id) != Some(PanePlacement::Tiled) {
            return Err(CollectionMutationError::TargetNotTiled);
        }
        let mut layout = self.layout.clone();
        let mut policies = self.collection_compat.clone();
        if !layout.remove_collection_member(container_id, pane_id)
            || !layout.insert_pane_near(target_pane_id, pane_id, direction, ratio, true)
        {
            return Err(CollectionMutationError::LayoutMutationFailed);
        }
        policies
            .get_mut(&container_id)
            .ok_or(CollectionMutationError::CollectionNotFound)?
            .remove_member(pane_id);
        self.commit_collection_state(layout, policies);
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
        self.commit_collection_state(layout, self.collection_compat.clone());
        Ok(())
    }

    pub fn reorder_collection_member(
        &mut self,
        collection_id: CollectionId,
        pane_id: PaneId,
        index: usize,
    ) -> Result<(), CollectionMutationError> {
        let mut layout = self.layout.clone();
        let mut policies = self.collection_compat.clone();
        let container_id = TerminalContainerId::from(collection_id);
        if !layout.reorder_collection_member(container_id, pane_id, index) {
            return Err(CollectionMutationError::PaneNotMember);
        }
        policies
            .get_mut(&container_id)
            .ok_or(CollectionMutationError::CollectionNotFound)?
            .bump_revision();
        self.commit_collection_state(layout, policies);
        Ok(())
    }

    pub fn set_collection_member_archived(
        &mut self,
        collection_id: CollectionId,
        pane_id: PaneId,
        archived: bool,
    ) -> Result<(), CollectionMutationError> {
        let layout = self.layout.clone();
        let mut policies = self.collection_compat.clone();
        let container_id = TerminalContainerId::from(collection_id);
        let Some(collection) = layout.collection(container_id) else {
            return Err(CollectionMutationError::CollectionNotFound);
        };
        if !collection.members().contains(&pane_id) {
            return Err(CollectionMutationError::PaneNotMember);
        }
        let policy = policies
            .get_mut(&container_id)
            .ok_or(CollectionMutationError::CollectionNotFound)?;
        policy.set_archived(pane_id, archived);
        self.commit_collection_state(layout, policies);
        Ok(())
    }

    fn commit_collection_state(
        &mut self,
        layout: TileLayout,
        policies: HashMap<TerminalContainerId, CollectionCompatibilityPolicy>,
    ) {
        self.layout = layout;
        self.collection_compat = policies;
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
        self.commit_collection_state(layout, self.collection_compat.clone());
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
            layout: TileLayout::from_saved(Node::Pane(pane_id), pane_id),
            collection_compat: HashMap::new(),
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
            let (layout, policies) = self.remove_layout_pane_candidate(pane_id)?;
            self.commit_collection_state(layout, policies);
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
        self.commit_collection_state(layout, self.collection_compat.clone());
        self.zoomed = false;
        Ok(pane_id)
    }

    fn detach_pane(&mut self, pane_id: PaneId) -> Option<DetachedPane> {
        if self.layout.pane_count() <= 1 {
            return None;
        }

        let next_root = self.promoted_root_if_needed(pane_id);

        let (layout, policies) = self.remove_layout_pane_candidate(pane_id)?;
        let pane = self.panes.remove(&pane_id)?;
        let terminal_id = pane.attached_terminal_id;
        self.commit_collection_state(layout, policies);
        self.zoomed = false;
        if let Some(next_root) = next_root {
            self.root_pane = next_root;
        }
        Some((pane_id, terminal_id))
    }

    fn remove_layout_pane_candidate(
        &self,
        pane_id: PaneId,
    ) -> Option<(
        TileLayout,
        HashMap<TerminalContainerId, CollectionCompatibilityPolicy>,
    )> {
        let placement = self.layout.placement(pane_id)?;
        let mut layout = self.layout.clone();
        let mut policies = self.collection_compat.clone();
        if !layout.close_pane(pane_id) {
            return None;
        }
        if let PanePlacement::Collection(collection_id) = placement {
            policies.get_mut(&collection_id)?.remove_member(pane_id);
        }
        Some((layout, policies))
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

    #[test]
    fn collection_create_preflights_before_id_allocation() {
        let mut workspace = crate::workspace::Workspace::test_new("preflight");
        let allocations = std::sync::atomic::AtomicUsize::new(0);
        let result = workspace.tabs[0].create_collection_near_with_allocator(
            LayoutLeaf::Pane(PaneId::from_raw(u32::MAX)),
            Direction::Horizontal,
            0.5,
            None,
            || {
                allocations.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                CollectionId::from_raw(8_700_001)
            },
        );
        assert_eq!(result, Err(CollectionMutationError::LayoutMutationFailed));
        assert_eq!(allocations.load(std::sync::atomic::Ordering::Relaxed), 0);
    }

    #[test]
    fn compatibility_invalidation_semantics_survive_private_extraction() {
        let mut workspace = crate::workspace::Workspace::test_new("revision");
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
        let container_id = TerminalContainerId::from(collection);
        assert_eq!(
            workspace.tabs[0].collection_compat[&container_id].revision(),
            0
        );
        workspace.tabs[0]
            .collect_tiled_pane(member, collection)
            .unwrap();
        assert_eq!(
            workspace.tabs[0].collection_compat[&container_id].revision(),
            1
        );
        workspace.tabs[0]
            .select_collection_member(collection, member)
            .unwrap();
        assert_eq!(
            workspace.tabs[0].collection_compat[&container_id].revision(),
            1
        );
        workspace.tabs[0]
            .set_collection_member_archived(collection, member, true)
            .unwrap();
        assert_eq!(
            workspace.tabs[0].collection_compat[&container_id].revision(),
            2
        );
        assert_eq!(workspace.tabs[0].layout.pane_ids().len(), 2);
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
            .collection_compat
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

        let replacement = TileLayout::from_typed_saved(
            TypedNode::Split {
                direction: Direction::Horizontal,
                ratio: 0.5,
                first: Box::new(TypedNode::Leaf(LayoutLeaf::Pane(root))),
                second: Box::new(TypedNode::Leaf(LayoutLeaf::Pane(member))),
            },
            LayoutLeaf::Pane(root),
            Vec::new(),
        )
        .unwrap();
        workspace.tabs[0].replace_layout_without_collections(replacement, root);

        assert!(workspace.tabs[0].collection_compat.is_empty());
        assert_eq!(workspace.tabs[0].layout.pane_ids(), vec![root, member]);
        workspace.assert_invariants_for_test();
    }
}
