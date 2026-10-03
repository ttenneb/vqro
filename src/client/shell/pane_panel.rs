//! Pane split panels: Vqro lays out and draws views that extensions publish.
//!
//! Meaning, hierarchy and state text belong to the publishing extension. This
//! module only selects the views published for a pane, lays them out in the
//! pane's bottom edge slot, draws cached content in the same shape as the
//! surrounding UI (tree connectors, state glyphs, one line per row) and joins
//! the live terminal and agent facts Vqro owns. Nothing here reads a terminal,
//! the filesystem or a process tree: the work is bounded by the panes on screen
//! and the rows that fit.
use std::collections::{BTreeSet, HashMap};

use super::render::{display_width, put_text};
use super::*;
use crate::extension_contract::{HostDocumentItem, HostViewState, HostViewTone};
use crate::protocol::endpoint::HostDocumentsProjectionV1;

/// Panel size comes from the layout, never from content.
const PANEL_DEFAULT_HEIGHT: u16 = 10;
const PANEL_MIN_HEIGHT: u16 = 3;
const TERMINAL_MIN_HEIGHT: u16 = 4;
/// The one quiet glyph the header carries after its title.
const HEADER_MARK: &str = "⊟";

/// A clickable cell inside a panel.
#[derive(Clone)]
pub(super) struct PanePanelHit {
    pub(super) rect: Rect,
    pub(super) pane_id: String,
    pub(super) action: PanePanelAction,
}

#[derive(Clone, PartialEq, Eq)]
pub(super) enum PanePanelAction {
    /// Collapse or expand the whole panel.
    TogglePanel,
    /// Collapse or expand one row's children.
    ToggleRow(String),
    /// Focus the pane this row's terminal link names.
    FocusPane(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct PanePanelGeometry {
    pub(super) terminal: Rect,
    pub(super) divider: Rect,
    pub(super) panel: Rect,
}

/// The panel's own display state, per client. Persistence and the toggle
/// command arrive with the `vqro pane panel` step; this keeps the interactions
/// real until then.
#[derive(Clone, Default)]
pub(super) struct PanePanelDisplay {
    pub(super) collapsed: bool,
    pub(super) collapsed_rows: BTreeSet<String>,
}

/// What one pane's panel needs from the caller: the terminal rect to claim as
/// the pane's effective viewport, and the hits to register.
pub(super) struct PanePanelRender {
    pub(super) hits: Vec<PanePanelHit>,
    /// The pane whose viewport the panel claims, when a panel is open.
    pub(super) claimed: Option<(String, Rect)>,
}

/// A published view that fills one pane's panel slot.
struct PanelView<'a> {
    title: &'a str,
    /// The extension's own summary of the slice, drawn after the title.
    summary: &'a str,
    /// The panel's aggregate state, drawn as a glyph before the title.
    state: Option<HostViewState>,
    rows: Vec<FlatRow<'a>>,
}

/// One visible row with the tree lanes needed to draw its connector.
struct FlatRow<'a> {
    item: &'a HostDocumentItem,
    /// One flag per ancestor lane: true when that ancestor still has a sibling
    /// below, so the lane continues with `│`.
    lanes: Vec<bool>,
    is_last: bool,
    children: usize,
}

/// The pane's bottom edge slot: a fixed height band, full pane width, with the
/// terminal above it. `None` means the pane is too small for a panel; the
/// caller keeps it closed (a border badge opens it as a popover in a later
/// step).
pub(super) fn geometry(area: Rect) -> Option<PanePanelGeometry> {
    if area.width < 12 {
        return None;
    }
    let height = PANEL_DEFAULT_HEIGHT.min(area.height.saturating_sub(TERMINAL_MIN_HEIGHT + 1));
    if height < PANEL_MIN_HEIGHT {
        return None;
    }
    let terminal_height = area.height.saturating_sub(height + 1);
    Some(PanePanelGeometry {
        terminal: Rect::new(area.x, area.y, area.width, terminal_height),
        divider: Rect::new(
            area.x,
            area.y.saturating_add(terminal_height),
            area.width,
            1,
        ),
        panel: Rect::new(
            area.x,
            area.y.saturating_add(terminal_height + 1),
            area.width,
            height,
        ),
    })
}

