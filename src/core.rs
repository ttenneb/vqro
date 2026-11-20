//! The vqro core: workspace store, resources from adapters, the event log, commands and extensions.
//!
//! Shared as `Arc<Mutex<Core>>`. The lock is never held across an await.

use std::collections::{BTreeSet, BTreeMap, VecDeque};
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
    /// Core-recorded launch assignment, never provider/child facts. Unknown across rehydration.
    pub principal: Option<String>,
    life: watch::Sender<bool>,
    pub facts: Arc<ResourceFacts>,
}

impl Resource {
    pub(crate) fn lifetime(&self)->watch::Receiver<bool>{self.life.subscribe()}
    pub fn to_json(&self) -> Value {serde_json::to_value(self).expect("resource JSON")}
}
impl serde::Serialize for Resource {
    fn serialize<S:serde::Serializer>(&self,serializer:S)->Result<S::Ok,S::Error> {
        #[derive(serde::Serialize)] struct Wire<'a> {
            id:&'a str,provider:&'a str,adapter:&'a str,instance:&'a str,available:bool,stale:bool,host:&'a Option<String>,principal:&'a Option<String>,
            #[serde(flatten)] facts:&'a ResourceFacts,
        }
        serde::Serialize::serialize(&Wire {id:&self.id,provider:&self.adapter,adapter:&self.adapter,instance:&self.instance,available:self.available,stale:!self.available,host:&self.host,principal:&self.principal,facts:&self.facts},serializer)
    }
}

