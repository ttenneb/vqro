//! Socket-backed providers. All backend/runtime policy stays on the other end of the connection.
//! This module adapts pinned provider-v0 messages to the same routes native adapters use.

use std::collections::{BTreeMap, BTreeSet, HashMap};
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
pub const MAX_PROVIDERS: usize = 32;
pub const MAX_REGISTRATION_BYTES: usize = 128 * 1024;
pub use crate::resource_admission::{encoded_bytes, MAX_FACT_BYTES, MAX_RESOURCES};
const MAX_EFFECTS: usize = 65_536;
const MAX_INFLIGHT: usize = 128;
const CONTROL_BYTES: usize = 4 * 1024 * 1024;
const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

fn invalid(message: impl Into<String>) -> RpcError { RpcError::invalid(message) }
fn overloaded(message: impl Into<String>) -> RpcError { provider::rejected("overloaded", message) }
fn reason(value: &Value) -> Result<Value, RpcError> {
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
    pub async fn register(&self, core: &Shared, p: Value) -> RpcResult {
        if !self.hello.load(Ordering::Acquire) { return Err(invalid("hello must precede provider.register")); }
        if self.is_provider() { return Err(RpcError::new("conflict", "one registration per connection")); }
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
        if self.is_provider() { return Err(RpcError::new("conflict", "one registration per connection")); }
        if let Some(claim) = &claim {
            if !c.bridge_tokens.valid_claim(r.token.as_deref().unwrap(), claim, &r.name, &r.instance) { return Err(RpcError::new("forbidden", "runtime resume identity mismatch or token used")); }
        }
        let resuming = claim.as_ref().is_some_and(|claim| claim.resuming);
        if c.providers.get(&r.name).is_some_and(|p| p.health.borrow().connected)
            && !(resuming && c.providers[&r.name].metadata["host"].as_str() == host.as_deref()) {
            return Err(RpcError::new("conflict", "provider name already has a live connection"));
        }
        if !c.providers.contains_key(&r.name) && c.providers.len() >= MAX_PROVIDERS { return Err(overloaded("provider registry is full (32)")); }
        let resume_token = if let Some(claim) = &claim {
            if !c.has_resource_identity(&claim.host) { return Err(RpcError::new("forbidden", "runtime host removed")); }
            Some(c.bridge_tokens.rotate(r.token.as_deref().unwrap(), claim, &r.name, &r.instance)?)
        } else { None };
        let window = r.limits.stream_window_bytes.min(default_window());
        let handle = AdapterHandle { name: r.name.clone(), capabilities: json!(r.capabilities), session: None, context: None, cmds: commands };
        let source = c.install_adapter(handle, false)?;
        c.on_adapter_event(&r.name, adapter::AdapterEvent::Registered(ProviderInfo { connection: self.id, instance: r.instance.clone(), role: r.role, software: json!(r.software) }));
        let generation = c.providers[&r.name].generation;
        c.providers.get_mut(&r.name).unwrap().metadata = json!({"provider_version": r.provider_version, "limits": {"max_streams": r.limits.max_streams.min(512), "stream_window_bytes": window, "max_record_bytes": MAX_RECORD_BYTES}, "host": host});
        c.provider_event(&r.name);
        let health = c.providers[&r.name].health.subscribe();
        let (stopped, _) = watch::channel(false);
        let provider = Arc::new(WireProvider { name: r.name, generation, source, connection: self.id,
            peer: self.peer.clone(), core: Arc::downgrade(core), window, streams: Mutex::new(HashMap::new()),
            stream_slots: Arc::new(Semaphore::new(r.limits.max_streams.min(512))), control: Arc::new(Semaphore::new(CONTROL_BYTES)),
            inflight: Arc::new(Semaphore::new(MAX_INFLIGHT)), next_stream: AtomicU64::new(1),
            inventory: Mutex::new(Inventory { started: false, syncing: false, seen: BTreeMap::new(), current: BTreeMap::new(), effects: BTreeSet::new() }), stopped, health });
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
        let provider = self.validate(core, &p)?;
        provider.notification(core, method, p)
    }
    pub fn disconnect(&self, core: &Shared) {
        let provider = self.provider.lock().unwrap().take();
        if let Some(provider) = provider { provider.disconnect(core); }
    }
}

