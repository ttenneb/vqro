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

pub mod items;

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
#[derive(Clone, PartialEq, Eq)]
pub struct ProviderError {
    pub kind: String,
    pub message: String,
    /// True only for an explicit core error reply, never a local deadline/transport failure.
    pub remote: bool,
    /// Original structured error data, including refusal reasons/outcomes and future fields.
    /// May contain credentials. Debug/Display redact known token fields; explicit data access is still secret.
    pub data: Value,
}

impl ProviderError {
    pub fn new(kind: &str, message: impl Into<String>) -> Self {
        ProviderError { kind: kind.into(), message: message.into(), remote: false, data: json!({}) }
    }
    pub fn with_data(mut self, data: Value) -> Self {
        self.data = data;
        self
    }
    fn to_json(&self) -> Value {
        let code = match self.kind.as_str() {
            "unknown_method" => -32601,
            "invalid" => -32602,
            _ => -32000,
        };
        let mut data = self.data.as_object().cloned().unwrap_or_default();
        data.insert("kind".into(), json!(self.kind));
        json!({"code": code, "message": self.message, "data": data})
    }
}

// Credentials remain available as evidence, but must not leak through routine diagnostics.
fn diagnostic_data(value: &Value, secrets: &mut Vec<String>) -> Value {
    match value {
        Value::Object(object) => Value::Object(object.iter().map(|(key, value)| {
            let value = if matches!(key.as_str(), "resume_token" | "token" | "extension_token" | "bridge_token" | "host_context") {
                if let Some(secret) = value.as_str().filter(|s| !s.is_empty()) { secrets.push(secret.into()); }
                json!("[redacted]")
            } else { diagnostic_data(value, secrets) };
            (key.clone(), value)
        }).collect()),
        Value::Array(array) => Value::Array(array.iter().map(|value| diagnostic_data(value, secrets)).collect()),
        _ => value.clone(),
    }
}
fn diagnostic_text(text: &str, secrets: &[String]) -> String {
    secrets.iter().fold(text.to_string(), |text, secret| text.replace(secret, "[redacted]"))
}
fn redact_echoes(value: &mut Value, secrets: &[String]) {
    match value {
        Value::String(text) => *text = diagnostic_text(text, secrets),
        Value::Object(object) => { for value in object.values_mut() { redact_echoes(value, secrets); } }
        Value::Array(array) => { for value in array { redact_echoes(value, secrets); } }
        _ => {}
    }
}
impl std::fmt::Debug for ProviderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut secrets = Vec::new();
        let mut data = diagnostic_data(&self.data, &mut secrets);
        secrets.sort_by_key(|s| std::cmp::Reverse(s.len()));
        // Replace raw strings before formatting, including escaped/Unicode credentials.
        redact_echoes(&mut data, &secrets);
        f.debug_struct("ProviderError").field("kind", &diagnostic_text(&self.kind, &secrets))
            .field("message", &diagnostic_text(&self.message, &secrets)).field("remote", &self.remote)
            .field("data", &data).finish()
    }
}
impl std::fmt::Display for ProviderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut secrets = Vec::new();
        let _ = diagnostic_data(&self.data, &mut secrets);
        secrets.sort_by_key(|s| std::cmp::Reverse(s.len()));
        write!(f, "{} ({})", diagnostic_text(&self.message, &secrets), diagnostic_text(&self.kind, &secrets))
    }
}

impl std::error::Error for ProviderError {}

