//! JSON-RPC 2.0, one message per line, in both directions on one connection.
//!
//! Every prim connection (client, extension, CLI) is symmetric: either side may send requests and
//! notifications. The core uses this to forward `command.invoke` to the extension that owns a command.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot, watch};
use std::time::Duration;
use crate::queue;

pub const OUTBOUND_BYTES: usize = 5 * 1024 * 1024;
pub const STREAM_OUTBOUND_BYTES: usize = 1024 * 1024;
pub const DEFAULT_CALL_TIMEOUT: Duration = Duration::from_secs(60);

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
    control: queue::Sender<Outbound>,
    data: queue::Sender<Outbound>,
    shutdown: watch::Sender<bool>,
    pending: Pending,
    next_id: Arc<AtomicU64>,
}

struct Outbound {
    line: String,
    active: Option<Arc<AtomicBool>>,
}

/// A per-stream cancellation flag lets the writer discard bounded queued data after a gap,
/// without ever splicing or replaying a partially written JSON line.
pub struct StreamWriter {
    peer: Peer,
    id: u64,
    active: Arc<AtomicBool>,
}

impl StreamWriter {
    pub fn send(&self, data: String) -> bool {
        if self.peer.enqueue_stream(self.id, data, self.active.clone()) { true }
        else { self.active.store(false, Ordering::Relaxed); false }
    }

    pub fn close(&self, reason: Value) {
        self.active.store(false, Ordering::Relaxed);
        self.peer.notify("stream.closed", json!({"stream": self.id, "reason": reason}));
    }

    /// Normal backend closure follows all accepted bytes in the data queue. If even that marker
    /// cannot fit, report a gap instead of silently losing the final output.
    pub fn finish(&self, reason: Value) {
        let line = json!({"jsonrpc": "2.0", "method": "stream.closed", "params": {"stream": self.id, "reason": reason}}).to_string();
        let bytes = line.len() + 1;
        if self.peer.data.try_send(Outbound { line, active: Some(self.active.clone()) }, bytes).is_err() {
            self.close(json!({"reason": "slow_consumer"}));
        }
    }
}

// Cancellation, timeout and late replies must not leave an ever-growing pending-call map.
struct PendingCall { id: u64, pending: Pending }
impl Drop for PendingCall {
    fn drop(&mut self) { self.pending.lock().unwrap().remove(&self.id); }
}

impl Peer {
    /// Start reader and writer tasks. Incoming requests and notifications arrive on the receiver;
    /// it closes when the connection ends.
    pub fn spawn<R, W>(reader: R, writer: W) -> (Peer, mpsc::UnboundedReceiver<Incoming>)
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let (control, mut control_rx) = queue::channel::<Outbound>(OUTBOUND_BYTES - STREAM_OUTBOUND_BYTES);
        let (data, mut data_rx) = queue::channel::<Outbound>(STREAM_OUTBOUND_BYTES);
        let (in_tx, in_rx) = mpsc::unbounded_channel();
        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let (shutdown, _) = watch::channel(false);
        let (stop, mut stopped) = (shutdown.clone(), shutdown.subscribe());
        tokio::spawn(async move {
            let write = async {
                let mut writer = writer;
                loop {
                    let item = tokio::select! {
                        biased;
                        item = control_rx.recv() => item,
                        item = data_rx.recv() => item,
                    };
                    let Some(item) = item else { break };
                    if item.value.active.as_ref().is_some_and(|a| !a.load(Ordering::Relaxed)) { continue; }
                    // Keep byte permits charged through the actual socket write/flush. A line
                    // already in flight must finish before control can overtake queued data.
                    writer.write_all(item.value.line.as_bytes()).await?;
                    writer.write_all(b"\n").await?;
                    writer.flush().await?;
                }
                Ok::<(), std::io::Error>(())
            };
            tokio::select! { _ = write => {}, _ = stopped.changed() => {} }
            stop.send_replace(true);
        });

        let pend = pending.clone();
        let (stop, mut stopped) = (shutdown.clone(), shutdown.subscribe());
        tokio::spawn(async move {
            let mut lines = BufReader::new(reader).lines();
            loop {
                let line = tokio::select! {
                    line = lines.next_line() => match line { Ok(Some(line)) => line, _ => break },
                    _ = stopped.changed() => break,
                };
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
            stop.send_replace(true);
            // Connection over: fail everything still waiting.
            for (_, tx) in pend.lock().unwrap().drain() {
                let _ = tx.send(Err(RpcError::new("disconnected", "connection closed")));
            }
        });

        (Peer { control, data, shutdown, pending, next_id: Arc::new(AtomicU64::new(1)) }, in_rx)
    }

    pub async fn call(&self, method: &str, params: Value) -> RpcResult {
        self.call_with_timeout(method, params, DEFAULT_CALL_TIMEOUT).await
    }

    pub async fn call_with_timeout(&self, method: &str, params: Value, deadline: Duration) -> RpcResult {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        let _guard = PendingCall { id, pending: self.pending.clone() };
        let msg = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        if !self.send_control(msg.to_string()) {
            return Err(RpcError::new("disconnected", "connection closed or outbound control queue full"));
        }
        match tokio::time::timeout(deadline, rx).await {
            Ok(result) => result.unwrap_or_else(|_| Err(RpcError::new("disconnected", "connection closed"))),
            Err(_) => {
                let mut error = RpcError::new("timeout", format!("{method} deadline expired; outcome indeterminate, do not resend automatically"));
                error.data.as_mut().unwrap()["outcome"] = json!("indeterminate");
                Err(error)
            }
        }
    }

