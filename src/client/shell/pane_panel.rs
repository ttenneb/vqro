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
/// A panel fits its content up to a share of the pane, then scrolls. Its size
/// follows the layout, the content at open time and the user, never the live
/// document.
pub(super) const PANEL_MIN_HEIGHT: u16 = 1;
const TERMINAL_MIN_HEIGHT: u16 = 4;
/// The most of a pane's height a panel takes before it scrolls.
pub(super) const PANEL_MAX_SHARE: u16 = 3;
pub(super) const WHEEL_ROWS: u16 = 3;
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
    /// Collapse or expand one section, by view key.
    ToggleView(String),
    /// Collapse or expand one row's children.
    ToggleRow(String),
    /// Focus the pane this row's terminal link names.
    FocusPane(String),
    /// The edge between the panel and the terminal: drag it to resize.
    ResizePanel,
    /// The pane's own badge: open every view this pane publishes.
    OpenViews,
}

/// A pane's own badge: it has views to open and none showing them.
pub(super) struct PaneBadge {
    pub(super) rect: Rect,
    pub(super) pane_id: String,
    pub(super) label: String,
}

/// Plan one badge per pane that has published views but nothing open. Pure:
/// the caller draws the labels and registers the hits.
pub(super) fn pane_badges(
    documents: &HostDocumentsProjectionV1,
    panes: &[PaneHit],
    open: &HashMap<String, BTreeSet<String>>,
) -> Vec<PaneBadge> {
    let mut badges = Vec::new();
    for pane in panes {
        if pane.rect.width < 12
            || pane.rect.height < 4
            || open
                .get(&pane.pane_id)
                .is_some_and(|views| !views.is_empty())
        {
            continue;
        }
        let available = published_views(documents, &pane.pane_id);
        if available.is_empty() {
            continue;
        }
        let label = format!(" ⚑ {} ", available.len());
        let width = display_width(&label);
        if width + 2 >= pane.rect.width {
            continue;
        }
        badges.push(PaneBadge {
            rect: Rect::new(
                pane.rect.right().saturating_sub(width + 1),
                pane.rect.y,
                width,
                1,
            ),
            pane_id: pane.pane_id.clone(),
            label,
        });
    }
    badges
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
    /// Sections collapsed to their header line, by view key.
    pub(super) collapsed_views: BTreeSet<String>,
    /// Rows whose children are hidden.
    pub(super) collapsed_rows: BTreeSet<String>,
    /// The height the user dragged. Their size wins over the content fit.
    pub(super) user_height: Option<u16>,
    /// The height the panel is drawn at. Recomputed when it opens or when the
    /// user expands or collapses something; live updates never change it.
    pub(super) height: Option<u16>,
    /// First visible content line. Live updates scroll rather than resize.
    pub(super) scroll: u16,
}

impl PanePanelDisplay {
    /// The moment the panel opens, or the user expands or collapses something:
    /// the height follows the content again.
    pub(super) fn refit(&mut self) {
        self.height = None;
        self.scroll = 0;
    }
}

/// What one pane's panel needs from the caller: the terminal rect to claim as
/// the pane's effective viewport, and the hits to register.
pub(super) struct PanePanelRender {
    pub(super) hits: Vec<PanePanelHit>,
    /// Every pane whose panel is open, with the terminal rect it claims.
    pub(super) claims: Vec<(String, Rect)>,
    /// The height the panel was drawn at: the content fit, capped by the pane.
    /// The caller freezes it at an open, expand or collapse, so a later live
    /// update scrolls instead of resizing the terminal.
    pub(super) drawn_height: Option<u16>,
    /// The pane whose height was drawn, for the caller to freeze.
    pub(super) drawn_pane: Option<String>,
}

