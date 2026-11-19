//! Backend adapters (D14, D15). An adapter maps one backend onto vqro resources and declares what it
//! can do. It translates protocols honestly and never invents guarantees the backend lacks.
//!
//! Adapters run as tasks that talk to the core through channels, so the core never blocks on a
//! backend and a fake adapter can stand in as a deterministic test fixture.

pub mod fake;
pub mod herdr;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot, OwnedSemaphorePermit};
use std::collections::BTreeMap;
use std::time::Instant;
use crate::provider::ProviderInfo;

/// Startup selection only. Execution-specific setup stays in the adapter layer, not the server.
/// `vqro serve` uses `Extensions`: backends are provider extensions (`extension/<name>` entries,
/// e.g. vqro-herdr, vqro-tmux). The in-process adapters remain for tests and embedders.
#[derive(Clone)]
pub enum Backend { Herdr(Vec<String>), Fake, Extensions }
impl Backend {
    /// The in-process adapter to install, if any.
    pub fn start(self, events: mpsc::UnboundedSender<(String, AdapterEvent)>) -> Option<AdapterHandle> {
        match self {
            Self::Herdr(argv) => Some(herdr::start("herdr".into(), argv, events)),
            Self::Fake => Some(fake::start("fake".into(), events)),
            Self::Extensions => None,
        }
    }
}

#[derive(Debug)]
pub enum AdapterError { Rejected(String), RejectedKind { kind: String, message: String }, Backend(String), Indeterminate(String), Timeout(String) }
impl From<String> for AdapterError { fn from(message: String) -> Self { Self::Rejected(message) } }
impl From<&str> for AdapterError { fn from(message: &str) -> Self { Self::Rejected(message.into()) } }
impl std::fmt::Display for AdapterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self { Self::Rejected(s) | Self::Backend(s) | Self::Indeterminate(s) | Self::Timeout(s) => s.fmt(f), Self::RejectedKind { message, .. } => message.fmt(f) }
    }
}
impl AdapterError {
    pub fn rpc(self) -> crate::rpc::RpcError {
        match self {
            Self::Rejected(s) => crate::provider::rejected("backend", s),
            Self::RejectedKind { kind, message } => crate::provider::rejected(&kind, message),
            Self::Backend(s) => {
                let mut error = crate::rpc::RpcError::new("backend", s);
                error.data.as_mut().unwrap()["outcome"] = serde_json::json!("indeterminate");
                error
            }
            Self::Indeterminate(s) => crate::provider::indeterminate(s),
            Self::Timeout(s) => {
                let mut error = crate::rpc::RpcError::new("timeout", s);
                error.data.as_mut().unwrap()["outcome"] = serde_json::json!("indeterminate");
                error
            }
        }
    }
}

/// What a backend says about one of its resources. Observed, not authoritative (D6).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ResourceFacts {
    /// The backend's own identifier, e.g. herdr-core's `t3`.
    pub backend_ref: String,
    #[serde(default)]
    pub incarnation: String,
    /// Open set, declared by the adapter: "terminal", later "agent", ...
    pub kind: String,
    pub name: Option<String>,
    pub title: Option<String>,
    /// "live" or "finished".
    pub state: String,
    /// Program status as reported (OSC 7501 via the backend), or null.
    #[serde(default)]
    pub status: Value,
    /// Exit details when finished, as reported.
    #[serde(default)]
    pub exit: Value,
    #[serde(default)]
    pub argv: Vec<String>,
    /// Opaque kind-specific facts, preserved without backend/runtime policy in core.
    #[serde(default = "empty_data")]
    pub data: Value,
    /// Where the program is working now (the backend's report, e.g. OSC 7 or process cwd), if known.
    #[serde(default)]
    pub cwd: Option<String>,
}

impl ResourceFacts {
    pub fn valid(&self) -> bool {
        !self.backend_ref.is_empty() && !self.kind.is_empty() && matches!(self.state.as_str(), "live" | "finished")
            && [&self.data,&self.status,&self.exit].into_iter().all(|value|crate::limits::json_depth(value,crate::limits::JSON_DEPTH).is_ok())
            && self.data.is_object() && self.data.to_string().len() <= 64 * 1024
    }
}

fn empty_data() -> Value { serde_json::json!({}) }

