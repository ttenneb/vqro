//! The workspace store: prim's authoritative workspace state (D15).
//!
//! State is a map of keyed JSON entries, each with a version. It changes only through transactions:
//! a list of operations that is validated against the resulting state and applied all-or-nothing,
//! recorded with its author, persisted atomically, and revertible.
//!
//! Success means the candidate file and its containing directory have been synced. Before rename,
//! persistence failure leaves memory and history untouched. After rename, a directory-sync failure
//! adopts the candidate (the file already names it) but returns `durability_unconfirmed`, with the
//! transaction id in error data. Callers must publish that adopted change, report the uncertainty,
//! and never replay it automatically. A restart after a crash may recover either state.
//!
//! Key namespaces:
//! - `view/<id>`       something a client can render: `{"kind": "terminal", "resource": "r1"}`,
//!                     `{"kind": "panel", "title": ..., "items": [...]}`, or any other kind (open set)
//! - `layout/<name>`   a tree of `{"view": id}`, `{"split": "h"|"v", "children": [...]}`,
//!                     `{"tabs": [{"title": ..., "node": ...}], "active": n}`
//! - `binding/<id>`    `{"key": "x", "command": "name", "args": {...}, "where"?, "owner"?}`: a prefix key,
//!                     or a gesture such as `mouse.right` with `where` = "pane" | "tab"
//! - `menu/<id>`       `{"where": "pane"|"tab"|"tabbar"|"status", "label", "icon"?, "command", "args"?, "input"?,
//!                      "pick"?, "when"?, "order"?, "owner"?}`: an item a client shows in that menu
//!   Entries with `owner` belong to that extension; clients hide them while it is off (D17).
//! - `extension/<name>` `{"argv": [...], "enabled": true}`
//! - `workspace/active` `{"layout": name}`
//! - `doc/<ns>/<name>` any JSON

use std::collections::{BTreeMap, VecDeque};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::rpc::RpcError;

