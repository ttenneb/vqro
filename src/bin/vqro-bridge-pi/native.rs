//! Owned Pi process and bounded native JSONL. No retry, replay or vqro policy here.
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, Weak};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::{mpsc, oneshot, watch, OwnedSemaphorePermit, Semaphore};
use tokio::time::Instant;

pub const MAX_RECORD: usize = 1024 * 1024;
const QUEUED: u8 = 0;
const WRITING: u8 = 1;
const WRITTEN: u8 = 2;
const REPLIED: u8 = 3;
const CANCELLED: u8 = 4;

#[derive(Debug, Clone)]
pub struct Error {
    pub kind: &'static str,
    pub message: String,
    /// A native write began; the caller must not turn this failure into a no-effect rejection.
    pub may_have_acted: bool,
}
impl Error {
    fn new(kind: &'static str, message: impl Into<String>) -> Self { Self { kind, message: message.into(), may_have_acted:false } }
    fn unknown(kind: &'static str, message: impl Into<String>) -> Self { Self { kind, message:message.into(),may_have_acted:true } }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(f,"{} ({})",self.message,self.kind) }
}
impl std::error::Error for Error {}

#[derive(Debug, Clone)]
pub struct Reply { pub success: bool, pub data: Value, pub error: Value }
#[derive(Debug, Clone)]
pub struct Exit { pub code: Option<i32>, pub success: bool }
#[derive(Debug, Clone)]
pub enum Event { Record(Value), ProtocolError(String), Exited(Exit) }
pub type Observe = Arc<dyn Fn(Event) + Send + Sync>;

struct Pending {
    command: String,
    result: oneshot::Sender<Result<Reply, Error>>,
    phase: Arc<AtomicU8>,
}
struct Write {
    id: Option<String>,
    line: Vec<u8>,
    deadline: Instant,
    phase: Arc<AtomicU8>,
    _permit: OwnedSemaphorePermit,
}
struct Inner {
    pending: Mutex<HashMap<String, Pending>>,
    writes: mpsc::Sender<Write>,
    budget: Arc<Semaphore>,
    serial: AtomicU64,
    dead: AtomicBool,
    broken: AtomicBool,
    stopping: AtomicBool,
    stop: watch::Sender<bool>,
    exit: watch::Sender<Option<Exit>>,
    stderr: Mutex<VecDeque<u8>>,
    stderr_total: AtomicU64,
    observe: Observe,
}
impl Inner {
    fn fail_pending(&self, message: &str) {
        for (_, pending) in self.pending.lock().unwrap().drain() {
            let error = if matches!(pending.phase.load(Ordering::SeqCst),WRITING | WRITTEN | REPLIED) {
                Error::unknown("unavailable",message)
            } else { Error::new("unavailable",message) };
            let _ = pending.result.send(Err(error));
        }
    }
    fn protocol_error(&self, message: impl Into<String>) {
        let message = message.into();
        if !self.broken.swap(true, Ordering::SeqCst) {
            (self.observe)(Event::ProtocolError(message.clone()));
            self.fail_pending(&message);
            self.stopping.store(true, Ordering::SeqCst);
            let _ = self.stop.send(true);
        }
    }
}

/// Dropping an unwritten ticket cancels it; a write already begun is never represented as rejected.
pub struct Ticket {
    pub id: String,
    result: Option<oneshot::Receiver<Result<Reply, Error>>>,
    phase: Arc<AtomicU8>,
    inner: Weak<Inner>,
}
impl Ticket {
    pub fn cancel_before_write(&self) -> bool {
        if self.phase.compare_exchange(QUEUED, CANCELLED, Ordering::SeqCst, Ordering::SeqCst).is_err() { return false; }
        if let Some(inner) = self.inner.upgrade() {
            if let Some(pending) = inner.pending.lock().unwrap().remove(&self.id) {
                let _ = pending.result.send(Err(Error::new("timeout", "deadline before native dispatch")));
            }
        }
        true
    }
    pub fn write_begun(&self) -> bool { matches!(self.phase.load(Ordering::SeqCst), WRITING | WRITTEN | REPLIED) }
    pub fn written(&self) -> bool { matches!(self.phase.load(Ordering::SeqCst), WRITTEN | REPLIED) }
    pub async fn reply(&mut self) -> Result<Reply, Error> {
        let Some(result) = self.result.as_mut() else { return Err(Error::new("invalid", "native result already taken")); };
        let result = result.await.map_err(|_| if self.write_begun() { Error::unknown("unavailable", "native response channel closed") }
            else { Error::new("unavailable", "native response channel closed") })?;
        self.result = None;
        result
    }
}
impl Drop for Ticket {
    fn drop(&mut self) {
        self.cancel_before_write();
        if let Some(inner) = self.inner.upgrade() { inner.pending.lock().unwrap().remove(&self.id); }
    }
}

#[derive(Clone)]
pub struct Native { inner: Arc<Inner> }
/// The environment any native Pi child gets (the RPC runtime and the --version read alike): the bridge's environment
/// cleared, with no Herdr control identity and no vqro bootstrap credential; only the public VQRO_SOCKET and
/// VQRO_PRINCIPAL pass.
pub fn native_env(command:&mut Command,env:&HashMap<String,String>) {
    command.env_clear();
    for (key,value) in env {
        if !key.starts_with("HERDR_") && (!key.starts_with("VQRO_") || matches!(key.as_str(),"VQRO_SOCKET"|"VQRO_PRINCIPAL")) { command.env(key,value); }
    }
}

impl Native {
    pub async fn spawn(argv: &[String], cwd: &std::path::Path, env: &HashMap<String,String>, observe: Observe) -> Result<Self, Error> {
        let Some(program) = argv.first() else { return Err(Error::new("invalid", "explicit Pi argv is required")); };
        let mut command = vqro::program::background_command(program);
        command.args(&argv[1..]).current_dir(cwd);
        native_env(&mut command,env);
        command.stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).kill_on_drop(true);
        let mut child = command.spawn().map_err(|e| Error::new("backend",format!("start native Pi: {e}")))?;
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        let (writes, receiver) = mpsc::channel(32);
        let (stop, mut stopping) = watch::channel(false);
        let (exit, _) = watch::channel(None);
        let inner = Arc::new(Inner { pending:Mutex::new(HashMap::new()), writes, budget:Arc::new(Semaphore::new(MAX_RECORD)),
            serial:AtomicU64::new(1),dead:AtomicBool::new(false),broken:AtomicBool::new(false),stopping:AtomicBool::new(false),
            stop,exit,stderr:Mutex::new(VecDeque::new()),stderr_total:AtomicU64::new(0),observe });
        tokio::spawn(write_native(stdin, receiver, inner.clone()));
        tokio::spawn(read_native(stdout, inner.clone()));
        tokio::spawn(read_stderr(stderr, inner.clone()));
        let state = inner.clone();
        tokio::spawn(async move {
            let status = tokio::select! {
                result = child.wait() => result,
                _ = async { let _ = stopping.wait_for(|stop| *stop).await; } => {
                    let _ = child.start_kill();
                    child.wait().await
                }
            };
            state.dead.store(true, Ordering::SeqCst);
            let exit = status.map(|s| Exit { code:s.code(),success:s.success() }).unwrap_or(Exit { code:None,success:false });
            (state.observe)(Event::Exited(exit.clone()));
            state.exit.send_replace(Some(exit));
        });
        Ok(Self { inner })
    }
    pub fn alive(&self) -> bool { !self.inner.dead.load(Ordering::SeqCst) && !self.inner.broken.load(Ordering::SeqCst) }
    pub fn pending(&self) -> usize { self.inner.pending.lock().unwrap().len() }
    pub fn stderr_tail(&self) -> (Vec<u8>, u64) {
        (self.inner.stderr.lock().unwrap().iter().copied().collect(),self.inner.stderr_total.load(Ordering::Relaxed))
    }
    pub fn stop(&self) {
        if !self.inner.stopping.swap(true,Ordering::SeqCst) { let _ = self.inner.stop.send(true); }
    }
    pub async fn exited(&self) -> Exit {
        let mut exit = self.inner.exit.subscribe();
        loop {
            if let Some(result) = exit.borrow().clone() { return result; }
            if exit.changed().await.is_err() { return Exit { code:None,success:false }; }
        }
    }
    pub fn submit(&self, command: &str, args: Value, deadline: Instant) -> Result<Ticket, Error> {
        if !self.alive() { return Err(Error::new("unavailable", "native Pi is not available")); }
        if deadline <= Instant::now() { return Err(Error::new("timeout", "deadline before native dispatch")); }
        let Some(mut params) = args.as_object().cloned() else { return Err(Error::new("invalid", "native arguments must be an object")); };
        let id = format!("n{}",self.inner.serial.fetch_add(1,Ordering::Relaxed));
        params.insert("type".into(),json!(command)); params.insert("id".into(),json!(id));
        let mut line = serde_json::to_vec(&params).map_err(|e|Error::new("invalid",e.to_string()))?;
        line.push(b'\n');
        if line.len() > MAX_RECORD { return Err(Error::new("too_large", "native record exceeds byte limit; not sent")); }
        let permit = self.inner.budget.clone().try_acquire_many_owned(line.len().max(64) as u32).map_err(|_|Error::new("overloaded","native output budget full; not sent"))?;
        let (tx,rx) = oneshot::channel();
        let phase = Arc::new(AtomicU8::new(QUEUED));
        {
            let mut pending = self.inner.pending.lock().unwrap();
            if pending.len() >= 32 { return Err(Error::new("overloaded","native pending limit; not sent")); }
            pending.insert(id.clone(),Pending { command:command.into(),result:tx,phase:phase.clone() });
        }
        let write = Write { id:Some(id.clone()),line,deadline,phase:phase.clone(),_permit:permit };
        if self.inner.writes.try_send(write).is_err() {
            self.inner.pending.lock().unwrap().remove(&id);
            return Err(Error::new("overloaded","native write queue full; not sent"));
        }
        Ok(Ticket { id,result:Some(rx),phase,inner:Arc::downgrade(&self.inner) })
    }
    /// Metadata convenience. Input admission keeps its Ticket for a late receipt instead of using this timeout.
    pub async fn call(&self, command: &str, args: Value, timeout: Duration) -> Result<Reply, Error> {
        let mut ticket = self.submit(command,args,Instant::now()+timeout)?;
        match tokio::time::timeout(timeout,ticket.reply()).await {
            Ok(reply) => reply,
            Err(_) => {
                if ticket.cancel_before_write() || !ticket.write_begun() { Err(Error::new("timeout","native deadline before dispatch")) }
                else { Err(Error::unknown("timeout","native response deadline; outcome unknown after a write, never resent")) }
            }
        }
    }
    /// No native acknowledgement exists for Pi UI answers. True means the whole line was written.
    pub async fn notify(&self, command: &str, args: Value, deadline: Instant) -> Result<(), Error> {
        if !self.alive() { return Err(Error::new("unavailable","native Pi is not available")); }
        let Some(mut params) = args.as_object().cloned() else { return Err(Error::new("invalid","native arguments must be an object")); };
        params.insert("type".into(),json!(command));
        let mut line = serde_json::to_vec(&params).map_err(|e|Error::new("invalid",e.to_string()))?;
        line.push(b'\n');
        if line.len() > MAX_RECORD { return Err(Error::new("too_large","native record exceeds byte limit; not sent")); }
        let permit = self.inner.budget.clone().try_acquire_many_owned(line.len().max(64) as u32).map_err(|_|Error::new("overloaded","native output budget full; not sent"))?;
        let phase = Arc::new(AtomicU8::new(QUEUED));
        self.inner.writes.try_send(Write { id:None,line,deadline,phase:phase.clone(),_permit:permit }).map_err(|_|Error::new("overloaded","native write queue full; not sent"))?;
        loop {
            match phase.load(Ordering::SeqCst) {
                WRITTEN => return Ok(()),
                CANCELLED => return Err(Error::new("timeout","native notice expired before dispatch")),
                _ if !self.alive() => return Err(if phase.load(Ordering::SeqCst)==QUEUED { Error::new("unavailable","native notice not dispatched") }
                    else { Error::unknown("unavailable","native notice outcome unknown") }),
                _ if Instant::now() >= deadline => {
                    if phase.compare_exchange(QUEUED,CANCELLED,Ordering::SeqCst,Ordering::SeqCst).is_ok() { return Err(Error::new("timeout","native notice expired before dispatch")); }
                    return Err(Error::unknown("timeout","native notice write outcome unknown"));
                }
                _ => tokio::time::sleep(Duration::from_millis(1)).await,
            }
        }
    }
}

async fn write_native(mut stdin: tokio::process::ChildStdin, mut writes: mpsc::Receiver<Write>, state: Arc<Inner>) {
    let mut exit = state.exit.subscribe();
    loop {
        let write = tokio::select! { value = writes.recv() => value, _ = exit.wait_for(|value|value.is_some()) => None };
        let Some(write) = write else { break; };
        if Instant::now() >= write.deadline || write.phase.compare_exchange(QUEUED,WRITING,Ordering::SeqCst,Ordering::SeqCst).is_err() {
            write.phase.store(CANCELLED,Ordering::SeqCst);
            if let Some(id) = write.id {
                if let Some(pending) = state.pending.lock().unwrap().remove(&id) { let _ = pending.result.send(Err(Error::new("timeout","deadline before native dispatch"))); }
            }
            continue;
        }
        let result = tokio::time::timeout(Duration::from_secs(5),async { stdin.write_all(&write.line).await?; stdin.flush().await }).await;
        if !matches!(result,Ok(Ok(()))) { state.protocol_error("native write failed/blocked; outcomes unknown"); break; }
        let _ = write.phase.compare_exchange(WRITING,WRITTEN,Ordering::SeqCst,Ordering::SeqCst);
    }
}

async fn read_native(stdout: tokio::process::ChildStdout, state: Arc<Inner>) {
    let mut stdout = BufReader::new(stdout);
    let mut exit = state.exit.subscribe();
    let mut drain_deadline = None;
    // Keep partial framing across cancellation by the child-exit arm below.
    let mut line = Vec::new();
    loop {
        if drain_deadline.is_none() && exit.borrow().is_some() { drain_deadline = Some(Instant::now()+Duration::from_secs(1)); }
        let read = async {
            loop {
                let bytes = stdout.fill_buf().await.map_err(|e|e.to_string())?;
                if bytes.is_empty() { return if line.is_empty() { Ok::<bool,String>(false) } else { Err("native stdout ended mid-record".into()) }; }
                let end = bytes.iter().position(|b|*b == b'\n');
                let take = end.map(|at|at+1).unwrap_or(bytes.len());
                if line.len()+take > MAX_RECORD { return Err("native stdout record exceeds byte limit".into()); }
                line.extend_from_slice(&bytes[..take]); stdout.consume(take);
                if end.is_some() { return Ok(true); }
            }
        };
        let result = tokio::select! {
            result = read => result,
            _ = exit.changed(), if drain_deadline.is_none() => { drain_deadline = Some(Instant::now()+Duration::from_secs(1)); continue; },
            _ = async { if let Some(deadline) = drain_deadline { tokio::time::sleep_until(deadline).await; } else { std::future::pending::<()>().await; } } => break,
        };
        match result {
            Ok(false) => break,
            Err(error) => { state.protocol_error(error); break; }
            Ok(true) => {}
        }
        if line.iter().all(u8::is_ascii_whitespace) { line.clear(); continue; }
        let record: Value = match serde_json::from_slice(&line) { Ok(record) => record, Err(error) => { state.protocol_error(format!("invalid native JSON: {error}")); break; } };
        line.clear();
        let Some(kind) = record["type"].as_str() else { state.protocol_error("native record is not a typed object"); break; };
        if kind == "response" {
            let Some(id) = record["id"].as_str() else { state.protocol_error("native response has no id"); break; };
            let pending = state.pending.lock().unwrap().remove(id);
            if let Some(pending) = pending {
                if record["command"] != pending.command || !record["success"].is_boolean() {
                    let _ = pending.result.send(Err(Error::unknown("protocol","native response command/type mismatch")));
                    state.protocol_error("native response command/type mismatch"); break;
                }
                pending.phase.store(REPLIED,Ordering::SeqCst);
                let reply = Reply { success:record["success"] == true,data:record.get("data").cloned().unwrap_or(Value::Null),error:record.get("error").cloned().unwrap_or(Value::Null) };
                // Normalize observations before waking an effect/config caller on another thread.
                (state.observe)(Event::Record(record));
                let _ = pending.result.send(Ok(reply));
                continue;
            }
        }
        (state.observe)(Event::Record(record));
    }
    state.fail_pending("native stdout closed; pending outcomes unknown");
    // EOF may race normal process exit. Do not leave an owned process alive with no usable RPC.
    if !state.dead.load(Ordering::SeqCst) {
        let _ = tokio::time::timeout(Duration::from_millis(100),exit.wait_for(|value|value.is_some())).await;
        if !state.dead.load(Ordering::SeqCst) { state.protocol_error("native stdout closed while process remains alive"); }
    }
}

async fn read_stderr(mut stderr: tokio::process::ChildStderr, state: Arc<Inner>) {
    let mut exit = state.exit.subscribe();
    let mut buf = [0;4096];
    loop {
        let read = tokio::select! { value = stderr.read(&mut buf) => value, _ = exit.wait_for(|value|value.is_some()) => break };
        let Ok(n) = read else { break; };
        if n == 0 { break; }
        state.stderr_total.fetch_add(n as u64,Ordering::Relaxed);
        let mut tail = state.stderr.lock().unwrap();
        tail.extend(&buf[..n]);
        while tail.len() > 65_536 { tail.pop_front(); }
    }
}
