//! Provider-side helper for `docs/spec/provider-v0.md` section 9 (pinned v0 wire messages).
//!
//! A provider connects to the core's endpoint, says `hello`, registers once (one provider per
//! connection), syncs its resources, and then serves the core's requests: `effect.invoke` and
//! `stream.open` arrive as [`Request`]s on a channel. Every request gets exactly one reply: a
//! dropped [`EffectRequest`] or [`OpenRequest`] replies with a `backend` error ("outcome unknown").
//! A provider never sends `indeterminate`; the core produces it.
//!
//! Streams are credit-driven: [`ProviderStream::send`] waits for credit, never has more than the
//! window outstanding, and numbers decoded bytes with contiguous offsets from 0. A gap or close ends
//! the stream; recovery is a fresh `stream.open` from the core. Nothing is replayed or resent.
//! Mirrors the Python SDK (`extensions/vqro_provider.py`) where the spec leaves room.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine;
use serde_json::{json, Map, Value};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot, watch, Notify, OwnedSemaphorePermit, Semaphore};
use tokio::time::Instant;

/// Maximum JSON-RPC record on a provider connection, both directions (9.1).
pub const MAX_RECORD: usize = 1 << 20;
/// Maximum decoded bytes in one `stream.data` or `stream.input` (9.5).
pub const MAX_DATA: usize = 64 * 1024;
/// The core caps the stream window at this (9.2).
pub const MAX_WINDOW: u64 = 262_144;
const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

/// A protocol-level failure: becomes a JSON-RPC error with `data.kind` (9.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderError {
    pub kind: String,
    pub message: String,
}

impl ProviderError {
    pub fn new(kind: &str, message: impl Into<String>) -> Self {
        ProviderError { kind: kind.into(), message: message.into() }
    }
    fn to_json(&self) -> Value {
        let code = match self.kind.as_str() {
            "unknown_method" => -32601,
            "invalid" => -32602,
            _ => -32000,
        };
        json!({"code": code, "message": self.message, "data": {"kind": self.kind}})
    }
}

impl std::fmt::Display for ProviderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.message, self.kind)
    }
}

impl std::error::Error for ProviderError {}

/// What a provider declares at registration, plus local bounds.
#[derive(Debug, Clone)]
pub struct Config {
    pub name: String,
    /// "backend" or "runtime".
    pub role: String,
    pub instance: String,
    pub software: Value,
    pub provider_version: String,
    /// Capability name -> parameter object (absent means unsupported; never `false`).
    pub capabilities: Map<String, Value>,
    pub max_streams: usize,
    pub stream_window_bytes: u64,
    /// Runtime bridges only.
    pub token: Option<String>,
    /// Name sent in `hello {extension}`; default `$VQRO_EXTENSION`, else `name`.
    pub extension: Option<String>,
    /// Deadline for this provider's own calls (hello, register, sync).
    pub call_timeout: Duration,
    /// Core requests handled concurrently; beyond this, `overloaded`.
    pub max_in_flight: usize,
    /// Active + recently completed effect ids. Evict completed ids at capacity, never active ids.
    pub max_effect_ids: usize,
    /// Bytes of outgoing lines queued for the socket (backpressure for data, upserts and syncs).
    pub outgoing_bytes: usize,
    /// Queued input/resize/close commands per stream; beyond this, `stream.error` overloaded.
    pub stream_commands: usize,
}

impl Config {
    pub fn new(name: impl Into<String>, instance: impl Into<String>, capabilities: Map<String, Value>) -> Self {
        let name = name.into();
        Config {
            software: json!({"name": name, "version": "0"}),
            name,
            role: "backend".into(),
            instance: instance.into(),
            provider_version: env!("CARGO_PKG_VERSION").into(),
            capabilities,
            max_streams: 512,
            stream_window_bytes: MAX_WINDOW,
            token: None,
            extension: None,
            call_timeout: Duration::from_secs(30),
            max_in_flight: 64,
            max_effect_ids: 65_536,
            outgoing_bytes: 2 << 20,
            stream_commands: 128,
        }
    }
}

/// Something the core asked for. Reply through the request; dropping it replies "outcome unknown".
pub enum Request {
    Effect(EffectRequest),
    Open(OpenRequest),
}

/// The result of an effect the provider knows (9.4). `Rejected` means no effect.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    Accepted(Option<Value>),
    Observed(Option<Value>),
    Rejected { kind: String, message: String },
}

impl Outcome {
    fn to_json(&self) -> Value {
        match self {
            Outcome::Accepted(d) | Outcome::Observed(d) => {
                let mut v = json!({"outcome": if matches!(self, Outcome::Accepted(_)) { "accepted" } else { "observed" }});
                if let Some(d) = d {
                    v["data"] = d.clone();
                }
                v
            }
            Outcome::Rejected { kind, message } => json!({"outcome": "rejected", "error": {"kind": kind, "message": message}}),
        }
    }
}

