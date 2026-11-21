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
//! Optional `list_detail: {requires:["list_detail"]}` on a panel/page displays the selected row's
//! `detail_panel: {title?, requires?, toolbar?, items}` beside/below it; ordinary items remain a fallback.
//! `summary`/string `tags` extend search. Text rows support `wrap:true` and `focusable:true` for reading.
//! Unknown types render as plain items (forward compatible). Selection, expansion, page, filter, facets,
//! sort and scroll are client state (per view), never written back. Panels may declare `facets` (a client-drawn
//! `Show:` line), `sorts` (`Sort:` on the same line) and `columns` (aligned cells with an optional header line);
//! see `ui_model::panel` and core-protocol.md, "Facets, sort and columns".
//!
//! Colours come from the client's palette (`theme.rs`) through semantic style names, so panels follow
//! the theme. A badge's `color` (any palette colour value) is the one exception: it shows a colour as
//! data, e.g. a theme preview swatch, and needs the `badge_color` feature.

use std::sync::Arc;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use serde_json::Value;

use super::icons::{self, Icons, truncate};
use super::theme::{self, Palette};

// Flattening, selection and `requires` are the renderer-independent model; this module draws it.
pub use crate::ui_model::panel::*;

#[derive(Default)]
pub struct Hits {
    pub rows: Vec<(Rect, usize)>,
    pub expanders: Vec<(Rect, usize)>,
    pub pages: Vec<(Rect, usize)>,
    /// Chips: rect, row index, chip index.
    pub chips: Vec<(Rect, usize, usize)>,
    pub detail: Option<Box<Hits>>,
    pub detail_area: Option<Rect>,
    pub toggles: Vec<(Rect,usize)>,
    /// The `Show:` line's chips: rect, facet key, value (None: All).
    pub facets: Vec<(Rect, String, Option<String>)>,
    /// The `Sort:` chip.
    pub sort: Option<Rect>,
    /// "↓ Latest · End" on a following list scrolled away from its end.
    pub latest: Option<Rect>,
    /// Inline row buttons (`actions`): rect, row index, action index.
    pub actions: Vec<(Rect, usize, usize)>,
}

fn wrap_text(text:&str,width:u16)->Vec<String> {
    let width=width.max(1);let mut out=Vec::new();
    for paragraph in text.split('\n') {
        let mut line=String::new();let mut used=0;
        for word in paragraph.split_whitespace() {
            let size=icons::width(word);
            if used>0 && used+1+size>width {out.push(std::mem::take(&mut line));used=0;}
            if used>0 {line.push(' ');used+=1;}
            for c in word.chars() {let size=icons::width(&c.to_string());if used+size>width && !line.is_empty(){out.push(std::mem::take(&mut line));used=0;}line.push(c);used+=size;}
        }
        out.push(line);
    }
    out
}

/// A chip's label: its `short` one when the row is narrow (`short`), else its text, with the icon.
fn chip_label(c: &Value, icons: &Icons, short: bool) -> String {
    let text = if short { c["short"].as_str().or(c["text"].as_str()) } else { c["text"].as_str() };
    let t = icons.label(&c["icon"], text.unwrap_or(""));
    if c["options"].is_array() {
        format!(" {t} ▾ ")
    } else {
        format!(" {t} ")
    }
}

/// Where each chip goes in a row of `width`: (dx, dy, width, short label). When the chips don't fit on one line,
/// those with a `short` label use it. Chips with `align: "right"` sit at the right end of the line when everything
/// fits on one; otherwise all chips wrap in order.
fn chip_layout(item: &Value, width: u16, icons: &Icons) -> Vec<(u16, u16, u16, bool)> {
    let chips: Vec<&Value> = item["chips"].as_array().into_iter().flatten().collect();
    let widths = |short: bool| chips.iter().map(|c| icons::width(&chip_label(c, icons, short)).min(width.max(1))).collect::<Vec<u16>>();
    let line = |ws: &[u16]| ws.iter().map(|w| u32::from(*w) + 1).sum::<u32>().saturating_sub(1);
    let mut short = false;
    let mut ws = widths(false);
    if line(&ws) > u32::from(width) && chips.iter().any(|c| c["short"].is_string()) {
        short = true;
        ws = widths(true);
    }
    let right: Vec<bool> = chips.iter().map(|c| c["align"] == "right").collect();
    let mut out = Vec::new();
    if right.iter().any(|r| *r) && line(&ws) <= u32::from(width) {
        let rights: Vec<u16> = ws.iter().zip(&right).filter(|(_, r)| **r).map(|(w, _)| *w).collect();
        let mut rx = width.saturating_sub(line(&rights) as u16);
        let mut x = 0u16;
        for (w, r) in ws.iter().zip(&right) {
            if *r { out.push((rx, 0, *w, short)); rx += w + 1; } else { out.push((x, 0, *w, short)); x += w + 1; }
        }
        return out;
    }
    let (mut x, mut y) = (0u16, 0u16);
    for w in ws {
        if x > 0 && x + w > width {
            x = 0;
            y += 1;
        }
        out.push((x, y, w, short));
        x += w + 1;
    }
    out
}

/// A row's height; a composer's follows its draft (it grows as you type, up to its `rows`); a folding row's its
/// shown lines.
fn row_lines(r: &Flat, st: &State, width: u16, icons: &Icons) -> u16 {
    if kind(&r.item) == "composer" {
        let draft = st.composers.get(&r.id).map(|c| c.editor.lines().0.len()).unwrap_or(1);
        return crate::ui_model::panel::composer_height_for(&r.item, draft);
    }
    if kind(&r.item) == "message" || fold(&r.item).is_some() {
        return block_lines(&r.item, width, r.depth, unfolded(r, st)).len().clamp(1, u16::MAX as usize) as u16;
    }
    lines(&r.item, width, icons)
}

/// A text row's or message block's text as lines of (text, style tag), before folding: markdown through the shared
/// layout (feature markdown), else wrapped (messages always wrap) or one line.
fn body_lines(item: &Value, width: u16) -> Vec<crate::ui_model::markdown::Line> {
    let message = kind(item) == "message";
    let text = if message { item["body"].as_str().or(item["text"].as_str()) } else { item["text"].as_str() }.unwrap_or("");
    if item["format"] == "markdown" { return crate::ui_model::markdown::lines(text, width as usize); }
    if message || item["wrap"] == true { return wrap_text(text, width).into_iter().map(|l| vec![(l, "")]).collect(); }
    vec![vec![(text.to_string(), "")]]
}

