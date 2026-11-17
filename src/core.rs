//! The prim core: workspace store, resources from adapters, the event log, commands and extensions.
//!
//! Shared as `Arc<Mutex<Core>>`. The lock is never held across an await.

use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::{broadcast, mpsc};

use crate::adapter::{AdapterEvent, AdapterHandle, ResourceFacts};
use crate::ext::ExtRuntime;
use crate::rpc::{Peer, RpcError};
use crate::store::{Store, Txn};

pub type Shared = Arc<Mutex<Core>>;

const LOG: usize = 10_000;

#[derive(Default, Serialize, Deserialize)]
struct ResourceMap {
    next: u64,
    /// prim id -> (adapter, backend ref). Keeps ids stable across core restarts.
    ids: BTreeMap<String, (String, String)>,
}

#[derive(Clone)]
pub struct Resource {
    pub id: String,
    pub adapter: String,
    pub facts: ResourceFacts,
}

impl Resource {
    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id, "adapter": self.adapter, "kind": self.facts.kind, "backend_ref": self.facts.backend_ref,
            "name": self.facts.name, "title": self.facts.title, "state": self.facts.state,
            "status": self.facts.status, "exit": self.facts.exit, "argv": self.facts.argv, "cwd": self.facts.cwd,
        })
    }
}

pub struct CommandInfo {
    pub name: String,
    pub title: String,
    pub owner: String,
    /// Connection that registered this command; author names may be shared across replacements.
    pub connection: Option<u64>,
    pub available: bool,
    pub peer: Option<Peer>,
}

impl CommandInfo {
    pub fn to_json(&self) -> Value {
        json!({"name": self.name, "title": self.title, "owner": self.owner, "available": self.available})
    }
}

/// Built-in recovery and convenience commands. Owned by the core, never replaceable (D15).
pub const BUILTINS: &[(&str, &str)] = &[
    ("prim.layout.reset", "Reset the layout to one tab per live terminal"),
    ("prim.revert", "Revert a workspace transaction (args: txn; default: the last one not made by an extension)"),
    ("prim.safe_mode", "Stop all extensions, or start them again (args: on)"),
    ("prim.extension.disable", "Disable an extension (args: name)"),
    ("prim.extension.enable", "Enable an extension (args: name)"),
    ("prim.extension.restart", "Restart an extension, clearing its crash budget (args: name)"),
];

pub struct Core {
    pub store: Store,
    pub resources: BTreeMap<String, Resource>,
    map: ResourceMap,
    map_path: Option<PathBuf>,
    pub adapters: BTreeMap<String, AdapterHandle>,
    pub adapter_state: BTreeMap<String, Value>,
    pub default_adapter: Option<String>,
    log: VecDeque<(u64, Value)>,
    pub seq: u64,
    /// Opaque identity of this event log; sequence numbers are meaningful only within it.
    pub epoch: String,
    pub events: broadcast::Sender<(u64, Value)>,
    pub commands: BTreeMap<String, CommandInfo>,
    pub exts: BTreeMap<String, ExtRuntime>,
    pub safe_mode: bool,
    /// Live view content (`view.publish`): derived, not persisted, not in history.
    /// view -> (content, author, connection id). Cleared when its publisher disconnects.
    pub live: BTreeMap<String, (Value, String, u64)>,
    /// Connected clients by connection id: what they said in `hello` (author, client, version, build).
    pub clients: BTreeMap<u64, Value>,
    pub socket: PathBuf,
    pub state_dir: Option<PathBuf>,
}

