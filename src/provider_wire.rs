//! Socket-backed providers. All backend/runtime policy stays on the other end of the connection.
//! This module adapts pinned provider-v0 messages to the same routes native adapters use.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Instant;
pub use crate::bridge_tokens::BridgeReservation;
use base64::Engine;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::{mpsc, oneshot, watch, OwnedSemaphorePermit, Semaphore};
use crate::adapter::{self, AdapterCmd, AdapterError, AdapterHandle, EffectContext, EffectResult, ResourceFacts, Stream, StreamEnd, StreamInput, StreamOutput};
use crate::core::{Core, Shared};
use crate::provider::{self, ProviderInfo};
use crate::rpc::{params, Peer, RpcError, RpcResult};

pub const MAX_RECORD_BYTES: usize = 1024 * 1024;
pub use crate::provider::MAX_PROVIDERS;
pub const MAX_REGISTRATION_BYTES: usize = 128 * 1024;
pub use crate::resource_admission::{encoded_bytes, MAX_FACT_BYTES, MAX_RESOURCES};
const RECENT_EFFECTS: usize = 256;
const MAX_INFLIGHT: usize = 128;
const CONTROL_BYTES: usize = 4 * 1024 * 1024;
const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

fn invalid(message: impl Into<String>) -> RpcError { RpcError::invalid(message) }
fn overloaded(message: impl Into<String>) -> RpcError { provider::rejected("overloaded", message) }
fn reason(value: &Value) -> Result<Value, RpcError> {
    crate::limits::json_depth(value,crate::limits::JSON_DEPTH)?;
    if value.is_object() && value["reason"].is_string() { Ok(value.clone()) } else { Err(invalid("reason must be an object with a string reason")) }
}

#[derive(Deserialize)]
struct Registration {
    name: String,
    role: String,
    instance: String,
    software: Software,
    provider_version: String,
    capabilities: BTreeMap<String, Value>,
    #[serde(default)] limits: Limits,
    token: Option<String>,
}
#[derive(Deserialize, serde::Serialize)] struct Software { name: String, version: String }
#[derive(Deserialize)]
struct Limits {
    #[serde(default = "default_streams")] max_streams: usize,
    #[serde(default = "default_window")] stream_window_bytes: usize,
}
fn default_streams() -> usize { 512 }
fn default_window() -> usize { adapter::STREAM_BUFFER_BYTES }
impl Default for Limits { fn default() -> Self { Self { max_streams: default_streams(), stream_window_bytes: default_window() } } }

#[derive(Deserialize)]
struct WireFacts {
    #[serde(rename = "ref")] reference: String,
    #[serde(default)] incarnation: String,
    kind: String,
    state: String,
    name: Option<String>, title: Option<String>, cwd: Option<String>,
    #[serde(default)] argv: Vec<String>,
    #[serde(default)] status: Value,
    #[serde(default)] exit: Value,
    #[serde(default = "empty_object")] data: Value,
}
fn empty_object() -> Value { json!({}) }
pub fn parse_facts(value: Value) -> Result<ResourceFacts, RpcError> {
    crate::limits::json_depth(&value,crate::limits::JSON_DEPTH)?;
    if encoded_bytes(&value) > MAX_FACT_BYTES { return Err(overloaded("whole facts record exceeds 128 KiB")); }
    let f: WireFacts = params(value)?;
    let facts = ResourceFacts { backend_ref: f.reference, incarnation: f.incarnation, kind: f.kind, state: f.state,
        name: f.name, title: f.title, cwd: f.cwd, argv: f.argv, status: f.status, exit: f.exit, data: f.data };
    if !facts.valid() || !(facts.status.is_null() || facts.status.is_object()) || !(facts.exit.is_null() || facts.exit.is_object()) { return Err(invalid("invalid resource facts")); }
    Ok(facts)
}

struct Inventory {
    started: bool,
    syncing: bool,
    seen: BTreeMap<String, ResourceFacts>,
    current: BTreeMap<String, ResourceFacts>,
    effects: BTreeSet<String>,
    recent_effects: VecDeque<String>,
}
/// IDs are core-generated and never resent. Retain active calls plus a small duplicate window,
/// not every effect ever seen by a healthy long-lived provider. Drop also covers cancellation.
struct EffectLease<'a> { inventory:&'a Mutex<Inventory>, effect:&'a str }
impl Drop for EffectLease<'_> {
    fn drop(&mut self) {
        let mut inventory=self.inventory.lock().unwrap();
        inventory.recent_effects.push_back(self.effect.to_string());
        while inventory.recent_effects.len()>RECENT_EFFECTS {
            if let Some(old)=inventory.recent_effects.pop_front() { inventory.effects.remove(&old); }
        }
    }
}
struct Credit { offset: u64, outstanding: usize }
struct WireStream {
    output: crate::queue::Sender<StreamOutput>,
    terminal: Option<oneshot::Sender<StreamEnd>>,
    credit: Arc<Mutex<Credit>>,
    _slot: OwnedSemaphorePermit,
}

struct Opening { provider: Weak<WireProvider>, id: u64, armed: bool }
impl Drop for Opening {
    fn drop(&mut self) {
        if self.armed { if let Some(provider) = self.provider.upgrade() { provider.end(self.id, false, json!({"reason": "closed"}), true); } }
    }
}

