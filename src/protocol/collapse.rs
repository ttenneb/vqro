//! Optional JSON companion; no published bincode payload reaches these types.
use super::{PaneSurfaceFrame, SurfaceRect};
use crate::layout::LayoutPathStep;
use serde::{Deserialize, Serialize};

pub const LAYOUT_COLLAPSE_KIND: &str = "layout.collapse.v1";
pub const LAYOUT_COLLAPSE_CAPABILITY: &str = "layout_collapse_v1";
pub const MAX_COLLAPSE_ITEMS: usize = 32 * 128;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollapseHeaderV1 {
    pub rect: SurfaceRect,
    pub action_rect: SurfaceRect,
    pub path: Vec<LayoutPathStep>,
    pub collapsed: bool,
    pub focused: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollapseAnchorV1 {
    pub pane_id: String,
    pub path: Vec<LayoutPathStep>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollapseContainerV1 {
    pub path: Vec<LayoutPathStep>,
    pub stack: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollapseBodyV1 {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tree_fingerprint: Option<String>,
    pub width: u16,
    pub height: u16,
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unavailable: Option<String>,
    pub headers: Vec<CollapseHeaderV1>,
    pub anchors: Vec<CollapseAnchorV1>,
    pub containers: Vec<CollapseContainerV1>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollapseProjectionV1 {
    pub boot_id: String,
    pub projection_revision: u64,
    pub base_surface_revision: u64,
    pub body: CollapseBodyV1,
}

impl CollapseBodyV1 {
    pub fn disable(&mut self, reason: &str) {
        self.enabled = false;
        self.unavailable = Some(reason.chars().take(160).collect());
        self.headers.clear();
        self.anchors.clear();
        self.containers.clear();
    }
}
impl CollapseProjectionV1 {
    pub fn validate(&self) -> Result<(), &'static str> {
        let body = &self.body;
        if self.boot_id.is_empty()
            || self.boot_id.len() > 128
            || body
                .tab_id
                .as_ref()
                .is_some_and(|tab| tab.is_empty() || tab.len() > 64)
            || body.tree_fingerprint.as_ref().is_some_and(|fingerprint| {
                fingerprint.len() != 64
                    || !fingerprint
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            })
            || body.tab_id.is_some() != body.tree_fingerprint.is_some()
            || (body.enabled && body.tab_id.is_none())
        {
            return Err("invalid collapse projection identity");
        }
        if body
            .headers
            .len()
            .saturating_add(body.anchors.len())
            .saturating_add(body.containers.len())
            > MAX_COLLAPSE_ITEMS
            || body.unavailable.as_ref().is_some_and(|text| {
                text.chars().count() > 160 || text.chars().any(char::is_control)
            })
        {
            return Err("collapse projection exceeds bounds");
        }
        let contains = |outer: SurfaceRect, inner: SurfaceRect| {
            inner.x >= outer.x
                && inner.y >= outer.y
                && u32::from(inner.x) + u32::from(inner.width)
                    <= u32::from(outer.x) + u32::from(outer.width)
                && u32::from(inner.y) + u32::from(inner.height)
                    <= u32::from(outer.y) + u32::from(outer.height)
        };
        let viewport = SurfaceRect {
            x: 0,
            y: 0,
            width: body.width,
            height: body.height,
        };
        for header in &body.headers {
            if header.path.len() > 128
                || header.rect.width == 0
                || header.rect.height == 0
                || header.action_rect.width == 0
                || header.action_rect.height == 0
                || !contains(viewport, header.rect)
                || !contains(header.rect, header.action_rect)
            {
                return Err("invalid collapse header geometry");
            }
        }
        if body.anchors.iter().any(|anchor| {
            anchor.path.len() > 128 || anchor.pane_id.is_empty() || anchor.pane_id.len() > 64
        }) || body
            .containers
            .iter()
            .any(|container| container.path.len() > 128)
            || (!body.enabled
                && (!body.headers.is_empty()
                    || !body.anchors.is_empty()
                    || !body.containers.is_empty()))
        {
            return Err("invalid collapse targets");
        }
        Ok(())
    }

    pub fn matches_surface(&self, surface: &PaneSurfaceFrame, tab_id: Option<&str>) -> bool {
        self.boot_id == surface.boot_id
            && self.projection_revision == surface.projection_revision
            && self.base_surface_revision == surface.surface_revision
            && self.body.tab_id.as_deref() == tab_id
            && self.body.width == surface.frame.width
            && self.body.height == surface.frame.height
            && self.body.anchors.iter().all(|anchor| {
                surface
                    .panes
                    .iter()
                    .any(|pane| pane.pane_id == anchor.pane_id)
                    || self
                        .body
                        .headers
                        .iter()
                        .any(|header| header.collapsed && header.path == anchor.path)
            })
    }
}
