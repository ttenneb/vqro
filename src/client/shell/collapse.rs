//! Coherent admission for the optional full-surface companion. No provider work.
use super::*;
use crate::{
    layout::LayoutPathStep,
    protocol::collapse::{CollapseProjectionV1, LAYOUT_COLLAPSE_CAPABILITY},
};

pub(super) struct CollapseHeaderHit {
    pub rect: Rect,
    pub index: usize,
}

#[derive(Clone, Debug, Default)]
pub(super) struct CollapsePairState {
    pub generation: Option<u64>,
    pending_surface: Option<(
        PaneSurfaceFrame,
        Vec<crate::protocol::PaneSurfaceViewEntryV2>,
    )>,
    pending_projection: Option<Box<CollapseProjectionV1>>,
    pub current: Option<Box<CollapseProjectionV1>>,
    pub current_surface_revision: u64,
    pub active_header: Option<Vec<LayoutPathStep>>,
    pub disabled: Option<String>,
}
impl CollapsePairState {
    pub fn advance_generation(&mut self, generation: u64) -> bool {
        if self.generation.is_some_and(|current| generation < current) {
            return false;
        }
        if self.generation != Some(generation) {
            *self = Self {
                generation: Some(generation),
                ..Self::default()
            };
        }
        true
    }
    pub fn offer_surface(
        &mut self,
        generation: u64,
        surface: PaneSurfaceFrame,
        views: Vec<crate::protocol::PaneSurfaceViewEntryV2>,
    ) {
        if !self.advance_generation(generation) {
            return;
        }
        let key = (surface.projection_revision, surface.surface_revision);
        if self.current.as_ref().is_some_and(|current| {
            current.boot_id == surface.boot_id
                && key <= (current.projection_revision, self.current_surface_revision)
        }) || self.pending_surface.as_ref().is_some_and(|(current, _)| {
            current.boot_id == surface.boot_id
                && key < (current.projection_revision, current.surface_revision)
        }) {
            return;
        }
        self.pending_surface = Some((surface, views));
    }
    pub fn offer_projection(
        &mut self,
        generation: u64,
        projection: Box<CollapseProjectionV1>,
    ) -> Result<(), &'static str> {
        if !self.advance_generation(generation) {
            return Ok(());
        }
        projection.validate()?;
        if self.disabled.is_some() {
            return Ok(());
        }
        let key = (
            projection.projection_revision,
            projection.base_surface_revision,
        );
        if self.current.as_ref().is_some_and(|current| {
            current.boot_id == projection.boot_id
                && key <= (current.projection_revision, self.current_surface_revision)
        }) || self.pending_projection.as_ref().is_some_and(|current| {
            current.boot_id == projection.boot_id
                && key < (current.projection_revision, current.base_surface_revision)
        }) {
            return Ok(());
        }
        self.pending_projection = Some(projection);
        Ok(())
    }
    pub fn disable(&mut self, reason: &str) {
        self.disabled = Some(
            reason
                .chars()
                .filter(|ch| !ch.is_control())
                .take(160)
                .collect(),
        );
        self.pending_projection = None;
        self.current = None;
        self.active_header = None;
    }
    pub fn take_ready(
        &mut self,
        snapshot: &ClientShellSnapshot,
    ) -> Option<(
        PaneSurfaceFrame,
        Vec<crate::protocol::PaneSurfaceViewEntryV2>,
    )> {
        let (surface, _) = self.pending_surface.as_ref()?;
        if surface.boot_id != snapshot.boot_id || surface.projection_revision != snapshot.revision {
            return None;
        }
        if self.disabled.is_some() {
            return self.pending_surface.take();
        }
        let projection = self.pending_projection.as_ref()?;
        if !projection.matches_surface(surface, snapshot.focused_tab_id.as_deref()) {
            return None;
        }
        self.active_header = projection
            .body
            .headers
            .iter()
            .find(|header| header.collapsed && header.focused)
            .map(|header| header.path.clone());
        self.current_surface_revision = surface.surface_revision;
        self.current = self.pending_projection.take();
        self.pending_surface.take()
    }
    pub fn coherent(&self) -> bool {
        self.pending_surface.is_none()
            && self.pending_projection.is_none()
            && (self.current.is_some() || self.disabled.is_some())
    }
}

