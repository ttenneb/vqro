//! Publication-time routing inventory. Never reached from key/PTY-byte handling.
use super::*;
use crate::protocol::scene::{SceneProjectionV1, SceneTargetV1};
use sha2::{Digest, Sha256};

pub(super) fn projection(message: ServerMessage) -> Option<SceneProjectionV1> {
    let ServerMessage::EndpointControl { kind, data } = message else {
        return None;
    };
    Some(SceneProjectionV1 { kind, data })
}

pub(super) fn projections(
    client: &crate::server::clients::ClientConnection,
) -> Option<Vec<SceneProjectionV1>> {
    let snapshot = client.shell_snapshot.as_ref()?;
    let mut messages = vec![
        crate::protocol::endpoint::snapshot_message(snapshot).ok()?,
        crate::protocol::endpoint::agent_view_projection_message(
            &snapshot.boot_id,
            snapshot.revision,
            client.shell_agent_view.as_ref(),
        )
        .ok()?,
    ];
    if let Some(structure) = client.shell_structure.as_ref() {
        messages.push(crate::protocol::endpoint::structure_message(structure).ok()?);
    }
    if client.shell_host_documents_enabled {
        messages.push(
            crate::protocol::endpoint::host_documents_message(
                &crate::protocol::endpoint::HostDocumentsProjectionV1 {
                    boot_id: snapshot.boot_id.clone(),
                    revision: snapshot.revision,
                    documents: client.shell_host_documents.clone(),
                },
            )
            .ok()?,
        );
    }
    messages.into_iter().map(projection).collect()
}

/// The single construction seam for full and retained-graphics publication.
pub(super) fn wrap_full_scene(
    client: &crate::server::clients::ClientConnection,
    prepared: crate::server::render_stream::PreparedRender,
    collapse: Option<&crate::protocol::collapse::CollapseBodyV1>,
    previews: Option<&crate::protocol::endpoint::ServerTerminalViewsSurface>,
    targets: Vec<SceneTargetV1>,
) -> Option<crate::server::render_stream::PreparedRender> {
    let ServerMessage::PaneSurfaceV2(surface) = prepared.message() else {
        return None;
    };
    let snapshot = client.shell_snapshot.as_ref()?;
    if snapshot.boot_id != surface.frame.boot_id
        || snapshot.revision != surface.frame.projection_revision
    {
        return None;
    }
    let mut projections = projections(client)?;
    if let Some(body) = collapse {
        projections.push(projection(
            crate::server::collapse::projection_message(
                &surface.frame.boot_id,
                surface.frame.projection_revision,
                surface.frame.surface_revision,
                body,
            )
            .ok()?,
        )?);
    }
    if let Some(previews) = previews {
        projections.push(SceneProjectionV1 {
            kind: crate::protocol::endpoint::TERMINAL_VIEWS_SURFACE_KIND.into(),
            data: serde_json::to_string(previews).ok()?,
        });
    }
    Some(prepared.with_scene(client.shell_route_epoch, projections, targets))
}

pub(super) fn revoke_route(
    client: &mut crate::server::clients::ClientConnection,
    boot_id: &str,
) -> bool {
    let Some(epoch) = client.shell_route_epoch.checked_add(1) else {
        return false;
    };
    let revoked = crate::protocol::scene::SceneRouteRevokedV1 {
        boot_id: boot_id.to_owned(),
        route_epoch: epoch,
    };
    let framed = serde_json::to_string(&revoked).ok().and_then(|data| {
        HeadlessServer::frame_server_message(&ServerMessage::EndpointControl {
            kind: crate::protocol::scene::ROUTE_REVOKED_KIND.into(),
            data,
        })
        .ok()
    });
    if framed.is_none_or(|frame| {
        client
            .writer
            .as_ref()
            .is_none_or(|writer| writer.control.send(frame).is_err())
    }) {
        return false;
    }
    client.shell_route_epoch = epoch;
    true
}

