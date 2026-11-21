//! Panel flattening and navigation, renderer-independent (moved from `tui/panel.rs`, which keeps drawing).
//! The panel catalogue itself is documented in `tui/panel.rs` and `docs/spec/core-protocol.md`; conformance cases
//! for other clients are `tests/fixtures/client/panels.json`.

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use serde_json::Value;

/// Per-view interaction state, owned by the client.
#[derive(Default, Clone)]
pub struct State {
    pub sel: usize,
    /// The page titles when last shown, so the same page stays shown when a page is added or removed before it.
    pub seen_pages: Vec<String>,
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
    /// Search (feature search): the query (empty: none), whether it is being typed, and the match shown.
    pub search: String,
    pub searching: bool,
    pub search_hit: usize,
    /// Chosen facet values, by facet key (feature `facets`); an absent key shows all.
    pub facets: BTreeMap<String, String>,
    /// The chosen entry of the panel's `sorts` (feature `sort`); None is the given order.
    pub sort: Option<usize>,
    pub(crate) prepared: Option<Arc<Prepared>>,
}

/// Only fields that affect flattening belong to this cache key. Selection/scroll/chips do not.
pub(crate) struct Prepared {
    value: Arc<Value>, page: usize, filter: String, open: HashMap<String,bool>, facets: BTreeMap<String,String>, sort: Option<usize>, pub(crate) rows: Vec<Flat>, pub(crate) total:usize,
}
impl State {
    /// Flattened rows for this value and state, reused while content and page/filter/expansion are unchanged.
    /// Keep showing the same page when the panel's pages change (another extension's page appears or goes): find the
    /// page shown last time by its title. Pages the user switches to are remembered as they are drawn.
    pub fn keep_page(&mut self, v: &Value) {
        let titles = page_titles(v);
        if titles != self.seen_pages {
            if let Some(i) = self.seen_pages.get(self.page).and_then(|t| titles.iter().position(|x| x == t)) { self.page = i; }
            self.seen_pages = titles;
        }
    }

