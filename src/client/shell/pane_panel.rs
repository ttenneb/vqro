//! View panes: Vqro draws extension views inside complete pane perimeters.
//!
//! Meaning, hierarchy and state text belong to the publishing extension. This
//! module only selects the views published for a pane, draws cached content
//! within the host-projected inner rectangle in the same shape as the
//! surrounding UI (tree connectors, state glyphs, one line per row) and joins
//! the live terminal and agent facts Vqro owns. Nothing here reads a terminal,
//! the filesystem or a process tree: the work is bounded by the panes on screen
//! and the rows that fit.
use std::collections::{BTreeSet, HashMap};

use super::render::{display_width, put_text};
use super::*;
use crate::extension_contract::{HostDocumentItem, HostViewState, HostViewTone};
use crate::protocol::endpoint::HostDocumentsProjectionV1;

#[cfg(test)]
mod frame_tests;

/// A published view that fills one pane's slot.
struct PanelView<'a> {
    title: &'a str,
    /// The extension's own summary of the slice, drawn after the title.
    summary: &'a str,
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
}

/// One view pane, as the host projects it: the content identity and the public
/// pane ids it covers. The scope is a host fact, shared by every client.
pub(super) struct ViewPaneProjection<'a> {
    pub(super) extension: &'a str,
    pub(super) view: &'a str,
    pub(super) scope: &'a [String],
}

/// Select the published view roots that cover a view pane's scope. The extension
/// keeps the meaning and the rows; core only supplies the covered terminals.
fn view_sections<'a>(
    index: &'a PanelIndex<'a>,
    documents: &HostDocumentsProjectionV1,
    projection: &ViewPaneProjection<'_>,
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
        if item.flags.disabled || view_id != projection.view {
            continue;
        }
        if !projection.scope.iter().any(|covered| covered == item_pane) {
            continue;
        }
        if documents
            .documents
            .get(*document)
            .is_some_and(|entry| entry.document.namespace != projection.extension)
        {
            continue;
        }
        views.push(PanelView {
            title: item.label.as_str(),
            summary: item.detail.as_deref().unwrap_or_default(),
            rows: flatten(index, *document, item.id.as_str(), &BTreeSet::new()),
        });
    }
    views
}

/// The view header's scope text: one covered terminal is named, several are
/// counted. The covered terminals come from the host's placement-derived scope.
fn scope_label(agents: &[crate::protocol::ClientShellAgent], scope: &[String]) -> String {
    if scope.is_empty() {
        return "no terminals".to_owned();
    }
    if scope.len() > 1 {
        return format!("{} terminals", scope.len());
    }
    let pane = scope[0].as_str();
    agents
        .iter()
        .find(|agent| agent.pane_id == pane)
        .and_then(|agent| {
            agent
                .display_agent
                .as_deref()
                .or(agent.name.as_deref())
                .or(agent.agent.as_deref())
        })
        .map(str::to_owned)
        .unwrap_or_else(|| pane.to_owned())
}

fn draw_view_header(
    buffer: &mut Buffer,
    rect: Rect,
    title: &str,
    scope: &str,
    summary: &str,
    focused: bool,
    palette: &Palette,
) {
    if rect.width == 0 || rect.height == 0 {
        return;
    }
    let bg = palette.panel_bg;
    let dim = Style::default()
        .fg(if focused {
            palette.accent
        } else {
            palette.overlay0
        })
        .bg(bg);
    let strong = Style::default()
        .fg(palette.text)
        .bg(bg)
        .add_modifier(Modifier::BOLD);
    // The caller supplies only the top edge's interior, preserving both corners.
    let line = "─".repeat(rect.width as usize);
    put_text(buffer, rect.x, rect.y, rect.width, &line, dim);
    let left = format!(" {title} · {scope} ");
    let left_width = display_width(&left).min(rect.width.saturating_sub(2));
    put_text(
        buffer,
        rect.x,
        rect.y,
        left_width,
        &truncate(&left, left_width),
        strong,
    );
    let right = format!(" {summary} ");
    let right_width = display_width(&right);
    if crate::view_sizing::header_outer_width(u32::from(left_width), u32::from(right_width))
        <= u32::from(rect.width) + 2
    {
        put_text(
            buffer,
            rect.x + rect.width - right_width - 1,
            rect.y,
            right_width,
            &truncate(&right, right_width),
            dim,
        );
    }
}

