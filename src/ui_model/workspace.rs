//! The client's cache of the workspace (snapshot plus ordered events) and what it implies for contributions:
//! which `menu/*` and `binding/*` entries are live, binding precedence and the context `when` is matched against.
//! A renderer reacts to the [`Change`]s that applying an event reports; it never edits this cache itself.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde_json::{json, Value};

use super::keys;

/// What applying one event changed, for the renderer to react to (redraw a panel, close a stream, say something).
#[derive(Debug, Clone, PartialEq)]
pub enum Change {
    /// The cache cannot follow (another core epoch, or a partial record for an unknown resource): take a snapshot.
    Resync,
    /// A view's entry or live content changed.
    View(String),
    /// A layout entry changed (a committed layout replaces any local preview).
    Layout,
    /// Someone else's transaction changed the workspace.
    ChangedBy { author: String, txn: Value },
    /// A resource record changed: its availability and liveness, and whether it just became available again.
    Resource { id: String, available: bool, live: bool, restored: bool },
    /// An extension failed, with its reason.
    ExtensionFailed { name: String, reason: String },
    /// Safe mode switched (`was` is the previous state).
    SafeMode { on: bool, was: bool },
    /// A transient notification for every client (`ui.notify`).
    Notify { text: String, style: Option<String> },
    /// A backend provider disconnected.
    BackendDisconnected { name: String, error: String },
}

#[derive(Default)]
pub struct Workspace {
    pub epoch: String,
    pub seq: u64,
    pub entries: BTreeMap<String, Value>,
    pub versions: BTreeMap<String, u64>,
    pub resources: BTreeMap<String, Value>,
    pub commands: BTreeMap<String, Value>,
    pub extensions: BTreeMap<String, Value>,
    pub safe_mode: bool,
    pub safe_mode_allow: BTreeSet<String>,
    pub provider_extensions: BTreeSet<String>,
    pub backends_connected: BTreeSet<String>,
    pub backend_notice: Option<String>,
    /// Live view content from `view.publish`, merged over the view entry.
    pub live: HashMap<String, Value>,
    /// The finish (`done`/`error`) each resource reached since its tab was last shown (`tabbar::reached`), for tab
    /// marks; a client removes a resource's entry when it shows the tab.
    pub reached: HashMap<String, &'static str>,
}

pub fn strings(value: &Value) -> BTreeSet<String> { value.as_array().into_iter().flatten().filter_map(Value::as_str).map(String::from).collect() }

impl Workspace {
    /// Replace the cache with a snapshot. Returns whether safe mode was on before.
    pub fn load(&mut self, snap: &Value) -> bool {
        self.epoch = snap["epoch"].as_str().unwrap_or("").to_string();
        self.seq = snap["seq"].as_u64().unwrap_or(0);
        self.entries.clear();
        self.versions.clear();
        if let Some(es) = snap["entries"].as_object() {
            for (k, e) in es {
                self.entries.insert(k.clone(), e["value"].clone());
                self.versions.insert(k.clone(), e["version"].as_u64().unwrap_or(0));
            }
        }
        self.resources = snap["resources"].as_array().into_iter().flatten().filter_map(|r| Some((r["id"].as_str()?.to_string(), r.clone()))).collect();
        let resources = &self.resources;
        self.reached.retain(|id, _| resources.contains_key(id));
        self.commands = snap["commands"].as_array().into_iter().flatten().filter_map(|c| Some((c["name"].as_str()?.to_string(), c.clone()))).collect();
        self.extensions = snap["extensions"].as_object().map(|o| o.iter().map(|(k, v)| (k.clone(), v.clone())).collect()).unwrap_or_default();
        let was_safe = self.safe_mode;
        self.safe_mode = snap["safe_mode"].as_bool().unwrap_or(false);
        self.safe_mode_allow = strings(&snap["safe_mode_allow"]);
        self.provider_extensions = strings(&snap["provider_extensions"]);
        self.backend_notice = snap["backend_notice"].as_str().filter(|s| !s.is_empty()).map(String::from);
        self.backends_connected = snap["providers"].as_object().into_iter().flatten().filter(|(_, p)| p["role"] == "backend" && p["connected"] == true).map(|(name, _)| name.clone()).collect();
        self.live = snap["live"].as_object().map(|o| o.iter().map(|(k, v)| (k.clone(), v.clone())).collect()).unwrap_or_default();
        was_safe
    }

