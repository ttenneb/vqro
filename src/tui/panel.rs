//! Panels: the declarative UI catalogue extensions draw with (D17). Generic, so every extension (and
//! every future client) gets the same elements.
//!
//! A panel value is `{"title"?, "items": [row]}` or `{"title"?, "pages": [{"title", "items"}]}`.
//! A row is:
//!   `{"type"?: "item"|"toggle"|"header"|"separator"|"text", "text", "id"?, "detail"?, "detail_style"?,
//!     "description"?, "checked"?, "style"?, "disabled"?, "badges"?: [{"text", "style"?, "color"?}],
//!     "children"?: [row], "expanded"?: bool, "command"?, "args"?, "input"?, "menu"?: [menu item],
//!     "icon"?: one or two characters before the text, or {"name"?, "nerd"?, "text"?} resolved through
//!     the active icon set (`icons.rs`; named icons and chip icons are feature `icon_names`),
//!     "icon_style"?: a semantic style for it}`
//! More row types: `chips` (`{"type": "chips", "chips": [chip]}`, a wrapping row of small buttons or
//! toggles; a chip is `{"text", "icon"?, "checked"?, "style"?, "command"?, "args"?, "input"?, "options"?}`) and
//! `select` (`{"type": "select", "text", "value", "options": [{"label", "value"}], "command", "args"?}`).
//! Anything with `options` opens a pick-one menu; the chosen `value` is added to its args.
//! A panel may pin rows above its list with `"toolbar": [row]`; they never scroll away.
//! Unknown types render as plain items (forward compatible). Selection, expansion, page, filter and
//! scroll are client state (per view), never written back.
//!
//! Colours come from the client's palette (`theme.rs`) through semantic style names, so panels follow
//! the theme. A badge's `color` (any palette colour value) is the one exception: it shows a colour as
//! data, e.g. a theme preview swatch, and needs the `badge_color` feature.

use std::collections::HashMap;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use serde_json::Value;

use super::icons::{self, Icons, truncate};
use super::theme::{self, Palette};

/// Per-view interaction state, owned by the client.
#[derive(Default, Clone)]
pub struct State {
    pub sel: usize,
    /// Expansion the user chose, by row id (overrides the row's `expanded`).
    pub open: HashMap<String, bool>,
    pub page: usize,
    pub filter: String,
    pub top: usize,
    /// Focused chip within a selected chips row.
    pub chip: usize,
}

/// A visible row after pages, tree expansion and filtering.
#[derive(Clone)]
pub struct Flat {
    pub item: Value,
    pub depth: usize,
    pub id: String,
    pub parent: Option<usize>,
    pub has_children: bool,
    pub expanded: bool,
    /// A toolbar row: drawn above the list, never scrolled.
    pub pinned: bool,
}

#[derive(Default)]
pub struct Hits {
    pub rows: Vec<(Rect, usize)>,
    pub expanders: Vec<(Rect, usize)>,
    pub pages: Vec<(Rect, usize)>,
    /// Chips: rect, row index, chip index.
    pub chips: Vec<(Rect, usize, usize)>,
}

/// Panel features this client renders. A panel may list what it needs in `requires`.
pub const SUPPORTED: &[&str] = &["items", "pages", "tree", "badges", "menu", "input", "description", "filter", "toolbar", "chips", "select", "options", "badge_color", "icon", "icon_names"];

/// Features a panel needs that this client lacks.
pub fn missing(v: &Value) -> Vec<String> {
    v["requires"].as_array().into_iter().flatten().filter_map(|x| x.as_str()).filter(|f| !SUPPORTED.contains(f)).map(String::from).collect()
}

pub fn kind(item: &Value) -> &str {
    item["type"].as_str().unwrap_or("item")
}

pub fn selectable(item: &Value) -> bool {
    !matches!(kind(item), "header" | "separator" | "text") && item["disabled"] != true
}

pub fn page_titles(v: &Value) -> Vec<String> {
    v["pages"].as_array().map(|p| p.iter().map(|x| x["title"].as_str().unwrap_or("page").to_string()).collect()).unwrap_or_default()
}