/// Draw one view pane: its frame and the rows the extension published for the
/// covered terminals. Read-only; it claims no terminal and resizes nothing.
#[allow(clippy::too_many_arguments)]
pub(super) fn render_view_pane(
    buffer: &mut Buffer,
    index: &PanelIndex<'_>,
    documents: &HostDocumentsProjectionV1,
    snapshot: &ClientShellSnapshot,
    pane: &PaneHit,
    projection: &ViewPaneProjection<'_>,
    palette: &Palette,
) {
    let outer = pane.rect.intersection(buffer.area);
    if outer.width < 4 || outer.height < 2 {
        return;
    }
    let views = view_sections(index, documents, projection);
    let title = views.first().map_or(projection.view, |view| view.title);
    let summary = views
        .iter()
        .map(|view| view.summary)
        .find(|text| !text.is_empty())
        .unwrap_or(crate::view_sizing::READ_ONLY_SUMMARY);
    let scope = scope_label(&snapshot.agents, projection.scope);
    let header = Rect::new(outer.x + 1, outer.y, outer.width.saturating_sub(2), 1);
    let background = Style::default().bg(palette.panel_bg);
    let focused = snapshot.focused_pane_id.as_deref() == Some(pane.pane_id.as_str());
    let edge = Style::default()
        .fg(if focused {
            palette.accent
        } else {
            palette.overlay0
        })
        .bg(palette.panel_bg);
    ratatui::widgets::Widget::render(
        ratatui::widgets::Block::default()
            .borders(ratatui::widgets::Borders::ALL)
            .border_style(edge),
        outer,
        buffer,
    );
    draw_view_header(buffer, header, title, &scope, summary, focused, palette);
    // Host geometry already accounts for child borders and any stack header.
    let body = pane
        .inner_rect
        .intersection(outer)
        .intersection(buffer.area);
    let columns = body.width;
    if body.is_empty() {
        return;
    }
    for y in body.y..body.bottom() {
        for x in body.x..body.right() {
            put_text(buffer, x, y, 1, " ", background);
        }
    }
    let agents: HashMap<&str, &crate::protocol::ClientShellAgent> = snapshot
        .agents
        .iter()
        .map(|agent| (agent.pane_id.as_str(), agent))
        .collect();
    let collapsed = BTreeSet::new();
    let mut y = body.y;
    let mut drew_row = false;
    for view in &views {
        for row in &view.rows {
            if y >= body.bottom() {
                break;
            }
            let rect = Rect::new(body.x, y, columns, 1);
            draw_row(buffer, rect, row, &collapsed, &agents, columns, palette);
            drew_row = true;
            y = y.saturating_add(1);
        }
        if y >= body.bottom() {
            break;
        }
    }
    if !drew_row {
        let text = if views.is_empty() {
            format!("└─ {} unavailable", projection.extension)
        } else {
            "└─ nothing to show".to_owned()
        };
        put_text(
            buffer,
            body.x,
            body.y,
            columns,
            &truncate(&text, columns),
            Style::default().fg(palette.overlay0).bg(palette.panel_bg),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(pane_id: &str, name: Option<&str>) -> crate::protocol::ClientShellAgent {
        crate::protocol::ClientShellAgent {
            pane_id: pane_id.into(),
            workspace_id: "w1".into(),
            tab_id: "w1:t1".into(),
            name: name.map(str::to_owned),
            display_agent: None,
            agent: None,
            title: None,
            terminal_title: None,
            terminal_title_stripped: None,
            agent_status: crate::api::schema::AgentStatus::Unknown,
            state_change_seq: 0,
            state_labels: Vec::new(),
            tokens: Vec::new(),
            focused: false,
        }
    }

    #[test]
    fn automatic_header_budget_keeps_actual_buffer_title_scope_and_summary() {
        let document: crate::extension_contract::HostDocumentV1=serde_json::from_value(serde_json::json!({"contract":"host.document.v1","namespace":"sample.views","generation":1,"revision":1,"surface":"pane","target":{"kind":"tab","tab_id":"w1:t1"},"items":[{"id":"root","role":"section","label":"Tasks","detail":"0/0 done","view":{"kind":"section","pane_id":"w1:p1","view_id":"tasks"}}]})).unwrap();
        let measured = crate::view_sizing::measure_document(&document);
        let mut failures = Vec::new();
        for (scope, columns) in [("worker".to_owned(), 100), ("a".repeat(64), 200)] {
            let terminal = crate::layout::PaneId::from_raw(1);
            let view = crate::layout::PaneId::from_raw(2);
            let mut layout = crate::layout::TileLayout::from_tree(
                crate::layout::TypedNode::Split {
                    direction: ratatui::layout::Direction::Horizontal,
                    ratio: 0.5,
                    first: Box::new(crate::layout::TypedNode::Leaf(terminal)),
                    second: Box::new(crate::layout::TypedNode::view(
                        view,
                        "sample.views",
                        "tasks",
                    )),
                },
                crate::layout::LayoutLeaf::Pane(terminal),
            )
            .unwrap();
            let metric =
                measured[&("tasks".into(), "w1:p1".into())].with_scope(display_width(&scope));
            let area = Rect::new(0, 0, columns, 40);
            layout.auto_size_views(area, &HashMap::from([(view, metric)]));
            let rect = layout
                .panes(area)
                .into_iter()
                .find(|pane| pane.id == view)
                .unwrap()
                .rect;
            let mut buffer = Buffer::empty(area);
            draw_view_header(
                &mut buffer,
                Rect::new(rect.x + 1, rect.y, rect.width.saturating_sub(2), 1),
                "Tasks",
                &scope,
                "0/0 done",
                false,
                &Palette::catppuccin(),
            );
            let line = (rect.x..rect.right())
                .map(|x| buffer[(x, rect.y)].symbol())
                .collect::<String>();
            if !line.contains(&format!("Tasks · {scope}")) || !line.contains("0/0 done") {
                failures.push(format!(
                    "scope_cells={},outer_width={}: {line}",
                    display_width(&scope),
                    rect.width
                ));
            }
        }
        assert!(
            failures.is_empty(),
            "header budget must fit actual title/scope/status when bounds allow: {failures:?}"
        );
    }

    #[test]
    fn view_header_names_one_covered_terminal_and_counts_several() {
        let agents = vec![
            agent("w1:p2", Some("agent1")),
            agent("w1:p3", Some("agent2")),
        ];
        assert_eq!("agent1", scope_label(&agents, &["w1:p2".to_owned()]));
        assert_eq!(
            "2 terminals",
            scope_label(&agents, &["w1:p2".to_owned(), "w1:p3".to_owned()])
        );
        // An unnamed covered pane falls back to its public id, and an empty
        // scope says so rather than inventing a name.
        assert_eq!("w1:p9", scope_label(&agents, &["w1:p9".to_owned()]));
        assert_eq!("no terminals", scope_label(&agents, &[]));
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