    /// Apply one event record in order. Old or duplicate events are ignored; an event from another epoch asks for
    /// a resync.
    pub fn apply(&mut self, ev: &Value) -> Vec<Change> {
        if ev["epoch"].as_str() != Some(self.epoch.as_str()) {
            return vec![Change::Resync];
        }
        let seq = ev["seq"].as_u64().unwrap_or(0);
        if seq <= self.seq {
            return Vec::new();
        }
        self.seq = seq;
        let d = &ev["data"];
        if d.get("backend_notice").is_some() {
            self.backend_notice = d["backend_notice"].as_str().filter(|s| !s.is_empty()).map(String::from);
        }
        let mut out = Vec::new();
        match ev["type"].as_str().unwrap_or("") {
            "entries" => {
                for c in d["changes"].as_array().into_iter().flatten() {
                    let Some(k) = c["key"].as_str() else { continue };
                    if let Some(view) = k.strip_prefix("view/") { out.push(Change::View(view.to_string())); }
                    if k.starts_with("layout/") && !out.contains(&Change::Layout) { out.push(Change::Layout); }
                    if c["value"].is_null() {
                        self.entries.remove(k);
                        self.versions.remove(k);
                    } else {
                        self.entries.insert(k.to_string(), c["value"].clone());
                        self.versions.insert(k.to_string(), c["version"].as_u64().unwrap_or(0));
                    }
                }
                out.push(Change::ChangedBy { author: d["author"].as_str().unwrap_or("?").to_string(), txn: d["txn"].clone() });
            }
            "resource" => {
                if let Some(id) = d["id"].as_str() {
                    // Merge the complete record before evaluating availability: partial status
                    // events must not accidentally make a stale resource available again.
                    let record = if ev["partial"] == true {
                        let Some(mut base) = self.resources.get(id).cloned() else { return vec![Change::Resync] };
                        for (key, value) in d.as_object().into_iter().flatten() { base[key] = value.clone(); }
                        base
                    } else { d.clone() };
                    let available = record["available"].as_bool().unwrap_or(true);
                    let restored = available && self.resources.get(id).is_some_and(|old| old["available"] == false);
                    let live = record["state"] == "live";
                    if let Some(r) = super::tabbar::reached(self.resources.get(id), &record) {
                        let held = self.reached.get(id).copied();
                        self.reached.insert(id.to_string(), super::tabbar::latch(held, r));
                    }
                    self.resources.insert(id.to_string(), record);
                    out.push(Change::Resource { id: id.to_string(), available, live, restored });
                }
            }
            "resource.removed" => {
                if let Some(id) = d["id"].as_str() { self.resources.remove(id); self.reached.remove(id); }
            }
            "command" => {
                if let Some(n) = d["name"].as_str() {
                    if d["available"] == false { self.commands.remove(n); } else { self.commands.insert(n.to_string(), d.clone()); }
                }
            }
            "extension" => {
                if let Some(n) = d["name"].as_str() {
                    let mut st = d["state"].clone();
                    // Events carry runtime facts only; keep the snapshot's static dependency facts (D29).
                    if st["dependencies"].is_null() { if let Some(old) = self.extensions.get(n) { st["dependencies"] = old["dependencies"].clone(); } }
                    if st["status"] == "failed" {
                        out.push(Change::ExtensionFailed { name: n.to_string(), reason: st["failure"]["reason"].as_str().or_else(|| st["last_exit"].as_str()).unwrap_or("?").to_string() });
                    }
                    self.extensions.insert(n.to_string(), st);
                }
            }
            "safe_mode" => {
                let was = self.safe_mode;
                self.safe_mode = d["on"].as_bool().unwrap_or(false);
                self.safe_mode_allow = strings(&d["allow"]);
                self.provider_extensions = strings(&d["provider_extensions"]);
                out.push(Change::SafeMode { on: self.safe_mode, was });
            }
            "notify" => out.push(Change::Notify { text: d["text"].as_str().unwrap_or("").to_string(), style: d["style"].as_str().map(String::from) }),
            "view.content" => {
                if let Some(v) = d["view"].as_str() {
                    if d["content"].is_null() { self.live.remove(v); } else { self.live.insert(v.to_string(), d["content"].clone()); }
                    out.push(Change::View(v.to_string()));
                }
            }
            "adapter" | "provider" => {
                if let Some(name) = d["name"].as_str() {
                    if d["state"]["role"] == "backend" && d["state"]["connected"] == true { self.backends_connected.insert(name.into()); } else { self.backends_connected.remove(name); }
                }
                if d["state"]["connected"] == false {
                    out.push(Change::BackendDisconnected { name: d["name"].as_str().unwrap_or("?").to_string(), error: d["state"]["error"].as_str().unwrap_or("").to_string() });
                }
            }
            _ => {}
        }
        out
    }

    // ---- contributions: menus, bindings ------------------------------------------------------------