/// One reply to one core request, exactly once.
#[derive(Default)]
struct IdLedger<T> { seen:HashSet<T>, active:HashSet<T>, recent:std::collections::VecDeque<T> }
impl<T:Eq+std::hash::Hash+Clone> IdLedger<T> {
    fn begin(&mut self,id:T,capacity:usize) -> Result<(),ProviderError> {
        if self.seen.contains(&id) { return Err(ProviderError::new("conflict","duplicate identity; never executed twice")); }
        while self.seen.len()>=capacity {
            let Some(old)=self.recent.pop_front() else { return Err(ProviderError::new("overloaded","all identity slots are active; not started")); };
            self.seen.remove(&old);
        }
        self.seen.insert(id.clone()); self.active.insert(id); Ok(())
    }
    fn finish(&mut self,id:&T) {
        if self.active.remove(id) { self.recent.push_back(id.clone()); }
    }
}

struct Responder {
    shared: Arc<Shared>,
    id: Value,
    done: bool,
    effect:Option<String>,
}

impl Responder {
    fn send(mut self, result: Result<Value, ProviderError>) {
        self.done = true;
        self.shared.reply(&self.id, result);
    }
}

impl Drop for Responder {
    fn drop(&mut self) {
        if !self.done {
            self.shared.reply(&self.id, Err(ProviderError::new("backend", "handler dropped the request; outcome unknown")));
        }
        if let Some(effect)=&self.effect { self.shared.effect_ids.lock().unwrap().finish(effect); }
    }
}

pub struct EffectRequest {
    pub effect: String,
    pub op: String,
    pub r#ref: Option<String>,
    pub incarnation: String,
    pub args: Value,
    /// Local monotonic deadline from `deadline_ms` (remaining at dispatch). Informational: the
    /// provider still replies once when it knows, even after it.
    pub deadline: Instant,
    responder: Responder,
}

impl EffectRequest {
    /// Reply with a known outcome, or a protocol error (no effect; or `backend` for unknown).
    pub fn respond(self, result: Result<Outcome, ProviderError>) {
        self.responder.send(result.map(|o| o.to_json()));
    }
}

/// What a fresh stream starts with (9.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Initial {
    History,
    Repaint,
    None,
}

impl Initial {
    fn as_str(self) -> &'static str {
        match self {
            Initial::History => "history",
            Initial::Repaint => "repaint",
            Initial::None => "none",
        }
    }
}

pub struct OpenRequest {
    pub stream: ProviderStream,
    pub cols: u16,
    pub rows: u16,
    pub history_lines: u64,
    responder: Responder,
}

impl OpenRequest {
    /// Reply. On success the stream becomes ready; data sent before this waits for it.
    /// On error the stream is finished and the error replied.
    pub fn respond(self, result: Result<Initial, ProviderError>) -> Option<ProviderStream> {
        let OpenRequest { stream, responder, .. } = self;
        match result {
            Ok(initial) => {
                responder.send(Ok(json!({"initial": initial.as_str()})));
                stream.inner.set_ready();
                Some(stream)
            }
            Err(e) => {
                stream.inner.finish();
                responder.send(Err(e));
                None
            }
        }
    }
}

/// Commands from the core for one stream, in the viewer's order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamCommand {
    Input(Vec<u8>),
    Resize(u16, u16),
    Close,
}

struct StreamState {
    ready: bool,
    closed: bool,
    credit: u64,
    outstanding: u64,
    offset: u64,
    queued: usize,
}

struct StreamInner {
    state: Mutex<StreamState>,
    changed: Notify,
    /// Orders data, gap and close lines of this stream on the wire.
    wire: tokio::sync::Mutex<()>,
    commands: Mutex<Option<mpsc::UnboundedSender<StreamCommand>>>,
}

impl StreamInner {
    fn set_ready(&self) {
        self.state.lock().unwrap().ready = true;
        self.changed.notify_waiters();
    }
    /// Output ended by the provider (gap or close): no more data lines. Input the core already sent
    /// for this stream keeps arriving as commands until the core's `stream.close` (provider-v0 9.5):
    /// a gap ends output, not input, so input taken before it is still delivered.
    fn finish_output(&self) {
        self.state.lock().unwrap().closed = true;
        self.changed.notify_waiters();
    }
    /// Locally finished: no more lines for it; command receiver ends.
    fn finish(&self) {
        self.state.lock().unwrap().closed = true;
        self.commands.lock().unwrap().take();
        self.changed.notify_waiters();
    }
}

/// One credited output stream to the core, and its incoming commands.
pub struct ProviderStream {
    pub id: u64,
    pub r#ref: String,
    pub incarnation: String,
    inner: Arc<StreamInner>,
    shared: Arc<Shared>,
    commands: mpsc::UnboundedReceiver<StreamCommand>,
}

impl ProviderStream {
    pub fn is_closed(&self) -> bool {
        self.inner.state.lock().unwrap().closed
    }

    /// Next command from the core; `None` once the stream ended (closed, gap, disconnect).
    pub async fn next(&mut self) -> Option<StreamCommand> {
        let cmd = self.commands.recv().await;
        if cmd.is_some() {
            self.inner.state.lock().unwrap().queued -= 1;
        }
        cmd
    }

    /// Bytes sent so far (the next offset).
    pub fn offset(&self) -> u64 {
        self.inner.state.lock().unwrap().offset
    }