/// What a provider declares at registration, plus local bounds.
#[derive(Clone)]
pub struct Config {
    pub name: String,
    /// Launcher-configured default identity; absent is undeclared, never implicit owner.
    pub principal:Option<String>,
    /// "backend" or "runtime".
    pub role: String,
    pub instance: String,
    pub software: Value,
    pub provider_version: String,
    /// Capability name -> parameter object (absent means unsupported; never `false`).
    pub capabilities: Map<String, Value>,
    pub max_streams: usize,
    /// Concurrent item streams (`items.stream`), a separate budget: at least agents × max viewers.
    pub max_item_streams: usize,
    pub stream_window_bytes: u64,
    /// Runtime bridges only.
    pub token: Option<String>,
    /// Runtime-only opt-in to a registration-bound credential exported explicitly to tool children.
    pub host_context:bool,
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

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config").field("name", &self.name).field("role", &self.role)
            .field("instance", &self.instance).field("token", &self.token.as_ref().map(|_| "[redacted]"))
            .finish_non_exhaustive()
    }
}

impl Config {
    pub fn new(name: impl Into<String>, instance: impl Into<String>, capabilities: Map<String, Value>) -> Self {
        let name = name.into();
        Config {
            principal:std::env::var("VQRO_PRINCIPAL").ok().filter(|principal|principal!=crate::principal::UNDECLARED),
            software: json!({"name": name, "version": "0"}),
            name,
            role: "backend".into(),
            instance: instance.into(),
            provider_version: env!("CARGO_PKG_VERSION").into(),
            capabilities,
            max_streams: 512,
            max_item_streams: 1024,
            stream_window_bytes: MAX_WINDOW,
            token: None,
            host_context:false,
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
    /// Handed to the native layer without an acknowledgement (for example a UI answer).
    Dispatched(Option<Value>),
    Accepted(Option<Value>),
    Observed(Option<Value>),
    Rejected { kind: String, message: String },
}

impl Outcome {
    fn to_json(&self) -> Value {
        match self {
            Outcome::Dispatched(d) | Outcome::Accepted(d) | Outcome::Observed(d) => {
                let name = match self { Outcome::Dispatched(_) => "dispatched", Outcome::Accepted(_) => "accepted", _ => "observed" };
                let mut v = json!({"outcome": name});
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
    /// Counted against `max_in_flight` (effects). Stream opens have their own budget (agent-v0 §9.2).
    counted: bool,
}

impl Responder {
    fn send(mut self, result: Result<Value, ProviderError>) {
        self.done = true;
        self.shared.reply(&self.id, result, self.counted);
    }
}

impl Drop for Responder {
    fn drop(&mut self) {
        if !self.done {
            self.shared.reply(&self.id, Err(ProviderError::new("backend", "handler dropped the request; outcome unknown")), self.counted);
        }
        if let Some(effect)=&self.effect { self.shared.effect_ids.lock().unwrap().finish(effect); }
    }
}

pub struct EffectRequest {
    pub effect: String,
    pub author:String,
    pub attribution:crate::principal::Attribution,
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
    /// Explicit caller links, unchanged; neither accessor establishes native evidence.
    pub fn cause(&self)->Option<&Value>{self.attribution.causality.cause.as_ref()}
    pub fn correlation(&self)->Option<&str>{self.attribution.causality.correlation.as_deref()}
    /// Reply with a D10 outcome. Adapter refusals (including `busy`/`stale`) must use
    /// `Ok(Outcome::Rejected { .. })`: an arbitrary error does NOT establish no effect.
    /// The core rejects only its fixed protocol-error kinds; other errors become indeterminate.
    /// Extra error data is serialized here but not forwarded by the core. Never resend.
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
    /// Core-supplied opener context; diagnostic metadata, not delegated authority.
    pub author:String,
    pub attribution:crate::principal::Attribution,
    pub stream: ProviderStream,
    pub cols: u16,
    pub rows: u16,
    pub history_lines: u64,
    /// Item streams (`items.stream`): the viewer's `{after?: {epoch, seq}}`; no dimensions.
    pub items: Option<Value>,
    responder: Responder,
}

impl OpenRequest {
    pub fn cause(&self)->Option<&Value>{self.attribution.causality.cause.as_ref()}
    pub fn correlation(&self)->Option<&str>{self.attribution.causality.correlation.as_deref()}
    /// Reply to an item-stream open: `continue` (only for an open with `after` in this same epoch; records strictly
    /// after the viewer's cursor follow) or `reset` (`snapshot begin`, puts, `snapshot end` follow in-stream), with
    /// the current epoch (1-256 bytes; anything else is refused here as `invalid` and the stream is not opened).
    /// Answer opens quickly (one cut under the agent lock, as [`items::ItemJournal::open`]): opens share the request
    /// channel with effects and are not bounded by `max_in_flight`.
    pub fn respond_items(self, reset: bool, epoch: &str) -> Option<ProviderStream> {
        if epoch.is_empty() || epoch.len() > 256 {
            return self.respond(Err(ProviderError::new("invalid", "item epoch must be 1-256 bytes")));
        }
        let OpenRequest { stream, responder, .. } = self;
        responder.send(Ok(json!({"initial": if reset { "reset" } else { "continue" }, "epoch": epoch})));
        stream.inner.set_ready();
        Some(stream)
    }
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
    // Core close cancels output, but queued commands and the provider's final marker survive.
    close_requested: bool,
    credit: u64,
    outstanding: u64,
    offset: u64,
    queued: usize,
}

struct StreamInner {
    /// An item stream (counted against `max_item_streams`, not `max_streams`).
    items: bool,
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
                    if s.closed || s.close_requested || self.shared.is_closed() {
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
        if !matches!(gap, "backend_dropped" | "paused" | "viewer_slow" | "retention") {
            // An unknown reason still ends the stream (as invalid), never leaves it silently open.
            let _ = self.end("stream.closed", json!({"reason": {"reason": "invalid", "message": format!("unknown gap kind {gap}")}})).await;
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

// Removing a cancelled local waiter does not cancel an already queued/written request or resend it.
struct PendingCall<'a> { pending: &'a Pending, id: &'a str }
impl Drop for PendingCall<'_> {
    fn drop(&mut self) { self.pending.lock().unwrap().remove(self.id); }
}

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
    /// Stream limits granted at registration (`limits.max_streams` / `max_item_streams`), `u64::MAX` before.
    granted_streams: AtomicU64,
    granted_item_streams: AtomicU64,
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
    fn reply(&self, id: &Value, result: Result<Value, ProviderError>, counted: bool) {
        if counted { self.in_flight.fetch_sub(1, Ordering::Relaxed); }
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
    registration: Arc<Value>,
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
            granted_streams: AtomicU64::new(u64::MAX),
            granted_item_streams: AtomicU64::new(u64::MAX),
            sync_started: AtomicBool::new(false),
            max_record: AtomicU64::new(MAX_RECORD as u64),
        });
        let (requests, requests_rx) = mpsc::unbounded_channel();
        tokio::spawn(write_lines(writer, out_rx, shared.clone()));
        let config = Arc::new(config);
        tokio::spawn(read_lines(reader, shared.clone(), config.clone(), requests));
        let mut provider = Provider { shared, config: config.clone(), window: config.stream_window_bytes.min(MAX_WINDOW), registration: Arc::new(Value::Null) };
        let result = provider.handshake().await;
        if let Err(e) = result {
            provider.shared.disconnect(&format!("registration failed: {e}"));
            return Err(e);
        }
        Ok((provider, requests_rx))
    }

    async fn handshake(&mut self) -> Result<(), ProviderError> {
        let ext = self.config.extension.clone().or_else(|| std::env::var("VQRO_EXTENSION").ok()).unwrap_or_else(|| self.config.name.clone());
        let mut hello=json!({"extension":ext,"extension_token":std::env::var("VQRO_EXTENSION_TOKEN").ok()});
        if let Some(principal)=&self.config.principal{hello["identity"]=json!({"kind":"principal","principal":principal});}
        self.call("hello",hello).await?;
        let mut params = json!({
            "name": self.config.name, "role": self.config.role, "instance": self.config.instance,
            "software": self.config.software, "provider_version": self.config.provider_version,
            "capabilities": self.config.capabilities,"host_context":self.config.host_context,
            "limits": {"max_streams": self.config.max_streams, "max_item_streams": self.config.max_item_streams, "stream_window_bytes": self.window},
        });
        if let Some(t) = &self.config.token {
            params["token"] = json!(t);
        }
        let r = self.call("provider.register", params).await?;
        self.registration = Arc::new(r.clone());
        let generation = r["generation"].as_u64().filter(|g| *g >= 1).ok_or_else(|| ProviderError::new("invalid", "register: bad generation"))?;
        if let Some(w) = r["limits"]["stream_window_bytes"].as_u64() {
            self.window = w.clamp(1, self.window);
        }
        if let Some(m) = r["limits"]["max_record_bytes"].as_u64() {
            self.shared.max_record.store(m.clamp(1024, MAX_RECORD as u64), Ordering::Relaxed);
        }
        // The core may grant less than asked; admit opens against the granted value.
        if let Some(m) = r["limits"]["max_streams"].as_u64() { self.shared.granted_streams.store(m, Ordering::Relaxed); }
        if let Some(m) = r["limits"]["max_item_streams"].as_u64() { self.shared.granted_item_streams.store(m, Ordering::Relaxed); }
        self.shared.generation.store(generation, Ordering::Relaxed);
        Ok(())
    }

    /// The acknowledged registration, including opaque runtime resume credentials and future metadata.
    /// Contains credentials: callers must not log it. Clones share this immutable reply.
    pub fn registration(&self) -> &Value {
        &self.registration
    }

    /// Last observed runtime credential. Observing it does not retry or resend anything.
    pub fn resume_token(&self) -> Option<&str> {
        self.registration["resume_token"].as_str().filter(|token| !token.is_empty())
    }

    /// Sensitive map for tools ONLY, never human shells or services. Clears inherited owner identity.
    /// It does not supply endpoint attribution and must not be logged or persisted.
    pub fn tool_context_env(&self,socket:&str)->Option<std::collections::BTreeMap<String,String>>{
        let token=self.registration["host_context"].as_str().filter(|token|!token.is_empty())?;
        Some(std::collections::BTreeMap::from([("VQRO_SOCKET".into(),socket.into()),("VQRO_SOCKET_SOURCE".into(),String::new()),("VQRO_PRINCIPAL".into(),crate::principal::UNDECLARED.into()),("VQRO_HOST_CONTEXT".into(),token.into())]))
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
        let _pending = PendingCall { pending: &self.shared.pending, id: &id };
        let line = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string();
        self.shared.send_line(line).await?;
        match tokio::time::timeout(self.config.call_timeout, rx).await {
            Ok(Ok(r)) => r,
            Ok(Err(_)) => Err(ProviderError::new("unavailable", "connection closed")),
            Err(_) => Err(ProviderError::new("timeout", "no answer before the deadline; delivery unknown, never resent")),
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
                Some(e) => {
                    let mut error = ProviderError::new(e["data"]["kind"].as_str().unwrap_or("backend"), e["message"].as_str().unwrap_or("core error"))
                        .with_data(e.get("data").cloned().unwrap_or(Value::Null));
                    error.remote = true;
                    Err(error)
                }
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
    if method != "stream.open" && shared.in_flight.load(Ordering::Relaxed) as usize >= config.max_in_flight {
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
    Responder { shared: shared.clone(), id: id.clone(), done: false, effect:None, counted: true }
}

/// Stream opens are bounded by `max_streams` / `max_item_streams`, not by the effect in-flight limit.
fn open_responder(shared: &Arc<Shared>, id: &Value) -> Responder {
    Responder { shared: shared.clone(), id: id.clone(), done: false, effect:None, counted: false }
}

// Read only the known scalar context and bounded causal object: never serde-flatten the entire
// effect/open (including its opaque args) into an intermediate attribution buffer.
fn request_context(p:&Value)->Result<(String,crate::principal::Attribution),ProviderError> {
    let scalar=|key:&str|match p.get(key){None|Some(Value::Null)=>Ok(None),Some(Value::String(value))=>Ok(Some(value.clone())),Some(_)=>Err(ProviderError::new("invalid",format!("{key} must be a string")))};
    let author=match p.get("author"){None=>crate::principal::UNDECLARED.into(),Some(Value::String(author))=>author.clone(),Some(_)=>return Err(ProviderError::new("invalid","author must be a core author string"))};
    let label=if p["label"].is_null(){None}else{crate::principal::diagnostic_label(p).map_err(|error|ProviderError::new(error.kind(),error.message.clone()))?};
    let causality=crate::causality::Causality::parse(p).map_err(|error|ProviderError::new(error.kind(),error.message.clone()))?;
    let attribution=crate::principal::Attribution{actor:scalar("actor")?,principal:scalar("principal")?,initiator:scalar("initiator")?,label,causality,authority:None};
    Ok((author,attribution))
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
    let(author,attribution)=request_context(p)?;
    shared.effect_ids.lock().unwrap().begin(effect.to_string(),config.max_effect_ids)?;
    let mut response=responder(shared,id);
    response.effect=Some(effect.to_string());
    Ok(Request::Effect(EffectRequest {
        effect: effect.into(),
        author,attribution,
        op: op.into(),
        r#ref: p["ref"].as_str().map(String::from),
        incarnation: p["incarnation"].as_str().unwrap_or("").into(),
        args: args.clone(),
        deadline: Instant::now() + Duration::from_millis(deadline_ms),
        responder: response,
    }))
}

fn open_request(id: &Value, p: &Value, shared: &Arc<Shared>, config: &Arc<Config>) -> Result<Request, ProviderError> {
    // Item channel (agent-v0 §4): `channel: "items"`, optional exact cursor `after`, no dimensions.
    let items = match p["channel"].as_str() {
        None | Some("terminal") => None,
        Some("items") => Some(json!({"after": p.get("after").cloned().unwrap_or(Value::Null)})),
        Some(other) => return Err(ProviderError::new("unsupported", format!("unknown stream channel {other}"))),
    };
    let needed = if items.is_some() { "items.stream" } else { "terminal.output" };
    if !config.capabilities.contains_key(needed) {
        return Err(ProviderError::new("unsupported", format!("{needed} is not declared")));
    }
    let stream = p["stream"].as_u64().ok_or_else(|| ProviderError::new("invalid", "stream id required"))?;
    let dim = |k: &str| p[k].as_u64().filter(|v| (1..=9999).contains(v)).map(|v| v as u16).ok_or_else(|| ProviderError::new("invalid", format!("{k} must be 1-9999")));
    let (cols, rows) = if items.is_some() { (0, 0) } else { (dim("cols")?, dim("rows")?) };
    let history_lines = if items.is_some() { 0 } else { p["history_lines"].as_u64().ok_or_else(|| ProviderError::new("invalid", "history_lines required"))? };
    let window = p["window_bytes"].as_u64().filter(|w| *w >= 1).ok_or_else(|| ProviderError::new("invalid", "window_bytes required"))?.min(MAX_WINDOW);
    let (Some(r), Some(inc)) = (p["ref"].as_str(), p["incarnation"].as_str()) else {
        return Err(ProviderError::new("invalid", "ref and incarnation required"));
    };
    let(author,attribution)=request_context(p)?;
    {
        let streams = shared.streams.lock().unwrap();
        let open = streams.values().filter(|s| s.items == items.is_some()).count();
        let granted = |g: &AtomicU64, own: usize| (g.load(Ordering::Relaxed).min(own as u64)) as usize;
        if items.is_some() && open >= granted(&shared.granted_item_streams, config.max_item_streams) { return Err(ProviderError::new("overloaded", "max_item_streams open")); }
        if items.is_none() && open >= granted(&shared.granted_streams, config.max_streams) { return Err(ProviderError::new("overloaded", "max_streams open")); }
    }
    shared.stream_ids.lock().unwrap().begin(stream,config.max_streams.saturating_add(config.max_item_streams).saturating_add(256))?;
    let (tx, rx) = mpsc::unbounded_channel();
    let inner = Arc::new(StreamInner {
        items: items.is_some(),
        state: Mutex::new(StreamState { ready: false, closed: false, close_requested: false, credit: window, outstanding: 0, offset: 0, queued: 0 }),
        changed: Notify::new(),
        wire: tokio::sync::Mutex::new(()),
        commands: Mutex::new(Some(tx)),
    });
    shared.streams.lock().unwrap().insert(stream, inner.clone());
    Ok(Request::Open(OpenRequest {
        author,attribution,
        stream: ProviderStream { id: stream, r#ref: r.into(), incarnation: inc.into(), inner, shared: shared.clone(), commands: rx },
        cols,
        rows,
        history_lines,
        items,
        responder: open_responder(shared, id),
    }))
}

fn notification(method: &str, p: &Value, shared: &Arc<Shared>, config: &Arc<Config>) -> Result<(), String> {
    if !matches!(method, "stream.credit" | "stream.input" | "stream.resize" | "stream.close") {
        return Err(format!("protocol error: unknown notification {method}"));
    }
    let Some(sid) = p["stream"].as_u64() else { return Err("protocol error: stream id required".into()) };
    let Some(inner) = shared.streams.lock().unwrap().get(&sid).cloned() else { return Ok(()) }; // closed or unknown: ignored
    if inner.state.lock().unwrap().close_requested { return Ok(()) }
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
    let closing = command == StreamCommand::Close;
    {
        let mut s = inner.state.lock().unwrap();
        if command != StreamCommand::Close && s.queued >= config.stream_commands {
            drop(s);
            reject(ProviderError::new("overloaded", "stream command queue full; not applied"));
            return Ok(());
        }
        s.queued += 1;
        if closing { s.close_requested = true; }
    }
    let _ = tx.send(command);
    if closing { inner.changed.notify_waiters(); }
    Ok(())
}

#[cfg(test)]
mod qa2_tests {
    use super::*;
    fn shared() -> Arc<Shared> {
        let (out,_rx)=mpsc::unbounded_channel(); let (closed_tx,_)=watch::channel(false);
        Arc::new(Shared { out,budget:Arc::new(Semaphore::new(1024*1024)),outgoing_bytes:1024*1024,generation:AtomicU64::new(1),pending:Mutex::new(HashMap::new()),next_id:AtomicU64::new(1),closed:AtomicBool::new(false),closed_tx,failure:Mutex::new(None),streams:Mutex::new(HashMap::new()),stream_ids:Mutex::new(IdLedger::default()),effect_ids:Mutex::new(IdLedger::default()),in_flight:AtomicU64::new(0),granted_streams:AtomicU64::new(u64::MAX),granted_item_streams:AtomicU64::new(u64::MAX),sync_started:AtomicBool::new(false),max_record:AtomicU64::new(MAX_RECORD as u64) })
    }
    fn caller() -> (Provider, mpsc::UnboundedReceiver<(String, Option<OwnedSemaphorePermit>)>) {
        let (out, rx) = mpsc::unbounded_channel();
        let mut shared = shared(); Arc::get_mut(&mut shared).unwrap().out = out;
        (Provider { shared, config: Arc::new(Config::new("test", "i", serde_json::Map::new())), window: 1024, registration: Arc::new(json!({})) }, rx)
    }
    #[tokio::test]
    async fn cancelled_call_releases_waiter_while_waiting_for_write_budget() {
        let (provider, mut rx) = caller();
        let permit = provider.shared.budget.clone().acquire_many_owned(1024*1024).await.unwrap();
        let mut call = Box::pin(provider.call("workspace.get", json!({"key":"owner"})));
        tokio::select! { biased; result = &mut call => panic!("unexpected reply: {result:?}"), _ = std::future::ready(()) => {} }
        assert_eq!(provider.shared.pending.lock().unwrap().len(), 1);
        assert!(rx.try_recv().is_err());
        drop(call);
        assert!(provider.shared.pending.lock().unwrap().is_empty());
        drop(permit);
        assert!(rx.try_recv().is_err(), "cancelled before enqueue, never submitted later");
    }
    #[tokio::test]
    async fn cancelled_calls_release_waiters_after_enqueue_without_replay() {
        let (provider, mut rx) = caller();
        for _ in 0..64 {
            let mut call = Box::pin(provider.call("workspace.get", json!({"key":"owner"})));
            let (line, permit) = tokio::select! { biased; result = &mut call => panic!("unexpected reply: {result:?}"), row = rx.recv() => row.unwrap() };
            let id = serde_json::from_str::<Value>(&line).unwrap()["id"].as_str().unwrap().to_owned();
            assert_eq!(provider.shared.pending.lock().unwrap().len(), 1);
            drop(call);
            assert!(provider.shared.pending.lock().unwrap().is_empty());
            assert!(provider.shared.pending.lock().unwrap().remove(&id).is_none(), "late reply has no waiter");
            drop(permit);
            assert!(rx.try_recv().is_err(), "local cancellation never resends the request");
        }
        let mut call = Box::pin(provider.call("workspace.get", json!({"key":"fresh"})));
        let (line, _) = tokio::select! { biased; result = &mut call => panic!("unexpected reply: {result:?}"), row = rx.recv() => row.unwrap() };
        let id = serde_json::from_str::<Value>(&line).unwrap()["id"].as_str().unwrap().to_owned();
        provider.shared.pending.lock().unwrap().remove(&id).unwrap().send(Ok(json!({"fresh":true}))).unwrap();
        assert_eq!(call.await.unwrap(), json!({"fresh":true}));
        assert!(provider.shared.pending.lock().unwrap().is_empty());
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
    fn item_open(shared: &Arc<Shared>, config: &Arc<Config>, n: u64) -> Result<Request, ProviderError> {
        open_request(&json!(n), &json!({"stream": n, "ref": "r", "incarnation": "i", "channel": "items", "window_bytes": 1024}), shared, config)
    }
    #[test]
    fn item_streams_have_their_own_budget_and_opens_are_outside_the_effect_limit() {
        let shared = shared();
        let mut config = Config::new("test", "i", json!({"terminal.output": {}, "items.stream": {}, "fixture": {}}).as_object().unwrap().clone());
        config.max_streams = 1; config.max_item_streams = 2; config.max_in_flight = 1;
        let config = Arc::new(config);
        // One effect in flight fills max_in_flight; opens are not counted against it (agent-v0 §9.2).
        let _effect = effect_request(&json!("e"), &json!({"effect": "e1", "op": "fixture", "args": {}, "deadline_ms": 1000}), &shared, &config).unwrap();
        assert_eq!(shared.in_flight.load(Ordering::Relaxed), 1);
        let mut kept = Vec::new();
        let Ok(Request::Open(t)) = open_request(&json!(10), &json!({"stream": 10, "ref": "r", "incarnation": "i", "cols": 80, "rows": 24, "history_lines": 0, "window_bytes": 1024}), &shared, &config) else { panic!("terminal open") };
        assert!(t.items.is_none());
        kept.push(t.respond(Ok(Initial::None)).unwrap());
        for n in 0..2 {
            let Ok(Request::Open(o)) = item_open(&shared, &config, n) else { panic!("item open {n}") };
            assert_eq!(o.items, Some(json!({"after": null})));
            kept.push(o.respond_items(true, "e1").unwrap());
        }
        assert_eq!(shared.in_flight.load(Ordering::Relaxed), 1, "opens must not count as in-flight effects");
        // Each budget is full independently: a terminal stream did not consume an item slot.
        assert_eq!(item_open(&shared, &config, 5).err().unwrap().kind, "overloaded");
        assert_eq!(open_request(&json!(11), &json!({"stream": 11, "ref": "r", "incarnation": "i", "cols": 80, "rows": 24, "history_lines": 0, "window_bytes": 1024}), &shared, &config).err().unwrap().kind, "overloaded");
        assert_eq!(open_request(&json!(12), &json!({"stream": 12, "ref": "r", "incarnation": "i", "channel": "video", "window_bytes": 1024}), &shared, &config).err().unwrap().kind, "unsupported");
    }
    #[tokio::test]
    async fn retention_is_a_gap_reason_and_an_unknown_reason_still_ends_the_stream() {
        let (out, mut rx) = mpsc::unbounded_channel(); let (closed_tx, _) = watch::channel(false);
        let shared = Arc::new(Shared { out,budget:Arc::new(Semaphore::new(1024*1024)),outgoing_bytes:1024*1024,generation:AtomicU64::new(1),pending:Mutex::new(HashMap::new()),next_id:AtomicU64::new(1),closed:AtomicBool::new(false),closed_tx,failure:Mutex::new(None),streams:Mutex::new(HashMap::new()),stream_ids:Mutex::new(IdLedger::default()),effect_ids:Mutex::new(IdLedger::default()),in_flight:AtomicU64::new(0),granted_streams:AtomicU64::new(u64::MAX),granted_item_streams:AtomicU64::new(u64::MAX),sync_started:AtomicBool::new(false),max_record:AtomicU64::new(MAX_RECORD as u64) });
        let config = Arc::new(Config::new("test", "i", json!({"items.stream": {}}).as_object().unwrap().clone()));
        let Ok(Request::Open(a)) = item_open(&shared, &config, 1) else { panic!() };
        let Ok(Request::Open(b)) = item_open(&shared, &config, 2) else { panic!() };
        let (a, b) = (a.respond_items(false, "e").unwrap(), b.respond_items(false, "e").unwrap());
        a.gap("retention").await.unwrap();
        assert_eq!(b.gap("exploded").await.unwrap_err().kind, "invalid");
        let mut lines = Vec::new();
        while let Ok((line, _)) = rx.try_recv() { lines.push(serde_json::from_str::<Value>(&line).unwrap()); }
        let gap = lines.iter().find(|l| l["params"]["stream"] == 1 && l["method"] == "stream.gap").expect("retention gap sent");
        assert_eq!(gap["params"]["reason"]["gap"], "retention");
        let closed = lines.iter().find(|l| l["params"]["stream"] == 2).expect("unknown reason must still end the stream");
        assert_eq!(closed["method"], "stream.closed");
        assert_eq!(closed["params"]["reason"]["reason"], "invalid");
        assert!(b.is_closed());
    }
    #[test]
    fn sdk_stream_ids_are_bounded_across_repeated_rejected_opens() {
        let shared=shared();let mut config=Config::new("test","i",json!({"terminal.output":{"raw":true}}).as_object().unwrap().clone());config.max_streams=1;config.max_item_streams=0;let config=Arc::new(config);
        for n in 0..300u64 {
            let request=open_request(&json!(n),&json!({"stream":n,"ref":"r","incarnation":"i","cols":80,"rows":24,"history_lines":0,"window_bytes":1024}),&shared,&config).unwrap();
            if let Request::Open(open)=request { open.respond(Err(ProviderError::new("unsupported","fixture rejects open"))); }
            assert!(shared.stream_ids.lock().unwrap().seen.len()<=257);
            assert!(shared.stream_ids.lock().unwrap().active.is_empty());
        }
    }
}
