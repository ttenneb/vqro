//! Rex's control channel: a WebSocket at `/api/control` with subprotocol `rex.control.v1` (documented by Rex's own
//! API help, `rex api describe`). JSON text messages: one `hello`/`welcome` exchange, then `request`/`response`
//! pairs matched by `id`, and `event` messages in between.
//!
//! One reader task owns the socket's read half: it routes responses to their callers, events to one bounded
//! channel, and answers pings. A response that never comes fails its call at the deadline; nothing is resent.
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use tokio::net::UnixStream;
use tokio::sync::{mpsc, oneshot};

use super::ws::{self, Message};

pub const PROTOCOL: &str = "rex.control.v1";
/// The Rex API revision this client is written against (`rex version`: "api: revision 19").
pub const API_REVISION: u64 = 19;
const MAX_MESSAGE: usize = 1 << 20;
/// Events waiting for the consumer. A consumer that falls this far behind gets `Event::Lagged` and re-reads state.
const EVENT_QUEUE: usize = 4096;

#[derive(Debug, Clone, PartialEq)]
pub enum Error {
    /// The connection failed or closed; an in-flight request's outcome is unknown.
    Lost(String),
    /// Rex answered with an error: the request had no effect (Rex reports failures before acting).
    Rejected { code: String, message: String },
    /// No answer within the deadline: outcome unknown.
    Timeout,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Lost(m) => write!(f, "rex connection lost: {m}"),
            Error::Rejected { code, message } => write!(f, "rex refused ({code}): {message}"),
            Error::Timeout => write!(f, "rex did not answer in time"),
        }
    }
}

#[derive(Debug, Clone)]
pub enum Event {
    /// One server event, as Rex sent it (`{type, payload, seq?}`).
    Event(Value),
    /// Events were dropped because the consumer fell behind; re-read state.
    Lagged,
}

type Pending = Arc<Mutex<HashMap<String, oneshot::Sender<Result<Value, Error>>>>>;

#[derive(Clone)]
pub struct Control {
    writer: Arc<tokio::sync::Mutex<ws::Writer<tokio::net::unix::OwnedWriteHalf>>>,
    pending: Pending,
    next_id: Arc<AtomicU64>,
    closed: Arc<tokio::sync::watch::Sender<Option<String>>>,
    pub welcome: Value,
    timeout: Duration,
}

impl Control {
    /// Connect and say hello. `events` receives every event the server sends this connection.
    pub async fn connect(socket: &Path, name: &str, timeout: Duration) -> Result<(Control, mpsc::Receiver<Event>), Error> {
        let lost = |e: std::io::Error| Error::Lost(e.to_string());
        let mut stream = tokio::time::timeout(timeout, UnixStream::connect(socket)).await.map_err(|_| Error::Timeout)?.map_err(lost)?;
        let leftover = tokio::time::timeout(timeout, ws::handshake(&mut stream, "/api/control", PROTOCOL)).await.map_err(|_| Error::Timeout)?.map_err(lost)?;
        let (read, write) = stream.into_split();
        let mut reader = ws::Reader::new(read, leftover, MAX_MESSAGE);
        let mut writer = ws::Writer::new(write);
        let hello = json!({"type": "hello", "api_revision": API_REVISION,
            "client": {"kind": "cli", "name": name, "version": env!("CARGO_PKG_VERSION")}});
        writer.text(&hello.to_string()).await.map_err(lost)?;
        let welcome = tokio::time::timeout(timeout, async {
            let mut pong = None;
            loop {
                match reader.next(&mut pong).await.map_err(lost)? {
                    Some(Message::Text(t)) => return serde_json::from_str::<Value>(&t).map_err(|e| Error::Lost(e.to_string())),
                    Some(Message::Binary(_)) => return Err(Error::Lost("binary message on the control channel".into())),
                    None => {
                        if let Some(p) = pong.take() {
                            writer.pong(&p).await.map_err(lost)?;
                        }
                    }
                }
            }
        }).await.map_err(|_| Error::Timeout)??;
        if welcome["type"] != "welcome" {
            return Err(Error::Rejected { code: welcome["error"]["code"].as_str().unwrap_or("hello").into(),
                message: format!("rex refused the connection: {welcome}") });
        }
        let control = Control {
            writer: Arc::new(tokio::sync::Mutex::new(writer)),
            pending: Arc::new(Mutex::new(HashMap::new())),
            next_id: Arc::new(AtomicU64::new(1)),
            closed: Arc::new(tokio::sync::watch::channel(None).0),
            welcome,
            timeout,
        };
        let (events_tx, events_rx) = mpsc::channel(EVENT_QUEUE);
        tokio::spawn(control.clone().read_loop(reader, events_tx));
        Ok((control, events_rx))
    }