const HISTORY: usize = 500;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub version: u64,
    pub value: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Change {
    pub key: String,
    pub before: Option<Entry>,
    pub after: Option<Entry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Txn {
    pub id: u64,
    pub author: String,
    pub at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reverts: Option<u64>,
    pub changes: Vec<Change>,
}

/// One operation in a transaction. `expect` is compare-and-set: the entry's current version,
/// or 0 for "must not exist".
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op {
    Set { key: String, value: Value, #[serde(default)] expect: Option<u64> },
    Delete { key: String, #[serde(default)] expect: Option<u64> },
}

#[derive(Default, Serialize, Deserialize)]
struct Persisted {
    rev: u64,
    next_txn: u64,
    entries: BTreeMap<String, Entry>,
    history: VecDeque<Txn>,
}

pub struct Store {
    rev: u64,
    next_txn: u64,
    entries: BTreeMap<String, Entry>,
    history: VecDeque<Txn>,
    path: Option<PathBuf>,
}

impl Store {
    pub fn in_memory() -> Store {
        Store { rev: 0, next_txn: 1, entries: BTreeMap::new(), history: VecDeque::new(), path: None }
    }

    /// Load from a file, or start empty if it does not exist.
    pub fn open(path: PathBuf) -> anyhow::Result<Store> {
        let p: Persisted = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Persisted { next_txn: 1, ..Default::default() },
            Err(e) => return Err(e.into()),
        };
        Ok(Store { rev: p.rev, next_txn: p.next_txn.max(1), entries: p.entries, history: p.history, path: Some(path) })
    }

    pub fn entries(&self) -> &BTreeMap<String, Entry> {
        &self.entries
    }

    pub fn get(&self, key: &str) -> Option<&Entry> {
        self.entries.get(key)
    }

    pub fn history(&self, limit: usize) -> Vec<Txn> {
        self.history.iter().rev().take(limit).cloned().collect()
    }

    /// Validate and durably apply a transaction. See the module header for the post-rename
    /// `durability_unconfirmed` exception: the transaction is adopted, not safe to retry.
    pub fn apply(&mut self, author: &str, note: Option<String>, ops: Vec<Op>) -> Result<Txn, RpcError> {
        self.apply_inner(author, note, None, ops)
    }

    fn apply_inner(&mut self, author: &str, note: Option<String>, reverts: Option<u64>, ops: Vec<Op>) -> Result<Txn, RpcError> {
        self.apply_with_writer(author, note, reverts, ops, write_atomic)
    }

    fn apply_with_writer(
        &mut self, author: &str, note: Option<String>, reverts: Option<u64>, ops: Vec<Op>,
        write: impl FnOnce(&Path, &[u8]) -> Result<(), AtomicWriteError>,
    ) -> Result<Txn, RpcError> {
        if ops.is_empty() {
            return Err(RpcError::invalid("empty transaction"));
        }
        let mut next = self.entries.clone();
        let mut rev = self.rev;
        let mut touched: BTreeMap<String, Option<Entry>> = BTreeMap::new();
        for op in ops {
            let (key, expect) = match &op {
                Op::Set { key, expect, .. } | Op::Delete { key, expect } => (key.clone(), *expect),
            };
            let current = next.get(&key).map(|e| e.version).unwrap_or(0);
            if let Some(want) = expect {
                if want != current {
                    return Err(RpcError::new("conflict", format!("{key} is at version {current}, expected {want}")));
                }
            }
            touched.entry(key.clone()).or_insert_with(|| self.entries.get(&key).cloned());
            match op {
                Op::Set { value, .. } => {
                    validate_entry(&key, &value)?;
                    rev += 1;
                    next.insert(key, Entry { version: rev, value });
                }
                Op::Delete { .. } => {
                    next.remove(&key);
                }
            }
        }
        validate_references(&next)?;

        let changes: Vec<Change> = touched
            .into_iter()
            .map(|(key, before)| Change { after: next.get(&key).cloned(), key, before })
            .filter(|c| c.before != c.after)
            .collect();
        let txn = Txn { id: self.next_txn, author: author.to_string(), at_ms: now_ms(), note, reverts, changes };
        let mut candidate = Persisted { rev, next_txn: self.next_txn + 1, entries: next, history: self.history.clone() };
        candidate.history.push_back(txn.clone());
        while candidate.history.len() > HISTORY {
            candidate.history.pop_front();
        }
        let mut uncertain = None;
        if let Some(path) = &self.path {
            let bytes = serde_json::to_vec_pretty(&candidate).map_err(|e| RpcError::new("io", format!("could not serialize workspace: {e}")))?;
            if let Err(e) = write(path, &bytes) {
                if !e.renamed {
                    return Err(RpcError::new("io", format!("could not persist workspace: {e}")));
                }
                let mut err = RpcError::new("durability_unconfirmed", format!("workspace transaction {} adopted after rename, but durability is unconfirmed: {e}; do not replay", txn.id));
                err.data.as_mut().unwrap()["txn"] = txn.id.into();
                uncertain = Some(err);
            }
        }
        self.rev = candidate.rev;
        self.next_txn = candidate.next_txn;
        self.entries = candidate.entries;
        self.history = candidate.history;
        match uncertain {
            Some(err) => Err(err),
            None => Ok(txn),
        }
    }

    /// Undo a transaction with a new one. Fails with `conflict` if any entry it touched has changed since.
    pub fn revert(&mut self, author: &str, txn_id: u64) -> Result<Txn, RpcError> {
        let txn = self
            .history
            .iter()
            .find(|t| t.id == txn_id)
            .cloned()
            .ok_or_else(|| RpcError::new("not_found", format!("no transaction {txn_id} in history")))?;
        let mut ops = Vec::new();
        for c in &txn.changes {
            let expect = Some(c.after.as_ref().map(|e| e.version).unwrap_or(0));
            ops.push(match &c.before {
                Some(b) => Op::Set { key: c.key.clone(), value: b.value.clone(), expect },
                None => Op::Delete { key: c.key.clone(), expect },
            });
        }
        if ops.is_empty() {
            return Err(RpcError::invalid(format!("transaction {txn_id} changed nothing")));
        }
        self.apply_inner(author, Some(format!("revert {txn_id}")), Some(txn_id), ops)
    }
}

/// `renamed` distinguishes a clean failure from an already-visible replacement whose durability
/// could not be confirmed. Never roll back memory or replay a write in the latter case.
#[derive(Debug)]
pub struct AtomicWriteError {
    pub renamed: bool,
    source: std::io::Error,
}

impl From<std::io::Error> for AtomicWriteError {
    fn from(source: std::io::Error) -> Self {
        Self { renamed: false, source }
    }
}

impl std::fmt::Display for AtomicWriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.source.fmt(f)
    }
}

