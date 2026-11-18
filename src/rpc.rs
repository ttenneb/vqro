//! JSON-RPC 2.0, one message per line, in both directions on one connection.
//!
//! Every vqro connection (client, extension, CLI) is symmetric: either side may send requests and
//! notifications. The core uses this to forward `command.invoke` to the extension that owns a command.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot, watch, OwnedSemaphorePermit, Semaphore};
use std::time::Duration;
use crate::queue;

pub const OUTBOUND_BYTES: usize = 5 * 1024 * 1024;
pub const STREAM_OUTBOUND_BYTES: usize = 1024 * 1024;
pub const CONTROL_BYTES: usize = OUTBOUND_BYTES - STREAM_OUTBOUND_BYTES;
pub const INBOUND_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_PENDING_CALLS: usize = 128;
/// Recovery envelope: 16 MiB entries + 8 MiB live + 32 MiB resource allowance + 8 MiB metadata.
/// Larger legacy/external state gets an explicit too_large reply, never a transport disconnect.
pub const LARGE_REPLY_BYTES: usize = crate::limits::SNAPSHOT_BYTES;
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

struct PendingReply { result: RpcResult, _permit: Option<OwnedSemaphorePermit> }
type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<PendingReply>>>>;

/// Byte-bounded application ingress. Dequeue releases its charge; at most one additional record
/// may be in the consumer's hands. Overflow closes the connection rather than dropping messages.
pub struct IncomingReceiver { rx: queue::Receiver<Incoming>, limit: Arc<AtomicUsize>, shutdown: watch::Sender<bool>, closed: bool }
impl IncomingReceiver {
    fn accept(&mut self, item: &queue::Item<Incoming>) -> bool {
        // Registration can tighten a limit after the reader already queued a complete record.
        // Recheck its original byte charge, not reserialized params (which lose whitespace).
        if item.payload_bytes() > self.limit.load(Ordering::Acquire) {
            self.closed = true; self.shutdown.send_replace(true); false
        } else { true }
    }
    pub async fn recv(&mut self) -> Option<Incoming> {
        if self.closed { return None; }
        let item = self.rx.recv().await?;
        self.accept(&item).then_some(item.value)
    }
    pub fn try_recv(&mut self) -> Result<Incoming, mpsc::error::TryRecvError> {
        if self.closed { return Err(mpsc::error::TryRecvError::Disconnected); }
        let item = self.rx.try_recv()?;
        if self.accept(&item) { Ok(item.value) } else { Err(mpsc::error::TryRecvError::Disconnected) }
    }
    pub fn len(&self) -> usize { self.rx.len() }
    pub fn is_empty(&self) -> bool { self.rx.is_empty() }
    pub fn queued_bytes(&self) -> usize { self.rx.used_bytes() }
}

/// One side of a connection. Cheap to clone.
#[derive(Clone)]
pub struct Peer {
    control: queue::Sender<Outbound>,
    data: queue::Sender<Outbound>,
    large: queue::Sender<Outbound>,
    large_bytes: Arc<AtomicUsize>,
    record_limit: Arc<AtomicUsize>,
    read_ended: Arc<AtomicBool>,
    shutdown: watch::Sender<bool>,
    pending: Pending,
    next_id: Arc<AtomicU64>,
}

struct Outbound {
    line: String,
    active: Option<Arc<AtomicBool>>,
    _large: Option<LargeCharge>,
    flushed: Option<oneshot::Sender<()>>,
}
struct LargeCharge { bytes: usize, used: Arc<AtomicUsize> }
impl Drop for LargeCharge { fn drop(&mut self) { self.used.fetch_sub(self.bytes, Ordering::AcqRel); } }

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
        self.finish_as("stream.closed", json!({"reason": reason}));
    }

    /// An ordered end/gap marker follows all accepted bytes, unlike control-priority notify.
    pub fn finish_as(&self, method: &str, params: Value) {
        let mut params = params.as_object().cloned().unwrap_or_default();
        params.insert("stream".into(), json!(self.id));
        let line = json!({"jsonrpc": "2.0", "method": method, "params": params}).to_string();
        let bytes = line.len() + 1;
        if self.peer.data.try_send(Outbound { line, active: Some(self.active.clone()), _large: None, flushed: None }, bytes).is_err() {
            self.close(json!({"reason": "slow_consumer"}));
        }
    }
}