/// A published view that fills one pane's panel slot.
struct PanelView<'a> {
    /// The extension's own id for this view, which the user names to
    /// `vqro pane panel`. It selects the view in `published_views` before the
    /// section is built, so the renderer itself does not read it again.
    #[allow(dead_code)]
    view_id: String,
    /// Stable identity of this section inside the pane's panel area.
    key: String,
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

/// The pane's top edge slot: a band above the terminal, full pane width, at the
/// caller's height. `None` means the pane is too small for a panel; the caller
/// keeps it closed (a border badge opens it as a popover in a later step).
pub(super) fn geometry(area: Rect, panel_height: u16) -> Option<PanePanelGeometry> {
    if area.width < 12 {
        return None;
    }
    let max_height = (area.height / PANEL_MAX_SHARE)
        .max(PANEL_MIN_HEIGHT)
        .min(area.height.saturating_sub(TERMINAL_MIN_HEIGHT));
    let height = panel_height.min(max_height);
    if height < PANEL_MIN_HEIGHT || area.height.saturating_sub(height) < TERMINAL_MIN_HEIGHT {
        return None;
    }
    let terminal_y = area.y.saturating_add(height).saturating_add(1);
    Some(PanePanelGeometry {
        terminal: Rect::new(
            area.x,
            terminal_y,
            area.width,
            area.bottom().saturating_sub(terminal_y),
        ),
        divider: Rect::new(area.x, area.y.saturating_add(height), area.width, 1),
        panel: Rect::new(area.x, area.y, area.width, height),
    })
}

