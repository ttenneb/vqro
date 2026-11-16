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

/// What a backend says about one of its resources. Observed, not authoritative (D6).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ResourceFacts {
    /// The backend's own identifier, e.g. herdr-core's `t3`.
    pub backend_ref: String,
    /// Open set, declared by the adapter: "terminal", later "agent", ...
    pub kind: String,
    pub name: Option<String>,
    pub title: Option<String>,
    /// "live" or "finished".
    pub state: String,
    /// Program status as reported (OSC 7501 via the backend), or null.
    pub status: Value,
    /// Exit details when finished, as reported.
    pub exit: Value,
    pub argv: Vec<String>,
    /// Where the program is working now (the backend's report, e.g. OSC 7 or process cwd), if known.
    #[serde(default)]
    pub cwd: Option<String>,
}

pub enum AdapterEvent {
    /// Full current list; resources not in it are gone. Sent on connect and after a resync.
    Snapshot(Vec<ResourceFacts>),
    Upsert(ResourceFacts),
    Removed(String),
    /// The adapter lost or regained its backend.
    Connected(bool, Option<String>),
}

pub enum StreamInput {
    Data(Vec<u8>),
    Resize(u16, u16),
    Close,
}

pub enum StreamOutput {
    Data(Vec<u8>),
    /// The stream ended, with the backend's reason (JSON).
    Closed(Value),
}

pub struct Stream {
    pub input: mpsc::UnboundedSender<StreamInput>,
    pub output: mpsc::UnboundedReceiver<StreamOutput>,
}

pub struct SpawnRequest {
    pub argv: Vec<String>,
    pub cwd: Option<String>,
    pub name: Option<String>,
}

type Reply<T> = oneshot::Sender<Result<T, String>>;

pub enum AdapterCmd {
    Spawn(SpawnRequest, Reply<ResourceFacts>),
    Kill(String, Reply<Value>),
    Attach { backend_ref: String, cols: u16, rows: u16, history: u32, reply: Reply<Stream> },
}

/// The core's handle on a running adapter.
#[derive(Clone)]
pub struct AdapterHandle {
    pub name: String,
    pub capabilities: Value,
    pub cmds: mpsc::UnboundedSender<AdapterCmd>,
}

impl AdapterHandle {
    pub async fn spawn(&self, req: SpawnRequest) -> Result<ResourceFacts, String> {
        let (tx, rx) = oneshot::channel();
        self.cmds.send(AdapterCmd::Spawn(req, tx)).map_err(|_| "adapter stopped".to_string())?;
        rx.await.map_err(|_| "adapter stopped".to_string())?
    }
    pub async fn kill(&self, backend_ref: String) -> Result<Value, String> {
        let (tx, rx) = oneshot::channel();
        self.cmds.send(AdapterCmd::Kill(backend_ref, tx)).map_err(|_| "adapter stopped".to_string())?;
        rx.await.map_err(|_| "adapter stopped".to_string())?
    }
    pub async fn attach(&self, backend_ref: String, cols: u16, rows: u16, history: u32) -> Result<Stream, String> {
        let (tx, rx) = oneshot::channel();
        self.cmds.send(AdapterCmd::Attach { backend_ref, cols, rows, history, reply: tx }).map_err(|_| "adapter stopped".to_string())?;
        rx.await.map_err(|_| "adapter stopped".to_string())?
    }
}