/// The fixed Pi-style glyph for a row state.
fn state_glyph(state: HostViewState) -> &'static str {
    match state {
        HostViewState::Open => "○",
        HostViewState::InProgress => "◐",
        HostViewState::Blocked => "⊘",
        HostViewState::Reported => "◎",
        HostViewState::Done => "●",
    }
}

/// The tone a state uses unless the extension overrides it.
fn state_tone(state: HostViewState) -> HostViewTone {
    match state {
        HostViewState::Open => HostViewTone::Muted,
        HostViewState::InProgress => HostViewTone::Warning,
        HostViewState::Blocked => HostViewTone::Danger,
        HostViewState::Reported => HostViewTone::Accent,
        HostViewState::Done => HostViewTone::Success,
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

/// Truncate to `width` display columns with an ellipsis.
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
        used = used.saturating_add(size);
    }
    out.push('…');
    out
}

/// Pi-style tree prefix: the ancestor lanes that must continue, then this row's
/// own connector.
fn row_prefix(lanes: &[bool], is_last: bool) -> String {
    let mut out = String::new();
    for lane in lanes {
        out.push_str(if *lane { "│  " } else { "   " });
    }
    out.push_str(if is_last { "└─ " } else { "├─ " });
    out
}

/// Descend the root's children in document order, honouring collapsed rows.
fn flatten<'a>(
    children: &HashMap<&str, Vec<&'a HostDocumentItem>>,
    root: &str,
    collapsed: &BTreeSet<String>,
) -> Vec<FlatRow<'a>> {
    let mut rows = Vec::new();
    // (item, ancestor lanes, is last child), pushed in reverse so the next pop
    // is the first child in document order.
    let mut stack: Vec<(&HostDocumentItem, Vec<bool>, bool)> = Vec::new();
    for (index, item) in children
        .get(root)
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .enumerate()
        .rev()
    {
        let total = children.get(root).map(Vec::len).unwrap_or(0);
        stack.push((item, Vec::new(), index == total - 1));
    }
    while let Some((item, lanes, is_last)) = stack.pop() {
        let descendants = children.get(item.id.as_str()).cloned().unwrap_or_default();
        rows.push(FlatRow {
            item,
            lanes: lanes.clone(),
            is_last,
            children: descendants.len(),
        });
        if descendants.is_empty() || collapsed.contains(item.id.as_str()) {
            continue;
        }
        let mut child_lanes = lanes;
        child_lanes.push(!is_last);
        let total = descendants.len();
        for (index, child) in descendants.into_iter().enumerate().rev() {
            stack.push((child, child_lanes.clone(), index == total - 1));
        }
    }
    rows
}

/// Select the panel views published for one pane. Items that name a pane and a
/// view id are roots; their descendants are the rows.
fn panel_views<'a>(
    documents: &'a HostDocumentsProjectionV1,
    pane_id: &str,
    collapsed: &BTreeSet<String>,
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
        let mut children: HashMap<&str, Vec<&HostDocumentItem>> = HashMap::new();
        for item in items {
            if let Some(parent) = item.parent_id.as_deref() {
                children.entry(parent).or_default().push(item);
            }
        }
        for item in items {
            let Some(view) = item.view.as_ref() else {
                continue;
            };
            let (Some(item_pane), Some(_view_id)) =
                (view.pane_id.as_deref(), view.view_id.as_deref())
            else {
                continue;
            };
            if item_pane != pane_id || item.flags.disabled {
                continue;
            }
            views.push(PanelView {
                title: item.label.as_str(),
                summary: item.detail.as_deref().unwrap_or_default(),
                state: view.state,
                rows: flatten(&children, item.id.as_str(), collapsed),
            });
        }
    }
    views
}

/// The right-aligned, quiet end of a row: who is on the hook, what they owe and
/// how long it has been that way. The live name and state are Vqro's own facts.
struct RowTail {
    name: Option<String>,
    status: Option<crate::api::schema::AgentStatus>,
    detail: Option<String>,
    age: Option<String>,
}

