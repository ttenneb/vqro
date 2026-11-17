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
use std::sync::Arc;

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
    /// Optional detail pane has independent selection and scroll, but follows the selected row id.
    pub detail: Option<Box<State>>,
    pub detail_for: String,
    pub detail_focus: bool,
    pub page_step: usize,
    pub manual_scroll: bool,
    pub(super) prepared: Option<Arc<Prepared>>,
}

/// Only fields that affect flattening belong to this cache key. Selection/scroll/chips do not.
pub(super) struct Prepared {
    value: Arc<Value>, page: usize, filter: String, open: HashMap<String,bool>, rows: Vec<Flat>, total:usize,
}
impl State {
    fn prepare(&mut self, value: &Arc<Value>) -> Arc<Prepared> {
        if let Some(cache) = &self.prepared {
            if Arc::ptr_eq(&cache.value,value) && cache.page==self.page && cache.filter==self.filter && cache.open==self.open { return cache.clone(); }
        }
        let shown=rows(value,self);
        let total=if self.filter.is_empty(){shown.iter().filter(|r|selectable(&r.item)).count()}else{let mut unfiltered=self.clone();unfiltered.filter.clear();rows(value,&unfiltered).iter().filter(|r|selectable(&r.item)).count()};
        let prepared=Arc::new(Prepared { value:value.clone(),page:self.page,filter:self.filter.clone(),open:self.open.clone(),rows:shown,total });
        self.prepared=Some(prepared.clone()); prepared
    }
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
    pub detail: Option<Box<Hits>>,
    pub detail_area: Option<Rect>,
    pub toggles: Vec<(Rect,usize)>,
}

/// Panel features this client renders. A panel may list what it needs in `requires`.
pub const SUPPORTED: &[&str] = &["items", "pages", "tree", "badges", "menu", "input", "description", "filter", "toolbar", "chips", "select", "options", "badge_color", "icon", "icon_names", "list_detail", "wrap"];

/// Features a panel needs that this client lacks.
pub fn missing(v: &Value) -> Vec<String> {
    v["requires"].as_array().into_iter().flatten().filter_map(|x| x.as_str()).filter(|f| !SUPPORTED.contains(f)).map(String::from).collect()
}

pub fn kind(item: &Value) -> &str {
    item["type"].as_str().unwrap_or("item")
}

pub fn selectable(item: &Value) -> bool {
    (!matches!(kind(item), "header" | "separator" | "text") || kind(item)=="text" && item["focusable"]==true) && item["disabled"] != true
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
    let hay = format!("{} {} {} {} {}", item["text"].as_str().unwrap_or(""), item["detail"].as_str().unwrap_or(""), item["description"].as_str().unwrap_or(""),item["summary"].as_str().unwrap_or(""),item["tags"].as_array().into_iter().flatten().filter_map(Value::as_str).collect::<Vec<_>>().join(" "));
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
                walk(children, depth + (kind(item)!="header") as usize, Some(me), &format!("{id}/"), st, f, out);
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

fn page_value<'a>(v:&'a Value,st:&State)->&'a Value {v["pages"].as_array().and_then(|p|p.get(st.page.min(p.len().saturating_sub(1)))).unwrap_or(v)}
/// Enhancement requirements live inside list_detail; the ordinary items are the older-client fallback.
pub fn list_detail(v:&Value,st:&State)->bool {let cfg=page_value(v,st).get("list_detail").unwrap_or(&v["list_detail"]);cfg==true || cfg.is_object() && missing(cfg).is_empty()}
pub fn selected_detail(v:&Arc<Value>,st:&State)->Option<(String,Value)> {
    if !list_detail(v,st){return None;}
    let fallback;
    let rows=if let Some(cache)=st.prepared.as_ref().filter(|p|Arc::ptr_eq(&p.value,v)&&p.page==st.page&&p.filter==st.filter&&p.open==st.open){&cache.rows}else{fallback=self::rows(v,st);&fallback};
    let at=if rows.get(st.sel).is_some_and(|r|selectable(&r.item)){st.sel}else{first(rows)};
    let row=rows.get(at)?;row.item["detail_panel"].is_object().then(||(row.id.clone(),row.item["detail_panel"].clone()))
}
pub fn sync_detail(st:&mut State,id:&str) {
    if st.detail_for!=id {st.detail=Some(Box::default());st.detail_for=id.into();}
    if st.detail.is_none(){st.detail=Some(Box::default());}
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
    if kind(item)=="text" && item["wrap"]==true {return wrap_text(item["text"].as_str().unwrap_or(""),width).len().min(u16::MAX as usize) as u16;}
    if kind(item) == "chips" {
        return chip_layout(item, width, icons).last().map(|c| c.1 + 1).unwrap_or(1);
    }
    1 + item["description"].as_str().map(|_| 1).unwrap_or(0)
}