impl ClientShellState {
    pub(crate) fn set_endpoint_collapse_projection_for_generation(
        &mut self,
        endpoint_id: &ClientEndpointId,
        generation: u64,
        projection: Result<Box<CollapseProjectionV1>, String>,
    ) {
        let Some(endpoint) = self
            .endpoints
            .iter_mut()
            .find(|endpoint| &endpoint.endpoint_id == endpoint_id)
        else {
            return;
        };
        if !endpoint
            .capabilities
            .as_ref()
            .is_some_and(|capabilities| capabilities.contains(LAYOUT_COLLAPSE_CAPABILITY))
        {
            return;
        }
        if endpoint
            .snapshot_generation
            .is_some_and(|current| generation < current)
        {
            return;
        }
        if let Ok(projection) = &projection {
            if endpoint.snapshot_generation == Some(generation)
                && endpoint
                    .snapshot
                    .as_ref()
                    .is_some_and(|snapshot| snapshot.boot_id != projection.boot_id)
            {
                return;
            }
        }
        if !endpoint.collapse.advance_generation(generation) {
            return;
        }
        match projection {
            Ok(projection) => {
                if let Err(reason) = endpoint.collapse.offer_projection(generation, projection) {
                    endpoint.collapse.disable(reason);
                }
            }
            Err(reason) => endpoint.collapse.disable(&reason),
        }
        if endpoint_id == &self.active_endpoint_id {
            self.try_install_collapse_pair();
        }
    }

    pub(crate) fn set_pane_surface_with_views_for_generation(
        &mut self,
        endpoint_id: &ClientEndpointId,
        generation: u64,
        surface: PaneSurfaceFrame,
        views: Vec<crate::protocol::PaneSurfaceViewEntryV2>,
    ) {
        if endpoint_id != &self.active_endpoint_id {
            return;
        }
        if !self.supports_endpoint_capability(LAYOUT_COLLAPSE_CAPABILITY) {
            self.set_pane_surface_with_views(surface, views);
            return;
        }
        let Some(endpoint) = self
            .endpoints
            .iter_mut()
            .find(|endpoint| &endpoint.endpoint_id == endpoint_id)
        else {
            return;
        };
        if endpoint
            .snapshot_generation
            .is_some_and(|current| generation < current)
        {
            return;
        }
        if endpoint.snapshot_generation == Some(generation)
            && endpoint.snapshot.as_ref().is_some_and(|snapshot| {
                snapshot.boot_id != surface.boot_id
                    || surface.projection_revision < snapshot.revision
            })
        {
            return;
        }
        endpoint.collapse.offer_surface(generation, surface, views);
        self.hits = ShellHitMap::default();
        self.try_install_collapse_pair();
    }

    pub(super) fn try_install_collapse_pair(&mut self) {
        if !self.supports_endpoint_capability(LAYOUT_COLLAPSE_CAPABILITY) {
            return;
        }
        let ready = self
            .endpoints
            .iter_mut()
            .find(|endpoint| endpoint.endpoint_id == self.active_endpoint_id)
            .and_then(|endpoint| {
                let snapshot = endpoint.snapshot.as_deref()?;
                if let Some(generation) = endpoint.snapshot_generation {
                    if !endpoint.collapse.advance_generation(generation) {
                        return None;
                    }
                }
                endpoint.collapse.take_ready(snapshot)
            });
        if let Some((surface, views)) = ready {
            self.install_pane_surface(surface, views, false);
        }
    }

    pub(super) fn collapse_controls_available(&self) -> bool {
        self.collapse_projection()
            .is_some_and(|projection| projection.body.enabled)
            && self.supports_endpoint_capability(crate::native_api::NATIVE_API_CAPABILITY_V1)
            && self
                .endpoints
                .iter()
                .find(|endpoint| endpoint.endpoint_id == self.active_endpoint_id)
                .and_then(|endpoint| endpoint.methods.as_ref())
                .is_some_and(|methods| {
                    methods.contains("layout.collapse") && methods.contains("layout.expand")
                })
    }

    pub(super) fn collapse_allows_pane_input(&self, target: &ClientInputTarget) -> bool {
        let ClientInputTarget::Pane(pane) = target else {
            return true;
        };
        if !self.supports_endpoint_capability(LAYOUT_COLLAPSE_CAPABILITY) {
            return true;
        }
        self.collapse_interactions_current()
            && (self
                .pane_surface
                .as_ref()
                .is_some_and(|surface| surface.panes.iter().any(|body| &body.pane_id == pane))
                || (self.collection_entered_pane.as_deref() == Some(pane)
                    && self
                        .hits
                        .collection_members
                        .iter()
                        .any(|member| &member.pane_id == pane && !member.preview.is_empty())))
    }

    pub(super) fn render_collapse_affordances(
        &self,
        buffer: &mut ratatui::buffer::Buffer,
        viewport: Rect,
    ) -> Vec<CollapseHeaderHit> {
        let mut hits = Vec::new();
        if self.collapse_controls_available() {
            if let Some(projection) = self.collapse_projection() {
                for (index, header) in projection.body.headers.iter().enumerate() {
                    let action = header.action_rect;
                    let rect = Rect::new(
                        viewport.x.saturating_add(action.x),
                        viewport.y.saturating_add(action.y),
                        action.width,
                        action.height,
                    )
                    .intersection(viewport)
                    .intersection(buffer.area);
                    if rect.is_empty() {
                        continue;
                    }
                    if !header.collapsed {
                        buffer.set_stringn(
                            rect.x,
                            rect.y,
                            "▾",
                            1,
                            Style::default().fg(self.config.palette.overlay0),
                        );
                    }
                    hits.push(CollapseHeaderHit { rect, index });
                }
            }
        }
        hits
    }