    /// Send output, waiting for credit. Chunks at 64 KiB and at the available credit.
    /// Fails with `unavailable` if the stream or connection ended.
    pub async fn send(&self, mut data: &[u8]) -> Result<(), ProviderError> {
        while !data.is_empty() {
            let _wire = self.inner.wire.lock().await;
            let (offset, n) = loop {
                let wait = self.inner.changed.notified();
                tokio::pin!(wait);
                wait.as_mut().enable();
                {
                    let mut s = self.inner.state.lock().unwrap();
                    if s.closed || self.shared.is_closed() {
                        return Err(ProviderError::new("unavailable", "stream ended"));
                    }
                    if s.ready && s.credit > 0 {
                        let n = (data.len() as u64).min(s.credit).min(MAX_DATA as u64) as usize;
                        let offset = s.offset;
                        s.offset += n as u64;
                        s.credit -= n as u64;
                        s.outstanding += n as u64;
                        break (offset, n);
                    }
                }
                tokio::select! {
                    _ = wait => {}
                    _ = self.shared.closed_signal() => {}
                }
            };
            let line = json!({"jsonrpc": "2.0", "method": "stream.data", "params": {
                "generation": self.shared.generation(), "stream": self.id, "offset": offset, "data": B64.encode(&data[..n])}});
            self.shared.send_line(line.to_string()).await?;
            data = &data[n..];
        }
        Ok(())
    }

    /// End the stream with a gap (`backend_dropped`, `paused` or `viewer_slow`) at the current offset.
    pub async fn gap(&self, gap: &str) -> Result<(), ProviderError> {
        if !matches!(gap, "backend_dropped" | "paused" | "viewer_slow") {
            return Err(ProviderError::new("invalid", "unknown gap kind"));
        }
        self.end("stream.gap", json!({"reason": {"reason": "gap", "gap": gap}})).await
    }

    /// End the stream with `stream.closed {reason}`; `reason` is an object with a `reason` string.
    pub async fn close(&self, reason: Value) -> Result<(), ProviderError> {
        if !reason.get("reason").is_some_and(Value::is_string) {
            return Err(ProviderError::new("invalid", "reason must be an object with a reason string"));
        }
        self.end("stream.closed", json!({"reason": reason})).await
    }

    /// Report a rejected command (no effect) without ending the stream.
    pub async fn error(&self, error: &ProviderError) -> Result<(), ProviderError> {
        if self.is_closed() {
            return Ok(());
        }
        let line = json!({"jsonrpc": "2.0", "method": "stream.error", "params": {"generation": self.shared.generation(),
            "stream": self.id, "error": {"kind": error.kind, "message": error.message}, "outcome": "rejected"}});
        self.shared.send_line(line.to_string()).await
    }

    async fn end(&self, method: &str, mut params: Value) -> Result<(), ProviderError> {
        // Never before the open reply: wait for ready (or closed).
        loop {
            let wait = self.inner.changed.notified();
            tokio::pin!(wait);
            wait.as_mut().enable();
            {
                let s = self.inner.state.lock().unwrap();
                if s.ready || s.closed {
                    break;
                }
            }
            tokio::select! {
                _ = wait => {}
                _ = self.shared.closed_signal() => return Err(ProviderError::new("unavailable", "connection closed")),
            }
        }
        let _wire = self.inner.wire.lock().await;
        let offset = {
            let s = self.inner.state.lock().unwrap();
            if s.closed {
                return Ok(());
            }
            s.offset
        };
        // Stay registered for input until the core closes the stream or this handle is dropped.
        self.inner.finish_output();
        params["generation"] = json!(self.shared.generation());
        params["stream"] = json!(self.id);
        if method == "stream.gap" {
            params["offset"] = json!(offset);
        }
        self.shared.send_line(json!({"jsonrpc": "2.0", "method": method, "params": params}).to_string()).await
    }
}

impl Drop for ProviderStream {
    fn drop(&mut self) {
        // A provider that drops a live stream without ending it: tell the core it closed.
        let live = {
            let s = self.inner.state.lock().unwrap();
            s.ready && !s.closed
        };
        self.inner.finish();
        self.shared.streams.lock().unwrap().remove(&self.id);
        self.shared.stream_ids.lock().unwrap().finish(&self.id);
        if live {
            let line = json!({"jsonrpc": "2.0", "method": "stream.closed", "params": {"generation": self.shared.generation(),
                "stream": self.id, "reason": {"reason": "closed", "message": "provider dropped the stream"}}});
            self.shared.push_line(line.to_string());
        }
    }
}

type Pending = Mutex<HashMap<String, oneshot::Sender<Result<Value, ProviderError>>>>;

struct Shared {
    out: mpsc::UnboundedSender<(String, Option<OwnedSemaphorePermit>)>,
    budget: Arc<Semaphore>,
    outgoing_bytes: usize,
    generation: AtomicU64,
    pending: Pending,
    next_id: AtomicU64,
    closed: AtomicBool,
    closed_tx: watch::Sender<bool>,
    failure: Mutex<Option<String>>,
    streams: Mutex<HashMap<u64, Arc<StreamInner>>>,
    stream_ids: Mutex<IdLedger<u64>>,
    effect_ids: Mutex<IdLedger<String>>,
    in_flight: AtomicU64,
    sync_started: AtomicBool,
    max_record: AtomicU64,
}