fn page_items<'a>(v: &'a Value, st: &State) -> &'a [Value] {
    if let Some(pages) = v["pages"].as_array() {
        let p = st.page.min(pages.len().saturating_sub(1));
        return pages.get(p).and_then(|x| x["items"].as_array()).map(|a| a.as_slice()).unwrap_or(&[]);
    }
    v["items"].as_array().map(|a| a.as_slice()).unwrap_or(&[])
}

fn matches(item: &Value, f: &str) -> bool {
    let hay = format!("{} {} {}", item["text"].as_str().unwrap_or(""), item["detail"].as_str().unwrap_or(""), item["description"].as_str().unwrap_or(""));
    hay.to_lowercase().contains(f)
}

fn subtree_matches(item: &Value, f: &str) -> bool {
    matches(item, f) || item["children"].as_array().map(|c| c.iter().any(|x| subtree_matches(x, f))).unwrap_or(false)
}

/// The rows to show, in order.
pub fn rows(v: &Value, st: &State) -> Vec<Flat> {
    let f = st.filter.to_lowercase();
    let mut out = Vec::new();
    fn walk(items: &[Value], depth: usize, parent: Option<usize>, prefix: &str, st: &State, f: &str, out: &mut Vec<Flat>) {
        for (i, item) in items.iter().enumerate() {
            if !f.is_empty() && !subtree_matches(item, f) {
                continue;
            }
            let id = item["id"].as_str().map(String::from).unwrap_or_else(|| format!("{prefix}{i}"));
            let children = item["children"].as_array().map(|c| c.as_slice()).unwrap_or(&[]);
            let has_children = !children.is_empty();
            // Filtering opens everything that matched below.
            let expanded = has_children && (!f.is_empty() || st.open.get(&id).copied().unwrap_or(item["expanded"] == true));
            let mut shown = item.clone();
            if let Some(o) = shown.as_object_mut() {
                o.remove("children");
            }
            let me = out.len();
            out.push(Flat { item: shown, depth, id: id.clone(), parent, has_children, expanded, pinned: false });
            if expanded {
                walk(children, depth + 1, Some(me), &format!("{id}/"), st, f, out);
            }
        }
    }
    for (i, item) in v["toolbar"].as_array().into_iter().flatten().enumerate() {
        out.push(Flat { item: item.clone(), depth: 0, id: format!("toolbar/{i}"), parent: None, has_children: false, expanded: false, pinned: true });
    }
    walk(page_items(v, st), 0, None, "", st, &f, &mut out);
    out
}

/// Ids of every row with children on the current page (for expand/collapse all).
pub fn parent_ids(v: &Value, st: &State) -> Vec<String> {
    let mut out = Vec::new();
    fn walk(items: &[Value], prefix: &str, out: &mut Vec<String>) {
        for (i, item) in items.iter().enumerate() {
            let id = item["id"].as_str().map(String::from).unwrap_or_else(|| format!("{prefix}{i}"));
            if let Some(c) = item["children"].as_array().filter(|c| !c.is_empty()) {
                out.push(id.clone());
                walk(c, &format!("{id}/"), out);
            }
        }
    }
    walk(page_items(v, st), "", &mut out);
    out
}

fn chip_label(c: &Value, icons: &Icons) -> String {
    let t = icons.label(&c["icon"], c["text"].as_str().unwrap_or(""));
    if c["options"].is_array() {
        format!(" {t} ▾ ")
    } else {
        format!(" {t} ")
    }
}

/// Where each chip goes in a row of `width`, wrapping: (dx, dy, width).
fn chip_layout(item: &Value, width: u16, icons: &Icons) -> Vec<(u16, u16, u16)> {
    let mut out = Vec::new();
    let (mut x, mut y) = (0u16, 0u16);
    for c in item["chips"].as_array().into_iter().flatten() {
        let w = icons::width(&chip_label(c, icons)).min(width.max(1));
        if x > 0 && x + w > width {
            x = 0;
            y += 1;
        }
        out.push((x, y, w));
        x += w + 1;
    }
    out
}