impl WireProvider {
    fn current(&self, core: &Shared) -> bool { core.lock().unwrap().providers.get(&self.name).is_some_and(|p| p.source == self.source && p.generation == self.generation && p.health.borrow().connected) }
    fn record(&self, facts: ResourceFacts) -> Result<(), RpcError> {
        let mut inventory = self.inventory.lock().unwrap();
        if !inventory.current.contains_key(&facts.backend_ref) && inventory.current.len() >= MAX_RESOURCES { return Err(overloaded("provider resource count exceeds 4096")); }
        inventory.current.insert(facts.backend_ref.clone(), facts.clone());
        if inventory.syncing { inventory.seen.insert(facts.backend_ref.clone(), facts); }
        Ok(())
    }
    fn sync(&self, core: &Shared, p: Value) -> RpcResult {
        let final_chunk = p["final"].as_bool().ok_or_else(|| invalid("sync requires final boolean"))?;
        let records = p["resources"].as_array().ok_or_else(|| invalid("sync requires resources array"))?;
        let facts: Vec<_> = records.iter().cloned().map(parse_facts).collect::<Result<_, _>>()?;
        {
            let mut inventory = self.inventory.lock().unwrap();
            if !inventory.syncing { inventory.seen.clear(); inventory.syncing = true; }
            inventory.started = true;
        }
        for facts in facts {
            let applied = core.lock().unwrap().upsert_resource(&self.name, facts.clone());
            if let Err(error) = applied { if !matches!(error.kind(), "io" | "durability_unconfirmed") { return Err(error); } }
            self.record(facts)?;
        }
        if final_chunk {
            let records = self.inventory.lock().unwrap().seen.values().cloned().collect::<Vec<_>>();
            core.lock().unwrap().on_adapter_event(&self.name, adapter::AdapterEvent::Snapshot(records));
        }
        if let Some(error) = core.lock().unwrap().provider_persistence_error(&self.name) { return Err(error); }
        if final_chunk {
            let mut inventory = self.inventory.lock().unwrap();
            inventory.current = std::mem::take(&mut inventory.seen);
            inventory.syncing = false;
        }
        Ok(json!({}))
    }
    fn notification(&self, core: &Shared, method: &str, p: Value) -> Result<(), RpcError> {
        match method {
            "resource.upsert" => {
                if !self.inventory.lock().unwrap().started { return Err(invalid("first sync must start before upsert")); }
                let facts = parse_facts(p.get("facts").cloned().ok_or_else(|| invalid("upsert requires facts"))?)?;
                let applied = core.lock().unwrap().upsert_resource(&self.name, facts.clone());
                if let Err(error) = applied { if !matches!(error.kind(), "io" | "durability_unconfirmed") { return Err(error); } }
                self.record(facts)?;
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
                core.lock().unwrap().on_adapter_event(&self.name, adapter::AdapterEvent::Removed { reference, incarnation });
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
    fn end(&self, id: u64, gap: bool, why: Value, tell_provider: bool) {
        if let Some(mut stream) = self.streams.lock().unwrap().remove(&id) {
            let bytes = encoded_bytes(&why).max(64);
            match self.control.clone().try_acquire_many_owned(bytes.min(u32::MAX as usize) as u32) {
                Ok(permit) => { if let Some(reply) = stream.terminal.take() { let _ = reply.send(StreamEnd { gap, reason: why, _permit: Some(permit) }); } }
                Err(_) => self.peer.close(),
            }
            if tell_provider { self.peer.notify("stream.close", json!({"generation": self.generation, "stream": id})); }
        }
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
        {
            let mut inventory = self.inventory.lock().unwrap();
            if inventory.effects.contains(&context.effect) { return Err(AdapterError::RejectedKind { kind: "conflict".into(), message: "duplicate effect id".into() }); }
            if inventory.effects.len() >= MAX_EFFECTS { return Err(AdapterError::RejectedKind { kind: "overloaded".into(), message: "effect ledger full; reconnect explicitly".into() }); }
            inventory.effects.insert(context.effect.clone());
        }
        let remaining = context.deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() { return Err(AdapterError::Indeterminate("effect deadline expired before provider dispatch".into())); }
        let mut p = json!({"generation": self.generation, "effect": context.effect, "op": op, "args": args, "deadline_ms": remaining.as_millis().clamp(1, 300000) as u64});
        if let Some((reference, incarnation)) = target { p["ref"] = json!(reference); p["incarnation"] = json!(incarnation); }
        if encoded_bytes(&p) + 128 > MAX_RECORD_BYTES { return Err(AdapterError::RejectedKind { kind: "overloaded".into(), message: "provider effect record exceeds 1 MiB".into() }); }
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
                Some(facts) => { let facts = parse_facts(facts.clone()).map_err(|e| AdapterError::Backend(e.to_string()))?; self.record(facts.clone()).map_err(|e| AdapterError::Backend(e.to_string()))?; Some(facts) }
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
            provider.end(id, false, json!({"reason": "closed"}), true);
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