    pub(crate) fn prepare(&mut self, value: &Arc<Value>) -> Arc<Prepared> {
        self.keep_page(value);
        if let Some(cache) = &self.prepared {
            if Arc::ptr_eq(&cache.value,value) && cache.page==self.page && cache.filter==self.filter && cache.open==self.open && cache.facets==self.facets && cache.sort==self.sort { return cache.clone(); }
        }
        let shown=rows(value,self);
        let total=if self.filter.is_empty()&&self.facets.is_empty(){shown.iter().filter(|r|selectable(&r.item)).count()}else{let mut unfiltered=self.clone();unfiltered.filter.clear();unfiltered.facets.clear();rows(value,&unfiltered).iter().filter(|r|selectable(&r.item)).count()};
        let prepared=Arc::new(Prepared { value:value.clone(),page:self.page,filter:self.filter.clone(),open:self.open.clone(),facets:self.facets.clone(),sort:self.sort,rows:shown,total });
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
pub const SUPPORTED: &[&str] = &["items", "pages", "tree", "badges", "menu", "input", "description", "filter", "toolbar", "chips", "select", "options", "badge_color", "icon", "icon_names", "list_detail", "wrap", "embedded_views", "current_when", "follow", "markdown", "pin", "composer", "multiline", "search", "facets", "sort", "columns", "message", "fold", "actions", "title_actions", "composer_buttons"];

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
    (!matches!(kind(item), "header" | "separator" | "text") || kind(item)=="text" && (item["focusable"]==true || fold(item).is_some())) && item["disabled"] != true
}

/// Rows that fold (feature `fold`): a text row or `message` block with `fold: N` shows its first N lines and
/// "▸ M more lines" until unfolded (Enter, Space, →, or a click on that line); "▾ less" folds it again.
pub fn fold(item: &Value) -> Option<usize> {
    if !matches!(kind(item), "text" | "message") { return None; }
    item["fold"].as_u64().map(|n| n.clamp(1, 10_000) as usize)
}

/// Whether a foldable row is shown whole: the user's choice, else the row's `expanded`.
pub fn unfolded(r: &Flat, st: &State) -> bool {
    st.open.get(&r.id).copied().unwrap_or(r.item["expanded"] == true)
}

/// The fold line a row shows after its first `fold` of `total` lines: "▸ N more lines" folded, "▾ less" unfolded,
/// none when everything fits.
pub fn fold_label(item: &Value, total: usize, open: bool) -> Option<String> {
    let n = fold(item)?;
    if total <= n { return None; }
    Some(if open { "▾ less".to_string() } else { format!("▸ {} more line{}", total - n, if total - n == 1 { "" } else { "s" }) })
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
    let hay = format!("{} {} {} {} {} {} {}", item["text"].as_str().unwrap_or(""), item["detail"].as_str().unwrap_or(""), item["description"].as_str().unwrap_or(""),item["summary"].as_str().unwrap_or(""),item["tags"].as_array().into_iter().flatten().filter_map(Value::as_str).collect::<Vec<_>>().join(" "), item["title"].as_str().unwrap_or(""), item["body"].as_str().unwrap_or(""));
    hay.to_lowercase().contains(f)
}

/// Row types every client renders natively; anything else (including namespaced `x.<owner>.<name>` types) needs
/// its feature claimed, else it is shown through its `fallback` or as a plain item.
pub const ROW_TYPES: &[&str] = &["item", "toggle", "header", "separator", "text", "chips", "select"];

/// A composer row's height: its text lines (`rows`, default 3, 1-12) and the control line.
pub fn composer_height(item: &Value) -> u16 { item["rows"].as_u64().unwrap_or(3).clamp(1, 12) as u16 + 1 }

/// A composer's height with a draft of `draft_lines` lines: it starts at one text line and grows with the draft up to
/// its `rows`, plus the control line.
pub fn composer_height_for(item: &Value, draft_lines: usize) -> u16 {
    // Clamp before narrowing: a legal 64 KiB draft can have 65,536+ lines, which would wrap a u16.
    draft_lines.clamp(1, usize::from(composer_height(item) - 1)) as u16 + 1
}

/// `requires_absent: [feature]`: content standing in for an enhancement, dropped by a client that has any of those
/// features (it shows the enhancement instead); older clients ignore the field and show it.
fn superseded(item: &Value, features: &[&str]) -> bool {
    item["requires_absent"].as_array().is_some_and(|l| l.iter().filter_map(Value::as_str).any(|f| features.contains(&f)))
}

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
        if superseded(item, features) { return; }
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
        // Each chip reached (the declared one, then each fallback) is checked for `requires_absent` first.
        for depth in 0..=4 {
            if superseded(&c, features) { c = Value::Null; break; }
            if depth == 4 || missing_for(&c, features).is_empty() { break; }
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

/// A row's own match: the text filter (lower case) and every chosen facet.
fn row_ok(item: &Value, f: &str, chosen: &[(String, String)]) -> bool {
    (f.is_empty() || matches(item, f)) && chosen.iter().all(|(k, want)| has_facet(item, k, want))
}

fn subtree_matches(item: &Value, id: &str, f: &str, chosen: &[(String, String)], features: &[&str]) -> bool {
    row_ok(item, f, chosen) || shown(item["children"].as_array().map(|c| c.as_slice()).unwrap_or(&[]), &format!("{id}/"), features).iter().any(|(c, cid)| subtree_matches(c, cid, f, chosen, features))
}

// ---- facets, sort, columns (features `facets`, `sort`, `columns`) ------------------------------------------------

/// Where a panel declares `facets`, `sorts` or `columns`: on the current page, else on the panel itself.
pub fn declared<'a>(v: &'a Value, st: &State, field: &str) -> Option<&'a Vec<Value>> {
    page_value(v, st).get(field).and_then(Value::as_array).or_else(|| v.get(field).and_then(Value::as_array))
}

/// Whether a row's `facets[key]` is `want` (a string), or contains it (a list).
pub fn has_facet(item: &Value, key: &str, want: &str) -> bool {
    match &item["facets"][key] {
        Value::String(s) => s == want,
        Value::Array(a) => a.iter().any(|x| x.as_str() == Some(want)),
        _ => false,
    }
}

/// The facet choices in effect: chosen values of declared facet keys, when the client renders `facets`.
pub fn chosen_facets(v: &Value, st: &State, features: &[&str]) -> Vec<(String, String)> {
    if !features.contains(&"facets") { return Vec::new(); }
    let keys: Vec<&str> = declared(v, st, "facets").into_iter().flatten().filter_map(|f| f["key"].as_str()).collect();
    st.facets.iter().filter(|(k, _)| keys.contains(&k.as_str())).map(|(k, w)| (k.clone(), w.clone())).collect()
}