impl std::error::Error for AtomicWriteError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

/// Provision state directories durably too: syncing a file's containing directory alone cannot
/// make a newly created ancestor's name survive a crash.
pub fn create_dir_all_durable(path: &Path) -> std::io::Result<()> {
    if path.is_dir() {
        return Ok(());
    }
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
    create_dir_all_durable(parent)?;
    match std::fs::create_dir(path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && path.is_dir() => {}
        Err(e) => return Err(e),
    }
    std::fs::File::open(path)?.sync_all()?;
    std::fs::File::open(parent)?.sync_all()
}

pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), AtomicWriteError> {
    write_atomic_with_sync(path, bytes, |dir| dir.sync_all())
}

fn write_atomic_with_sync(path: &Path, bytes: &[u8], sync_dir: impl FnOnce(&std::fs::File) -> std::io::Result<()>) -> Result<(), AtomicWriteError> {
    let dir = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
    create_dir_all_durable(dir)?;
    let directory = std::fs::File::open(dir)?;
    // Exclusive, randomly named files in the destination directory. The guard removes only our
    // own temp on any pre-rename failure, never another writer's file or a fixed `.tmp` path.
    let mut tmp = tempfile::Builder::new().prefix(".prim-").tempfile_in(dir)?;
    tmp.write_all(bytes)?;
    tmp.as_file().sync_all()?;
    tmp.persist(path).map_err(|e| e.error)?;
    sync_dir(&directory).map_err(|source| AtomicWriteError { renamed: true, source })
}

pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

fn invalid(key: &str, why: &str) -> RpcError {
    RpcError::invalid(format!("{key}: {why}"))
}

fn validate_entry(key: &str, v: &Value) -> Result<(), RpcError> {
    let (ns, name) = key.split_once('/').ok_or_else(|| invalid(key, "keys look like <namespace>/<name>"))?;
    if name.is_empty() {
        return Err(invalid(key, "empty name"));
    }
    match ns {
        "view" => {
            let kind = v.get("kind").and_then(|k| k.as_str()).ok_or_else(|| invalid(key, "a view needs a string `kind`"))?;
            if kind == "terminal" && v.get("resource").and_then(|r| r.as_str()).is_none() {
                return Err(invalid(key, "a terminal view needs a string `resource`"));
            }
            if kind == "panel" {
                if let Some(items) = v.get("items") {
                    let items = items.as_array().ok_or_else(|| invalid(key, "`items` must be an array"))?;
                    for it in items {
                        if it.get("text").and_then(|t| t.as_str()).is_none() {
                            return Err(invalid(key, "every panel item needs a string `text`"));
                        }
                    }
                }
            }
        }
        "layout" => validate_node(key, v, 0)?,
        "binding" => {
            let k = v.get("key").and_then(|k| k.as_str()).unwrap_or("");
            if k.is_empty() || v.get("command").and_then(|c| c.as_str()).is_none() {
                return Err(invalid(key, "a binding needs `key` and `command` strings"));
            }
        }
        "menu" => {
            for f in ["where", "label", "command"] {
                if v.get(f).and_then(|x| x.as_str()).is_none() {
                    return Err(invalid(key, "a menu item needs `where`, `label` and `command` strings"));
                }
            }
        }
        "extension" => {
            let argv = v.get("argv").and_then(|a| a.as_array()).ok_or_else(|| invalid(key, "an extension needs an `argv` array"))?;
            if argv.is_empty() || !argv.iter().all(|a| a.is_string()) {
                return Err(invalid(key, "`argv` must be a non-empty array of strings"));
            }
            if let Some(e) = v.get("enabled") {
                if !e.is_boolean() {
                    return Err(invalid(key, "`enabled` must be a boolean"));
                }
            }
        }
        "workspace" => {
            if name == "active" && v.get("layout").and_then(|l| l.as_str()).is_none() {
                return Err(invalid(key, "needs a `layout` string"));
            }
        }
        "doc" => {}
        _ => return Err(invalid(key, "unknown namespace (view, layout, binding, menu, extension, workspace, doc)")),
    }
    Ok(())
}