pub enum AdapterEvent {
    /// FIFO acknowledgement after earlier native metadata reached its consumer.
    Consumed(oneshot::Sender<()>),
    Registered(ProviderInfo),
    /// Full current list; resources not in it are gone. Sent on connect and after a resync.
    Snapshot(Vec<ResourceFacts>),
    Upsert(ResourceFacts),
    Removed { reference: String, incarnation: String, reason:Value },
    /// The adapter lost or regained its backend.
    Connected(bool, Option<String>),
}

pub const STREAM_BUFFER_BYTES: usize = 256 * 1024;
pub const MAX_STREAM_CHUNK: usize = 64 * 1024;

pub enum StreamInput {
    Data(Vec<u8>),
    Resize(u16, u16),
    Close,
    /// Bytes handed to the connection's bounded writer queue, not merely enqueued by the adapter.
    Consumed(u32),
}

impl StreamInput {
    pub fn send(self, tx: &crate::queue::Sender<Self>) -> Result<(), crate::queue::QueueError> {
        let bytes = match &self { Self::Data(d) => d.len(), _ => 64 };
        tx.try_send(self, bytes)
    }
}

pub enum StreamOutput {
    Data(Vec<u8>),
    Error(Value),
    /// The stream ended, with the backend's reason (JSON).
    Closed(Value),
}

pub struct Stream {
    pub initial: &'static str,
    /// Item streams only: the provider's open reply (`epoch`), passed to the viewer unchanged.
    pub epoch: Option<String>,
    pub input: crate::queue::Sender<StreamInput>,
    pub output: crate::queue::Receiver<StreamOutput>,
    /// Wire termination is separate from data so a full data window cannot lose its end marker.
    /// Its permit belongs to the provider-wide control budget until handed downstream.
    pub terminal: Option<oneshot::Receiver<StreamEnd>>,
}

pub struct StreamEnd {
    pub gap: bool,
    pub reason: Value,
    pub _permit: Option<OwnedSemaphorePermit>,
}

#[derive(Clone)]
pub struct EffectContext {
    pub effect: String,
    pub deadline: Instant,
    pub target: Option<(String, String)>,
    /// Rechecked at the adapter's native dispatch seam, not just RPC admission.
    pub caller: Option<crate::ext::Owner>,
}

#[derive(Debug)]
pub struct EffectResult {
    pub outcome: &'static str,
    pub data: Value,
    pub facts: Option<ResourceFacts>,
}
impl EffectResult {
    pub fn accepted(data: Value) -> Self { Self { outcome: "accepted", data, facts: None } }
}

pub struct SpawnRequest {
    pub argv: Vec<String>,
    pub cwd: Option<String>,
    pub name: Option<String>,
    pub env: BTreeMap<String, String>,
    pub cols: Option<u16>,
    pub rows: Option<u16>,
    pub placement: Option<Value>,
}

type Reply<T> = oneshot::Sender<Result<T, AdapterError>>;

pub enum AdapterCmd {
    /// Stop this provider link/worker only, never backend-owned terminal execution.
    Shutdown(Reply<()>),
    /// A core route binds queued work to the native connection that accepted the registration.
    Bound { connection: u64, context: Option<EffectContext>, command: Box<AdapterCmd> },
    Spawn(SpawnRequest, Reply<ResourceFacts>),
    Kill(String, Reply<EffectResult>),
    Invoke { op: String, reference: String, incarnation: String, args: Value, reply: Reply<EffectResult> },
    Attach { backend_ref: String, cols: u16, rows: u16, history: u32, items: Option<Value>, reply: Reply<Stream> },
}

pub(crate) fn reject_command(command: AdapterCmd, why: &str) {
    match command {
        AdapterCmd::Bound { command, .. } => reject_command(*command, why),
        AdapterCmd::Shutdown(reply) => { let _ = reply.send(Err(AdapterError::Rejected(why.into()))); }
        AdapterCmd::Spawn(_, reply) => { let _ = reply.send(Err(AdapterError::Rejected(why.into()))); }
        AdapterCmd::Kill(_, reply) | AdapterCmd::Invoke { reply, .. } => { let _ = reply.send(Err(AdapterError::Rejected(why.into()))); }
        AdapterCmd::Attach { reply, .. } => { let _ = reply.send(Err(AdapterError::Rejected(why.into()))); }
    }
}