/// The sort in effect: (key, descending) of the chosen entry of `sorts`, when the client renders `sort`.
pub fn chosen_sort(v: &Value, st: &State, features: &[&str]) -> Option<(String, bool)> {
    if !features.contains(&"sort") { return None; }
    let s = declared(v, st, "sorts")?.get(st.sort?)?;
    Some((s["key"].as_str()?.to_string(), s["desc"] == true))
}

/// Compare two rows by `sort[key]`: numbers before strings, strings case-insensitively; rows without the key last
/// (also when descending).
fn compare(a: &Value, b: &Value, key: &str, desc: bool) -> Ordering {
    let (x, y) = (&a["sort"][key], &b["sort"][key]);
    let keyed = |v: &Value| v.is_number() || v.is_string();
    match (keyed(x), keyed(y)) {
        (false, false) => return Ordering::Equal,
        (false, true) => return Ordering::Greater,
        (true, false) => return Ordering::Less,
        _ => {}
    }
    let o = match (x.as_f64(), y.as_f64()) {
        (Some(p), Some(q)) => p.partial_cmp(&q).unwrap_or(Ordering::Equal),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => x.as_str().unwrap_or("").to_lowercase().cmp(&y.as_str().unwrap_or("").to_lowercase()),
    };
    if desc { o.reverse() } else { o }
}

/// One sibling list sorted, stably, within each run between `header` and `separator` rows (they stay put).
fn sorted(list: Vec<(Value, String)>, by: &Option<(String, bool)>) -> Vec<(Value, String)> {
    let Some((key, desc)) = by else { return list };
    let mut out = Vec::with_capacity(list.len());
    let mut run: Vec<(Value, String)> = Vec::new();
    let flush = |run: &mut Vec<(Value, String)>, out: &mut Vec<(Value, String)>| {
        run.sort_by(|a, b| compare(&a.0, &b.0, key, *desc));
        out.append(run);
    };
    for x in list {
        if matches!(kind(&x.0), "header" | "separator") { flush(&mut run, &mut out); out.push(x); } else { run.push(x); }
    }
    flush(&mut run, &mut out);
    out
}

/// One facet's chips: its key and label, the chosen value, the count under All and each value's count.
#[derive(Debug, Clone, PartialEq)]
pub struct FacetCount {
    pub key: String,
    pub label: String,
    pub chosen: Option<String>,
    pub all: usize,
    /// (value, label, count), in declared order, else first seen in the rows.
    pub values: Vec<(String, String, usize)>,
}

/// The facet line's counts. A count is the number of rows that would show if that value were chosen, given the
/// text filter and the other facets' choices. Headers, separators, toolbar and `pin: "bottom"` rows don't count;
/// rows without the key count only under All.
pub fn facet_counts(v: &Value, st: &State, features: &[&str]) -> Vec<FacetCount> {
    if !features.contains(&"facets") { return Vec::new(); }
    let Some(decl) = declared(v, st, "facets") else { return Vec::new() };
    let f = st.filter.to_lowercase();
    let chosen = chosen_facets(v, st, features);
    let pin = features.contains(&"pin");
    let mut items = Vec::new();
    fn walk(list: &[Value], prefix: &str, features: &[&str], out: &mut Vec<Value>) {
        for (item, id) in shown(list, prefix, features) {
            if !matches!(kind(&item), "header" | "separator") { out.push(item.clone()); }
            if let Some(c) = item["children"].as_array() { walk(c, &format!("{id}/"), features, out); }
        }
    }
    let top: Vec<Value> = page_items(v, st).iter().filter(|i| !(pin && i["pin"] == "bottom")).cloned().collect();
    walk(&top, "", features, &mut items);
    decl.iter().filter_map(|d| {
        let key = d["key"].as_str()?.to_string();
        let others: Vec<(String, String)> = chosen.iter().filter(|(k, _)| *k != key).cloned().collect();
        let pool: Vec<&Value> = items.iter().filter(|i| row_ok(i, &f, &others)).collect();
        let mut values: Vec<(String, String)> = d["values"].as_array().into_iter().flatten()
            .filter_map(|x| Some((x["value"].as_str()?.to_string(), x["label"].as_str().or(x["value"].as_str())?.to_string()))).collect();
        if !d["values"].is_array() {
            for i in &items {
                let found: Vec<&str> = match &i["facets"][key.as_str()] { Value::String(s) => vec![s.as_str()], Value::Array(a) => a.iter().filter_map(Value::as_str).collect(), _ => vec![] };
                for s in found { if !values.iter().any(|(x, _)| x == s) { values.push((s.to_string(), s.to_string())); } }
            }
        }
        Some(FacetCount {
            label: d["label"].as_str().unwrap_or(&key).to_string(),
            chosen: chosen.iter().find(|(k, _)| *k == key).map(|(_, w)| w.clone()),
            all: pool.len(),
            values: values.into_iter().map(|(value, label)| { let n = pool.iter().filter(|i| has_facet(i, &key, &value)).count(); (value, label, n) }).collect(),
            key,
        })
    }).collect()
}