fn validate_node(key: &str, n: &Value, depth: usize) -> Result<(), RpcError> {
    if depth > 32 {
        return Err(invalid(key, "layout is nested too deeply"));
    }
    if n.get("view").and_then(|v| v.as_str()).is_some() {
        return Ok(());
    }
    if let Some(dir) = n.get("split") {
        if dir != "h" && dir != "v" {
            return Err(invalid(key, "`split` is \"h\" or \"v\""));
        }
        let children = n.get("children").and_then(|c| c.as_array()).ok_or_else(|| invalid(key, "a split needs `children`"))?;
        if children.is_empty() {
            return Err(invalid(key, "a split needs at least one child"));
        }
        if let Some(sizes) = n.get("sizes") {
            let ok = sizes.as_array().map(|s| s.len() == children.len() && s.iter().all(|x| x.as_u64().is_some())).unwrap_or(false);
            if !ok {
                return Err(invalid(key, "`sizes` must hold one integer weight per child"));
            }
        }
        return children.iter().try_for_each(|c| validate_node(key, c, depth + 1));
    }
    if let Some(tabs) = n.get("tabs") {
        let tabs = tabs.as_array().ok_or_else(|| invalid(key, "`tabs` must be an array"))?;
        if tabs.is_empty() {
            return Err(invalid(key, "`tabs` needs at least one tab"));
        }
        for t in tabs {
            let node = t.get("node").ok_or_else(|| invalid(key, "every tab needs a `node`"))?;
            validate_node(key, node, depth + 1)?;
        }
        return Ok(());
    }
    Err(invalid(key, "a layout node is {view}, {split, children} or {tabs}"))
}

/// Cross-entry consistency: layouts only reference existing views, the active layout exists.
fn validate_references(entries: &BTreeMap<String, Entry>) -> Result<(), RpcError> {
    for (key, e) in entries.range("layout/".to_string()..) {
        if !key.starts_with("layout/") {
            break;
        }
        let mut views = Vec::new();
        collect_views(&e.value, &mut views);
        for v in views {
            if !entries.contains_key(&format!("view/{v}")) {
                return Err(invalid(key, &format!("references missing view `{v}`")));
            }
        }
    }
    if let Some(active) = entries.get("workspace/active") {
        let name = active.value.get("layout").and_then(|l| l.as_str()).unwrap_or("");
        if !entries.contains_key(&format!("layout/{name}")) {
            return Err(invalid("workspace/active", &format!("layout `{name}` does not exist")));
        }
    }
    Ok(())
}