/// The next selectable row from `from` in direction `delta` (any distance), clamped at the ends.
pub fn step(rows: &[Flat], from: usize, delta: i64) -> usize {
    if rows.is_empty() {
        return 0;
    }
    let dir = delta.signum();
    let mut i = from.min(rows.len() - 1) as i64;
    let mut left = delta.abs();
    let mut last_ok = if selectable(&rows[i as usize].item) { Some(i) } else { None };
    while left > 0 {
        i += dir;
        if i < 0 || i >= rows.len() as i64 {
            break;
        }
        if selectable(&rows[i as usize].item) {
            last_ok = Some(i);
            left -= 1;
        }
    }
    last_ok.map(|x| x as usize).unwrap_or_else(|| first(rows))
}

pub fn first(rows: &[Flat]) -> usize {
    rows.iter().position(|r| selectable(&r.item)).unwrap_or(0)
}

pub fn last(rows: &[Flat]) -> usize {
    rows.iter().rposition(|r| selectable(&r.item)).unwrap_or(0)
}

fn lines(item: &Value, width: u16, icons: &Icons) -> u16 {
    if kind(item) == "chips" {
        return chip_layout(item, width, icons).last().map(|c| c.1 + 1).unwrap_or(1);
    }
    1 + item["description"].as_str().map(|_| 1).unwrap_or(0)
}

/// Height the content wants at `width` (for sizing popups), including the page bar and toolbar rule.
pub fn height(v: &Value, st: &State, width: u16, icons: &Icons) -> u16 {
    let bar = if page_titles(v).is_empty() { 0 } else { 2 };
    let rows = rows(v, st);
    let rule = rows.iter().any(|r| r.pinned) as u16;
    bar + rule + rows.iter().map(|r| lines(&r.item, width, icons)).sum::<u16>()
}

fn text_style(item: &Value, pal: &Palette) -> Style {
    match item["style"].as_str() {
        None if item["disabled"] == true => pal.style(Some("dim")),
        s => pal.style(s),
    }
}

/// A badge: its `color` if it names one this client can show, else its semantic style.
fn badge_style(b: &Value, pal: &Palette) -> Style {
    match theme::parse_color(&b["color"]) {
        Some(c) => Style::new().bg(c).fg(pal.badge_text),
        None => pal.badge(b["style"].as_str()),
    }
}