    pub(super) fn request_collapse_target(
        &mut self,
        target: crate::api::schema::LayoutCollapseTarget,
        collapsed: bool,
        outcome: &mut ClientShellInput,
    ) {
        let current = self.collapse_projection().is_some_and(|projection| {
            projection.boot_id == target.boot_id
                && projection.body.tab_id.as_deref() == Some(target.tab_id.as_str())
                && projection.body.tree_fingerprint.as_deref()
                    == Some(target.tree_fingerprint.as_str())
        });
        if !self.collapse_controls_available() || !current {
            self.receive_endpoint_unavailable(
                "Collapse target is unavailable or changed; reopen its header or menu".into(),
            );
            outcome.repaint = true;
            return;
        }
        let method = if collapsed {
            crate::api::schema::Method::LayoutCollapse(target)
        } else {
            crate::api::schema::Method::LayoutExpand(target)
        };
        self.push_endpoint_method(method, outcome);
    }

    fn collapse_target(
        &self,
        path: Vec<LayoutPathStep>,
    ) -> Option<crate::api::schema::LayoutCollapseTarget> {
        let projection = self.collapse_projection()?;
        Some(crate::api::schema::LayoutCollapseTarget {
            boot_id: projection.boot_id.clone(),
            tab_id: projection.body.tab_id.clone()?,
            tree_fingerprint: projection.body.tree_fingerprint.clone()?,
            path,
        })
    }

    pub(super) fn set_pane_collapsed(
        &mut self,
        pane_id: Option<&str>,
        collapsed: bool,
        outcome: &mut ClientShellInput,
    ) {
        let path = self.collapse_projection().and_then(|projection| {
            let pane = pane_id.or_else(|| self.snapshot.as_ref()?.focused_pane_id.as_deref())?;
            projection
                .body
                .anchors
                .iter()
                .find(|anchor| anchor.pane_id == pane)
                .map(|anchor| anchor.path.clone())
        });
        if let Some(target) = path.and_then(|path| self.collapse_target(path)) {
            self.request_collapse_target(target, collapsed, outcome);
        }
    }

    pub(super) fn toggle_focused_collapse(&mut self, outcome: &mut ClientShellInput) {
        let active = self
            .endpoints
            .iter()
            .find(|endpoint| endpoint.endpoint_id == self.active_endpoint_id)
            .and_then(|endpoint| endpoint.collapse.active_header.clone());
        if let Some(target) = active.and_then(|path| self.collapse_target(path)) {
            self.request_collapse_target(target, false, outcome);
        } else {
            self.set_pane_collapsed(None, true, outcome);
        }
    }

    pub(super) fn open_collapse_box_picker(
        &mut self,
        pane_id: Option<&str>,
        x: u16,
        y: u16,
        outcome: &mut ClientShellInput,
    ) {
        if !self.collapse_controls_available() {
            return;
        }
        let Some(projection) = self.collapse_projection() else {
            return;
        };
        let active = self
            .endpoints
            .iter()
            .find(|endpoint| endpoint.endpoint_id == self.active_endpoint_id)
            .and_then(|endpoint| endpoint.collapse.active_header.as_ref());
        let pane = pane_id.or_else(|| {
            self.snapshot
                .as_ref()
                .and_then(|snapshot| snapshot.focused_pane_id.as_deref())
        });
        let pane_anchor = pane.and_then(|pane| {
            projection
                .body
                .anchors
                .iter()
                .find(|anchor| anchor.pane_id == pane)
                .map(|anchor| &anchor.path)
        });
        if pane_id.is_some() && pane_anchor.is_none() {
            return;
        }
        let anchor = pane_anchor.or(active);
        let mut paths = projection
            .body
            .containers
            .iter()
            .filter(|container| {
                anchor.is_some_and(|anchor| {
                    anchor.starts_with(&container.path) && container.path.len() < anchor.len()
                }) || (anchor.is_none() && container.path.is_empty())
            })
            .map(|container| container.path.clone())
            .collect::<Vec<_>>();
        paths.sort_by_key(|path| std::cmp::Reverse(path.len()));
        paths.dedup();
        let targets = paths
            .into_iter()
            .filter_map(|path| self.collapse_target(path))
            .collect::<Vec<_>>();
        if targets.is_empty() {
            return;
        }
        self.overlay = Some(ClientShellOverlay::ContextMenu(ClientContextMenuOverlay {
            target: ClientContextMenuTarget::CollapseBoxes { targets },
            x,
            y,
            highlighted: 0,
        }));
        outcome.repaint = true;
    }