impl Core {
    pub fn new(store: Store, state_dir: Option<PathBuf>, socket: PathBuf, safe_mode: bool) -> anyhow::Result<Core> {
        let map_path = state_dir.as_ref().map(|d| d.join("resources.json"));
        let map = match &map_path {
            Some(p) if p.exists() => serde_json::from_slice(&std::fs::read(p)?)?,
            _ => ResourceMap { next: 1, ..Default::default() },
        };
        let mut boot = [0u8; 16];
        getrandom::fill(&mut boot).map_err(|e| anyhow::anyhow!("event epoch: {e}"))?;
        let epoch = boot.iter().map(|b| format!("{b:02x}")).collect();
        let (events, _) = broadcast::channel(4096);
        let mut commands = BTreeMap::new();
        for (name, title) in BUILTINS {
            commands.insert(name.to_string(), CommandInfo { name: name.to_string(), title: title.to_string(), owner: "core".into(), connection: None, available: true, peer: None });
        }
        Ok(Core {
            store,
            resources: BTreeMap::new(),
            map,
            map_path,
            adapters: BTreeMap::new(),
            adapter_state: BTreeMap::new(),
            default_adapter: None,
            log: VecDeque::new(),
            seq: 0,
            epoch,
            events,
            commands,
            exts: BTreeMap::new(),
            safe_mode,
            live: BTreeMap::new(),
            clients: BTreeMap::new(),
            socket,
            state_dir,
        })
    }

    /// Append to the event log and broadcast.
    pub fn emit(&mut self, ty: &str, data: Value) {
        self.seq += 1;
        let ev = json!({"epoch": self.epoch, "seq": self.seq, "type": ty, "data": data});
        self.log.push_back((self.seq, ev.clone()));
        while self.log.len() > LOG {
            self.log.pop_front();
        }
        let _ = self.events.send((self.seq, ev));
    }

    /// Events after `from`, or None if some were already dropped from the bounded log (`missed`).
    pub fn events_since(&self, from: u64) -> Option<Vec<Value>> {
        let oldest = self.log.front().map(|(s, _)| *s).unwrap_or(self.seq + 1);
        if from > self.seq || from.saturating_add(1) < oldest {
            return None;
        }
        Some(self.log.iter().filter(|(s, _)| *s > from).map(|(_, e)| e.clone()).collect())
    }

    pub fn snapshot(&self) -> Value {
        json!({
            "epoch": self.epoch,
            "seq": self.seq,
            "entries": self.store.entries(),
            "resources": self.resources.values().map(|r| r.to_json()).collect::<Vec<_>>(),
            "commands": self.commands.values().map(|c| c.to_json()).collect::<Vec<_>>(),
            "extensions": self.exts.iter().map(|(n, e)| (n.clone(), e.to_json())).collect::<BTreeMap<_, _>>(),
            "adapters": self.adapter_state,
            "safe_mode": self.safe_mode,
            "live": self.live.iter().map(|(k, (v, _, _))| (k.clone(), v.clone())).collect::<BTreeMap<_, _>>(),
        })
    }

    /// Record a committed transaction as an event.
    pub fn emit_txn(&mut self, txn: &Txn) {
        let changes: Vec<Value> = txn
            .changes
            .iter()
            .map(|c| json!({"key": c.key, "version": c.after.as_ref().map(|e| e.version), "value": c.after.as_ref().map(|e| e.value.clone())}))
            .collect();
        self.emit("entries", json!({"txn": txn.id, "author": txn.author, "note": txn.note, "reverts": txn.reverts, "changes": changes}));
    }

    /// Publish every adopted transaction, including a replacement whose final directory sync
    /// failed. Clean persistence errors do not change state and never produce an entries event.
    pub fn finish_txn(&mut self, result: Result<Txn, RpcError>) -> Result<Txn, RpcError> {
        match &result {
            Ok(txn) => self.emit_txn(txn),
            Err(err) if err.kind() == "durability_unconfirmed" => {
                if let Some(txn) = self.store.history(1).first() {
                    self.emit_txn(txn);
                }
            }
            Err(_) => {}
        }
        result
    }

    pub fn txn_summary(txn: &Txn) -> Value {
        json!({"txn": txn.id, "changes": txn.changes.iter().map(|c| json!({"key": c.key, "version": c.after.as_ref().map(|e| e.version)})).collect::<Vec<_>>()})
    }

    pub fn resource(&self, id: &str) -> Result<&Resource, RpcError> {
        self.resources.get(id).ok_or_else(|| RpcError::new("not_found", format!("no resource {id}")))
    }

    pub fn adapter(&self, name: Option<&str>) -> Result<AdapterHandle, RpcError> {
        let name = name.map(String::from).or_else(|| self.default_adapter.clone()).ok_or_else(|| RpcError::new("unavailable", "no adapter configured"))?;
        self.adapters.get(&name).cloned().ok_or_else(|| RpcError::new("not_found", format!("no adapter {name}")))
    }