/// `client.panel {action: "facet", key, value?}`: choose a value (None: All). Unknown keys are ignored.
pub fn choose_facet(v: &Value, st: &mut State, key: &str, value: Option<&str>) {
    if !declared(v, st, "facets").into_iter().flatten().any(|f| f["key"] == key) { return; }
    match value { Some(w) => { st.facets.insert(key.to_string(), w.to_string()); } None => { st.facets.remove(key); } }
    st.sel = 0; st.top = 0;
}

/// The facet key `f` cycles: the first declared facet, All -> each value in order -> All.
pub fn cycle_facet(v: &Value, st: &mut State, features: &[&str]) {
    let Some(first) = facet_counts(v, st, features).into_iter().next() else { return };
    let at = first.chosen.as_ref().and_then(|c| first.values.iter().position(|(x, _, _)| x == c));
    let next = match at { None => first.values.first(), Some(i) => first.values.get(i + 1) }.map(|(x, _, _)| x.clone());
    choose_facet(v, st, &first.key, next.as_deref());
}

/// `client.panel {action: "sort", key?}`: sort by that entry of `sorts` (no key: the given order).
pub fn choose_sort(v: &Value, st: &mut State, key: Option<&str>) {
    st.sort = key.and_then(|k| declared(v, st, "sorts")?.iter().position(|s| s["key"] == k));
}

/// The sort key `s` cycles: given order -> each entry of `sorts` -> given order.
pub fn cycle_sort(v: &Value, st: &mut State) {
    let n = declared(v, st, "sorts").map(|s| s.len()).unwrap_or(0);
    st.sort = match st.sort { None if n > 0 => Some(0), Some(i) if i + 1 < n => Some(i + 1), _ => None };
}

/// The label of the sort in effect (`given` for the given order), for the client's line.
pub fn sort_label(v: &Value, st: &State) -> Option<String> {
    let sorts = declared(v, st, "sorts").filter(|s| !s.is_empty())?;
    Some(st.sort.and_then(|i| sorts.get(i)).and_then(|s| s["label"].as_str().or(s["key"].as_str())).unwrap_or("given order").to_string())
}

/// Column widths for `columns` in `width` cells (gap of one between shown columns): numbers are fixed, absent widths
/// take the widest cell (capped at a third of the width), `"fill"` columns share what is left. Columns degrade by
/// width: while a column with `min_width` would get less, the shown column with the lowest `priority` is dropped
/// (width 0; columns without `priority` are never dropped). Clients skip zero-width columns.
pub fn column_widths(spec: &[Value], rows: &[Flat], width: usize) -> Vec<usize> {
    let n = spec.len();
    if n == 0 { return Vec::new(); }
    let mut shown = vec![true; n];
    loop {
        let w = widths_for(spec, rows, width, &shown);
        // A column that gets less than its `min_width` drops the lowest-priority shown column, which may be itself.
        let short = spec.iter().enumerate().any(|(i, c)| shown[i] && c["min_width"].as_u64().is_some_and(|m| w[i] < m as usize));
        let victim = spec.iter().enumerate().filter(|(i, c)| shown[*i] && c["priority"].is_number())
            .min_by(|a, b| a.1["priority"].as_f64().partial_cmp(&b.1["priority"].as_f64()).unwrap_or(Ordering::Equal).then(b.0.cmp(&a.0)))
            .map(|(i, _)| i);
        match (short, victim) { (true, Some(i)) => shown[i] = false, _ => return w }
    }
}