    pub(super) fn handle_collapse_header_mouse(
        &mut self,
        mouse: crossterm::event::MouseEvent,
        outcome: &mut ClientShellInput,
    ) -> bool {
        use crossterm::event::{MouseButton, MouseEventKind};
        if !self.collapse_interactions_current() {
            return !matches!(mouse.kind, MouseEventKind::Up(_))
                && self.last_composed_size.is_some_and(|(cols, rows)| {
                    super::contains(
                        self.layout(cols, rows).pane_surface,
                        (mouse.column, mouse.row),
                    )
                });
        }
        if !matches!(mouse.kind, MouseEventKind::Down(_)) {
            return false;
        }
        let Some(index) = self
            .hits
            .collapse_headers
            .iter()
            .find(|hit| super::contains(hit.rect, (mouse.column, mouse.row)))
            .map(|hit| hit.index)
        else {
            return false;
        };
        let Some(header) = self
            .collapse_projection()
            .and_then(|projection| projection.body.headers.get(index))
            .cloned()
        else {
            return true;
        };
        if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
            if let Some(target) = self.collapse_target(header.path) {
                self.request_collapse_target(target, !header.collapsed, outcome);
            }
        } else if header.collapsed && mouse.kind == MouseEventKind::Down(MouseButton::Right) {
            let pane = self
                .collapse_projection()
                .and_then(|projection| {
                    projection
                        .body
                        .anchors
                        .iter()
                        .find(|anchor| anchor.path == header.path)
                })
                .map(|anchor| anchor.pane_id.clone());
            if let Some(pane) = pane {
                self.open_pane_context_menu(pane, mouse.column, mouse.row);
                if let Some(ClientShellOverlay::ContextMenu(ClientContextMenuOverlay {
                    target: ClientContextMenuTarget::Pane { collapsed, .. },
                    ..
                })) = self.overlay.as_mut()
                {
                    *collapsed = true;
                }
            } else if let Some(target) = self.collapse_target(header.path) {
                self.overlay = Some(ClientShellOverlay::ContextMenu(ClientContextMenuOverlay {
                    target: ClientContextMenuTarget::CollapsedHeader { target },
                    x: mouse.column,
                    y: mouse.row,
                    highlighted: 0,
                }));
            }
            outcome.repaint = true;
        } else if !header.collapsed {
            return false;
        }
        true
    }

    pub(super) fn advance_collapse_patch(
        &mut self,
        boot_id: &str,
        projection_revision: u64,
        surface_revision: u64,
    ) {
        if let Some(endpoint) = self
            .endpoints
            .iter_mut()
            .find(|endpoint| endpoint.endpoint_id == self.active_endpoint_id)
        {
            if endpoint
                .collapse
                .current
                .as_ref()
                .is_some_and(|projection| {
                    projection.boot_id == boot_id
                        && projection.projection_revision == projection_revision
                })
            {
                endpoint.collapse.current_surface_revision = surface_revision;
            }
        }
    }

    pub(super) fn collapse_projection(&self) -> Option<&CollapseProjectionV1> {
        if !self.supports_endpoint_capability(LAYOUT_COLLAPSE_CAPABILITY) {
            return None;
        }
        let endpoint = self
            .endpoints
            .iter()
            .find(|endpoint| endpoint.endpoint_id == self.active_endpoint_id)?;
        if !endpoint.collapse.coherent() {
            return None;
        }
        let projection = endpoint.collapse.current.as_deref()?;
        let snapshot = self.snapshot.as_deref()?;
        let surface = self.pane_surface.as_ref()?;
        (projection.boot_id == snapshot.boot_id
            && projection.projection_revision == snapshot.revision
            && projection.boot_id == surface.boot_id
            && projection.projection_revision == surface.projection_revision
            && endpoint.collapse.current_surface_revision == surface.surface_revision)
            .then_some(projection)
    }

    pub(super) fn collapse_interactions_current(&self) -> bool {
        if !self.supports_endpoint_capability(LAYOUT_COLLAPSE_CAPABILITY) {
            return true;
        }
        let coherent = self
            .endpoints
            .iter()
            .find(|endpoint| endpoint.endpoint_id == self.active_endpoint_id)
            .is_some_and(|endpoint| endpoint.collapse.coherent());
        coherent
            && self
                .snapshot
                .as_ref()
                .zip(self.pane_surface.as_ref())
                .is_some_and(|(snapshot, surface)| {
                    snapshot.boot_id == surface.boot_id
                        && snapshot.revision == surface.projection_revision
                })
    }
}