    async fn read_loop(self, mut reader: ws::Reader<tokio::net::unix::OwnedReadHalf>, events: mpsc::Sender<Event>) {
        let mut pong = None;
        let mut lagged = false;
        let why = loop {
            // Keep the same in-flight read future: WebSocket frame parsing is not cancellation-safe.
            // A freed queue slot must deliver the pending lag marker even if Rex sends nothing else.
            let next = {
                let next = reader.next(&mut pong);
                tokio::pin!(next);
                loop {
                    tokio::select! {
                        result = &mut next => break result,
                        permit = events.reserve(), if lagged => {
                            if let Ok(permit) = permit { permit.send(Event::Lagged); }
                            lagged = false;
                        }
                    }
                }
            };
            let message = match next {
                Ok(Some(Message::Text(t))) => t,
                Ok(Some(Message::Binary(_))) => break "binary message on the control channel".to_string(),
                Ok(None) => {
                    if let Some(p) = pong.take() {
                        if let Err(e) = self.writer.lock().await.pong(&p).await {
                            break e.to_string();
                        }
                    }
                    continue;
                }
                Err(e) => break e.to_string(),
            };
            let Ok(value) = serde_json::from_str::<Value>(&message) else { break "invalid JSON from rex".into() };
            match value["type"].as_str() {
                Some("response") => {
                    let Some(id) = value["id"].as_str() else { continue };
                    let Some(reply) = self.pending.lock().unwrap().remove(id) else { continue };
                    let result = if let Some(error) = value.get("error").filter(|e| !e.is_null()) {
                        Err(Error::Rejected { code: error["code"].as_str().unwrap_or("error").into(),
                            message: error["message"].as_str().unwrap_or_default().into() })
                    } else {
                        Ok(value.get("payload").cloned().unwrap_or(Value::Null))
                    };
                    let _ = reply.send(result);
                }
                Some("event") => {
                    let payload = value.get("payload").cloned().unwrap_or(Value::Null);
                    if lagged {
                        if events.try_send(Event::Lagged).is_ok() {
                            lagged = false;
                        }
                    }
                    if !lagged && events.try_send(Event::Event(payload)).is_err() {
                        lagged = true;
                    }
                }
                _ => {}
            }
        };
        self.fail(why);
    }

    fn fail(&self, why: String) {
        self.closed.send_replace(Some(why.clone()));
        for (_, reply) in self.pending.lock().unwrap().drain() {
            let _ = reply.send(Err(Error::Lost(why.clone())));
        }
    }

    pub fn is_closed(&self) -> bool {
        self.closed.borrow().is_some()
    }

    /// Resolves when the connection has failed or closed, with why.
    pub async fn closed(&self) -> String {
        let mut rx = self.closed.subscribe();
        loop {
            if let Some(why) = rx.borrow().clone() {
                return why;
            }
            if rx.changed().await.is_err() {
                return "control connection dropped".into();
            }
        }
    }

    /// One request. `Rejected` means Rex refused it; `Lost`/`Timeout` mean the outcome is unknown. Never resent.
    pub async fn call(&self, method: &str, payload: Value) -> Result<Value, Error> {
        if let Some(why) = self.closed.borrow().clone() {
            return Err(Error::Lost(why));
        }
        let id = format!("v{}", self.next_id.fetch_add(1, Ordering::Relaxed));
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id.clone(), tx);
        let request = json!({"type": "request", "id": id, "method": method, "payload": payload});
        if let Err(e) = self.writer.lock().await.text(&request.to_string()).await {
            self.pending.lock().unwrap().remove(&id);
            self.fail(e.to_string());
            return Err(Error::Lost(e.to_string()));
        }
        match tokio::time::timeout(self.timeout, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(Error::Lost("control connection dropped".into())),
            Err(_) => {
                self.pending.lock().unwrap().remove(&id);
                Err(Error::Timeout)
            }
        }
    }

    /// Close politely; in-flight calls fail as `Lost`.
    pub async fn close(&self) {
        let _ = self.writer.lock().await.close().await;
        self.fail("closed by vqro-rex".into());
    }
}

#[cfg(all(test, unix))]
mod regression_tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

    #[tokio::test]
    async fn lag_marker_arrives_after_drain_without_another_server_event() {
        let (client, mut server) = tokio::net::UnixStream::pair().unwrap();
        let (read, write) = client.into_split();
        let control = Control { writer:Arc::new(tokio::sync::Mutex::new(ws::Writer::new(write))),
            pending:Arc::new(Mutex::new(HashMap::new())), next_id:Arc::new(AtomicU64::new(1)),
            closed:Arc::new(tokio::sync::watch::channel(None).0), welcome:Value::Null, timeout:Duration::from_secs(1) };
        let (tx, mut rx) = mpsc::channel(1);
        let task = tokio::spawn(control.read_loop(ws::Reader::new(read, vec![], MAX_MESSAGE), tx));
        let payload = json!({"type":"event","payload":{}}).to_string();
        let mut frame = vec![0x81, payload.len() as u8];
        frame.extend_from_slice(payload.as_bytes());
        server.write_all(&[frame.clone(),frame].concat()).await.unwrap();
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert!(matches!(rx.recv().await, Some(Event::Event(_))));
        assert!(matches!(tokio::time::timeout(Duration::from_millis(100), rx.recv()).await,
            Ok(Some(Event::Lagged))), "no subsequent event may be needed to signal loss");
        task.abort();
    }
}
