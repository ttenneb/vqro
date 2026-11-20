//! The workspace store: vqro's authoritative workspace state (D15).
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
use serde_json::{json,Value};

use crate::rpc::RpcError;

const HISTORY: usize = 500;
pub const HISTORY_BYTES: usize = 8 * 1024 * 1024;
pub const NOTE_BYTES: usize = 4096;
/// How long a starting core waits for the state lock. flock locks belong to the open file
/// description: any child forked while a core holds the lock (by that core, an extension spawn,
/// or another test in the same process) shares it until its exec, even with CLOEXEC, and a
/// stopping core's late durable writer holds it until it finishes. Both are brief, so a
/// replacement waits a bounded time instead of failing at once; a lock still held after this is a
/// live core or a stuck writer and fails as before. Process-associated fcntl locks would avoid
/// fork inheritance but cannot fence two cores in one process and drop on any close.
pub const STATE_LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(3);
pub(crate) fn validate_note(note:Option<&str>)->Result<(),RpcError> {
    if let Some(note)=note {
        if note.len()>NOTE_BYTES {
            let mut error=crate::limits::too_large("transaction note",note.len(),NOTE_BYTES);
            error.message=format!("transaction note uses {} UTF-8 bytes; limit is {NOTE_BYTES}; write not applied",note.len());
            error.data.as_mut().unwrap()["unit"]=json!("utf8_bytes");
            return Err(error);
        }
    }
    Ok(())
}
type SharedEntry = std::sync::Arc<Entry>;
pub type Entries = BTreeMap<String, SharedEntry>;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Entry {
    pub version: u64,
    pub value: Value,
    #[serde(skip)] depth_quarantined:bool,
}
impl Entry {
    pub(crate) fn new(version:u64,value:Value)->Self {Self {version,value,depth_quarantined:false}}
    pub(crate) fn quarantined(&self)->bool {self.depth_quarantined}
}
impl<'de> Deserialize<'de> for Entry {
    fn deserialize<D:serde::Deserializer<'de>>(deserializer:D)->Result<Self,D::Error> {
        #[derive(Deserialize)] struct Wire {version:u64,value:Box<serde_json::value::RawValue>}
        let raw=Wire::deserialize(deserializer)?;
        let depth_quarantined=!crate::limits::raw_depth_within(raw.value.get(),crate::limits::JSON_DEPTH);
        let value=if depth_quarantined {Value::Null} else {serde_json::from_str(raw.value.get()).map_err(serde::de::Error::custom)?};
        Ok(Self {version:raw.version,value,depth_quarantined})
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Change {
    pub key: String,
    pub before: Option<SharedEntry>,
    pub after_version: Option<u64>,
}
impl<'de> Deserialize<'de> for Change {
    fn deserialize<D:serde::Deserializer<'de>>(deserializer:D)->Result<Self,D::Error> {
        // Legacy after.value is ignored while parsing, not allocated just to discard it.
        #[derive(Deserialize)] struct Version { version:u64 }
        #[derive(Deserialize)] struct Wire { key:String, before:Option<SharedEntry>, #[serde(default)] after_version:Option<u64>, #[serde(default)] after:Option<Version> }
        let wire=Wire::deserialize(deserializer)?;
        Ok(Self {key:wire.key,before:wire.before,after_version:wire.after_version.or(wire.after.map(|entry|entry.version))})
    }
}
fn yes()->bool { true }
fn is_true(value:&bool)->bool { *value }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Txn {
    pub id: u64,
    pub author: String,
    #[serde(flatten)]
    pub attribution: crate::principal::Attribution,
    /// Original command invoker, validated before commit admission; writer stays in author.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_behalf_of: Option<String>,
    pub at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reverts: Option<u64>,
    pub changes: Vec<Change>,
    #[serde(default="yes", skip_serializing_if="is_true")]
    pub undoable: bool,
    #[serde(skip)]
    encoded_bytes: usize,
    #[serde(skip)]
    revert_bytes: usize,
    #[serde(skip)] depth_quarantined:bool,
}

