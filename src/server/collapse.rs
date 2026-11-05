//! Optional layout metadata derived from the same geometry as the pane surface.
use crate::{
    app::App,
    layout::TypedNode,
    protocol::{collapse::*, SurfaceRect},
    ui::TabSurfaceLayout,
};
use ratatui::{layout::Rect, widgets::Borders};
use std::collections::HashSet;

pub(super) fn projection_body(app: &App, layout: &TabSurfaceLayout, area: Rect) -> CollapseBodyV1 {
    let mut body = CollapseBodyV1 {
        tab_id: None,
        tree_fingerprint: None,
        width: area.width,
        height: area.height,
        enabled: false,
        unavailable: Some("No active tab".into()),
        headers: Vec::new(),
        anchors: Vec::new(),
        containers: Vec::new(),
    };
    let Some(target) = layout.target else {
        return body;
    };
    let Some(tab) = app
        .state
        .workspaces
        .get(target.workspace_index)
        .and_then(|workspace| workspace.tabs.get(target.tab_index))
    else {
        return body;
    };
    body.tab_id = app.public_tab_id(target.workspace_index, target.tab_index);
    body.tree_fingerprint = Some(tab.layout.tree_fingerprint().to_owned());
    body.enabled = true;
    body.unavailable = None;
    if layout
        .collapsed_frames
        .len()
        .saturating_add(layout.container_geometry.len())
        .saturating_add(layout.pane_infos.len())
        > MAX_COLLAPSE_ITEMS
    {
        body.disable("Collapse controls exceed the bounded projection budget");
        return body;
    }
    let mut occupied = HashSet::new();
    for header in &layout.collapsed_frames {
        if header.rect.width == 0 || header.rect.height == 0 {
            continue;
        }
        let rect: SurfaceRect = header.rect.into();
        occupied.insert((rect.x, rect.y));
        body.headers.push(CollapseHeaderV1 {
            rect,
            action_rect: rect,
            path: header.path.clone(),
            collapsed: true,
            focused: header.focused,
        });
    }
    for container in &layout.container_geometry {
        body.containers.push(CollapseContainerV1 {
            path: container.path.clone(),
            stack: container.stack,
        });
        if container.framed {
            add_header(
                &mut body,
                &mut occupied,
                container.rect,
                container.path.clone(),
            );
        }
    }
    for pane in &layout.pane_infos {
        let Some(pane_id) = app.public_pane_id(target.workspace_index, pane.id) else {
            continue;
        };
        let path = layout.pane_paths.get(&pane.id).cloned().or_else(|| {
            // Zoom has one visible pane and intentionally skips ordinary box
            // geometry. One narrow structural lookup, never one per hidden pane.
            let indices = tab.layout.pane_path(pane.id)?;
            let path = tab.layout.tagged_path(&indices)?;
            for depth in 0..indices.len() {
                if let Some(node) = tab.layout.typed_root().at(&indices[..depth]) {
                    body.containers.push(CollapseContainerV1 {
                        path: path[..depth].to_vec(),
                        stack: matches!(node.content(), TypedNode::Stack { .. }),
                    });
                }
            }
            Some(path)
        });
        let Some(path) = path else {
            continue;
        };
        body.anchors.push(CollapseAnchorV1 {
            pane_id,
            path: path.clone(),
        });
        if pane.borders.contains(Borders::TOP) {
            add_header(&mut body, &mut occupied, pane.rect, path);
        }
    }
    body
}

pub(super) fn projection_message(
    boot_id: &str,
    projection_revision: u64,
    base_surface_revision: u64,
    body: &CollapseBodyV1,
) -> Result<crate::protocol::ServerMessage, serde_json::Error> {
    let mut projection = CollapseProjectionV1 {
        boot_id: boot_id.into(),
        projection_revision,
        base_surface_revision,
        body: body.clone(),
    };
    if let Err(reason) = projection.validate() {
        projection.body.disable(reason);
    }
    let mut data = serde_json::to_string(&projection)?;
    if data.len() > crate::protocol::MAX_FRAME_SIZE / 2 {
        projection
            .body
            .disable("Collapse controls exceed the bounded projection byte budget");
        data = serde_json::to_string(&projection)?;
    }
    Ok(crate::protocol::ServerMessage::EndpointControl {
        kind: LAYOUT_COLLAPSE_KIND.into(),
        data,
    })
}

fn add_header(
    body: &mut CollapseBodyV1,
    occupied: &mut HashSet<(u16, u16)>,
    rect: Rect,
    path: Vec<crate::layout::LayoutPathStep>,
) {
    if rect.width == 0 || rect.height == 0 {
        return;
    }
    let x = rect.x.saturating_add(u16::from(rect.width > 2));
    if !occupied.insert((x, rect.y)) {
        return;
    }
    body.headers.push(CollapseHeaderV1 {
        rect: SurfaceRect {
            x: rect.x,
            y: rect.y,
            width: rect.width,
            height: 1,
        },
        action_rect: SurfaceRect {
            x,
            y: rect.y,
            width: 1,
            height: 1,
        },
        path,
        collapsed: false,
        focused: false,
    });
}