/// The first `tabs` node in a layout (depth-first), where new tabs go and digits select.
pub fn first_tabs_mut(n: &mut Value) -> Option<&mut Value> {
    if n.get("tabs").is_some() {
        return Some(n);
    }
    if let Some(children) = n.get_mut("children").and_then(|c| c.as_array_mut()) {
        for c in children {
            if let Some(t) = first_tabs_mut(c) {
                return Some(t);
            }
        }
    }
    None
}

pub fn collect_views(n: &Value, out: &mut Vec<String>) {
    if let Some(v) = n.get("view").and_then(|v| v.as_str()) {
        out.push(v.to_string());
    }
    if let Some(children) = n.get("children").and_then(|c| c.as_array()) {
        children.iter().for_each(|c| collect_views(c, out));
    }
    if let Some(tabs) = n.get("tabs").and_then(|t| t.as_array()) {
        tabs.iter().filter_map(|t| t.get("node")).for_each(|c| collect_views(c, out));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn set(key: &str, value: Value) -> Op {
        Op::Set { key: key.into(), value, expect: None }
    }

    #[test]
    fn transaction_is_all_or_nothing() {
        let mut s = Store::in_memory();
        let err = s
            .apply("t", None, vec![set("view/a", json!({"kind": "panel"})), set("layout/main", json!({"view": "missing"}))])
            .unwrap_err();
        assert_eq!(err.kind(), "invalid");
        assert!(s.entries().is_empty(), "nothing applied");
    }

    #[test]
    fn compare_and_set() {
        let mut s = Store::in_memory();
        s.apply("t", None, vec![Op::Set { key: "doc/x/y".into(), value: json!(1), expect: Some(0) }]).unwrap();
        let v = s.get("doc/x/y").unwrap().version;
        let err = s.apply("t", None, vec![Op::Set { key: "doc/x/y".into(), value: json!(2), expect: Some(v + 1) }]).unwrap_err();
        assert_eq!(err.kind(), "conflict");
        s.apply("t", None, vec![Op::Set { key: "doc/x/y".into(), value: json!(2), expect: Some(v) }]).unwrap();
        assert_eq!(s.get("doc/x/y").unwrap().value, json!(2));
    }

    #[test]
    fn cannot_delete_a_view_a_layout_uses() {
        let mut s = Store::in_memory();
        s.apply("t", None, vec![set("view/a", json!({"kind": "panel"})), set("layout/main", json!({"view": "a"}))]).unwrap();
        let err = s.apply("t", None, vec![Op::Delete { key: "view/a".into(), expect: None }]).unwrap_err();
        assert_eq!(err.kind(), "invalid");
    }

    #[test]
    fn revert_restores_and_detects_conflicts() {
        let mut s = Store::in_memory();
        s.apply("human", None, vec![set("view/a", json!({"kind": "panel", "title": "one"}))]).unwrap();
        let t2 = s.apply("agent", None, vec![set("view/a", json!({"kind": "panel", "title": "two"})), set("view/b", json!({"kind": "panel"}))]).unwrap();
        let r = s.revert("human", t2.id).unwrap();
        assert_eq!(r.reverts, Some(t2.id));
        assert_eq!(s.get("view/a").unwrap().value["title"], "one");
        assert!(s.get("view/b").is_none());
        // Reverting again conflicts: the entries moved on.
        assert_eq!(s.revert("human", t2.id).unwrap_err().kind(), "conflict");
    }

    #[test]
    fn failed_write_leaves_everything_unchanged_and_can_retry() {
        let dir = tempfile::tempdir().unwrap();
        let parent = dir.path().join("state");
        std::fs::create_dir(&parent).unwrap();
        let path = parent.join("ws.json");
        let mut s = Store::open(path.clone()).unwrap();
        s.apply("t", None, vec![set("doc/a/b", json!(1))]).unwrap();
        let before_entries = s.entries.clone();
        let before_history = serde_json::to_value(&s.history).unwrap();
        let (rev, next_txn) = (s.rev, s.next_txn);
        let saved = dir.path().join("saved");
        std::fs::rename(&parent, &saved).unwrap();
        std::fs::write(&parent, b"not a directory").unwrap();

        let err = s.apply("t", None, vec![set("doc/a/b", json!(2))]).unwrap_err();
        assert_eq!(err.kind(), "io");
        assert_eq!(s.entries, before_entries);
        assert_eq!(serde_json::to_value(&s.history).unwrap(), before_history);
        assert_eq!((s.rev, s.next_txn), (rev, next_txn));

        std::fs::remove_file(&parent).unwrap();
        std::fs::rename(saved, &parent).unwrap();
        let txn = s.apply("t", None, vec![set("doc/a/b", json!(2))]).unwrap();
        assert_eq!(txn.id, next_txn);
        assert_eq!(s.get("doc/a/b").unwrap().version, rev + 1);
        assert_eq!(Store::open(path).unwrap().entries, s.entries);
    }

    #[test]
    fn directory_sync_failure_adopts_candidate_and_reports_uncertainty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ws.json");
        let mut s = Store::open(path.clone()).unwrap();
        let err = s.apply_with_writer("t", None, None, vec![set("doc/a/b", json!(1))], |path, bytes| {
            write_atomic_with_sync(path, bytes, |_| Err(std::io::Error::other("injected directory sync failure")))
        }).unwrap_err();
        assert_eq!(err.kind(), "durability_unconfirmed");
        assert_eq!(err.data.as_ref().unwrap()["txn"], 1);
        assert_eq!((s.rev, s.next_txn), (1, 2));
        assert_eq!(s.history(10).len(), 1);
        assert_eq!(Store::open(path).unwrap().entries, s.entries);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1, "no stale temp after rename");
        let mut core = crate::core::Core::new(s, None, dir.path().join("unused.sock"), false).unwrap();
        assert_eq!(core.finish_txn(Err(err)).unwrap_err().kind(), "durability_unconfirmed");
        let events = core.events_since(0).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["type"], "entries");
        assert_eq!(events[0]["data"]["txn"], 1);
        let txn = core.store.apply("t", None, vec![set("doc/a/b", json!(2))]).unwrap();
        assert_eq!(txn.id, 2);
    }

    #[test]
    fn atomic_writer_cleans_failed_temps_without_touching_other_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ws.json");
        std::fs::create_dir(&path).unwrap(); // rename cannot replace a directory with a file
        let legacy = path.with_extension("tmp");
        std::fs::write(&legacy, b"not ours").unwrap();
        let err = write_atomic(&path, b"candidate").unwrap_err();
        assert!(!err.renamed);
        assert!(path.is_dir());
        assert_eq!(std::fs::read(&legacy).unwrap(), b"not ours");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2, "our failed temp was removed");
        std::fs::remove_dir(&path).unwrap();
        write_atomic(&path, b"committed").unwrap();
        assert_eq!(std::fs::read(path).unwrap(), b"committed");
        assert_eq!(std::fs::read(legacy).unwrap(), b"not ours");
    }

    #[test]
    fn persists_and_reloads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ws.json");
        let mut s = Store::open(path.clone()).unwrap();
        s.apply("t", None, vec![set("doc/a/b", json!({"n": 1}))]).unwrap();
        let s2 = Store::open(path).unwrap();
        assert_eq!(s2.get("doc/a/b").unwrap().value, json!({"n": 1}));
        assert_eq!(s2.history(10).len(), 1);
    }

    #[test]
    fn rejects_unknown_namespace_and_bad_layout() {
        let mut s = Store::in_memory();
        assert_eq!(s.apply("t", None, vec![set("nope/x", json!(1))]).unwrap_err().kind(), "invalid");
        assert_eq!(s.apply("t", None, vec![set("layout/x", json!({"split": "x", "children": []}))]).unwrap_err().kind(), "invalid");
    }
}