fn widths_for(spec: &[Value], rows: &[Flat], width: usize, shown: &[bool]) -> Vec<usize> {
    // Terminal cells, not characters: wide (CJK, emoji) text takes two each.
    let cells = |s: &str| unicode_width::UnicodeWidthStr::width(s);
    let cell = |r: &Flat, i: usize| r.item["columns"].get(i).and_then(Value::as_str).map(cells).unwrap_or(0);
    let mut w: Vec<Option<usize>> = spec.iter().enumerate().map(|(i, c)| if !shown[i] { Some(0) } else { match &c["width"] {
        Value::Number(x) => x.as_u64().map(|x| x as usize),
        Value::String(s) if s == "fill" => None,
        _ => Some(rows.iter().map(|r| cell(r, i)).chain(std::iter::once(c["label"].as_str().map(cells).unwrap_or(0))).max().unwrap_or(0).min(width / 3).max(1)),
    } }).collect();
    let visible = shown.iter().filter(|s| **s).count();
    let fixed: usize = w.iter().flatten().sum::<usize>() + visible.saturating_sub(1);
    let fills = w.iter().filter(|x| x.is_none()).count();
    let left = width.saturating_sub(fixed);
    for (k, x) in w.iter_mut().filter(|x| x.is_none()).enumerate() { *x = Some(left / fills.max(1) + usize::from(k < left % fills.max(1))); }
    w.into_iter().map(|x| x.unwrap_or(0)).collect()
}

/// The rows to show, in order, for this client's features.
pub fn rows(v: &Value, st: &State) -> Vec<Flat> { rows_for(v, st, SUPPORTED) }

/// The rows a client rendering `features` shows: toolbar, page, tree expansion, filter and element fallbacks. With
/// feature `pin`, top-level rows with `pin: "bottom"` (and their children) come last, marked `bottom`, never
/// filtered out; clients without it show them where they are.
pub fn rows_for(v: &Value, st: &State, features: &[&str]) -> Vec<Flat> {
    let f = st.filter.to_lowercase();
    let chosen = chosen_facets(v, st, features);
    let by = chosen_sort(v, st, features);
    let mut out = Vec::new();
    struct Ctx<'a> { st: &'a State, features: &'a [&'a str], chosen: &'a [(String, String)], by: &'a Option<(String, bool)> }
    fn one(item: Value, id: String, depth: usize, parent: Option<usize>, c: &Ctx, f: &str, bottom: bool, out: &mut Vec<Flat>) {
        let (st, features) = (c.st, c.features);
        let chosen: &[(String, String)] = if bottom { &[] } else { c.chosen };  // pin: "bottom" rows are never filtered
        let narrowed = !f.is_empty() || !chosen.is_empty();
        if narrowed && !subtree_matches(&item, &id, f, chosen, features) {
            return;
        }
        let children = item["children"].as_array().cloned().unwrap_or_default();
        let has_children = !children.is_empty();
        // Filtering (text or facets) opens everything that matched below.
        let expanded = has_children && (narrowed || st.open.get(&id).copied().unwrap_or(item["expanded"] == true));
        let mut row = item.clone();
        if let Some(o) = row.as_object_mut() {
            o.remove("children");
        }
        let me = out.len();
        out.push(Flat { item: row, depth, id: id.clone(), parent, has_children, expanded, pinned: false, bottom });
        if expanded {
            for (child, cid) in sorted(shown(&children, &format!("{id}/"), features), c.by) {
                one(child, cid, depth + (kind(&item) != "header") as usize, Some(me), c, f, bottom, out);
            }
        }
    }
    let c = Ctx { st, features, chosen: &chosen, by: &by };
    let toolbar = v["toolbar"].as_array().map(|a| a.as_slice()).unwrap_or(&[]);
    for (n, (item, _)) in shown(toolbar, "toolbar/", features).into_iter().enumerate() {
        out.push(Flat { item, depth: 0, id: format!("toolbar/{n}"), parent: None, has_children: false, expanded: false, pinned: true, bottom: false });
    }
    let pin = features.contains(&"pin");
    let (bottom, list): (Vec<_>, Vec<_>) = shown(page_items(v, st), "", features).into_iter().partition(|(item, _)| pin && item["pin"] == "bottom");
    for (item, id) in sorted(list, &by) { one(item, id, 0, None, &c, &f, false, &mut out); }
    for (item, id) in bottom { one(item, id, 0, None, &c, "", true, &mut out); }
    out
}