    /// Whether a contributed entry is live: its owner extension is on (and allowed in safe mode), and its command
    /// exists. Contributions of a disabled or removed extension disappear.
    pub fn visible(&self, e: &Value) -> bool {
        if let Some(owner) = e["owner"].as_str() {
            let on = self.entries.get(&format!("extension/{owner}")).map(|x| x["enabled"].as_bool().unwrap_or(true)).unwrap_or(false);
            if if self.safe_mode { !self.safe_mode_allow.contains(owner) } else { !on } { return false; }
        }
        let cmd = e["command"].as_str().unwrap_or("");
        cmd.starts_with("client.") || self.commands.get(cmd).map(|c| c["available"] == true).unwrap_or(false)
    }

    /// The binding a gesture runs in a place: the same precedence as keys (see [`rank`]).
    pub fn gesture(&self, key: &str, place: &str) -> Option<Value> {
        self.entries
            .iter()
            .filter(|(k, b)| k.starts_with("binding/") && b["key"] == key && b["where"].as_str().unwrap_or("pane") == place && self.visible(b))
            .min_by_key(|(k, b)| rank(k, b))
            .map(|(_, b)| b.clone())
            .filter(|b| b["command"] != "client.none")
    }

    /// Visible `menu/*` entries for a place whose `when` holds in the context, in `order` (then id).
    pub fn menu_entries(&self, place: &str, ctx: &Value) -> Vec<Value> {
        let mut found: Vec<(i64, &String, &Value)> = self
            .entries
            .iter()
            .filter(|(k, v)| k.starts_with("menu/") && v["where"] == place && self.visible(v) && when_holds(v, ctx))
            .map(|(k, v)| (v["order"].as_i64().unwrap_or(50), k, v))
            .collect();
        found.sort_by(|a, b| (a.0, a.1).cmp(&(b.0, b.1)));
        found.into_iter().map(|(_, _, v)| v.clone()).collect()
    }

    /// Every keyboard binding with its id that applies in `ctx` (the focused pane's context): the client's
    /// defaults, then visible `binding/*` entries whose `when` holds. A binding whose `when` does not hold is no
    /// candidate, so the key falls through to the next binding for it (or to none).
    pub fn key_candidates(&self, ctx: &Value) -> Vec<(String, Value)> {
        let mut found: Vec<(String, Value)> = builtin_bindings().into_iter().map(|b| (format!("{BUILTIN}{}", b["key"].as_str().unwrap_or("")), b)).collect();
        found.extend(
            self.entries
                .iter()
                .filter(|(k, v)| k.starts_with("binding/") && scope(v) == "prefix" && !v["key"].as_str().unwrap_or("").starts_with("mouse.") && self.visible(v) && when_holds(v, ctx))
                .map(|(k, v)| (k.clone(), v.clone())),
        );
        found
    }

    /// The binding a key runs without the prefix in a focused pane (D29): `scope: "pane"` while the reader is not
    /// typing, `scope: "composer"` while typing in a composer. Matched like prefix keys (`when` against the pane's
    /// context, then priority and lowest id); `client.none` unbinds. Never applied to terminal views, whose keys
    /// belong to their programs; the prefix key always comes first.
    pub fn scoped_binding(&self, key: &str, scope_name: &str, ctx: &Value) -> Option<Value> {
        if ctx["kind"] == "terminal" { return None; }
        self.entries.iter()
            .filter(|(k, v)| k.starts_with("binding/") && scope(v) == scope_name && self.visible(v) && when_holds(v, ctx)
                && v["key"].as_str().and_then(keys::normalize).as_deref() == Some(key))
            .min_by_key(|(k, v)| rank(k, v))
            .map(|(_, v)| v.clone())
            .filter(|v| v["command"] != "client.none")
    }

    /// The binding a key (canonical name) runs in `ctx`, if any; `client.none` leaves the key unbound.
    pub fn binding_for(&self, key: &str, ctx: &Value) -> Option<Value> {
        let cands = self.key_candidates(ctx);
        let (_, b) = &cands[*resolve(&cands).get(key)?];
        (b["command"] != "client.none").then(|| b.clone())
    }

    /// A binding for this key that applies only elsewhere (its `when` does not hold in `ctx`).
    pub fn bound_elsewhere(&self, key: &str, ctx: &Value) -> Option<Value> {
        self.entries.iter()
            .filter(|(k, v)| k.starts_with("binding/") && scope(v) == "prefix" && self.visible(v) && !when_holds(v, ctx))
            .find(|(_, v)| v["key"].as_str().and_then(keys::normalize).as_deref() == Some(key))
            .map(|(_, v)| v.clone())
    }

