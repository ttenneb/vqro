//! Pane split panels: Vqro lays out and draws views that extensions publish.
//!
//! Meaning, hierarchy and state text belong to the publishing extension. This
//! module only selects the views published for a pane, lays them out on the
//! pane's right or bottom edge, draws cached content and joins the live
//! terminal and agent facts Vqro owns. Nothing here reads a terminal, the
//! filesystem or a process tree: the work is bounded by the panes on screen and
//! the rows that fit.
use std::collections::HashMap;

use super::render::{display_width, put_text};
use super::*;
use crate::extension_contract::{HostDocumentItem, HostViewEdge, HostViewTone};
use crate::protocol::endpoint::HostDocumentsProjectionV1;

/// Panel size comes from the layout, never from content.
const PANEL_DEFAULT_WIDTH: u16 = 34;
const PANEL_MIN_WIDTH: u16 = 22;
const PANEL_MAX_WIDTH: u16 = 48;
const PANEL_DEFAULT_HEIGHT: u16 = 11;
const PANEL_MIN_HEIGHT: u16 = 7;
const TERMINAL_MIN_WIDTH: u16 = 24;
const TERMINAL_MIN_HEIGHT: u16 = 5;
/// Below this width the panel moves to the bottom edge.
const BOTTOM_EDGE_BELOW_WIDTH: u16 = 56;
const ROW_LINES: u16 = 2;

/// A clickable cell inside a panel.
#[derive(Clone)]
pub(super) struct PanePanelHit {
    pub(super) rect: Rect,
    /// Focus this pane; `None` is a non-interactive cell.
    pub(super) target_pane: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct PanePanelGeometry {
    pub(super) terminal: Rect,
    pub(super) divider: Rect,
    pub(super) panel: Rect,
}

/// What one pane's panel needs from the caller: the terminal rect to claim as
/// the pane's effective viewport, and the hits to register.
pub(super) struct PanePanelRender {
    pub(super) hits: Vec<PanePanelHit>,
    /// The pane whose viewport the panel claims, when a panel is open.
    pub(super) claimed: Option<(String, Rect)>,
}

/// A published view that fills one pane's panel edge.
struct PanelView<'a> {
    title: &'a str,
    count: Option<u32>,
    edge: HostViewEdge,
    /// Direct children of the root, in document order.
    rows: Vec<&'a HostDocumentItem>,
}

/// Split a pane's inner area into a terminal, a divider and the panel, keeping
/// the terminal usable. `None` means the pane is too small for a panel; the
/// caller keeps it closed (a border badge opens it as a popover in a later
/// step).
pub(super) fn geometry(area: Rect, edge: HostViewEdge) -> Option<PanePanelGeometry> {
    let edge = match edge {
        HostViewEdge::Bottom => HostViewEdge::Bottom,
        HostViewEdge::Right if area.width < BOTTOM_EDGE_BELOW_WIDTH => HostViewEdge::Bottom,
        HostViewEdge::Right => HostViewEdge::Right,
    };
    match edge {
        HostViewEdge::Right => {
            if area.height < TERMINAL_MIN_HEIGHT + 2 {
                return None;
            }
            let width = PANEL_DEFAULT_WIDTH
                .clamp(PANEL_MIN_WIDTH, PANEL_MAX_WIDTH)
                .min(area.width.saturating_sub(TERMINAL_MIN_WIDTH + 1));
            if width < PANEL_MIN_WIDTH {
                return None;
            }
            let terminal_width = area.width.saturating_sub(width + 1);
            Some(PanePanelGeometry {
                terminal: Rect::new(area.x, area.y, terminal_width, area.height),
                divider: Rect::new(area.x.saturating_add(terminal_width), area.y, 1, area.height),
                panel: Rect::new(
                    area.x.saturating_add(terminal_width + 1),
                    area.y,
                    width,
                    area.height,
                ),
            })
        }
        HostViewEdge::Bottom => {
            if area.width < TERMINAL_MIN_WIDTH {
                return None;
            }
            let height = PANEL_DEFAULT_HEIGHT
                .clamp(PANEL_MIN_HEIGHT, area.height)
                .min(area.height.saturating_sub(TERMINAL_MIN_HEIGHT + 1));
            if height < PANEL_MIN_HEIGHT {
                return None;
            }
            let terminal_height = area.height.saturating_sub(height + 1);
            Some(PanePanelGeometry {
                terminal: Rect::new(area.x, area.y, area.width, terminal_height),
                divider: Rect::new(area.x, area.y.saturating_add(terminal_height), area.width, 1),
                panel: Rect::new(
                    area.x,
                    area.y.saturating_add(terminal_height + 1),
                    area.width,
                    height,
                ),
            })
        }
    }
}