// Cancellation, timeout and late replies must not leave an ever-growing pending-call map.
struct PendingCall { id: u64, pending: Pending }
impl Drop for PendingCall {
    fn drop(&mut self) { self.pending.lock().unwrap().remove(&self.id); }
}

async fn read_record<R: AsyncBufRead + Unpin>(reader: &mut R, limit: &AtomicUsize) -> std::io::Result<Option<Vec<u8>>> {
    let mut line = Vec::new();
    loop {
        let max = limit.load(Ordering::Acquire);
        if line.len() > max { return Err(std::io::Error::other("record exceeds framing limit")); }
        let buf = reader.fill_buf().await?;
        if buf.is_empty() { return Ok(if line.is_empty() { None } else { Some(line) }); }
        let end = buf.iter().position(|b| *b == b'\n').map(|i| i + 1);
        let count = end.unwrap_or(buf.len()).min(max.saturating_sub(line.len()) + 1);
        line.extend_from_slice(&buf[..count]);
        reader.consume(count);
        if line.len() > limit.load(Ordering::Acquire) { return Err(std::io::Error::other("record exceeds framing limit")); }
        if end == Some(count) { return Ok(Some(line)); }
    }
}

impl Peer {
    /// Start reader and writer tasks. Incoming requests and notifications arrive on the receiver;
    /// it closes when the connection ends.
    pub fn spawn<R, W>(reader: R, writer: W) -> (Peer, IncomingReceiver)
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        Self::spawn_transport(reader,writer,false)
    }

    /// A serving application drains already-read requests and explicitly closes after flushing replies.
    /// Clean read EOF alone does not revoke its still-writable response half.
    pub(crate) fn spawn_serving<R,W>(reader:R,writer:W)->(Peer,IncomingReceiver)
    where R:AsyncRead+Unpin+Send+'static,W:AsyncWrite+Unpin+Send+'static {
        Self::spawn_transport(reader,writer,true)
    }

    fn spawn_transport<R,W>(reader:R,writer:W,drain_read_eof:bool)->(Peer,IncomingReceiver)
    where R:AsyncRead+Unpin+Send+'static,W:AsyncWrite+Unpin+Send+'static {
        let (control, mut control_rx) = queue::channel::<Outbound>(OUTBOUND_BYTES - STREAM_OUTBOUND_BYTES);
        let (data, mut data_rx) = queue::channel::<Outbound>(STREAM_OUTBOUND_BYTES);
        // The large slot charges one item, with actual bytes tracked separately through writing.
        let (large, mut large_rx) = queue::channel::<Outbound>(64);
        let large_bytes = Arc::new(AtomicUsize::new(0));
        let record_limit = Arc::new(AtomicUsize::new(LARGE_REPLY_BYTES));
        let read_ended=Arc::new(AtomicBool::new(false));
        let (in_tx, in_rx) = queue::channel::<Incoming>(INBOUND_BYTES);
        let response_budget = Arc::new(Semaphore::new(INBOUND_BYTES));
        let large_response = Arc::new(Semaphore::new(1));
        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let (shutdown, _) = watch::channel(false);
        let (stop, mut stopped) = (shutdown.clone(), shutdown.subscribe());
        tokio::spawn(async move {
            let write = async {
                let mut writer = writer;
                let (mut large_done, mut control_done, mut data_done) = (false, false, false);
                loop {
                    let item = tokio::select! {
                        biased;
                        item = large_rx.recv(), if !large_done => { if item.is_none() { large_done = true; continue; } item },
                        item = control_rx.recv(), if !control_done => { if item.is_none() { control_done = true; continue; } item },
                        item = data_rx.recv(), if !data_done => { if item.is_none() { data_done = true; continue; } item },
                        else => break,
                    };
                    let item = item.unwrap();
                    // Local-only FIFO barrier: never emit an empty JSON record on the wire.
                    if item.value.line.is_empty() {
                        if let Some(done)=item.value.flushed { let _=done.send(()); }
                        continue;
                    }
                    if item.value.active.as_ref().is_some_and(|a| !a.load(Ordering::Relaxed)) { continue; }
                    // Keep byte permits charged through the actual socket write/flush. A line
                    // already in flight must finish before control can overtake queued data.
                    writer.write_all(item.value.line.as_bytes()).await?;
                    writer.write_all(b"\n").await?;
                    writer.flush().await?;
                    if let Some(done) = item.value.flushed { let _ = done.send(()); }
                }
                Ok::<(), std::io::Error>(())
            };
            tokio::select! { _ = write => {}, _ = stopped.changed() => {} }
            stop.send_replace(true);
        });

        let pend = pending.clone();
        let limit = record_limit.clone();
        let ended=read_ended.clone();
        let (stop, mut stopped) = (shutdown.clone(), shutdown.subscribe());
        tokio::spawn(async move {
            let mut reader = BufReader::new(reader);
            let mut clean_eof=false;
            loop {
                let line = tokio::select! {
                    line = read_record(&mut reader, &limit) => match line { Ok(Some(line)) => line, Ok(None)=>{clean_eof=true;break}, Err(_)=>break },
                    _ = stopped.changed() => break,
                };
                let bytes = line.len();
                if bytes > limit.load(Ordering::Acquire) { break; }
                let Ok(mut msg) = serde_json::from_slice::<Value>(&line) else { continue };
                if let Some(method) = msg.get("method").and_then(Value::as_str).map(str::to_owned) {
                    if bytes > INBOUND_BYTES { break; }
                    let params = msg.get_mut("params").map(std::mem::take).unwrap_or(Value::Null);
                    let incoming = match msg.get_mut("id").map(std::mem::take) {
                        Some(id) if !id.is_null() => Incoming::Request { id, method, params },
                        _ => Incoming::Notification { method, params },
                    };
                    if in_tx.try_send(incoming, bytes).is_err() { break; }
                } else if let Some(id) = msg.get("id").and_then(Value::as_u64) {
                    // Leave envelope headroom when opaque provider/extension replies are audited
                    // or forwarded; a malformed peer does not get to poison retained events.
                    if crate::limits::json_depth(&msg,crate::limits::MESSAGE_DEPTH).is_err() {break;}
                    let Some(tx) = pend.lock().unwrap().remove(&id) else { continue };
                    let permit = if bytes > INBOUND_BYTES {
                        large_response.clone().try_acquire_owned()
                    } else { response_budget.clone().try_acquire_many_owned(bytes.max(64) as u32) };
                    let Ok(permit) = permit else { break; };
                    let result = match msg.get_mut("error").map(std::mem::take) {
                        Some(e) => Err(serde_json::from_value(e.clone()).unwrap_or_else(|_| RpcError::new("error", e.to_string()))),
                        None => Ok(msg.get_mut("result").map(std::mem::take).unwrap_or(Value::Null)),
                    };
                    let _ = tx.send(PendingReply { result, _permit: Some(permit) });
                }
            }
            ended.store(true,Ordering::Release);
            if !drain_read_eof || !clean_eof {stop.send_replace(true);}
            // No further response can arrive, even if the serving side can still send replies.
            for (_, tx) in pend.lock().unwrap().drain() {
                let _ = tx.send(PendingReply { result: Err(RpcError::new("disconnected", "connection closed")), _permit: None });
            }
        });

        let incoming = IncomingReceiver { rx: in_rx, limit: record_limit.clone(), shutdown: shutdown.clone(), closed: false };
        (Peer { control, data, large, large_bytes, record_limit, read_ended, shutdown, pending, next_id: Arc::new(AtomicU64::new(1)) }, incoming)
    }

    pub async fn call(&self, method: &str, params: Value) -> RpcResult {
        self.call_with_timeout(method, params, DEFAULT_CALL_TIMEOUT).await
    }

    pub async fn call_with_timeout(&self, method: &str, params: Value, deadline: Duration) -> RpcResult {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        {
            let mut pending = self.pending.lock().unwrap();
            if self.read_ended.load(Ordering::Acquire) {return Err(RpcError::new("disconnected","request input ended; peer cannot answer a new call"));}
            if pending.len() >= MAX_PENDING_CALLS { return Err(RpcError::new("overloaded", "pending call limit; request not sent")); }
            pending.insert(id, tx);
        }
        let _guard = PendingCall { id, pending: self.pending.clone() };
        let msg = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        crate::limits::json_depth(&msg,crate::limits::MESSAGE_DEPTH)?;
        let line = msg.to_string();
        if line.len() + 1 > INBOUND_BYTES.min(self.record_limit.load(Ordering::Acquire)) {
            return Err(RpcError::new("too_large", "request exceeds record limit; not sent"));
        }
        if !self.send_control(line) {
            return Err(RpcError::new("disconnected", "connection closed or outbound control queue full"));
        }
        with_deadline(method, deadline, async {
            rx.await.map(|reply| reply.result).unwrap_or_else(|_| Err(RpcError::new("disconnected", "connection closed")))
        }).await
    }

    fn send_control(&self, line: String) -> bool {
        let bytes = line.len() + 1;
        if self.is_closed() || bytes > self.record_limit.load(Ordering::Acquire) || self.control.try_send(Outbound { line, active: None, _large: None, flushed: None }, bytes).is_err() {
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
        self.data.try_send(Outbound { line, active: Some(active), _large: None, flushed: None }, bytes).is_ok()
    }

    pub fn outbound_bytes(&self) -> usize { self.control.used_bytes() + self.data.used_bytes() + self.large_bytes.load(Ordering::Acquire) }

    pub fn close(&self) { self.shutdown.send_replace(true); }
    pub async fn closed(&self) {
        let mut stopped = self.shutdown.subscribe();
        if !*stopped.borrow() { let _ = stopped.changed().await; }
    }
    /// Framing cap, including replies; lowering it also affects a record currently being read.
    pub fn set_max_record_bytes(&self, bytes: usize) -> Result<(), RpcError> {
        if bytes == 0 || bytes > LARGE_REPLY_BYTES { return Err(RpcError::invalid("record limit outside transport envelope")); }
        self.record_limit.store(bytes, Ordering::Release);
        Ok(())
    }

    pub fn notify(&self, method: &str, params: Value) {
        if crate::limits::json_depth(&params,crate::limits::MESSAGE_DEPTH-1).is_err() {self.close();return;}
        self.send_control(json!({"jsonrpc": "2.0", "method": method, "params": params}).to_string());
    }

    /// Barrier behind already queued control replies (large replies have higher priority).
    /// This confirms local transport flushing, not that the remote application read a reply.
    pub async fn flush_control(&self) -> bool {
        if self.is_closed() { return false; }
        let (done,flushed)=oneshot::channel();
        if self.control.send(Outbound { line:String::new(),active:None,_large:None,flushed:Some(done) },64).await.is_err() { return false; }
        flushed.await.is_ok()
    }

    /// A small reply with a completion barrier, used before the caller can tear down this peer.
    /// Completion means local write/flush only, not that the other process read it.
    pub async fn respond_flushed(&self, id: Value, result: RpcResult) -> Result<(), RpcError> {
        let message=Self::reply_message(&id,result);
        let line = serde_json::to_string(&message).map_err(|_| RpcError::invalid("reply encoding failed"))?;
        let bytes = line.len() + 1;
        let (done, flushed) = oneshot::channel();
        if self.is_closed() || bytes > self.record_limit.load(Ordering::Acquire) || self.control.try_send(Outbound { line, active: None, _large: None, flushed: Some(done) }, bytes).is_err() {
            self.close(); return Err(RpcError::new("disconnected", "reply could not be queued"));
        }
        match tokio::time::timeout(Duration::from_secs(1), flushed).await {
            Ok(Ok(())) => Ok(()),
            _ => { self.close(); Err(RpcError::new("disconnected", "reply flush unconfirmed")) }
        }
    }

    fn reply_message(id:&Value,result:RpcResult)->Value {
        let msg=match result {Ok(value)=>json!({"jsonrpc":"2.0","id":id,"result":value}),Err(error)=>json!({"jsonrpc":"2.0","id":id,"error":error})};
        if crate::limits::json_depth(&msg,crate::limits::MESSAGE_DEPTH).is_ok() {return msg;}
        // Execution may already have happened. Preserve that uncertainty on both ordinary
        // and completion-barrier replies, including malformed over-deep request ids.
        let mut error=RpcError::new("too_large","reply JSON nesting exceeds recovery envelope; inspect state, do not replay");
        error.data.as_mut().unwrap()["outcome"]=json!("indeterminate");
        let id=if id.is_array()||id.is_object() {Value::Null}else{id.clone()};
        json!({"jsonrpc":"2.0","id":id,"error":error})
    }

    pub fn respond(&self, id: Value, result: RpcResult) {
        let msg=Self::reply_message(&id,result);
        let id=&msg["id"];
        let bytes = crate::limits::encoded_bytes(&msg).saturating_add(1);
        if bytes > self.record_limit.load(Ordering::Acquire) {
            let error = RpcError::new("too_large", "reply exceeds recovery envelope; use recovery commands to revert/delete state");
            self.send_control(json!({"jsonrpc":"2.0", "id":id, "error":error}).to_string());
            return;
        }
        let line = msg.to_string();
        let error = if bytes > CONTROL_BYTES {
            if self.large_bytes.compare_exchange(0, bytes, Ordering::AcqRel, Ordering::Acquire).is_err() {
                Some(RpcError::new("overloaded", "one large reply is already in flight"))
            } else {
                let charge = LargeCharge { bytes, used: self.large_bytes.clone() };
                if self.large.try_send(Outbound { line, active: None, _large: Some(charge), flushed: None }, 64).is_ok() { return; }
                Some(RpcError::new("disconnected", "large reply writer closed"))
            }
        } else { self.send_control(line); return; };
        self.send_control(json!({"jsonrpc":"2.0", "id":id, "error":error.unwrap()}).to_string());
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
    async fn serving_read_eof_keeps_replies_writable_but_refuses_new_callbacks() {
        let(raw,serving)=tokio::io::duplex(4096);let(r,w)=tokio::io::split(serving);
        let(peer,mut incoming)=Peer::spawn_serving(r,w);let(r,mut w)=tokio::io::split(raw);
        w.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":7,\"method\":\"work\"}\n").await.unwrap();w.shutdown().await.unwrap();
        let Some(Incoming::Request{id,..})=incoming.recv().await else{panic!("missing request")};
        assert!(incoming.recv().await.is_none());assert!(!peer.is_closed());
        assert_eq!(peer.call("callback",Value::Null).await.unwrap_err().kind(),"disconnected");
        peer.respond_flushed(id,Ok(json!(42))).await.unwrap();
        let mut r=BufReader::new(r);let mut line=String::new();r.read_line(&mut line).await.unwrap();
        assert_eq!(serde_json::from_str::<Value>(&line).unwrap()["result"],42);peer.close();
    }

    #[tokio::test]
    async fn depth_fences_reject_unsent_calls_and_mark_unpublishable_replies_indeterminate() {
        fn nested(depth:usize)->Value {(0..depth).fold(Value::Null,|value,_|json!([value]))}
        let (a,b)=tokio::io::duplex(4096);let (r,w)=tokio::io::split(a);let(peer,_incoming)=Peer::spawn(r,w);
        let (r,w)=tokio::io::split(b);let(remote,mut requests)=Peer::spawn(r,w);
        let error=peer.call("too-deep",nested(crate::limits::MESSAGE_DEPTH)).await.unwrap_err();
        assert_eq!(error.data.unwrap()["outcome"],"rejected");assert!(peer.pending.lock().unwrap().is_empty());assert!(requests.try_recv().is_err());
        let p=peer.clone();let waiting=tokio::spawn(async move {p.call("effect",Value::Null).await});
        let id=match requests.recv().await.unwrap() {Incoming::Request {id,..}=>id,_=>panic!("request")};
        remote.respond(id,Ok(nested(crate::limits::MESSAGE_DEPTH)));
        let error=waiting.await.unwrap().unwrap_err();assert_eq!(error.kind(),"too_large");assert_eq!(error.data.unwrap()["outcome"],"indeterminate");
        let p=peer.clone();let waiting=tokio::spawn(async move {p.call("boundary",Value::Null).await});
        let id=match requests.recv().await.unwrap() {Incoming::Request {id,..}=>id,_=>panic!("request")};
        remote.respond(id,Ok(nested(crate::limits::MESSAGE_DEPTH-1)));
        assert_eq!(waiting.await.unwrap().unwrap(),nested(crate::limits::MESSAGE_DEPTH-1));
        let p=peer.clone();let waiting=tokio::spawn(async move {p.call("flushed-depth",Value::Null).await});
        let id=match requests.recv().await.unwrap() {Incoming::Request {id,..}=>id,_=>panic!("request")};
        remote.respond_flushed(id,Ok(nested(crate::limits::MESSAGE_DEPTH))).await.unwrap();
        let error=waiting.await.unwrap().unwrap_err();assert_eq!(error.data.unwrap()["outcome"],"indeterminate");
        peer.close();remote.close();
    }

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
    async fn aggregate_reply_larger_than_backlog_budget_remains_recoverable() {
        let (a, b) = tokio::io::duplex(65536);
        let (ar, aw) = tokio::io::split(a);
        let (br, bw) = tokio::io::split(b);
        let (client, _) = Peer::spawn(ar, aw);
        let (server, mut incoming) = Peer::spawn(br, bw);
        let task = tokio::spawn(async move {
            let Some(Incoming::Request { id, .. }) = incoming.recv().await else { panic!("missing request") };
            server.respond(id, Ok(json!({"entries": "x".repeat(6 * 1024 * 1024)})));
            server
        });
        let reply = client.call("workspace.snapshot", Value::Null).await.expect("aggregate state must not be locked out by a backlog cap");
        assert_eq!(reply["entries"].as_str().unwrap().len(), 6 * 1024 * 1024);
        assert!(!task.await.unwrap().is_closed());
    }

    #[tokio::test]
    async fn undrained_incoming_queue_is_bounded_and_overflow_is_explicit() {
        let (reader, mut writer) = tokio::io::duplex(65536);
        let (peer, incoming) = Peer::spawn(reader, tokio::io::sink());
        let line = format!("{}\n", json!({"jsonrpc":"2.0","method":"stream.data","params":{"data":"x".repeat(1024)}}));
        for _ in 0..20_000 {
            if writer.write_all(line.as_bytes()).await.is_err() { break; }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(incoming.len() <= 4096, "undrained application queue retained {} records", incoming.len());
        assert!(peer.is_closed(), "overflow must close rather than silently lose records");
    }

    #[tokio::test]
    async fn record_limit_catches_unterminated_records_and_replies() {
        let (reader, mut raw) = tokio::io::duplex(4096);
        let (peer, _) = Peer::spawn(reader, tokio::io::sink());
        peer.set_max_record_bytes(128).unwrap();
        raw.write_all(&vec![b'x'; 129]).await.unwrap();
        tokio::time::timeout(Duration::from_secs(1), peer.closed()).await.unwrap();
        assert!(peer.is_closed());

        let (a, b) = tokio::io::duplex(4096);
        let (ar, aw) = tokio::io::split(a); let (br, bw) = tokio::io::split(b);
        let (client, _) = Peer::spawn(ar, aw); let (server, mut incoming) = Peer::spawn(br, bw);
        client.set_max_record_bytes(1024).unwrap();
        tokio::spawn(async move {
            if let Some(Incoming::Request { id, .. }) = incoming.recv().await { server.respond(id, Ok(json!("x".repeat(2048)))); }
            server.closed().await;
        });
        assert!(client.call("small_request", Value::Null).await.is_err());
        assert!(client.is_closed(), "registered-provider limit includes response records");
    }

    #[tokio::test]
    async fn large_slot_is_charged_through_write_and_exclusive() {
        let (reader, _hold) = tokio::io::duplex(64);
        let (writer, _stalled_reader) = tokio::io::duplex(64);
        let (peer, _) = Peer::spawn(reader, writer);
        peer.respond(json!(1), Ok(json!("x".repeat(6 * 1024 * 1024))));
        tokio::task::yield_now().await;
        assert!(peer.large_bytes.load(Ordering::Acquire) >= 6 * 1024 * 1024);
        peer.respond(json!(2), Ok(json!("x".repeat(6 * 1024 * 1024))));
        assert!(!peer.is_closed(), "busy large slot reports overload without disconnecting recovery");
        assert!(peer.outbound_bytes() < 7 * 1024 * 1024);
        peer.close();
    }

    #[tokio::test]
    async fn pending_calls_are_count_bounded_and_cancel_cleanly() {
        let (reader, _hold) = tokio::io::duplex(64);
        let (peer, _) = Peer::spawn(reader, tokio::io::sink());
        let tasks: Vec<_> = (0..MAX_PENDING_CALLS).map(|_| {
            let p = peer.clone(); tokio::spawn(async move { p.call("held", Value::Null).await })
        }).collect();
        tokio::time::timeout(Duration::from_secs(1), async {
            while peer.pending.lock().unwrap().len() != MAX_PENDING_CALLS { tokio::task::yield_now().await; }
        }).await.unwrap();
        assert_eq!(peer.call("not_sent", Value::Null).await.unwrap_err().kind(), "overloaded");
        for task in tasks { task.abort(); let _ = task.await; }
        assert!(peer.pending.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn tightening_a_record_limit_also_checks_already_queued_records() {
        let (a, b) = tokio::io::duplex(4096);
        let (r, w) = tokio::io::split(a);
        let (peer, mut incoming) = Peer::spawn(r, w);
        let (r, w) = tokio::io::split(b);
        let (sender, _rx) = Peer::spawn(r, w);
        sender.notify("queued", json!({"payload":"x".repeat(512)}));
        tokio::time::timeout(Duration::from_secs(1), async { while incoming.is_empty() { tokio::task::yield_now().await; } }).await.unwrap();
        peer.set_max_record_bytes(128).unwrap();
        assert!(incoming.recv().await.is_none());
        assert!(peer.is_closed());
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
    async fn gap_marker_follows_all_accepted_data() {
        let (a, b) = tokio::io::duplex(64);
        let (r, w) = tokio::io::split(a);
        let (peer, _) = Peer::spawn(r, w);
        let stream = peer.stream_writer(3);
        assert!(stream.send("YWJj".into()));
        stream.finish_as("stream.gap", json!({"reason":"paused"}));
        let mut lines = BufReader::new(b).lines();
        let data: Value = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        let gap: Value = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        assert_eq!(data["method"], "stream.data");
        assert_eq!(gap["method"], "stream.gap");
        assert_eq!(gap["params"]["stream"], 3);
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

/// Shared effect deadline: expiry says nothing about whether the layer below already ran it.
pub async fn with_deadline<T>(method: &str, deadline: Duration, work: impl std::future::Future<Output = Result<T, RpcError>>) -> Result<T, RpcError> {
    match tokio::time::timeout(deadline, work).await {
        Ok(result) => result,
        Err(_) => {
            let mut error = RpcError::new("timeout", format!("{method} deadline expired; outcome indeterminate, do not resend automatically"));
            error.data.as_mut().unwrap()["outcome"] = json!("indeterminate");
            Err(error)
        }
    }
}

/// Parse params into a type, as an `invalid` error on failure.
pub fn params<T: serde::de::DeserializeOwned>(v: Value) -> Result<T, RpcError> {
    let v = if v.is_null() { json!({}) } else { v };
    serde_json::from_value(v).map_err(|e| RpcError::invalid(format!("bad params: {e}")))
}