/// Immutable fence captured under the core lock; all large JSON construction happens later.
#[derive(serde::Serialize)]
pub struct CapturedSnapshot {
    #[serde(flatten)] header:Value,
    entries:Arc<crate::store::Entries>,resources:Vec<Resource>,
    providers:BTreeMap<String,Arc<Value>>,adapters:BTreeMap<String,Arc<Value>>,live:BTreeMap<String,Arc<Value>>,
}
impl CapturedSnapshot {
    fn add_entry_notice(&mut self) {
        if let Some(notice)=Store::entries_notice(&self.entries) {
            self.header["recovery_notice"]=json!(match self.header["recovery_notice"].as_str() {Some(existing)=>format!("{existing}; {notice}"),None=>notice.to_string()});
        }
    }
    pub fn into_value(mut self)->Value {
        self.add_entry_notice();
        serde_json::to_value(self).expect("snapshot JSON")
    }
    pub fn into_value_limited(mut self,limit:usize)->Result<Value,RpcError> {
        struct Budget(usize);
        impl std::io::Write for Budget {
            fn write(&mut self,bytes:&[u8])->std::io::Result<usize> {
                if bytes.len()>self.0 {return Err(std::io::Error::other("snapshot envelope exceeded"));}
                self.0-=bytes.len();Ok(bytes.len())
            }
            fn flush(&mut self)->std::io::Result<()> {Ok(())}
        }
        self.add_entry_notice();
        let mut budget=Budget(limit);
        serde_json::to_writer(&mut budget,&self).map_err(|_|provider::rejected("too_large","snapshot exceeds this connection recovery envelope"))?;
        Ok(serde_json::to_value(self).expect("snapshot JSON"))
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
    pub lifetime: Option<crate::ext::Owner>,
}

impl CommandInfo {
    pub fn to_json(&self) -> Value {
        json!({"name": self.name, "title": self.title, "owner": self.owner, "available": self.available})
    }
}

/// Built-in recovery and convenience commands. Owned by the core, never replaceable (D15).
pub const BUILTINS: &[(&str, &str)] = &[
    ("vqro.layout.reset", "Reset the layout to one tab per live terminal"),
    ("vqro.revert", "Revert a workspace transaction (args: txn; default: the last one not made by an extension)"),
    ("vqro.safe_mode", "Stop all extensions/providers, or leave safe mode (args: on)"),
    ("vqro.safe_mode.allow", "Temporarily allow a declared extension in safe mode (args: name, on)"),
    ("vqro.shutdown", "Stop extensions and provider connections, then exit the core"),
    ("vqro.extension.disable", "Disable an extension (args: name)"),
    ("vqro.extension.enable", "Enable an extension (args: name)"),
    ("vqro.extension.restart", "Restart an extension, clearing its crash budget (args: name)"),
];

pub struct Core {
    // Runtime mutations must go through the serialized, cancellation-independent committer.
    pub(crate) store: Store,
    pub(crate) committer: crate::commit::Committer,
    pub(crate) operational:crate::operational::Registry,
    pub resources: BTreeMap<String, Resource>,
    identities: IdentityMap,
    admission: crate::resource_admission::Admission,
    pub bridge_tokens: crate::bridge_tokens::Tokens,
    pub providers: BTreeMap<String, Provider>,
    provider_metadata_bytes:usize,
    /// Compatibility connection-state alias; authoritative declarations are in `providers`.
    pub adapter_state: BTreeMap<String, Arc<Value>>,
    pub manifest_catalog: crate::manifest::Catalog,
    pub default_adapter: Option<String>,
    pub(crate) embedder_backend: Option<crate::adapter::Backend>,
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
    pub(crate) invocations: crate::invocation::Shared,
    pub exts: BTreeMap<String, ExtRuntime>,
    pub shutdown: crate::shutdown::Shutdown,
    pub(crate) extension_tasks: Vec<tokio::task::JoinHandle<()>>,
    pub safe_mode: bool,
    pub safe_mode_allow: BTreeSet<String>,
    pub provider_extensions: BTreeSet<String>,
    pub recovery_gate: Arc<tokio::sync::Mutex<()>>,
    /// Live view content (`view.publish`): derived, not persisted, not in history.
    /// view -> (content, author, connection id, encoded charge). Cleared on publisher disconnect.
    pub live: BTreeMap<String, (Arc<Value>, String, u64, usize)>,
    pub(crate) snapshot_slots:Arc<tokio::sync::Semaphore>,
    pub(crate) provider_snapshot_slots:Arc<tokio::sync::Semaphore>,
    pub(crate) registration_slots:Arc<tokio::sync::Semaphore>,
    pub live_bytes:usize,
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
            commands.insert(name.to_string(), CommandInfo { name: name.to_string(), title: title.to_string(), owner: "core".into(), connection: None, available: true, peer: None, lifetime:None });
        }
        let operational=crate::operational::Registry::open(state_dir.as_deref(),&store);
        Ok(Core {
            store,
            operational,
            committer: crate::commit::Committer::new(),
            resources: BTreeMap::new(),
            identities,
            admission: Default::default(),
            bridge_tokens: crate::bridge_tokens::Tokens::open(state_dir.as_ref().map(|dir| dir.join("runtime-tokens.json")))?,
            providers: BTreeMap::new(),provider_metadata_bytes:0,
            adapter_state: BTreeMap::new(),
            manifest_catalog:crate::manifest::Catalog::new(crate::paths::extensions_dir()),
            default_adapter: None,
            embedder_backend: None,
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
            invocations: Default::default(),
            exts: BTreeMap::new(),
            shutdown: crate::shutdown::Shutdown::default(),
            extension_tasks: Vec::new(),
            safe_mode,
            safe_mode_allow: BTreeSet::new(),
            provider_extensions: BTreeSet::new(),
            recovery_gate: Arc::new(tokio::sync::Mutex::new(())),
            live: BTreeMap::new(),live_bytes:0,
            snapshot_slots:Arc::new(tokio::sync::Semaphore::new(2)),
            provider_snapshot_slots:Arc::new(tokio::sync::Semaphore::new(2)),
            registration_slots:Arc::new(tokio::sync::Semaphore::new(8)),
            clients: BTreeMap::new(),
            socket,
            state_dir,
        })
    }

    /// Configure the in-process workspace writer before serving mutations (latency/fault harness).
    pub fn configure_workspace_writer(&mut self, writer: crate::store::DurableWriter) -> Result<(), RpcError> {
        if self.committer.started() { return Err(RpcError::new("conflict", "workspace writer cannot change after commit admission begins")); }
        self.store.set_writer(writer);
        Ok(())
    }

    /// Configure the operational writer before serving mutations (embedding/fault harness).
    pub fn configure_operational_writer(&mut self,writer:crate::store::DurableWriter)->Result<(),RpcError> {
        if self.committer.started() {return Err(RpcError::new("conflict","operational writer cannot change after admission"));}
        self.operational.set_writer(writer);Ok(())
    }

    /// Admitted durable mutations across domains, including the active writer (harness metric).
    pub fn workspace_pending_commits(&self) -> usize { self.committer.pending() }

    /// Invalidate old sources immediately; stopping transport workers never kills backend terminals.
    pub fn fence_providers(&mut self, owner: Option<&str>) -> Vec<AdapterHandle> {
        let names: Vec<_> = self.providers.iter().filter(|(_, p)| owner.is_none_or(|owner| p.metadata["owner_extension"].as_str() == Some(owner))).map(|(name, _)| name.clone()).collect();
        let mut handles=Vec::new();
        for name in names {
            handles.push(self.providers[&name].handle.clone());
            self.on_adapter_event(&name, AdapterEvent::Connected(false, Some("stopped for safe mode".into())));
            if let Some(provider)=self.providers.get_mut(&name) {provider.source=self.next_provider_source;}
            self.next_provider_source+=1;
        }
        handles
    }

    /// Retirement invalidates provider routes immediately, not after child cleanup/EOF.
    pub(crate) fn fence_retired_extensions(&mut self) {
        let names:Vec<_>=self.providers.iter().filter(|(_,p)|p.owner.as_ref().is_some_and(|o|!o.is_live()) && p.health.borrow().connected).map(|(name,_)|name.clone()).collect();
        for name in names { self.on_adapter_event(&name,AdapterEvent::Connected(false,Some("supervised extension lifetime ended".into()))); }
        let mut commands=Vec::new();
        for cmd in self.commands.values_mut().filter(|cmd|cmd.available && cmd.lifetime.as_ref().is_some_and(|o|!o.is_live())) {
            cmd.available=false;
            commands.push(cmd.to_json());
        }
        for command in commands { self.emit("command",command); }
    }

    /// Append to the event log and broadcast.
    pub fn emit(&mut self, ty: &str, mut data: Value) {
        // Authoritative recovery notices follow readiness/default changes without a snapshot poll.
        if matches!(ty, "entries" | "provider" | "adapter" | "safe_mode") {
            data["backend_notice"] = json!(self.backend_notice());
        }
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

    pub fn snapshot(&self) -> Value {self.capture_snapshot().into_value()}

    pub fn capture_snapshot(&self)->CapturedSnapshot {
        CapturedSnapshot {
            header:json!({"epoch":self.epoch,"seq":self.seq,"operational":self.operational.summaries(),"identity_reservations":self.identities.reservation_stats(),"recovery_notice":self.store.recovery_notice(),
                "commands":self.commands.values().map(|c|c.to_json()).collect::<Vec<_>>(),
                "extensions":self.extension_states(),
                "backend_notice":self.backend_notice(),"safe_mode":self.safe_mode,"safe_mode_allow":self.safe_mode_allow,"provider_extensions":self.provider_extensions}),
            entries:self.store.shared_entries(),resources:self.resources.values().cloned().collect(),
            providers:self.providers.keys().map(|n|(n.clone(),self.adapter_state.get(n).cloned().unwrap_or_else(||Arc::new(self.provider_json(n))))).collect(),
            adapters:self.adapter_state.clone(),
            live:self.live.iter().map(|(k,(v,_,_,_))|(k.clone(),v.clone())).collect(),
        }
    }

    /// Record a committed transaction as an event.
    pub fn emit_txn(&mut self, txn: &Txn) {
        let changes: Vec<Value> = txn
            .changes
            .iter()
            .map(|c| json!({"key": c.key, "version": c.after_version, "value": self.store.get(&c.key).map(|entry|entry.value.clone())}))
            .collect();
        let mut audit=json!({"txn": txn.id, "author": txn.author, "actor":txn.attribution.actor,"principal":txn.attribution.principal,"initiator":txn.attribution.initiator,"label":txn.attribution.label, "on_behalf_of": txn.on_behalf_of, "undoable":txn.undoable, "note": txn.note, "reverts": txn.reverts, "changes": changes});
        txn.attribution.causality.annotate(&mut audit);self.emit("entries",audit);
    }

    /// Publish every adopted transaction, including a replacement whose final directory sync
    /// failed. Clean persistence errors do not change state and never produce an entries event.
    pub fn finish_txn(&mut self, result: Result<Arc<Txn>, RpcError>) -> Result<Arc<Txn>, RpcError> {
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

    pub(crate) fn finish_operational(&mut self,domain:crate::operational::Domain,mut result:Result<Arc<crate::operational::Transaction>,RpcError>,registration:bool)->(crate::rpc::RpcResult,Option<crate::operational::Domain>) {
        let adopted=result.is_ok()||result.as_ref().is_err_and(|error|error.kind()=="durability_unconfirmed");
        if !adopted {return (result.map(|_|Value::Null),None);}
        let transaction=domain.last.as_ref().expect("adopted operational audit").clone();
        let changes=transaction.changes.iter().map(|change|json!({"key":change.key,"version":change.version,"value":domain.entries.get(&change.key).map(|entry|&entry.value)})).collect::<Vec<_>>();
        let mut audit=json!({"domain":domain.name,"owner":domain.owner,"txn":transaction.id,"author":transaction.attribution.author,"actor":transaction.attribution.context.actor,"principal":transaction.attribution.context.principal,"initiator":transaction.attribution.context.initiator,"label":transaction.attribution.context.label,"on_behalf_of":transaction.attribution.on_behalf_of,"writer_epoch":transaction.attribution.epoch,"generation":transaction.attribution.generation,"operation":if registration {"register"}else{"apply"},"bytes":domain.max_bytes,"records_limit":domain.max_records,"capacity_request":domain.metadata()["capacity_request"],"undoable":false,"durability_unconfirmed":result.is_err(),"changes":changes});
        transaction.attribution.context.causality.annotate(&mut audit);self.emit("operational",audit);
        let mut metadata=domain.metadata();metadata["epoch"]=json!(self.epoch);metadata["seq"]=json!(self.seq);metadata["txn"]=json!(transaction.id);metadata["undoable"]=json!(false);
        metadata["changes"]=json!(transaction.changes);
        if let Err(error)=&mut result {let data=error.data.as_mut().unwrap();data["epoch"]=json!(self.epoch);data["seq"]=json!(self.seq);}
        let retired=self.operational.adopt(domain);
        (result.map(|_|metadata),retired)
    }

    pub fn txn_summary(txn: &Txn) -> Value {
        json!({"txn": txn.id, "undoable":txn.undoable, "changes": txn.changes.iter().map(|c| json!({"key": c.key, "version": c.after_version})).collect::<Vec<_>>()})
    }

    pub fn resource(&self, id: &str) -> Result<&Resource, RpcError> {
        self.resources.get(id).ok_or_else(|| RpcError::new("not_found", format!("no resource {id}")))
    }

    pub fn provider_route(&self, name: Option<&str>, capability: &str) -> Result<Route, RpcError> {
        // A declared default is not interchangeable with whichever backend happened to register first.
        let preferred=self.store.entries().iter().find_map(|(key,entry)| (entry.value["kind"]=="provider" && entry.value["default"]==true).then(||key.strip_prefix("extension/")).flatten());
        let preferred_provider=preferred.and_then(|owner| self.providers.iter().find(|(name,p)| name.as_str()==owner || p.metadata["owner_extension"].as_str()==Some(owner)).map(|(name,_)|name.as_str())).or(preferred);
        let name = name.or(preferred_provider).or(self.default_adapter.as_deref()).ok_or_else(|| provider::rejected("unavailable", "no backend provider configured; open recovery choices"))?;
        self.providers.get(name).ok_or_else(|| provider::rejected("unavailable", format!("configured backend {name} is not connected; open recovery choices")))?.route(capability)
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

    /// Every declaration's facts plus its resolved `dependencies` (reported, never acted on).
    pub fn extension_states(&self)->BTreeMap<String,Value> {
        let mut states:BTreeMap<String,Value>=self.exts.keys().map(|n|(n.clone(),self.extension_state(n))).collect();
        let status=states.iter().map(|(n,v)|(n.clone(),v["display_status"].as_str().unwrap_or("").to_string())).collect();
        let clients:Vec<Value>=self.clients.values().cloned().collect();
        let mut deps=self.manifest_catalog.dependencies(&status,&clients);
        for (name,state) in states.iter_mut() {state["dependencies"]=deps.remove(name).unwrap_or(Value::Null);}
        states
    }

    pub fn extension_state(&self,name:&str)->Value {
        let mut value=self.exts.get(name).map(|r|r.to_json()).unwrap_or_else(||json!({"status":"stopped","crashes":0,"crashes_total":0,"crash_window_seconds":crate::ext::CRASH_WINDOW.as_secs(),"last_exit":null}));
        let key=format!("extension/{name}");let spec=self.store.get(&key).map(|e|&e.value);
        let enabled=spec.is_some_and(|s|s["enabled"]!=false);
        value["enabled"]=json!(enabled);
        value["safe_mode"]=json!(self.safe_mode);value["allowed_in_safe_mode"]=json!(self.safe_mode_allow.contains(name));
        let is_provider=spec.is_some_and(|s|s["kind"]=="provider") || self.provider_extensions.contains(name);
        let ready=self.providers.values().any(|p|p.metadata["owner_extension"].as_str()==Some(name) && p.health.borrow().connected && p.synced);
        value["display_status"]=json!(if !enabled && !self.safe_mode_allow.contains(name){"off"}else if self.safe_mode && !self.safe_mode_allow.contains(name){"stopped-by-safe-mode"}else{match value["status"].as_str().unwrap_or("stopped"){"failed"=>"failed","running" if is_provider && !ready=>"provider-not-connected","running"=>"running",_=>"starting"}});
        if let Some(facts)=self.manifest_catalog.facts(name).as_object(){for(k,v)in facts{value[k]=v.clone();}}
        value
    }

    pub fn backend_notice(&self) -> Option<String> {
        if self.safe_mode { return None; }
        self.provider_route(None,"terminal.spawn").err().map(|error|format!("No default backend is ready: {}. Use recovery choices.",error.message))
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
            eprintln!("vqro: resource identities for {provider}: {error}");
            self.identity_failures.insert(provider.into(), error.clone());
            self.emit("persistence.error", json!({"scope": "resources", "provider": provider, "message": error.message, "error": error}));
            self.provider_event(provider);
        }
        error
    }

    fn identity_recovered(&mut self, provider: &str) {
        if self.identity_failures.remove(provider).is_some() {
            eprintln!("vqro: resource identity persistence recovered for {provider}");
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
            let mut ready_facts=Vec::new();
            for (reference, (instance, facts)) in records {
                let current = self.providers.get(&name).is_some_and(|p| p.info.as_ref().is_some_and(|i| i.instance == instance));
                if !current { continue; }
                let ready = self.providers[&name].synced && self.providers[&name].health.borrow().connected;
                if ready { ready_facts.push(facts); }
                else {
                    let host = self.providers[&name].metadata["host"].as_str();
                    self.admission.pending(&name, &reference, crate::resource_admission::charge(&name, &instance, host, &facts));
                    self.pending_facts.entry(name.clone()).or_default().insert(reference, (instance, facts));
                }
            }
            if !ready_facts.is_empty(){let _=self.upsert_resources(&name,ready_facts,false);}
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

    fn retire_visible(&mut self, ids: Vec<String>, reason: &str) { self.retire_visible_reported(ids,reason,None); }

    fn retire_visible_reported(&mut self, ids: Vec<String>, reason: &str, reported:Option<(&str,Value)>) {
        if ids.is_empty(){return;}
        let hosts=ids.iter().map(String::as_str).collect::<std::collections::HashSet<_>>();
        if let Err(error)=self.bridge_tokens.expire_hosts(&hosts){eprintln!("vqro: runtime credential revocation persistence failed: {error}");self.emit("persistence.error",json!({"scope":"runtime_tokens","message":error.message}));}
        for id in ids {
            self.admission.remove_resource(&id);
            if let Some(resource) = self.resources.remove(&id) { resource.life.send_replace(false); }
            let mut event=json!({"id": id, "reason": {"reason": reason}});
            if let Some((provider,native_reason))=&reported {event["provider"]=json!(provider);event["native_reason"]=native_reason.clone();}
            self.emit("resource.removed",event);
        }
    }

    /// Forget only unavailable cached observations, never execute or kill a backend resource.
    pub fn forget_cached_resource(&mut self,id:&str)->Result<(),RpcError> {
        if self.shutdown.is_requested() {return Err(provider::rejected("unavailable","core stopping; cached retirement not admitted"));}
        let resource=self.resources.get(id).ok_or_else(||provider::rejected("not_found",format!("no resource {id}")))?;
        if resource.available {return Err(provider::rejected("invalid","available resources require native forget"));}
        let name=resource.adapter.clone();let identity=provider::identity(&name,&resource.instance,&resource.facts);
        let (ids,uncertain)=self.identities.forget_cached(&identity).map_err(|mut error| {error.data.as_mut().unwrap()["outcome"]=json!("rejected");error})?;
        if let Some(pending)=self.pending_facts.get_mut(&name) {
            if pending.get(&identity.reference).is_some_and(|(instance,facts)|instance==&identity.instance&&facts.incarnation==identity.incarnation) {
                pending.remove(&identity.reference);self.admission.remove_pending(&name,&identity.reference);
            }
        }
        self.retire_visible(ids,"forgotten");self.reap_runtime_provider(&name);
        if let Some(mut error)=uncertain {error.data.as_mut().unwrap()["scope"]=json!("cached_facts");return Err(error);}
        Ok(())
    }

    fn mark_unavailable(&mut self, provider: &str) {
        let mut changes = Vec::new();
        for id in self.identities.provider_ids(provider) {
            let Some(resource)=self.resources.get_mut(&id).filter(|r|r.available) else {continue};
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
    /// Install (or replace) a provider. `default`: it becomes the provider for spawns that name none;
    /// `may_default`: it becomes that provider only while there is none yet (runtime bridges never are).
    pub fn install_adapter(&mut self, handle: AdapterHandle, default: bool) -> Result<u64, RpcError> {
        self.install_provider(handle, default, true)
    }

    pub fn admit_provider_registration(&self,name:&str,bytes:usize)->Result<(),RpcError>{
        if !self.providers.contains_key(name)&&self.providers.len()>=provider::MAX_PROVIDERS{return Err(provider::rejected("overloaded",format!("provider {name}: registry limit is {}",provider::MAX_PROVIDERS)));}
        if bytes.saturating_sub(provider::REGISTRATION_FLOOR)>128*1024{return Err(provider::rejected("overloaded",format!("provider {name}: registration metadata exceeds128KiB")));}
        let old=self.providers.get(name).map(|p|p.registration_bytes).unwrap_or(0);
        if self.provider_metadata_bytes.saturating_sub(old).saturating_add(bytes)>provider::MAX_REGISTRY_METADATA_BYTES{return Err(provider::rejected("overloaded",format!("provider {name}: aggregate registration metadata exceeds4MiB")));}
        Ok(())
    }
    fn charge_provider_registration(&mut self,name:&str,info:&crate::provider::ProviderInfo)->Result<(),RpcError>{
        let p=&self.providers[name];let charge=provider::registration_charge(name,&p.handle.capabilities,Some(info),&p.metadata);self.admit_provider_registration(name,charge)?;
        let p=self.providers.get_mut(name).unwrap();self.provider_metadata_bytes=self.provider_metadata_bytes.saturating_sub(p.registration_bytes).saturating_add(charge);p.registration_bytes=charge;Ok(())
    }
    pub fn reap_runtime_provider(&mut self,name:&str){
        let eligible=self.providers.get(name).is_some_and(|p|!p.health.borrow().connected&&p.info.as_ref().is_some_and(|i|i.role=="runtime"));
        if !eligible||self.admission.has_provider(name)||self.identities.has_provider(name){return;}
        if let Some(p)=self.providers.remove(name){self.provider_metadata_bytes=self.provider_metadata_bytes.saturating_sub(p.registration_bytes);}
        self.adapter_state.remove(name);self.identity_failures.remove(name);self.pending_facts.remove(name);
        if self.default_adapter.as_deref()==Some(name){self.default_adapter=None;}
        self.emit("provider.removed",json!({"name":name,"reason":"retired"}));self.emit("adapter.removed",json!({"name":name,"reason":"retired"}));
    }

    pub fn install_provider(&mut self, handle: AdapterHandle, default: bool, may_default: bool) -> Result<u64, RpcError> {
        let name = handle.name.clone();
        crate::limits::json_depth(&handle.capabilities,crate::limits::JSON_DEPTH)?;
        if name.is_empty() || name.len() > 128 || crate::resource_admission::encoded_bytes(&handle.capabilities) > 128 * 1024 {
            return Err(provider::rejected("overloaded", format!("provider {name}: name/registration metadata exceeds admission limits")));
        }
        let charge=provider::registration_charge(&name,&handle.capabilities,None,&json!({}));self.admit_provider_registration(&name,charge)?;
        let generation = self.providers.get(&name).map(|p| p.generation).unwrap_or(0);
        if self.providers.contains_key(&name) { self.on_adapter_event(&name, AdapterEvent::Connected(false, Some("provider replaced".into()))); }
        let source = self.next_provider_source;
        self.next_provider_source += 1;
        let old=self.providers.get(&name).map(|p|p.registration_bytes).unwrap_or(0);self.provider_metadata_bytes=self.provider_metadata_bytes.saturating_sub(old).saturating_add(charge);
        let mut p=Provider::new(handle,source,generation);p.registration_bytes=charge;
        if default || (may_default&&self.default_adapter.is_none()){self.default_adapter=Some(name.clone());}
        self.providers.insert(name,p);
        Ok(source)
    }

    fn provider_json(&self, name: &str) -> Value {
        let mut value = self.providers[name].to_json();
        value["identity_persistence"] = json!({"state": if self.identity_failures.contains_key(name) { "failed" } else { "ok" }, "error": self.identity_failures.get(name).map(|error| &error.message)});
        value
    }

    pub(crate) fn provider_event(&mut self, name: &str) {
        let state = self.provider_json(name);
        self.adapter_state.insert(name.into(), Arc::new(state.clone()));
        self.emit("provider", json!({"name": name, "state": state}));
        // Temporary compatibility event/field for existing extensions.
        self.emit("adapter", json!({"name": name, "state": state}));
    }

    /// A spawn reply describes creation, not a new observation after every concurrent event.
    /// Reuse an already-published identity without rolling its newer facts back to that snapshot.
    pub(crate) fn record_spawn(&mut self,name:&str,facts:ResourceFacts,principal:Option<String>)->Result<String,RpcError>{
        let terminal=facts.kind=="terminal";
        let existing=self.providers.get(name).filter(|p|facts.valid()&&p.health.borrow().connected).and_then(|p|p.info.as_ref())
            .and_then(|info|self.identities.lookup(&provider::identity(name,&info.instance,&facts)))
            .filter(|id|self.resources.contains_key(*id)).map(String::from);
        let id=match existing{Some(id)=>id,None=>self.upsert_resource(name,facts)?};
        // A sync can arrive before its spawn receipt. Fill only the exact returned identity, never
        // a nearby/new incarnation, and never rewrite an already known launcher assignment.
        if terminal && principal.is_some(){
            if let Some(resource)=self.resources.get_mut(&id).filter(|resource|resource.principal.is_none()){
                resource.principal=principal;let event=resource.to_json();self.emit("resource",event);
            }
        }
        Ok(id)
    }
    pub fn upsert_resource(&mut self,name:&str,facts:ResourceFacts)->Result<String,RpcError>{
        self.upsert_resources(name,vec![facts],false).map(|mut ids|ids.remove(0))
    }

    /// Admit a complete observed batch before its one identity-map replace. Publication follows adoption.
    pub fn upsert_resources(&mut self,name:&str,list:Vec<ResourceFacts>,authoritative:bool)->Result<Vec<String>,RpcError>{
        let mut references=std::collections::HashSet::new();
        if list.iter().any(|f|!f.valid() || !references.insert(f.backend_ref.clone())) {
            self.on_adapter_event(name,AdapterEvent::Connected(false,Some("provider supplied invalid/duplicate inventory facts".into())));
            return Err(provider::indeterminate("provider supplied invalid facts; effect may already have happened"));
        }
        let p=self.providers.get(name).ok_or_else(||provider::rejected("not_found","unknown provider"))?;
        if !p.health.borrow().connected{return Err(provider::rejected("unavailable","provider disconnected"));}
        let instance=p.health.borrow().instance.clone();let agent_host=p.metadata["host"].as_str().map(String::from);
        let identities=list.iter().map(|f|provider::identity(name,&instance,f)).collect::<Vec<_>>();
        let seen=identities.iter().cloned().collect::<std::collections::HashSet<_>>();
        // Independent observations fence replacements/missing refs even if durability subsequently fails.
        let mut fenced=Vec::new();
        let candidates=if authoritative {self.identities.provider_ids(name)} else {references.iter().filter_map(|reference|self.identities.reference_id(name,reference).map(String::from)).collect()};
        for id in candidates {
            let Some(r)=self.resources.get_mut(&id).filter(|r|r.available) else {continue};
            let identity=provider::identity(name,&r.instance,&r.facts);
            if !seen.contains(&identity) {r.available=false;r.life.send_replace(false);fenced.push(r.to_json());}
        }
        for r in fenced{self.emit("resource",r);}
        if authoritative {if let Some(pending)=self.pending_facts.get_mut(name){pending.retain(|_,(i,f)|seen.contains(&provider::identity(name,i,f)));}self.refresh_pending_admission();}
        let mut staged=(list.len()>1).then(||self.admission.clone());let mut prepared=Vec::with_capacity(list.len());
        for (facts,identity) in list.into_iter().zip(&identities){
            let host=if facts.kind=="agent"{agent_host.clone()}else{None};
            let bytes=crate::resource_admission::charge(name,&instance,host.as_deref(),&facts);
            let unchanged=self.identities.lookup(identity).and_then(|id|self.resources.get(id)).is_some_and(|r|r.facts.as_ref()==&facts&&r.available);
            if !unchanged {
                if let Err(mut error)=staged.as_ref().unwrap_or(&self.admission).check(name,&facts.backend_ref,&facts,bytes){
                    let data=error.data.as_mut().unwrap();data["outcome"]=json!("indeterminate");data["backend_outcome"]=json!("observed");data["provider"]=json!(name);data["instance"]=json!(instance);data["resource_facts"]=json!(facts);
                    self.on_adapter_event(name,AdapterEvent::Connected(false,Some(error.message.clone())));return Err(error);
                }
                // Reserve all facts that would need retaining on an IO failure, not just the last one.
                if let Some(staged)=&mut staged{staged.pending(name,&facts.backend_ref,bytes);}
            }
            prepared.push((facts,host,bytes,!unchanged));
        }
        let newly_synced=authoritative&&!self.providers[name].synced;
        if authoritative{self.providers.get_mut(name).unwrap().synced=true;}
        let result=if authoritative{self.identities.sync_provider(name,&identities)}else{self.identities.identify_many(&identities)};
        let update=match result{
            Ok(update)=>update,
            Err(mut error)=>{
                if let Some(staged)=staged{self.admission=staged;}else{for(facts,_,bytes,changed)in &prepared{if *changed{self.admission.pending(name,&facts.backend_ref,*bytes);}}}
                for(facts,_,_,changed)in &prepared{if *changed{self.pending_facts.entry(name.into()).or_default().insert(facts.backend_ref.clone(),(instance.clone(),facts.clone()));}}
                let data=error.data.as_mut().unwrap();data["backend_outcome"]=json!("observed");data["resource_count"]=json!(prepared.len());if prepared.len()==1{data["resource_facts"]=json!(prepared[0].0);}
                if newly_synced{self.provider_event(name);}return Err(self.identity_error(name,error));
            }
        };
        self.retire_visible(update.replaced,"replaced");self.retire_visible(update.removed,"closed");
        for ((facts,host,bytes,_),id) in prepared.into_iter().zip(&update.ids){
            self.admission.remove_pending(name,&facts.backend_ref);if let Some(pending)=self.pending_facts.get_mut(name){pending.remove(&facts.backend_ref);}
            self.admission.resource(id,name,&facts.backend_ref,bytes);
            if self.resources.get(id).is_none_or(|r|r.facts.as_ref()!=&facts||!r.available){
                let life=self.resources.get(id).map(|r|r.life.clone()).unwrap_or_else(||watch::channel(true).0);let available=*life.borrow();if !available{life.send_replace(true);}
                let principal=self.resources.get(id).and_then(|resource|resource.principal.clone());
                let resource=Resource{id:id.clone(),adapter:name.into(),instance:instance.clone(),available:true,host,principal,life,facts:Arc::new(facts)};self.emit("resource",resource.to_json());self.resources.insert(id.clone(),resource);
            }
        }
        if newly_synced{self.provider_event(name);}
        if let Some(mut error)=update.uncertain {
            let data=error.data.as_mut().unwrap();data["backend_outcome"]=json!("observed");data["resource_count"]=json!(update.ids.len());if update.ids.len()==1{data["resource"]=json!(update.ids[0]);data["resource_facts"]=json!(self.resources.get(&update.ids[0]).map(|r|&r.facts));}
            return Err(self.identity_error(name,error));
        }
        self.identity_recovered(name);self.recover_pending_identities();Ok(update.ids)
    }

    pub fn on_adapter_event(&mut self, name: &str, event: AdapterEvent) {
        if self.shutdown.is_stopped() { return; } // no late backend mutation after the state fence is released
        if !self.providers.contains_key(name) { return; }
        match event {
            AdapterEvent::Consumed(ack) => { let _ = ack.send(()); }
            AdapterEvent::Registered(info) => {
                let capabilities = &self.providers[name].handle.capabilities;
                if crate::limits::json_depth(&info.software,crate::limits::JSON_DEPTH).is_err()
                    || info.connection == 0 || info.instance.is_empty() || info.instance.len() > 256
                    || crate::resource_admission::encoded_bytes(&json!({"name":name,"instance":info.instance,"software":info.software,"capabilities":capabilities})) > 128 * 1024
                    || !matches!(info.role.as_str(), "backend" | "runtime")
                    || !info.software["name"].is_string() || !info.software["version"].is_string()
                    || !capabilities.as_object().is_some_and(|caps| caps.values().all(Value::is_object)) {
                    self.on_adapter_event(name, AdapterEvent::Connected(false, Some("provider supplied invalid registration facts or capabilities".into())));
                    return;
                }
                if let Err(error)=self.charge_provider_registration(name,&info){self.on_adapter_event(name,AdapterEvent::Connected(false,Some(error.message)));return;}
                self.mark_unavailable(name);
                // One core-wide lease counter preserves monotonic generations even after a name is reaped.
                let generation=self.next_provider_source;self.next_provider_source+=1;
                self.providers.get_mut(name).unwrap().generation=generation-1;
                self.providers.get_mut(name).unwrap().register(info.clone());
                if let Some(pending) = self.pending_facts.get_mut(name) { pending.retain(|_, (instance, _)| instance == &info.instance); }
                self.refresh_pending_admission();
                match self.identities.retire_provider_except(name,&info.instance) {
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
                self.reap_runtime_provider(name);
            }
            AdapterEvent::Snapshot(list) => {let _=self.upsert_resources(name,list,true);}
            AdapterEvent::Upsert(facts) => { let _ = self.upsert_resource(name, facts); }
            AdapterEvent::Removed { reference, incarnation, reason } => {
                let Some(info) = self.providers[name].info.clone() else { return };
                let mut fenced=Vec::new();
                let id=self.identities.reference_id(name,&reference).map(String::from);
                if let Some(r)=id.as_ref().and_then(|id|self.resources.get_mut(id)).filter(|r|r.instance==info.instance&&r.facts.incarnation==incarnation&&r.available) {r.available=false;r.life.send_replace(false);fenced.push(r.to_json());}
                for r in fenced{self.emit("resource",r);}
                if let Some(pending) = self.pending_facts.get_mut(name) {
                    if pending.get(&reference).is_some_and(|(instance, facts)| instance == &info.instance && facts.incarnation == incarnation) { pending.remove(&reference); self.admission.remove_pending(name, &reference); }
                }
                match self.identities.retire(|i| i.provider == name && i.instance == info.instance && i.reference == reference && i.incarnation == incarnation) {
                    Ok((ids, error)) => { self.retire_visible_reported(ids, "closed",Some((name,reason))); if let Some(error) = error { self.identity_error(name, error); } }
                    Err(error) => { self.identity_error(name, error); }
                }
                self.reap_runtime_provider(name);
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
            Err(error) => { eprintln!("vqro: native provider admission rejected: {error}"); c.emit("provider.rejected", json!({"name":name,"error":error})); return; }
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

#[cfg(test)] mod hygiene_tests {
    use super::*;
    #[test]
    fn reaped_runtime_names_keep_old_routes_fenced_and_generations_monotonic(){
        let mut c=Core::new(Store::in_memory(),None,PathBuf::from("unused"),false).unwrap();
        let register=|c:&mut Core|{let(tx,_rx)=mpsc::unbounded_channel();c.install_provider(AdapterHandle{name:"runtime".into(),session:None,context:None,capabilities:json!({"agent.prompt":{}}),cmds:tx},false,false).unwrap();c.on_adapter_event("runtime",AdapterEvent::Registered(crate::provider::ProviderInfo{connection:1,instance:"A".into(),role:"runtime".into(),software:json!({"name":"fixture","version":"1"})}));};
        register(&mut c);let generation=c.providers["runtime"].generation;let old=c.provider_route(Some("runtime"),"agent.prompt").unwrap();c.on_adapter_event("runtime",AdapterEvent::Connected(false,None));assert!(!c.providers.contains_key("runtime"));assert_eq!(c.provider_metadata_bytes,0);
        register(&mut c);assert!(c.providers["runtime"].generation>generation);assert!(!old.valid());assert_eq!(c.providers.len(),1);
    }
    #[test]
    fn captured_snapshot_is_an_immutable_epoch_sequence_fence() {
        let mut c=Core::new(Store::in_memory(),None,PathBuf::from("unused"),false).unwrap();
        let(tx,_rx)=mpsc::unbounded_channel();c.install_adapter(AdapterHandle{name:"p".into(),session:None,context:None,capabilities:json!({}),cmds:tx},true).unwrap();
        c.on_adapter_event("p",AdapterEvent::Registered(crate::provider::ProviderInfo {connection:1,instance:"i".into(),role:"backend".into(),software:json!({"name":"fixture","version":"1"})}));
        let fact=|value|serde_json::from_value(json!({"backend_ref":"a","kind":"terminal","state":"live","data":{"value":value}})).unwrap();
        let id=c.upsert_resource("p",fact(1)).unwrap();
        let txn=c.store.apply("fixture",None,vec![crate::store::Op::Set{key:"doc/a".into(),value:json!(1),expect:None}]).unwrap();c.emit_txn(&txn);
        c.live.insert("v".into(),(Arc::new(json!({"value":1})),"fixture".into(),1,10));
        let seq=c.seq;let captured=c.capture_snapshot();
        c.upsert_resource("p",fact(2)).unwrap();c.on_adapter_event("p",AdapterEvent::Connected(false,None));
        c.live.insert("v".into(),(Arc::new(json!({"value":2})),"fixture".into(),1,10));
        let txn=c.store.apply("fixture",None,vec![crate::store::Op::Set{key:"doc/a".into(),value:json!(2),expect:None}]).unwrap();c.emit_txn(&txn);
        let snapshot=captured.into_value();assert_eq!(snapshot["seq"],seq);
        assert_eq!(snapshot["entries"]["doc/a"]["value"],1);assert_eq!(snapshot["live"]["v"]["value"],1);
        assert_eq!(snapshot["resources"][0]["id"],id);assert_eq!(snapshot["resources"][0]["data"]["value"],1);assert_eq!(snapshot["resources"][0]["available"],true);
        assert_eq!(snapshot["providers"]["p"]["connected"],true);assert_eq!(snapshot["adapters"]["p"]["connected"],true);
        assert!(c.capture_snapshot().into_value_limited(64).is_err());
    }
    #[test]
    fn stale_agent_forget_reaps_resources_providers_and_never_reuses_ids() {
        let mut c=Core::new(Store::in_memory(),None,PathBuf::from("unused"),false).unwrap();let mut previous=String::new();
        for i in 0..200 {
            let name=format!("agent{i}");let(tx,mut commands)=mpsc::unbounded_channel();c.install_provider(AdapterHandle{name:name.clone(),session:None,context:None,capabilities:json!({}),cmds:tx},false,false).unwrap();
            c.on_adapter_event(&name,AdapterEvent::Registered(crate::provider::ProviderInfo{connection:1,instance:"i".into(),role:"runtime".into(),software:json!({"name":"fixture","version":"1"})}));
            let id=c.upsert_resource(&name,serde_json::from_value(json!({"backend_ref":"a","kind":"agent","state":"live"})).unwrap()).unwrap();
            assert_ne!(id,previous);assert_eq!(c.forget_cached_resource(&id).unwrap_err().kind(),"invalid");
            c.on_adapter_event(&name,AdapterEvent::Connected(false,None));c.forget_cached_resource(&id).unwrap();
            assert!(commands.try_recv().is_err(),"cache retirement never invokes the backend");
            assert!(c.resources.is_empty());assert!(c.providers.is_empty());assert_eq!(c.provider_metadata_bytes,0);assert!(!c.identities.active(&id));previous=id;
        }
    }
    #[test]
    fn cached_agent_return_keeps_id_and_config_without_native_calls_or_pause_policy() {
        let mut c=Core::new(Store::in_memory(),None,PathBuf::from("unused"),false).unwrap();
        let install=|c:&mut Core| {let(tx,rx)=mpsc::unbounded_channel();c.install_provider(AdapterHandle{name:"runtime".into(),session:None,context:None,capabilities:json!({}),cmds:tx},false,false).unwrap();c.on_adapter_event("runtime",AdapterEvent::Registered(crate::provider::ProviderInfo{connection:1,instance:"host".into(),role:"runtime".into(),software:json!({"name":"fixture","version":"1"})}));rx};
        let mut commands=install(&mut c);let fact=||serde_json::from_value(json!({"backend_ref":"endpoint","incarnation":"session","kind":"agent","state":"live"})).unwrap();
        let id=c.upsert_resource("runtime",fact()).unwrap();let key=format!("doc/agents/membership/{id}");c.store.apply("fixture",None,vec![crate::store::Op::Set{key:key.clone(),value:json!({"paused":true}),expect:None}]).unwrap();
        c.on_adapter_event("runtime",AdapterEvent::Connected(false,None));c.forget_cached_resource(&id).unwrap();assert!(commands.try_recv().is_err());assert!(c.resources.is_empty());assert_eq!(c.capture_snapshot().into_value()["identity_reservations"]["count"],1);
        let mut returning=install(&mut c);assert_eq!(c.upsert_resource("runtime",fact()).unwrap(),id);assert!(returning.try_recv().is_err());assert_eq!(c.store.get(&key).unwrap().value,json!({"paused":true}));assert_eq!(c.capture_snapshot().into_value()["identity_reservations"]["count"],0);
    }
    #[test]
    fn cached_retirement_preserves_identity_write_failure_semantics() {
        for renamed in [false,true] {
            let root=tempfile::tempdir().unwrap();let mut c=Core::new(Store::in_memory(),Some(root.path().into()),root.path().join("unused"),false).unwrap();
            let(tx,_rx)=mpsc::unbounded_channel();c.install_provider(AdapterHandle{name:"runtime".into(),session:None,context:None,capabilities:json!({}),cmds:tx},false,false).unwrap();
            c.on_adapter_event("runtime",AdapterEvent::Registered(crate::provider::ProviderInfo{connection:1,instance:"i".into(),role:"runtime".into(),software:json!({"name":"fixture","version":"1"})}));
            let id=c.upsert_resource("runtime",serde_json::from_value(json!({"backend_ref":"a","kind":"agent","state":"live"})).unwrap()).unwrap();
            c.on_adapter_event("runtime",AdapterEvent::Connected(false,None));
            c.identities.set_writer(Arc::new(move|path,bytes| {
                if renamed {crate::store::write_atomic(path,bytes)?;}
                let mut error:crate::store::AtomicWriteError=std::io::Error::other("injected retirement write").into();error.renamed=renamed;Err(error)
            }));
            let error=c.forget_cached_resource(&id).unwrap_err();
            assert_eq!(error.kind(),if renamed {"durability_unconfirmed"}else{"io"});
            assert_eq!(c.resources.contains_key(&id),!renamed);assert_eq!(c.providers.contains_key("runtime"),!renamed);
            c.identities.set_writer(Arc::new(crate::store::write_atomic));
            if !renamed {c.forget_cached_resource(&id).unwrap();}
            assert!(!IdentityMap::open(Some(root.path().join("resources.json"))).unwrap().active(&id));
        }
    }
    #[test]
    fn registry_count_and_aggregate_registration_metadata_are_bounded(){
        let mut c=Core::new(Store::in_memory(),None,PathBuf::from("unused"),false).unwrap();
        for i in 0..crate::provider::MAX_PROVIDERS{let(tx,_rx)=mpsc::unbounded_channel();c.install_provider(AdapterHandle{name:format!("p{i}"),session:None,context:None,capabilities:json!({}),cmds:tx},false,false).unwrap();}
        let(tx,_rx)=mpsc::unbounded_channel();assert_eq!(c.install_provider(AdapterHandle{name:"extra".into(),session:None,context:None,capabilities:json!({}),cmds:tx},false,false).unwrap_err().kind(),"overloaded");
        assert_eq!(c.provider_metadata_bytes,c.providers.values().map(|p|p.registration_bytes).sum::<usize>());
        let mut c=Core::new(Store::in_memory(),None,PathBuf::from("unused"),false).unwrap();let mut rejected=false;
        for i in 0..1024{let(tx,_rx)=mpsc::unbounded_channel();if let Err(error)=c.install_provider(AdapterHandle{name:format!("large{i}"),session:None,context:None,capabilities:json!({"op":{"description":"x".repeat(8192)}}),cmds:tx},false,false){assert!(error.message.contains("aggregate"));rejected=true;break;}}
        assert!(rejected);assert!(c.provider_metadata_bytes<=crate::provider::MAX_REGISTRY_METADATA_BYTES);
    }
    #[tokio::test]
    async fn identity_batch_faults_publish_only_after_adoption_and_retry_once(){
        for renamed in [false,true] {
            let root=tempfile::tempdir().unwrap();let mut c=Core::new(Store::in_memory(),Some(root.path().into()),root.path().join("unused"),false).unwrap();let(tx,_rx)=mpsc::unbounded_channel();
            c.install_adapter(AdapterHandle{name:"fault".into(),session:None,context:None,capabilities:json!({}),cmds:tx},true).unwrap();
            c.on_adapter_event("fault",AdapterEvent::Registered(crate::provider::ProviderInfo{connection:1,instance:"A".into(),role:"backend".into(),software:json!({"name":"fixture","version":"1"})}));
            c.identities.set_writer(Arc::new(move|path,bytes|{if renamed{crate::store::write_atomic(path,bytes)?;}let mut error:crate::store::AtomicWriteError=std::io::Error::other("injected identity write").into();error.renamed=renamed;Err(error)}));
            let facts=(0..4).map(|i|serde_json::from_value(json!({"backend_ref":format!("f{i}"),"kind":"terminal","state":"live"})).unwrap()).collect::<Vec<_>>();
            let before=c.identities.writes();let error=c.upsert_resources("fault",facts.clone(),true).unwrap_err();assert_eq!(error.kind(),if renamed{"durability_unconfirmed"}else{"io"});
            assert_eq!(c.resources.len(),if renamed{4}else{0});assert_eq!(c.identities.writes()-before,1);if !renamed{assert_eq!(c.pending_facts["fault"].len(),4);}
            c.identities.set_writer(Arc::new(crate::store::write_atomic));let ids=c.upsert_resources("fault",facts,true).unwrap();assert_eq!(ids,vec!["r1","r2","r3","r4"]);assert_eq!(c.identities.writes()-before,2);
            assert_eq!(c.resources.len(),4);assert!(c.pending_facts.values().all(|p|p.is_empty()));assert!(c.provider_persistence_error("fault").is_none());
        }
    }
    #[tokio::test]
    async fn inventory_is_one_durable_write_with_a_bounded_core_lock_hold(){
        let root=tempfile::tempdir().unwrap();let core=Arc::new(Mutex::new(Core::new(Store::in_memory(),Some(root.path().into()),root.path().join("unused"),false).unwrap()));
        let(tx,_rx)=mpsc::unbounded_channel();let mut c=core.lock().unwrap();
        c.install_adapter(AdapterHandle{name:"batch".into(),session:None,context:None,capabilities:json!({}),cmds:tx},true).unwrap();
        c.on_adapter_event("batch",AdapterEvent::Registered(crate::provider::ProviderInfo{connection:1,instance:"A".into(),role:"backend".into(),software:json!({"name":"fixture","version":"1"})}));
        let facts=(0..600).map(|i|serde_json::from_value(json!({"backend_ref":format!("t{i}"),"incarnation":"birth","kind":"terminal","state":"live","argv":["sh"]})).unwrap()).collect();
        let before=c.identities.writes();let started=crate::test_timing::WorkClock::start();c.on_adapter_event("batch",AdapterEvent::Snapshot(facts));let elapsed=started.finish();
        assert_eq!(c.resources.len(),600);assert_eq!(c.identities.writes()-before,1,"one durable map write per complete inventory");
        println!("identity inventory600 lock_work_cpu_ms={:.3} wall_lock_hold_ms={:.3}",elapsed.cpu.as_secs_f64()*1000.0,elapsed.wall.as_secs_f64()*1000.0);
        elapsed.assert_cpu_budget(std::time::Duration::from_millis(150),"inventory under core lock");
        let before=c.identities.writes();c.on_adapter_event("batch",AdapterEvent::Snapshot(Vec::new()));assert_eq!(c.identities.writes()-before,1);assert!(c.resources.is_empty());
    }
}
