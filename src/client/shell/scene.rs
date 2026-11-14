//! Validate the entire publication before mutating any shell projection.
use crate::client::{endpoint, shell};
use crate::protocol::{scene::ClientSceneV1, ClientShellSnapshot};

pub(crate) struct DecodedScene {
    pub snapshot: Box<ClientShellSnapshot>,
    pub projections: Vec<endpoint::EndpointControlMessage>,
    pub surface: crate::protocol::PaneSurfaceFrameV2,
    pub route_epoch: u64,
    pub targets: Vec<crate::protocol::scene::SceneTargetV1>,
}

impl DecodedScene {
    pub fn decode(scene: ClientSceneV1) -> Result<Self, String> {
        use endpoint::EndpointControlMessage as C;
        let mut snapshot = None;
        let mut projections = Vec::new();
        let mut kinds = std::collections::HashSet::new();
        let boot = &scene.surface.frame.boot_id;
        let revision = scene.surface.frame.projection_revision;
        if scene.route_epoch == 0 || boot.is_empty() || scene.projections.len() > 6 {
            return Err("invalid scene identity or projection count".into());
        }
        for projection in scene.projections {
            if !kinds.insert(projection.kind.clone()) {
                return Err("duplicate scene projection".into());
            }
            let decoded = endpoint::decode_endpoint_control(&projection.kind, &projection.data)?;
            let matches = match &decoded {
                C::Snapshot(value) => value.boot_id == *boot && value.revision == revision,
                C::Structure(value) => value.boot_id == *boot && value.revision == revision,
                C::AgentViewProjection(value) => {
                    value.boot_id == *boot && value.revision == revision
                }
                C::HostDocuments(value) => value.boot_id == *boot && value.revision == revision,
                C::TerminalViewsSurface(value) => {
                    value.boot_id == *boot && value.projection_revision == revision
                }
                C::CollapseProjection(Ok(value)) => {
                    value.validate().map_err(str::to_owned)?;
                    value.boot_id == *boot
                        && value.projection_revision == revision
                        && value.base_surface_revision == scene.surface.frame.surface_revision
                        && value.body.width == scene.surface.frame.frame.width
                        && value.body.height == scene.surface.frame.frame.height
                }
                _ => false,
            };
            if !matches {
                return Err("invalid or mismatched scene projection".into());
            }
            if let C::Snapshot(value) = decoded {
                snapshot = Some(value);
            } else {
                projections.push(decoded);
            }
        }
        let frame = &scene.surface.frame;
        if frame.frame.cells.len()
            != usize::from(frame.frame.width) * usize::from(frame.frame.height)
        {
            return Err("invalid scene raster dimensions".into());
        }
        let mut pane_ids = std::collections::HashSet::new();
        for pane in &frame.panes {
            if !pane_ids.insert(&pane.pane_id)
                || pane.pane_id.is_empty()
                || u32::from(pane.rect.x) + u32::from(pane.rect.width)
                    > u32::from(frame.frame.width)
                || u32::from(pane.rect.y) + u32::from(pane.rect.height)
                    > u32::from(frame.frame.height)
            {
                return Err("invalid scene pane geometry".into());
            }
        }
        let mut target_panes = std::collections::HashSet::new();
        let mut terminal_ids = std::collections::HashSet::new();
        for target in &scene.targets {
            if target.terminal_id.is_empty()
                || !terminal_ids.insert(&target.terminal_id)
                || !target_panes.insert(&target.pane_id)
            {
                return Err("invalid scene target descriptors".into());
            }
        }
        if scene
            .surface
            .views
            .iter()
            .any(|view| !pane_ids.contains(&view.pane_id))
            || frame.panes.iter().any(|pane| {
                !scene
                    .surface
                    .views
                    .iter()
                    .any(|view| view.pane_id == pane.pane_id)
                    && !scene
                        .targets
                        .iter()
                        .any(|target| target.pane_id.as_ref() == Some(&pane.pane_id))
            })
        {
            return Err("scene is missing target descriptors".into());
        }
        let snapshot = snapshot.ok_or("scene is missing its snapshot")?;
        for projection in &projections {
            if let C::CollapseProjection(Ok(value)) = projection {
                if !value.matches_surface(&scene.surface.frame, snapshot.focused_tab_id.as_deref())
                {
                    return Err("scene collapse geometry does not match its surface".into());
                }
            }
        }
        Ok(Self {
            snapshot,
            projections,
            surface: scene.surface,
            route_epoch: scene.route_epoch,
            targets: scene.targets,
        })
    }
}