/// Draw the panel into `area`. Keeps the selection visible and returns hit areas.
pub fn render(buf: &mut Buffer, area: Rect, v: &Value, st: &mut State, focused: bool, pal: &Palette, icons: &Icons) -> Hits {
    let mut hits = Hits::default();
    if area.width < 6 || area.height == 0 {
        return hits;
    }
    let missing = missing(v);
    if !missing.is_empty() {
        buf.set_string(area.x, area.y, "This view needs a newer prim UI", pal.style(Some("warn")));
        if area.height > 1 {
            buf.set_string(area.x, area.y + 1, format!("(missing: {})", missing.join(", ")), pal.style(Some("dim")));
        }
        return hits;
    }
    let mut area = area;

    // Page bar.
    let titles = page_titles(v);
    if !titles.is_empty() {
        st.page = st.page.min(titles.len() - 1);
        let mut x = area.x;
        for (i, t) in titles.iter().enumerate() {
            let label = format!(" {t} ");
            let w = icons::width(&label);
            if x + w > area.right() {
                break;
            }
            let style = if i == st.page { pal.badge(Some("accent")).add_modifier(Modifier::BOLD) } else { pal.style(Some("muted")) };
            buf.set_string(x, area.y, &label, style);
            hits.pages.push((Rect { x, y: area.y, width: w, height: 1 }, i));
            x += w + 1;
        }
        if area.height > 1 {
            buf.set_string(area.x, area.y + 1, "─".repeat(area.width as usize), pal.border(false));
        }
        area = Rect { y: area.y + 2, height: area.height.saturating_sub(2), ..area };
    }
    if !st.filter.is_empty() && area.height > 1 {
        buf.set_string(area.x, area.y, format!("/{}", st.filter), Style::new().fg(pal.prompt));
        area = Rect { y: area.y + 1, height: area.height - 1, ..area };
    }

    let rows = rows(v, st);
    if rows.is_empty() {
        let msg = if st.filter.is_empty() { "(empty)" } else { "(no matches)" };
        buf.set_string(area.x, area.y, msg, pal.style(Some("dim")));
        return hits;
    }
    st.sel = st.sel.min(rows.len() - 1);
    if !selectable(&rows[st.sel].item) {
        st.sel = step(&rows, st.sel, 1);
    }

    // Pinned toolbar rows first, then a rule, then the scrolling list.
    let pinned = rows.iter().take_while(|r| r.pinned).count();
    let mut y = area.y;
    for (i, r) in rows.iter().enumerate().take(pinned) {
        let h = lines(&r.item, area.width, icons).min(area.bottom().saturating_sub(y));
        if h == 0 {
            break;
        }
        draw_row(buf, Rect { x: area.x, y, width: area.width, height: h }, r, focused && i == st.sel, st.chip, i, &mut hits, pal, icons);
        y += h;
    }
    if pinned > 0 && y < area.bottom() {
        buf.set_string(area.x, y, "─".repeat(area.width as usize), pal.border(false));
        y += 1;
    }
    let area = Rect { y, height: area.bottom().saturating_sub(y), ..area };
    if area.height == 0 {
        return hits;
    }
    let list = &rows[pinned..];

    // Scroll so the selection is visible, in lines.
    let heights: Vec<u16> = list.iter().map(|r| lines(&r.item, area.width, icons)).collect();
    let total: u16 = heights.iter().sum();
    let mut top = st.top as u16;
    if st.sel >= pinned {
        let s = st.sel - pinned;
        let sel_start: u16 = heights[..s].iter().sum();
        let sel_end = sel_start + heights[s];
        if sel_start < top {
            top = sel_start;
        }
        if sel_end > top + area.height {
            top = sel_end.saturating_sub(area.height);
        }
    }
    top = top.min(total.saturating_sub(area.height));
    st.top = top as usize;
    let scrollbar = total > area.height;
    let body = Rect { width: area.width - scrollbar as u16, ..area };

    let mut line: u16 = 0;
    for (j, r) in list.iter().enumerate() {
        let i = j + pinned;
        let h = heights[j];
        if line + h <= top {
            line += h;
            continue;
        }
        let y = body.y + (line - top);
        line += h;
        if y >= body.bottom() {
            break;
        }
        draw_row(buf, Rect { x: body.x, y, width: body.width, height: h.min(body.bottom() - y) }, r, focused && i == st.sel, st.chip, i, &mut hits, pal, icons);
    }

    if scrollbar {
        let x = area.right() - 1;
        let thumb = ((area.height as u32 * area.height as u32) / total as u32).max(1) as u16;
        let pos = ((top as u32 * area.height as u32) / total as u32) as u16;
        for dy in 0..area.height {
            let (sym, style) = if dy >= pos && dy < pos + thumb { ("┃", Style::new().fg(pal.accent)) } else { ("│", pal.border(false)) };
            buf.set_string(x, area.y + dy, sym, style);
        }
    }
    hits
}

