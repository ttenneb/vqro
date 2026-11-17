//! The prim core: workspace store, resources from adapters, the event log, commands and extensions.
//!
//! Shared as `Arc<Mutex<Core>>`. The lock is never held across an await.

use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use tokio::sync::{broadcast, mpsc, watch};
use crate::provider::{self, IdentityMap, Provider, Route};

use crate::adapter::{AdapterEvent, AdapterHandle, ResourceFacts};
use crate::ext::ExtRuntime;
use crate::rpc::{Peer, RpcError};
use crate::store::{Store, Txn};

pub type Shared = Arc<Mutex<Core>>;

const LOG: usize = 10_000;
pub const LOG_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone)]
pub struct Resource {
    pub id: String,
    pub adapter: String,
    pub instance: String,
    pub available: bool,
    pub host: Option<String>,
    life: watch::Sender<bool>,
    pub facts: ResourceFacts,
}

impl Resource {
    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id, "provider": self.adapter, "adapter": self.adapter, "instance": self.instance,
            "incarnation": self.facts.incarnation, "available": self.available, "stale": !self.available, "host": self.host,
            "kind": self.facts.kind, "backend_ref": self.facts.backend_ref,
            "name": self.facts.name, "title": self.facts.title, "state": self.facts.state,
            "status": self.facts.status, "exit": self.facts.exit, "argv": self.facts.argv, "cwd": self.facts.cwd, "data": self.facts.data,
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
    identities: IdentityMap,
    admission: crate::resource_admission::Admission,
    pub bridge_tokens: crate::bridge_tokens::Tokens,
    pub providers: BTreeMap<String, Provider>,
    /// Compatibility connection-state alias; authoritative declarations are in `providers`.
    pub adapter_state: BTreeMap<String, Value>,
    pub default_adapter: Option<String>,
    next_provider_source: u64,
    next_effect: u64,
    identity_failures: BTreeMap<String, RpcError>,
    pending_facts: BTreeMap<String, BTreeMap<String, (String, ResourceFacts)>>,
    recovering_identities: bool,
    log: VecDeque<(u64, Value, usize)>,
    log_bytes: usize,
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
        let identities = IdentityMap::open(state_dir.as_ref().map(|d| d.join("resources.json")))?;
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
            identities,
            admission: Default::default(),
            bridge_tokens: crate::bridge_tokens::Tokens::open(state_dir.as_ref().map(|dir| dir.join("runtime-tokens.json")))?,
            providers: BTreeMap::new(),
            adapter_state: BTreeMap::new(),
            default_adapter: None,
            next_provider_source: 1,
            next_effect: 1,
            identity_failures: BTreeMap::new(),
            pending_facts: BTreeMap::new(),
            recovering_identities: false,
            log: VecDeque::new(),
            log_bytes: 0,
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
        let bytes = crate::limits::encoded_bytes(&ev).max(64);
        // An oversized record creates a cursor gap, not an oversized retained allocation. Keep
        // live delivery unchanged; subscribers unable to accept it recover through a snapshot.
        if bytes > LOG_BYTES {
            self.log.clear();
            self.log_bytes = 0;
        } else {
            while self.log.len() >= LOG || self.log_bytes + bytes > LOG_BYTES {
                if let Some((_, _, old)) = self.log.pop_front() { self.log_bytes -= old; }
            }
            self.log_bytes += bytes;
            self.log.push_back((self.seq, ev.clone(), bytes));
        }
        let _ = self.events.send((self.seq, ev));
    }

    /// Events after `from`, or None if some were already dropped from the bounded log (`missed`).
    pub fn events_since(&self, from: u64) -> Option<Vec<Value>> {
        let oldest = self.log.front().map(|(s, _, _)| *s).unwrap_or(self.seq + 1);
        if from > self.seq || from.saturating_add(1) < oldest {
            return None;
        }
        Some(self.log.iter().filter(|(s, _, _)| *s > from).map(|(_, e, _)| e.clone()).collect())
    }

    pub fn snapshot(&self) -> Value {
        json!({
            "epoch": self.epoch,
            "seq": self.seq,
            "recovery_notice": self.store.admission_notice(),
            "entries": self.store.entries(),
            "resources": self.resources.values().map(|r| r.to_json()).collect::<Vec<_>>(),
            "commands": self.commands.values().map(|c| c.to_json()).collect::<Vec<_>>(),
            "extensions": self.exts.iter().map(|(n, e)| (n.clone(), e.to_json())).collect::<BTreeMap<_, _>>(),
            "providers": self.providers.keys().map(|n| (n.clone(), self.provider_json(n))).collect::<BTreeMap<_, _>>(),
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

    pub fn provider_route(&self, name: Option<&str>, capability: &str) -> Result<Route, RpcError> {
        let name = name.or(self.default_adapter.as_deref()).ok_or_else(|| provider::rejected("unavailable", "no provider configured"))?;
        self.providers.get(name).ok_or_else(|| provider::rejected("not_found", format!("no provider {name}")))?.route(capability)
    }

    pub fn resource_route(&self, id: &str, capability: &str) -> Result<Route, RpcError> {
        let resource = self.resource(id)?;
        if !resource.available { return Err(provider::rejected("unavailable", format!("resource {id} is unavailable"))); }
        let mut route = self.provider_route(Some(&resource.adapter), capability)?;
        if route.instance != resource.instance { return Err(provider::rejected("unavailable", "resource belongs to an old provider instance")); }
        route.resource = Some(resource.life.subscribe());
        route.target = Some((resource.facts.backend_ref.clone(), resource.facts.incarnation.clone()));
        Ok(route)
    }

    pub fn has_resource_identity(&self, id: &str) -> bool { self.identities.active(id) }

    pub fn effect_id(&mut self) -> String {
        let id = format!("{}:e{}", self.epoch, self.next_effect);
        self.next_effect += 1;
        id
    }

    fn identity_error(&mut self, provider: &str, mut error: RpcError) -> RpcError {
        let data = error.data.as_mut().unwrap();
        data["outcome"] = json!("indeterminate");
        data["provider"] = json!(provider);
        data["instance"] = json!(self.providers.get(provider).and_then(|p| p.info.as_ref()).map(|i| &i.instance));
        if !self.identity_failures.contains_key(provider) {
            eprintln!("prim: resource identities for {provider}: {error}");
            self.identity_failures.insert(provider.into(), error.clone());
            self.emit("persistence.error", json!({"scope": "resources", "provider": provider, "message": error.message, "error": error}));
            self.provider_event(provider);
        }
        error
    }

    fn identity_recovered(&mut self, provider: &str) {
        if self.identity_failures.remove(provider).is_some() {
            eprintln!("prim: resource identity persistence recovered for {provider}");
            self.emit("persistence.recovered", json!({"scope": "resources", "provider": provider}));
            self.provider_event(provider);
        }
    }

    fn recover_pending_identities(&mut self) {
        if self.recovering_identities { return; }
        self.recovering_identities = true;
        let pending = std::mem::take(&mut self.pending_facts);
        self.admission.clear_pending();
        for (name, records) in pending {
            for (reference, (instance, facts)) in records {
                let current = self.providers.get(&name).is_some_and(|p| p.info.as_ref().is_some_and(|i| i.instance == instance));
                if !current { continue; }
                let ready = self.providers[&name].synced && self.providers[&name].health.borrow().connected;
                if ready { let _ = self.upsert_resource(&name, facts); }
                else {
                    let host = self.providers[&name].metadata["host"].as_str();
                    self.admission.pending(&name, &reference, crate::resource_admission::charge(&name, &instance, host, &facts));
                    self.pending_facts.entry(name.clone()).or_default().insert(reference, (instance, facts));
                }
            }
        }
        self.recovering_identities = false;
    }

    fn refresh_pending_admission(&mut self) {
        self.admission.clear_pending();
        for (provider, records) in &self.pending_facts {
            let host = self.providers.get(provider).and_then(|p| p.metadata["host"].as_str());
            for (reference, (instance, facts)) in records {
                self.admission.pending(provider, reference, crate::resource_admission::charge(provider, instance, host, facts));
            }
        }
    }

    fn retire_visible(&mut self, ids: Vec<String>, reason: &str) {
        for id in ids {
            self.admission.remove_resource(&id);
            if let Err(error) = self.bridge_tokens.expire_host(&id) {
                eprintln!("prim: runtime credential revocation persistence failed: {error}");
                self.emit("persistence.error", json!({"scope": "runtime_tokens", "message": error.message}));
            }
            if let Some(resource) = self.resources.remove(&id) { resource.life.send_replace(false); }
            self.emit("resource.removed", json!({"id": id, "reason": {"reason": reason}}));
        }
    }

    fn mark_unavailable(&mut self, provider: &str) {
        let mut changes = Vec::new();
        for resource in self.resources.values_mut().filter(|r| r.adapter == provider && r.available) {
            resource.available = false;
            resource.life.send_replace(false);
            changes.push(resource.to_json());
        }
        for resource in changes { self.emit("resource", resource); }
    }

    pub fn provider_persistence_error(&self, name: &str) -> Option<RpcError> {
        self.identity_failures.get(name).cloned()
    }

    /// Administrative install shared by native starts and a validated wire registration.
    /// Wire callers check duplicate live names before using this operation.
    pub fn install_adapter(&mut self, handle: AdapterHandle, default: bool) -> Result<u64, RpcError> {
        let name = handle.name.clone();
        if name.is_empty() || name.len() > 128 || crate::resource_admission::encoded_bytes(&handle.capabilities) > 128 * 1024 {
            return Err(provider::rejected("overloaded", format!("provider {name}: name/registration metadata exceeds admission limits")));
        }
        if !self.providers.contains_key(&name) && self.providers.len() >= 32 { return Err(provider::rejected("overloaded", format!("provider {name}: registry limit is 32"))); }
        if default || self.default_adapter.is_none() { self.default_adapter = Some(name.clone()); }
        let generation = self.providers.get(&name).map(|p| p.generation).unwrap_or(0);
        if self.providers.contains_key(&name) { self.on_adapter_event(&name, AdapterEvent::Connected(false, Some("provider replaced".into()))); }
        let source = self.next_provider_source;
        self.next_provider_source += 1;
        self.providers.insert(name, Provider::new(handle, source, generation));
        Ok(source)
    }

    fn provider_json(&self, name: &str) -> Value {
        let mut value = self.providers[name].to_json();
        value["identity_persistence"] = json!({"state": if self.identity_failures.contains_key(name) { "failed" } else { "ok" }, "error": self.identity_failures.get(name).map(|error| &error.message)});
        value
    }

    pub(crate) fn provider_event(&mut self, name: &str) {
        let state = self.provider_json(name);
        self.adapter_state.insert(name.into(), state.clone());
        self.emit("provider", json!({"name": name, "state": state}));
        // Temporary compatibility event/field for existing extensions.
        self.emit("adapter", json!({"name": name, "state": state}));
    }

    pub fn upsert_resource(&mut self, name: &str, facts: ResourceFacts) -> Result<String, RpcError> {
        if !facts.valid() {
            self.on_adapter_event(name, AdapterEvent::Connected(false, Some("provider supplied invalid facts".into())));
            return Err(provider::indeterminate("provider supplied invalid facts; effect may already have happened"));
        }
        let provider = self.providers.get(name).ok_or_else(|| provider::rejected("not_found", "unknown provider"))?;
        if !provider.health.borrow().connected { return Err(provider::rejected("unavailable", "provider disconnected")); }
        let instance = provider.health.borrow().instance.clone();
        let host = if facts.kind == "agent" { provider.metadata["host"].as_str().map(String::from) } else { None };
        // Even if disk persistence fails, an observed replacement must immediately stop old
        // routes targeting the reused reference. Retiring its id still waits for durable commit.
        let mut replaced = Vec::new();
        for resource in self.resources.values_mut().filter(|r| r.adapter == name && r.facts.backend_ref == facts.backend_ref
            && (r.instance != instance || r.facts.incarnation != facts.incarnation) && r.available) {
            resource.available = false;
            resource.life.send_replace(false);
            replaced.push(resource.to_json());
        }
        for resource in replaced { self.emit("resource", resource); }
        let bytes = crate::resource_admission::charge(name, &instance, host.as_deref(), &facts);
        if let Err(mut error) = self.admission.check(name, &facts.backend_ref, &facts, bytes) {
            let data = error.data.as_mut().unwrap();
            data["outcome"] = json!("indeterminate"); data["backend_outcome"] = json!("observed");
            data["provider"] = json!(name); data["instance"] = json!(instance); data["resource_facts"] = json!(facts);
            self.on_adapter_event(name, AdapterEvent::Connected(false, Some(error.message.clone())));
            return Err(error);
        }
        let update = match self.identities.identify(provider::identity(name, &instance, &facts)) {
            Ok(update) => update,
            Err(mut error) => {
                self.admission.pending(name, &facts.backend_ref, bytes);
                self.pending_facts.entry(name.into()).or_default().insert(facts.backend_ref.clone(), (instance.clone(), facts.clone()));
                let data = error.data.as_mut().unwrap();
                data["backend_outcome"] = json!("observed");
                data["resource_facts"] = json!(facts);
                return Err(self.identity_error(name, error));
            }
        };
        self.retire_visible(update.retired, "replaced");
        self.admission.remove_pending(name, &facts.backend_ref);
        if let Some(pending) = self.pending_facts.get_mut(name) { pending.remove(&facts.backend_ref); }
        self.admission.resource(&update.id, name, &facts.backend_ref, bytes);
        let changed = self.resources.get(&update.id).is_none_or(|r| r.facts != facts || !r.available);
        if changed {
            let life = self.resources.get(&update.id).map(|r| r.life.clone()).unwrap_or_else(|| watch::channel(true).0);
            let was_available = *life.borrow();
            if !was_available { life.send_replace(true); }
            let resource = Resource { id: update.id.clone(), adapter: name.into(), instance, available: true, host, life, facts };
            self.emit("resource", resource.to_json());
            self.resources.insert(update.id.clone(), resource);
        }
        if let Some(mut error) = update.uncertain {
            let data = error.data.as_mut().unwrap();
            data["resource"] = json!(update.id);
            data["backend_outcome"] = json!("observed");
            data["resource_facts"] = json!(self.resources.get(&update.id).map(|r| &r.facts));
            return Err(self.identity_error(name, error));
        }
        self.identity_recovered(name);
        self.recover_pending_identities();
        Ok(update.id)
    }

    pub fn on_adapter_event(&mut self, name: &str, event: AdapterEvent) {
        if !self.providers.contains_key(name) { return; }
        match event {
            AdapterEvent::Registered(info) => {
                let capabilities = &self.providers[name].handle.capabilities;
                if info.connection == 0 || info.instance.is_empty() || info.instance.len() > 256
                    || crate::resource_admission::encoded_bytes(&json!({"name":name,"instance":info.instance,"software":info.software,"capabilities":capabilities})) > 128 * 1024
                    || !matches!(info.role.as_str(), "backend" | "runtime")
                    || !info.software["name"].is_string() || !info.software["version"].is_string()
                    || !capabilities.as_object().is_some_and(|caps| caps.values().all(Value::is_object)) {
                    self.on_adapter_event(name, AdapterEvent::Connected(false, Some("provider supplied invalid registration facts or capabilities".into())));
                    return;
                }
                self.mark_unavailable(name);
                self.providers.get_mut(name).unwrap().register(info.clone());
                if let Some(pending) = self.pending_facts.get_mut(name) { pending.retain(|_, (instance, _)| instance == &info.instance); }
                self.refresh_pending_admission();
                match self.identities.retire(|i| i.provider == name && i.instance != info.instance) {
                    Ok((ids, error)) => { self.retire_visible(ids, "replaced"); if let Some(error) = error { self.identity_error(name, error); } }
                    Err(error) => { self.identity_error(name, error); }
                }
                self.provider_event(name);
            }
            AdapterEvent::Connected(up, error) => {
                if up {
                    if let Some(info) = self.providers[name].info.clone() { self.on_adapter_event(name, AdapterEvent::Registered(info)); }
                    return;
                }
                self.providers.get_mut(name).unwrap().disconnect(error);
                self.mark_unavailable(name);
                self.provider_event(name);
            }
            AdapterEvent::Snapshot(list) => {
                if list.iter().any(|facts| !facts.valid()) {
                    self.on_adapter_event(name, AdapterEvent::Connected(false, Some("provider supplied an invalid inventory".into())));
                    return;
                }
                let Some(info) = self.providers[name].info.clone() else { return };
                if !self.providers[name].health.borrow().connected { return; }
                let seen: Vec<_> = list.iter().map(|f| provider::identity(name, &info.instance, f)).collect();
                if let Some(pending) = self.pending_facts.get_mut(name) {
                    pending.retain(|_, (instance, facts)| seen.contains(&provider::identity(name, instance, facts)));
                }
                self.refresh_pending_admission();
                let newly_synced = !self.providers[name].synced;
                self.providers.get_mut(name).unwrap().synced = true;
                for facts in list { let _ = self.upsert_resource(name, facts); }
                match self.identities.retire(|i| i.provider == name && !seen.contains(i)) {
                    Ok((ids, error)) => {
                        self.retire_visible(ids, "closed");
                        if let Some(error) = error { self.identity_error(name, error); }
                        else { self.identity_recovered(name); self.recover_pending_identities(); }
                    }
                    Err(error) => { self.identity_error(name, error); }
                }
                if newly_synced { self.provider_event(name); }
            }
            AdapterEvent::Upsert(facts) => { let _ = self.upsert_resource(name, facts); }
            AdapterEvent::Removed { reference, incarnation } => {
                let Some(info) = self.providers[name].info.clone() else { return };
                if let Some(pending) = self.pending_facts.get_mut(name) {
                    if pending.get(&reference).is_some_and(|(instance, facts)| instance == &info.instance && facts.incarnation == incarnation) { pending.remove(&reference); self.admission.remove_pending(name, &reference); }
                }
                match self.identities.retire(|i| i.provider == name && i.instance == info.instance && i.reference == reference && i.incarnation == incarnation) {
                    Ok((ids, error)) => { self.retire_visible(ids, "closed"); if let Some(error) = error { self.identity_error(name, error); } }
                    Err(error) => { self.identity_error(name, error); }
                }
            }
        }
    }
}

/// Start an adapter and pump its events into the core.
pub fn add_adapter(core: &Shared, handle: AdapterHandle, events: mpsc::UnboundedReceiver<(String, AdapterEvent)>, default: bool) {
    let name = handle.name.clone();
    let source = {
        let mut c = core.lock().unwrap();
        match c.install_adapter(handle, default) {
            Ok(source) => source,
            Err(error) => { eprintln!("prim: native provider admission rejected: {error}"); c.emit("provider.rejected", json!({"name":name,"error":error})); return; }
        }
    };
    let core = core.clone();
    let mut events = events;
    tokio::spawn(async move {
        while let Some((sender, event)) = events.recv().await {
            let mut core = core.lock().unwrap();
            if sender == name && core.providers.get(&name).is_some_and(|p| p.source == source) { core.on_adapter_event(&name, event); }
        }
        let mut core = core.lock().unwrap();
        if core.providers.get(&name).is_some_and(|p| p.source == source) {
            core.on_adapter_event(&name, AdapterEvent::Connected(false, Some("provider event connection closed".into())));
        }
    });
}
