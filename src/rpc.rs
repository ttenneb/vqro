//! JSON-RPC 2.0, one message per line, in both directions on one connection.
//!
//! Every prim connection (client, extension, CLI) is symmetric: either side may send requests and
//! notifications. The core uses this to forward `command.invoke` to the extension that owns a command.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot};

/// An error with a stable `kind` (in `data.kind`), as herdr-core does.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl RpcError {
    pub fn new(kind: &str, message: impl Into<String>) -> Self {
        RpcError { code: -32000, message: message.into(), data: Some(json!({ "kind": kind })) }
    }
    pub fn kind(&self) -> &str {
        self.data.as_ref().and_then(|d| d.get("kind")).and_then(|k| k.as_str()).unwrap_or("error")
    }
    pub fn method_not_found(method: &str) -> Self {
        RpcError { code: -32601, message: format!("unknown method {method}"), data: Some(json!({"kind": "unknown_method"})) }
    }
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new("invalid", message)
    }
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.message, self.kind())
    }
}

impl std::error::Error for RpcError {}

pub type RpcResult = Result<Value, RpcError>;

/// Something the other side sent us.
#[derive(Debug)]
pub enum Incoming {
    Request { id: Value, method: String, params: Value },
    Notification { method: String, params: Value },
}

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<RpcResult>>>>;

/// One side of a connection. Cheap to clone.
#[derive(Clone)]
pub struct Peer {
    out: mpsc::UnboundedSender<String>,
    pending: Pending,
    next_id: Arc<AtomicU64>,
}

impl Peer {
    /// Start reader and writer tasks. Incoming requests and notifications arrive on the receiver;
    /// it closes when the connection ends.
    pub fn spawn<R, W>(reader: R, writer: W) -> (Peer, mpsc::UnboundedReceiver<Incoming>)
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let (out_tx, mut out_rx) = mpsc::unbounded_channel::<String>();
        let (in_tx, in_rx) = mpsc::unbounded_channel();
        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));

        tokio::spawn(async move {
            let mut writer = writer;
            while let Some(line) = out_rx.recv().await {
                if writer.write_all(line.as_bytes()).await.is_err() || writer.write_all(b"\n").await.is_err() {
                    break;
                }
                if writer.flush().await.is_err() {
                    break;
                }
            }
        });

        let pend = pending.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(reader).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if line.trim().is_empty() {
                    continue;
                }
                let Ok(msg) = serde_json::from_str::<Value>(&line) else { continue };
                if let Some(method) = msg.get("method").and_then(|m| m.as_str()) {
                    let params = msg.get("params").cloned().unwrap_or(Value::Null);
                    let incoming = match msg.get("id") {
                        Some(id) if !id.is_null() => Incoming::Request { id: id.clone(), method: method.to_string(), params },
                        _ => Incoming::Notification { method: method.to_string(), params },
                    };
                    if in_tx.send(incoming).is_err() {
                        break;
                    }
                } else if let Some(id) = msg.get("id").and_then(|i| i.as_u64()) {
                    let result = match msg.get("error") {
                        Some(e) => Err(serde_json::from_value(e.clone())
                            .unwrap_or_else(|_| RpcError::new("error", e.to_string()))),
                        None => Ok(msg.get("result").cloned().unwrap_or(Value::Null)),
                    };
                    if let Some(tx) = pend.lock().unwrap().remove(&id) {
                        let _ = tx.send(result);
                    }
                }
            }
            // Connection over: fail everything still waiting.
            for (_, tx) in pend.lock().unwrap().drain() {
                let _ = tx.send(Err(RpcError::new("disconnected", "connection closed")));
            }
        });

        (Peer { out: out_tx, pending, next_id: Arc::new(AtomicU64::new(1)) }, in_rx)
    }

    pub async fn call(&self, method: &str, params: Value) -> RpcResult {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        let msg = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        if self.out.send(msg.to_string()).is_err() {
            self.pending.lock().unwrap().remove(&id);
            return Err(RpcError::new("disconnected", "connection closed"));
        }
        rx.await.unwrap_or_else(|_| Err(RpcError::new("disconnected", "connection closed")))
    }

    pub fn notify(&self, method: &str, params: Value) {
        let _ = self.out.send(json!({"jsonrpc": "2.0", "method": method, "params": params}).to_string());
    }

    pub fn respond(&self, id: Value, result: RpcResult) {
        let msg = match result {
            Ok(v) => json!({"jsonrpc": "2.0", "id": id, "result": v}),
            Err(e) => json!({"jsonrpc": "2.0", "id": id, "error": e}),
        };
        let _ = self.out.send(msg.to_string());
    }

    pub fn is_closed(&self) -> bool {
        self.out.is_closed()
    }
}

/// Parse params into a type, as an `invalid` error on failure.
pub fn params<T: serde::de::DeserializeOwned>(v: Value) -> Result<T, RpcError> {
    let v = if v.is_null() { json!({}) } else { v };
    serde_json::from_value(v).map_err(|e| RpcError::invalid(format!("bad params: {e}")))
}