pub fn command_for(mut command: AdapterCmd, current: u64) -> Option<AdapterCmd> {
    loop {
        match command {
            AdapterCmd::Bound { connection, context, command: inner } => {
                if context.as_ref().and_then(|c|c.caller.as_ref()).is_some_and(|o|!o.is_live()) { reject_command(*inner,"supervised caller retired before dispatch"); return None; }
                if connection != current { reject_command(*inner, "provider connection changed before dispatch"); return None; }
                command = *inner;
            }
            native => {
                let cancelled = match &native {
                    AdapterCmd::Spawn(_, reply) => reply.is_closed(),
                    AdapterCmd::Kill(_, reply) | AdapterCmd::Invoke { reply, .. } => reply.is_closed(),
                    AdapterCmd::Attach { reply, .. } => reply.is_closed(),
                    AdapterCmd::Shutdown(_) => false,
                    AdapterCmd::Bound { .. } => unreachable!(),
                };
                return if cancelled { None } else { Some(native) };
            }
        }
    }
}

/// The core's handle on a running adapter.
#[derive(Clone)]
pub struct AdapterHandle {
    pub name: String,
    pub capabilities: Value,
    pub session: Option<u64>,
    pub context: Option<EffectContext>,
    pub cmds: mpsc::UnboundedSender<AdapterCmd>,
}

impl AdapterHandle {
    pub fn for_effect(&self, context: EffectContext) -> Self {
        let mut handle = self.clone();
        handle.context = Some(context);
        handle
    }

    fn bind(&self, command: AdapterCmd) -> AdapterCmd {
        match self.session {
            Some(connection) => AdapterCmd::Bound { connection, context: self.context.clone(), command: Box::new(command) },
            None => command, // Direct adapter fixture calls; core routes are always bound.
        }
    }

    pub async fn shutdown(&self) -> Result<(), AdapterError> {
        let (tx, rx) = oneshot::channel();
        if self.cmds.send(AdapterCmd::Shutdown(tx)).is_err() { return Ok(()); }
        rx.await.map_err(|_| AdapterError::Indeterminate("provider shutdown unconfirmed".into()))?
    }

    pub async fn spawn(&self, req: SpawnRequest) -> Result<ResourceFacts, AdapterError> {
        let (tx, rx) = oneshot::channel();
        self.cmds.send(self.bind(AdapterCmd::Spawn(req, tx))).map_err(|_| AdapterError::Rejected("provider stopped before dispatch".into()))?;
        rx.await.map_err(|_| AdapterError::Indeterminate("provider stopped before replying".into()))?
    }
    pub async fn kill(&self, backend_ref: String) -> Result<EffectResult, AdapterError> {
        let (tx, rx) = oneshot::channel();
        self.cmds.send(self.bind(AdapterCmd::Kill(backend_ref, tx))).map_err(|_| AdapterError::Rejected("provider stopped before dispatch".into()))?;
        rx.await.map_err(|_| AdapterError::Indeterminate("provider stopped before replying".into()))?
    }
    pub async fn invoke(&self, op: String, reference: String, incarnation: String, args: Value) -> Result<EffectResult, AdapterError> {
        if op == "terminal.kill" { return self.kill(reference).await; }
        let (tx, rx) = oneshot::channel();
        self.cmds.send(self.bind(AdapterCmd::Invoke { op, reference, incarnation, args, reply: tx })).map_err(|_| AdapterError::Rejected("provider stopped before dispatch".into()))?;
        rx.await.map_err(|_| AdapterError::Indeterminate("provider stopped before replying".into()))?
    }

    pub async fn attach(&self, backend_ref: String, cols: u16, rows: u16, history: u32) -> Result<Stream, AdapterError> {
        let (tx, rx) = oneshot::channel();
        self.cmds.send(self.bind(AdapterCmd::Attach { backend_ref, cols, rows, history, items: None, reply: tx })).map_err(|_| AdapterError::Rejected("provider stopped before dispatch".into()))?;
        rx.await.map_err(|_| AdapterError::Indeterminate("provider stopped before replying".into()))?
    }
    /// A non-terminal item stream (JSONL records, no dimensions): same credit, window and closure rules.
    pub async fn attach_items(&self, backend_ref: String, items: Value) -> Result<Stream, AdapterError> {
        let (tx, rx) = oneshot::channel();
        self.cmds.send(self.bind(AdapterCmd::Attach { backend_ref, cols: 0, rows: 0, history: 0, items: Some(items), reply: tx })).map_err(|_| AdapterError::Rejected("provider stopped before dispatch".into()))?;
        rx.await.map_err(|_| AdapterError::Indeterminate("provider stopped before replying".into()))?
    }
}