impl Shared {
    fn generation(&self) -> u64 {
        self.generation.load(Ordering::Relaxed)
    }
    fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Relaxed)
    }
    async fn closed_signal(&self) {
        let mut rx = self.closed_tx.subscribe();
        let _ = rx.wait_for(|c| *c).await;
    }
    fn disconnect(&self, why: &str) {
        if self.closed.swap(true, Ordering::Relaxed) {
            return;
        }
        *self.failure.lock().unwrap() = Some(why.to_string());
        for (_, tx) in self.pending.lock().unwrap().drain() {
            let _ = tx.send(Err(ProviderError::new("unavailable", why)));
        }
        for (id, s) in self.streams.lock().unwrap().drain() {
            s.finish();
            self.stream_ids.lock().unwrap().finish(&id);
        }
        self.closed_tx.send_replace(true);
    }
    /// Queue a line without backpressure (replies and final markers: bounded by request limits).
    fn push_line(&self, line: String) {
        if !self.is_closed() {
            let _ = self.out.send((line, None));
        }
    }
    /// Queue a line within the outgoing byte budget, waiting for room.
    async fn send_line(&self, line: String) -> Result<(), ProviderError> {
        if line.len() + 1 > self.max_record.load(Ordering::Relaxed) as usize {
            return Err(ProviderError::new("invalid", "record exceeds the negotiated byte limit"));
        }
        let want = (line.len() + 1).min(self.outgoing_bytes) as u32;
        let permit = tokio::select! {
            p = self.budget.clone().acquire_many_owned(want) => p.map_err(|_| ProviderError::new("unavailable", "connection closed"))?,
            _ = self.closed_signal() => return Err(ProviderError::new("unavailable", "connection closed")),
        };
        if self.is_closed() {
            return Err(ProviderError::new("unavailable", "connection closed"));
        }
        self.out.send((line, Some(permit))).map_err(|_| ProviderError::new("unavailable", "connection closed"))
    }
    fn reply(&self, id: &Value, result: Result<Value, ProviderError>) {
        self.in_flight.fetch_sub(1, Ordering::Relaxed);
        let line = match result {
            Ok(v) => json!({"jsonrpc": "2.0", "id": id, "result": v}),
            Err(e) => json!({"jsonrpc": "2.0", "id": id, "error": e.to_json()}),
        };
        self.push_line(line.to_string());
    }
    fn reply_now(&self, id: &Value, error: ProviderError) {
        self.push_line(json!({"jsonrpc": "2.0", "id": id, "error": error.to_json()}).to_string());
    }
}

/// A registered provider connection. Cheap to clone.
#[derive(Clone)]
pub struct Provider {
    shared: Arc<Shared>,
    config: Arc<Config>,
    window: u64,
}

impl Provider {
    /// Connect over `io` (the core endpoint), say hello and register. Returns the provider and the
    /// channel of core requests.
    pub async fn connect<IO>(io: IO, config: Config) -> Result<(Provider, mpsc::UnboundedReceiver<Request>), ProviderError>
    where
        IO: AsyncRead + AsyncWrite + Send + 'static,
    {
        if config.capabilities.values().any(|v| !v.is_object()) {
            return Err(ProviderError::new("invalid", "capability values must be objects (absent means unsupported)"));
        }
        let (reader, writer) = tokio::io::split(io);
        let (out, out_rx) = mpsc::unbounded_channel();
        let (closed_tx, _) = watch::channel(false);
        let shared = Arc::new(Shared {
            out,
            budget: Arc::new(Semaphore::new(config.outgoing_bytes)),
            outgoing_bytes: config.outgoing_bytes,
            generation: AtomicU64::new(0),
            pending: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            closed: AtomicBool::new(false),
            closed_tx,
            failure: Mutex::new(None),
            streams: Mutex::new(HashMap::new()),
            stream_ids: Mutex::new(IdLedger::default()),
            effect_ids: Mutex::new(IdLedger::default()),
            in_flight: AtomicU64::new(0),
            sync_started: AtomicBool::new(false),
            max_record: AtomicU64::new(MAX_RECORD as u64),
        });
        let (requests, requests_rx) = mpsc::unbounded_channel();
        tokio::spawn(write_lines(writer, out_rx, shared.clone()));
        let config = Arc::new(config);
        tokio::spawn(read_lines(reader, shared.clone(), config.clone(), requests));
        let mut provider = Provider { shared, config: config.clone(), window: config.stream_window_bytes.min(MAX_WINDOW) };
        let result = provider.handshake().await;
        if let Err(e) = result {
            provider.shared.disconnect(&format!("registration failed: {e}"));
            return Err(e);
        }
        Ok((provider, requests_rx))
    }

    async fn handshake(&mut self) -> Result<(), ProviderError> {
        let ext = self.config.extension.clone().or_else(|| std::env::var("VQRO_EXTENSION").ok()).unwrap_or_else(|| self.config.name.clone());
        self.call("hello", json!({"extension": ext,"extension_token":std::env::var("VQRO_EXTENSION_TOKEN").ok()})).await?;
        let mut params = json!({
            "name": self.config.name, "role": self.config.role, "instance": self.config.instance,
            "software": self.config.software, "provider_version": self.config.provider_version,
            "capabilities": self.config.capabilities,
            "limits": {"max_streams": self.config.max_streams, "stream_window_bytes": self.window},
        });
        if let Some(t) = &self.config.token {
            params["token"] = json!(t);
        }
        let r = self.call("provider.register", params).await?;
        let generation = r["generation"].as_u64().filter(|g| *g >= 1).ok_or_else(|| ProviderError::new("invalid", "register: bad generation"))?;
        if let Some(w) = r["limits"]["stream_window_bytes"].as_u64() {
            self.window = w.clamp(1, self.window);
        }
        if let Some(m) = r["limits"]["max_record_bytes"].as_u64() {
            self.shared.max_record.store(m.clamp(1024, MAX_RECORD as u64), Ordering::Relaxed);
        }
        self.shared.generation.store(generation, Ordering::Relaxed);
        Ok(())
    }