/// Search (feature `search`): every row on the current page whose text matches `query` (as the filter matches,
/// case-insensitive), collapsed rows included, in display order, with the ids of the rows to open to show it.
/// Toolbar rows and bottom-pinned rows are not searched; the text filter and facets still apply, and the order is the
/// display order (sort included).
pub fn search(v: &Value, st: &State, query: &str, features: &[&str]) -> Vec<(String, Vec<String>)> {
    let q = query.to_lowercase();
    let f = st.filter.to_lowercase();
    let chosen = chosen_facets(v, st, features);
    let by = chosen_sort(v, st, features);
    let mut out = Vec::new();
    if q.is_empty() { return out; }
    #[allow(clippy::too_many_arguments)]
    fn walk(items: Vec<(Value, String)>, path: &[String], q: &str, f: &str, chosen: &[(String, String)], by: &Option<(String, bool)>, features: &[&str], out: &mut Vec<(String, Vec<String>)>) {
        for (item, id) in sorted(items, by) {
            if (!f.is_empty() || !chosen.is_empty()) && !subtree_matches(&item, &id, f, chosen, features) { continue; }
            if matches(&item, q) && selectable(&item) { out.push((id.clone(), path.to_vec())); }
            let children = item["children"].as_array().map(|c| c.as_slice()).unwrap_or(&[]);
            if !children.is_empty() {
                let mut p = path.to_vec();
                p.push(id.clone());
                walk(shown(children, &format!("{id}/"), features), &p, q, f, chosen, by, features, out);
            }
        }
    }
    let pin = features.contains(&"pin");
    let list: Vec<_> = shown(page_items(v, st), "", features).into_iter().filter(|(item, _)| !(pin && item["pin"] == "bottom")).collect();
    walk(list, &[], &q, &f, &chosen, &by, features, &mut out);
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
    // The end of the conversation: the last selectable row of the scrolling list, not a toolbar or bottom-pinned
    // row (a composer, a waiting line). A selection on those stays put; the renderer keeps the list at its end.
    let Some(end) = rows.iter().rposition(|r| selectable(&r.item) && !r.pinned && !r.bottom) else { return };
    // Typing in a composer: the selection follows the composer row as rows arrive above it.
    if let Some(at) = st.editing.as_deref().and_then(|id| rows.iter().position(|r| r.id == id)) { st.sel = at; }
    let in_list = rows.get(st.sel).is_some_and(|r| !r.pinned && !r.bottom);
    // First shown (and not typing): start at the end. Later: keep the end while the reader is there.
    let first = st.follow_end.is_none() && st.editing.is_none();
    if first || in_list && st.follow_end.is_some_and(|was| st.sel >= was) {
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
    } else if fold(&r.item).is_some() {
        let open = unfolded(r, st);
        st.open.insert(r.id.clone(), !open);
    }
    None
}

/// ←: close an open row, else go to its parent. →: open a closed row, else go to its first child.
pub fn left(rows: &[Flat], st: &mut State) {
    let Some(r) = rows.get(st.sel) else { return };
    if r.has_children && r.expanded {
        st.open.insert(r.id.clone(), false);
    } else if fold(&r.item).is_some() && unfolded(r, st) {
        st.open.insert(r.id.clone(), false);
    } else if let Some(p) = r.parent {
        st.sel = p;
    }
}