#[derive(Serialize)]
#[serde(tag="op",rename_all="snake_case")]
enum UndoRef<'a> {
    Set { key:&'a str, value:&'a Value, expect:u64 },
    Delete { key:&'a str, expect:u64 },
}
fn retained_txn(mut txn:Txn, legacy:bool)->Result<std::sync::Arc<Txn>,RpcError> {
    if txn.changes.iter().any(|change|change.before.as_ref().is_some_and(|entry|entry.depth_quarantined)) {
        txn.depth_quarantined=true;txn.undoable=false;
        for change in &mut txn.changes {change.before=None;}
    }
    if legacy {
        if let Some(note)=&txn.note {
            if note.len()>NOTE_BYTES {
                let suffix="... [truncated]";
                let mut end=NOTE_BYTES-suffix.len(); while !note.is_char_boundary(end) {end-=1;}
                txn.note=Some(format!("{}{suffix}",&note[..end]));
            }
        }
    }
    let mut bytes=crate::limits::encoded_bytes(&txn);
    if !txn.undoable || bytes.saturating_add(2)>HISTORY_BYTES {
        txn.undoable=false;
        for change in &mut txn.changes {change.before=None;}
        bytes=crate::limits::encoded_bytes(&txn);
    }
    txn.encoded_bytes=bytes;
    if txn.encoded_bytes.saturating_add(2)>HISTORY_BYTES {
        return Err(crate::limits::too_large("transaction metadata",txn.encoded_bytes,HISTORY_BYTES-2));
    }
    if txn.undoable {
        let undo:Vec<_>=txn.changes.iter().map(|change|match &change.before {
            Some(before)=>UndoRef::Set {key:&change.key,value:&before.value,expect:change.after_version.unwrap_or(0)},
            None=>UndoRef::Delete {key:&change.key,expect:change.after_version.unwrap_or(0)},
        }).collect();
        txn.revert_bytes=crate::limits::encoded_bytes(&undo);
    }
    Ok(std::sync::Arc::new(txn))
}

#[derive(Clone)]
struct History { txns:VecDeque<std::sync::Arc<Txn>>, bytes:usize,depth_quarantined:usize }
impl Default for History {fn default()->Self {Self {txns:VecDeque::new(),bytes:2,depth_quarantined:0}}}
impl History {
    fn push(&mut self,txn:std::sync::Arc<Txn>) {
        self.bytes+=txn.encoded_bytes+usize::from(!self.txns.is_empty());
        self.txns.push_back(txn);
        while self.txns.len()>HISTORY || self.bytes>HISTORY_BYTES {
            self.bytes-=self.txns.pop_front().unwrap().encoded_bytes+usize::from(!self.txns.is_empty());
        }
    }
}
impl Serialize for History {
    fn serialize<S:serde::Serializer>(&self,serializer:S)->Result<S::Ok,S::Error> {serializer.collect_seq(self.txns.iter())}
}
impl<'de> Deserialize<'de> for History {
    fn deserialize<D:serde::Deserializer<'de>>(deserializer:D)->Result<Self,D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value=History;
            fn expecting(&self,f:&mut std::fmt::Formatter)->std::fmt::Result {write!(f,"workspace transaction history")}
            fn visit_seq<A:serde::de::SeqAccess<'de>>(self,mut seq:A)->Result<History,A::Error> {
                let mut history=History::default();
                while let Some(txn)=seq.next_element::<Txn>()? {
                    let unsafe_before=txn.changes.iter().any(|change|change.before.as_ref().is_some_and(|entry|entry.depth_quarantined));
                    match retained_txn(txn,true) {
                        Ok(txn)=>{if txn.depth_quarantined {history.depth_quarantined+=1;}history.push(txn);},
                        // An unretainable legacy metadata record is an undo boundary: never
                        // silently undo an older change across it. New writes reject it instead.
                        Err(_)=>{let recovered=history.depth_quarantined+usize::from(unsafe_before);history=History::default();history.depth_quarantined=recovered;}
                    }
                }
                Ok(history)
            }
        }
        deserializer.deserialize_seq(Visitor)
    }
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
    entries: Entries,
    history: History,
    #[serde(default,skip_serializing_if="Option::is_none")] recovery:Option<RecoveryInfo>,
}

#[derive(Clone,Serialize,Deserialize)]
struct RecoveryInfo {note:String,backup:Option<PathBuf>}
struct Recovery {info:RecoveryInfo,preserve_source:bool,backup:std::sync::OnceLock<PathBuf>}
impl Recovery {
    fn persisted(&self)->RecoveryInfo {RecoveryInfo {note:self.info.note.clone(),backup:self.backup.get().cloned().or_else(||self.info.backup.clone())}}
}