    /// What a binding does, for menus: its label, else its command's title, else the command name.
    pub fn binding_label(&self, b: &Value) -> String {
        let cmd = b["command"].as_str().unwrap_or("");
        if cmd == "vqro.safe_mode" && self.safe_mode {
            return "leave safe mode".into();
        }
        b["label"].as_str().or_else(|| self.commands.get(cmd).and_then(|c| c["title"].as_str())).unwrap_or(cmd).to_string()
    }

    /// The client setting `doc/settings/tui.<name>`.
    pub fn setting(&self, name: &str) -> Option<&Value> { self.entries.get("doc/settings/tui").and_then(|s| s.get(name)) }
}

/// Ids of the client's default bindings, which win ties (see [`rank`]).
pub const BUILTIN: &str = "builtin:";

/// The client's default prefix keys, as bindings. `recovery` ones stay in the prefix menu even when a binding
/// takes their key; `hidden` ones work but stay out of the menu.
pub fn builtin_bindings() -> Vec<Value> {
    let mut b = vec![
        json!({"key": ":", "command": "client.palette", "label": "all commands", "recovery": true}),
        json!({"key": "o", "command": "client.focus", "args": {"dir": "next"}, "label": "next pane"}),
        json!({"key": "O", "command": "client.focus", "args": {"dir": "prev"}, "label": "previous pane", "hidden": true}),
        json!({"key": "u", "command": "vqro.revert", "label": "undo last change", "recovery": true}),
        json!({"key": "R", "command": "vqro.layout.reset", "label": "reset layout", "recovery": true}),
        json!({"key": "!", "command": "client.safe_mode", "label": "safe mode / recovery choices", "recovery": true}),
        json!({"key": "q", "command": "client.quit", "label": "quit this UI", "recovery": true}),
    ];
    b.extend((1..=9).map(|n| json!({"key": n.to_string(), "command": "client.tab", "args": {"to": n}, "label": format!("tab {n}"), "hidden": true})));
    b
}

/// A binding's scope: `prefix` (default: after the prefix key), `pane` or `composer` (without the prefix).
pub fn scope(v: &Value) -> &str { v["scope"].as_str().unwrap_or("prefix") }

/// Whether an entry's `when` holds in a context: every field equal. No `when` always holds.
pub fn when_holds(v: &Value, ctx: &Value) -> bool {
    v["when"].as_object().map(|w| w.iter().all(|(k, want)| ctx[k] == *want)).unwrap_or(true)
}

/// Which of several bindings for one key wins (smallest rank): the highest `priority` (default 0), then the
/// client's defaults, then the lowest id, so the outcome never depends on load order.
pub fn rank(id: &str, b: &Value) -> (std::cmp::Reverse<i64>, bool, String) {
    (std::cmp::Reverse(b["priority"].as_i64().unwrap_or(0)), !id.starts_with(BUILTIN), id.to_string())
}

/// For each key (canonical name), the index of the binding that wins it. Keys that don't parse are ignored, so
/// a binding from a newer protocol can't break the keymap.
pub fn resolve(cands: &[(String, Value)]) -> HashMap<String, usize> {
    let mut won: HashMap<String, usize> = HashMap::new();
    for (i, (id, b)) in cands.iter().enumerate() {
        let Some(key) = b["key"].as_str().and_then(keys::normalize) else { continue };
        let better = won.get(&key).map(|&j| rank(id, b) < rank(&cands[j].0, &cands[j].1)).unwrap_or(true);
        if better {
            won.insert(key, i);
        }
    }
    won
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resource(ws: &mut Workspace, seq: u64, state: Value) {
        ws.apply(&json!({"epoch": "", "seq": seq, "type": "resource", "data": {"id": "r1", "state": "live", "status": state}}));
    }

    #[test]
    fn a_finish_is_latched_from_events_even_when_the_status_clears_at_once() {
        // herdr-core: working, done, then null as soon as the reporting command returns to the shell.
        let mut ws = Workspace::default();
        resource(&mut ws, 1, json!({"state": "working"}));
        resource(&mut ws, 2, json!({"state": "done"}));
        resource(&mut ws, 3, Value::Null);
        assert_eq!(ws.reached.get("r1"), Some(&"done"));
        resource(&mut ws, 4, json!({"state": "error"}));
        resource(&mut ws, 5, json!({"state": "done"}));
        assert_eq!(ws.reached.get("r1"), Some(&"error"), "the more urgent finish is kept");
        ws.apply(&json!({"epoch": "", "seq": 6, "type": "resource.removed", "data": {"id": "r1"}}));
        assert!(ws.reached.is_empty());
    }
}