#[allow(clippy::too_many_arguments)]
fn draw_row(buf: &mut Buffer, row: Rect, r: &Flat, selected: bool, chip: usize, i: usize, hits: &mut Hits, pal: &Palette, icons: &Icons) {
    let item = &r.item;
    let w = row.width as usize;
    let y = row.y;
    let text = item["text"].as_str().unwrap_or("");
    let indent = (r.depth * 2) as u16;
    match kind(item) {
        "chips" => {
            let chips = item["chips"].as_array().cloned().unwrap_or_default();
            for (ci, ((dx, dy, cw), c)) in chip_layout(item, row.width, icons).into_iter().zip(chips.iter()).enumerate() {
                if dy >= row.height {
                    break;
                }
                let style = match c["checked"].as_bool() {
                    Some(true) => pal.badge(Some("accent")),
                    Some(false) => Style::new().bg(pal.badge).fg(pal.muted),
                    None => Style::new().fg(pal.accent).bg(pal.surface),
                };
                let style = if let Some(n) = c["style"].as_str() { style.patch(pal.style(Some(n))) } else { style };
                let style = if selected && ci == chip { style.add_modifier(Modifier::REVERSED | Modifier::BOLD) } else { style };
                let rect = Rect { x: row.x + dx, y: y + dy, width: cw, height: 1 };
                buf.set_string(rect.x, rect.y, truncate(&chip_label(c, icons), cw as usize), style);
                hits.chips.push((rect, i, ci));
            }
            hits.rows.push((row, i));
        }
        "select" => {
            let base = if selected { pal.selected() } else { Style::new() };
            // The row's own colours, with the selection's on top when the theme sets them.
            let ink = |s: Style| base.patch(s).patch(base);
            buf.set_string(row.x, y, " ".repeat(w), base);
            let value = item["value"].clone();
            let shown = item["options"]
                .as_array()
                .and_then(|o| o.iter().find(|x| x["value"] == value))
                .and_then(|x| x["label"].as_str())
                .map(String::from)
                .or_else(|| value.as_str().map(String::from))
                .unwrap_or_default();
            let label = format!(" {text}: ");
            buf.set_string(row.x + indent, y, &label, ink(pal.style(Some("muted"))));
            let vx = row.x + indent + icons::width(&label);
            buf.set_string(vx, y, truncate(&format!("{shown} ▾"), w.saturating_sub((vx - row.x) as usize)), ink(pal.style(Some("accent"))));
            hits.rows.push((row, i));
        }
        "separator" => buf.set_string(row.x, y, "─".repeat(w), pal.border(false)),
        "header" => buf.set_string(row.x + indent, y, truncate(text, w.saturating_sub(indent as usize)), pal.style(Some("accent")).add_modifier(Modifier::BOLD)),
        "text" => buf.set_string(row.x + indent, y, truncate(text, w.saturating_sub(indent as usize)), text_style(item, pal).fg(pal.muted)),
        k => {
            let base = if selected { pal.selected() } else { Style::new() };
            // The row's own colours, with the selection's on top when the theme sets them.
            let ink = |s: Style| base.patch(s).patch(base);
            buf.set_string(row.x, y, " ".repeat(w), base);
            let mut x = row.x + indent;
            if r.has_children {
                buf.set_string(x, y, if r.expanded { "▾ " } else { "▸ " }, ink(pal.style(Some("accent"))));
                hits.expanders.push((Rect { x, y, width: 2, height: 1 }, i));
            } else if r.depth > 0 {
                buf.set_string(x, y, "  ", base);
            }
            x += 2 * (r.has_children || r.depth > 0) as u16;
            if k == "toggle" {
                let on = item["checked"] == true;
                buf.set_string(x, y, if on { "[x] " } else { "[ ] " }, ink(pal.style(Some(if on { "ok" } else { "dim" }))));
                x += 4;
            } else if r.depth == 0 && !r.has_children {
                x += 1;
            }
            // An icon says what the row is; its style can carry state (e.g. an agent working). It
            // fills the set's icon column, so text lines up across 1- and 2-cell glyphs.
            if let Some(icon) = icons.resolve(&item["icon"]) {
                let icon: String = icon.chars().take(2).collect();
                let (cell, cells) = icons.cell(&icon);
                buf.set_string(x, y, &cell, ink(pal.style(item["icon_style"].as_str().or(Some("muted")))));
                x += cells + 1;
            }
            // Right side: detail, then badges before it.
            let detail = item["detail"].as_str().unwrap_or("");
            let mut right = row.right().saturating_sub(1);
            if !detail.is_empty() {
                let dw = icons::width(detail);
                if right > x + dw + 2 {
                    right -= dw;
                    let ds = item["detail_style"].as_str().unwrap_or("dim");
                    let ds = pal.style(Some(ds));
                    buf.set_string(right, y, detail, ink(ds));
                    right = right.saturating_sub(1);
                }
            }
            let badges: Vec<(String, Style)> = item["badges"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|b| Some((format!(" {} ", b["text"].as_str()?), badge_style(b, pal))))
                .collect();
            let room_for_text = |right: u16| right.saturating_sub(x) as usize;
            let mut bx = right;
            let mut placed = Vec::new();
            // The name keeps some room before badges get any.
            let keep = icons::width(text).min(12) + 1;
            for (t, s) in badges.iter().rev() {
                let bw = icons::width(t);
                if bx < x + bw + keep {
                    break;
                }
                bx -= bw;
                placed.push((bx, t.clone(), *s));
                bx = bx.saturating_sub(1);
            }
            buf.set_string(x, y, truncate(text, room_for_text(bx)), ink(text_style(item, pal)));
            for (bx, t, s) in placed {
                buf.set_string(bx, y, t, s);
            }
            if let Some(d) = item["description"].as_str() {
                if y + 1 < row.bottom() {
                    let dx = row.x + indent + 4;
                    buf.set_string(dx, y + 1, truncate(d, w.saturating_sub(dx as usize - row.x as usize)), pal.style(Some("dim")));
                }
            }
            hits.rows.push((row, i));
        }
    }
}