    fn send_control(&self, line: String) -> bool {
        let bytes = line.len() + 1;
        if self.is_closed() || self.control.try_send(Outbound { line, active: None }, bytes).is_err() {
            // No room even for recovery/control: disconnect rather than silently dropping replies
            // or events. Stream data has a smaller budget, leaving room for explicit stream closure.
            self.shutdown.send_replace(true);
            false
        } else { true }
    }

    pub fn stream_writer(&self, id: u64) -> StreamWriter {
        StreamWriter { peer: self.clone(), id, active: Arc::new(AtomicBool::new(true)) }
    }

    fn enqueue_stream(&self, stream: u64, data: String, active: Arc<AtomicBool>) -> bool {
        let line = json!({"jsonrpc": "2.0", "method": "stream.data", "params": {"stream": stream, "data": data}}).to_string();
        let bytes = line.len() + 1;
        if self.is_closed() || bytes > STREAM_OUTBOUND_BYTES { return false; }
        if !active.load(Ordering::Relaxed) { return false; }
        self.data.try_send(Outbound { line, active: Some(active) }, bytes).is_ok()
    }

    pub fn outbound_bytes(&self) -> usize { self.control.used_bytes() + self.data.used_bytes() }

    pub fn notify(&self, method: &str, params: Value) {
        self.send_control(json!({"jsonrpc": "2.0", "method": method, "params": params}).to_string());
    }

    pub fn respond(&self, id: Value, result: RpcResult) {
        let msg = match result {
            Ok(v) => json!({"jsonrpc": "2.0", "id": id, "result": v}),
            Err(e) => json!({"jsonrpc": "2.0", "id": id, "error": e}),
        };
        self.send_control(msg.to_string());
    }

    pub fn is_closed(&self) -> bool {
        *self.shutdown.borrow() || self.control.is_closed() || self.data.is_closed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncBufReadExt;

    #[tokio::test]
    async fn stalled_writer_is_byte_bounded_and_keeps_room_for_explicit_close() {
        let (socket, client) = tokio::io::duplex(64);
        let (r, w) = tokio::io::split(socket);
        let (peer, _incoming) = Peer::spawn(r, w);
        let data = "x".repeat(64 * 1024);
        let mut sent = 0;
        let stream = peer.stream_writer(7);
        while stream.send(data.clone()) {
            sent += 1;
            assert!(peer.outbound_bytes() <= STREAM_OUTBOUND_BYTES);
        }
        assert!(sent > 0 && sent < 20);
        stream.close(json!({"reason": "slow_consumer"}));
        assert!(!peer.is_closed(), "data overflow leaves control capacity");
        assert!(peer.outbound_bytes() <= OUTBOUND_BYTES);
        let mut lines = BufReader::new(client).lines();
        for _ in 0..=sent {
            let line = tokio::time::timeout(Duration::from_secs(2), lines.next_line()).await.unwrap().unwrap().unwrap();
            let msg: Value = serde_json::from_str(&line).unwrap();
            if msg["method"] == "stream.closed" {
                assert_eq!(msg["params"]["reason"]["reason"], "slow_consumer");
                return;
            }
        }
        panic!("explicit close was lost behind the stalled writer");
    }

    #[tokio::test]
    async fn normal_close_preserves_final_output_order() {
        let (socket, client) = tokio::io::duplex(64);
        let (r, w) = tokio::io::split(socket);
        let (peer, _incoming) = Peer::spawn(r, w);
        let stream = peer.stream_writer(3);
        assert!(stream.send("YWJj".into()));
        stream.finish(json!({"reason": "finished"}));
        let mut lines = BufReader::new(client).lines();
        let first: Value = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        let last: Value = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        assert_eq!(first["method"], "stream.data");
        assert_eq!(first["params"]["data"], "YWJj");
        assert_eq!(last["method"], "stream.closed");
        assert_eq!(last["params"]["reason"]["reason"], "finished");
    }

    #[tokio::test]
    async fn timed_out_and_cancelled_calls_remove_pending_entries_and_ignore_late_replies() {
        let (a, b) = tokio::io::duplex(4096);
        let (r, w) = tokio::io::split(a);
        let (peer, _) = Peer::spawn(r, w);
        let (r, w) = tokio::io::split(b);
        let (other, mut incoming) = Peer::spawn(r, w);
        let result = peer.call_with_timeout("hang", Value::Null, Duration::from_millis(10)).await.unwrap_err();
        assert_eq!(result.kind(), "timeout");
        assert_eq!(result.data.unwrap()["outcome"], "indeterminate");
        assert!(peer.pending.lock().unwrap().is_empty());
        let Some(Incoming::Request { id, .. }) = incoming.recv().await else { panic!("missing request") };
        other.respond(id, Ok(json!("late")));
        let task = tokio::spawn({ let p = peer.clone(); async move { p.call("cancel", Value::Null).await } });
        let Some(Incoming::Request { .. }) = incoming.recv().await else { panic!("missing request") };
        task.abort();
        let _ = task.await;
        assert!(peer.pending.lock().unwrap().is_empty());
    }
}

/// Parse params into a type, as an `invalid` error on failure.
pub fn params<T: serde::de::DeserializeOwned>(v: Value) -> Result<T, RpcError> {
    let v = if v.is_null() { json!({}) } else { v };
    serde_json::from_value(v).map_err(|e| RpcError::invalid(format!("bad params: {e}")))
}