impl RowTail {
    fn text(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if let Some(name) = self.name.as_deref() {
            parts.push(name.to_owned());
        }
        if let Some(detail) = self.detail.as_deref() {
            parts.push(detail.to_owned());
        }
        if let Some(age) = self.age.as_deref() {
            parts.push(age.to_owned());
        }
        parts.join(" · ")
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn render(
    buffer: &mut Buffer,
    documents: &HostDocumentsProjectionV1,
    snapshot: &ClientShellSnapshot,
    panes: &[PaneHit],
    focused_pane: Option<&str>,
    inspector_pane: Option<&str>,
    display: &PanePanelDisplay,
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
    let views = panel_views(documents, &pane.pane_id, &display.collapsed_rows);
    if views.is_empty() {
        return;
    }
    // The pane's full inner rect, including a scrollback lane: the panel owns
    // that edge, so the terminal's own lane must not paint inside the panel.
    let area = pane.projected_inner_rect.intersection(buffer.area);
    let collapsed = display.collapsed;
    let body = if collapsed { 1 } else { 0 };
    let Some(mut geometry) = geometry(area) else {
        return;
    };
    if collapsed {
        // Collapsed to the header line alone, so the terminal keeps its space.
        geometry.panel.height = body.max(1).min(geometry.panel.height);
        geometry.panel.y = geometry.panel.bottom().saturating_sub(1);
        geometry.divider.y = geometry.panel.y.saturating_sub(1);
        geometry.terminal.height = geometry.divider.y.saturating_sub(area.y);
        // A collapsed panel is a header line only: no divider band of its own.
        geometry.terminal.height = geometry.panel.y.saturating_sub(area.y);
    }
    if geometry.panel.height == 0 || geometry.terminal.height == 0 {
        return;
    }
    let base = Style::default()
        .fg(palette.subtext0)
        .bg(ratatui::style::Color::Reset);
    super::render::render_pane_contribution_divider(buffer, geometry.divider, palette);
    for y in geometry.panel.y..geometry.panel.bottom() {
        for x in geometry.panel.x..geometry.panel.right() {
            put_text(buffer, x, y, 1, " ", base);
        }
    }
    // The panel takes the pane's edge, so the terminal's scrollback lane, which
    // the server still draws at the layout edge, is cleared with it.
    if let Some(lane) = pane.scrollbar_rect {
        let lane = lane.intersection(buffer.area);
        for y in lane.y..lane.bottom() {
            for x in lane.x..lane.right() {
                put_text(buffer, x, y, 1, " ", base);
            }
        }
    }
    let agents: HashMap<&str, &crate::protocol::ClientShellAgent> = snapshot
        .agents
        .iter()
        .map(|agent| (agent.pane_id.as_str(), agent))
        .collect();
    let width = geometry.panel.width;
    let mut y = geometry.panel.y;
    for view in &views {
        if y >= geometry.panel.bottom() {
            break;
        }
        let header_rect = Rect::new(geometry.panel.x, y, width, 1);
        let marker = if collapsed { "▸" } else { "▾" };
        let state = view.state.unwrap_or_default();
        // The header's own glyph reads as live only while work is in progress.
        let (aggregate, aggregate_color) = if state == HostViewState::InProgress {
            (state_glyph(HostViewState::Done), palette.accent)
        } else {
            (state_glyph(HostViewState::Open), palette.overlay0)
        };
        let dim = Style::default().fg(palette.overlay0);
        let right = format!("{HEADER_MARK} · {}", view.summary);
        let right_width = display_width(&right);
        let right_fits = right_width.saturating_add(8) < width;
        put_text(buffer, header_rect.x, y, 1, marker, dim);
        put_text(
            buffer,
            header_rect.x.saturating_add(2),
            y,
            1,
            aggregate,
            Style::default().fg(aggregate_color),
        );
        put_text(
            buffer,
            header_rect.x.saturating_add(4),
            y,
            if right_fits {
                width.saturating_sub(4).saturating_sub(right_width)
            } else {
                width.saturating_sub(4)
            },
            view.title,
            Style::default()
                .fg(palette.text)
                .add_modifier(Modifier::BOLD),
        );
        if right_fits {
            put_text(
                buffer,
                header_rect
                    .x
                    .saturating_add(width.saturating_sub(right_width)),
                y,
                right_width,
                &right,
                dim,
            );
        }
        out.hits.push(PanePanelHit {
            rect: header_rect,
            pane_id: pane.pane_id.clone(),
            action: PanePanelAction::TogglePanel,
        });
        y = y.saturating_add(1);
        if collapsed {
            continue;
        }
        if view.rows.is_empty() {
            put_text(
                buffer,
                geometry.panel.x,
                y,
                width,
                &truncate("└─ nothing to show", width),
                dim,
            );
            y = y.saturating_add(1);
            continue;
        }
        let mut hidden = 0_u16;
        for row in &view.rows {
            if y >= geometry.panel.bottom() {
                hidden = hidden.saturating_add(1);
                continue;
            }
            let row_view = row.item.view.as_ref();
            let state = row_view.and_then(|view| view.state).unwrap_or_default();
            let tone = row_view
                .and_then(|view| view.tone)
                .unwrap_or_else(|| state_tone(state));
            let prefix = row_prefix(&row.lanes, row.is_last);
            let expandable = row.children > 0;
            let expanded = expandable && !display.collapsed_rows.contains(row.item.id.as_str());
            let marker = if !expandable {
                "  "
            } else if expanded {
                "▾ "
            } else {
                "▸ "
            };
            let target = row
                .item
                .terminal
                .as_ref()
                .and_then(|terminal| terminal.placement.as_ref())
                .map(|placement| placement.pane_id.clone());
            let live = target.as_deref().and_then(|pane_id| agents.get(pane_id));
            let tail = RowTail {
                name: live.map(|agent| {
                    agent
                        .name
                        .as_deref()
                        .or(agent.terminal_title_stripped.as_deref())
                        .unwrap_or(agent.pane_id.as_str())
                        .to_owned()
                }),
                status: live.map(|agent| agent.agent_status),
                detail: row.item.detail.clone(),
                age: row_view.and_then(|view| view.since).and_then(age_label),
            };
            let tail_text = tail.text();
            let tail_width = display_width(&tail_text);
            let tail_fits = !tail_text.is_empty() && tail_width.saturating_add(8) < width;
            let head_width = display_width(&prefix).saturating_add(4);
            let title = truncate(
                &row.item.label,
                width
                    .saturating_sub(head_width)
                    .saturating_sub(if tail_fits {
                        tail_width.saturating_add(2)
                    } else {
                        0
                    }),
            );
            let title_style = Style::default().fg(match state {
                HostViewState::InProgress => palette.accent,
                HostViewState::Done => palette.overlay0,
                _ => palette.text,
            });
            let row_rect = Rect::new(geometry.panel.x, y, width, 1);
            let prefix_width = display_width(&prefix);
            put_text(buffer, row_rect.x, y, prefix_width, &prefix, dim);
            put_text(
                buffer,
                row_rect.x.saturating_add(prefix_width),
                y,
                2,
                marker,
                dim,
            );
            put_text(
                buffer,
                row_rect.x.saturating_add(prefix_width).saturating_add(2),
                y,
                1,
                state_glyph(state),
                tone_style(tone, palette),
            );
            put_text(
                buffer,
                row_rect.x.saturating_add(prefix_width).saturating_add(4),
                y,
                width.saturating_sub(prefix_width).saturating_sub(4),
                &title,
                title_style,
            );
            if tail_fits {
                // One column of margin at the pane's edge, then the live dot
                // and its space before the text itself.
                let tail_x = row_rect
                    .x
                    .saturating_add(width.saturating_sub(tail_width).saturating_sub(3));
                let mut text_x = tail_x;
                if let Some(status) = tail.status {
                    let (glyph, color) = match status {
                        crate::api::schema::AgentStatus::Working => ("●", palette.green),
                        crate::api::schema::AgentStatus::Blocked => ("●", palette.red),
                        crate::api::schema::AgentStatus::Idle => ("○", palette.yellow),
                        crate::api::schema::AgentStatus::Done => ("○", palette.teal),
                        crate::api::schema::AgentStatus::Unknown => ("·", palette.overlay0),
                    };
                    put_text(
                        buffer,
                        text_x,
                        y,
                        2,
                        &format!("{glyph} "),
                        Style::default().fg(color),
                    );
                    text_x = text_x.saturating_add(2);
                }
                put_text(
                    buffer,
                    text_x,
                    y,
                    width.saturating_sub(text_x.saturating_sub(row_rect.x)),
                    &tail_text,
                    dim,
                );
            }
            // Row clicks: the marker toggles, the rest of the row jumps to the
            // linked pane when the extension named one.
            let primary = match (&target, expandable) {
                (Some(target), _) => Some(PanePanelAction::FocusPane(target.clone())),
                (None, true) => Some(PanePanelAction::ToggleRow(row.item.id.clone())),
                (None, false) => None,
            };
            // The expand marker is pushed first so a click on it toggles the
            // row even though the whole row is also a target.
            if expandable {
                out.hits.push(PanePanelHit {
                    rect: Rect::new(row_rect.x.saturating_add(prefix_width), y, 2, 1),
                    pane_id: pane.pane_id.clone(),
                    action: PanePanelAction::ToggleRow(row.item.id.clone()),
                });
            }
            if let Some(action) = primary {
                out.hits.push(PanePanelHit {
                    rect: row_rect,
                    pane_id: pane.pane_id.clone(),
                    action,
                });
            }
            y = y.saturating_add(1);
        }
        if hidden > 0 && y < geometry.panel.bottom() {
            put_text(
                buffer,
                geometry.panel.x,
                y,
                width,
                &truncate(&format!("└─ +{hidden} more"), width),
                dim,
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

    #[test]
    fn geometry_keeps_the_terminal_and_a_fixed_bottom_band() {
        let area = Rect::new(3, 4, 100, 30);
        let layout = geometry(area).unwrap();
        assert_eq!(layout.panel.width, 100);
        assert_eq!(layout.panel.height, PANEL_DEFAULT_HEIGHT);
        assert_eq!(layout.panel.bottom(), area.bottom());
        assert_eq!(layout.divider.width, 100);
        assert!(layout.terminal.height >= TERMINAL_MIN_HEIGHT);
        // A pane too small for a panel stays closed.
        assert!(geometry(Rect::new(0, 0, 40, 6)).is_none());
        assert!(geometry(Rect::new(0, 0, 10, 30)).is_none());
    }

    #[test]
    fn prefixes_draw_continuation_lanes_like_the_todo_widget() {
        assert_eq!("├─ ", row_prefix(&[], false));
        assert_eq!("└─ ", row_prefix(&[], true));
        assert_eq!("│  └─ ", row_prefix(&[true], true));
        assert_eq!("   ├─ ", row_prefix(&[false], false));
    }

    #[test]
    fn states_map_to_the_pi_glyphs_and_tones() {
        assert_eq!("○", state_glyph(HostViewState::Open));
        assert_eq!("◐", state_glyph(HostViewState::InProgress));
        assert_eq!("⊘", state_glyph(HostViewState::Blocked));
        assert_eq!("◎", state_glyph(HostViewState::Reported));
        assert_eq!("●", state_glyph(HostViewState::Done));
        assert_eq!(HostViewTone::Muted, state_tone(HostViewState::Open));
        assert_eq!(HostViewTone::Warning, state_tone(HostViewState::InProgress));
        assert_eq!(HostViewTone::Danger, state_tone(HostViewState::Blocked));
        assert_eq!(HostViewTone::Accent, state_tone(HostViewState::Reported));
        assert_eq!(HostViewTone::Success, state_tone(HostViewState::Done));
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

    #[test]
    fn rows_truncate_with_an_ellipsis() {
        assert_eq!("short", truncate("short", 10));
        assert_eq!("shor…", truncate("shortening", 5));
        assert_eq!("", truncate("short", 0));
    }

    #[test]
    fn tails_join_only_the_parts_they_have() {
        assert_eq!(
            "panel-b · owes unblock · 12m",
            RowTail {
                name: Some("panel-b".into()),
                status: None,
                detail: Some("owes unblock".into()),
                age: Some("12m".into()),
            }
            .text()
        );
        assert_eq!(
            "",
            RowTail {
                name: None,
                status: None,
                detail: None,
                age: None,
            }
            .text()
        );
    }
}