    pub fn generation(&self) -> u64 {
        self.shared.generation()
    }

    /// Effective per-stream window (from the register result).
    pub fn window(&self) -> u64 {
        self.window
    }

    pub fn is_closed(&self) -> bool {
        self.shared.is_closed()
    }

    pub fn failure(&self) -> Option<String> {
        self.shared.failure.lock().unwrap().clone()
    }

    /// Resolves when the connection has ended.
    pub async fn closed(&self) {
        self.shared.closed_signal().await
    }

    /// Close the connection. Streams end; pending calls fail `unavailable`.
    pub fn close(&self) {
        self.shared.disconnect("provider closed");
    }

    /// A request to the core. Adds `generation` once registered.
    pub async fn call(&self, method: &str, mut params: Value) -> Result<Value, ProviderError> {
        let generation = self.shared.generation();
        if generation > 0 {
            params["generation"] = json!(generation);
        }
        let id = format!("p-{}", self.shared.next_id.fetch_add(1, Ordering::Relaxed));
        let (tx, rx) = oneshot::channel();
        self.shared.pending.lock().unwrap().insert(id.clone(), tx);
        let line = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string();
        if let Err(e) = self.shared.send_line(line).await {
            self.shared.pending.lock().unwrap().remove(&id);
            return Err(e);
        }
        match tokio::time::timeout(self.config.call_timeout, rx).await {
            Ok(Ok(r)) => r,
            Ok(Err(_)) => Err(ProviderError::new("unavailable", "connection closed")),
            Err(_) => {
                self.shared.pending.lock().unwrap().remove(&id);
                Err(ProviderError::new("timeout", "no answer before the deadline; delivery unknown, never resent"))
            }
        }
    }

    async fn notify(&self, method: &str, mut params: Value) -> Result<(), ProviderError> {
        if self.shared.generation() == 0 {
            return Err(ProviderError::new("unavailable", "not registered"));
        }
        params["generation"] = json!(self.shared.generation());
        self.shared.send_line(json!({"jsonrpc": "2.0", "method": method, "params": params}).to_string()).await
    }

    /// One `provider.sync` chunk; `final` ends the sync (9.2).
    pub async fn sync(&self, resources: Vec<Value>, last: bool) -> Result<(), ProviderError> {
        for r in &resources {
            check_facts(r)?;
        }
        self.shared.sync_started.store(true, Ordering::Relaxed);
        self.call("provider.sync", json!({"resources": resources, "final": last})).await.map(|_| ())
    }

    /// A complete sync, chunked to stay under the record limit.
    pub async fn full_sync(&self, resources: Vec<Value>) -> Result<(), ProviderError> {
        let limit = self.shared.max_record.load(Ordering::Relaxed) as usize - 512;
        let (mut chunk, mut size) = (Vec::new(), 0usize);
        for r in resources {
            check_facts(&r)?;
            let len = r.to_string().len() + 1;
            if !chunk.is_empty() && size + len > limit {
                self.sync(std::mem::take(&mut chunk), false).await?;
                size = 0;
            }
            chunk.push(r);
            size += len;
        }
        self.sync(chunk, true).await
    }

    /// `resource.upsert`; only after the first sync started (9.2).
    pub async fn upsert(&self, facts: Value) -> Result<(), ProviderError> {
        check_facts(&facts)?;
        if !self.shared.sync_started.load(Ordering::Relaxed) {
            return Err(ProviderError::new("conflict", "the first sync must start before any upsert"));
        }
        self.notify("resource.upsert", json!({"facts": facts})).await
    }