// Back up raw source bytes, not a sanitized re-encoding. Failure never replaces the original.
fn backup_quarantine(path:&Path)->std::io::Result<PathBuf> {
    let parent=path.parent().filter(|p|!p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    // Same private0600/exclusive tempfile mechanism as durable workspace replacement.
    let mut backup=tempfile::Builder::new().prefix("workspace.quarantine-").suffix(".json").tempfile_in(parent)?;
    std::io::copy(&mut std::fs::File::open(path)?,&mut backup)?;
    backup.as_file().sync_all()?;
    // The exclusive final pathname already exists; keep only disarms tempfile cleanup.
    // If the directory barrier fails, the guard removes our copy and the original is untouched.
    std::fs::File::open(parent)?.sync_all()?;
    let (_file,backup)=backup.keep().map_err(|error|error.error)?;
    Ok(backup)
}

/// In-process durable writer injection for deterministic fault/latency harnesses, not a wire API.
pub type DurableWriter = std::sync::Arc<dyn Fn(&Path, &[u8]) -> Result<(), AtomicWriteError> + Send + Sync>;

pub struct Store {
    rev: u64,
    next_txn: u64,
    entries: std::sync::Arc<Entries>,
    history: std::sync::Arc<History>,
    path: Option<PathBuf>,
    writer: DurableWriter,
    // Copied into dispatched writers: a late replace must fence replacement core startup.
    lifetime:Option<std::sync::Arc<std::fs::File>>,
    recovery:Option<std::sync::Arc<Recovery>>,
}

impl Store {
    pub fn in_memory() -> Store {
        Store { rev: 0, next_txn: 1, entries: std::sync::Arc::new(BTreeMap::new()), history: std::sync::Arc::new(History::default()), path: None, writer: std::sync::Arc::new(write_atomic), lifetime:None,recovery:None }
    }

    /// Load from a file, or start empty if it does not exist.
    pub fn open(path: PathBuf) -> anyhow::Result<Store> {
        let mut p: Persisted = match std::fs::File::open(&path) {
            Ok(file) => serde_json::from_reader(std::io::BufReader::new(file))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Persisted { next_txn: 1, ..Default::default() },
            Err(e) => return Err(e.into()),
        };
        let quarantined=p.entries.iter().filter(|(_,entry)|entry.depth_quarantined).map(|(key,_)|key.clone()).collect::<Vec<_>>();
        let panel=json!({"kind":"panel","title":"Quarantined view","items":[{"type":"text","style":"warn","text":"Unsafe legacy JSON quarantined. Use recovery commands to repair this view/layout; original data is preserved."}]});
        let mut recovery_view=String::from("vqro-depth-quarantine");
        if quarantined.iter().any(|key|key.starts_with("layout/")) {
            while p.entries.contains_key(&format!("view/{recovery_view}")) {recovery_view.push('-');}
            p.entries.insert(format!("view/{recovery_view}"),std::sync::Arc::new(Entry {version:p.rev.max(1),value:panel.clone(),depth_quarantined:false}));
        }
        for key in &quarantined {
            // Keep dependent healthy layouts/active selections valid, without interpreting an
            // unsafe tree. Only the quarantined declaration becomes a recovery placeholder.
            let replacement=if key.starts_with("view/") {Some(panel.clone())} else if key.starts_with("layout/") {Some(json!({"view":recovery_view}))} else {None};
            if let Some(value)=replacement {let version=p.entries[key].version;p.entries.insert(key.clone(),std::sync::Arc::new(Entry {version,value,depth_quarantined:false}));} else {p.entries.remove(key);}
        }
        let unsafe_history=p.history.depth_quarantined;
        let recovery=if !quarantined.is_empty() || unsafe_history>0 {
            let keys=quarantined.iter().take(5).map(|key|key.chars().take(120).collect::<String>()).collect::<Vec<_>>().join(", ");
            let note=format!("Recovered over-deep JSON: {} entries quarantined ({keys}); {unsafe_history} unsafe undo records disabled. Other entries are unchanged. Repair affected layouts with recovery commands if necessary.",quarantined.len());
            eprintln!("vqro: {note}");
            Some(std::sync::Arc::new(Recovery {info:RecoveryInfo {note,backup:None},preserve_source:true,backup:Default::default()}))
        } else {p.recovery.map(|info|std::sync::Arc::new(Recovery {info,preserve_source:false,backup:Default::default()}))};
        Ok(Store { rev: p.rev, next_txn: p.next_txn.max(1), entries: std::sync::Arc::new(p.entries), history: std::sync::Arc::new(p.history), path: Some(path), writer: std::sync::Arc::new(write_atomic), lifetime:None,recovery })
    }

    /// Acquire before reading state; blocking writers retain the same process-held lock.
    /// A core start waits up to `STATE_LOCK_WAIT` for a previous holder to let go (see there).
    pub(crate) fn open_locked(path:PathBuf) -> anyhow::Result<Store> {
        Self::open_locked_within(path, STATE_LOCK_WAIT)
    }

    pub(crate) fn open_locked_within(path:PathBuf, wait:std::time::Duration) -> anyhow::Result<Store> {
        let lock=Self::lock_state_within(&path, wait)?;
        let mut store=Self::open(path)?;
        store.lifetime=Some(lock);
        Ok(store)
    }

    /// Immediate probe (the TUI's "is a core using this state?" check): never waits.
    pub(crate) fn lock_state(path:&Path) -> anyhow::Result<std::sync::Arc<std::fs::File>> {
        Self::lock_state_within(path, std::time::Duration::ZERO)
    }

    pub(crate) fn lock_state_within(path:&Path, wait:std::time::Duration) -> anyhow::Result<std::sync::Arc<std::fs::File>> {
        let lock=std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(path.with_file_name(".workspace.lock"))?;
        let deadline=std::time::Instant::now()+wait;
        loop {
            match lock.try_lock() {
                Ok(()) => return Ok(std::sync::Arc::new(lock)),
                Err(std::fs::TryLockError::WouldBlock) if std::time::Instant::now()<deadline => std::thread::sleep(std::time::Duration::from_millis(10)),
                Err(error) => return Err(anyhow::anyhow!("workspace state still in use (possibly a late durable write); wait for the previous core to finish, do not replay: {error}")),
            }
        }
    }

    pub(crate) fn release_state(&mut self) { self.lifetime=None; }
    pub(crate) fn durable_context(&self)->(DurableWriter,Option<std::sync::Arc<std::fs::File>>) {(self.writer.clone(),self.lifetime.clone())}

    pub(crate) fn set_writer(&mut self, writer: DurableWriter) { self.writer = writer; }

    /// O(1) immutable committed-state copy. Only the serialized committer may persist it.
    pub(crate) fn committed_copy(&self) -> Self {
        Self { rev:self.rev, next_txn:self.next_txn, entries:self.entries.clone(), history:self.history.clone(), path:self.path.clone(), writer:self.writer.clone(), lifetime:self.lifetime.clone(),recovery:self.recovery.clone() }
    }

    pub(crate) fn transaction(&self, id:u64) -> Option<&Txn> { self.history.txns.iter().find(|t| t.id == id).map(|txn|txn.as_ref()) }
    pub(crate) fn revert_bytes(&self,id:u64)->Option<usize> {self.transaction(id).map(|txn|txn.revert_bytes)}
    pub fn history_bytes(&self)->usize {self.history.bytes}

    pub(crate) fn latest_txn_matching(&self, predicate: impl Fn(&Txn) -> bool) -> Option<u64> {
        self.history.txns.iter().rev().find(|txn| predicate(txn)).map(|txn| txn.id)
    }

    pub(crate) fn is_pristine(&self)->bool {self.rev==0 && self.next_txn==1 && self.entries.is_empty() && self.history.txns.is_empty()}

    pub fn entries(&self) -> &Entries {
        &self.entries
    }

    pub(crate) fn shared_entries(&self)->std::sync::Arc<Entries> {self.entries.clone()}

    pub fn admission_notice(&self) -> Option<&'static str> {Self::entries_notice(&self.entries)}
    pub(crate) fn recovery_notice(&self)->Option<String> {self.recovery.as_ref().map(|recovery| {
        let info=recovery.persisted();match info.backup {Some(path)=>format!("{} Raw source backup: {}",info.note,path.display()),None=>format!("{} Original workspace retained; a raw quarantine backup is required before the next write.",info.note)}
    })}

    pub(crate) fn entries_notice(entries:&Entries)->Option<&'static str> {
        (crate::limits::encoded_bytes(entries) > crate::limits::WORKSPACE_BYTES ||
            entries.iter().any(|item| crate::limits::encoded_bytes(&item) > crate::limits::ENTRY_BYTES))
            .then_some("Legacy workspace exceeds current size limits; it remains loaded. Revert/delete/shrink state to recover.")
    }

    pub fn get(&self, key: &str) -> Option<&Entry> {
        self.entries.get(key).map(|entry|entry.as_ref())
    }

    pub fn history(&self, limit: usize) -> Vec<std::sync::Arc<Txn>> {
        self.history.txns.iter().rev().take(limit).cloned().collect()
    }

    /// Validate and durably apply a transaction. See the module header for the post-rename
    /// `durability_unconfirmed` exception: the transaction is adopted, not safe to retry.
    pub fn apply(&mut self, author: &str, note: Option<String>, ops: Vec<Op>) -> Result<std::sync::Arc<Txn>, RpcError> {
        self.apply_attributed(author, None, note, ops)
    }

    pub fn apply_attributed(&mut self, author: &str, on_behalf_of: Option<&str>, note: Option<String>, ops: Vec<Op>) -> Result<std::sync::Arc<Txn>, RpcError> {
        self.apply_context(author,on_behalf_of,crate::principal::Attribution::default(),note,ops)
    }

    pub fn apply_context(&mut self, author:&str, on_behalf_of:Option<&str>, attribution:crate::principal::Attribution, note:Option<String>, ops:Vec<Op>)->Result<std::sync::Arc<Txn>,RpcError> {
        self.apply_inner(author,on_behalf_of,attribution,note,None,ops)
    }

    fn apply_inner(&mut self, author: &str, on_behalf_of: Option<&str>, attribution:crate::principal::Attribution, note: Option<String>, reverts: Option<u64>, ops: Vec<Op>) -> Result<std::sync::Arc<Txn>, RpcError> {
        let writer = self.writer.clone();
        self.apply_with_writer(author, on_behalf_of, note, reverts, ops, &attribution, |path, bytes| writer(path, bytes))
    }

    fn apply_with_writer(
        &mut self, author: &str, on_behalf_of: Option<&str>, note: Option<String>, reverts: Option<u64>, ops: Vec<Op>, attribution:&crate::principal::Attribution,
        write: impl FnOnce(&Path, &[u8]) -> Result<(), AtomicWriteError>,
    ) -> Result<std::sync::Arc<Txn>, RpcError> {
        validate_note(note.as_deref())?;
        if ops.is_empty() {
            return Err(RpcError::invalid("empty transaction"));
        }
        let mut next = self.entries.as_ref().clone();
        let mut rev = self.rev;
        let mut touched: BTreeMap<String, Option<SharedEntry>> = BTreeMap::new();
        for op in ops {
            let (key, expect) = match &op {
                Op::Set { key, expect, .. } | Op::Delete { key, expect } => (key.clone(), *expect),
            };
            if crate::operational::protected_key(&key) {return Err(RpcError::new("forbidden","protected operational keys cannot be changed by workspace writes or reverts"));}
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
                    next.insert(key, std::sync::Arc::new(Entry { version: rev, value,depth_quarantined:false }));
                }
                Op::Delete { .. } => {
                    next.remove(&key);
                }
            }
        }
        validate_references(&next)?;
        // Old oversized state remains loadable and shrinkable. Limits reject growth, never
        // block deletion/revert that reduces a legacy violation.
        for (key, entry) in &next {
            let bytes = crate::limits::encoded_bytes(&(key, entry));
            if bytes > crate::limits::ENTRY_BYTES {
                let previous = self.entries.get(key).map(|old| crate::limits::encoded_bytes(&(key, old))).unwrap_or(0);
                if bytes > previous { return Err(crate::limits::too_large("workspace entry", bytes, crate::limits::ENTRY_BYTES)); }
            }
        }
        let bytes = crate::limits::encoded_bytes(&next);
        if bytes > crate::limits::WORKSPACE_BYTES && bytes > crate::limits::encoded_bytes(self.entries.as_ref()) {
            return Err(crate::limits::too_large("workspace entries", bytes, crate::limits::WORKSPACE_BYTES));
        }

        let changes: Vec<Change> = touched
            .into_iter()
            .map(|(key, before)| Change { after_version: next.get(&key).map(|entry|entry.version), key, before })
            .filter(|change| change.before.as_ref().map(|entry|entry.version) != change.after_version)
            .collect();
        let txn = retained_txn(Txn { id: self.next_txn, author: author.to_string(), attribution:attribution.clone().recorded(), on_behalf_of:on_behalf_of.map(String::from), at_ms: now_ms(), note, reverts, changes, undoable:true, encoded_bytes:0, revert_bytes:0,depth_quarantined:false },false)?;
        if let (Some(recovery),Some(path))=(&self.recovery,&self.path) {
            if recovery.preserve_source && recovery.backup.get().is_none() {
                let backup=backup_quarantine(path).map_err(|error| {
                    let mut error=RpcError::new("io",format!("quarantine backup failed; original workspace retained, write not applied: {error}"));
                    error.data.as_mut().unwrap()["outcome"]=json!("rejected");error
                })?;
                let _=recovery.backup.set(backup);
            }
        }
        let mut candidate = Persisted { rev, next_txn: self.next_txn + 1, entries: next, history: self.history.as_ref().clone(),recovery:self.recovery.as_ref().map(|r|r.persisted()) };
        candidate.history.push(txn.clone());
        let mut uncertain = None;
        if let Some(path) = &self.path {
            let bytes = serde_json::to_vec(&candidate).map_err(|e| RpcError::new("io", format!("could not serialize workspace: {e}")))?;
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
        self.entries = std::sync::Arc::new(candidate.entries);
        self.history = std::sync::Arc::new(candidate.history);
        match uncertain {
            Some(err) => Err(err),
            None => Ok(txn),
        }
    }

    /// Undo a transaction with a new one. Fails with `conflict` if any entry it touched has changed since.
    pub fn revert(&mut self, author: &str, txn_id: u64) -> Result<std::sync::Arc<Txn>, RpcError> {
        self.revert_attributed(author, None, txn_id)
    }

    pub fn revert_attributed(&mut self, author: &str, on_behalf_of: Option<&str>, txn_id: u64) -> Result<std::sync::Arc<Txn>, RpcError> {
        self.revert_context(author,on_behalf_of,crate::principal::Attribution::default(),txn_id)
    }

    pub fn revert_context(&mut self, author:&str, on_behalf_of:Option<&str>, attribution:crate::principal::Attribution, txn_id:u64)->Result<std::sync::Arc<Txn>,RpcError> {
        let txn = self
            .history.txns
            .iter()
            .find(|t| t.id == txn_id)
            .cloned()
            .ok_or_else(|| RpcError::new("not_found", format!("no transaction {txn_id} in history")))?;
        if !txn.undoable {
            let mut error=RpcError::new("unavailable",format!("transaction {txn_id} exceeds the undo history byte horizon; undo data not retained"));
            let data=error.data.as_mut().unwrap();
            data["outcome"]=json!("rejected");data["txn"]=json!(txn_id);data["undoable"]=json!(false);
            return Err(error);
        }
        let mut ops = Vec::new();
        for c in &txn.changes {
            let expect = Some(c.after_version.unwrap_or(0));
            ops.push(match &c.before {
                Some(b) => Op::Set { key: c.key.clone(), value: b.value.clone(), expect },
                None => Op::Delete { key: c.key.clone(), expect },
            });
        }
        if ops.is_empty() {
            return Err(RpcError::invalid(format!("transaction {txn_id} changed nothing")));
        }
        self.apply_inner(author, on_behalf_of, attribution, Some(format!("revert {txn_id}")), Some(txn_id), ops)
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
    let mut tmp = tempfile::Builder::new().prefix(".vqro-").tempfile_in(dir)?;
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
    crate::limits::json_depth(v,crate::limits::JSON_DEPTH)?;
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
fn validate_references(entries: &Entries) -> Result<(), RpcError> {
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
    fn recovery_backup_precedes_write_and_survives_a_discarded_writer_snapshot() {
        let root=tempfile::tempdir().unwrap();let path=root.path().join("workspace.json");
        let original=format!(r#"{{"rev":2,"next_txn":2,"entries":{{"doc/deep":{{"version":2,"value":{}0{}}}}},"history":[]}}"#,"[".repeat(123),"]".repeat(123));
        std::fs::write(&path,&original).unwrap();let mut store=Store::open(path.clone()).unwrap();
        let mut no_backup=store.committed_copy();no_backup.path=Some(root.path().join("missing/workspace.json"));
        no_backup.writer=std::sync::Arc::new(|_,_|panic!("writer reached without raw source backup"));
        let error=no_backup.apply("user",None,vec![set("doc/new",json!(1))]).unwrap_err();assert_eq!(error.data.unwrap()["outcome"],"rejected");
        assert_eq!(std::fs::read_to_string(&path).unwrap(),original);
        let mut failed=store.committed_copy();
        failed.writer=std::sync::Arc::new(|_,_|Err(AtomicWriteError {renamed:false,source:std::io::Error::other("injected pre-rename failure")}));
        assert!(failed.apply("user",None,vec![set("doc/new",json!(1))]).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(),original);drop(failed);
        store.apply("user",None,vec![set("doc/new",json!(2))]).unwrap();
        let backups=std::fs::read_dir(root.path()).unwrap().filter_map(|entry|{let path=entry.unwrap().path();path.file_name().unwrap().to_string_lossy().starts_with("workspace.quarantine-").then_some(path)}).collect::<Vec<_>>();
        assert_eq!(backups.len(),1,"discarded commits do not recopy the same raw original");assert_eq!(std::fs::read_to_string(&backups[0]).unwrap(),original);
    }

    #[test]
    fn replacement_core_is_fenced_until_dispatched_writer_snapshot_drops() {
        let root=tempfile::tempdir().unwrap();let path=root.path().join("workspace.json");
        let store=Store::open_locked(path.clone()).unwrap();
        let writer=store.committed_copy();drop(store);
        assert!(Store::open_locked_within(path.clone(),std::time::Duration::ZERO).is_err(),"a late old writer must fence replacement startup");
        drop(writer);
        // A child forked meanwhile (other tests spawn) may share the flock until its exec:
        // open_locked's bounded wait covers that window.
        Store::open_locked(path.clone()).expect("the lock is free once the last writer handle drops");
    }

    #[test]
    fn a_starting_core_waits_out_a_briefly_shared_lock_but_not_a_held_one() {
        // Regression for the recurring "workspace state still in use" test flake: a forked child
        // shares the lock until its exec; the replacement must wait for it, not fail at once.
        let root=tempfile::tempdir().unwrap();let path=root.path().join("workspace.json");
        let old=Store::open_locked(path.clone()).unwrap();
        let shared=old.lifetime.clone().unwrap();  // stands in for the forked child's copy of the fd
        drop(old);
        let release=std::thread::spawn(move||{std::thread::sleep(std::time::Duration::from_millis(300));drop(shared);});
        let started=std::time::Instant::now();
        let replacement=Store::open_locked(path.clone());
        assert!(replacement.is_ok(),"{:?}",replacement.as_ref().err());
        assert!(started.elapsed()>=std::time::Duration::from_millis(250),"it waited for the release");
        release.join().unwrap();
        // Still a real fence: a lock held past the bound fails, and the TUI's probe never waits.
        let started=std::time::Instant::now();
        assert!(Store::open_locked_within(path.clone(),std::time::Duration::from_millis(200)).is_err());
        assert!(started.elapsed()<std::time::Duration::from_secs(2));
        assert!(Store::lock_state(&path).is_err());
        drop(replacement);
        assert!(Store::lock_state(&path).is_ok());
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
        let before_history = serde_json::to_value(s.history.as_ref()).unwrap();
        let (rev, next_txn) = (s.rev, s.next_txn);
        let saved = dir.path().join("saved");
        std::fs::rename(&parent, &saved).unwrap();
        std::fs::write(&parent, b"not a directory").unwrap();

        let err = s.apply("t", None, vec![set("doc/a/b", json!(2))]).unwrap_err();
        assert_eq!(err.kind(), "io");
        assert_eq!(s.entries, before_entries);
        assert_eq!(serde_json::to_value(s.history.as_ref()).unwrap(), before_history);
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
        let err = s.apply_with_writer("t", None, None, None, vec![set("doc/a/b", json!(1))], &crate::principal::Attribution::default(), |path, bytes| {
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
