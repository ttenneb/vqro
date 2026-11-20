//! Panel flattening and navigation, renderer-independent (moved from `tui/panel.rs`, which keeps drawing).
//! The panel catalogue itself is documented in `tui/panel.rs` and `docs/spec/core-protocol.md`; conformance cases
//! for other clients are `tests/fixtures/client/panels.json`.

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::Value;

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
    /// `follow`: the last row when the reader was last at the end (None: not yet shown).
    pub follow_end: Option<usize>,
    /// Composer rows' client-local drafts and control values, by row id (feature `composer`).
    pub composers: HashMap<String, super::composer::Composer>,
    /// The composer row being typed into (keys go to its editor), by row id.
    pub editing: Option<String>,
    pub(crate) prepared: Option<Arc<Prepared>>,
}

/// Only fields that affect flattening belong to this cache key. Selection/scroll/chips do not.
pub(crate) struct Prepared {
    value: Arc<Value>, page: usize, filter: String, open: HashMap<String,bool>, pub(crate) rows: Vec<Flat>, pub(crate) total:usize,
}
impl State {
    /// Flattened rows for this value and state, reused while content and page/filter/expansion are unchanged.
    pub(crate) fn prepare(&mut self, value: &Arc<Value>) -> Arc<Prepared> {
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
    /// A `pin: "bottom"` row (or its child): drawn below the list, never scrolled (feature `pin`).
    pub bottom: bool,
}


/// Panel features this client renders. A panel may list what it needs in `requires`.
pub const SUPPORTED: &[&str] = &["items", "pages", "tree", "badges", "menu", "input", "description", "filter", "toolbar", "chips", "select", "options", "badge_color", "icon", "icon_names", "list_detail", "wrap", "embedded_views", "current_when", "follow", "markdown", "pin", "composer", "multiline"];

/// Features a panel needs that this client lacks.
pub fn missing(v: &Value) -> Vec<String> { missing_for(v, SUPPORTED) }

/// Features a panel needs that a client rendering `features` lacks (bare names and `vqro.` ids are the same).
pub fn missing_for(v: &Value, features: &[&str]) -> Vec<String> {
    v["requires"].as_array().into_iter().flatten().filter_map(|x| x.as_str())
        .filter(|f| !features.iter().any(|have| have == f || f.strip_prefix("vqro.") == Some(have)))
        .map(String::from).collect()
}

/// Panel features every `base` client renders (D29). Others are optional and need a fallback in content.
pub const BASE_FEATURES: &[&str] = &["items", "pages", "tree", "toolbar", "badges", "description", "menu", "input", "options", "select", "chips", "icon", "wrap"];

pub fn kind(item: &Value) -> &str {
    item["type"].as_str().unwrap_or("item")
}

pub fn selectable(item: &Value) -> bool {
    if kind(item) == "composer" { return true; }
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

/// Row types every client renders natively; anything else (including namespaced `x.<owner>.<name>` types) needs
/// its feature claimed, else it is shown through its `fallback` or as a plain item.
pub const ROW_TYPES: &[&str] = &["item", "toggle", "header", "separator", "text", "chips", "select"];

/// A composer row's height: its text lines (`rows`, default 3, 1-12) and the control line.
pub fn composer_height(item: &Value) -> u16 { item["rows"].as_u64().unwrap_or(3).clamp(1, 12) as u16 + 1 }

fn native(item: &Value, features: &[&str]) -> bool {
    let k = kind(item);
    (ROW_TYPES.contains(&k) || features.contains(&k)) && missing_for(item, features).is_empty()
}

/// Element fallback (D29): a row this client can't render natively (unknown type, or unmet scoped `requires`)
/// becomes its `fallback`: a row, a list of rows, or `"drop"`; without one it stays a plain item. Applied before
/// flattening. A fallback row without an `id` inherits the original's (`<id>~<n>` for lists), so selection
/// survives a client upgrade. At most four levels; chips follow the same rule (a chip's fallback is a chip).
pub fn expand(item: &Value, id: &str, features: &[&str]) -> Vec<(Value, String)> {
    fn go(item: &Value, id: &str, features: &[&str], depth: usize, out: &mut Vec<(Value, String)>) {
        let own = item["id"].as_str().map(String::from).unwrap_or_else(|| id.to_string());
        if depth >= 4 || native(item, features) || item.get("fallback").is_none() {
            out.push((chips(item, features), own));
            return;
        }
        match &item["fallback"] {
            Value::String(s) if s == "drop" => {}
            Value::Object(_) => go(&item["fallback"], &own, features, depth + 1, out),
            Value::Array(list) => for (n, fb) in list.iter().enumerate() {
                go(fb, &if list.len() == 1 { own.clone() } else { format!("{own}~{n}") }, features, depth + 1, out);
            },
            _ => out.push((chips(item, features), own)),
        }
    }
    let mut out = Vec::new();
    go(item, id, features, 0, &mut out);
    out
}

fn chips(item: &Value, features: &[&str]) -> Value {
    let Some(list) = item["chips"].as_array().filter(|_| kind(item) == "chips") else { return item.clone() };
    let mut shown = Vec::new();
    for chip in list {
        let mut c = chip.clone();
        for _ in 0..4 {
            if missing_for(&c, features).is_empty() { break; }
            match &c["fallback"] { Value::Object(_) => c = c["fallback"].clone(), Value::String(s) if s == "drop" => { c = Value::Null; break; } _ => break }
        }
        if !c.is_null() { shown.push(c); }
    }
    let mut out = item.clone();
    out["chips"] = Value::Array(shown);
    out
}

/// Rows as this client shows them: each item expanded through its fallback, with positional ids.
fn shown(items: &[Value], prefix: &str, features: &[&str]) -> Vec<(Value, String)> {
    items.iter().enumerate().flat_map(|(i, item)| expand(item, &format!("{prefix}{i}"), features)).collect()
}

fn subtree_matches(item: &Value, id: &str, f: &str, features: &[&str]) -> bool {
    matches(item, f) || shown(item["children"].as_array().map(|c| c.as_slice()).unwrap_or(&[]), &format!("{id}/"), features).iter().any(|(c, cid)| subtree_matches(c, cid, f, features))
}

/// The rows to show, in order, for this client's features.
pub fn rows(v: &Value, st: &State) -> Vec<Flat> { rows_for(v, st, SUPPORTED) }

/// The rows a client rendering `features` shows: toolbar, page, tree expansion, filter and element fallbacks. With
/// feature `pin`, top-level rows with `pin: "bottom"` (and their children) come last, marked `bottom`, never
/// filtered out; clients without it show them where they are.
pub fn rows_for(v: &Value, st: &State, features: &[&str]) -> Vec<Flat> {
    let f = st.filter.to_lowercase();
    let mut out = Vec::new();
    #[allow(clippy::too_many_arguments)]
    fn one(item: Value, id: String, depth: usize, parent: Option<usize>, st: &State, f: &str, features: &[&str], bottom: bool, out: &mut Vec<Flat>) {
        if !f.is_empty() && !subtree_matches(&item, &id, f, features) {
            return;
        }
        let children = item["children"].as_array().cloned().unwrap_or_default();
        let has_children = !children.is_empty();
        // Filtering opens everything that matched below.
        let expanded = has_children && (!f.is_empty() || st.open.get(&id).copied().unwrap_or(item["expanded"] == true));
        let mut row = item.clone();
        if let Some(o) = row.as_object_mut() {
            o.remove("children");
        }
        let me = out.len();
        out.push(Flat { item: row, depth, id: id.clone(), parent, has_children, expanded, pinned: false, bottom });
        if expanded {
            for (child, cid) in shown(&children, &format!("{id}/"), features) {
                one(child, cid, depth + (kind(&item) != "header") as usize, Some(me), st, f, features, bottom, out);
            }
        }
    }
    let toolbar = v["toolbar"].as_array().map(|a| a.as_slice()).unwrap_or(&[]);
    for (n, (item, _)) in shown(toolbar, "toolbar/", features).into_iter().enumerate() {
        out.push(Flat { item, depth: 0, id: format!("toolbar/{n}"), parent: None, has_children: false, expanded: false, pinned: true, bottom: false });
    }
    let pin = features.contains(&"pin");
    let (bottom, list): (Vec<_>, Vec<_>) = shown(page_items(v, st), "", features).into_iter().partition(|(item, _)| pin && item["pin"] == "bottom");
    for (item, id) in list { one(item, id, 0, None, st, &f, features, false, &mut out); }
    for (item, id) in bottom { one(item, id, 0, None, st, "", features, true, &mut out); }
    out
}

/// Ids of every row with children on the current page (for expand/collapse all).
pub fn parent_ids(v: &Value, st: &State) -> Vec<String> {
    let mut out = Vec::new();
    fn walk(items: &[Value], prefix: &str, out: &mut Vec<String>) {
        for (item, id) in shown(items, prefix, SUPPORTED) {
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

/// A panel with `follow: true` (feature `follow`, e.g. a log tail): while the reader is at the last row (or
/// hasn't moved yet), new rows keep the selection on the last row. Moving up or scrolling by hand stops it;
/// returning to the end resumes it. Clients without the feature show the same rows from the top.
pub fn follow(v: &Value, rows: &[Flat], st: &mut State) {
    if v["follow"] != true || st.manual_scroll || rows.is_empty() {
        return;
    }
    let end = last(rows);
    if st.follow_end.is_none_or(|was| st.sel >= was) {
        st.sel = end;
    }
    st.follow_end = Some(end);
}

/// The next (or previous) selectable row after `from` whose item has every field of `matching` (for example
/// `{"role": "user"}`); None when there is none (no wrap). `client.panel {action: next|prev, match}`.
pub fn find_row(rows: &[Flat], from: usize, forward: bool, matching: &Value) -> Option<usize> {
    let ok = |r: &Flat| selectable(&r.item) && matching.as_object().is_none_or(|m| m.iter().all(|(k, v)| r.item[k] == *v));
    if forward { (from + 1..rows.len()).find(|&i| ok(&rows[i])) } else { (0..from.min(rows.len())).rev().find(|&i| ok(&rows[i])) }
}

pub fn first(rows: &[Flat]) -> usize {
    rows.iter().position(|r| selectable(&r.item)).unwrap_or(0)
}

pub fn last(rows: &[Flat]) -> usize {
    rows.iter().rposition(|r| selectable(&r.item)).unwrap_or(0)
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

    #[test]
    fn follow_keeps_the_end_while_the_reader_is_there() {
        let panel = |n: usize| serde_json::json!({"follow": true, "items": (0..n).map(|i| serde_json::json!({"type": "text", "text": format!("line {i}"), "focusable": true})).collect::<Vec<_>>()});
        let mut st = State::default();
        let shown = |v: &Value, st: &mut State| { let r = rows(v, st); follow(v, &r, st); st.sel };
        assert_eq!(shown(&panel(3), &mut st), 2, "starts at the end");
        assert_eq!(shown(&panel(5), &mut st), 4, "new lines keep the end");
        st.sel = 1;
        assert_eq!(shown(&panel(8), &mut st), 1, "reading earlier lines stops following");
        st.sel = 7;
        assert_eq!(shown(&panel(9), &mut st), 8, "back at the end, follows again");
        let mut plain = State::default();
        let still = serde_json::json!({"items": [{"type": "text", "text": "a", "focusable": true}, {"type": "text", "text": "b", "focusable": true}]});
        assert_eq!(shown(&still, &mut plain), 0, "without follow nothing moves");
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
}