/// Connection-local monotonic revocation; neither patches nor stale full scenes reopen it.
#[derive(Clone, Debug, Default)]
pub(crate) struct SceneAuthority {
    generation: Option<u64>,
    installed: Option<u64>,
    revoked: u64,
}

impl SceneAuthority {
    fn advance(&mut self, generation: u64) -> bool {
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
    pub fn revoke(&mut self, generation: u64, epoch: u64) {
        if self.advance(generation) {
            self.revoked = self.revoked.max(epoch);
        }
    }
    pub fn can_install(&mut self, generation: u64, epoch: u64) -> bool {
        self.advance(generation)
            && epoch >= self.revoked
            && self.installed.is_none_or(|old| epoch >= old)
    }
    pub fn install(&mut self, epoch: u64) {
        self.installed = Some(epoch);
    }
    pub fn current(&self) -> bool {
        self.installed.is_none_or(|epoch| epoch >= self.revoked)
            && (self.revoked == 0 || self.installed.is_some())
    }
}

impl shell::ClientShellState {
    pub(super) fn scene_target_current(&self, target: &shell::ClientInputTarget) -> bool {
        let Some(endpoint) = self
            .endpoints
            .iter()
            .find(|endpoint| endpoint.endpoint_id == self.active_endpoint_id)
        else {
            return false;
        };
        if endpoint.scene_authority.installed.is_none() {
            return endpoint.scene_authority.current();
        }
        endpoint.scene_authority.current()
            && endpoint.scene_targets.iter().any(|descriptor| {
                descriptor.runtime_instance.is_some()
                    && match target {
                        shell::ClientInputTarget::Pane(id) => {
                            descriptor.pane_id.as_ref() == Some(id)
                        }
                        shell::ClientInputTarget::Popup(id) => {
                            descriptor.pane_id.is_none() && descriptor.terminal_id == *id
                        }
                    }
            })
    }

    pub(super) fn scene_route_current(&self) -> bool {
        self.endpoints
            .iter()
            .find(|endpoint| endpoint.endpoint_id == self.active_endpoint_id)
            .is_none_or(|endpoint| endpoint.scene_authority.current())
    }

    pub(crate) fn revoke_scene_route(
        &mut self,
        endpoint_id: &endpoint::ClientEndpointId,
        generation: u64,
        revoked: crate::protocol::scene::SceneRouteRevokedV1,
    ) {
        let Some(endpoint) = self
            .endpoints
            .iter_mut()
            .find(|endpoint| &endpoint.endpoint_id == endpoint_id)
        else {
            return;
        };
        if endpoint
            .snapshot
            .as_ref()
            .is_some_and(|snapshot| snapshot.boot_id == revoked.boot_id)
        {
            endpoint
                .scene_authority
                .revoke(generation, revoked.route_epoch);
        }
    }