pub struct Connection {
    id: u64,
    peer: Peer,
    hello: AtomicBool,
    provider: Mutex<Option<Arc<WireProvider>>>,
}

pub struct WireProvider {
    name: String,
    generation: u64,
    source: u64,
    connection: u64,
    peer: Peer,
    core: Weak<Mutex<Core>>,
    window: usize,
    streams: Mutex<HashMap<u64, WireStream>>,
    stream_slots: Arc<Semaphore>,
    control: Arc<Semaphore>,
    inflight: Arc<Semaphore>,
    next_stream: AtomicU64,
    inventory: Mutex<Inventory>,
    stopped: watch::Sender<bool>,
    health: watch::Receiver<crate::provider::Health>,
}

#[cfg(test)]
#[path = "provider_wire_lifecycle_tests.rs"]
mod lifecycle_tests;

impl Connection {
    pub fn new(id: u64, peer: Peer) -> Self { Self { id, peer, hello: AtomicBool::new(false), provider: Mutex::new(None) } }
    pub fn is_provider(&self) -> bool { self.provider.lock().unwrap().is_some() }
    pub fn hello(&self) -> Result<(), RpcError> {
        if self.is_provider() { return Err(RpcError::new("conflict", "provider connection already registered")); }
        self.hello.store(true, Ordering::Release);
        Ok(())
    }
    pub fn validate(&self, core: &Shared, p: &Value) -> Result<Arc<WireProvider>, RpcError> {
        let provider = self.provider.lock().unwrap().clone().ok_or_else(|| invalid("provider must register first"))?;
        if p["generation"].as_u64() != Some(provider.generation) || !provider.current(core) {
            return Err(RpcError::new("stale_generation", "provider generation does not match this connection"));
        }
        Ok(provider)
    }
    pub async fn register(&self, core: &Shared, p: Value, owner: Option<crate::ext::Owner>) -> RpcResult {
        let gate=core.lock().unwrap().registration_slots.clone();
        let _slot=gate.try_acquire_owned().map_err(|_|overloaded("registration concurrency limit reached; retry later"))?;
        if !self.hello.load(Ordering::Acquire) { return Err(invalid("hello must precede provider.register")); }
        if self.is_provider() { return Err(RpcError::new("conflict", "one registration per connection")); }
        crate::limits::json_depth(&p,crate::limits::JSON_DEPTH)?;
        if encoded_bytes(&p) > MAX_REGISTRATION_BYTES { return Err(overloaded("registration exceeds 128 KiB")); }
        let r: Registration = params(p)?;
        if r.name.is_empty() || r.name.len() > 128 || r.instance.is_empty() || r.instance.len() > 256
            || !matches!(r.role.as_str(), "backend" | "runtime") || r.software.name.is_empty() || r.provider_version.is_empty()
            || !r.capabilities.values().all(Value::is_object) || r.limits.max_streams == 0 || r.limits.stream_window_bytes == 0 {
            return Err(invalid("invalid provider registration, capabilities or limits"));
        }
        let claim = if r.role == "runtime" { Some(crate::bridge_tokens::claim(core, &self.peer, r.token.as_deref().ok_or_else(|| RpcError::new("forbidden", "runtime provider requires a token"))?).await?) } else { None };
        let host = claim.as_ref().map(|claim| claim.host.clone());
        self.peer.set_max_record_bytes(MAX_RECORD_BYTES)?;
        let (commands, inbox) = mpsc::unbounded_channel();
        let mut c = core.lock().unwrap();
        // Revalidate both stop and recovery admission after a pending host-token wait.
        if c.shutdown.is_requested() { return Err(provider::rejected("unavailable","core stopping; provider registration not admitted")); }
        if owner.as_ref().is_some_and(|owner| !crate::ext::owner_valid(&c,owner)) { return Err(crate::provider::rejected("forbidden","supervised extension lifetime ended before registration")); }
        if c.safe_mode && owner.as_ref().is_none_or(|owner| !c.safe_mode_allow.contains(&owner.name)) {
            let mut error = crate::provider::rejected("forbidden", "safe mode requires an explicitly allowed extension owner");
            error.data.as_mut().unwrap()["reason"] = json!("safe_mode");
            return Err(error);
        }
        if self.is_provider() { return Err(RpcError::new("conflict", "one registration per connection")); }
        if let Some(claim) = &claim {
            if !c.bridge_tokens.valid_claim(r.token.as_deref().unwrap(), claim, &r.name, &r.instance) { return Err(RpcError::new("forbidden", "runtime resume identity mismatch or token used")); }
        }
        let resuming = claim.as_ref().is_some_and(|claim| claim.resuming);
        if c.providers.get(&r.name).is_some_and(|p| p.health.borrow().connected)
            && !(resuming && c.providers[&r.name].metadata["host"].as_str() == host.as_deref()) {
            return Err(RpcError::new("conflict", "provider name already has a live connection"));
        }
        let window=r.limits.stream_window_bytes.min(default_window());
        let metadata=json!({"provider_version":r.provider_version,"limits":{"max_streams":r.limits.max_streams.min(512),"stream_window_bytes":window,"max_record_bytes":MAX_RECORD_BYTES},"host":host,"owner_extension":owner.as_ref().map(|o|&o.name)});
        let info=ProviderInfo{connection:self.id,instance:r.instance.clone(),role:r.role.clone(),software:json!(r.software)};
        let charge=provider::registration_charge(&r.name,&json!(r.capabilities),Some(&info),&metadata);c.admit_provider_registration(&r.name,charge)?;
        let resume_token = if let Some(claim) = &claim {
            if !c.has_resource_identity(&claim.host) { return Err(RpcError::new("forbidden", "runtime host removed")); }
            Some(c.bridge_tokens.rotate(r.token.as_deref().unwrap(), claim, &r.name, &r.instance)?)
        } else { None };
        let handle = AdapterHandle { name: r.name.clone(), capabilities: json!(r.capabilities), session: None, context: None, cmds: commands };
        // The default backend is the one whose extension entry says so (`extension/<name>`
        // `{kind: "provider", default: true}`), else the first backend; never a runtime bridge.
        let backend = r.role == "backend";
        let preferred = backend && c.store.get(&format!("extension/{}", r.name)).is_some_and(|e| e.value["default"] == true);
        let source = c.install_provider(handle, preferred, backend)?;
        c.providers.get_mut(&r.name).unwrap().metadata=metadata;
        c.on_adapter_event(&r.name,adapter::AdapterEvent::Registered(info));
        let generation=c.providers[&r.name].generation;
        if let Some(owner) = owner { c.provider_extensions.insert(owner.name); }
        // Registered already published this exact state (including compatibility alias).
        let health = c.providers[&r.name].health.subscribe();
        let (stopped, _) = watch::channel(false);
        let provider = Arc::new(WireProvider { name: r.name, generation, source, connection: self.id,
            peer: self.peer.clone(), core: Arc::downgrade(core), window, streams: Mutex::new(HashMap::new()),
            stream_slots: Arc::new(Semaphore::new(r.limits.max_streams.min(512))), control: Arc::new(Semaphore::new(CONTROL_BYTES)),
            inflight: Arc::new(Semaphore::new(MAX_INFLIGHT)), next_stream: AtomicU64::new(1),
            inventory: Mutex::new(Inventory { started: false, syncing: false, seen: BTreeMap::new(), current: BTreeMap::new(), effects: BTreeSet::new(), recent_effects:VecDeque::new() }), stopped, health });
        *self.provider.lock().unwrap() = Some(provider.clone());
        tokio::spawn(provider.clone().run(inbox));
        let mut result = json!({"provider": provider.name, "generation": generation, "limits": {"stream_window_bytes": window, "max_record_bytes": MAX_RECORD_BYTES}});
        if let Some(token) = resume_token { result["resume_token"] = json!(token); }
        Ok(result)
    }
    pub fn sync(&self, core: &Shared, p: Value) -> RpcResult {
        let provider = self.validate(core, &p)?;
        provider.sync(core, p)
    }
    pub fn notification(&self, core: &Shared, method: &str, p: Value) -> Result<(), RpcError> {
        crate::limits::json_depth(&p,crate::limits::MESSAGE_DEPTH)?;
        let provider = if method == "resource.gone" {
            // Peer EOF can disable execution routes before connection() drains
            // complete accepted records. Retirement is monotonic, but must still
            // belong to this generation/source (checked atomically below).
            let provider = self.provider.lock().unwrap().clone().ok_or_else(|| invalid("provider must register first"))?;
            if p["generation"].as_u64() != Some(provider.generation) {
                return Err(RpcError::new("stale_generation", "provider generation does not match this connection"));
            }
            provider
        } else { self.validate(core, &p)? };
        provider.notification(core, method, p)
    }
    pub fn disconnect(&self, core: &Shared) {
        let provider = self.provider.lock().unwrap().take();
        if let Some(provider) = provider { provider.disconnect(core); }
    }
}

