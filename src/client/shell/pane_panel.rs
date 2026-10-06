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
    documents: &PanelIndex<'_>,
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
        let available = documents.published_views(&pane.pane_id);
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
    /// For each claimed pane: the band the host painted the grid in (the top of
    /// the pane's inner rect) and the rect below the panel the grid belongs in.
    /// The caller moves every grid row across, so the agent's newest rows and its
    /// input box end at the pane's bottom edge and nothing is hidden.
    pub(super) projected: Vec<(String, Rect, Rect)>,
    /// For each claimed pane: where the server painted its terminal (the top of
    /// the pane's inner rect) and where it belongs (the rect below the panel).
    /// The panel band each pane reserved, clickable or not: a retained surface
    /// patch must never paint over it.
    pub(super) reserved: Vec<Rect>,
    /// Every pane whose panel was drawn, with the height it was drawn at: the
    /// content fit, capped by the pane. The caller freezes each at an open,
    /// expand or collapse, so a later live update scrolls instead of resizing
    /// the terminal - for every open pane, not just the last one rendered.
    pub(super) drawn: Vec<(String, u16)>,
}

/// A published view that fills one pane's panel slot.
struct PanelView<'a> {
    /// The extension's own id for this view. It is the section's identity: the
    /// user names it to `vqro pane panel`, and collapse state is kept by it.
    view_id: String,
    /// The root block's kind, when the extension named one.
    kind: Option<String>,
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
        if collapsed_views.contains(&view.view_id) {
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

/// What Vqro can draw. Anything else is drawn as "unsupported" in place, which
/// is the compatibility guarantee: a new block never breaks an older surface.
fn block_kind_supported(kind: Option<&str>) -> bool {
    matches!(kind, None | Some("row") | Some("section"))
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
    index: &PanelIndex<'a>,
    document: usize,
    root: &str,
    collapsed: &BTreeSet<String>,
) -> Vec<FlatRow<'a>> {
    let children = &index.children;
    let mut rows = Vec::new();
    // A bound on the work, not just on the output: documents are independent, but
    // one document's ids could still loop, and this walk must always terminate.
    let bound = index.items.len().max(1);
    // (item, ancestor lanes, is last child), pushed in reverse so the next pop
    // is the first child in document order.
    let mut stack: Vec<(&HostDocumentItem, Vec<bool>, bool)> = Vec::new();
    for (position, item) in children
        .get(&(document, root))
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .enumerate()
        .rev()
    {
        let total = children.get(&(document, root)).map(Vec::len).unwrap_or(0);
        stack.push((item, Vec::new(), position == total - 1));
    }
    while let Some((item, lanes, is_last)) = stack.pop() {
        if rows.len() >= bound {
            break;
        }
        let descendants = children
            .get(&(document, item.id.as_str()))
            .cloned()
            .unwrap_or_default();
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

/// One pass over the published documents, reused by every pane in a frame.
/// Building this per pane made the panel path scale with panes times items.
pub(super) struct PanelIndex<'a> {
    /// `(priority, document index, item)` in render order. The document index is
    /// this frame's identity for the document, so two documents that share a
    /// namespace never share rows.
    items: Vec<(i32, usize, &'a HostDocumentItem)>,
    /// Per document, its items by parent id. Scoped by document index: a chain of
    /// ids across two documents can never be walked as one tree.
    children: HashMap<(usize, &'a str), Vec<&'a HostDocumentItem>>,
}

impl<'a> PanelIndex<'a> {
    /// `with_rows` builds the per-document children index only when a caller will
    /// read rows: a frame where nothing is open needs the item list for badges and
    /// nothing more.
    pub(super) fn with_rows(documents: &'a HostDocumentsProjectionV1, rows: bool) -> Self {
        let mut entries = documents
            .documents
            .iter()
            .enumerate()
            .collect::<Vec<(usize, _)>>();
        entries.sort_by(|left, right| {
            right
                .1
                .provider_priority
                .cmp(&left.1.provider_priority)
                .then_with(|| left.1.document.namespace.cmp(&right.1.document.namespace))
                .then_with(|| left.0.cmp(&right.0))
        });
        let mut items = Vec::new();
        let mut children: HashMap<(usize, &str), Vec<&HostDocumentItem>> = HashMap::new();
        for (document_index, entry) in entries {
            for item in &entry.document.items {
                if rows {
                    if let Some(parent) = item.parent_id.as_deref() {
                        children
                            .entry((document_index, parent))
                            .or_default()
                            .push(item);
                    }
                }
                items.push((entry.provider_priority, document_index, item));
            }
        }
        Self { items, children }
    }

    /// Every view id a pane publishes, whatever the user has open.
    pub(super) fn published_views(&self, pane_id: &str) -> Vec<String> {
        let mut ids = Vec::new();
        for (_, _, item) in &self.items {
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
        ids
    }
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
    index: &'a PanelIndex<'a>,
    pane_id: &str,
    open_views: &BTreeSet<String>,
    collapsed: &BTreeSet<String>,
) -> Vec<PanelView<'a>> {
    let mut views = Vec::new();
    for (_, document, item) in &index.items {
        let Some(view) = item.view.as_ref() else {
            continue;
        };
        let (Some(item_pane), Some(view_id)) = (view.pane_id.as_deref(), view.view_id.as_deref())
        else {
            continue;
        };
        if item_pane != pane_id || item.flags.disabled || !open_views.contains(view_id) {
            continue;
        }
        views.push(PanelView {
            view_id: view_id.to_owned(),
            kind: view.kind.clone(),
            title: item.label.as_str(),
            summary: item.detail.as_deref().unwrap_or_default(),
            state: view.state,
            rows: flatten(index, *document, item.id.as_str(), collapsed),
        });
    }
    views
}

#[allow(clippy::too_many_arguments)]
pub(super) fn render(
    buffer: &mut Buffer,
    index: &PanelIndex<'_>,
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
            index,
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
    index: &PanelIndex<'_>,
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
    let views = panel_views(index, &pane.pane_id, open_views, &display.collapsed_rows);
    // The user has views open here and none of them is published any more: the
    // extension failed or was disabled. Vqro owns that state and says so in the
    // same place, instead of silently dropping a panel the user opened.
    if views.is_empty() {
        let missing = open_views.iter().cloned().collect::<Vec<_>>();
        render_placeholder(
            buffer,
            pane,
            display,
            &missing,
            "unavailable",
            "└─ extension unavailable",
            palette,
            out,
        );
        return;
    }
    // The pane's full inner rect, including a scrollback lane: the panel owns
    // that edge, so the terminal's own lane must not paint inside the panel.
    let area = pane.projected_inner_rect.intersection(buffer.area);
    // The height is fixed once the panel is open: live updates scroll instead
    // of resizing the terminal, and the user's own size always wins.
    // One header line per section is the floor: a collapsed panel is its
    // headers, and the terminal gets every other row back.
    // A view the user opened that is no longer published keeps its own row, so it
    // counts as reserved space too: otherwise a collapsed panel is nothing but
    // headers and the missing view is never drawn at all.
    let missing = open_views
        .iter()
        .filter(|view_id| !views.iter().any(|view| &view.view_id == *view_id))
        .count() as u16;
    let pinned = views.len() as u16;
    let headers = pinned + missing;
    let all_collapsed = views
        .iter()
        .all(|view| display.collapsed_views.contains(&view.view_id));
    let fit = fit_height(&views, &display.collapsed_views)
        + views
            .iter()
            .filter(|view| !block_kind_supported(view.kind.as_deref()))
            .count() as u16
        + missing;
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
    // The band is Vqro chrome. Filling it with the panel background is what stops
    // the terminal's own text showing through the panel's empty cells; a Reset
    // background leaves whatever the terminal painted underneath.
    let panel_style = Style::default().bg(palette.panel_bg);
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
    // Headers are pinned in the sense that the panel's height reserves one line per
    // section; the rows below them are what scrolls. Every section keeps one line of
    // its own, in order: its header, or for a view the user opened that is no longer
    // published, its one row. Those lines are reserved, so a long section can never
    // push a later section's line out of the band.
    let row_space = visible.saturating_sub(pinned);
    let sections = panel_sections(&views, open_views, &display.collapsed_views);
    // The rows below the headers share one budget: one row for every section that has
    // a row, while the budget lasts, and then the rest of the budget to the sections
    // in order. Reserving that first row is what keeps a long earlier section from
    // starving a later section's rows out of the band entirely - its header was never
    // in doubt, its rows are what would go missing - and spending the budget on what
    // is drawn is what keeps the panel's bottom line from being left blank.
    // A section with no header (a view the user opened that is no longer published)
    // already draws its one row as its own reserved line, so it asks the pool for
    // nothing: budgeting it a row too would leave a band line unused under a section
    // whose rows are still waiting.
    let pooled_rows = sections
        .iter()
        .map(|section| {
            section
                .rows
                .len()
                .saturating_sub(usize::from(section.header.is_none()))
        })
        .collect::<Vec<_>>();
    let pool = usize::from(visible).saturating_sub(sections.len());
    let headers_only = pool == 0;
    let mut allocation = vec![0_usize; sections.len()];
    let mut spare = pool;
    for (index, wanted) in pooled_rows.iter().enumerate() {
        if *wanted == 0 || spare == 0 {
            continue;
        }
        allocation[index] = 1;
        spare -= 1;
    }
    for (index, wanted) in pooled_rows.iter().enumerate() {
        if spare == 0 {
            break;
        }
        let give = wanted.saturating_sub(allocation[index]).min(spare);
        allocation[index] += give;
        spare -= give;
    }
    // One scroll offset moves every section that has more rows than its share, so a
    // later section's later rows are reachable while every header stays where it is.
    let most_scrollable = pooled_rows
        .iter()
        .zip(&allocation)
        .max_by_key(|(wanted, allocated)| wanted.saturating_sub(**allocated))
        .filter(|(wanted, allocated)| **wanted > **allocated);
    let scroll_total = most_scrollable.map_or(0, |(wanted, _)| *wanted);
    let scroll_page = most_scrollable.map_or(0, |(_, allocated)| *allocated);
    let overflow = most_scrollable.is_some();
    // The scroll indicator owns the last column, with one quiet column of
    // gutter between it and the content.
    let body_width = if overflow {
        width.saturating_sub(2)
    } else {
        width.saturating_sub(1)
    };
    let scroll = display
        .scroll
        .min(scroll_total.saturating_sub(scroll_page) as u16);
    let mut y = geometry.panel.y;
    for (index, section) in sections.iter().enumerate() {
        let mut rest = section.rows.as_slice();
        if let Some(view) = section.header {
            let collapsed = display.collapsed_views.contains(&view.view_id);
            let header_rect = Rect::new(geometry.panel.x, y, body_width, 1);
            if let Some(kind) = view
                .kind
                .as_deref()
                .filter(|kind| !block_kind_supported(Some(kind)))
            {
                draw_unsupported_header(buffer, header_rect, view, kind, palette);
            } else {
                draw_header(buffer, header_rect, view, collapsed, palette);
            }
            out.hits.push(PanePanelHit {
                rect: header_rect,
                pane_id: pane.pane_id.clone(),
                action: PanePanelAction::ToggleView(view.view_id.clone()),
            });
            y = y.saturating_add(1);
        } else if let Some((first, remaining)) = rest.split_first() {
            // No header to draw: the section's own row is the line that keeps it in
            // the band, so it is drawn here and not counted against the pool.
            let row_rect = Rect::new(geometry.panel.x, y, body_width, 1);
            draw_plain_row(buffer, row_rect, first, palette);
            y = y.saturating_add(1);
            rest = remaining;
        }
        if headers_only || rest.is_empty() {
            continue;
        }
        // The section's share of the row budget, with the shared scroll offset
        // applied to every section that has more rows than its share.
        let space = allocation[index].min(rest.len());
        let offset = usize::from(scroll).min(rest.len().saturating_sub(space));
        let remaining = &rest[offset..offset.saturating_add(space)];
        for line in remaining {
            if y >= geometry.panel.bottom() {
                break;
            }
            let row_rect = Rect::new(geometry.panel.x, y, body_width, 1);
            match line {
                PanelRow::Unsupported { title, kind } => {
                    put_text(
                        buffer,
                        row_rect.x,
                        y,
                        body_width,
                        &truncate(&format!("└─ {title}: unsupported {kind}"), body_width),
                        Style::default().fg(palette.overlay0).bg(palette.panel_bg),
                    );
                }
                PanelRow::Unavailable { view_id } => {
                    put_text(
                        buffer,
                        row_rect.x,
                        y,
                        body_width,
                        &truncate(&format!("└─ {view_id}: unavailable"), body_width),
                        Style::default().fg(palette.overlay0).bg(palette.panel_bg),
                    );
                }
                PanelRow::Empty => {
                    put_text(
                        buffer,
                        row_rect.x,
                        y,
                        body_width,
                        &truncate("└─ nothing to show", body_width),
                        Style::default().fg(palette.overlay0).bg(palette.panel_bg),
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
    }
    if overflow {
        let track = Rect::new(
            geometry.panel.x.saturating_add(width.saturating_sub(1)),
            geometry.panel.y.saturating_add(pinned),
            1,
            row_space,
        );
        super::render::render_panel_scroll(
            buffer,
            track,
            scroll_total,
            usize::from(scroll),
            scroll_page,
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
    // The panel is a split: the host resized this pane's PTY to the rows below
    // the panel, so the grid is exactly as tall as that rect and every one of its
    // rows belongs there. The band they were painted in is the panel's.
    out.projected.push((
        pane.pane_id.clone(),
        Rect::new(
            area.x,
            area.y,
            geometry.terminal.width,
            geometry
                .panel
                .height
                .saturating_add(geometry.divider.height),
        ),
        geometry.terminal,
    ));
    out.reserved.push(Rect::new(
        geometry.panel.x,
        geometry.panel.y,
        geometry.panel.width,
        geometry
            .panel
            .height
            .saturating_add(geometry.divider.height),
    ));
    out.drawn
        .push((pane.pane_id.clone(), geometry.panel.height));
}

/// The panel a user opened whose view is no longer published: the extension
/// failed or was disabled. Vqro draws the header and one dead row, and keeps
/// the pane's edge reserved so the space stays stable when it recovers.
fn render_placeholder(
    buffer: &mut Buffer,
    pane: &PaneHit,
    display: &PanePanelDisplay,
    missing: &[String],
    right: &str,
    row: &str,
    palette: &Palette,
    out: &mut PanePanelRender,
) {
    let area = pane.projected_inner_rect.intersection(buffer.area);
    let collapsed = missing
        .iter()
        .any(|view_id| display.collapsed_views.contains(view_id));
    let fit = if collapsed { 1 } else { 2 };
    let panel_height = if collapsed {
        1
    } else {
        display.user_height.or(display.height).unwrap_or(fit).max(1)
    };
    let Some(geometry) = geometry(area, panel_height) else {
        return;
    };
    let dim = Style::default().fg(palette.overlay0).bg(palette.panel_bg);
    let style = Style::default().bg(palette.panel_bg);
    super::render::render_pane_contribution_divider(buffer, geometry.divider, palette);
    for y in geometry.panel.y..geometry.panel.bottom() {
        for x in geometry.panel.x..geometry.panel.right() {
            put_text(buffer, x, y, 1, " ", style);
        }
    }
    let width = geometry.panel.width.saturating_sub(1);
    let title = missing.first().map(String::as_str).unwrap_or("view");
    let header = Rect::new(geometry.panel.x, geometry.panel.y, width, 1);
    let marker = if collapsed { "▸" } else { "▾" };
    put_text(buffer, header.x, header.y, 1, marker, dim);
    put_text(
        buffer,
        header.x.saturating_add(2),
        header.y,
        1,
        state_glyph(HostViewState::Blocked),
        Style::default().fg(palette.red),
    );
    put_text(
        buffer,
        header.x.saturating_add(4),
        header.y,
        width.saturating_sub(4),
        title,
        Style::default()
            .fg(palette.text)
            .add_modifier(Modifier::BOLD),
    );
    let right = format!("{HEADER_MARK} · {right}");
    let right_width = display_width(&right);
    if right_width.saturating_add(8) < width {
        put_text(
            buffer,
            header.x.saturating_add(width.saturating_sub(right_width)),
            header.y,
            right_width,
            &right,
            dim,
        );
    }
    out.hits.push(PanePanelHit {
        rect: header,
        pane_id: pane.pane_id.clone(),
        action: PanePanelAction::ToggleView(title.to_owned()),
    });
    if !collapsed && geometry.panel.height > 1 {
        put_text(
            buffer,
            geometry.panel.x,
            geometry.panel.y.saturating_add(1),
            width,
            &truncate(row, width),
            dim,
        );
    }
    if let Some(claimed) = out.claims.iter_mut().find(|claim| claim.0 == pane.pane_id) {
        claimed.1 = geometry.terminal;
    } else {
        out.claims.push((pane.pane_id.clone(), geometry.terminal));
    }
    // The panel is a split: the host resized this pane's PTY to the rows below
    // the panel, so the grid is exactly as tall as that rect and every one of its
    // rows belongs there. The band they were painted in is the panel's.
    out.projected.push((
        pane.pane_id.clone(),
        Rect::new(
            area.x,
            area.y,
            geometry.terminal.width,
            geometry
                .panel
                .height
                .saturating_add(geometry.divider.height),
        ),
        geometry.terminal,
    ));
    out.reserved.push(Rect::new(
        geometry.panel.x,
        geometry.panel.y,
        geometry.panel.width,
        geometry
            .panel
            .height
            .saturating_add(geometry.divider.height),
    ));
    out.drawn
        .push((pane.pane_id.clone(), geometry.panel.height));
}

/// One drawable line of the scrolling region.
enum PanelRow<'a> {
    Row(&'a FlatRow<'a>),
    Empty,
    /// A section whose block kind Vqro does not know, in place.
    Unsupported {
        title: &'a str,
        kind: &'a str,
    },
    /// A view the user opened whose provider stopped publishing it.
    Unavailable {
        view_id: &'a str,
    },
}

/// The rows of every expanded section, in draw order.
/// One drawable section: a published view and the rows that belong to it, in the
/// order the panel stacks them. A view the user opened that is no longer published
/// is a section of its own with no header, so its row stays where its section was.
struct PanelSection<'a> {
    header: Option<&'a PanelView<'a>>,
    rows: Vec<PanelRow<'a>>,
}

fn panel_sections<'a>(
    views: &'a [PanelView<'a>],
    open_views: &'a BTreeSet<String>,
    collapsed_views: &BTreeSet<String>,
) -> Vec<PanelSection<'a>> {
    let mut sections = Vec::new();
    for view in views {
        let mut rows = Vec::new();
        if !collapsed_views.contains(&view.view_id) {
            if let Some(kind) = view
                .kind
                .as_deref()
                .filter(|kind| !block_kind_supported(Some(kind)))
            {
                rows.push(PanelRow::Unsupported {
                    title: view.title,
                    kind,
                });
            } else if view.rows.is_empty() {
                rows.push(PanelRow::Empty);
            } else {
                rows.extend(view.rows.iter().map(PanelRow::Row));
            }
        }
        sections.push(PanelSection {
            header: Some(view),
            rows,
        });
    }
    for view_id in open_views {
        if !views.iter().any(|view| &view.view_id == view_id) {
            sections.push(PanelSection {
                header: None,
                rows: vec![PanelRow::Unavailable { view_id }],
            });
        }
    }
    sections
}

/// A section line that has no header of its own: an unsupported, unavailable or empty
/// row, drawn in the same dim style the rows pool uses.
fn draw_plain_row(buffer: &mut Buffer, rect: Rect, line: &PanelRow<'_>, palette: &Palette) {
    let text = match line {
        PanelRow::Unsupported { title, kind } => format!("└─ {title}: unsupported {kind}"),
        PanelRow::Unavailable { view_id } => format!("└─ {view_id}: unavailable"),
        PanelRow::Empty => "└─ nothing to show".to_string(),
        PanelRow::Row(_) => return,
    };
    put_text(
        buffer,
        rect.x,
        rect.y,
        rect.width,
        &truncate(&text, rect.width),
        Style::default().fg(palette.overlay0).bg(palette.panel_bg),
    );
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

/// A section Vqro cannot draw: its own header, marked unsupported, so it keeps
/// its place in the stack and says why instead of replacing its siblings.
fn draw_unsupported_header(
    buffer: &mut Buffer,
    rect: Rect,
    view: &PanelView<'_>,
    kind: &str,
    palette: &Palette,
) {
    let dim = Style::default().fg(palette.overlay0);
    put_text(buffer, rect.x, rect.y, 1, "▾", dim);
    put_text(
        buffer,
        rect.x.saturating_add(2),
        rect.y,
        1,
        "?",
        Style::default().fg(palette.overlay0),
    );
    put_text(
        buffer,
        rect.x.saturating_add(4),
        rect.y,
        rect.width.saturating_sub(4),
        view.title,
        Style::default()
            .fg(palette.text)
            .add_modifier(Modifier::BOLD),
    );
    let right = format!("{HEADER_MARK} · unsupported {kind}");
    let right_width = display_width(&right);
    if right_width.saturating_add(8) < rect.width {
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
    if row_view
        .and_then(|view| view.kind.as_deref())
        .is_some_and(|kind| !block_kind_supported(Some(kind)))
    {
        // In place: the row keeps its tree position and label, and says what
        // Vqro cannot draw instead of dropping it.
        let prefix = row_prefix(&row.lanes, row.is_last);
        let indent_x = 1_u16;
        put_text(
            buffer,
            rect.x,
            rect.y,
            width,
            &prefix,
            Style::default().fg(palette.overlay0),
        );
        put_text(
            buffer,
            rect.x.saturating_add(display_width(&prefix)),
            rect.y,
            1,
            "?",
            Style::default().fg(palette.overlay0),
        );
        put_text(
            buffer,
            rect.x
                .saturating_add(display_width(&prefix))
                .saturating_add(2),
            rect.y,
            width
                .saturating_sub(display_width(&prefix))
                .saturating_sub(2),
            &truncate(
                &row.item.label,
                width
                    .saturating_sub(display_width(&prefix))
                    .saturating_sub(2),
            ),
            Style::default().fg(palette.text),
        );
        let _ = indent_x;
        return;
    }
    let state = row_view.and_then(|view| view.state).unwrap_or_default();
    let tone = state_tone(state);
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
        let join = self.pane_terminal_join();
        // A panel kept under its pane's id while the terminal was unknown moves
        // to the terminal identity as soon as one is known again, so a provider
        // outage does not strand it under the fallback key.
        for (pane_id, terminal_id) in &join {
            if terminal_id == pane_id || self.panels_by_terminal.contains_key(terminal_id) {
                continue;
            }
            if let Some(views) = self.panels_by_terminal.remove(pane_id) {
                self.panels_by_terminal.insert(terminal_id.clone(), views);
            }
            if let Some(display) = self.display_by_terminal.remove(pane_id) {
                self.display_by_terminal
                    .insert(terminal_id.clone(), display);
            }
        }
        let live = self
            .snapshot
            .as_deref()
            .map(|snapshot| {
                snapshot
                    .panes
                    .iter()
                    .map(|pane| pane.pane_id.clone())
                    .collect::<std::collections::BTreeSet<_>>()
            })
            .unwrap_or_default();
        // A pane whose terminal is known from before keeps that identity while
        // it is still live: a provider outage removes every placement, and the
        // panel must not lose what it is keyed by. A pane that never had a
        // placement falls back to its own id, so its panels survive anyway.
        let previous = self.pane_terminal_ids.clone();
        let mut merged = join;
        for pane_id in &live {
            if !merged.contains_key(pane_id) {
                if let Some(known) = previous.get(pane_id) {
                    merged.insert(pane_id.clone(), known.clone());
                } else {
                    merged.insert(pane_id.clone(), pane_id.clone());
                }
            }
        }
        merged.retain(|pane_id, _| live.contains(pane_id));
        self.pane_terminal_ids = merged;
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
            // The same key the rekey uses: the terminal when it is known, the
            // pane's own id otherwise. Skipping an unknown terminal here would
            // drop the user's panel on the floor.
            let terminal_id = self
                .pane_terminal_ids
                .get(&pane_id)
                .cloned()
                .unwrap_or_else(|| pane_id.clone());
            panels.insert(terminal_id, views);
        }
        for (pane_id, display) in std::mem::take(&mut self.pane_panel_display) {
            let terminal_id = self
                .pane_terminal_ids
                .get(&pane_id)
                .cloned()
                .unwrap_or_else(|| pane_id.clone());
            displays.insert(terminal_id, display);
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
            view_id: key.to_string(),
            kind: None,
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