    /// No composition, paint or event enqueue occurs between these setters.
    pub(crate) fn install_scene(
        &mut self,
        endpoint_id: &endpoint::ClientEndpointId,
        generation: u64,
        scene: DecodedScene,
        active: bool,
    ) -> bool {
        use endpoint::EndpointControlMessage as C;
        if active
            && self.active_endpoint_id == *endpoint_id
            && self.pane_surface.as_ref().is_some_and(|current| {
                current.boot_id == scene.surface.frame.boot_id
                    && current.surface_revision >= scene.surface.frame.surface_revision
            })
        {
            return false;
        }
        let Some(endpoint) = self
            .endpoints
            .iter_mut()
            .find(|endpoint| &endpoint.endpoint_id == endpoint_id)
        else {
            return false;
        };
        if endpoint
            .snapshot_generation
            .is_some_and(|old| generation < old)
            || (endpoint.snapshot_generation == Some(generation)
                && endpoint.snapshot.as_ref().is_some_and(|old| {
                    old.boot_id != scene.snapshot.boot_id || old.revision > scene.snapshot.revision
                }))
        {
            return false;
        }
        let required = endpoint.capabilities.as_ref();
        for (capability, kind) in [
            (
                crate::protocol::collapse::LAYOUT_COLLAPSE_CAPABILITY,
                crate::protocol::collapse::LAYOUT_COLLAPSE_KIND,
            ),
            (
                crate::protocol::endpoint::SESSION_STRUCTURE_CAPABILITY,
                crate::protocol::endpoint::SESSION_STRUCTURE_KIND,
            ),
            (
                crate::protocol::endpoint::HOST_DOCUMENTS_KIND,
                crate::protocol::endpoint::HOST_DOCUMENTS_KIND,
            ),
        ] {
            let present = scene.projections.iter().any(|projection| {
                matches!(
                    (projection, kind),
                    (
                        C::CollapseProjection(_),
                        crate::protocol::collapse::LAYOUT_COLLAPSE_KIND
                    ) | (
                        C::Structure(_),
                        crate::protocol::endpoint::SESSION_STRUCTURE_KIND
                    ) | (
                        C::HostDocuments(_),
                        crate::protocol::endpoint::HOST_DOCUMENTS_KIND
                    )
                )
            });
            if required.is_some_and(|caps| caps.contains(capability)) && !present {
                return false;
            }
        }
        if !endpoint
            .scene_authority
            .can_install(generation, scene.route_epoch)
        {
            return false;
        }
        // Retire pending legacy pair fragments: the transaction is its own full base.
        endpoint.collapse = Default::default();
        endpoint.scene_targets = scene.targets;
        if active {
            self.set_endpoint_snapshot_for_generation(endpoint_id, generation, scene.snapshot);
        } else {
            self.cache_endpoint_snapshot_inactive_for_generation(
                endpoint_id,
                generation,
                scene.snapshot,
            );
        }
        for projection in scene.projections {
            match projection {
                C::Structure(value) => {
                    self.set_endpoint_structure_for_generation(endpoint_id, generation, value)
                }
                C::AgentViewProjection(value) => self
                    .set_endpoint_agent_view_projection_for_generation(
                        endpoint_id,
                        generation,
                        value,
                    ),
                C::HostDocuments(value) => {
                    self.set_endpoint_host_documents_for_generation(endpoint_id, generation, value)
                }
                C::CollapseProjection(value) => self
                    .set_endpoint_collapse_projection_for_generation(
                        endpoint_id,
                        generation,
                        value,
                    ),
                C::TerminalViewsSurface(value) => {
                    self.set_terminal_views_surface_for_generation(endpoint_id, generation, *value);
                }
                _ => unreachable!("validated scene projections"),
            }
        }
        if active {
            self.set_pane_surface_with_views_for_generation(
                endpoint_id,
                generation,
                scene.surface.frame,
                scene.surface.views,
            );
        }
        if let Some(endpoint) = self
            .endpoints
            .iter_mut()
            .find(|endpoint| &endpoint.endpoint_id == endpoint_id)
        {
            endpoint.scene_authority.install(scene.route_epoch);
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stale_scene_cannot_clear_newer_route_revocation_or_connection() {
        let mut authority = SceneAuthority::default();
        assert!(authority.can_install(1, 1));
        authority.install(1);
        authority.revoke(1, 2);
        assert!(!authority.current());
        assert!(!authority.can_install(1, 1));
        assert!(authority.can_install(1, 2));
        authority.install(2);
        assert!(authority.current());
        authority.revoke(1, 1);
        assert!(authority.current());
        assert!(authority.can_install(2, 1));
        authority.install(1);
        authority.revoke(1, 99);
        assert!(authority.current());
        assert!(!authority.can_install(1, 99));
    }
}