impl WireProvider {
    fn current_locked(&self, core:&Core) -> bool { core.providers.get(&self.name).is_some_and(|p| p.source == self.source && p.generation == self.generation && p.health.borrow().connected) }
    fn current(&self, core: &Shared) -> bool { self.current_locked(&core.lock().unwrap()) }
    fn check_capacity(inventory:&Inventory, facts:&[ResourceFacts]) -> Result<(),RpcError> {
        let additional:BTreeSet<_>=facts.iter().filter(|f| !inventory.current.contains_key(&f.backend_ref)).map(|f|f.backend_ref.as_str()).collect();
        if inventory.current.len().saturating_add(additional.len())>MAX_RESOURCES { return Err(overloaded("provider resource count exceeds 4096")); }
        Ok(())
    }
    fn record_locked(inventory:&mut Inventory,facts:ResourceFacts) {
        inventory.current.insert(facts.backend_ref.clone(),facts.clone());
        if inventory.syncing { inventory.seen.insert(facts.backend_ref.clone(),facts); }
    }
    fn record(&self, facts: ResourceFacts, creation:bool) -> Result<(), RpcError> {
        let mut inventory = self.inventory.lock().unwrap();
        // Creation is not a newer publication. In particular it must not replace an already-seen
        // sync row, or the final chunk would roll Core back after record_spawn preserved its facts.
        if creation && inventory.current.get(&facts.backend_ref).is_some_and(|current|current.incarnation==facts.incarnation) {return Ok(());}
        Self::check_capacity(&inventory,std::slice::from_ref(&facts))?;
        Self::record_locked(&mut inventory,facts);
        Ok(())
    }
    fn publish_locked(&self,core:&Shared,inventory:&mut Inventory,facts:ResourceFacts) -> Result<(),RpcError> {
        // Inventory admission and publication are one critical section: concurrent effect
        // observations cannot consume the last local slot between checking and recording.
        let mut c=core.lock().unwrap();
        if !self.current_locked(&c) { return Err(RpcError::new("stale_generation","provider changed before facts publication")); }
        if let Err(error)=c.upsert_resource(&self.name,facts.clone()) {
            if !matches!(error.kind(),"io"|"durability_unconfirmed") { return Err(error); }
        }
        Self::record_locked(inventory,facts);
        Ok(())
    }
    fn sync(&self, core: &Shared, p: Value) -> RpcResult {
        let final_chunk = p["final"].as_bool().ok_or_else(|| invalid("sync requires final boolean"))?;
        let records = p["resources"].as_array().ok_or_else(|| invalid("sync requires resources array"))?;
        let facts: Vec<_> = records.iter().cloned().map(parse_facts).collect::<Result<_, _>>()?;
        let mut inventory=self.inventory.lock().unwrap();
        Self::check_capacity(&inventory,&facts)?;
        if !inventory.syncing { inventory.seen.clear(); inventory.syncing=true; }
        inventory.started=true;
        // The final chunk includes authoritative removals in this same durable batch, not a second write.
        let records=if final_chunk {let mut all=inventory.seen.clone();for f in &facts{all.insert(f.backend_ref.clone(),f.clone());}all.into_values().collect()}else{facts.clone()};
        {
            let mut c=core.lock().unwrap();
            if !self.current_locked(&c){return Err(RpcError::new("stale_generation","provider changed before sync publication"));}
            if let Err(error)=c.upsert_resources(&self.name,records,final_chunk){if !matches!(error.kind(),"io"|"durability_unconfirmed"){return Err(error);}}
        }
        for facts in facts {Self::record_locked(&mut inventory,facts);}
        if let Some(error)=core.lock().unwrap().provider_persistence_error(&self.name) { return Err(error); }
        if final_chunk {
            inventory.current=std::mem::take(&mut inventory.seen);
            inventory.syncing=false;
        }
        Ok(json!({}))
    }
    fn notification(&self, core: &Shared, method: &str, p: Value) -> Result<(), RpcError> {
        match method {
            "resource.upsert" => {
                let facts=parse_facts(p.get("facts").cloned().ok_or_else(||invalid("upsert requires facts"))?)?;
                let mut inventory=self.inventory.lock().unwrap();
                if !inventory.started { return Err(invalid("first sync must start before upsert")); }
                Self::check_capacity(&inventory,std::slice::from_ref(&facts))?;
                self.publish_locked(core,&mut inventory,facts)?;
            }
            "resource.gone" => {
                let reference = p["ref"].as_str().ok_or_else(|| invalid("gone requires ref"))?.to_string();
                let incarnation = p["incarnation"].as_str().ok_or_else(|| invalid("gone requires incarnation"))?.to_string();
                reason(&p["reason"])?;
                {
                    let mut inventory = self.inventory.lock().unwrap();
                    if inventory.current.get(&reference).is_some_and(|f| f.incarnation == incarnation) { inventory.current.remove(&reference); }
                    if inventory.seen.get(&reference).is_some_and(|f| f.incarnation == incarnation) { inventory.seen.remove(&reference); }
                }
                let mut c = core.lock().unwrap();
                if !c.providers.get(&self.name).is_some_and(|p| p.source == self.source && p.generation == self.generation) {
                    return Err(RpcError::new("stale_generation", "provider changed before retirement publication"));
                }
                c.on_adapter_event(&self.name, adapter::AdapterEvent::Removed { reference, incarnation });
            }
            "stream.data" | "stream.closed" | "stream.gap" | "stream.error" => self.stream_message(method, p)?,
            _ => return Err(RpcError::method_not_found(method)),
        }
        Ok(())
    }
    fn stream_message(&self, method: &str, p: Value) -> Result<(), RpcError> {
        let id = p["stream"].as_u64().ok_or_else(|| invalid("stream id must be u64"))?;
        // Closed or unknown stream messages are deliberately ignored, including stale late tails.
        if !self.streams.lock().unwrap().contains_key(&id) { return Ok(()); }
        match method {
            "stream.data" => {
                let encoded = p["data"].as_str().ok_or_else(|| invalid("stream data must be base64"))?;
                if encoded.len() > (adapter::MAX_STREAM_CHUNK + 2) / 3 * 4 { self.end(id, false, json!({"reason": "invalid", "message": "data chunk exceeds 64 KiB"}), true); return Ok(()); }
                let data = B64.decode(encoded).map_err(|_| invalid("invalid base64 stream data"))?;
                let streams = self.streams.lock().unwrap();
                let Some(stream) = streams.get(&id) else { return Ok(()) };
                let mut credit = stream.credit.lock().unwrap();
                if data.len() > adapter::MAX_STREAM_CHUNK || p["offset"].as_u64() != Some(credit.offset) || credit.outstanding.saturating_add(data.len()) > self.window {
                    drop(credit); drop(streams);
                    self.end(id, false, json!({"reason": "invalid", "message": "noncontiguous offset or exceeded credit"}), true);
                    return Ok(());
                }
                credit.offset = credit.offset.checked_add(data.len() as u64).ok_or_else(|| invalid("stream offset overflow"))?;
                credit.outstanding += data.len();
                let n = data.len();
                let failed = stream.output.try_send(StreamOutput::Data(data), n).is_err();
                drop(credit); drop(streams);
                if failed { self.end(id, false, json!({"reason": "slow_consumer"}), true); }
            }
            "stream.closed" | "stream.gap" => {
                let why = reason(&p["reason"])?;
                if method == "stream.gap" {
                    let expected = self.streams.lock().unwrap().get(&id).map(|s| s.credit.lock().unwrap().offset);
                    if p["offset"].as_u64() != expected { self.end(id, false, json!({"reason": "invalid", "message": "gap offset mismatch"}), true); return Ok(()); }
                }
                self.end(id, method == "stream.gap", why, false);
            }
            "stream.error" => {
                crate::limits::json_depth(&p["error"],crate::limits::JSON_DEPTH)?;
                if p["outcome"] != "rejected" || !p["error"]["kind"].is_string() || !p["error"]["message"].is_string() { return Err(invalid("stream.error requires rejected and error kind/message")); }
                let streams = self.streams.lock().unwrap();
                let Some(stream) = streams.get(&id) else { return Ok(()) };
                let bytes = encoded_bytes(&p["error"]);
                if stream.output.try_send(StreamOutput::Error(p["error"].clone()), bytes).is_err() { drop(streams); self.end(id, false, json!({"reason": "slow_consumer"}), true); }
            }
            _ => unreachable!(),
        }
        Ok(())
    }
    /// End stream `id` if it is still open; returns whether it was.
    fn end(&self, id: u64, gap: bool, why: Value, tell_provider: bool) -> bool {
        let removed = self.streams.lock().unwrap().remove(&id);
        let ended = removed.is_some();
        if let Some(mut stream) = removed {
            let bytes = encoded_bytes(&why).max(64);
            match self.control.clone().try_acquire_many_owned(bytes.min(u32::MAX as usize) as u32) {
                Ok(permit) => { if let Some(reply) = stream.terminal.take() { let _ = reply.send(StreamEnd { gap, reason: why, _permit: Some(permit) }); } }
                Err(_) => self.peer.close(),
            }
            if tell_provider { self.peer.notify("stream.close", json!({"generation": self.generation, "stream": id})); }
        }
        ended
    }
    fn disconnect(&self, core: &Shared) {
        self.stopped.send_replace(true);
        self.stream_slots.close();
        self.inflight.close();
        self.streams.lock().unwrap().clear();
        let mut c = core.lock().unwrap();
        if c.providers.get(&self.name).is_some_and(|p| p.source == self.source && p.health.borrow().connected) {
            c.on_adapter_event(&self.name, adapter::AdapterEvent::Connected(false, Some("provider connection closed".into())));
        }
    }
    async fn run(self: Arc<Self>, mut commands: mpsc::UnboundedReceiver<AdapterCmd>) {
        let mut stopped = self.stopped.subscribe();
        let mut health = self.health.clone();
        loop {
            let cmd = tokio::select! { biased; _ = stopped.changed() => break, _ = health.changed() => { self.peer.close(); break; }, _ = self.peer.closed() => break, cmd = commands.recv() => match cmd { Some(cmd) => cmd, None => break } };
            let (context, cmd) = match cmd {
                AdapterCmd::Shutdown(reply) => { self.peer.close(); let _ = reply.send(Ok(())); break; }
                AdapterCmd::Bound { connection, context, command } if connection == self.connection => (context, *command),
                stale => { adapter::reject_command(stale, "wire provider connection changed"); continue; }
            };
            let Some(context) = context else { adapter::reject_command(cmd, "wire effects require a core context"); continue };
            let Some(cmd) = adapter::command_for(cmd, self.connection) else { continue };
            let Ok(slot) = self.inflight.clone().try_acquire_owned() else { adapter::reject_command(cmd, "provider in-flight effect limit reached"); continue };
            let provider = self.clone();
            tokio::spawn(async move { let _slot = slot; provider.command(context, cmd).await; });
        }
        if let Some(core) = self.core.upgrade() { self.disconnect(&core); }
    }
    async fn command(self: Arc<Self>, context: EffectContext, cmd: AdapterCmd) {
        match cmd {
            AdapterCmd::Shutdown(reply) => { self.peer.close(); let _ = reply.send(Ok(())); }
            AdapterCmd::Spawn(req, reply) => {
                let mut args = json!({"argv": req.argv, "env": req.env});
                if let Some(cwd) = req.cwd { args["cwd"] = json!(cwd); }
                if let Some(name) = req.name { args["name"] = json!(name); }
                if let Some(cols) = req.cols { args["cols"] = json!(cols); }
                if let Some(rows) = req.rows { args["rows"] = json!(rows); }
                if let Some(placement) = req.placement { args["placement"] = placement; }
                let result = self.invoke(&context, "terminal.spawn", None, args).await.and_then(|r| {
                    if r.outcome != "observed" { return Err(AdapterError::Backend("terminal.spawn must return observed facts".into())); }
                    r.facts.ok_or_else(|| AdapterError::Backend("terminal.spawn omitted facts".into()))
                });
                let _ = reply.send(result);
            }
            AdapterCmd::Kill(reference, reply) => { let _ = reply.send(self.invoke(&context, "terminal.kill", Some((reference, context.target.as_ref().map(|t| t.1.clone()).unwrap_or_default())), json!({})).await); }
            AdapterCmd::Invoke { op, reference, incarnation, args, reply } => { let _ = reply.send(self.invoke(&context, &op, Some((reference, incarnation)), args).await); }
            AdapterCmd::Attach { backend_ref, cols, rows, history, reply } => { let _ = reply.send(self.open(&context, backend_ref, cols, rows, history).await); }
            AdapterCmd::Bound { .. } => adapter::reject_command(cmd, "nested provider binding"),
        }
    }
    async fn invoke(&self, context: &EffectContext, op: &str, target: Option<(String, String)>, args: Value) -> Result<EffectResult, AdapterError> {
        let remaining=context.deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() { return Err(AdapterError::RejectedKind { kind:"timeout".into(),message:"effect deadline expired before provider dispatch".into() }); }
        {
            let mut inventory=self.inventory.lock().unwrap();
            if inventory.effects.contains(&context.effect) { return Err(AdapterError::RejectedKind { kind:"conflict".into(),message:"duplicate effect id".into() }); }
            if inventory.effects.len().saturating_sub(inventory.recent_effects.len())>=MAX_INFLIGHT { return Err(AdapterError::RejectedKind { kind:"overloaded".into(),message:"provider in-flight effect limit reached".into() }); }
            inventory.effects.insert(context.effect.clone());
        }
        let _effect=EffectLease { inventory:&self.inventory,effect:&context.effect };
        let mut p = json!({"generation": self.generation, "effect": context.effect, "op": op, "args": args, "deadline_ms": remaining.as_millis().clamp(1, 300000) as u64});
        if let Some((reference, incarnation)) = target { p["ref"] = json!(reference); p["incarnation"] = json!(incarnation); }
        if encoded_bytes(&p) + 128 > MAX_RECORD_BYTES { return Err(AdapterError::RejectedKind { kind: "overloaded".into(), message: "provider effect record exceeds 1 MiB".into() }); }
        let remaining=context.deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() { return Err(AdapterError::RejectedKind { kind:"timeout".into(),message:"effect deadline expired before provider dispatch".into() }); }
        p["deadline_ms"]=json!(remaining.as_millis().clamp(1,300000) as u64);
        let response = self.peer.call_with_timeout("effect.invoke", p, remaining).await;
        if Instant::now() >= context.deadline { return Err(AdapterError::Timeout("provider effect deadline expired; outcome indeterminate".into())); }
        let r = response.map_err(provider_error)?;
        let outcome = match r["outcome"].as_str() {
            Some("accepted") => "accepted", Some("observed") => "observed", Some("dispatched") => "dispatched",
            Some("rejected") => {
                let kind = r["error"]["kind"].as_str().ok_or_else(|| AdapterError::Backend("rejected result omitted error kind".into()))?;
                let message = r["error"]["message"].as_str().ok_or_else(|| AdapterError::Backend("rejected result omitted message".into()))?;
                return Err(AdapterError::RejectedKind { kind: kind.into(), message: message.into() });
            }
            _ => return Err(AdapterError::Backend("invalid provider effect outcome".into())),
        };
        let data = r.get("data").cloned().unwrap_or(Value::Null);
        let facts = if matches!(op, "terminal.spawn" | "terminal.kill") {
            match data.get("facts") {
                Some(facts) => { let facts = parse_facts(facts.clone()).map_err(|e| AdapterError::Backend(e.to_string()))?; self.record(facts.clone(),op=="terminal.spawn").map_err(|e| AdapterError::Backend(e.to_string()))?; Some(facts) }
                None => None,
            }
        } else { None };
        Ok(EffectResult { outcome, data, facts })
    }
    async fn open(self: &Arc<Self>, context: &EffectContext, reference: String, cols: u16, rows: u16, history: u32) -> Result<Stream, AdapterError> {
        let slot = self.stream_slots.clone().try_acquire_owned().map_err(|_| AdapterError::RejectedKind { kind: "overloaded".into(), message: "provider max_streams reached".into() })?;
        let id = self.next_stream.fetch_add(1, Ordering::Relaxed);
        let (output, receiver) = crate::queue::channel(adapter::STREAM_BUFFER_BYTES);
        let (input, mut incoming) = crate::queue::channel(adapter::STREAM_BUFFER_BYTES);
        let (terminal, end) = oneshot::channel();
        let credit = Arc::new(Mutex::new(Credit { offset: 0, outstanding: 0 }));
        self.streams.lock().unwrap().insert(id, WireStream { output, terminal: Some(terminal), credit: credit.clone(), _slot: slot });
        let mut opening = Opening { provider: Arc::downgrade(self), id, armed: true };
        let remaining = context.deadline.saturating_duration_since(Instant::now());
        let result = self.peer.call_with_timeout("stream.open", json!({"generation": self.generation, "stream": id, "ref": reference,
            "incarnation": context.target.as_ref().map(|t| t.1.as_str()).unwrap_or(""), "cols": cols, "rows": rows,
            "history_lines": history, "window_bytes": self.window}), remaining).await;
        if Instant::now() >= context.deadline { return Err(AdapterError::Timeout("provider stream.open deadline expired; outcome indeterminate".into())); }
        let initial = match result.as_ref().ok().and_then(|r| r["initial"].as_str()) { Some("history") => "history", Some("repaint") => "repaint", Some("none") => "none", _ => {
            self.end(id, false, json!({"reason": "closed"}), true);
            return Err(result.err().map(provider_error).unwrap_or_else(|| AdapterError::Backend("stream.open returned invalid initial".into())));
        }};
        let provider = self.clone();
        let mut stopped = self.stopped.subscribe();
        tokio::spawn(async move {
            loop {
                let item = tokio::select! { biased; _ = stopped.changed() => break, input = incoming.recv() => match input { Some(input) => input, None => break } };
                match item.value {
                    StreamInput::Data(data) => provider.peer.notify("stream.input", json!({"generation": provider.generation, "stream": id, "data": B64.encode(data)})),
                    StreamInput::Resize(cols, rows) => provider.peer.notify("stream.resize", json!({"generation": provider.generation, "stream": id, "cols": cols, "rows": rows})),
                    StreamInput::Consumed(n) => {
                        if provider.streams.lock().unwrap().contains_key(&id) {
                            let mut state = credit.lock().unwrap();
                            let n = (n as usize).min(state.outstanding);
                            state.outstanding -= n;
                            drop(state);
                            if n > 0 { provider.peer.notify("stream.credit", json!({"generation": provider.generation, "stream": id, "bytes": n})); }
                        }
                    }
                    StreamInput::Close => break,
                }
            }
            // The viewer's input side is done. If the provider already ended this stream's output (a gap
            // or close), it still delivers the input forwarded above until it hears this close.
            if !provider.end(id, false, json!({"reason": "closed"}), true) {
                provider.peer.notify("stream.close", json!({"generation": provider.generation, "stream": id}));
            }
        });
        opening.armed = false;
        Ok(Stream { initial, input, output: receiver, terminal: Some(end) })
    }
}