/// Every line a text row (with `fold`) or message block draws, as (text, style tag, kind): the frame of a message
/// ("top", "side", "bottom"), or its unframed "title" on a card, its body, the fold line ("fold"), and a card's
/// padding lines ("pad"). Tags: markdown's, or "" plain.
fn block_lines(item: &Value, width: u16, depth: usize, open: bool) -> Vec<(crate::ui_model::markdown::Line, &'static str)> {
    let message = kind(item) == "message";
    let (card, frame) = message_look(item, SUPPORTED);
    let framed = message && frame;
    let inner = width.saturating_sub((depth * 2) as u16 + if framed { 2 } else { 0 } + if card { 2 } else { 0 }).max(1);
    let body = body_lines(item, inner);
    let total = body.len();
    let label = fold_label(item, total, open);
    let shown = match (fold(item), open) { (Some(n), false) => n.min(total), _ => total };
    let mut out: Vec<(crate::ui_model::markdown::Line, &'static str)> = Vec::new();
    if card { out.push((Vec::new(), "pad")); }
    if message { out.push((vec![(item["title"].as_str().or(item["text"].as_str()).unwrap_or("").to_string(), "")], if framed { "top" } else { "title" })); }
    out.extend(body.into_iter().take(shown).map(|l| (l, if framed { "side" } else { "" })));
    if let Some(l) = label { out.push((vec![(l, "")], "fold")); }
    if framed { out.push((Vec::new(), "bottom")); }
    if card { out.push((Vec::new(), "pad")); }
    out
}

fn lines(item: &Value, width: u16, icons: &Icons) -> u16 {
    if kind(item)=="composer" {return composer_height(item);}
    if kind(item)=="text" && item["format"]=="markdown" {return crate::ui_model::markdown::lines(item["text"].as_str().unwrap_or(""),width as usize).len().clamp(1,u16::MAX as usize) as u16;}
    if kind(item)=="text" && item["wrap"]==true {return wrap_text(item["text"].as_str().unwrap_or(""),width).len().min(u16::MAX as usize) as u16;}
    if kind(item) == "chips" {
        return chip_layout(item, width, icons).iter().map(|c| c.1 + 1).max().unwrap_or(1);
    }
    1 + item["description"].as_str().map(|_| 1).unwrap_or(0)
}

/// Height the content wants at `width` (for sizing popups), including the page bar and toolbar rule.
pub fn height(v: &Value, st: &State, width: u16, icons: &Icons) -> u16 {
    let bar:u16 = if page_titles(v).is_empty() { 0 } else { 2 };
    if list_detail(v,st){return bar+28;}
    let rows = rows(v, st);
    let rule = rows.iter().any(|r| r.pinned) as u16;
    bar.saturating_add(rule).saturating_add(rows.iter().fold(0u16,|sum,r|sum.saturating_add(row_lines(r,st,width,icons))))
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
pub fn render(buf: &mut Buffer, area: Rect, v: &Arc<Value>, st: &mut State, focused: bool, pal: &Palette, icons: &Icons, key_labels: Option<&str>) -> Hits {
    let mut hits = Hits::default();
    if area.width < 6 || area.height == 0 {
        return hits;
    }
    let missing = missing(v);
    if !missing.is_empty() {
        buf.set_string(area.x, area.y, "This view needs a newer vqro UI", pal.style(Some("warn")));
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

    if list_detail(v,st) {
        let all=st.prepare(v);st.sel=st.sel.min(all.rows.len().saturating_sub(1));
        if !all.rows.get(st.sel).is_some_and(|r|selectable(&r.item)){st.sel=first(&all.rows);}
        let selected=selected_detail(v,st);
        let wide=area.width>=96;
        let (list,detail)=if wide {let w=(area.width*2/5).max(36);(Rect{width:w,..area},Rect{x:area.x+w+1,width:area.width.saturating_sub(w+1),..area})}
            else {let wanted=all.rows.iter().fold(1u16,|h,r|h.saturating_add(lines(&r.item,area.width,icons))).saturating_add(chrome_lines(v,st,area.width,pal));let h=(area.height/2).max(1).min(area.height).min(wanted);(Rect{height:h,..area},Rect{y:area.y+h+1,height:area.height.saturating_sub(h+1),..area})};
        render_list(buf,list,v,st,focused&&!st.detail_focus,pal,icons,&mut hits,true,key_labels);
        if detail.height>1 {
            if wide {for y in area.y..area.bottom(){buf.set_string(detail.x-1,y,"│",pal.border(false));}}
            else if detail.y>area.y {buf.set_string(area.x,detail.y-1,"─".repeat(area.width as usize),pal.border(false));}
            if let Some((id,value))=selected {
                sync_detail(st,&id);
                let title=value["title"].as_str().unwrap_or("Details");
                buf.set_string(detail.x,detail.y,truncate(title,detail.width as usize),pal.style(Some("accent")).add_modifier(Modifier::BOLD));
                let body=Rect{y:detail.y+1,height:detail.height-1,..detail};
                let mut child=st.detail.take().unwrap_or_default();
                hits.detail=Some(Box::new(render(buf,body,&Arc::new(value),&mut child,focused&&st.detail_focus,pal,icons,key_labels)));
                st.detail=Some(child);hits.detail_area=Some(detail);
            }else{buf.set_string(detail.x,detail.y,"Select an item for details",pal.style(Some("dim")));}
        }
    } else { render_list(buf,area,v,st,focused,pal,icons,&mut hits,false,key_labels); }
    hits
}

#[allow(clippy::too_many_arguments)]
fn render_list(buf:&mut Buffer,mut area:Rect,v:&Arc<Value>,st:&mut State,focused:bool,pal:&Palette,icons:&Icons,hits:&mut Hits,indicator:bool,key_labels:Option<&str>) {
    if area.width<2 || area.height==0{return;}
    let prepared = st.prepare(v);
    let rows = &prepared.rows;
    let footer=(indicator && area.height>1).then_some(area.bottom().saturating_sub(1));
    if indicator && area.height>1 {
        let n=rows.iter().filter(|r|selectable(&r.item)).count();
        let total=prepared.total;
        let text=super::keys::display_labels(&format!("{n} of {total} · PgUp/PgDn{} · Tab: details",hint_keys(v,st)),key_labels);
        buf.set_string(area.x,area.bottom()-1,truncate(&text,area.width as usize),pal.style(Some("dim")));
        area.height-=1;
    }
    if rows.is_empty() {
        let msg = if st.filter.is_empty() { "(empty)" } else { "(no matches)" };
        buf.set_string(area.x, area.y, msg, pal.style(Some("dim")));
        return;
    }
    follow(v, rows, st);
    st.sel = st.sel.min(rows.len() - 1);
    if !selectable(&rows[st.sel].item) {
        st.sel = step(&rows, st.sel, 1);
    }

    // Pinned toolbar rows first, then a rule, then the scrolling list.
    let pinned = rows.iter().take_while(|r| r.pinned).count();
    let mut y = area.y;
    for (i, r) in rows.iter().enumerate().take(pinned) {
        let h = row_lines(r, st, area.width, icons).min(area.bottom().saturating_sub(y));
        if h == 0 {
            break;
        }
        draw_row(buf, Rect { x: area.x, y, width: area.width, height: h }, r, focused && i == st.sel, st.chip, i, 0, hits, pal, icons, composer_of(st, r, focused), unfolded(r, st));
        y += h;
    }
    // The facet/sort line (features `facets`, `sort`): client-drawn, below the toolbar.
    let line_h = if y < area.bottom() { facet_line(buf, Rect { x: area.x, y, width: area.width, height: area.bottom() - y }, v, st, pal, hits) } else { 0 };
    let line_drawn = line_h > 0;
    y += line_h;
    // Columns (feature `columns`): one layout for every row of this pane, and a header line when labelled.
    let layout = declared(v, st, "columns").filter(|c| !c.is_empty()).map(|spec| column_layout(spec, rows, area.width.saturating_sub(1), icons));
    if let Some(l) = layout.as_ref().filter(|l| l.header.is_some()) {
        if y < area.bottom() {
            buf.set_string(area.x + l.start, y, truncate(l.header.as_deref().unwrap_or(""), area.width.saturating_sub(l.start) as usize), pal.style(Some("muted")).add_modifier(Modifier::BOLD));
            y += 1;
        }
    }
    if (pinned > 0 || line_drawn || layout.as_ref().is_some_and(|l| l.header.is_some())) && y < area.bottom() {
        buf.set_string(area.x, y, "─".repeat(area.width as usize), pal.border(false));
        y += 1;
    }
    let laid: Vec<Flat>;
    let rows: &[Flat] = match &layout { Some(l) => { laid = rows.iter().map(|r| l.apply(r, icons)).collect(); &laid } None => rows };
    let local = child_tables(rows, area.width.saturating_sub(1), icons);
    let rows = local.as_slice();
    // `pin: "bottom"` rows (feature pin): below the list, never scrolled, at most half of what is left.
    let first_bottom = rows.iter().position(|r| r.bottom).unwrap_or(rows.len());
    let mut bottom_y = area.bottom();
    if first_bottom < rows.len() && area.bottom() > y + 2 {
        let wanted: u16 = rows[first_bottom..].iter().fold(1u16, |h, r| h.saturating_add(row_lines(r, st, area.width, icons)));
        let room = area.bottom() - y;
        bottom_y = area.bottom() - wanted.min((room / 2).max(2));
        buf.set_string(area.x, bottom_y, "─".repeat(area.width as usize), pal.border(false));
        let mut by = bottom_y + 1;
        for (i, r) in rows.iter().enumerate().skip(first_bottom) {
            let h = row_lines(r, st, area.width, icons).min(area.bottom().saturating_sub(by));
            if h == 0 {
                break;
            }
            draw_row(buf, Rect { x: area.x, y: by, width: area.width, height: h }, r, focused && i == st.sel, st.chip, i, 0, hits, pal, icons, composer_of(st, r, focused), unfolded(r, st));
            by += h;
        }
    }
    let area = Rect { y, height: bottom_y.saturating_sub(y), ..area };
    if area.height == 0 {
        return;
    }
    let list = &rows[pinned..first_bottom];

    // Scroll so the selection is visible, in lines.
    let heights: Vec<u32> = list.iter().map(|r| row_lines(r, st, area.width.saturating_sub(1), icons) as u32).collect();
    let total: u32 = heights.iter().sum();
    let mut top = st.top.min(u32::MAX as usize) as u32;
    if v["follow"] == true && !st.manual_scroll && !(st.sel >= pinned && st.sel < first_bottom) {
        top = u32::MAX;  // following while the selection is on a pinned row: keep the list at its end
    } else if st.sel >= pinned && st.sel < first_bottom && !st.manual_scroll {
        let s = st.sel - pinned;
        let sel_start: u32 = heights[..s].iter().sum();
        let sel_end = sel_start + heights[s];
        if sel_start < top {
            top = sel_start;
        }
        if sel_end > top + area.height as u32 {
            top = if heights[s]>area.height as u32 {sel_start}else{sel_end.saturating_sub(area.height as u32)};
        }
    }
    top = top.min(total.saturating_sub(area.height as u32));
    // Scrolled (wheel, PgUp/PgDn while typing) back down to the end of a following list: follow it again.
    if st.manual_scroll && v["follow"] == true && top >= total.saturating_sub(area.height as u32) && st.top as u32 >= top {
        st.manual_scroll = false;
        st.follow_end = None;
    }
    let mut area=area;
    if indicator && top>0 && area.height>2 {
        let header_at=|top:u32| {let mut offset=0;let mut header=None;for (r,h) in list.iter().zip(&heights){if offset>top{break;}if kind(&r.item)=="header"{header=r.item["text"].as_str();}offset+=*h;}header};
        if header_at(top).is_some() {
            let header_y=area.y;area.y+=1;area.height-=1;
            if st.sel>=pinned && st.sel<first_bottom && !st.manual_scroll {let s=st.sel-pinned;let start: u32=heights[..s].iter().sum();let end=start+heights[s];if end>top+area.height as u32{top=if heights[s]>area.height as u32{start}else{end.saturating_sub(area.height as u32)};}}
            if let Some(header)=header_at(top){buf.set_string(area.x,header_y,truncate(header,area.width as usize),pal.style(Some("accent")).add_modifier(Modifier::BOLD));}
        }
    }
    st.top = top as usize;
    let scrollbar = total > area.height as u32;
    let body = Rect { width: area.width - scrollbar as u16, ..area };

    let mut line: u32 = 0;
    for (j, r) in list.iter().enumerate() {
        let i = j + pinned;
        let h = heights[j];
        if line + h <= top {
            line += h;
            continue;
        }
        let skip=top.saturating_sub(line);
        let y = body.y + line.saturating_sub(top).min(body.height as u32) as u16;
        line += h;
        if y >= body.bottom() {
            break;
        }
        draw_row(buf, Rect { x: body.x, y, width: body.width, height: (h-skip).min((body.bottom() - y) as u32) as u16 }, r, focused && i == st.sel, st.chip, i, skip.min(u16::MAX as u32) as u16, hits, pal, icons, composer_of(st, r, focused), unfolded(r, st));
    }

    // A following list (a conversation) scrolled away from its newest rows says how to get back.
    if v["follow"] == true && top + u32::from(body.height) < total && body.height > 1 {
        let hint = "↓ Latest · End";
        let w = icons::width(hint);
        if body.width > w + 2 {
            let r = Rect { x: body.right() - w - 1, y: body.bottom() - 1, width: w, height: 1 };
            buf.set_string(r.x, r.y, hint, pal.badge(Some("accent")));
            hits.latest = Some(r);
        }
    }
    if scrollbar {
        let x = area.right() - 1;
        let thumb = ((area.height as u64 * area.height as u64) / total as u64).max(1) as u16;
        let pos = ((top as u64 * area.height as u64) / total as u64) as u16;
        for dy in 0..area.height {
            let (sym, style) = if dy >= pos && dy < pos + thumb { ("┃", Style::new().fg(pal.accent)) } else { ("│", pal.border(false)) };
            buf.set_string(x, area.y + dy, sym, style);
        }
    }
    let visible=hits.rows.iter().filter(|(_,i)|rows.get(*i).is_some_and(|r|selectable(&r.item))).map(|(_,i)|*i).collect::<Vec<_>>();
    st.page_step=visible.len().max(1);
    if let (Some(y),Some(first),Some(last))=(footer,visible.first(),visible.last()) {
        let n=rows.iter().filter(|r|selectable(&r.item)).count();
        let total=prepared.total;
        let start=rows[..*first].iter().filter(|r|selectable(&r.item)).count()+1;let end=rows[..=*last].iter().filter(|r|selectable(&r.item)).count();
        buf.set_string(area.x,y," ".repeat(area.width as usize),pal.style(Some("dim")));
        let extra=hint_keys(v,st);
        buf.set_string(area.x,y,truncate(&super::keys::display_labels(&format!("{n} of {total} · {start}-{end} · PgUp/PgDn{extra} · Tab"),key_labels),area.width as usize),pal.style(Some("dim")));
    }
}

/// Footer hints for the keys a pane's declarations add: `f` cycles Show, `s` cycles Sort.
fn hint_keys(v: &Value, st: &State) -> String {
    let mut s = String::new();
    if declared(v, st, "facets").is_some_and(|f| !f.is_empty()) { s.push_str(" · f show"); }
    if declared(v, st, "sorts").is_some_and(|f| !f.is_empty()) { s.push_str(" · s sort"); }
    s
}

/// The `Show: All n · value n …   Sort: label ▾` line (features `facets`, `sort`) as chips laid out in `width`,
/// wrapping onto further lines: (dx, dy, text, style, what) where `what` is a facet (key, value) or the sort chip.
type LineChip = (u16, u16, String, Style, Option<(String, Option<String>)>, bool);
fn facet_chips(v: &Value, st: &State, width: u16, pal: &Palette) -> Vec<LineChip> {
    let counts = facet_counts(v, st, SUPPORTED);
    let sort = sort_label(v, st);
    let mut out: Vec<LineChip> = Vec::new();
    let (mut x, mut y) = (0u16, 0u16);
    let mut put = |text: String, style: Style, what: Option<(String, Option<String>)>, sort: bool, keep_with_next: bool, out: &mut Vec<LineChip>| {
        let w = icons::width(&text);
        if x > 0 && x + w > width && !keep_with_next { x = 0; y += 1; }
        out.push((x, y, text, style, what, sort));
        x += w;
    };
    for c in &counts {
        put(format!("{}: ", c.label), pal.style(Some("muted")), None, false, false, &mut out);
        let mut chips = vec![(None, "All".to_string(), c.all)];
        chips.extend(c.values.iter().map(|(v, l, n)| (Some(v.clone()), l.clone(), *n)));
        for (n, (value, label, count)) in chips.into_iter().enumerate() {
            let on = value == c.chosen;
            let style = if on { pal.badge(Some("accent")) } else if count == 0 { pal.style(Some("dim")) } else { pal.style(None) };
            if n > 0 { put("·".into(), pal.style(Some("dim")), None, false, false, &mut out); }
            put(format!(" {label} {count} "), style, Some((c.key.clone(), value)), false, false, &mut out);
        }
        put("  ".into(), Style::new(), None, false, true, &mut out);
    }
    if let Some(label) = sort {
        put("Sort: ".into(), pal.style(Some("muted")), None, false, false, &mut out);
        put(format!(" {label} ▾ "), Style::new().fg(pal.accent).bg(pal.surface), None, true, true, &mut out);
    }
    out
}

/// Lines a list spends above its rows on the facet/sort line, a column header and the rule below them.
fn chrome_lines(v: &Value, st: &State, width: u16, pal: &Palette) -> u16 {
    let facet = facet_chips(v, st, width, pal).last().map(|c| (c.1 + 1).min(3)).unwrap_or(0);
    let header = declared(v, st, "columns").is_some_and(|c| c.iter().any(|x| x["label"].is_string())) as u16;
    facet + header + (facet + header > 0) as u16
}

/// Draw the facet/sort line(s) from the top of `area` (at most three lines). Returns the lines used.
fn facet_line(buf: &mut Buffer, area: Rect, v: &Value, st: &State, pal: &Palette, hits: &mut Hits) -> u16 {
    let chips = facet_chips(v, st, area.width, pal);
    let mut used = 0;
    for (dx, dy, text, style, what, sort) in chips {
        if dy >= 3 || area.y + dy >= area.bottom() { break; }
        let x = area.x + dx;
        let text = truncate(&text, area.right().saturating_sub(x) as usize);
        buf.set_string(x, area.y + dy, &text, style);
        let r = Rect { x, y: area.y + dy, width: icons::width(&text), height: 1 };
        if let Some((key, value)) = what { hits.facets.push((r, key, value)); }
        if sort { hits.sort = Some(r); }
        used = used.max(dy + 1);
    }
    used
}

/// Where columns start and how wide they are, for one pane (feature `columns`).
struct ColumnLayout { start: u16, widths: Vec<usize>, aligns: Vec<bool>, header: Option<String> }

/// Cells before a row's text: indent, expander, checkbox and icon, as `draw_row` lays them out.
fn prefix_width(r: &Flat, icons: &Icons) -> u16 {
    let mut x = (r.depth * 2) as u16 + 2 * (r.has_children || r.depth > 0) as u16;
    if kind(&r.item) == "toggle" { x += 4; } else if r.depth == 0 && !r.has_children { x += 1; }
    if let Some(icon) = icons.resolve(&r.item["icon"]) { let icon: String = icon.chars().take(2).collect(); x += icons.cell(&icon).1 + 1; }
    x
}

/// Cells after a row's text that every row's columns leave room for: its right-aligned detail and first badge
/// (further badges, rarer, take room from that row's own cells).
fn suffix_width(item: &Value) -> u16 {
    let detail = item["detail"].as_str().map(|d| icons::width(d) + 1).unwrap_or(0);
    let badge = item["badges"].get(0).and_then(|b| b["text"].as_str()).map(|t| icons::width(t) + 3).unwrap_or(0);
    let actions = item["actions"].as_array().into_iter().flatten()
        .filter(|a| a["command"].is_string()).filter_map(|a| a["text"].as_str())
        .fold(0u16, |n, t| n.saturating_add(icons::width(t).saturating_add(3)));
    detail.saturating_add(badge).saturating_add(actions).saturating_add(1)
}

fn column_layout(spec: &[Value], rows: &[Flat], width: u16, icons: &Icons) -> ColumnLayout {
    let data: Vec<&Flat> = rows.iter().filter(|r| r.item["columns"].is_array()).collect();
    let start = data.iter().map(|r| prefix_width(r, icons)).max().unwrap_or(1);
    let reserve = data.iter().map(|r| suffix_width(&r.item)).max().unwrap_or(1);
    let owned: Vec<Flat> = data.iter().map(|r| (*r).clone()).collect();
    let widths = column_widths(spec, &owned, width.saturating_sub(start + reserve) as usize);
    let aligns = spec.iter().map(|c| c["align"] == "right").collect::<Vec<_>>();
    let labelled = spec.iter().any(|c| c["label"].is_string());
    let header = labelled.then(|| join_cells(spec.iter().map(|c| c["label"].as_str().unwrap_or("").to_string()).collect(), &widths, &aligns));
    ColumnLayout { start, widths, aligns, header }
}

/// A group's `child_table` lays out only its immediate children. Descendants and
/// neighbouring conversation rows keep their own layout; every row retains its id/hit index.
fn child_tables(rows: &[Flat], width: u16, icons: &Icons) -> Vec<Flat> {
    let mut out = rows.to_vec();
    let mut by_parent = std::collections::HashMap::<usize, Vec<usize>>::new();
    for (i, row) in rows.iter().enumerate().filter(|(_, r)| r.item["columns"].is_array()) {
        if let Some(parent) = row.parent { by_parent.entry(parent).or_default().push(i); }
    }
    for (parent, group) in rows.iter().enumerate() {
        let table = &group.item["child_table"];
        let Some(spec) = table["columns"].as_array().filter(|s| !s.is_empty() && s.iter().all(Value::is_object)) else { continue };
        let width = table["max_width"].as_u64().filter(|w| *w > 0)
            .map(|w| w.min(u16::MAX as u64) as u16).unwrap_or(width).min(width);
        let Some(indices) = by_parent.get(&parent) else { continue };
        let children: Vec<Flat> = indices.iter().map(|&i| rows[i].clone()).collect();
        let layout = column_layout(spec, &children, width, icons);
        for &i in indices {
            out[i] = layout.apply(&rows[i], icons);
            out[i].item["_child_table_width"] = Value::from(width);
        }
    }
    out
}

fn join_cells(cells: Vec<String>, widths: &[usize], aligns: &[bool]) -> String {
    let mut out = String::new();
    for (i, w) in widths.iter().enumerate() {
        if *w == 0 { continue; }  // dropped for width (`min_width`/`priority`)
        let cell = cells.get(i).map(String::as_str).unwrap_or("");
        let cut = truncate(cell, *w);
        let pad = w.saturating_sub(icons::width(&cut) as usize);
        if !out.is_empty() { out.push(' '); }
        if aligns.get(i) == Some(&true) { out.push_str(&" ".repeat(pad)); out.push_str(&cut); } else { out.push_str(&cut); out.push_str(&" ".repeat(pad)); }
    }
    out.trim_end().to_string()
}

impl ColumnLayout {
    /// A row with `columns` drawn as aligned cells: its text becomes the padded cells, starting at `start`.
    fn apply(&self, r: &Flat, icons: &Icons) -> Flat {
        let Some(cells) = r.item["columns"].as_array() else { return r.clone() };
        let cells: Vec<String> = cells.iter().map(|c| c.as_str().unwrap_or("").to_string()).collect();
        let lead = " ".repeat(self.start.saturating_sub(prefix_width(r, icons)) as usize);
        let mut out = r.clone();
        out.item["text"] = Value::String(format!("{lead}{}", join_cells(cells, &self.widths, &self.aligns)));
        out
    }
}

/// A composer row's state and whether it is being typed into (in a focused panel).
fn composer_of<'a>(st: &'a State, r: &Flat, focused: bool) -> Option<(&'a crate::ui_model::composer::Composer, bool)> {
    if kind(&r.item) != "composer" { return None; }
    st.composers.get(&r.id).map(|c| (c, focused && st.editing.as_deref() == Some(r.id.as_str())))
}

/// A composer: the draft (or placeholder) with a `▌` margin and the cursor, then controls, Send and a key hint.
#[allow(clippy::too_many_arguments)]
fn draw_composer(buf: &mut Buffer, row: Rect, item: &Value, selected: bool, chip: usize, i: usize, hits: &mut Hits, pal: &Palette, icons: &Icons, comp: Option<(&crate::ui_model::composer::Composer, bool)>) {
    use crate::ui_model::composer;
    let fresh;
    let (c, editing) = match comp { Some(c) => c, None => { fresh = composer::Composer::new(item); (&fresh, false) } };
    let text_rows = row.height.saturating_sub(1).max(1);
    let (lines, (cl, cc)) = c.editor.lines();
    let first = cl.saturating_sub(text_rows as usize - 1);
    let margin = if editing { Style::new().fg(pal.prompt) } else { pal.border(selected) };
    let empty = c.editor.text.is_empty();
    for dy in 0..text_rows {
        let y = row.y + dy;
        buf.set_string(row.x, y, "▌", margin);
        if empty && dy == 0 {
            buf.set_string(row.x + 1, y, truncate(item["placeholder"].as_str().unwrap_or("Message"), row.width.saturating_sub(2) as usize), pal.style(Some("dim")));
        } else if let Some(line) = lines.get(first + dy as usize) {
            buf.set_string(row.x + 1, y, truncate(line, row.width.saturating_sub(2) as usize), Style::new().fg(pal.text));
        }
        if editing && first + dy as usize == cl {
            let before: String = lines.get(cl).map(|l| l.chars().take(cc).collect()).unwrap_or_default();
            let x = row.x + 1 + icons::width(&before).min(row.width.saturating_sub(2));
            // On an empty draft the cursor sits on the placeholder's first letter, which stays readable.
            let under = if empty { item["placeholder"].as_str().unwrap_or("Message").chars().next().map(String::from) } else { lines.get(cl).and_then(|l| l.chars().nth(cc)).map(String::from) };
            buf.set_string(x, y, under.unwrap_or(" ".into()), Style::new().add_modifier(Modifier::REVERSED));
        }
    }
    hits.rows.push((Rect { height: text_rows, ..row }, i));
    // Control line: each control's choices, then Send; the hint fills what is left.
    let y = row.y + text_rows;
    if y >= row.bottom() { return; }
    let mut x = row.x + 1;
    let controls = composer::controls(item);
    let mut b = 0usize;
    let mut button = |label: String, style: Style, x: &mut u16, buf: &mut Buffer, hits: &mut Hits| {
        let on = selected && !editing && chip == b + 1;
        let style = if on { style.add_modifier(Modifier::REVERSED | Modifier::BOLD) } else { style };
        let w = icons::width(&label).min(row.right().saturating_sub(*x));
        if w > 0 { buf.set_string(*x, y, truncate(&label, w as usize), style); hits.chips.push((Rect { x: *x, y, width: w, height: 1 }, i, b + 1)); }
        *x = x.saturating_add(w + 1);
        b += 1;
    };
    for ctl in &controls {
        let arg = ctl["arg"].as_str().unwrap_or("");
        let value = c.values.get(arg).cloned().unwrap_or(Value::Null);
        if ctl["type"] == "button" {
            // A command button (feature `composer_buttons`): its icon (from the icon set) and text.
            let label = icons.label(&ctl["icon"], ctl["text"].as_str().unwrap_or(""));
            button(format!("[{}]", if label.is_empty() { "?".into() } else { label }), pal.style(Some(ctl["style"].as_str().unwrap_or("accent"))), &mut x, buf, hits);
        } else if ctl["type"] == "chips" {
            if let Some(label) = ctl["text"].as_str() { let l = format!("{label}:"); buf.set_string(x, y, &l, pal.style(Some("muted"))); x += icons::width(&l) + 1; }
            for chipv in ctl["chips"].as_array().into_iter().flatten() {
                let on = chipv["value"] == value;
                let label = format!("[{}{}]", if on { "● " } else { "" }, chipv["text"].as_str().unwrap_or(""));
                button(label, if on { pal.style(Some("accent")) } else { pal.style(Some("muted")) }, &mut x, buf, hits);
            }
        } else {
            let shown = ctl["options"].as_array().and_then(|o| o.iter().find(|x| x["value"] == value)).and_then(|x| x["label"].as_str()).map(String::from).or_else(|| value.as_str().map(String::from)).unwrap_or_default();
            button(format!("{}: {shown} ▾", ctl["text"].as_str().unwrap_or(arg)), pal.style(Some("accent")), &mut x, buf, hits);
        }
    }
    if item["send_button"] != false {
        button("[Send]".into(), pal.style(Some("accent")).add_modifier(Modifier::BOLD), &mut x, buf, hits);
    }
    let n = lines.len();
    // The new-line key that works here: Shift-Enter when the terminal reports it (keyboard enhancement), else Alt-Enter
    // (Ctrl-J works everywhere).
    let shift = super::keys::shift_enter_reported();
    let send = match (item["submit"] == "C-Enter", shift) {
        (true, _) => "Ctrl-Enter send · Enter new line",
        (false, true) => "Enter send · Shift-Enter new line",
        (false, false) => "Enter send · Alt-Enter new line",
    };
    let hint = format!("{n} line{} · {send}", if n == 1 { "" } else { "s" });
    let hw = icons::width(&hint);
    if x + hw < row.right() { buf.set_string(row.right() - hw, y, &hint, pal.style(Some("dim"))); }
}

/// A text row with `fold`, or a `message` block: a heavy frame (accent) with the title, the body (markdown or
/// wrapped), and the fold line, which toggles on a click.
#[allow(clippy::too_many_arguments)]
fn draw_block(buf: &mut Buffer, row: Rect, r: &Flat, selected: bool, i: usize, skip: u16, hits: &mut Hits, pal: &Palette, open: bool) {
    let item = &r.item;
    let indent = (r.depth * 2) as u16;
    let message = kind(item) == "message";
    // A card (feature `card`) fills the row's width from its indent with the palette's `card` and keeps one
    // column clear on each side; its frame, if kept, is drawn inside.
    let (card, framed) = message_look(item, SUPPORTED);
    let pad = if card { 1 } else { 0 };
    let x0 = row.x + indent + pad;
    let w = row.width.saturating_sub(indent + 2 * pad);
    let right = x0 + w;
    let frame = if item["style"].is_string() { text_style(item, pal) } else { pal.style(Some("accent")) };
    let base = if message || item["format"] == "markdown" { Style::new().fg(pal.text) } else if item["style"].is_string() { text_style(item, pal) } else { pal.style(Some("muted")) };
    if selectable(item) { hits.rows.push((row, i)); }
    for (dy, (line, part)) in block_lines(item, row.width, r.depth, open).into_iter().skip(skip as usize).take(row.height as usize).enumerate() {
        let y = row.y + dy as u16;
        let mut x = x0;
        if card { buf.set_style(Rect { x: row.x + indent, y, width: row.width.saturating_sub(indent), height: 1 }, Style::new().bg(pal.card)); }
        let pre = |s: &str, x: &mut u16, buf: &mut Buffer| { buf.set_string(*x, y, s, frame); *x += icons::width(s); };
        match part {
            "pad" => continue,
            "title" => {
                // An unframed card's sender line: bold, in the row's style (plain text when it has none).
                let title = line.first().map(|s| s.0.as_str()).unwrap_or("");
                let own = if item["style"].is_string() { text_style(item, pal) } else { Style::new().fg(pal.text) };
                let style = if selected { pal.selected() } else { own.add_modifier(Modifier::BOLD) };
                buf.set_string(x, y, truncate(title, w as usize), style);
                continue;
            }
            "top" => {
                let title = line.first().map(|s| s.0.as_str()).unwrap_or("");
                let t = truncate(&format!("┏━ {title} "), w as usize);
                let style = if selected { pal.selected() } else { frame.add_modifier(Modifier::BOLD) };
                buf.set_string(x, y, &t, style);
                let used = icons::width(&t);
                if used < w { buf.set_string(x + used, y, "━".repeat((w - used) as usize), frame); }
                continue;
            }
            "bottom" => { buf.set_string(x, y, format!("┗{}", "━".repeat(w.saturating_sub(1) as usize)), frame); continue; }
            "side" => pre("┃ ", &mut x, buf),
            "fold" => {
                if message && framed { pre("┃ ", &mut x, buf); }
                let label = line.first().map(|s| s.0.as_str()).unwrap_or("");
                buf.set_string(x, y, truncate(label, right.saturating_sub(x) as usize), pal.style(Some("accent")));
                hits.expanders.push((Rect { x, y, width: icons::width(label).min(right.saturating_sub(x)), height: 1 }, i));
                continue;
            }
            _ => {}
        }
        for (t, st) in line {
            let style = match st { "h" => base.patch(pal.style(Some("accent"))).add_modifier(Modifier::BOLD), "b" => base.add_modifier(Modifier::BOLD), "c" | "pre" => base.patch(pal.style(Some("muted"))), _ => base };
            let style = if selected && !message { style.patch(pal.selected()) } else { style };
            let room = right.saturating_sub(x) as usize;
            if room == 0 { break; }
            buf.set_string(x, y, truncate(&t, room), style);
            x = x.saturating_add(icons::width(&t));
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_row(buf: &mut Buffer, row: Rect, r: &Flat, selected: bool, chip: usize, i: usize, skip:u16, hits: &mut Hits, pal: &Palette, icons: &Icons, comp: Option<(&crate::ui_model::composer::Composer, bool)>, open: bool) {
    if kind(&r.item) == "composer" { return draw_composer(buf, row, &r.item, selected, chip, i, hits, pal, icons, comp); }
    if kind(&r.item) == "message" || fold(&r.item).is_some() { return draw_block(buf, row, r, selected, i, skip, hits, pal, open); }
    let item = &r.item;
    let row = Rect { width: item["_child_table_width"].as_u64().map(|w| (w.min(u16::MAX as u64) as u16).min(row.width)).unwrap_or(row.width), ..row };
    let w = row.width as usize;
    let y = row.y;
    let text = item["text"].as_str().unwrap_or("");
    let indent = (r.depth * 2) as u16;
    if skip>0 && !matches!(kind(item),"text"|"chips") {
        if let Some(description)=item["description"].as_str(){buf.set_string(row.x,y,truncate(description,w),pal.style(Some("dim")));hits.rows.push((row,i));}return;
    }
    match kind(item) {
        "chips" => {
            let chips = item["chips"].as_array().cloned().unwrap_or_default();
            for (ci, ((dx, dy, cw, short), c)) in chip_layout(item, row.width, icons).into_iter().zip(chips.iter()).enumerate() {
                if dy<skip {continue;}
                let dy=dy-skip;
                if dy >= row.height {break;}
                let style = match c["checked"].as_bool() {
                    Some(true) => pal.badge(Some("accent")),
                    Some(false) => Style::new().bg(pal.badge).fg(pal.muted),
                    None => Style::new().fg(pal.accent).bg(pal.surface),
                };
                let style = if let Some(n) = c["style"].as_str() { style.patch(pal.style(Some(n))) } else { style };
                let style = if c["disabled"]==true {style.patch(pal.style(Some("dim")))}else if selected && ci == chip { style.add_modifier(Modifier::REVERSED | Modifier::BOLD) } else { style };
                let rect = Rect { x: row.x + dx, y: y + dy, width: cw, height: 1 };
                buf.set_string(rect.x, rect.y, truncate(&chip_label(c, icons, short), cw as usize), style);
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
        "text" if item["format"]=="markdown" => {
            // Formatted text (feature markdown): the shared layout, styled through the palette.
            let base=if selected {pal.selected()}else if item["style"].is_string(){text_style(item,pal)}else{Style::new().fg(pal.text)};
            if selectable(item){hits.rows.push((row,i));}
            let lines=crate::ui_model::markdown::lines(text,row.width.saturating_sub(indent) as usize);
            for (dy,line) in lines.iter().skip(skip as usize).take(row.height as usize).enumerate(){
                let mut x=row.x+indent;
                for (t,st) in line {
                    let style=match *st {"h"=>base.patch(pal.style(Some("accent"))).add_modifier(Modifier::BOLD),"b"=>base.add_modifier(Modifier::BOLD),"c"|"pre"=>base.patch(pal.style(Some("muted"))),_=>base};
                    let style=if selected {style.patch(pal.selected())}else{style};
                    let room=(row.right().saturating_sub(x)) as usize;
                    if room==0 {break;}
                    buf.set_string(x,y+dy as u16,truncate(t,room),style);
                    x=x.saturating_add(icons::width(t));
                }
            }
        },
        "text" => {
            let style=if selected {pal.selected()}else if item["style"].is_string(){text_style(item,pal)}else{pal.style(Some("muted"))};
            if selectable(item){hits.rows.push((row,i));}
            let lines=if item["wrap"]==true {wrap_text(text,row.width.saturating_sub(indent))}else{vec![text.into()]};
            for (dy,text) in lines.iter().skip(skip as usize).take(row.height as usize).enumerate(){buf.set_string(row.x+indent,y+dy as u16,truncate(text,w.saturating_sub(indent as usize)),style);}
        },
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
                hits.toggles.push((Rect{x,y,width:4.min(row.right().saturating_sub(x)),height:1},i));
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
            // Right side: inline buttons (`actions`, feature actions), then detail, then badges before it.
            let mut right = row.right().saturating_sub(1);
            for (ai, a) in item["actions"].as_array().into_iter().flatten().enumerate().collect::<Vec<_>>().into_iter().rev() {
                let Some(t) = a["text"].as_str().filter(|_| a["command"].is_string()) else { continue };
                let label = format!("[{t}]");
                let aw = icons::width(&label);
                if right < x + aw + 12 { break; }
                right -= aw;
                buf.set_string(right, y, &label, ink(pal.style(Some("accent"))));
                hits.actions.push((Rect { x: right, y, width: aw, height: 1 }, i, ai));
                right = right.saturating_sub(1);
            }
            let detail = item["detail"].as_str().unwrap_or("");
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Draw a panel and return its lines, skipping the cell a wide glyph covers.
    fn draw(v: &Value, icons: &Icons, width: u16) -> Vec<String> {
        let area = Rect::new(0, 0, width, 6);
        let mut buf = Buffer::empty(area);
        render(&mut buf, area, &Arc::new(v.clone()), &mut State::default(), false, &Palette::default(), icons, None);
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

    #[test]
    fn child_tables_are_local_bounded_and_keep_actions_with_their_rows() {
        let v = json!({"requires":["child_table"], "items":[
            {"id":"g", "text":"Group", "expanded":true,
             "child_table":{"columns":[{"width":8},{"width":"fill","min_width":8},{"width":6,"priority":1}],"max_width":60},
             "children":[
                {"id":"a","text":"fallback a","columns":["read","src/a.rs","20 B"],"copy":"a","actions":[{"text":"copy","command":"client.copy"}],
                 "expanded":true,"children":[{"text":"unmodified output"}]},
                {"id":"b","text":"fallback b","columns":["bash","pwd","3 B"],"copy":"b","actions":[{"text":"copy","command":"client.copy"}]}]},
            {"text":"neighbour"}]});
        let icons = Icons::default();
        let wide = draw(&v, &icons, 180);
        assert!(wide.iter().any(|l| l.contains("unmodified output")));
        assert!(wide.iter().any(|l| l.contains("neighbour")));
        let a = wide.iter().find(|l| l.contains("src/a.rs")).unwrap();
        let b = wide.iter().find(|l| l.contains("pwd")).unwrap();
        let column = |line: &str, text: &str| icons::width(&line[..line.find(text).unwrap()]);
        assert_eq!(column(a, "src/a.rs"), column(b, "pwd"));
        assert_eq!(column(a, "[copy]"), column(b, "[copy]"));
        assert!(a.find("[copy]").unwrap() < 60);
        for width in [18, 35, 60] {
            let screen = draw(&v, &icons, width);
            assert!(screen.iter().any(|l| l.contains("read")), "{width}: {screen:?}");
        }
        let area = Rect::new(0,0,180,10);
        let mut buf = Buffer::empty(area);
        let mut state = State::default();
        let hits = render(&mut buf, area, &Arc::new(v), &mut state, true, &Palette::default(), &icons, None);
        assert_eq!(hits.actions.len(), 2);
        assert!(hits.actions.iter().all(|(rect, _, _)| rect.right() <= 60));
    }

    #[test]
    fn malformed_child_table_schema_preserves_readable_fallback() {
        for schema in [json!([null]), json!([17]), json!([{"width": 8}, false]), json!([]), json!("columns")] {
            let v = json!({"items":[{"text":"Group", "expanded":true,
                "child_table":{"columns":schema,"max_width":40},
                "children":[{"text":"readable fallback includes every cell", "columns":["read","target","done"]}]}]});
            let screen = draw(&v, &Icons::default(), 100);
            assert!(screen.iter().any(|l| l.contains("readable fallback includes every cell")), "{v}: {screen:?}");
        }
    }

    #[test]
    fn facet_line_sort_chip_and_columns_are_drawn_and_hit() {
        let v=Arc::new(json!({"facets":[{"key":"kind","label":"Show"}],"sorts":[{"key":"name","label":"name"}],
            "columns":[{"label":"kind","width":8},{"label":"name","width":"fill"}],
            "items":[{"text":"command · agents.abort","columns":["command","agents.abort"],"facets":{"kind":"command"},"sort":{"name":"abort"},"badges":[{"text":"ok"}]},
                     {"text":"view · agents-*","columns":["view","agents-*"],"facets":{"kind":"view"},"sort":{"name":"agents"}}]}));
        let area=Rect::new(0,0,60,8);let mut buf=Buffer::empty(area);let mut st=State::default();
        let hits=render(&mut buf,area,&v,&mut st,true,&Palette::default(),&Icons::new(None,false),None);
        let line=|y:u16|(0..60).map(|x|buf[(x,y)].symbol().to_string()).collect::<String>().trim_end().to_string();
        assert!(line(0).starts_with("Show:  All 2 · command 1 · view 1"),"{}",line(0));
        assert!(line(0).contains("Sort:  given order ▾"),"{}",line(0));
        assert_eq!(hits.facets.len(),3);assert!(hits.sort.is_some());
        assert!(line(1).trim_start().starts_with("kind     name"),"header: {}",line(1));
        let (a,b)=(line(3),line(4));
        assert_eq!(a.find("agents.abort"),b.find("agents-*"),"columns align: {a} / {b}");
        assert!(a.ends_with("ok"),"badges stay at the right: {a}");
        // Without the features: plain text rows, no line.
        let plain=Arc::new(json!({"items":[{"text":"command · agents.abort","columns":["command","agents.abort"]}]}));
        let mut buf=Buffer::empty(area);render(&mut buf,area,&plain,&mut State::default(),true,&Palette::default(),&Icons::new(None,false),None);
        assert!((0..60).map(|x|buf[(x,0)].symbol().to_string()).collect::<String>().contains("command · agents.abort"));
    }

    #[test]
    fn narrow_panes_drop_low_priority_columns_before_squeezing_the_title() {
        let v=Arc::new(json!({"columns":[{"label":"extension","width":"fill","min_width":16},{"label":"id","priority":1},{"label":"kind","priority":2}],
            "items":[{"text":"Finished resource retention","columns":["Finished resource retention","retention","management"]}]}));
        let draw=|w:u16|{let area=Rect::new(0,0,w,4);let mut buf=Buffer::empty(area);render(&mut buf,area,&v,&mut State::default(),true,&Palette::default(),&Icons::new(None,false),None);
            (0..4).map(|y|(0..w).map(|x|buf[(x,y)].symbol().to_string()).collect::<String>()).collect::<Vec<_>>().join("\n")};
        let wide=draw(60);assert!(wide.contains("retention")&&wide.contains("management"),"{wide}");
        let narrow=draw(34);assert!(!narrow.contains(" retention ")&&narrow.contains("management")&&narrow.contains("Finished resource"),"{narrow}");
        let tight=draw(24);assert!(!tight.contains("management")&&tight.contains("Finished resource re"),"{tight}");
    }

    #[test]
    fn list_detail_is_responsive_filterable_and_has_an_ordinary_list_fallback() {
        let value=Arc::new(json!({"list_detail":{"requires":["list_detail"]},"items":[{"type":"header","text":"Tools","expanded":true,"children":[
            {"text":"One","summary":"alpha","tags":["search-tag"],"detail_panel":{"title":"One detail","items":[{"type":"text","wrap":true,"focusable":true,"text":"This paragraph remains readable in a narrow panel."}]}},
            {"text":"Two","detail_panel":{"title":"Two detail","items":[]}}
        ]}]}));
        for width in [120,60] {let area=Rect::new(0,0,width,24);let mut buf=Buffer::empty(area);let mut st=State::default();
            let hits=render(&mut buf,area,&value,&mut st,true,&Palette::default(),&Icons::new(None,false),None);let detail=hits.detail_area.unwrap();
            if width==120{assert!(detail.x>0);assert_eq!(detail.y,0);}else{assert_eq!(detail.x,0);assert!(detail.y>0);}
            assert!(!hits.rows.is_empty());assert!(hits.detail.is_some());assert!(st.page_step>0);
        }
        let mut st=State{filter:"search-tag".into(),..Default::default()};let found=super::rows(&value,&st);assert_eq!(found.len(),2);assert_eq!(found[0].item["text"],"Tools");
        let area=Rect::new(0,0,120,24);let mut buf=Buffer::empty(area);render(&mut buf,area,&value,&mut st,true,&Palette::default(),&Icons::new(None,false),None);
        let text=(0..24).map(|y|(0..120).map(|x|buf[(x,y)].symbol()).collect::<String>()).collect::<Vec<_>>().join("\n");assert!(text.contains("1 of 2"));assert!(text.contains("One detail"));
        let mut plain=(*value).clone();plain.as_object_mut().unwrap().remove("list_detail");assert!(missing(&plain).is_empty());assert_eq!(super::rows(&plain,&State::default()).len(),3);
    }

    #[test]
    fn detail_group_headers_are_semantic_bold_and_remain_readable_when_scrolled() {
        let children=|prefix:&str,n:usize|(0..n).map(|i|json!({"text":format!("{prefix}{i}")})).collect::<Vec<_>>();
        let value=Arc::new(json!({"list_detail":{"requires":["list_detail"]},"items":[{"type":"header","text":"Alpha","expanded":true,"children":children("a",5)},{"type":"header","text":"Beta","expanded":true,"children":children("b",8)}]}));
        let area=Rect::new(0,0,120,8);let mut buffer=Buffer::empty(area);let mut state=State{sel:14,..Default::default()};let palette=Palette::default();
        render(&mut buffer,area,&value,&mut state,true,&palette,&Icons::new(None,false),None);
        assert_eq!(buffer[(0,0)].symbol(),"B");assert_eq!(buffer[(0,0)].fg,palette.accent);assert!(buffer[(0,0)].modifier.contains(Modifier::BOLD));
        let unsupported=json!({"list_detail":{"requires":["future-widget"]},"items":[{"text":"fallback"}]});
        assert!(!list_detail(&unsupported,&State::default()));assert!(missing(&unsupported).is_empty());
    }

    #[test]
    fn markdown_rows_and_bottom_pinned_rows_render() {
        let value=Arc::new(json!({"items":[
            {"type":"text","format":"markdown","text":"## Plan\nRun `x` and **go**\n- one\n- two"},
            {"text":"filler 1"},{"text":"filler 2"},{"text":"filler 3"},{"text":"filler 4"},{"text":"filler 5"},
            {"text":"Messages: 2 waiting","pin":"bottom"}]}));
        let area=Rect::new(0,0,30,10);let mut buf=Buffer::empty(area);let mut st=State::default();let pal=Palette::default();
        render(&mut buf,area,&value,&mut st,false,&pal,&Icons::new(None,false),None);
        let line=|y:u16|(0..29).map(|x|buf[(x,y)].symbol()).collect::<String>().trim_end().to_string();  // column 29: scrollbar
        assert_eq!(line(0),"Plan");assert_eq!(buf[(0,0)].fg,pal.accent);assert!(buf[(0,0)].modifier.contains(Modifier::BOLD));
        assert_eq!(line(2),"Run x and go");assert!(buf[(10,2)].modifier.contains(Modifier::BOLD));
        assert_eq!(line(3),"");assert_eq!(line(4),"• one");assert_eq!(line(5),"• two");
        assert_eq!(line(9).trim(),"Messages: 2 waiting","pinned at the bottom, never scrolled away");
        assert!(line(8).starts_with("──"));
        assert!((0..8).all(|y|!line(y).contains("Messages")));
    }

    #[test]
    fn partial_wrapped_rows_scroll_without_underflow_or_losing_text() {
        let value=Arc::new(json!({"items":[{"type":"text","focusable":true,"wrap":true,"text":"one two three four five six seven eight nine ten"}]}));
        let mut st=State{top:2,manual_scroll:true,..Default::default()};let area=Rect::new(0,0,12,3);let mut buf=Buffer::empty(area);
        render(&mut buf,area,&value,&mut st,true,&Palette::default(),&Icons::new(None,false),None);assert!(st.top>0);assert!(buf[(0,0)].symbol()!=" ");
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