    /// `resource.gone {ref, incarnation, reason}`.
    pub async fn gone(&self, r#ref: &str, incarnation: &str, reason: Value) -> Result<(), ProviderError> {
        if !reason.get("reason").is_some_and(Value::is_string) {
            return Err(ProviderError::new("invalid", "reason must be an object with a reason string"));
        }
        self.notify("resource.gone", json!({"ref": r#ref, "incarnation": incarnation, "reason": reason})).await
    }
}

/// Minimal facts validation (9.3): ref, kind, state; optional strings; data an object.
pub fn check_facts(f: &Value) -> Result<(), ProviderError> {
    let bad = |what: &str| Err(ProviderError::new("invalid", format!("facts: {what}")));
    if !f.is_object() {
        return bad("not an object");
    }
    for k in ["ref", "kind"] {
        if f[k].as_str().is_none_or(|s| s.is_empty()) {
            return bad(&format!("{k} required"));
        }
    }
    if !matches!(f["state"].as_str(), Some("live" | "finished")) {
        return bad("state must be live or finished");
    }
    for k in ["incarnation", "name", "title", "cwd"] {
        if !(f[k].is_null() || f[k].is_string()) {
            return bad(&format!("{k} must be a string"));
        }
    }
    for k in ["status", "exit", "data"] {
        if !(f[k].is_null() || f[k].is_object()) {
            return bad(&format!("{k} must be an object"));
        }
    }
    if !(f["argv"].is_null() || f["argv"].as_array().is_some_and(|a| a.iter().all(Value::is_string))) {
        return bad("argv must be strings");
    }
    if f["data"].is_object() && f["data"].to_string().len() > 64 * 1024 {
        return bad("data over 64 KiB");
    }
    Ok(())
}

async fn write_lines<W: AsyncWrite + Unpin + Send + 'static>(
    writer: W, mut lines: mpsc::UnboundedReceiver<(String, Option<OwnedSemaphorePermit>)>, shared: Arc<Shared>,
) {
    let mut writer = tokio::io::BufWriter::new(writer);
    let mut closed = shared.closed_tx.subscribe();
    loop {
        let next = tokio::select! {
            l = lines.recv() => l,
            _ = closed.wait_for(|c| *c) => None,
        };
        let Some((line, _permit)) = next else { break };
        let ok = async {
            writer.write_all(line.as_bytes()).await?;
            writer.write_all(b"\n").await?;
            if lines.is_empty() {
                writer.flush().await?;
            }
            Ok::<(), std::io::Error>(())
        }
        .await;
        if ok.is_err() {
            shared.disconnect("write failed");
            break;
        }
    }
    let _ = writer.flush().await;
    let _ = writer.shutdown().await;
}

async fn read_lines<R: AsyncRead + Unpin + Send + 'static>(
    reader: R, shared: Arc<Shared>, config: Arc<Config>, requests: mpsc::UnboundedSender<Request>,
) {
    let mut reader = BufReader::new(reader);
    let mut buf = Vec::new();
    let mut closed = shared.closed_tx.subscribe();
    loop {
        buf.clear();
        let read = tokio::select! {
            r = read_line_capped(&mut reader, &mut buf) => r,
            _ = closed.wait_for(|c| *c) => return,
        };
        match read {
            Ok(true) => {}
            Ok(false) => {
                shared.disconnect("connection closed by the core");
                return;
            }
            Err(e) => {
                shared.disconnect(&e);
                return;
            }
        }
        if buf.last() == Some(&b'\r') {
            buf.pop();
        }
        if buf.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let Ok(msg) = serde_json::from_slice::<Value>(&buf) else {
            shared.disconnect("protocol error: invalid JSON");
            return;
        };
        if let Err(e) = handle(&msg, &shared, &config, &requests) {
            shared.disconnect(&e);
            return;
        }
    }
}

/// Read one LF-terminated line of at most MAX_RECORD bytes. Ok(false) at clean EOF.
async fn read_line_capped<R: AsyncRead + Unpin>(reader: &mut BufReader<R>, buf: &mut Vec<u8>) -> Result<bool, String> {
    loop {
        let available = reader.fill_buf().await.map_err(|e| format!("read failed: {e}"))?;
        if available.is_empty() {
            return if buf.is_empty() { Ok(false) } else { Err("connection closed mid-record".into()) };
        }
        let (take, done) = match available.iter().position(|b| *b == b'\n') {
            Some(i) => (i + 1, true),
            None => (available.len(), false),
        };
        let line_part = if done { &available[..take - 1] } else { &available[..take] };
        if buf.len() + line_part.len() > MAX_RECORD {
            return Err("protocol error: record over 1 MiB".into());
        }
        buf.extend_from_slice(line_part);
        reader.consume(take);
        if done {
            return Ok(true);
        }
    }
}

/// Handle one incoming message. Err ends the connection (protocol error).
fn handle(msg: &Value, shared: &Arc<Shared>, config: &Arc<Config>, requests: &mpsc::UnboundedSender<Request>) -> Result<(), String> {
    let Some(method) = msg.get("method").and_then(Value::as_str) else {
        // A response to one of our calls.
        let Some(id) = msg.get("id").and_then(Value::as_str) else { return Ok(()) };
        if let Some(tx) = shared.pending.lock().unwrap().remove(id) {
            let result = match msg.get("error") {
                Some(e) => Err(ProviderError::new(e["data"]["kind"].as_str().unwrap_or("backend"), e["message"].as_str().unwrap_or("core error"))),
                None => Ok(msg.get("result").cloned().unwrap_or(Value::Null)),
            };
            let _ = tx.send(result);
        }
        return Ok(());
    };
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    let id = msg.get("id").filter(|i| !i.is_null()).cloned();
    let generation_ok = params.get("generation").and_then(Value::as_u64) == Some(shared.generation()) && shared.generation() > 0;
    let Some(id) = id else {
        // Notifications from the core: a wrong generation on one connection is a bug (9.1).
        if !generation_ok {
            return Err(format!("protocol error: {method} with a wrong generation"));
        }
        return notification(method, &params, shared, config);
    };
    if !generation_ok {
        shared.reply_now(&id, ProviderError::new("stale_generation", "wrong provider generation"));
        return Ok(());
    }
    if shared.in_flight.load(Ordering::Relaxed) as usize >= config.max_in_flight {
        shared.reply_now(&id, ProviderError::new("overloaded", "too many requests in flight; not started"));
        return Ok(());
    }
    let request = match method {
        "effect.invoke" => effect_request(&id, &params, shared, config),
        "stream.open" => open_request(&id, &params, shared, config),
        _ => Err(ProviderError::new("unknown_method", format!("unknown method {method}"))),
    };
    match request {
        Ok(r) => {
            // The responder now owns the in-flight slot (released when it replies or drops).
            if let Err(mpsc::error::SendError(r)) = requests.send(r) {
                drop(r); // replies "outcome unknown"
            }
        }
        Err(e) => shared.reply_now(&id, e),
    }
    Ok(())
}

fn responder(shared: &Arc<Shared>, id: &Value) -> Responder {
    shared.in_flight.fetch_add(1, Ordering::Relaxed);
    Responder { shared: shared.clone(), id: id.clone(), done: false, effect:None }
}

fn effect_request(id: &Value, p: &Value, shared: &Arc<Shared>, config: &Arc<Config>) -> Result<Request, ProviderError> {
    let (Some(effect), Some(op), Some(args)) = (p["effect"].as_str(), p["op"].as_str(), p.get("args").filter(|a| a.is_object())) else {
        return Err(ProviderError::new("invalid", "effect, op and args are required"));
    };
    let deadline_ms = p["deadline_ms"].as_u64().filter(|d| (1..=300_000).contains(d)).ok_or_else(|| ProviderError::new("invalid", "deadline_ms must be 1-300000"))?;
    if !(p["ref"].is_null() || p["ref"].is_string()) || !(p["incarnation"].is_null() || p["incarnation"].is_string()) {
        return Err(ProviderError::new("invalid", "ref and incarnation must be strings"));
    }
    if !config.capabilities.contains_key(op) { return Err(ProviderError::new("unsupported",format!("op {op} is not a declared capability"))); }
    shared.effect_ids.lock().unwrap().begin(effect.to_string(),config.max_effect_ids)?;
    let mut response=responder(shared,id);
    response.effect=Some(effect.to_string());
    Ok(Request::Effect(EffectRequest {
        effect: effect.into(),
        op: op.into(),
        r#ref: p["ref"].as_str().map(String::from),
        incarnation: p["incarnation"].as_str().unwrap_or("").into(),
        args: args.clone(),
        deadline: Instant::now() + Duration::from_millis(deadline_ms),
        responder: response,
    }))
}

fn open_request(id: &Value, p: &Value, shared: &Arc<Shared>, config: &Arc<Config>) -> Result<Request, ProviderError> {
    if !config.capabilities.contains_key("terminal.output") {
        return Err(ProviderError::new("unsupported", "terminal.output is not declared"));
    }
    let stream = p["stream"].as_u64().ok_or_else(|| ProviderError::new("invalid", "stream id required"))?;
    let dim = |k: &str| p[k].as_u64().filter(|v| (1..=9999).contains(v)).map(|v| v as u16).ok_or_else(|| ProviderError::new("invalid", format!("{k} must be 1-9999")));
    let (cols, rows) = (dim("cols")?, dim("rows")?);
    let history_lines = p["history_lines"].as_u64().ok_or_else(|| ProviderError::new("invalid", "history_lines required"))?;
    let window = p["window_bytes"].as_u64().filter(|w| *w >= 1).ok_or_else(|| ProviderError::new("invalid", "window_bytes required"))?.min(MAX_WINDOW);
    let (Some(r), Some(inc)) = (p["ref"].as_str(), p["incarnation"].as_str()) else {
        return Err(ProviderError::new("invalid", "ref and incarnation required"));
    };
    if shared.streams.lock().unwrap().len() >= config.max_streams {
        return Err(ProviderError::new("overloaded", "max_streams open"));
    }
    shared.stream_ids.lock().unwrap().begin(stream,config.max_streams.saturating_add(256))?;
    let (tx, rx) = mpsc::unbounded_channel();
    let inner = Arc::new(StreamInner {
        state: Mutex::new(StreamState { ready: false, closed: false, credit: window, outstanding: 0, offset: 0, queued: 0 }),
        changed: Notify::new(),
        wire: tokio::sync::Mutex::new(()),
        commands: Mutex::new(Some(tx)),
    });
    shared.streams.lock().unwrap().insert(stream, inner.clone());
    Ok(Request::Open(OpenRequest {
        stream: ProviderStream { id: stream, r#ref: r.into(), incarnation: inc.into(), inner, shared: shared.clone(), commands: rx },
        cols,
        rows,
        history_lines,
        responder: responder(shared, id),
    }))
}

fn notification(method: &str, p: &Value, shared: &Arc<Shared>, config: &Arc<Config>) -> Result<(), String> {
    if !matches!(method, "stream.credit" | "stream.input" | "stream.resize" | "stream.close") {
        return Err(format!("protocol error: unknown notification {method}"));
    }
    let Some(sid) = p["stream"].as_u64() else { return Err("protocol error: stream id required".into()) };
    let Some(inner) = shared.streams.lock().unwrap().get(&sid).cloned() else { return Ok(()) }; // closed or unknown: ignored
    let reject = |e: ProviderError| {
        let line = json!({"jsonrpc": "2.0", "method": "stream.error", "params": {"generation": shared.generation(),
            "stream": sid, "error": {"kind": e.kind, "message": e.message}, "outcome": "rejected"}});
        shared.push_line(line.to_string());
    };
    let command = match method {
        "stream.credit" => {
            let n = p["bytes"].as_u64().filter(|b| *b >= 1 && *b <= u32::MAX as u64).ok_or("protocol error: bad credit")?;
            let mut s = inner.state.lock().unwrap();
            let n = n.min(s.outstanding); // never more than outstanding; ignore the excess
            s.outstanding -= n;
            s.credit += n;
            drop(s);
            inner.changed.notify_waiters();
            return Ok(());
        }
        "stream.input" => {
            if !config.capabilities.contains_key("terminal.input") {
                reject(ProviderError::new("unsupported", "terminal.input is not declared"));
                return Ok(());
            }
            match p["data"].as_str().map(|d| B64.decode(d)) {
                Some(Ok(bytes)) if bytes.len() <= MAX_DATA => StreamCommand::Input(bytes),
                Some(Ok(_)) => {
                    reject(ProviderError::new("invalid", "input over 64 KiB"));
                    return Ok(());
                }
                _ => {
                    reject(ProviderError::new("invalid", "input data must be base64"));
                    return Ok(());
                }
            }
        }
        "stream.resize" => {
            if !config.capabilities.contains_key("terminal.resize") {
                reject(ProviderError::new("unsupported", "terminal.resize is not declared"));
                return Ok(());
            }
            let dim = |k: &str| p[k].as_u64().filter(|v| (1..=9999).contains(v)).map(|v| v as u16);
            match (dim("cols"), dim("rows")) {
                (Some(c), Some(r)) => StreamCommand::Resize(c, r),
                _ => {
                    reject(ProviderError::new("invalid", "cols and rows must be 1-9999"));
                    return Ok(());
                }
            }
        }
        _ => StreamCommand::Close,
    };
    let tx = inner.commands.lock().unwrap().clone();
    let Some(tx) = tx else { return Ok(()) };
    {
        let mut s = inner.state.lock().unwrap();
        if command != StreamCommand::Close && s.queued >= config.stream_commands {
            drop(s);
            reject(ProviderError::new("overloaded", "stream command queue full; not applied"));
            return Ok(());
        }
        s.queued += 1;
    }
    let _ = tx.send(command);
    Ok(())
}

#[cfg(test)]
mod qa2_tests {
    use super::*;
    fn shared() -> Arc<Shared> {
        let (out,_rx)=mpsc::unbounded_channel(); let (closed_tx,_)=watch::channel(false);
        Arc::new(Shared { out,budget:Arc::new(Semaphore::new(1024*1024)),outgoing_bytes:1024*1024,generation:AtomicU64::new(1),pending:Mutex::new(HashMap::new()),next_id:AtomicU64::new(1),closed:AtomicBool::new(false),closed_tx,failure:Mutex::new(None),streams:Mutex::new(HashMap::new()),stream_ids:Mutex::new(IdLedger::default()),effect_ids:Mutex::new(IdLedger::default()),in_flight:AtomicU64::new(0),sync_started:AtomicBool::new(false),max_record:AtomicU64::new(MAX_RECORD as u64) })
    }
    #[test]
    fn identity_eviction_never_forgets_active_ids() {
        let mut ids=IdLedger::default();
        ids.begin("active",2).unwrap();
        for _ in 0..10 { ids.begin("recent",2).unwrap();ids.finish(&"recent");ids.begin("other",2).unwrap();ids.finish(&"other"); }
        assert!(ids.active.contains("active"));
        assert_eq!(ids.seen.len(),2);
        assert_eq!(ids.begin("active",2).unwrap_err().kind,"conflict");
    }
    #[test]
    fn sdk_effect_ids_are_completed_when_replied_or_dropped() {
        let shared=shared(); let mut config=Config::new("test","i",json!({"fixture":{}}).as_object().unwrap().clone());config.max_effect_ids=4;
        for n in 0..12 {
            let request=effect_request(&json!(n),&json!({"effect":format!("e{n}"),"op":"fixture","args":{},"deadline_ms":1000}),&shared,&Arc::new(config.clone())).unwrap();
            if let Request::Effect(effect)=request { if n%2==0 { effect.respond(Ok(Outcome::Accepted(None))); } }
            assert!(shared.effect_ids.lock().unwrap().seen.len()<=4);
            assert!(shared.effect_ids.lock().unwrap().active.is_empty());
        }
    }
    #[test]
    fn sdk_stream_ids_are_bounded_across_repeated_rejected_opens() {
        let shared=shared();let mut config=Config::new("test","i",json!({"terminal.output":{"raw":true}}).as_object().unwrap().clone());config.max_streams=1;let config=Arc::new(config);
        for n in 0..300u64 {
            let request=open_request(&json!(n),&json!({"stream":n,"ref":"r","incarnation":"i","cols":80,"rows":24,"history_lines":0,"window_bytes":1024}),&shared,&config).unwrap();
            if let Request::Open(open)=request { open.respond(Err(ProviderError::new("unsupported","fixture rejects open"))); }
            assert!(shared.stream_ids.lock().unwrap().seen.len()<=257);
            assert!(shared.stream_ids.lock().unwrap().active.is_empty());
        }
    }
}