pub fn right(rows: &[Flat], st: &mut State) {
    let Some(r) = rows.get(st.sel) else { return };
    if r.has_children && !r.expanded {
        st.open.insert(r.id.clone(), true);
    } else if fold(&r.item).is_some() && !unfolded(r, st) {
        st.open.insert(r.id.clone(), true);
    } else if r.has_children && st.sel + 1 < rows.len() {
        st.sel += 1;
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn composer_height_is_capped_for_drafts_past_u16_lines() {
        let item = serde_json::json!({"type": "composer", "rows": 3});
        for lines in [0usize, 1, 2, 3, 4, 65_535, 65_536, 65_537, 131_073, usize::MAX] {
            let want = lines.clamp(1, 3) as u16 + 1;
            assert_eq!(composer_height_for(&item, lines), want, "{lines} draft lines");
        }
    }

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
        // A bottom-pinned composer: the end is the last list row, and a selection on the composer stays there.
        let with_pin = |n: usize| { let mut v = panel(n); v["items"].as_array_mut().unwrap().push(serde_json::json!({"type": "composer", "id": "c", "text": "Message", "pin": "bottom"})); v };
        let pin_rows = |v: &Value, st: &mut State| { let r = rows_for(v, st, &["pin", "composer"]); follow(v, &r, st); st.sel };
        let mut st = State::default();
        assert_eq!(pin_rows(&with_pin(3), &mut st), 2, "the end is the last conversation row, not the composer");
        st.sel = 3;
        st.editing = Some("c".into());
        assert_eq!(pin_rows(&with_pin(5), &mut st), 5, "typing in the composer: the selection stays on it as rows arrive");
        assert_eq!(st.follow_end, Some(4));
        // First shown with the selection on a toolbar row (a log viewer's source picker): start at the end.
        let mut tb = panel(3);
        tb["toolbar"] = serde_json::json!([{"type": "select", "text": "Source", "options": []}]);
        let mut st = State::default();
        assert_eq!(pin_rows(&tb, &mut st), 3, "toolbar row 0, then list rows 1..=3");
    }

    #[test]
    fn facet_and_sort_choices_cycle_and_invalidate_prepared_rows() {
        let v = Arc::new(serde_json::json!({"facets": [{"key": "k"}], "sorts": [{"key": "n"}, {"key": "n", "desc": true}],
            "items": [{"text": "b", "facets": {"k": "x"}, "sort": {"n": "b"}}, {"text": "a", "facets": {"k": "y"}, "sort": {"n": "a"}}]}));
        let mut st = State::default();
        let all = st.prepare(&v);
        cycle_facet(&v, &mut st, SUPPORTED); assert_eq!(st.facets.get("k").map(String::as_str), Some("x"));
        let narrowed = st.prepare(&v); assert!(!Arc::ptr_eq(&all, &narrowed)); assert_eq!(narrowed.rows.len(), 1);
        assert_eq!(narrowed.total, 2, "the total counts every row, the shown count only matches");
        cycle_facet(&v, &mut st, SUPPORTED); assert_eq!(st.facets.get("k").map(String::as_str), Some("y"));
        cycle_facet(&v, &mut st, SUPPORTED); assert!(st.facets.is_empty(), "back to All");
        cycle_sort(&v, &mut st); assert_eq!(st.sort, Some(0));
        assert_eq!(st.prepare(&v).rows.iter().map(|r| r.item["text"].clone()).collect::<Vec<_>>(), vec!["a", "b"]);
        cycle_sort(&v, &mut st); cycle_sort(&v, &mut st); assert_eq!(st.sort, None, "back to the given order");
        choose_sort(&v, &mut st, Some("n")); assert_eq!(st.sort, Some(0));
        choose_facet(&v, &mut st, "unknown", Some("x")); assert!(st.facets.is_empty(), "undeclared keys are ignored");
        assert_eq!(sort_label(&v, &st).as_deref(), Some("n"));
    }

    #[test]
    fn the_shown_page_stays_when_a_page_is_added_before_it() {
        let v = |titles: &[&str]| serde_json::json!({"pages": titles.iter().map(|t| serde_json::json!({"title": t, "items": []})).collect::<Vec<_>>()});
        let mut st = State::default();
        st.keep_page(&v(&["Extensions", "Icons", "Keys"]));
        st.page = 2;  // the user went to Keys
        st.keep_page(&v(&["Extensions", "Icons", "Keys"]));
        assert_eq!(st.page, 2, "unchanged pages: the user's choice stands");
        st.keep_page(&v(&["Extensions", "Sidebar", "Icons", "Keys"]));
        assert_eq!(st.page, 3, "a page added before it: still Keys");
        st.keep_page(&v(&["Extensions", "Icons"]));
        assert_eq!(st.page, 3, "its page gone: the index stays (clamped when drawn)");
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
