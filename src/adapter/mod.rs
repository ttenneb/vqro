//! Backend adapters (D14, D15). An adapter maps one backend onto prim resources and declares what it
//! can do. It translates protocols honestly and never invents guarantees the backend lacks.
//!
//! Adapters run as tasks that talk to the core through channels, so the core never blocks on a
//! backend and a fake adapter can stand in as a deterministic test fixture.

pub mod fake;
pub mod herdr;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};
use crate::provider::ProviderInfo;

/// Startup selection only. Execution-specific setup stays in the adapter layer, not the server.
pub enum Backend { Herdr(Vec<String>), Fake }
impl Backend {
    pub fn start(self, events: mpsc::UnboundedSender<(String, AdapterEvent)>) -> AdapterHandle {
        match self {
            Self::Herdr(argv) => herdr::start("herdr".into(), argv, events),
            Self::Fake => fake::start("fake".into(), events),
        }
    }
}

#[derive(Debug)]
pub enum AdapterError { Rejected(String), Backend(String), Indeterminate(String) }
impl From<String> for AdapterError { fn from(message: String) -> Self { Self::Rejected(message) } }
impl From<&str> for AdapterError { fn from(message: &str) -> Self { Self::Rejected(message.into()) } }
impl std::fmt::Display for AdapterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self { Self::Rejected(s) | Self::Backend(s) | Self::Indeterminate(s) => s.fmt(f) }
    }
}
impl AdapterError {
    pub fn rpc(self) -> crate::rpc::RpcError {
        match self {
            Self::Rejected(s) => crate::provider::rejected("backend", s),
            Self::Backend(s) => {
                let mut error = crate::rpc::RpcError::new("backend", s);
                error.data.as_mut().unwrap()["outcome"] = serde_json::json!("indeterminate");
                error
            }
            Self::Indeterminate(s) => crate::provider::indeterminate(s),
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
            && self.data.is_object() && self.data.to_string().len() <= 64 * 1024
    }
}

fn empty_data() -> Value { serde_json::json!({}) }

pub enum AdapterEvent {
    Registered(ProviderInfo),
    /// Full current list; resources not in it are gone. Sent on connect and after a resync.
    Snapshot(Vec<ResourceFacts>),
    Upsert(ResourceFacts),
    Removed { reference: String, incarnation: String },
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
    /// The stream ended, with the backend's reason (JSON).
    Closed(Value),
}

pub struct Stream {
    pub initial: &'static str,
    pub input: crate::queue::Sender<StreamInput>,
    pub output: crate::queue::Receiver<StreamOutput>,
}

pub struct SpawnRequest {
    pub argv: Vec<String>,
    pub cwd: Option<String>,
    pub name: Option<String>,
}

type Reply<T> = oneshot::Sender<Result<T, AdapterError>>;

pub enum AdapterCmd {
    /// A core route binds queued work to the native connection that accepted the registration.
    Bound { connection: u64, command: Box<AdapterCmd> },
    Spawn(SpawnRequest, Reply<ResourceFacts>),
    Kill(String, Reply<Value>),
    Attach { backend_ref: String, cols: u16, rows: u16, history: u32, reply: Reply<Stream> },
}

pub(crate) fn reject_command(command: AdapterCmd, why: &str) {
    match command {
        AdapterCmd::Bound { command, .. } => reject_command(*command, why),
        AdapterCmd::Spawn(_, reply) => { let _ = reply.send(Err(AdapterError::Rejected(why.into()))); }
        AdapterCmd::Kill(_, reply) => { let _ = reply.send(Err(AdapterError::Rejected(why.into()))); }
        AdapterCmd::Attach { reply, .. } => { let _ = reply.send(Err(AdapterError::Rejected(why.into()))); }
    }
}

pub fn command_for(mut command: AdapterCmd, current: u64) -> Option<AdapterCmd> {
    loop {
        match command {
            AdapterCmd::Bound { connection, command: inner } => {
                if connection != current { reject_command(*inner, "provider connection changed before dispatch"); return None; }
                command = *inner;
            }
            native => return Some(native),
        }
    }
}

/// The core's handle on a running adapter.
#[derive(Clone)]
pub struct AdapterHandle {
    pub name: String,
    pub capabilities: Value,
    pub session: Option<u64>,
    pub cmds: mpsc::UnboundedSender<AdapterCmd>,
}

impl AdapterHandle {
    fn bind(&self, command: AdapterCmd) -> AdapterCmd {
        match self.session {
            Some(connection) => AdapterCmd::Bound { connection, command: Box::new(command) },
            None => command, // Direct adapter fixture calls; core routes are always bound.
        }
    }

    pub async fn spawn(&self, req: SpawnRequest) -> Result<ResourceFacts, AdapterError> {
        let (tx, rx) = oneshot::channel();
        self.cmds.send(self.bind(AdapterCmd::Spawn(req, tx))).map_err(|_| AdapterError::Rejected("provider stopped before dispatch".into()))?;
        rx.await.map_err(|_| AdapterError::Indeterminate("provider stopped before replying".into()))?
    }
    pub async fn kill(&self, backend_ref: String) -> Result<Value, AdapterError> {
        let (tx, rx) = oneshot::channel();
        self.cmds.send(self.bind(AdapterCmd::Kill(backend_ref, tx))).map_err(|_| AdapterError::Rejected("provider stopped before dispatch".into()))?;
        rx.await.map_err(|_| AdapterError::Indeterminate("provider stopped before replying".into()))?
    }
    pub async fn attach(&self, backend_ref: String, cols: u16, rows: u16, history: u32) -> Result<Stream, AdapterError> {
        let (tx, rx) = oneshot::channel();
        self.cmds.send(self.bind(AdapterCmd::Attach { backend_ref, cols, rows, history, reply: tx })).map_err(|_| AdapterError::Rejected("provider stopped before dispatch".into()))?;
        rx.await.map_err(|_| AdapterError::Indeterminate("provider stopped before replying".into()))?
    }
}