/// Height the content wants at `width` (for sizing popups), including the page bar and toolbar rule.
pub fn height(v: &Value, st: &State, width: u16, icons: &Icons) -> u16 {
    let bar:u16 = if page_titles(v).is_empty() { 0 } else { 2 };
    if list_detail(v,st){return bar+28;}
    let rows = rows(v, st);
    let rule = rows.iter().any(|r| r.pinned) as u16;
    bar.saturating_add(rule).saturating_add(rows.iter().fold(0u16,|sum,r|sum.saturating_add(lines(&r.item,width,icons))))
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
pub fn render(buf: &mut Buffer, area: Rect, v: &Arc<Value>, st: &mut State, focused: bool, pal: &Palette, icons: &Icons) -> Hits {
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

    if list_detail(v,st) {
        let all=st.prepare(v);st.sel=st.sel.min(all.rows.len().saturating_sub(1));
        if !all.rows.get(st.sel).is_some_and(|r|selectable(&r.item)){st.sel=first(&all.rows);}
        let selected=selected_detail(v,st);
        let wide=area.width>=96;
        let (list,detail)=if wide {let w=(area.width*2/5).max(36);(Rect{width:w,..area},Rect{x:area.x+w+1,width:area.width.saturating_sub(w+1),..area})}
            else {let wanted=all.rows.iter().fold(1u16,|h,r|h.saturating_add(lines(&r.item,area.width,icons)));let h=(area.height/2).max(1).min(area.height).min(wanted);(Rect{height:h,..area},Rect{y:area.y+h+1,height:area.height.saturating_sub(h+1),..area})};
        render_list(buf,list,v,st,focused&&!st.detail_focus,pal,icons,&mut hits,true);
        if detail.height>1 {
            if wide {for y in area.y..area.bottom(){buf.set_string(detail.x-1,y,"│",pal.border(false));}}
            else if detail.y>area.y {buf.set_string(area.x,detail.y-1,"─".repeat(area.width as usize),pal.border(false));}
            if let Some((id,value))=selected {
                sync_detail(st,&id);
                let title=value["title"].as_str().unwrap_or("Details");
                buf.set_string(detail.x,detail.y,truncate(title,detail.width as usize),pal.style(Some("accent")).add_modifier(Modifier::BOLD));
                let body=Rect{y:detail.y+1,height:detail.height-1,..detail};
                let mut child=st.detail.take().unwrap_or_default();
                hits.detail=Some(Box::new(render(buf,body,&Arc::new(value),&mut child,focused&&st.detail_focus,pal,icons)));
                st.detail=Some(child);hits.detail_area=Some(detail);
            }else{buf.set_string(detail.x,detail.y,"Select an item for details",pal.style(Some("dim")));}
        }
    } else { render_list(buf,area,v,st,focused,pal,icons,&mut hits,false); }
    hits
}

#[allow(clippy::too_many_arguments)]
fn render_list(buf:&mut Buffer,mut area:Rect,v:&Arc<Value>,st:&mut State,focused:bool,pal:&Palette,icons:&Icons,hits:&mut Hits,indicator:bool) {
    if area.width<2 || area.height==0{return;}
    let prepared = st.prepare(v);
    let rows = &prepared.rows;
    let footer=(indicator && area.height>1).then_some(area.bottom().saturating_sub(1));
    if indicator && area.height>1 {
        let n=rows.iter().filter(|r|selectable(&r.item)).count();
        let total=prepared.total;
        let text=format!("{n} of {total} · PgUp/PgDn · Tab: details");
        buf.set_string(area.x,area.bottom()-1,truncate(&text,area.width as usize),pal.style(Some("dim")));
        area.height-=1;
    }
    if rows.is_empty() {
        let msg = if st.filter.is_empty() { "(empty)" } else { "(no matches)" };
        buf.set_string(area.x, area.y, msg, pal.style(Some("dim")));
        return;
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
        draw_row(buf, Rect { x: area.x, y, width: area.width, height: h }, r, focused && i == st.sel, st.chip, i, 0, hits, pal, icons);
        y += h;
    }
    if pinned > 0 && y < area.bottom() {
        buf.set_string(area.x, y, "─".repeat(area.width as usize), pal.border(false));
        y += 1;
    }
    let area = Rect { y, height: area.bottom().saturating_sub(y), ..area };
    if area.height == 0 {
        return;
    }
    let list = &rows[pinned..];

    // Scroll so the selection is visible, in lines.
    let heights: Vec<u32> = list.iter().map(|r| lines(&r.item, area.width.saturating_sub(1), icons) as u32).collect();
    let total: u32 = heights.iter().sum();
    let mut top = st.top.min(u32::MAX as usize) as u32;
    if st.sel >= pinned && !st.manual_scroll {
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
    let mut area=area;
    if indicator && top>0 && area.height>2 {
        let header_at=|top:u32| {let mut offset=0;let mut header=None;for (r,h) in list.iter().zip(&heights){if offset>top{break;}if kind(&r.item)=="header"{header=r.item["text"].as_str();}offset+=*h;}header};
        if header_at(top).is_some() {
            let header_y=area.y;area.y+=1;area.height-=1;
            if st.sel>=pinned && !st.manual_scroll {let s=st.sel-pinned;let start: u32=heights[..s].iter().sum();let end=start+heights[s];if end>top+area.height as u32{top=if heights[s]>area.height as u32{start}else{end.saturating_sub(area.height as u32)};}}
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
        draw_row(buf, Rect { x: body.x, y, width: body.width, height: (h-skip).min((body.bottom() - y) as u32) as u16 }, r, focused && i == st.sel, st.chip, i, skip.min(u16::MAX as u32) as u16, hits, pal, icons);
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
        buf.set_string(area.x,y,truncate(&format!("{n} of {total} · {start}-{end} · PgUp/PgDn · Tab"),area.width as usize),pal.style(Some("dim")));
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_row(buf: &mut Buffer, row: Rect, r: &Flat, selected: bool, chip: usize, i: usize, skip:u16, hits: &mut Hits, pal: &Palette, icons: &Icons) {
    let item = &r.item;
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
            for (ci, ((dx, dy, cw), c)) in chip_layout(item, row.width, icons).into_iter().zip(chips.iter()).enumerate() {
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
        render(&mut buf, area, &Arc::new(v.clone()), &mut State::default(), false, &Palette::default(), icons);
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
    fn list_detail_is_responsive_filterable_and_has_an_ordinary_list_fallback() {
        let value=Arc::new(json!({"list_detail":{"requires":["list_detail"]},"items":[{"type":"header","text":"Tools","expanded":true,"children":[
            {"text":"One","summary":"alpha","tags":["search-tag"],"detail_panel":{"title":"One detail","items":[{"type":"text","wrap":true,"focusable":true,"text":"This paragraph remains readable in a narrow panel."}]}},
            {"text":"Two","detail_panel":{"title":"Two detail","items":[]}}
        ]}]}));
        for width in [120,60] {let area=Rect::new(0,0,width,24);let mut buf=Buffer::empty(area);let mut st=State::default();
            let hits=render(&mut buf,area,&value,&mut st,true,&Palette::default(),&Icons::new(None,false));let detail=hits.detail_area.unwrap();
            if width==120{assert!(detail.x>0);assert_eq!(detail.y,0);}else{assert_eq!(detail.x,0);assert!(detail.y>0);}
            assert!(!hits.rows.is_empty());assert!(hits.detail.is_some());assert!(st.page_step>0);
        }
        let mut st=State{filter:"search-tag".into(),..Default::default()};let found=super::rows(&value,&st);assert_eq!(found.len(),2);assert_eq!(found[0].item["text"],"Tools");
        let area=Rect::new(0,0,120,24);let mut buf=Buffer::empty(area);render(&mut buf,area,&value,&mut st,true,&Palette::default(),&Icons::new(None,false));
        let text=(0..24).map(|y|(0..120).map(|x|buf[(x,y)].symbol()).collect::<String>()).collect::<Vec<_>>().join("\n");assert!(text.contains("1 of 2"));assert!(text.contains("One detail"));
        let mut plain=(*value).clone();plain.as_object_mut().unwrap().remove("list_detail");assert!(missing(&plain).is_empty());assert_eq!(super::rows(&plain,&State::default()).len(),3);
    }

    #[test]
    fn detail_group_headers_are_semantic_bold_and_remain_readable_when_scrolled() {
        let children=|prefix:&str,n:usize|(0..n).map(|i|json!({"text":format!("{prefix}{i}")})).collect::<Vec<_>>();
        let value=Arc::new(json!({"list_detail":{"requires":["list_detail"]},"items":[{"type":"header","text":"Alpha","expanded":true,"children":children("a",5)},{"type":"header","text":"Beta","expanded":true,"children":children("b",8)}]}));
        let area=Rect::new(0,0,120,8);let mut buffer=Buffer::empty(area);let mut state=State{sel:14,..Default::default()};let palette=Palette::default();
        render(&mut buffer,area,&value,&mut state,true,&palette,&Icons::new(None,false));
        assert_eq!(buffer[(0,0)].symbol(),"B");assert_eq!(buffer[(0,0)].fg,palette.accent);assert!(buffer[(0,0)].modifier.contains(Modifier::BOLD));
        let unsupported=json!({"list_detail":{"requires":["future-widget"]},"items":[{"text":"fallback"}]});
        assert!(!list_detail(&unsupported,&State::default()));assert!(missing(&unsupported).is_empty());
    }

    #[test]
    fn partial_wrapped_rows_scroll_without_underflow_or_losing_text() {
        let value=Arc::new(json!({"items":[{"type":"text","focusable":true,"wrap":true,"text":"one two three four five six seven eight nine ten"}]}));
        let mut st=State{top:2,manual_scroll:true,..Default::default()};let area=Rect::new(0,0,12,3);let mut buf=Buffer::empty(area);
        render(&mut buf,area,&value,&mut st,true,&Palette::default(),&Icons::new(None,false));assert!(st.top>0);assert!(buf[(0,0)].symbol()!=" ");
    }

    #[test]
    fn prepared_rows_reuse_only_when_value_and_shape_are_unchanged() {
        let value=Arc::new(serde_json::json!({"items":[{"id":"p","text":"parent","expanded":true,"children":[{"text":"child"}]}]}));
        let mut state=State::default(); let first=state.prepare(&value);
        assert_eq!(first.rows.len(),2);
        state.sel=1; state.top=3; state.chip=2;
        assert!(Arc::ptr_eq(&first,&state.prepare(&value)),"selection/scroll are not flattening inputs");
        state.open.insert("p".into(),false);
        let folded=state.prepare(&value); assert!(!Arc::ptr_eq(&first,&folded)); assert_eq!(folded.rows.len(),1);
        state.filter="child".into(); let filtered=state.prepare(&value); assert_eq!(filtered.rows.len(),2);
        assert!(!Arc::ptr_eq(&folded,&filtered));
        state.page=1; assert!(!Arc::ptr_eq(&filtered,&state.prepare(&value)));
        let current=state.prepare(&value);
        assert!(!Arc::ptr_eq(&current,&state.prepare(&Arc::new((*value).clone()))),"new content identity invalidates even identical JSON");
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