fn tone_style(tone: HostViewTone, palette: &Palette) -> Style {
    let color = match tone {
        HostViewTone::Default => palette.text,
        HostViewTone::Muted => palette.overlay0,
        HostViewTone::Accent => palette.accent,
        HostViewTone::Success => palette.green,
        HostViewTone::Warning => palette.yellow,
        HostViewTone::Danger => palette.red,
    };
    Style::default().fg(color)
}

fn agent_status_style(status: crate::api::schema::AgentStatus, palette: &Palette) -> (Style, &'static str) {
    match status {
        crate::api::schema::AgentStatus::Working => (Style::default().fg(palette.green), "●"),
        crate::api::schema::AgentStatus::Blocked => (Style::default().fg(palette.red), "●"),
        crate::api::schema::AgentStatus::Idle => (Style::default().fg(palette.yellow), "○"),
        crate::api::schema::AgentStatus::Done => (Style::default().fg(palette.teal), "○"),
        crate::api::schema::AgentStatus::Unknown => (Style::default().fg(palette.overlay0), "·"),
    }
}

/// Elapsed time as the view's age. Vqro owns the rendering of a published
/// instant; the extension owns what the instant means.
fn age_label(since: u64) -> Option<String> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    if since == 0 || since > now.saturating_add(60) {
        return None;
    }
    let seconds = now.saturating_sub(since);
    Some(match seconds {
        0..=59 => format!("{seconds}s"),
        60..=3599 => format!("{}m", seconds / 60),
        3600..=86_399 => format!("{}h", seconds / 3600),
        _ => format!("{}d", seconds / 86_400),
    })
}

fn truncate(text: &str, width: u16) -> String {
    if width == 0 {
        return String::new();
    }
    if display_width(text) <= width {
        return text.to_owned();
    }
    let mut out = String::new();
    let mut used = 0_u16;
    for character in text.chars() {
        let size = display_width(&character.to_string());
        if used.saturating_add(size).saturating_add(1) > width {
            break;
        }
        out.push(character);
        used += size;
    }
    out.push('…');
    out
}

/// Select the panel views published for one pane. Items that name a pane and a
/// view are roots; their descendants are the rows.
fn panel_views<'a>(
    documents: &'a HostDocumentsProjectionV1,
    pane_id: &str,
) -> Vec<PanelView<'a>> {
    let mut views = Vec::new();
    let mut entries = documents.documents.iter().collect::<Vec<_>>();
    entries.sort_by(|left, right| {
        right
            .provider_priority
            .cmp(&left.provider_priority)
            .then_with(|| left.document.namespace.cmp(&right.document.namespace))
    });
    for entry in entries {
        let document = &entry.document;
        let items = &document.items;
        let children: HashMap<&str, Vec<&HostDocumentItem>> = {
            let mut children: HashMap<&str, Vec<&HostDocumentItem>> = HashMap::new();
            for item in items {
                if let Some(parent) = item.parent_id.as_deref() {
                    children.entry(parent).or_default().push(item);
                }
            }
            children
        };
        for item in items {
            let Some(view) = item.view.as_ref() else {
                continue;
            };
            let (Some(item_pane), Some(_view_id)) = (view.pane_id.as_deref(), view.view_id.as_deref())
            else {
                continue;
            };
            if item_pane != pane_id || item.flags.disabled || item.flags.selected == false && false {
                continue;
            }
            if item.flags.disabled {
                continue;
            }
            views.push(PanelView {
                title: item.label.as_str(),
                count: view.count,
                edge: view.edge.unwrap_or_default(),
                rows: children.get(item.id.as_str()).cloned().unwrap_or_default(),
            });
        }
    }
    views
}