impl HeadlessServer {
    pub(super) fn scene_targets(
        &self,
        snapshot: &crate::protocol::ClientShellSnapshot,
    ) -> Vec<SceneTargetV1> {
        let mut targets = Vec::new();
        for pane in snapshot
            .panes
            .iter()
            .filter(|pane| Some(&pane.tab_id) == snapshot.focused_tab_id.as_ref())
        {
            let Some((ws, id)) = self.app.parse_pane_id(&pane.pane_id) else {
                continue;
            };
            let Some(terminal) = self.app.state.workspaces[ws].terminal_id(id) else {
                continue;
            };
            let runtime =
                self.app
                    .state
                    .runtime_for_pane_in_workspace(&self.app.terminal_runtimes, ws, id);
            targets.push(SceneTargetV1 {
                pane_id: Some(pane.pane_id.clone()),
                terminal_id: terminal.to_string(),
                runtime_instance: runtime
                    .and_then(crate::terminal::TerminalRuntime::scene_runtime_instance),
            });
        }
        if let Some(popup) = self
            .app
            .state
            .popup_pane
            .as_ref()
            .filter(|_| self.popup_owner_tab_id == snapshot.focused_tab_id)
        {
            targets.push(SceneTargetV1 {
                pane_id: None,
                terminal_id: popup.terminal_id.to_string(),
                runtime_instance: self
                    .app
                    .terminal_runtimes
                    .get(&popup.terminal_id)
                    .and_then(crate::terminal::TerminalRuntime::scene_runtime_instance),
            });
        }
        targets
    }
}

/// Hash only enumerated routing facts. In particular, no cells, content sequence,
/// status, titles, document bodies, graphics bytes or cleanup-eligibility epoch.
pub(super) struct SceneRoutingFacts<'a> {
    pub snapshot: &'a crate::protocol::ClientShellSnapshot,
    pub structure: Option<&'a crate::protocol::endpoint::ClientShellStructureProjection>,
    pub surface: &'a crate::protocol::PaneSurfaceFrame,
    pub views: &'a [crate::protocol::PaneSurfaceViewEntryV2],
    pub collapse: Option<&'a crate::protocol::collapse::CollapseBodyV1>,
    pub targets: &'a [SceneTargetV1],
    pub previews: Option<&'a crate::protocol::endpoint::ServerTerminalViewsSurface>,
    pub tree_fingerprint: Option<&'a str>,
    pub viewport: Option<&'a crate::protocol::endpoint::ClientPaneViewportRequest>,
}

pub(super) fn route_witness(facts: SceneRoutingFacts<'_>) -> Result<[u8; 32], serde_json::Error> {
    let SceneRoutingFacts {
        snapshot,
        structure,
        surface,
        views,
        collapse,
        targets,
        previews,
        tree_fingerprint,
        viewport,
    } = facts;
    let panes = surface
        .panes
        .iter()
        .map(|pane| {
            (
                &pane.pane_id,
                pane.rect,
                pane.inner_rect,
                pane.focused,
                pane.mouse_reporting,
                pane.sgr_pixel_mouse,
                pane.pixel_width,
                pane.pixel_height,
            )
        })
        .collect::<Vec<_>>();
    let snapshot_panes = snapshot
        .panes
        .iter()
        .filter(|pane| Some(&pane.tab_id) == snapshot.focused_tab_id.as_ref())
        .map(|pane| {
            (
                &pane.pane_id,
                &pane.workspace_id,
                &pane.tab_id,
                pane.focused,
                pane.right_click_passthrough,
            )
        })
        .collect::<Vec<_>>();
    let collections = structure
        .into_iter()
        .flat_map(|structure| &structure.collections)
        .filter(|collection| Some(&collection.tab_id) == snapshot.focused_tab_id.as_ref())
        .map(|collection| {
            (
                &collection.collection_id,
                &collection.selected_pane_id,
                &collection.members,
            )
        })
        .collect::<Vec<_>>();
    let popup = surface.popup.as_ref().map(|popup| {
        (
            &popup.terminal_id,
            popup.width,
            popup.height,
            popup.frame.width,
            popup.frame.height,
            popup.mouse_reporting,
            popup.sgr_pixel_mouse,
            popup.pixel_width,
            popup.pixel_height,
        )
    });
    let previews = previews
        .into_iter()
        .flat_map(|surface| &surface.views)
        .map(|view| {
            (
                &view.pane_id,
                view.frame.width,
                view.frame.height,
                view.mouse_reporting,
                view.sgr_pixel_mouse,
                view.pixel_width,
                view.pixel_height,
            )
        })
        .collect::<Vec<_>>();
    let bytes = serde_json::to_vec(&(
        (
            &snapshot.boot_id,
            &snapshot.focused_workspace_id,
            &snapshot.focused_tab_id,
            &snapshot.focused_pane_id,
        ),
        snapshot_panes,
        collections,
        targets,
        (surface.frame.width, surface.frame.height),
        panes,
        views,
        (&surface.collections, &surface.splits),
        popup,
        collapse,
        previews,
        tree_fingerprint,
        viewport.map(|viewport| (&viewport.pane_id, viewport.size)),
    ))?;
    Ok(Sha256::digest(bytes).into())
}