/// The height at which the whole content fits, before the pane's cap. Sections
/// stack in one area, so their heights add.
fn fit_height(views: &[PanelView<'_>], collapsed_views: &BTreeSet<String>) -> u16 {
    let mut height = 0_u16;
    for view in views {
        height = height.saturating_add(1);
        if collapsed_views.contains(&view.key) {
            continue;
        }
        height = height.saturating_add(if view.rows.is_empty() {
            1
        } else {
            view.rows.len() as u16
        });
    }
    height.max(1)
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

/// Every view id a pane publishes, whatever the user has open.
pub(super) fn published_views(documents: &HostDocumentsProjectionV1, pane_id: &str) -> Vec<String> {
    let mut ids = Vec::new();
    for entry in &documents.documents {
        for item in &entry.document.items {
            let Some(view) = item.view.as_ref() else {
                continue;
            };
            let (Some(item_pane), Some(view_id)) =
                (view.pane_id.as_deref(), view.view_id.as_deref())
            else {
                continue;
            };
            if item_pane == pane_id && !item.flags.disabled && !ids.iter().any(|id| id == view_id) {
                ids.push(view_id.to_owned());
            }
        }
    }
    ids
}

/// Select the panel views published for one pane. Items that name a pane and a
/// view id are roots; their descendants are the rows.
fn panel_views<'a>(
    documents: &'a HostDocumentsProjectionV1,
    pane_id: &str,
    open_views: &BTreeSet<String>,
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
            if item_pane != pane_id || item.flags.disabled || !open_views.contains(_view_id) {
                continue;
            }
            views.push(PanelView {
                view_id: _view_id.to_owned(),
                key: format!("{}.{}", document.namespace, item.id),
                title: item.label.as_str(),
                summary: item.detail.as_deref().unwrap_or_default(),
                state: view.state,
                rows: flatten(&children, item.id.as_str(), collapsed),
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
    open: &HashMap<String, BTreeSet<String>>,
    inspector_pane: Option<&str>,
    displays: &HashMap<String, PanePanelDisplay>,
    palette: &Palette,
    out: &mut PanePanelRender,
) {
    for pane in panes {
        let Some(open_views) = open.get(&pane.pane_id) else {
            continue;
        };
        if open_views.is_empty() {
            continue;
        }
        let display = displays.get(&pane.pane_id).cloned().unwrap_or_default();
        render_pane(
            buffer,
            documents,
            snapshot,
            pane,
            open_views,
            inspector_pane,
            &display,
            palette,
            out,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn render_pane(
    buffer: &mut Buffer,
    documents: &HostDocumentsProjectionV1,
    snapshot: &ClientShellSnapshot,
    pane: &PaneHit,
    open_views: &BTreeSet<String>,
    inspector_pane: Option<&str>,
    display: &PanePanelDisplay,
    palette: &Palette,
    out: &mut PanePanelRender,
) {
    if inspector_pane == Some(pane.pane_id.as_str()) {
        return;
    }
    let views = panel_views(
        documents,
        &pane.pane_id,
        open_views,
        &display.collapsed_rows,
    );
    if views.is_empty() {
        return;
    }
    // The pane's full inner rect, including a scrollback lane: the panel owns
    // that edge, so the terminal's own lane must not paint inside the panel.
    let area = pane.projected_inner_rect.intersection(buffer.area);
    // The height is fixed once the panel is open: live updates scroll instead
    // of resizing the terminal, and the user's own size always wins.
    // One header line per section is the floor: a collapsed panel is its
    // headers, and the terminal gets every other row back.
    let headers = views.len() as u16;
    let all_collapsed = views
        .iter()
        .all(|view| display.collapsed_views.contains(&view.key));
    let fit = fit_height(&views, &display.collapsed_views);
    let panel_height = if all_collapsed {
        headers
    } else {
        display
            .user_height
            .or(display.height)
            .unwrap_or(fit)
            .max(headers)
    };
    let Some(geometry) = geometry(area, panel_height) else {
        return;
    };
    let panel_style = Style::default().bg(ratatui::style::Color::Reset);
    super::render::render_pane_contribution_divider(buffer, geometry.divider, palette);
    for y in geometry.panel.y..geometry.panel.bottom() {
        for x in geometry.panel.x..geometry.panel.right() {
            put_text(buffer, x, y, 1, " ", panel_style);
        }
    }
    // The panel takes the pane's edge, so the terminal's scrollback lane, which
    // the server still draws at the layout edge, is cleared with it.
    if let Some(lane) = pane.scrollbar_rect {
        let lane = lane.intersection(buffer.area);
        for y in lane.y..lane.bottom() {
            for x in lane.x..lane.right() {
                put_text(buffer, x, y, 1, " ", panel_style);
            }
        }
    }
    let agents: HashMap<&str, &crate::protocol::ClientShellAgent> = snapshot
        .agents
        .iter()
        .map(|agent| (agent.pane_id.as_str(), agent))
        .collect();
    let width = geometry.panel.width;
    let visible = geometry.panel.height;
    // Section headers stay pinned; only the rows scroll, so a section never
    // loses its name or its collapse marker.
    let row_space = visible.saturating_sub(headers);
    let rows = panel_rows(&views, &display.collapsed_views);
    let overflow = rows.len() as u16 > row_space;
    // The scroll indicator owns the last column, with one quiet column of
    // gutter between it and the content.
    let body_width = if overflow {
        width.saturating_sub(2)
    } else {
        width.saturating_sub(1)
    };
    let scroll = display
        .scroll
        .min((rows.len() as u16).saturating_sub(row_space));
    let mut y = geometry.panel.y;
    for view in &views {
        let collapsed = display.collapsed_views.contains(&view.key);
        let header_rect = Rect::new(geometry.panel.x, y, body_width, 1);
        draw_header(buffer, header_rect, view, collapsed, palette);
        out.hits.push(PanePanelHit {
            rect: header_rect,
            pane_id: pane.pane_id.clone(),
            action: PanePanelAction::ToggleView(view.key.clone()),
        });
        y = y.saturating_add(1);
    }
    for (drawn, line) in rows.iter().skip(usize::from(scroll)).enumerate() {
        if drawn as u16 >= row_space {
            break;
        }
        let row_rect = Rect::new(geometry.panel.x, y, body_width, 1);
        match line {
            PanelRow::Empty => {
                put_text(
                    buffer,
                    row_rect.x,
                    y,
                    body_width,
                    &truncate("└─ nothing to show", body_width),
                    Style::default().fg(palette.overlay0),
                );
            }
            PanelRow::Row(row) => {
                draw_row(
                    buffer,
                    row_rect,
                    row,
                    &display.collapsed_rows,
                    &agents,
                    body_width,
                    palette,
                );
                let prefix_width = display_width(&row_prefix(&row.lanes, row.is_last));
                let expandable = row.children > 0;
                if expandable {
                    out.hits.push(PanePanelHit {
                        rect: Rect::new(row_rect.x.saturating_add(prefix_width), y, 2, 1),
                        pane_id: pane.pane_id.clone(),
                        action: PanePanelAction::ToggleRow(row.item.id.clone()),
                    });
                }
                let target = row
                    .item
                    .terminal
                    .as_ref()
                    .and_then(|terminal| terminal.placement.as_ref())
                    .map(|placement| placement.pane_id.clone());
                let primary = match (&target, expandable) {
                    (Some(target), _) => Some(PanePanelAction::FocusPane(target.clone())),
                    (None, true) => Some(PanePanelAction::ToggleRow(row.item.id.clone())),
                    (None, false) => None,
                };
                if let Some(action) = primary {
                    out.hits.push(PanePanelHit {
                        rect: row_rect,
                        pane_id: pane.pane_id.clone(),
                        action,
                    });
                }
            }
        }
        y = y.saturating_add(1);
    }
    if overflow {
        let track = Rect::new(
            geometry.panel.x.saturating_add(width.saturating_sub(1)),
            geometry.panel.y.saturating_add(headers),
            1,
            row_space,
        );
        super::render::render_panel_scroll(
            buffer,
            track,
            rows.len(),
            usize::from(scroll),
            usize::from(row_space),
            palette,
        );
    }
    // The edge between the panel and the terminal is the drag handle, unless
    // every section is collapsed: there is nothing to resize then.
    if !all_collapsed {
        out.hits.push(PanePanelHit {
            rect: geometry.divider,
            pane_id: pane.pane_id.clone(),
            action: PanePanelAction::ResizePanel,
        });
    }
    if let Some(claimed) = out.claims.iter_mut().find(|claim| claim.0 == pane.pane_id) {
        claimed.1 = geometry.terminal;
    } else {
        out.claims.push((pane.pane_id.clone(), geometry.terminal));
    }
    out.drawn_height = Some(geometry.panel.height);
    out.drawn_pane = Some(pane.pane_id.clone());
}

/// One drawable line of the scrolling region.
enum PanelRow<'a> {
    Row(&'a FlatRow<'a>),
    Empty,
}

/// The rows of every expanded section, in draw order.
fn panel_rows<'a>(
    views: &'a [PanelView<'a>],
    collapsed_views: &BTreeSet<String>,
) -> Vec<PanelRow<'a>> {
    let mut rows = Vec::new();
    for view in views {
        if collapsed_views.contains(&view.key) {
            continue;
        }
        if view.rows.is_empty() {
            rows.push(PanelRow::Empty);
            continue;
        }
        for row in &view.rows {
            rows.push(PanelRow::Row(row));
        }
    }
    rows
}

fn draw_header(
    buffer: &mut Buffer,
    rect: Rect,
    view: &PanelView<'_>,
    collapsed: bool,
    palette: &Palette,
) {
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
    let right_fits = right_width.saturating_add(8) < rect.width;
    put_text(buffer, rect.x, rect.y, 1, marker, dim);
    put_text(
        buffer,
        rect.x.saturating_add(2),
        rect.y,
        1,
        aggregate,
        Style::default().fg(aggregate_color),
    );
    put_text(
        buffer,
        rect.x.saturating_add(4),
        rect.y,
        if right_fits {
            rect.width.saturating_sub(4).saturating_sub(right_width)
        } else {
            rect.width.saturating_sub(4)
        },
        view.title,
        Style::default()
            .fg(palette.text)
            .add_modifier(Modifier::BOLD),
    );
    if right_fits {
        put_text(
            buffer,
            rect.x
                .saturating_add(rect.width.saturating_sub(right_width)),
            rect.y,
            right_width,
            &right,
            dim,
        );
    }
}

/// The plain sentence for what a row waits for: Vqro says "you" or names the
/// terminal link, and the extension supplies only the action word.
fn waiting_text(row: &FlatRow<'_>, name: Option<&str>) -> String {
    let Some(view) = row.item.view.as_ref() else {
        return String::new();
    };
    let action = view.action.as_deref().unwrap_or_default();
    match view.waiting {
        Some(crate::extension_contract::HostViewWaiting::You) => {
            format!("waiting on you: {action}")
        }
        Some(crate::extension_contract::HostViewWaiting::Them) => match name {
            Some(name) => format!("waiting on {name}: {action}"),
            None => format!("waiting on them: {action}"),
        },
        None => row.item.detail.clone().unwrap_or_default(),
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_row(
    buffer: &mut Buffer,
    rect: Rect,
    row: &FlatRow<'_>,
    collapsed_rows: &BTreeSet<String>,
    agents: &HashMap<&str, &crate::protocol::ClientShellAgent>,
    width: u16,
    palette: &Palette,
) {
    let row_view = row.item.view.as_ref();
    let state = row_view.and_then(|view| view.state).unwrap_or_default();
    let tone = row_view
        .and_then(|view| view.tone)
        .unwrap_or_else(|| state_tone(state));
    let prefix = row_prefix(&row.lanes, row.is_last);
    let expandable = row.children > 0;
    let expanded = expandable && !collapsed_rows.contains(row.item.id.as_str());
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
    let name = live.map(|agent| {
        agent
            .name
            .as_deref()
            .or(agent.terminal_title_stripped.as_deref())
            .unwrap_or(agent.pane_id.as_str())
    });
    let dim = Style::default().fg(palette.overlay0);
    // The quiet end of the row: who is on the hook, what they are waiting for
    // and how long it has been that way. The name appears once, joined live.
    let sentence = waiting_text(row, name);
    let mut parts = Vec::new();
    if let Some(name) = name {
        if !sentence.contains(name) {
            parts.push(name.to_owned());
        }
    }
    if !sentence.is_empty() {
        parts.push(sentence);
    }
    if let Some(age) = row_view.and_then(|view| view.since).and_then(age_label) {
        parts.push(age);
    }
    let tail = parts.join(" · ");
    let tail_width = display_width(&tail);
    let dot_width = if live.is_some() && !tail.is_empty() {
        2
    } else {
        0
    };
    let tail_fits = !tail.is_empty() && tail_width.saturating_add(dot_width + 8) < width;
    let prefix_width = display_width(&prefix);
    let head_width = prefix_width.saturating_add(4);
    let title = truncate(
        &row.item.label,
        width
            .saturating_sub(head_width)
            .saturating_sub(if tail_fits {
                tail_width.saturating_add(dot_width).saturating_add(2)
            } else {
                0
            }),
    );
    let title_style = Style::default().fg(match state {
        HostViewState::InProgress => palette.accent,
        HostViewState::Done => palette.overlay0,
        _ => palette.text,
    });
    put_text(buffer, rect.x, rect.y, prefix_width, &prefix, dim);
    put_text(
        buffer,
        rect.x.saturating_add(prefix_width),
        rect.y,
        2,
        marker,
        dim,
    );
    put_text(
        buffer,
        rect.x.saturating_add(prefix_width).saturating_add(2),
        rect.y,
        1,
        state_glyph(state),
        tone_style(tone, palette),
    );
    put_text(
        buffer,
        rect.x.saturating_add(prefix_width).saturating_add(4),
        rect.y,
        width.saturating_sub(prefix_width).saturating_sub(4),
        &title,
        title_style,
    );
    if tail_fits {
        let total = tail_width.saturating_add(dot_width);
        let mut text_x = rect
            .x
            .saturating_add(width.saturating_sub(total))
            .saturating_sub(1);
        if let Some(agent) = live {
            let (glyph, color) = match agent.agent_status {
                crate::api::schema::AgentStatus::Working => ("●", palette.green),
                crate::api::schema::AgentStatus::Blocked => ("●", palette.red),
                crate::api::schema::AgentStatus::Idle => ("○", palette.yellow),
                crate::api::schema::AgentStatus::Done => ("○", palette.teal),
                crate::api::schema::AgentStatus::Unknown => ("·", palette.overlay0),
            };
            put_text(
                buffer,
                text_x,
                rect.y,
                2,
                &format!("{glyph} "),
                Style::default().fg(color),
            );
            text_x = text_x.saturating_add(2);
        }
        put_text(
            buffer,
            text_x,
            rect.y,
            width.saturating_sub(text_x.saturating_sub(rect.x)),
            &tail,
            dim,
        );
    }
}

impl ClientShellState {
    /// Apply one user request from the server: open, close or toggle a view on
    /// a pane. Only a user action ever opens a view.
    pub(crate) fn apply_pane_panel_request(
        &mut self,
        request: crate::protocol::endpoint::ServerPanePanelRequest,
    ) -> bool {
        let Some(documents) = self
            .endpoints
            .iter()
            .find(|endpoint| endpoint.endpoint_id == self.active_endpoint_id)
            .and_then(ClientShellEndpoint::current_host_documents)
        else {
            return false;
        };
        let available = published_views(documents, &request.pane_id);
        if available.is_empty() {
            return false;
        }
        let wanted = match request.view_id.as_deref() {
            Some(view_id) if available.iter().any(|id| id == view_id) => vec![view_id.to_owned()],
            Some(_) => return false,
            None => available,
        };
        let open = self
            .pane_panel_open
            .entry(request.pane_id.clone())
            .or_default();
        for view_id in wanted {
            match request.action {
                crate::protocol::endpoint::ServerPanePanelAction::Show => {
                    open.insert(view_id);
                }
                crate::protocol::endpoint::ServerPanePanelAction::Hide => {
                    open.remove(&view_id);
                }
                crate::protocol::endpoint::ServerPanePanelAction::Toggle => {
                    if !open.remove(&view_id) {
                        open.insert(view_id);
                    }
                }
            }
        }
        if open.is_empty() {
            self.pane_panel_open.remove(&request.pane_id);
        }
        // A user open, close or toggle re-fits the panel to its content.
        self.pane_panel_display
            .entry(request.pane_id)
            .or_default()
            .refit();
        self.persist_pane_panels();
        true
    }

    /// Show the pane's views when an agent starts there, if the client setting
    /// asks for it. A rule the user enabled, never the extension.
    pub(crate) fn apply_show_panels_on_agent_start(&mut self) -> bool {
        if !self.config.show_panels_on_agent_start {
            return false;
        }
        let Some(snapshot) = self.snapshot.as_deref() else {
            return false;
        };
        let agents = snapshot
            .agents
            .iter()
            .map(|agent| agent.pane_id.clone())
            .collect::<Vec<_>>();
        let Some(documents) = self
            .endpoints
            .iter()
            .find(|endpoint| endpoint.endpoint_id == self.active_endpoint_id)
            .and_then(ClientShellEndpoint::current_host_documents)
            .cloned()
        else {
            return false;
        };
        let mut changed = false;
        for pane_id in agents {
            if self
                .pane_panel_open
                .get(&pane_id)
                .is_some_and(|views| !views.is_empty())
            {
                continue;
            }
            let available = published_views(&documents, &pane_id);
            if available.is_empty() {
                continue;
            }
            // Only a pane that gained an agent since the last projection.
            if !self.panes_with_agents.insert(pane_id.clone()) {
                continue;
            }
            self.pane_panel_open
                .entry(pane_id)
                .or_default()
                .insert(available[0].clone());
            changed = true;
        }
        self.panes_with_agents.retain(|pane_id| {
            snapshot
                .agents
                .iter()
                .any(|agent| &agent.pane_id == pane_id)
        });
        if changed {
            self.persist_pane_panels();
        }
        changed
    }

    /// Persist which views are open and how their panels look, per pane.
    pub(super) fn persist_pane_panels(&mut self) {
        let Some(path) = self.config.preferences_path.clone() else {
            return;
        };
        self.sync_panel_identity();
        let mut preferences = self.config.preferences.clone();
        preferences.panels = self
            .panels_by_terminal
            .iter()
            .filter(|(_, views)| !views.is_empty())
            .map(|(terminal_id, views)| {
                let display = self
                    .display_by_terminal
                    .get(terminal_id)
                    .cloned()
                    .unwrap_or_default();
                preferences::ClientPanelPreference {
                    terminal_id: terminal_id.clone(),
                    views: views.iter().cloned().collect(),
                    height: display.user_height,
                    collapsed_views: display.collapsed_views.iter().cloned().collect(),
                    collapsed_rows: display.collapsed_rows.iter().cloned().collect(),
                }
            })
            .collect();
        preferences
            .panels
            .sort_by(|left, right| left.terminal_id.cmp(&right.terminal_id));
        self.config.preferences = preferences.clone();
        let mut outcome = ClientShellInput::default();
        if let Err(error) = preferences::store(&path, preferences) {
            self.set_endpoint_error(error);
            outcome.repaint = true;
        }
    }

    /// The pane-to-terminal join, from the published views: a pane has exactly
    /// one terminal, so a terminal placed in that pane is that pane's terminal.
    fn pane_terminal_join(&self) -> HashMap<String, String> {
        let Some(documents) = self
            .endpoints
            .iter()
            .find(|endpoint| endpoint.endpoint_id == self.active_endpoint_id)
            .and_then(ClientShellEndpoint::current_host_documents)
        else {
            return HashMap::new();
        };
        let mut join = HashMap::new();
        for entry in &documents.documents {
            for item in &entry.document.items {
                if let Some(terminal) = item.terminal.as_ref() {
                    if let Some(placement) = terminal.placement.as_ref() {
                        join.insert(placement.pane_id.clone(), terminal.terminal_id.clone());
                    }
                }
            }
        }
        join
    }

    /// Update the pane-to-terminal join and derive the pane-keyed state the
    /// renderer uses from the terminal-keyed state that is authoritative.
    fn rekey_panels(&mut self) {
        self.pane_terminal_ids = self.pane_terminal_join();
        let mut open = HashMap::new();
        let mut display = HashMap::new();
        for (pane_id, terminal_id) in &self.pane_terminal_ids {
            if let Some(views) = self.panels_by_terminal.get(terminal_id) {
                if !views.is_empty() {
                    open.insert(pane_id.clone(), views.clone());
                }
            }
            if let Some(stored) = self.display_by_terminal.get(terminal_id) {
                display.insert(pane_id.clone(), stored.clone());
            }
        }
        self.pane_panel_open = open;
        self.pane_panel_display = display;
    }

    /// Keep the durable, terminal-keyed panels and the pane-keyed ones the
    /// renderer uses in step. Called whenever the pane-keyed state changes and
    /// when the pane identities may have: the fold uses the join that was in
    /// force, so a pane that moved keeps its entry under its terminal.
    pub(crate) fn sync_panel_identity(&mut self) {
        let mut panels = HashMap::new();
        let mut displays = HashMap::new();
        for (pane_id, views) in std::mem::take(&mut self.pane_panel_open) {
            if let Some(terminal_id) = self.pane_terminal_ids.get(&pane_id) {
                panels.insert(terminal_id.clone(), views);
            }
        }
        for (pane_id, display) in std::mem::take(&mut self.pane_panel_display) {
            if let Some(terminal_id) = self.pane_terminal_ids.get(&pane_id) {
                displays.insert(terminal_id.clone(), display);
            }
        }
        // A terminal whose pane is live but has no panel is closed on purpose:
        // forgetting it is what makes closing stick. A terminal whose pane is
        // not live (a restore before the join is known) keeps its entry.
        let live = self
            .pane_terminal_ids
            .values()
            .cloned()
            .collect::<std::collections::HashSet<_>>();
        self.panels_by_terminal
            .retain(|terminal_id, _| !live.contains(terminal_id));
        self.display_by_terminal
            .retain(|terminal_id, _| !live.contains(terminal_id));
        self.panels_by_terminal.extend(panels);
        self.display_by_terminal.extend(displays);
        self.rekey_panels();
    }

    /// Restore the persisted panels. The stored key is the terminal id, so a
    /// pane that moved keeps its panels wherever it is now.
    pub(super) fn restore_pane_panels(&mut self) {
        if self.snapshot.is_none() {
            return;
        }
        for panel in self.config.preferences.panels.clone() {
            let terminal_id = panel.terminal_id.clone();
            self.panels_by_terminal
                .entry(terminal_id.clone())
                .or_default()
                .extend(panel.views);
            let display = PanePanelDisplay {
                user_height: panel.height,
                collapsed_views: panel.collapsed_views.into_iter().collect(),
                collapsed_rows: panel.collapsed_rows.into_iter().collect(),
                ..Default::default()
            };
            self.display_by_terminal
                .entry(terminal_id)
                .or_insert(display);
        }
        self.sync_panel_identity();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn geometry_puts_the_panel_above_a_usable_terminal() {
        let area = Rect::new(3, 4, 100, 30);
        let layout = geometry(area, 6).unwrap();
        assert_eq!(layout.panel.width, 100);
        assert_eq!(layout.panel.height, 6);
        assert_eq!(layout.panel.y, area.y);
        assert_eq!(layout.divider.y, area.y + 6);
        assert_eq!(layout.terminal.y, area.y + 7);
        assert!(layout.terminal.height >= TERMINAL_MIN_HEIGHT);
        // The cap is a share of the pane, and a tiny pane stays closed.
        let capped = geometry(area, 100).unwrap();
        assert_eq!(capped.panel.height, 30 / PANEL_MAX_SHARE);
        assert!(geometry(Rect::new(0, 0, 40, 4), 2).is_none());
        assert!(geometry(Rect::new(0, 0, 10, 30), 2).is_none());
    }

    #[test]
    fn fit_height_adds_stacked_sections_and_collapsed_ones_cost_one_line() {
        use crate::extension_contract::{HostDocumentItem, HostItemRole};
        let item = HostDocumentItem {
            id: "task:1".into(),
            parent_id: None,
            role: HostItemRole::Section,
            label: "row".into(),
            detail: None,
            terminal: None,
            action_ids: Vec::new(),
            flags: Default::default(),
            view: None,
        };
        let view = |key: &str, count: usize| PanelView {
            view_id: "tasks".into(),
            key: key.to_string(),
            title: "Tasks",
            summary: "",
            state: None,
            rows: (0..count)
                .map(|_| FlatRow {
                    item: &item,
                    lanes: Vec::new(),
                    is_last: true,
                    children: 1,
                })
                .collect(),
        };
        let views = vec![view("a", 2), view("b", 3)];
        assert_eq!(7, fit_height(&views, &BTreeSet::new()));
        assert_eq!(4, fit_height(&views, &BTreeSet::from(["b".to_string()])));
        assert_eq!(
            2,
            fit_height(&views, &BTreeSet::from(["a".to_string(), "b".to_string()]))
        );
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
}