    fn id_for(&mut self, adapter: &str, backend_ref: &str) -> String {
        if let Some((id, _)) = self.map.ids.iter().find(|(_, (a, r))| a == adapter && r == backend_ref) {
            return id.clone();
        }
        let id = format!("r{}", self.map.next);
        self.map.next += 1;
        self.map.ids.insert(id.clone(), (adapter.to_string(), backend_ref.to_string()));
        self.save_map();
        id
    }

    fn save_map(&mut self) {
        let Some(path) = &self.map_path else { return };
        let result = serde_json::to_vec_pretty(&self.map)
            .map_err(anyhow::Error::from)
            .and_then(|bytes| crate::store::write_atomic(path, &bytes).map_err(anyhow::Error::from));
        if let Err(error) = result {
            // Backend effects already happened. Keep the observed resource and its in-memory id,
            // but explicitly report that its identity may not survive a core restart.
            let message = format!("could not persist resource ids at {}: {error}", path.display());
            eprintln!("prim: {message}");
            self.emit("persistence.error", json!({"scope": "resources", "message": message}));
        }
    }

    /// Upsert from an adapter; returns the prim id.
    pub fn upsert_resource(&mut self, adapter: &str, facts: ResourceFacts) -> String {
        let id = self.id_for(adapter, &facts.backend_ref);
        let changed = self.resources.get(&id).map(|r| r.facts != facts).unwrap_or(true);
        if changed {
            let r = Resource { id: id.clone(), adapter: adapter.to_string(), facts };
            self.emit("resource", r.to_json());
            self.resources.insert(id.clone(), r);
        }
        id
    }

    fn remove_resource(&mut self, adapter: &str, backend_ref: &str) {
        let id = self.map.ids.iter().find(|(_, (a, r))| a == adapter && r == backend_ref).map(|(id, _)| id.clone());
        if let Some(id) = id {
            self.map.ids.remove(&id);
            self.save_map();
            if self.resources.remove(&id).is_some() {
                self.emit("resource.removed", json!({"id": id}));
            }
        }
    }

    pub fn on_adapter_event(&mut self, adapter: &str, ev: AdapterEvent) {
        match ev {
            AdapterEvent::Connected(up, error) => {
                let st = json!({"connected": up, "error": error});
                self.adapter_state.insert(adapter.to_string(), st.clone());
                self.emit("adapter", json!({"name": adapter, "state": st}));
            }
            AdapterEvent::Snapshot(list) => {
                let seen: Vec<String> = list.iter().map(|f| f.backend_ref.clone()).collect();
                for f in list {
                    self.upsert_resource(adapter, f);
                }
                let gone: Vec<String> = self
                    .resources
                    .values()
                    .filter(|r| r.adapter == adapter && !seen.contains(&r.facts.backend_ref))
                    .map(|r| r.facts.backend_ref.clone())
                    .collect();
                for r in gone {
                    self.remove_resource(adapter, &r);
                }
                // Forget mappings to backend refs that no longer exist.
                let stale: Vec<String> = self.map.ids.iter().filter(|(_, (a, r))| a == adapter && !seen.contains(r)).map(|(id, _)| id.clone()).collect();
                if !stale.is_empty() {
                    for id in stale {
                        self.map.ids.remove(&id);
                    }
                    self.save_map();
                }
            }
            AdapterEvent::Upsert(f) => {
                self.upsert_resource(adapter, f);
            }
            AdapterEvent::Removed(r) => self.remove_resource(adapter, &r),
        }
    }
}

/// Start an adapter and pump its events into the core.
pub fn add_adapter(core: &Shared, handle: AdapterHandle, events: mpsc::UnboundedReceiver<(String, AdapterEvent)>, default: bool) {
    {
        let mut c = core.lock().unwrap();
        if default || c.default_adapter.is_none() {
            c.default_adapter = Some(handle.name.clone());
        }
        c.adapters.insert(handle.name.clone(), handle);
    }
    let core = core.clone();
    let mut events = events;
    tokio::spawn(async move {
        while let Some((name, ev)) = events.recv().await {
            core.lock().unwrap().on_adapter_event(&name, ev);
        }
    });
}
