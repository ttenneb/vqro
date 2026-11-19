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
}


/// Panel features this client renders. A panel may list what it needs in `requires`.
pub const SUPPORTED: &[&str] = &["items", "pages", "tree", "badges", "menu", "input", "description", "filter", "toolbar", "chips", "select", "options", "badge_color", "icon", "icon_names", "list_detail", "wrap", "embedded_views"];

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