/// Provider JSON-RPC errors that mean the provider did nothing (provider-v0 9.1, 9.4): protocol
/// problems, refused before any effect. They are definite rejections with the provider's kind.
const NO_EFFECT_KINDS: &[&str] = &[
    "invalid", "unknown_method", "unsupported", "not_found", "stale_generation", "conflict", "forbidden", "overloaded",
    "too_large",
];

/// Map a provider's JSON-RPC error. Protocol errors are rejections with no effect; only an unknown
/// outcome stays indeterminate: the provider's own `backend` error (it may have acted), a lost
/// connection, a deadline, or a kind this core does not know.
fn provider_error(error: RpcError) -> AdapterError {
    match error.kind() {
        "disconnected" | "unavailable" => AdapterError::Indeterminate(error.to_string()),
        "timeout" => AdapterError::Timeout(error.to_string()),
        kind if NO_EFFECT_KINDS.contains(&kind) => AdapterError::RejectedKind { kind: kind.to_string(), message: error.message },
        _ => AdapterError::Backend(error.to_string()),
    }
}

#[cfg(test)]
mod qa2_tests {
    use super::*;
    async fn fixture() -> (Shared,Arc<WireProvider>,Peer,crate::rpc::IncomingReceiver) {
        let core=Arc::new(Mutex::new(Core::new(crate::store::Store::in_memory(),None,"unused".into(),false).unwrap()));
        let (a,b)=tokio::io::duplex(4096);
        let (r,w)=tokio::io::split(a); let (local,_incoming)=Peer::spawn(r,w);
        let (r,w)=tokio::io::split(b); let (remote,incoming)=Peer::spawn(r,w);
        let connection=Connection::new(1,local);
        connection.hello().unwrap();
        connection.register(&core,json!({"name":"qa2","role":"backend","instance":"i","software":{"name":"fixture","version":"1"},"provider_version":"0","capabilities":{"terminal.output":{"raw":true}}}),None).await.unwrap();
        let provider=connection.provider.lock().unwrap().clone().unwrap();
        (core,provider,remote,incoming)
    }
    #[tokio::test]
    async fn registration_concurrency_rejects_without_consuming_identity_or_mutating_registry() {
        let (core,_provider,remote,_incoming)=fixture().await;
        let (a,_b)=tokio::io::duplex(4096);let(r,w)=tokio::io::split(a);let(peer,_rx)=Peer::spawn(r,w);
        let connection=Connection::new(2,peer);connection.hello().unwrap();
        let gate=core.lock().unwrap().registration_slots.clone();let held=gate.acquire_many_owned(8).await.unwrap();
        let p=json!({"name":"late","role":"backend","instance":"i","software":{"name":"fixture","version":"1"},"provider_version":"0","capabilities":{}});
        assert_eq!(connection.register(&core,p.clone(),None).await.unwrap_err().kind(),"overloaded");
        assert!(!connection.is_provider());assert!(!core.lock().unwrap().providers.contains_key("late"));
        drop(held);connection.register(&core,p,None).await.unwrap();remote.close();
    }
    #[tokio::test]
    async fn registration_rechecks_shutdown_before_mutating_state() {
        let (core,_provider,remote,_incoming)=fixture().await;
        let (a,_b)=tokio::io::duplex(4096);let (r,w)=tokio::io::split(a);let (peer,_rx)=Peer::spawn(r,w);
        let connection=Connection::new(2,peer);connection.hello().unwrap();
        core.lock().unwrap().shutdown.request();
        let error=connection.register(&core,json!({"name":"late","role":"backend","instance":"i","software":{"name":"fixture","version":"1"},"provider_version":"0","capabilities":{}}),None).await.unwrap_err();
        assert_eq!(error.kind(),"unavailable");assert_eq!(error.data.unwrap()["outcome"],"rejected");
        assert!(!core.lock().unwrap().providers.contains_key("late"));remote.close();
    }
    fn facts(reference:&str) -> ResourceFacts { parse_facts(json!({"ref":reference,"kind":"terminal","state":"live"})).unwrap() }
    fn fill_local(provider:&WireProvider) {
        let mut inventory=provider.inventory.lock().unwrap();
        inventory.started=true;
        for n in 0..MAX_RESOURCES { let facts=facts(&format!("old{n}")); inventory.current.insert(facts.backend_ref.clone(),facts); }
    }
    #[tokio::test]
    async fn expired_before_dispatch_is_definitely_rejected_and_sends_nothing() {
        let (_core,provider,remote,mut incoming)=fixture().await;
        let context=EffectContext { effect:"expired".into(),deadline:Instant::now()-std::time::Duration::from_millis(1),target:None };
        let error=provider.invoke(&context,"fixture",None,json!({})).await.unwrap_err().rpc();
        assert!(incoming.try_recv().is_err(),"expired effect must never reach provider");
        assert_eq!(error.kind(),"timeout");
        assert_eq!(error.data.unwrap()["outcome"],"rejected");
        remote.close();
    }
    #[tokio::test]
    async fn completed_effect_window_evicts_old_ids_but_never_active_ids() {
        let (_core,provider,remote,_incoming)=fixture().await;
        provider.inventory.lock().unwrap().effects.insert("active".into());
        for n in 0..RECENT_EFFECTS+10 {
            let effect=format!("e{n}");
            provider.inventory.lock().unwrap().effects.insert(effect.clone());
            drop(EffectLease { inventory:&provider.inventory,effect:&effect });
        }
        let inventory=provider.inventory.lock().unwrap();
        assert_eq!(inventory.effects.len(),RECENT_EFFECTS+1);
        assert_eq!(inventory.recent_effects.len(),RECENT_EFFECTS);
        assert!(inventory.effects.contains("active"));
        assert!(!inventory.effects.contains("e0"));
        assert!(inventory.effects.contains(&format!("e{}",RECENT_EFFECTS+9)));
        remote.close();
    }
    #[tokio::test]
    async fn duplicate_active_and_recent_effect_ids_never_dispatch_twice() {
        let (_core,provider,remote,mut incoming)=fixture().await;
        let context=EffectContext { effect:"same".into(),deadline:Instant::now()+std::time::Duration::from_secs(2),target:None };
        let call=tokio::spawn({let provider=provider.clone();let context=context.clone();async move { provider.invoke(&context,"fixture",None,json!({})).await }});
        let Some(crate::rpc::Incoming::Request { id,method,.. })=incoming.recv().await else { panic!("missing effect"); };
        assert_eq!(method,"effect.invoke");
        assert_eq!(provider.invoke(&context,"fixture",None,json!({})).await.unwrap_err().rpc().kind(),"conflict");
        remote.respond(id,Ok(json!({"outcome":"accepted"})));
        call.await.unwrap().unwrap();
        assert_eq!(provider.invoke(&context,"fixture",None,json!({})).await.unwrap_err().rpc().kind(),"conflict");
        assert!(incoming.try_recv().is_err());
        remote.close();
    }
    #[tokio::test]
    async fn local_sync_cap_is_checked_before_any_core_publication_or_sync_state_change() {
        let (core,provider,remote,_incoming)=fixture().await;
        fill_local(&provider);
        let seq=core.lock().unwrap().seq;
        let error=provider.sync(&core,json!({"resources":[{"ref":"new","kind":"terminal","state":"live"}],"final":false})).unwrap_err();
        assert_eq!(error.kind(),"overloaded");
        assert!(core.lock().unwrap().resources.is_empty(),"local refusal must not leave a core-only resource");
        assert_eq!(core.lock().unwrap().seq,seq);
        assert!(!provider.inventory.lock().unwrap().syncing);
        remote.close();
    }
    #[tokio::test]
    async fn empty_final_sync_cannot_clear_a_replaced_generations_resources() {
        let (core,provider,remote,_incoming)=fixture().await;
        { let mut c=core.lock().unwrap();c.providers.get_mut("qa2").unwrap().generation+=1;c.upsert_resource("qa2",facts("new-generation")).unwrap(); }
        assert_eq!(provider.sync(&core,json!({"resources":[],"final":true})).unwrap_err().kind(),"stale_generation");
        assert_eq!(core.lock().unwrap().resources.len(),1);
        remote.close();
    }
    #[tokio::test]
    async fn local_upsert_cap_is_checked_before_core_publication() {
        let (core,provider,remote,_incoming)=fixture().await;
        fill_local(&provider);
        let seq=core.lock().unwrap().seq;
        let error=provider.notification(&core,"resource.upsert",json!({"facts":{"ref":"new","kind":"terminal","state":"live"}})).unwrap_err();
        assert_eq!(error.kind(),"overloaded");
        assert!(core.lock().unwrap().resources.is_empty(),"local refusal must not leave a core-only resource");
        assert_eq!(core.lock().unwrap().seq,seq);
        remote.close();
    }
}