/// Activate a row: run its command (the caller does that), or open/close it if it only has children.
/// Returns the row to run, if any.
pub fn activate(rows: &[Flat], st: &mut State, i: usize) -> Option<Value> {
    let r = rows.get(i)?;
    if !selectable(&r.item) {
        return None;
    }
    if kind(&r.item) == "chips" {
        let c = r.item["chips"].get(st.chip)?;
        return (c["command"].is_string() && c["disabled"] != true).then(|| c.clone());
    }
    if r.item["command"].is_string() {
        return Some(r.item.clone());
    }
    if r.has_children {
        st.open.insert(r.id.clone(), !r.expanded);
    }
    None
}

/// ←: close an open row, else go to its parent. →: open a closed row, else go to its first child.
pub fn left(rows: &[Flat], st: &mut State) {
    let Some(r) = rows.get(st.sel) else { return };
    if r.has_children && r.expanded {
        st.open.insert(r.id.clone(), false);
    } else if let Some(p) = r.parent {
        st.sel = p;
    }
}

pub fn right(rows: &[Flat], st: &mut State) {
    let Some(r) = rows.get(st.sel) else { return };
    if r.has_children && !r.expanded {
        st.open.insert(r.id.clone(), true);
    } else if r.has_children && st.sel + 1 < rows.len() {
        st.sel += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Draw a panel and return its lines, skipping the cell a wide glyph covers.
    fn draw(v: &Value, icons: &Icons, width: u16) -> Vec<String> {
        let area = Rect::new(0, 0, width, 6);
        let mut buf = Buffer::empty(area);
        render(&mut buf, area, v, &mut State::default(), false, &Palette::default(), icons);
        let line = |y| {
            let mut s = String::new();
            let mut x = 0;
            while x < width {
                let sym = buf[(x, y)].symbol();
                s.push_str(sym);
                x += icons::width(sym).max(1);
            }
            s.trim_end().to_string()
        };
        (0..area.height).map(line).collect()
    }

    fn rows() -> Value {
        json!({"items": [
            {"text": "agent", "icon": {"name": ["working", "agent"], "nerd": "\u{f06a9}", "text": "◆"}},
            {"text": "repo", "icon": {"name": "repo", "text": "⎇"}},
            {"text": "plain", "icon": "x"},
            {"type": "chips", "chips": [{"text": "", "icon": {"name": "filter", "text": "/"}}, {"text": "all"}]},
        ]})
    }

    #[test]
    fn no_set_draws_as_before() {
        let lines = draw(&rows(), &Icons::new(None, false), 30);
        assert_eq!(lines[..4], [" ◆ agent", " ⎇ repo", " x plain", " /   all"]);
    }

    #[test]
    fn a_wide_set_keeps_text_aligned() {
        // The set has the state glyph and not `repo`: the repo row falls back but keeps the column.
        let icons = Icons::new(Some(&json!({"name": "emoji", "width": 2, "glyphs": {"working": "⏳", "filter": "🔍"}})), false);
        let lines = draw(&rows(), &icons, 30);
        let col = |l: &str, t: &str| l.find(t).map(|b| icons::width(&l[..b]));
        assert_eq!(col(&lines[0], "agent"), Some(4));
        assert_eq!(col(&lines[1], "repo"), Some(4));
        assert_eq!(col(&lines[2], "plain"), Some(4));
        assert!(lines[0].starts_with(" ⏳"));
        assert!(lines[3].starts_with(" 🔍"));
    }
}