#[allow(clippy::too_many_arguments)]
pub(super) fn render(
    buffer: &mut Buffer,
    documents: &HostDocumentsProjectionV1,
    snapshot: &ClientShellSnapshot,
    panes: &[PaneHit],
    focused_pane: Option<&str>,
    inspector_pane: Option<&str>,
    palette: &Palette,
    out: &mut PanePanelRender,
) {
    // Spike stand-in for the user's toggle: v1 opens views only from a user
    // action, and `vqro pane panel` plus per-client persistence land next. Until
    // then one real pane proves the pipeline: the focused pane.
    let Some(focused_pane) = focused_pane else {
        return;
    };
    let Some(pane) = panes.iter().find(|pane| pane.pane_id == focused_pane) else {
        return;
    };
    if inspector_pane == Some(pane.pane_id.as_str()) {
        return;
    }
    let views = panel_views(documents, &pane.pane_id);
    if views.is_empty() {
        return;
    }
    let area = pane.inner_rect;
    if area.is_empty() {
        return;
    }
    let Some(geometry) = geometry(area, views[0].edge) else {
        return;
    };
    let agents: HashMap<&str, &crate::protocol::ClientShellAgent> = snapshot
        .agents
        .iter()
        .map(|agent| (agent.pane_id.as_str(), agent))
        .collect();
    let panel_style = Style::default()
        .fg(palette.subtext0)
        .bg(palette.panel_bg)
        .remove_modifier(Modifier::DIM);
    for y in geometry.panel.y..geometry.panel.bottom() {
        for x in geometry.panel.x..geometry.panel.right() {
            put_text(buffer, x, y, 1, " ", panel_style);
        }
    }
    let mut y = geometry.panel.y;
    let width = geometry.panel.width;
    for view in &views {
        if y >= geometry.panel.bottom() {
            break;
        }
        let title = match view.count {
            Some(count) => format!(" {} {}", view.title, count),
            None => format!(" {}", view.title),
        };
        put_text(
            buffer,
            geometry.panel.x,
            y,
            width,
            &truncate(&title, width),
            Style::default()
                .fg(palette.text)
                .bg(palette.panel_bg)
                .add_modifier(Modifier::BOLD),
        );
        y = y.saturating_add(1);
        let mut hidden_rows = 0_u16;
        for row in &view.rows {
            if y + ROW_LINES > geometry.panel.bottom() {
                hidden_rows = hidden_rows.saturating_add(1);
                continue;
            }
            let row_rect = Rect::new(geometry.panel.x, y, width, ROW_LINES);
            let row_view = row.view.as_ref();
            let chip = row_view.and_then(|view| view.chip.as_ref());
            let headline = format!(" {}", row.label);
            put_text(
                buffer,
                row_rect.x,
                y,
                width,
                &truncate(&headline, width.saturating_sub(2)),
                Style::default().fg(palette.text).bg(palette.panel_bg),
            );
            let mut detail_x = 2_u16;
            if let Some(chip) = chip {
                let text = format!("{} ", chip.text);
                let chip_width = display_width(&text);
                put_text(
                    buffer,
                    row_rect.x,
                    y.saturating_add(1),
                    chip_width.min(width),
                    &text,
                    tone_style(chip.tone, palette).bg(palette.panel_bg),
                );
                detail_x = detail_x.saturating_add(chip_width);
            }
            let detail = row
                .detail
                .as_deref()
                .map(|detail| truncate(detail, width.saturating_sub(detail_x)))
                .unwrap_or_default();
            if !detail.is_empty() {
                put_text(
                    buffer,
                    row_rect.x.saturating_add(detail_x),
                    y.saturating_add(1),
                    width.saturating_sub(detail_x),
                    &detail,
                    Style::default().fg(palette.overlay0).bg(palette.panel_bg),
                );
            }
            let age = row_view.and_then(|view| view.since).and_then(age_label);
            let target_pane = row
                .terminal
                .as_ref()
                .and_then(|terminal| terminal.placement.as_ref())
                .map(|placement| placement.pane_id.clone());
            // The live end of the row: Vqro's own agent facts, joined here.
            let mut tail_x = width;
            if let Some(age) = age {
                let text = format!("{age} ");
                let text_width = display_width(&text);
                tail_x = tail_x.saturating_sub(text_width);
                put_text(
                    buffer,
                    row_rect.x.saturating_add(tail_x),
                    y.saturating_add(1),
                    text_width,
                    &text,
                    Style::default()
                        .fg(palette.overlay0)
                        .bg(palette.panel_bg)
                        .remove_modifier(Modifier::DIM),
                );
            }
            if let Some(target) = target_pane.as_deref() {
                if let Some(agent) = agents.get(target) {
                    let name = agent
                        .name
                        .as_deref()
                        .or(agent.terminal_title_stripped.as_deref())
                        .unwrap_or(target);
                    let (status_style, marker) = agent_status_style(agent.agent_status, palette);
                    let name_width = display_width(name);
                    let needed = name_width.saturating_add(3);
                    if tail_x > needed {
                        let x = row_rect.x.saturating_add(tail_x.saturating_sub(needed));
                        put_text(buffer, x, y, 2, marker, status_style.bg(palette.panel_bg));
                        put_text(
                            buffer,
                            x.saturating_add(2),
                            y,
                            name_width,
                            &truncate(name, name_width),
                            Style::default().fg(palette.subtext0).bg(palette.panel_bg),
                        );
                    }
                }
            }
            out.hits.push(PanePanelHit {
                rect: row_rect,
                target_pane,
            });
            y = y.saturating_add(ROW_LINES);
        }
        if hidden_rows > 0 && y < geometry.panel.bottom() {
            put_text(
                buffer,
                geometry.panel.x,
                y,
                width,
                &truncate(&format!(" +{hidden_rows} more"), width),
                Style::default().fg(palette.overlay0).bg(palette.panel_bg),
            );
            y = y.saturating_add(1);
        }
    }
    if let Some(claimed) = out.claimed.as_mut() {
        if claimed.0 == pane.pane_id {
            claimed.1 = geometry.terminal;
        }
    } else {
        out.claimed = Some((pane.pane_id.clone(), geometry.terminal));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extension_contract::{
        HostDocumentItemFlags, HostDocumentV1, HostItemRole, HostTarget, HostViewChip, HostViewItem,
        HOST_VIEW_CONTRACT_V1,
    };
    use crate::protocol::endpoint::HostDocumentProjectionEntryV1;

    fn row(id: &str, parent: Option<&str>, label: &str, pane: &str) -> HostDocumentItem {
        HostDocumentItem {
            id: id.into(),
            parent_id: parent.map(str::to_owned),
            role: HostItemRole::Section,
            label: label.into(),
            detail: Some("owes report".into()),
            terminal: None,
            action_ids: Vec::new(),
            flags: HostDocumentItemFlags::default(),
            view: Some(HostViewItem {
                pane_id: parent.is_none().then(|| pane.to_owned()),
                view_id: parent.is_none().then(|| "tasks".to_owned()),
                edge: parent.is_none().then_some(HostViewEdge::Right),
                chip: parent.is_some().then(|| HostViewChip {
                    text: "blocked".into(),
                    tone: HostViewTone::Danger,
                }),
                count: parent.is_none().then_some(1),
                since: parent.is_some().then_some(1_700_000_000),
            }),
        }
    }

    fn documents() -> HostDocumentsProjectionV1 {
        HostDocumentsProjectionV1 {
            boot_id: "boot".into(),
            revision: 1,
            documents: vec![HostDocumentProjectionEntryV1 {
                provider_priority: 1,
                document: HostDocumentV1 {
                    contract: HOST_VIEW_CONTRACT_V1.into(),
                    namespace: "example.view".into(),
                    generation: 1,
                    revision: 1,
                    surface: crate::extension_contract::HostSurface::Navigation,
                    target: HostTarget::Session,
                    items: vec![
                        row("tasks:w1:p1", None, "Tasks", "w1:p1"),
                        row("tasks:w1:p1:child", Some("tasks:w1:p1"), "Child", "w1:p1"),
                        row("tasks:w1:p2", None, "Other", "w1:p2"),
                    ],
                    actions: Vec::new(),
                    required_capabilities: Vec::new(),
                },
                confirmation_tokens: Default::default(),
            }],
        }
    }

    #[test]
    fn selects_only_the_views_published_for_one_pane() {
        let documents = documents();
        let views = panel_views(&documents, "w1:p1");
        assert_eq!(views.len(), 1);
        assert_eq!(views[0].title, "Tasks");
        assert_eq!(views[0].count, Some(1));
        assert_eq!(views[0].rows.len(), 1);
        assert_eq!(views[0].rows[0].label, "Child");
        assert!(panel_views(&documents, "w1:p9").is_empty());
    }

    #[test]
    fn panel_geometry_keeps_the_terminal_usable() {
        let wide = geometry(Rect::new(0, 0, 100, 30), HostViewEdge::Right).unwrap();
        assert_eq!(wide.panel.width, PANEL_DEFAULT_WIDTH);
        assert!(wide.terminal.width >= TERMINAL_MIN_WIDTH);
        assert_eq!(wide.divider.width, 1);
        // A narrow pane moves the panel to the bottom edge.
        let narrow = geometry(Rect::new(0, 0, 40, 30), HostViewEdge::Right).unwrap();
        assert_eq!(narrow.panel.height, PANEL_DEFAULT_HEIGHT);
        assert_eq!(narrow.panel.width, 40);
        // A pane too small for a panel stays closed.
        assert!(geometry(Rect::new(0, 0, 30, 12), HostViewEdge::Right).is_none());
    }

    #[test]
    fn ages_are_bounded_and_relative() {
        assert!(age_label(0).is_none());
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        assert_eq!(age_label(now - 12), Some("12s".into()));
        assert_eq!(age_label(now - 12 * 60), Some("12m".into()));
        assert_eq!(age_label(now - 3 * 3600), Some("3h".into()));
        assert_eq!(age_label(now - 2 * 86_400), Some("2d".into()));
        assert!(age_label(now + 600).is_none());
    }
}
