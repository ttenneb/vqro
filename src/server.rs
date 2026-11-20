//! The core protocol server: JSON-RPC lines over a local socket. See `docs/spec/core-protocol.md`.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use base64::Engine;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::net::UnixListener;
use tokio::sync::{broadcast, mpsc};

use crate::adapter::{SpawnRequest, StreamInput, StreamOutput};
use crate::core::{CommandInfo, Core, Shared};
use crate::rpc::{params, Incoming, Peer, RpcError, RpcResult};
use crate::store::Op;
use crate::principal::{self,Attribution};
use crate::ext;
use crate::provider::{self, Route};

pub use crate::adapter::Backend;

pub const PROTOCOL: u64 = 1;
/// Requested OS send buffer per client connection (see `serve`).
pub const CONNECTION_SEND_BUFFER: usize = 16 * 1024;
const MUTATION_SLOTS:usize=crate::rpc::MAX_PENDING_CALLS+crate::commit::CONTROL_REQUESTS;
const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

#[cfg(test)]
#[path="operational_read_tests.rs"]
mod operational_read_tests;

struct Conn {
    id: u64,
    actual_author:String,
    principal:Mutex<Option<String>>,
    peer: Peer,
    author: Mutex<String>,
    extension: Mutex<Option<String>>,
    supervised: Mutex<Option<crate::ext::Owner>>,
    streams: Mutex<HashMap<u64, StreamBinding>>,
    next_stream: AtomicU64,
    subscribed: Mutex<bool>,
    requests: Arc<tokio::sync::Semaphore>,
    control_requests:Arc<tokio::sync::Semaphore>,
    mutations:Arc<tokio::sync::Semaphore>,
    wire: crate::provider_wire::Connection,
}

impl Conn {
    fn author(&self) -> String {self.author.lock().unwrap().clone()}
    fn actor(&self)->String {self.principal.lock().unwrap().clone().unwrap_or_else(||principal::UNDECLARED.into())}
    fn bind_actor(&self,actor:String)->Result<(),RpcError> {
        if actor==principal::UNDECLARED{return Ok(())}
        let mut known=self.principal.lock().unwrap();
        if known.as_ref().is_some_and(|known|known!=&actor){return Err(RpcError::new("conflict","connection identity is already bound; open a new connection to change it"));}
        *known=Some(actor);Ok(())
    }
    fn caller(&self)->Caller { Caller { author:self.actual_author.clone(), owner:self.supervised.lock().unwrap().clone() } }
}

/// Immutable request attribution. A shared author name is not a lifetime credential.
#[derive(Clone, serde::Serialize)]
#[serde(transparent)]
struct Caller { author:String, #[serde(skip)] owner:Option<ext::Owner> }
impl std::ops::Deref for Caller { type Target=str; fn deref(&self)->&str { &self.author } }
impl From<&Caller> for Caller { fn from(caller:&Caller)->Self { caller.clone() } }
impl Caller { fn check(&self,c:&Core)->Result<(),RpcError> { ext::check_owner(c,self.owner.as_ref()) } }

struct StreamBinding {
    input: crate::queue::Sender<StreamInput>,
    route: Route,
}

/// Open the store, bind the socket, start the adapter and extensions, and serve in the background.
/// `defaults`: a directory with `defaults.json` listing extensions to install on first run.
pub fn start(socket: std::path::PathBuf, state: std::path::PathBuf, safe: bool, backend: Backend, defaults: Option<std::path::PathBuf>) -> anyhow::Result<Shared> {
    raise_nofile_limit();
    crate::store::create_dir_all_durable(&state)?;
    let mut store=crate::store::Store::open_locked(state.join("workspace.json"))?;
    principal::bootstrap(&mut store)?;
    if socket.exists() {
        if std::os::unix::net::UnixStream::connect(&socket).is_ok() {
            anyhow::bail!("vqro is already running at {}", socket.display());
        }
        std::fs::remove_file(&socket)?;
    }
    if let Some(dir) = &defaults {
        install_defaults(&mut store, dir);
    }
    let core: Shared = Arc::new(Mutex::new(Core::new(store, Some(state), socket.clone(), safe)?));
    if let Some(dir)=defaults {core.lock().unwrap().manifest_catalog.root=dir;}
    let listener = UnixListener::bind(&socket)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))?;
    }
    core.lock().unwrap().embedder_backend=Some(backend.clone());
    // Only explicit test/embedder adapters remain; even they stay stopped in safe mode.
    if !safe {
        let (tx, rx) = mpsc::unbounded_channel();
        if let Some(handle) = backend.start(tx) { crate::core::add_adapter(&core, handle, rx, true); }
    }
    ext::reconcile(&core);
    let c = core.clone();
    tokio::spawn(async move {
        if let Err(e) = serve(c, listener).await {
            eprintln!("vqro: server stopped: {e}");
        }
    });
    Ok(core)
}

async fn stop_recovery_providers(handles: Vec<crate::adapter::AdapterHandle>) {
    let mut tasks=tokio::task::JoinSet::new();
    for handle in handles { tasks.spawn(async move { let _=handle.shutdown().await; }); }
    if tokio::time::timeout(std::time::Duration::from_secs(2),async { while tasks.join_next().await.is_some() {} }).await.is_err() {
        eprintln!("vqro: safe-mode provider stop deadline; transport cleanup unconfirmed"); tasks.abort_all();
    }
}

/// Declare versioned defaults once; preserve existing entries and deliberate removals.
/// Provider metadata (`kind`, `default`, `description`) remains part of each declaration.
fn install_defaults(store: &mut crate::store::Store, dir: &std::path::Path) {
    let Ok(bytes) = std::fs::read(dir.join("defaults.json")) else { return };
    let Ok(list) = serde_json::from_slice::<Value>(&bytes) else { return };
    let version = list["version"].as_u64().unwrap_or(1);
    let recorded = store.get("doc/vqro/defaults").map(|e| e.value.clone());
    // No record: nothing installed yet. A record without a version predates versioned defaults (1).
    let installed = recorded.as_ref().map(|r| r["version"].as_u64().unwrap_or(1)).unwrap_or(0);
    // Repair only declarations still identical to an installed default, except its old pinned program.
    let mut repairs=Vec::new();
    for spec in list["extensions"].as_array().into_iter().flatten() {
        let (Some(name),Some(program))=(spec["name"].as_str(),spec["argv"][0].as_str()) else {continue};
        if spec["kind"]!="provider" || !program.starts_with("vqro-") {continue}
        if !recorded.as_ref().and_then(|r|r["installed"].as_array()).is_some_and(|names|names.iter().any(|n|n==name)) {continue}
        let key=format!("extension/{name}"); let Some(entry)=store.get(&key) else {continue};
        let Some(old)=entry.value["argv"][0].as_str() else {continue};
        if old==program || !std::path::Path::new(old).is_absolute() {continue}
        let filename=std::path::Path::new(old).file_name().and_then(|n|n.to_str()).unwrap_or("");
        if filename!=program && filename!=format!("{program}.exe") {continue}
        let mut expected=json!({"argv":spec["argv"]});
        for field in ["description","kind","enabled","default"] {if !spec[field].is_null(){expected[field]=spec[field].clone();}}
        let mut current=entry.value.clone();current["argv"][0]=json!(program);
        if current==expected {repairs.push(Op::Set{key,value:expected,expect:Some(entry.version)});}
    }
    if !repairs.is_empty() { if let Err(error)=store.apply("vqro:defaults",Some("resolve unchanged default providers at start".into()),repairs) {eprintln!("vqro: default program repair failed: {error}");} }
    if installed >= version {
        return;
    }
    let mut ops = Vec::new();
    let mut names: Vec<Value> = recorded.as_ref().and_then(|r| r["installed"].as_array().cloned()).unwrap_or_default();
    for e in list["extensions"].as_array().into_iter().flatten() {
        let (Some(name), Some(argv)) = (e["name"].as_str(), e["argv"].as_array()) else { continue };
        if e["since"].as_u64().unwrap_or(1) <= installed {
            continue; // offered before: never re-add what the user removed
        }
        let argv: Vec<Value> = argv
            .iter()
            .filter_map(|a| a.as_str())
            .map(|a| {
                if a.ends_with(".py") && !a.starts_with('/') {
                    json!(dir.join(a).to_string_lossy())
                } else {
                    json!(a)
                }
            })
            .collect();
        if store.get(&format!("extension/{name}")).is_none() {
            let mut value = json!({"argv": argv});
            for key in ["description", "kind", "enabled", "default"] {
                if !e[key].is_null() {
                    value[key] = e[key].clone();
                }
            }
            ops.push(Op::Set { key: format!("extension/{name}"), value, expect: None });
        }
        if !names.iter().any(|n| n == name) {
            names.push(json!(name));
        }
    }
    ops.push(Op::Set { key: "doc/vqro/defaults".into(), value: json!({"installed": names, "from": dir.to_string_lossy(), "version": version}), expect: None });
    let note = if installed == 0 { "install default extensions" } else { "add new default extensions" };
    if let Err(e) = store.apply("vqro:defaults", Some(note.into()), ops) {
        eprintln!("vqro: could not install default extensions: {e}");
    }
}

pub const MAX_CONNECTIONS:usize=2048;

/// Best effort, this process only: never change the inherited hard limit.
#[cfg(unix)]
fn raise_nofile_limit() {
    let mut limit=libc::rlimit {rlim_cur:0,rlim_max:0};
    // SAFETY: valid pointers to the OS resource-limit struct; no ownership crosses the call.
    unsafe {
        if libc::getrlimit(libc::RLIMIT_NOFILE,&mut limit)!=0 {eprintln!("vqro: could not inspect nofile limit: {}",std::io::Error::last_os_error());return;}
        if limit.rlim_cur<limit.rlim_max {
            let old=limit.rlim_cur;limit.rlim_cur=limit.rlim_max;
            if libc::setrlimit(libc::RLIMIT_NOFILE,&limit)==0 {eprintln!("vqro: raised soft nofile limit {old} -> {} (hard limit unchanged)",limit.rlim_cur);}
            else {eprintln!("vqro: could not raise soft nofile limit: {}",std::io::Error::last_os_error());}
        }
    }
}
#[cfg(not(unix))] fn raise_nofile_limit() {}

fn transient_accept_error(error:&std::io::Error)->bool {
    matches!(error.kind(),std::io::ErrorKind::Interrupted|std::io::ErrorKind::ConnectionAborted|std::io::ErrorKind::WouldBlock)
        || matches!(error.raw_os_error(),Some(libc::EMFILE|libc::ENFILE|libc::ENOMEM|libc::ENOBUFS))
}

#[cfg(test)]
mod accept_tests {
    #[test]
    fn descriptor_and_transient_accept_failures_are_not_fatal_listener_errors() {
        for code in [libc::EMFILE,libc::ENFILE,libc::ENOMEM,libc::ENOBUFS,libc::EINTR,libc::ECONNABORTED] {assert!(super::transient_accept_error(&std::io::Error::from_raw_os_error(code)),"{code}");}
        assert!(!super::transient_accept_error(&std::io::Error::from_raw_os_error(libc::EBADF)));
    }
}

async fn refuse_connection(mut stream:tokio::net::UnixStream) {
    use tokio::io::{AsyncReadExt,AsyncBufReadExt,AsyncWriteExt};
    // A tiny bounded handshake preserves the caller's request id, so normal clients receive
    // the overload reason instead of an unexplained EOF. Silent clients do not hold admission.
    let id={
        let mut reader=tokio::io::BufReader::new(&mut stream).take(4096);let mut line=Vec::new();
        match tokio::time::timeout(std::time::Duration::from_millis(100),reader.read_until(b'\n',&mut line)).await {
            Ok(Ok(_))=>serde_json::from_slice::<Value>(&line).ok().and_then(|value|value.get("id").cloned()).unwrap_or(Value::Null),
            _=>Value::Null,
        }
    };
    let reply=json!({"jsonrpc":"2.0","id":id,"error":{"code":-32000,"message":format!("core connection limit {MAX_CONNECTIONS}; connection not admitted"),"data":{"kind":"overloaded","outcome":"rejected","reason":"connection_limit","limit":MAX_CONNECTIONS}}});
    let line=format!("{reply}\n");
    let _=tokio::time::timeout(std::time::Duration::from_millis(100),stream.write_all(line.as_bytes())).await;
}

pub async fn serve(core: Shared, listener: UnixListener) -> anyhow::Result<()> {
    let next = AtomicU64::new(1);
    let shutdown = core.lock().unwrap().shutdown.clone();
    let mut connections = tokio::task::JoinSet::new();
    let mut peers = HashMap::new();
    let mut last_accept_notice=None::<std::time::Instant>;
    loop {
        let (stream, _) = tokio::select! {
            biased;
            _ = shutdown.requested() => break,
            done = connections.join_next(), if !connections.is_empty() => { if let Some(Ok(id)) = done { peers.remove(&id); } continue; }
            accepted = listener.accept() => match accepted {
                Ok(stream)=>stream,
                Err(error) if transient_accept_error(&error)=>{
                    let now=std::time::Instant::now();
                    if last_accept_notice.is_none_or(|last|now.duration_since(last)>=std::time::Duration::from_secs(5)) {
                        eprintln!("vqro: listener resource pressure ({error}); retrying, existing connections remain served");last_accept_notice=Some(now);
                    }
                    tokio::select! {_=shutdown.requested()=>break,_=tokio::time::sleep(std::time::Duration::from_millis(100))=>{}}
                    continue;
                }
                Err(error)=>{eprintln!("vqro: listener failed: {error}");break;}
            },
        };
        if connections.len()>=MAX_CONNECTIONS {refuse_connection(stream).await;continue;}
        // Bounded outbound: bytes in the OS send buffer can't be revoked when a slow viewer's streams
        // are closed, so keep that buffer small (the OS may round it up; Linux doubles it). Measured
        // with the scale harness: a 64 KiB/s viewer's first echo after attaching to output floods fell
        // from 5.6 s with the 208 KiB default to 0.5-0.65 s; much smaller buffers slow such a reader
        // down. Draining viewers' throughput is unchanged. Best effort: failure keeps the OS default.
        let _ = socket2::SockRef::from(&stream).set_send_buffer_size(CONNECTION_SEND_BUFFER);
        let id = next.fetch_add(1, Ordering::Relaxed);
        let (r, w) = stream.into_split();
        let (peer, incoming) = Peer::spawn_serving(r, w);
        let mutations=Arc::new(tokio::sync::Semaphore::new(MUTATION_SLOTS));
        peers.insert(id,(peer.clone(),mutations.clone()));
        let conn = Arc::new(Conn {
            id,
            actual_author:format!("client:{}:{id}",core.lock().unwrap().epoch),
            principal:Mutex::new(None),
            wire: crate::provider_wire::Connection::new(id, peer.clone()),
            peer,
            author: Mutex::new(format!("client:{id}")),
            extension: Mutex::new(None),
            supervised: Mutex::new(None),
            streams: Mutex::new(HashMap::new()),
            next_stream: AtomicU64::new(1),
            subscribed: Mutex::new(false),
            requests: Arc::new(tokio::sync::Semaphore::new(crate::rpc::MAX_PENDING_CALLS)),
            control_requests:Arc::new(tokio::sync::Semaphore::new(crate::commit::CONTROL_REQUESTS)),
            mutations,
        });
        let c = core.clone();
        connections.spawn(async move { connection(c, conn, incoming).await; id });
    }
    // A listener failure also enters the same stop state as an explicit shutdown request.
    shutdown.request();
    // Keep the endpoint bound until cleanup finishes, so a replacement cannot race our unlink.
    let commits=crate::commit::shutdown(&core,std::time::Duration::from_secs(3)).await;
    if !commits.completed { eprintln!("vqro: workspace shutdown deadline: {} queued commits rejected without effects; {} active outcomes indeterminate; do not replay",commits.rejected,commits.active); }
    // A worker receipt is not yet a transport reply. Mutation handlers retain these permits
    // through reply flushing, including replies issued on provider/extension connections.
    let replies=tokio::time::timeout(std::time::Duration::from_secs(1),async {
        let mut held=Vec::new();
        for (_,mutations) in peers.values() {
            held.push(mutations.clone().acquire_many_owned(MUTATION_SLOTS as u32).await.map_err(|_|())?);
        }
        Ok::<_,()>(held)
    }).await;
    if !matches!(replies,Ok(Ok(_))) { eprintln!("vqro: workspace reply delivery indeterminate at shutdown deadline; inspect history, never replay"); }
    ext::shutdown(&core).await;
    let handles: Vec<_> = core.lock().unwrap().providers.values().map(|p| p.handle.clone()).collect();
    let mut providers = tokio::task::JoinSet::new();
    for handle in handles { providers.spawn(async move { (handle.name.clone(), handle.shutdown().await) }); }
    if tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while let Some(result) = providers.join_next().await {
            if let Ok((name, Err(error))) = result { eprintln!("vqro: provider {name} shutdown unconfirmed: {error}"); }
        }
    }).await.is_err() { eprintln!("vqro: provider shutdown deadline; cleanup unconfirmed"); providers.abort_all(); }
    { let mut c = core.lock().unwrap(); let names: Vec<_> = c.providers.keys().cloned().collect();
      for name in names { c.on_adapter_event(&name, crate::adapter::AdapterEvent::Connected(false, None)); } }
    for (peer,_) in peers.values() { peer.close(); }
    if tokio::time::timeout(std::time::Duration::from_secs(1), async { while connections.join_next().await.is_some() {} }).await.is_err() { connections.abort_all(); }
    let socket = core.lock().unwrap().socket.clone();
    let _ = std::fs::remove_file(socket);
    drop(listener);
    { let mut c=core.lock().unwrap();c.operational.release_state();c.store.release_state();shutdown.finish(); }
    Ok(())
}

/// Signals and embedders use the same ordered stop as protocol callers.
pub async fn shutdown(core: &Shared) {
    let lifetime = core.lock().unwrap().shutdown.clone();
    lifetime.request(); lifetime.stopped().await;
}

async fn connection(core: Shared, conn: Arc<Conn>, mut incoming: crate::rpc::IncomingReceiver) {
    let mut tasks=tokio::task::JoinSet::new();
    while let Some(msg) = incoming.recv().await {
        // Reap finished handlers continuously; a long-lived connection cannot accumulate task results.
        while tasks.try_join_next().is_some() {}
        match msg {
            // Provider lifecycle and fact messages stay ordered on their connection. Native
            // replies are read independently by Peer, so a pending host-token wait cannot block it.
            Incoming::Request { id, method, params } if matches!(method.as_str(), "hello" | "provider.register" | "provider.sync") => {
                let result = dispatch(&core, &conn, &method, params).await;
                conn.peer.respond(id, result);
            }
            Incoming::Notification { method, params } if conn.wire.is_provider() || matches!(method.as_str(), "resource.upsert" | "resource.gone" | "stream.data" | "stream.closed" | "stream.gap" | "stream.error") => {
                let result = if matches!(method.as_str(), "resource.upsert" | "resource.gone" | "stream.data" | "stream.closed" | "stream.gap" | "stream.error") {
                    // Notification validation includes the EOF-safe retirement
                    // path; do not reject an accepted tail before it can drain.
                    conn.wire.notification(&core, &method, params)
                } else { conn.wire.validate(&core, &params).map(|_| ()) };
                if result.is_err() { conn.peer.close(); }
            }
            // Stream input is handled in order, inline: keystrokes must not be reordered.
            Incoming::Request { id, method, params } if method.starts_with("stream.") => {
                let result = if conn.wire.is_provider() {
                    conn.wire.validate(&core, &params).and_then(|_| Err(RpcError::method_not_found(&method)))
                } else { stream_op(&core, &conn, &method, params) };
                conn.peer.respond(id, result);
            }
            Incoming::Notification { method, params } if method.starts_with("stream.") => {
                let sid = params["stream"].clone();
                if let Err(error) = stream_op(&core, &conn, &method, params) {
                    conn.peer.notify("stream.error", json!({"stream": sid, "error": error, "outcome": "rejected"}));
                }
            }
            Incoming::Request { id, method, params } => {
                let control=method=="operational.register"||(method=="operational.apply"&&params["control"]==true)||method=="core.shutdown"||(method=="command.invoke"&&matches!(params["name"].as_str(),Some("vqro.shutdown"|"vqro.safe_mode"|"vqro.layout.reset"|"vqro.revert")));
                let requests=if control {conn.control_requests.clone()}else{conn.requests.clone()};
                let Ok(permit) = requests.try_acquire_owned() else {
                    conn.peer.respond(id, Err(provider::rejected("overloaded", "connection request limit; not dispatched")));
                    continue;
                };
                let (core, conn) = (core.clone(), conn.clone());
                tasks.spawn(async move {
                    let _permit = permit;
                    let stopping = method == "core.shutdown" || (method == "command.invoke" && params["name"] == "vqro.shutdown");
                    let mutation=matches!(method.as_str(),"workspace.apply"|"workspace.revert"|"operational.register"|"operational.apply") || (method=="command.invoke" && matches!(params["name"].as_str(),Some("vqro.revert"|"vqro.layout.reset"|"vqro.extension.enable"|"vqro.extension.disable")));
                    let _mutation=if mutation {
                        if core.lock().unwrap().shutdown.is_requested() { conn.peer.respond(id,Err(provider::rejected("unavailable","core shutting down; commit not admitted"))); return; }
                        match conn.mutations.clone().try_acquire_owned() { Ok(permit)=>Some(permit),Err(_)=>{conn.peer.respond(id,Err(provider::rejected("unavailable","core shutting down; commit not admitted")));return;} }
                    } else { None };
                    let result = dispatch(&core, &conn, &method, params).await;
                    if stopping && result.is_ok() {
                        let _ = conn.peer.respond_flushed(id, result).await;
                        core.lock().unwrap().shutdown.request();
                    } else {
                        conn.peer.respond(id,result);
                        if mutation && !matches!(tokio::time::timeout(std::time::Duration::from_secs(1),conn.peer.flush_control()).await,Ok(true)) {
                            eprintln!("vqro: workspace reply delivery indeterminate; inspect history, never replay");
                        }
                    }
                });
            }
            Incoming::Notification { .. } => {}
        }
    }
    // Clean input EOF is a half-close. Finish already-read work and flush its replies before
    // revoking the writable half. Real write failure remains indeterminate, never replayed.
    while tasks.join_next().await.is_some() {}
    let _=conn.peer.flush_control().await;
    conn.peer.close();
    // Disconnected: invalidate provider routes before closing its viewer streams and commands.
    conn.wire.disconnect(&core);
    for (_, s) in conn.streams.lock().unwrap().drain() {
        let _ = StreamInput::Close.send(&s.input);
    }
    let mut c = core.lock().unwrap();
    c.clients.remove(&conn.id);
    let gone: Vec<String> = c.live.iter().filter(|(_, (_, _, id, _))| *id == conn.id).map(|(k, _)| k.clone()).collect();
    for view in gone {
        if let Some((_,_,_,bytes))=c.live.remove(&view){c.live_bytes=c.live_bytes.saturating_sub(bytes);}
        c.emit("view.content", json!({"view": view, "content": null}));
    }
    let names: Vec<String> = c.commands.values().filter(|cmd| cmd.connection == Some(conn.id)).map(|cmd| cmd.name.clone()).collect();
    for name in names {
        if let Some(mut cmd) = c.commands.remove(&name) {
            cmd.available = false;
            cmd.peer = None;
            let j = cmd.to_json();
            c.emit("command", j);
        }
    }
}

#[derive(Deserialize)]
struct Hello {
    client: Option<String>,
    extension: Option<String>,
    extension_token: Option<String>,
    version: Option<String>,
    /// Build identity (e.g. the binary's modification time), to spot out-of-date clients.
    build: Option<u64>,
    /// Client profiles (`base`, `terminal`, `keys`) and UI feature ids it renders; reported in `client.list`.
    features: Option<Vec<String>>,
    identity:Option<principal::Identity>,
    profiles: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct Apply {
    #[serde(default)] history:Option<bool>,
    ops: Vec<Op>,
    note: Option<String>,
}

#[derive(Deserialize)]
struct WorkspaceGet {
    key: String,
}

#[derive(Deserialize)]
struct ResourceList {
    provider: Option<String>,
    backend_ref: Option<String>,
    id: Option<String>,
    host: Option<String>,
}

#[derive(Deserialize)]
struct Revert {
    txn: u64,
}

#[derive(Deserialize)]
struct Spawn {
    argv: Option<Vec<String>>,
    cwd: Option<String>,
    name: Option<String>,
    #[serde(alias = "adapter")]
    provider: Option<String>,
    deadline_ms: Option<u64>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    cols: Option<u16>,
    rows: Option<u16>,
    placement: Option<Value>,
    #[serde(default)]
    bridge: bool,
}

#[derive(Deserialize)]
struct Attach {
    resource: String,
    cols: Option<u16>,
    rows: Option<u16>,
    /// Scrollback lines to replay first, if the backend can.
    history: Option<u32>,
    /// A non-terminal item stream instead of terminal output: `{after?: {epoch, seq}}`.
    items: Option<Value>,
    deadline_ms: Option<u64>,
}

#[derive(Deserialize)]
struct StreamParams {
    stream: u64,
    data: Option<String>,
    text: Option<String>,
    cols: Option<u16>,
    rows: Option<u16>,
}

#[derive(Deserialize)]
struct Register {
    name: String,
    title: Option<String>,
}

#[derive(Deserialize)]
struct Invoke {
    name: String,
    #[serde(default)]
    args: Value,
    deadline_ms: Option<u64>,
}

fn attribution(core:&Shared,conn:&Conn,params:&mut Value)->Result<crate::invocation::Context,RpcError> {
    let base=Attribution::for_actor(&conn.actor(),params)?;
    let string=|key:&str|params.get(key).map(|value|value.as_str().map(String::from).ok_or_else(||RpcError::invalid(format!("{key} must be a string")))).transpose();
    let proof=string("invocation")?;let target=string("on_behalf_of")?;
    if let Some(object)=params.as_object_mut(){object.remove("invocation");object.remove("on_behalf_of");}
    // An immutable connection identity needs no core state; only live proofs/standing delegation do.
    if proof.is_none() && target.is_none(){return Ok(crate::invocation::Context{attribution:base,..Default::default()})}
    principal::resolve(&core.lock().unwrap(),conn.id,base,proof,target)
}

async fn dispatch(core: &Shared, conn: &Arc<Conn>, method: &str, mut p: Value) -> RpcResult {
    crate::limits::json_depth(&p,crate::limits::MESSAGE_DEPTH)?;
    crate::causality::Causality::check_method(method,&p)?;
    if method=="resource.invoke" {crate::causality::Causality::lift_args(&mut p)?;}
    let context=attribution(core,conn,&mut p)?;
    if conn.peer.is_closed() { return Err(provider::rejected("disconnected", "connection closed before dispatch")); }
    if core.lock().unwrap().shutdown.is_requested() && method != "core.shutdown" && !(method == "command.invoke" && p["name"] == "vqro.shutdown") {
        return Err(provider::rejected("unavailable", "core is shutting down; not dispatched"));
    }
    let caller=conn.caller();
    if method != "hello" { caller.check(&core.lock().unwrap())?; }
    if method != "provider.register" && conn.wire.is_provider() { conn.wire.validate(core, &p)?; }
    match method {
        "core.shutdown" => Ok(json!({"outcome":"accepted"})),
        "provider.register" => {
            let owner=conn.supervised.lock().unwrap().clone();
            { let c=core.lock().unwrap(); if c.safe_mode && owner.as_ref().is_none_or(|owner| !ext::owner_valid(&c,owner) || !c.safe_mode_allow.contains(&owner.name)) {
                let mut error = provider::rejected("forbidden", "safe mode requires an explicitly allowed extension owner");
                error.data.as_mut().unwrap()["reason"] = json!("safe_mode");
                return Err(error);
            } }
            let mut reply=conn.wire.register(core,p,owner).await?;
            if conn.actor()==principal::UNDECLARED {if let Some(host)=reply["host"].as_str(){conn.bind_actor(format!("host:{host}"))?;}}
            reply["actor"]=json!(conn.actor());
            if let Some(client)=core.lock().unwrap().clients.get_mut(&conn.id){client["actor"]=json!(conn.actor());}
            Ok(reply)
        },
        "provider.sync" => conn.wire.sync(core, p),
        "hello" => {
            let h: Hello = params(p)?;
            for list in [&h.features,&h.profiles].into_iter().flatten() {
                if list.len()>256 || list.iter().any(|f|f.is_empty() || f.len()>128) {return Err(RpcError::invalid("hello features/profiles: at most 256 names of 1-128 bytes"));}
            }
            let supervised=match (h.extension.as_deref(),h.extension_token.as_deref()) {
                (Some(name),Some(token))=>Some(ext::authenticate(&core.lock().unwrap(),name,token).ok_or_else(||RpcError::new("forbidden","invalid or expired supervised extension identity"))?),
                _=>None,
            };
            let actor={
                let mut c=core.lock().unwrap();
                match principal::identity(&c.store,h.identity.as_ref(),supervised.as_ref().map(|owner|owner.name.as_str())) {
                    Ok(actor)=>actor,
                    Err(error)=>{
                        // Bounded diagnostics only: no asserted identity, token or proof is logged.
                        c.emit("identity.rejected",json!({"author":conn.actual_author,"actor":conn.actor(),"outcome":error.kind(),"error":error.message}));
                        return Err(error)
                    }
                }
            };
            conn.bind_actor(actor)?;
            if conn.supervised.lock().unwrap().is_some() && supervised.is_none(){return Err(RpcError::new("conflict","a supervised connection cannot drop its lifetime identity"));}
            *conn.supervised.lock().unwrap()=supervised.clone();
            conn.wire.hello()?;
            if let Some(owner)=supervised {
                *conn.author.lock().unwrap()=format!("ext:{}",owner.name);
                *conn.extension.lock().unwrap()=Some(owner.name);
            } else {
                *conn.author.lock().unwrap()=format!("client:{}",conn.id);
                *conn.extension.lock().unwrap()=None;
            }
            let mut c = core.lock().unwrap();
            let me = json!({"id": conn.id, "author": conn.author(), "client": h.client, "version": h.version, "build": h.build, "features": h.features, "profiles": h.profiles,"actor":conn.actor(),"label":context.attribution.label,"actual_author":conn.actual_author});
            c.clients.insert(conn.id, me);
            Ok(json!({
                "server": "vqro", "version": env!("CARGO_PKG_VERSION"), "protocol": PROTOCOL, "you": conn.author(),
                "provides":["ability/vqro.operational-domains@1"],
                "epoch": c.epoch, "seq": c.seq, "actual_author":conn.actual_author,"actor":conn.actor(),"principal":conn.actor(),"backend_notice":c.backend_notice(), "safe_mode": c.safe_mode, "socket": c.socket, "state_dir": c.state_dir,
            }))
        }
        "workspace.snapshot" => {
            let limited=conn.wire.is_provider();
            // Limited provider envelopes cannot consume the ordinary recovery construction pool.
            let gate={let c=core.lock().unwrap();if limited {c.provider_snapshot_slots.clone()} else {c.snapshot_slots.clone()}};
            let slot=tokio::time::timeout(std::time::Duration::from_millis(250),gate.acquire_owned()).await
                .map_err(|_|provider::rejected("overloaded","snapshot construction busy; retry recovery later"))?
                .map_err(|_|provider::rejected("unavailable","snapshot construction stopped"))?;
            let snapshot=core.lock().unwrap().capture_snapshot();
            tokio::task::spawn_blocking(move || {
                let _slot=slot;
                snapshot.into_value_limited(if limited {crate::provider_wire::MAX_RECORD_BYTES} else {crate::rpc::LARGE_REPLY_BYTES})
            }).await.map_err(|_|provider::rejected("unavailable","snapshot construction stopped"))?
        },
        "workspace.get" => {
            let query: WorkspaceGet = params(p)?;
            let entry = { core.lock().unwrap().store.entries().get(&query.key).cloned() };
            Ok(json!(entry))
        }
        "operational.register" | "operational.get" | "operational.list" | "operational.snapshot" | "operational.apply" => operational(core,conn,&caller,method,p,context.attribution).await,
        "workspace.apply" => {
            let a: Apply = params(p)?;
            if a.history==Some(false) {return Err(provider::rejected("forbidden","history:false is not a raw workspace bypass; use the owning protected operational domain"));}
            apply(core, &caller, None, a.note, a.ops, context.attribution).await
        }
        "workspace.history" => {
            let limit = p.get("limit").and_then(|l| l.as_u64()).unwrap_or(20) as usize;
            Ok(json!({"txns": core.lock().unwrap().store.history(limit)}))
        }
        "workspace.revert" => {
            let revert_request: Revert = params(p)?;
            revert(core, &caller, None, Some(revert_request.txn), context.attribution).await
        }
        "events.subscribe" => subscribe(core, conn, params(p)?),
        "resource.list" => {
            let query: ResourceList = params(p)?;
            let resources = {
                let c = core.lock().unwrap();
                c.resources.values().filter(|r| query.provider.as_deref().is_none_or(|name| r.adapter == name)
                    && query.backend_ref.as_deref().is_none_or(|reference| r.facts.backend_ref == reference)
                    && query.id.as_deref().is_none_or(|id|r.id==id)
                    && query.host.as_deref().is_none_or(|host|r.host.as_deref()==Some(host))).cloned().collect::<Vec<_>>()
            };
            Ok(json!({"resources": resources.iter().map(|r| r.to_json()).collect::<Vec<_>>()}))
        }
        "resource.spawn" => {
            spawn(core, &caller, params(p)?, context.attribution).await
        }
        "resource.kill" => {
            let id = p.get("resource").and_then(|r| r.as_str()).ok_or_else(|| RpcError::invalid("needs `resource`"))?;
            let (route, reference) = {
                let c = core.lock().unwrap();
                (c.resource_route(id, "terminal.kill")?, c.resource(id)?.facts.backend_ref.clone())
            };
            let (handle, operation, c) = (route.handle.clone(), route.clone(), core.clone());
            let deadline = provider::deadline(p.get("deadline_ms").map(|v| v.as_u64().ok_or_else(|| RpcError::invalid("deadline_ms must be an unsigned integer"))).transpose()?)?;
            let (effect, mut result) = effect(core, caller.clone(), context.attribution, route, "resource.kill", json!({"resource":id,"op":"terminal.kill"}), deadline, |value: &crate::adapter::EffectResult| value.outcome, move |context| async move {
                let mut result = handle.for_effect(context).kill(reference).await.map_err(|e| e.rpc())?;
                let mut c=c.lock().unwrap();
                if !operation.valid() { return Err(provider::indeterminate("provider changed before kill result was recorded")); }
                if let Some(facts) = result.facts.take() { c.upsert_resource(&operation.handle.name, facts)?; }
                Ok(result)
            }).await?;
            Ok(json!({"outcome": result.outcome, "effect": effect, "backend": result.data.take()}))
        }
        "resource.forget" => forget(core, &caller, p,context.attribution).await,
        "resource.forget_cached" => forget_cached(core, &caller, p,context.attribution),
        "resource.attach" => attach(core, conn, params(p)?,context.attribution).await,
        "resource.invoke" => invoke_resource(core, &caller, p,context.attribution).await,
        "command.register" => {
            let r: Register = params(p)?;
            if r.name.starts_with("vqro.") {
                return Err(RpcError::new("forbidden", "`vqro.` commands belong to the core"));
            }
            let mut c = core.lock().unwrap();
            caller.check(&c)?;
            if let Some(existing) = c.commands.get(&r.name) {
                if existing.available && existing.owner != conn.author() {
                    return Err(RpcError::new("conflict", format!("{} is owned by {}", r.name, existing.owner)));
                }
            }
            let info = CommandInfo { name: r.name.clone(), title: r.title.unwrap_or_else(|| r.name.clone()), owner: conn.author(), connection: Some(conn.id), available: true, peer: Some(conn.peer.clone()), lifetime:caller.owner.clone() };
            let j = info.to_json();
            let bytes=crate::limits::encoded_bytes(&j);
            if bytes.saturating_add(2)>crate::limits::COMMAND_BYTES {return Err(crate::limits::too_large("command metadata",bytes.saturating_add(2),crate::limits::COMMAND_BYTES));}
            // Registration is infrequent; count the bounded live registry rather than maintain a
            // second mutable index. Replacing this name releases its old encoded charge atomically.
            let retained=c.commands.values().filter(|cmd|cmd.name!=info.name).fold(2usize,|sum,cmd|sum.saturating_add(crate::limits::encoded_bytes(&cmd.to_json())).saturating_add(1));
            if retained.saturating_add(bytes)>crate::limits::COMMAND_BYTES {return Err(provider::rejected("overloaded","command metadata budget is full; registration not applied"));}
            c.commands.insert(r.name, info);
            c.emit("command", j.clone());
            Ok(j)
        }
        "command.list" => Ok(json!({"commands": core.lock().unwrap().commands.values().map(|c| c.to_json()).collect::<Vec<_>>()})),
        "command.invoke" => {
            let i: Invoke = params(p)?;
            invoke_deadline(core, &caller, context, &i.name, i.args, i.deadline_ms).await
        }
        "ui.notify" => {
            // A transient message for every client (toast). Not state.
            let text = p.get("text").and_then(|t| t.as_str()).ok_or_else(|| RpcError::invalid("needs `text`"))?.to_string();
            if let Some(style)=p.get("style") {crate::limits::json_depth(style,crate::limits::JSON_DEPTH)?;}
            let mut c = core.lock().unwrap();
            caller.check(&c)?;
            c.emit("notify", json!({"text": text, "style": p.get("style"), "author": caller}));
            Ok(json!({"outcome": "dispatched"}))
        }
        "view.publish" => {
            let view = p.get("view").and_then(|v| v.as_str()).ok_or_else(|| RpcError::invalid("needs `view`"))?.to_string();
            let content = p.get("content").cloned().unwrap_or(Value::Null);
            if !content.is_null() && !content.is_object() {
                return Err(RpcError::invalid("`content` is an object, or null to clear"));
            }
            crate::limits::json_depth(&content,crate::limits::JSON_DEPTH)?;
            let next = if content.is_null() { 0 } else { crate::limits::encoded_bytes(&(&view, &content)) };
            let author=conn.author();
            let mut c = core.lock().unwrap();
            caller.check(&c)?;
            if let Some((_,owner,connection,_))=c.live.get(&view){if owner!=&author && c.clients.contains_key(connection){return Err(RpcError::new("conflict",format!("view {view} is owned by {owner}")));}}
            let old=c.live.get(&view).map(|(_,_,_,bytes)|*bytes).unwrap_or(0);
            if next > crate::limits::LIVE_VALUE_BYTES && next > old {
                return Err(crate::limits::too_large("live view", next, crate::limits::LIVE_VALUE_BYTES));
            }
            let total=c.live_bytes;
            let candidate = total.saturating_sub(old).saturating_add(next);
            if candidate > crate::limits::LIVE_BYTES && candidate > total {
                return Err(crate::limits::too_large("live content", candidate, crate::limits::LIVE_BYTES));
            }
            let unchanged = c.live.get(&view).map(|(v, _, _, _)| v.as_ref() == &content).unwrap_or(content.is_null());
            if content.is_null() {
                c.live.remove(&view);
            } else {
                c.live.insert(view.clone(), (Arc::new(content.clone()), author, conn.id,next));
            }
            c.live_bytes=candidate;
            if !unchanged {
                c.emit("view.content", json!({"view": view, "content": content, "author": conn.author()}));
            }
            Ok(json!({"outcome": "accepted"}))
        }
        "client.list" => Ok(json!({"clients": core.lock().unwrap().clients.values().cloned().collect::<Vec<_>>()})),
        "extension.list" => {
            let mut c = core.lock().unwrap();
            let specs=c.store.entries().iter().filter_map(|(key,e)|Some((key.strip_prefix("extension/")?.to_string(),e.version,json!({"manifest":e.value["manifest"]})))).collect::<Vec<_>>();
            c.manifest_catalog.refresh(&specs);
            Ok(json!({"extensions": c.extension_states()}))
        }
        _ => Err(RpcError::method_not_found(method)),
    }
}

fn stream_op(core:&Shared, conn: &Arc<Conn>, method: &str, mut p: Value) -> RpcResult {
    crate::causality::Causality::check_method(method,&p)?;
    let _declared=Attribution::for_actor(&conn.actor(),&mut p)?;
    let c=core.lock().unwrap();conn.caller().check(&c)?;
    let s: StreamParams = params(p)?;
    let streams = conn.streams.lock().unwrap();
    let binding = streams.get(&s.stream).ok_or_else(|| RpcError::new("not_found", format!("no stream {}", s.stream)))?;
    if !binding.route.valid() { return Err(provider::rejected("unavailable", "stream provider or resource is unavailable")); }
    let tx = &binding.input;
    let input = match method {
        "stream.input" => {
            if !binding.route.handle.capabilities.get("terminal.input").is_some_and(Value::is_object) || binding.route.handle.capabilities["terminal.input"]["stream"] == false {
                return Err(provider::rejected("unsupported", "provider does not declare terminal.input"));
            }
            let bytes = match (s.data, s.text) {
                (Some(d), _) => B64.decode(d).map_err(|_| RpcError::invalid("`data` must be base64"))?,
                (None, Some(t)) => t.into_bytes(),
                _ => return Err(RpcError::invalid("needs `data` (base64) or `text`")),
            };
            if bytes.len() > crate::adapter::MAX_STREAM_CHUNK {
                return Err(RpcError::invalid("stream.input exceeds 64 KiB; input was not dispatched"));
            }
            StreamInput::Data(bytes)
        }
        "stream.resize" => {
            if !binding.route.handle.capabilities.get("terminal.resize").is_some_and(Value::is_object) {
                return Err(provider::rejected("unsupported", "provider does not declare terminal.resize"));
            }
            let (cols, rows) = (s.cols.unwrap_or(80), s.rows.unwrap_or(24));
            if !(1..=9999).contains(&cols) || !(1..=9999).contains(&rows) { return Err(provider::rejected("invalid", "stream dimensions must be between 1 and 9999")); }
            StreamInput::Resize(cols, rows)
        }
        "stream.close" => StreamInput::Close,
        _ => return Err(RpcError::method_not_found(method)),
    };
    input.send(tx).map_err(|e| RpcError::new("overloaded", format!("stream input not dispatched: {e}")))?;
    Ok(json!({"outcome": "dispatched"}))
}

fn op_key(op: &Op) -> &str {
    match op {
        Op::Set { key, .. } | Op::Delete { key, .. } => key,
    }
}

fn subscribe(core: &Shared, conn: &Arc<Conn>, opts: crate::events::SubscribeOptions) -> RpcResult {
    let (from, epoch) = (opts.from, opts.epoch.as_deref());
    let mut filter = crate::events::EventFilter::new(&opts);
    let mut subscribed = conn.subscribed.lock().unwrap();
    if *subscribed {
        return Err(RpcError::new("conflict", "already subscribed"));
    }
    *subscribed = true;
    let (backlog, mut rx, seq, epoch) = {
        let c = core.lock().unwrap();
        // A sequence without its epoch cannot identify a log position after restart.
        let backlog = match from {
            Some(from) if epoch == Some(c.epoch.as_str()) => c.events_since(from),
            Some(_) => None,
            None => Some(Vec::new()),
        };
        (backlog, c.events.subscribe(), c.seq, c.epoch.clone())
    };
    let peer = conn.peer.clone();
    match backlog {
        None => peer.notify("missed", json!({"epoch": epoch, "seq": seq})),
        Some(evs) => {
            let shaped: Vec<_> = evs.into_iter().filter_map(|e| filter.shape(e)).collect();
            let bytes: usize = shaped.iter().map(|e| crate::limits::encoded_bytes(e).saturating_add(64)).sum();
            if bytes > crate::rpc::CONTROL_BYTES - 4096 {
                filter.reset();
                peer.notify("missed", json!({"epoch":epoch,"seq":seq,"reason":"backlog_budget"}));
            } else { for event in shaped { peer.notify("event", event); } }
        },
    }
    let response = json!({"epoch": epoch, "seq": seq});
    tokio::spawn(async move {
        loop {
            let received=tokio::select! {biased; _=peer.closed()=>break, received=rx.recv()=>received};
            match received {
                // The receiver was created under the same lock as `seq`, so everything here is newer.
                Ok((_, ev)) => {
                    if let Some(ev) = filter.shape(ev) {
                        peer.notify("event", ev);
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    // Too slow: the client must resnapshot.
                    filter.reset();
                    peer.notify("missed", json!({"epoch": epoch}));
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
            if peer.is_closed() {
                break;
            }
        }
    });
    Ok(response)
}

/// Audit the caller's observed outcome. A timeout drops the reply receiver, never the backend
/// effect or its independent fact feed. Late replies are ignored, as pinned provider v0 requires.
async fn effect<T: Send + 'static, F>(
    core: &Shared, author: Caller, attribution:Attribution, route: Route, method: &'static str, detail:Value, deadline: std::time::Duration, success: fn(&T) -> &'static str,
    work: impl FnOnce(crate::adapter::EffectContext) -> F,
) -> Result<(String, T), RpcError>
where F: std::future::Future<Output = Result<T, RpcError>> + Send + 'static {
    if !route.valid() { return Err(provider::rejected("unavailable", "provider changed before dispatch")); }
    let id = { let mut c=core.lock().unwrap(); author.check(&c)?; c.effect_id() };
    let context = crate::adapter::EffectContext { effect: id.clone(), deadline: std::time::Instant::now() + deadline, target: route.target.clone(),caller:author.owner.clone(),author:principal::CallAuthor{author:author.to_string(),attribution:attribution.clone().recorded()} };
    let result = crate::rpc::with_deadline(method, deadline, route.guard(work(context))).await;
    let outcome = match &result {
        Ok(value) => success(value),
        Err(error) => error.data.as_ref().and_then(|d| d["outcome"].as_str()).unwrap_or("indeterminate"),
    };
    let mut audit=json!({"effect": id, "provider": route.handle.name, "generation": route.generation, "method": method, "author": author,"actor":attribution.actor,"principal":attribution.principal,"initiator":attribution.initiator,"label":attribution.label, "resource":detail["resource"], "op":detail["op"], "target":route.target.as_ref().map(|(reference,incarnation)|json!({"ref":reference,"incarnation":incarnation})), "outcome": outcome, "error": result.as_ref().err()});
    attribution.causality.annotate(&mut audit);
    core.lock().unwrap().emit("effect",audit);
    result.map(|value| (id.clone(), value)).map_err(|mut error| {
        if let Some(data) = error.data.as_mut() { data["effect"] = json!(id); }
        error
    })
}

async fn spawn(core: &Shared, author: &Caller, s: Spawn, attribution:Attribution) -> RpcResult {
    let deadline = provider::deadline(s.deadline_ms)?;
    let route = core.lock().unwrap().provider_route(s.provider.as_deref(), "terminal.spawn")?;
    let argv = s.argv.unwrap_or_default();
    if (s.bridge || !s.env.is_empty()) && route.handle.capabilities["terminal.spawn"]["env"] == false {
        return Err(provider::rejected("unsupported", "provider explicitly does not support spawn environment"));
    }
    let reservation = if s.bridge { Some(crate::provider_wire::BridgeReservation::new(core)?) } else { None };
    let mut env = s.env;
    if route.handle.capabilities["terminal.spawn"]["env"]!=false && !env.contains_key("VQRO_PRINCIPAL") {
        env.insert("VQRO_PRINCIPAL".into(),attribution.effective().unwrap_or(principal::UNDECLARED).into());
    }
    // Every terminal can reach vqro (`vqro call` from any pane): its opaque endpoint (D22), unless the
    // provider cannot pass an environment at all. The bridge credential stays opt-in.
    if route.handle.capabilities["terminal.spawn"]["env"] != false && !env.contains_key("VQRO_SOCKET") {
        env.insert("VQRO_SOCKET".into(),core.lock().unwrap().socket.to_string_lossy().into_owned());
        env.insert("VQRO_SOCKET_SOURCE".into(),"terminal".into());
    }
    if let Some(reservation) = &reservation {
        env.insert("VQRO_BRIDGE_TOKEN".into(), reservation.token.clone());
        env.insert("VQRO_SOCKET".into(), core.lock().unwrap().socket.to_string_lossy().into_owned());
        env.insert("VQRO_SOCKET_SOURCE".into(), "terminal".into());
    }
    for dimension in [s.cols, s.rows].into_iter().flatten() {
        if !(1..=9999).contains(&dimension) { return Err(provider::rejected("invalid", "spawn dimensions must be between 1 and 9999")); }
    }
    let (c, operation) = (core.clone(), route.clone());
    let (effect, id) = effect(core, author.into(), attribution, route, "resource.spawn", json!({"op":"terminal.spawn"}), deadline, |_| "observed", move |context| async move {
        let facts = operation.handle.for_effect(context).spawn(SpawnRequest { argv, cwd: s.cwd, name: s.name, env, cols: s.cols, rows: s.rows, placement: s.placement }).await.map_err(|e| e.rpc())?;
        let id = {
            let mut c = c.lock().unwrap();
            if !operation.valid() { return Err(provider::indeterminate("provider changed before spawn result was recorded")); }
            c.record_spawn(&operation.handle.name, facts)?
        };
        if let Some(reservation) = reservation { reservation.bind(&id)?; }
        Ok(id)
    }).await?;
    Ok(json!({"outcome": "observed", "effect": effect, "resource": core.lock().unwrap().resource(&id)?.to_json()}))
}

async fn invoke_resource(core: &Shared, author: &Caller, p: Value, attribution:Attribution) -> RpcResult {
    let id = p["resource"].as_str().ok_or_else(|| RpcError::invalid("needs resource"))?;
    let op = p["op"].as_str().ok_or_else(|| RpcError::invalid("needs op"))?.to_string();
    if op == "terminal.spawn" { return Err(provider::rejected("invalid", "terminal.spawn has no target; use resource.spawn")); }
    if op == "resource.forget" { return Err(provider::rejected("invalid", "use resource.forget (it checks the resource finished and retires its id)")); }
    let args = p.get("args").cloned().unwrap_or_else(|| json!({}));
    if !args.is_object() { return Err(RpcError::invalid("args must be an object")); }
    let route = core.lock().unwrap().resource_route(id, &op)?;
    if route.handle.capabilities[&op]["effect"] == false {
        return Err(provider::rejected("unsupported", format!("provider declares {op} unavailable through effects")));
    }
    let (reference, incarnation) = route.target.clone().unwrap();
    let (handle, operation, c) = (route.handle.clone(), route.clone(), core.clone());
    let deadline = provider::deadline(p.get("deadline_ms").map(|v| v.as_u64().ok_or_else(|| RpcError::invalid("deadline_ms must be an unsigned integer"))).transpose()?)?;
    let (effect, result) = effect(core, author.into(), attribution, route, "resource.invoke", json!({"resource":id,"op":op}), deadline, |value: &crate::adapter::EffectResult| value.outcome, move |context| async move {
        let mut result = handle.for_effect(context).invoke(op, reference, incarnation, args).await.map_err(|e| e.rpc())?;
        let mut c=c.lock().unwrap();
        if !operation.valid() { return Err(provider::indeterminate("provider changed before effect result was recorded")); }
        if let Some(facts) = result.facts.take() { c.upsert_resource(&operation.handle.name, facts)?; }
        Ok(result)
    }).await?;
    Ok(json!({"outcome": result.outcome, "effect": effect, "data": result.data}))
}

/// Ask the owning backend to release a finished resource, then retire its id (never reused).
fn forget_cached(core:&Shared,author:&Caller,p:Value,attribution:Attribution)->RpcResult {
    let id=p["resource"].as_str().ok_or_else(||RpcError::invalid("needs `resource`"))?;
    let mut c=core.lock().unwrap();author.check(&c)?;let effect=c.effect_id();let result=c.forget_cached_resource(id);
    let outcome=if result.is_ok(){"observed"}else{result.as_ref().err().and_then(|e|e.data.as_ref()).and_then(|d|d["outcome"].as_str()).unwrap_or("indeterminate")};
    let mut audit=json!({"effect":effect,"provider":null,"generation":null,"method":"resource.forget_cached","resource":id,"op":"resource.forget_cached","author":author,"actor":attribution.actor,"principal":attribution.principal,"initiator":attribution.initiator,"label":attribution.label,"scope":"cached_facts","outcome":outcome,"error":result.as_ref().err()});
    attribution.causality.annotate(&mut audit);c.emit("effect",audit);
    result.map_err(|mut error| {if let Some(data)=error.data.as_mut(){data["effect"]=json!(effect);data["scope"]=json!("cached_facts");}error})?;
    Ok(json!({"outcome":"observed","effect":effect,"data":{"scope":"cached_facts","backend_effect":"none"}}))
}

/// Live resources are refused: ending a program is `resource.kill`. Mechanism only: when to
/// forget is extension policy (`extensions/retention.py`).
async fn forget(core: &Shared, author: &Caller, p: Value, attribution:Attribution) -> RpcResult {
    let id = p["resource"].as_str().ok_or_else(|| RpcError::invalid("needs `resource`"))?;
    let deadline = provider::deadline(p.get("deadline_ms").map(|v| v.as_u64().ok_or_else(|| RpcError::invalid("deadline_ms must be an unsigned integer"))).transpose()?)?;
    let mut route = {
        let c = core.lock().unwrap();
        if c.resource(id)?.facts.state != "finished" {
            return Err(provider::rejected("invalid", format!("resource {id} is live; end it with resource.kill first")));
        }
        c.resource_route(id, "resource.forget")?
    };
    // Retirement is this effect's purpose: a `resource.gone` that overtakes the reply must not
    // turn success into an indeterminate outcome. Provider generation fencing still applies.
    route.resource = None;
    let (reference, incarnation) = route.target.clone().unwrap();
    let (handle, operation, c) = (route.handle.clone(), route.clone(), core.clone());
    let (effect, result) = effect(core, author.into(), attribution, route, "resource.forget", json!({"resource":id,"op":"resource.forget"}), deadline, |value: &crate::adapter::EffectResult| value.outcome, move |context| async move {
        let result = handle.for_effect(context).invoke("resource.forget".into(), reference.clone(), incarnation.clone(), json!({})).await.map_err(|e| e.rpc())?;
        let mut c=c.lock().unwrap();
        if !operation.valid() { return Err(provider::indeterminate("provider changed before forget result was recorded")); }
        if result.outcome == "observed" {
            c.on_adapter_event(&operation.handle.name, crate::adapter::AdapterEvent::Removed { reference, incarnation, reason:json!({"reason":"forgotten"}) });
        }
        Ok(result)
    }).await?;
    Ok(json!({"outcome": result.outcome, "effect": effect, "data": result.data}))
}

async fn attach(core: &Shared, conn: &Arc<Conn>, a: Attach, attribution:Attribution) -> RpcResult {
    let (cols, rows) = (a.cols.unwrap_or(80), a.rows.unwrap_or(24));
    if a.items.is_none() && (!(1..=9999).contains(&cols) || !(1..=9999).contains(&rows)) { return Err(provider::rejected("invalid", "stream dimensions must be between 1 and 9999")); }
    if a.items.is_some() && (a.cols.is_some() || a.rows.is_some() || a.history.is_some()) {
        return Err(provider::rejected("invalid", "items streams take no cols, rows or history"));
    }
    if let Some(items) = &a.items {
        // `{after?: {epoch, seq}}`: an exact cursor or nothing (agent-v0 §4).
        let after = &items["after"];
        let ok = items.is_object() && items.as_object().unwrap().keys().all(|k| k == "after")
            && (after.is_null() || (after["epoch"].as_str().is_some_and(|e| !e.is_empty() && e.len() <= 256) && after["seq"].is_u64() && after.as_object().unwrap().len() == 2));
        if !ok { return Err(provider::rejected("invalid", "items must be {after?: {epoch, seq}}")); }
    }
    let capability = if a.items.is_some() { "items.stream" } else { "terminal.output" };
    let (route, reference) = {
        let c = core.lock().unwrap();
        (c.resource_route(&a.resource, capability)?, c.resource(&a.resource)?.facts.backend_ref.clone())
    };
    let handle = route.handle.clone();
    let items = a.items.clone();
    let (effect, mut stream) = effect(core, conn.caller(), attribution, route.clone(), "resource.attach", json!({"resource":a.resource,"op":capability}), provider::deadline(a.deadline_ms)?, |_| "accepted", move |context| async move {
        match items {
            Some(items) => handle.for_effect(context).attach_items(reference, items).await.map_err(|e| e.rpc()),
            None => handle.for_effect(context).attach(reference, cols, rows, a.history.unwrap_or(0)).await.map_err(|e| e.rpc()),
        }
    }).await?;
    let epoch = stream.epoch.take();
    if !route.valid() { return Err(provider::indeterminate("provider changed before stream registration")); }
    let initial = stream.initial;
    let sid = conn.next_stream.fetch_add(1, Ordering::Relaxed);
    {
        let c=core.lock().unwrap();
        if !route.valid() || conn.caller().check(&c).is_err() {
            let _=StreamInput::Close.send(&stream.input);
            return Err(provider::indeterminate("provider or supervised caller retired before stream registration"));
        }
        let mut streams = conn.streams.lock().unwrap();
        // Publish under the same lock as disconnect cleanup. A late idle attachment must never
        // become invisible to cleanup, nor wait for backend output before noticing the viewer died.
        if conn.peer.is_closed() {
            let _ = StreamInput::Close.send(&stream.input);
            return Err(provider::indeterminate("viewer disconnected before stream registration"));
        }
        streams.insert(sid, StreamBinding { input: stream.input.clone(), route: route.clone() });
    }
    let peer = conn.peer.clone();
    let writer = peer.stream_writer(sid);
    let conn2 = conn.clone();
    tokio::spawn(async move {
        let mut reason = json!({"reason": "slow_consumer"});
        let mut normal_close = false;
        let mut gap = false;
        'output: loop {
            let out = tokio::select! {
                biased;
                _ = peer.closed() => { reason = json!({"reason": "viewer_disconnected"}); break; }
                _ = route.ended() => { reason = json!({"reason": "provider_disconnected"}); break; }
                out = stream.output.recv() => out,
            };
            let Some(out) = out else {
                if let Some(terminal) = stream.terminal.take() {
                    match terminal.await {
                        Ok(end) => { gap = end.gap; reason = end.reason; normal_close = true; }
                        Err(_) => { reason = json!({"reason": "provider_disconnected"}); }
                    }
                }
                break;
            };
            match out.value {
                StreamOutput::Error(error) => peer.notify("stream.error", json!({"stream": sid, "error": error, "outcome": "rejected"})),
                StreamOutput::Data(d) => {
                    for chunk in d.chunks(crate::adapter::MAX_STREAM_CHUNK) {
                        if !writer.send(B64.encode(chunk)) { break 'output; }
                    }
                    // Credit follows acceptance by a bounded downstream buffer, never an
                    // unbounded adapter enqueue. The backend still owns its native history.
                    if StreamInput::Consumed(d.len() as u32).send(&stream.input).is_err() { break; }
                }
                StreamOutput::Closed(why) => {
                    reason = if why.get("reason").and_then(Value::as_str).is_some() { why } else { json!({"reason": "closed", "backend": why}) };
                    normal_close = true;
                    break;
                }
            }
            if peer.is_closed() { break; }
        }
        let _ = StreamInput::Close.send(&stream.input);
        if normal_close {
            if gap { writer.finish_as("stream.gap", json!({"stream": sid, "reason": reason})); }
            else { writer.finish(reason); }
        } else { writer.close(reason); }
        conn2.streams.lock().unwrap().remove(&sid);
    });
    let mut reply = json!({"outcome": "accepted", "effect": effect, "initial": initial, "stream": sid, "resource": a.resource});
    if let Some(epoch) = epoch { reply["epoch"] = json!(epoch); }
    Ok(reply)
}

fn undo_target(core:&Core,txn:Option<u64>,principal:Option<&str>)->Option<u64> {
    txn.or_else(||principal.and_then(|principal|core.store.latest_txn_matching(|txn|txn.attribution.effective()==Some(principal))))
}
async fn revert(core: &Shared, author: &Caller, on_behalf_of: Option<Arc<str>>, txn: Option<u64>, attribution:Attribution) -> RpcResult {
    let owner=author.owner.clone();
    if txn.is_none() && attribution.effective().is_none(){return Err(RpcError::invalid("default undo needs a connection-known principal; explicit transaction revert remains available"));}
    let author = author.to_string();
    // Reserve the currently known undo payload, without cloning it. A preceding queued
    // commit may change default undo; the worker re-resolves and re-accounts before dispatch.
    let known_bytes={let core=core.lock().unwrap();undo_target(&core,txn,attribution.effective()).and_then(|id|core.store.revert_bytes(id)).unwrap_or(0)};
    let bytes = known_bytes.saturating_add(crate::limits::encoded_bytes(&(&author, &attribution, on_behalf_of.as_deref()))).saturating_add(128);
    crate::commit::submit(core, bytes, move |c| {
        ext::check_owner(c,owner.as_ref())?;
        // Preserve the presser's scope, including command work started on their behalf.
        // Select only after preceding queued commits have been adopted.
        let attribution=principal::revalidate(c,&attribution)?;
        let id = undo_target(c,txn,attribution.effective())
            .ok_or_else(|| RpcError::new("not_found", "no transactions to revert"))?;
        principal::validate_revert(&c.store,&attribution,id)?;
        let touched_ext = c.store.transaction(id).is_some_and(|t| t.changes.iter().any(|ch| ch.key.starts_with("extension/")));
        Ok(crate::commit::Plan { owner, action:crate::commit::Action::Revert { author, attribution, on_behalf_of, txn:id }, after_adopt:if touched_ext { Some(ext::reconcile) } else { None } })
    }).await
}

#[derive(Deserialize)]
struct OperationalRegister {name:String,bytes:Option<usize>}
#[derive(Deserialize)]
struct OperationalApply {domain:String,ops:Vec<Op>,note:Option<String>,#[serde(default)] control:bool}
#[derive(Deserialize)]
struct OperationalQuery {domain:Option<String>,key:Option<String>,after:Option<String>,limit:Option<usize>,revision:Option<u64>}

async fn operational(core:&Shared,conn:&Arc<Conn>,caller:&Caller,method:&str,p:Value,attribution:Attribution)->RpcResult {
    use crate::operational as op;
    if matches!(method,"operational.register"|"operational.apply") {
        if crate::limits::encoded_bytes(&p)>op::PAGE_BYTES {return Err(crate::limits::too_large("operational request",crate::limits::encoded_bytes(&p),op::PAGE_BYTES));}
        let owner=caller.owner.clone().ok_or_else(||provider::rejected("forbidden","operational writes require a current supervised extension owner"))?;
        let(name,ops,note,control,declared_bytes)=if method=="operational.register" {
            let r:OperationalRegister=params(p)?;(r.name,None,None,true,r.bytes)
        } else {let r:OperationalApply=params(p)?;(r.domain,Some(r.ops),r.note,r.control,None)};
        let author=caller.author.clone();let origin:Option<Arc<str>>=None;
        let bytes=crate::limits::encoded_bytes(&ops).saturating_add(crate::limits::encoded_bytes(&(&author,&attribution,&origin,&note,&name)));
        let prepare=move|c:&Core| {
            let attribution=principal::revalidate(c,&attribution)?;
            if ops.is_some() {c.operational.get(&name)?;}
            let domain=c.operational.prepare(&name,&owner.name,declared_bytes)?;
            Ok(crate::commit::Plan {owner:Some(owner),action:crate::commit::Action::Operational {domain:Box::new(domain),author,attribution,on_behalf_of:origin,note,ops,control},after_adopt:None})
        };
        return crate::commit::submit_operational(core,bytes,control,prepare).await;
    }
    let q:OperationalQuery=params(p)?;
    if q.domain.as_ref().is_some_and(|name|!op::valid_name(name))||q.key.as_ref().is_some_and(|key|key.len()>512)||q.after.as_ref().is_some_and(|key|key.len()>512) {return Err(RpcError::invalid("operational read name/key/cursor exceeds its bound"));}
    let(mut metadata,entries,last,slots)={
    let c=core.lock().unwrap();
    if method=="operational.list"&&q.domain.is_none() {let mut reply=c.operational.summaries();reply["epoch"]=json!(c.epoch);reply["seq"]=json!(c.seq);return Ok(reply);}
    let name=q.domain.ok_or_else(||RpcError::invalid("needs domain"))?;let domain=c.operational.get(&name)?;
    if q.revision.is_some_and(|revision|revision!=domain.revision) {return Err(provider::rejected("conflict","operational read fence changed; discard partial pages"));}
    let mut metadata=domain.metadata();metadata["epoch"]=json!(c.epoch);metadata["seq"]=json!(c.seq);
    if method=="operational.get" {
        let key=q.key.ok_or_else(||RpcError::invalid("needs key"))?;
        if !key.starts_with(&op::prefix(&name)) {return Err(provider::rejected("forbidden","key is outside the operational domain"));}
        let entry=domain.entries.get(&key).cloned();drop(c);metadata["key"]=json!(key);metadata["entry"]=json!(entry);return Ok(metadata);
    }
    // Public operational reads preserve the same reserved recovery-pool boundary as workspace snapshots.
    let slots=if conn.wire.is_provider(){c.provider_snapshot_slots.clone()}else{c.snapshot_slots.clone()};
    (metadata,domain.entries.clone(),domain.last.clone(),slots)
    };
    let slot=tokio::time::timeout(std::time::Duration::from_millis(250),slots.acquire_owned()).await.map_err(|_|provider::rejected("overloaded","operational snapshot construction busy"))?.map_err(|_|provider::rejected("unavailable","snapshot construction stopped"))?;
    let snapshot=method=="operational.snapshot";
    tokio::task::spawn_blocking(move|| {
        let _slot=slot;
        if snapshot {metadata["entries"]=json!(entries);metadata["last"]=json!(last);return Ok(metadata);}
        let limit=q.limit.unwrap_or(op::PAGE_ITEMS);
        if limit==0||limit>op::PAGE_ITEMS {return Err(RpcError::invalid("operational page limit is1..64"));}
        let mut page=Vec::new();let mut bytes=0usize;let mut more=false;let after=q.after.unwrap_or_default();
        for(key,entry)in entries.range(after.clone()..) {
            if key<=&after {continue;}
            let size=crate::limits::encoded_bytes(&(key,entry));
            if page.len()>=limit||bytes+size>op::PAGE_BYTES-4096 {more=true;break;}
            bytes+=size;page.push((key.clone(),entry.clone()));
        }
        metadata["next"]=if more {page.last().map(|(key,_)|json!(key)).unwrap_or(Value::Null)}else{Value::Null};metadata["entries"]=json!(page.into_iter().map(|(key,entry)|json!({"key":key,"entry":entry})).collect::<Vec<_>>());
        if crate::limits::encoded_bytes(&metadata)>op::PAGE_BYTES {return Err(provider::rejected("too_large","operational page exceeds its bounded reply"));}
        Ok(metadata)
    }).await.map_err(|_|provider::rejected("unavailable","operational snapshot construction failed"))?
}

async fn apply_prepared(
    core:&Shared, author:&Caller, on_behalf_of:Option<Arc<str>>, note:Option<String>, bytes:usize, attribution:Attribution,
    ops:impl FnOnce(&Core)->Result<Vec<Op>,RpcError> + Send + 'static,
) -> RpcResult {
    crate::store::validate_note(note.as_deref())?;
    let bytes = bytes.saturating_add(crate::limits::encoded_bytes(&(author, &attribution, on_behalf_of.as_deref(), &note)));
    let owner=author.owner.clone();
    let author = author.to_string();
    crate::commit::submit(core, bytes, move |c| {
        ext::check_owner(c,owner.as_ref())?;
        let attribution=principal::revalidate(c,&attribution)?;
        let ops = ops(c)?;
        principal::validate_writes(&c.store,&attribution,&ops)?;
        let touched_ext = ops.iter().any(|o| op_key(o).starts_with("extension/"));
        Ok(crate::commit::Plan { owner, action:crate::commit::Action::Apply { author, attribution, on_behalf_of, note, ops }, after_adopt:if touched_ext { Some(ext::reconcile) } else { None } })
    }).await
}

async fn apply(core:&Shared, author:&Caller, on_behalf_of:Option<Arc<str>>, note:Option<String>, ops:Vec<Op>, attribution:Attribution) -> RpcResult {
    let bytes = crate::limits::encoded_bytes(&ops);
    apply_prepared(core, author, on_behalf_of, note, bytes, attribution, move |_| Ok(ops)).await
}

/// Run a command: built-ins here, everything else forwarded to the connection that owns it.
pub async fn invoke(core: &Shared, author: &str, name: &str, args: Value) -> RpcResult {
    invoke_as(core,author,principal::UNDECLARED,name,args).await
}
/// Explicit configured identity for trusted embedded callers, not a delegation grant.
pub async fn invoke_as(core:&Shared,author:&str,principal:&str,name:&str,args:Value)->RpcResult {
    let actual=format!("internal:{}:{author}",core.lock().unwrap().epoch);
    if principal!=principal::UNDECLARED{principal::validate(principal)?;}
    let attribution=Attribution::for_actor(principal,&mut json!({}))?;
    let context=crate::invocation::Context {attribution,..Default::default()};
    invoke_deadline(core,&Caller{author:actual,owner:None},context,name,args,None).await
}

async fn invoke_deadline(core: &Shared, author: &Caller, context: crate::invocation::Context, name: &str, args: Value, deadline_ms: Option<u64>) -> RpcResult {
    crate::limits::json_depth(&args,crate::limits::JSON_DEPTH)?;
    let deadline = provider::deadline(deadline_ms)?;
    author.check(&core.lock().unwrap())?;
    let arg = |k: &str| args.get(k).cloned().unwrap_or(Value::Null);
    match name {
        "vqro.shutdown" => Ok(json!({"outcome":"accepted"})),
        "vqro.layout.reset" => layout_reset(core, author, None,context.attribution.clone()).await,
        "vqro.revert" => {
            if !args.is_null() && !args.is_object() {return Err(RpcError::invalid("revert args must be an object"));}
            let txn=match args.get("txn") {None=>None,Some(value)=>Some(value.as_u64().ok_or_else(||RpcError::invalid("txn must be an unsigned integer when supplied"))?)};
            revert(core, author, None, txn,context.attribution.clone()).await
        },
        "vqro.safe_mode" => {
            let gate=core.lock().unwrap().recovery_gate.clone(); let _guard=gate.lock().await;
            let (on,handles,restart)={
                let mut c=core.lock().unwrap();
                author.check(&c)?;
                let on=arg("on").as_bool().unwrap_or(!c.safe_mode);
                let restart=if c.safe_mode && !on { c.embedder_backend.clone() } else { None };
                c.safe_mode=on; c.safe_mode_allow.clear();
                let handles=if on { ext::stop_all_locked(&mut c); c.fence_providers(None) } else { Vec::new() };
                let data=json!({"on":on,"allow":c.safe_mode_allow,"provider_extensions":c.provider_extensions}); c.emit("safe_mode",data);
                (on,handles,restart)
            };
            if on { ext::shutdown(core).await; stop_recovery_providers(handles).await; }
            ext::reconcile(core);
            if let Some(backend)=restart { let(tx,rx)=mpsc::unbounded_channel(); if let Some(handle)=backend.start(tx) { crate::core::add_adapter(core,handle,rx,true); } }
            Ok(json!({"safe_mode":on,"safe_mode_allow":core.lock().unwrap().safe_mode_allow}))
        }
        "vqro.safe_mode.allow" => {
            let gate=core.lock().unwrap().recovery_gate.clone(); let _guard=gate.lock().await;
            let ext_name=arg("name").as_str().map(String::from).ok_or_else(|| RpcError::invalid("needs name"))?;
            let on=arg("on").as_bool().ok_or_else(|| RpcError::invalid("needs boolean on"))?;
            let handles={
                let mut c=core.lock().unwrap();
                author.check(&c)?;
                if !c.safe_mode { return Err(RpcError::new("conflict","safe mode is not on")); }
                if c.store.get(&format!("extension/{ext_name}")).is_none() { return Err(RpcError::new("not_found","no declared extension")); }
                if on {
                    c.safe_mode_allow.insert(ext_name.clone());
                    if let Some(runtime)=c.exts.get_mut(&ext_name) { if runtime.status=="failed" { runtime.crashes.clear(); runtime.status="stopped".into(); } }
                } else { c.safe_mode_allow.remove(&ext_name); }
                let handles=if on { Vec::new() } else { c.fence_providers(Some(&ext_name)) };
                let data=json!({"on":true,"allow":c.safe_mode_allow,"provider_extensions":c.provider_extensions}); c.emit("safe_mode",data);
                handles
            };
            ext::reconcile(core);
            stop_recovery_providers(handles).await;
            Ok(json!({"safe_mode":true,"safe_mode_allow":core.lock().unwrap().safe_mode_allow}))
        }
        "vqro.extension.disable" | "vqro.extension.enable" => {
            let ext_name = arg("name").as_str().map(String::from).or_else(|| arg("text").as_str().map(String::from)).ok_or_else(|| RpcError::invalid("needs `name`"))?;
            let enabled = name == "vqro.extension.enable";
            let target=ext_name.clone();
            let mut reply=apply_prepared(core, author, None, Some(name.into()), crate::limits::encoded_bytes(&ext_name).saturating_add(128), context.attribution.clone(), move |c| {
                let key = format!("extension/{ext_name}");
                let mut value = c.store.get(&key).map(|e| e.value.clone()).ok_or_else(|| RpcError::new("not_found", format!("no extension {ext_name}")))?;
                value["enabled"] = json!(enabled);
                Ok(vec![Op::Set { key, value, expect:None }])
            }).await?;
            // Report what the change leaves unmet; never cascade (the caller decides what else to switch).
            if let Some(obj)=reply.as_object_mut() {
                let states=core.lock().unwrap().extension_states();
                let deps=&states.get(&target).map(|s|s["dependencies"].clone()).unwrap_or(Value::Null);
                if enabled {
                    let unmet:Vec<Value>=deps["needs"].as_array().into_iter().flatten().filter(|n|matches!(n["status"].as_str(),Some("off"|"missing"|"version"))).cloned().collect();
                    obj.insert("needs_unmet".into(),Value::Array(unmet));
                } else {
                    // Users whose need on this extension's capability is no longer satisfied by anyone running.
                    let breaks:Vec<Value>=deps["used_by"].as_array().into_iter().flatten().filter(|u|u["hard"]==true && u["display_status"]!="off").filter(|u|{
                        states.get(u["name"].as_str().unwrap_or("")).is_some_and(|s|s["dependencies"]["needs"].as_array().into_iter().flatten().any(|n|n["cap"]==u["cap"] && n["status"]!="satisfied"))
                    }).map(|u|json!({"name":u["name"],"cap":u["cap"]})).collect();
                    obj.insert("breaks".into(),Value::Array(breaks));
                }
            }
            Ok(reply)
        }
        "vqro.extension.restart" => {
            let ext_name = arg("name").as_str().map(String::from).or_else(|| arg("text").as_str().map(String::from)).ok_or_else(|| RpcError::invalid("needs `name`"))?;
            ext::restart_owned(core, &ext_name, author.owner.as_ref())?;
            Ok(json!({"restarted": ext_name}))
        }
        _ => {
            let (peer, connection, invocations, lifetime) = {
                let c = core.lock().unwrap();
                let cmd = c.commands.get(name).ok_or_else(|| RpcError::new("not_found", format!("no command {name}")))?;
                author.check(&c)?;
                ext::check_owner(&c,cmd.lifetime.as_ref())?;
                if !cmd.available {
                    return Err(RpcError::new("unavailable", format!("{name} is unavailable: {} is not connected", cmd.owner)));
                }
                (cmd.peer.clone().ok_or_else(|| RpcError::new("unavailable", format!("{name} has no handler")))?,
                 cmd.connection.ok_or_else(|| RpcError::new("unavailable", "command has no owning connection"))?, c.invocations.clone(), cmd.lifetime.clone())
            };
            let scope = crate::invocation::enter_attributed(&invocations, connection, Some(context.attribution.effective().unwrap_or(principal::UNDECLARED)), context.attribution.initiator.as_deref(), &context, deadline)?;
            if lifetime.as_ref().is_some_and(|o|!o.is_live()) || author.owner.as_ref().is_some_and(|o|!o.is_live()) {
                return Err(provider::rejected("stale_generation","command owner or caller retired before dispatch"));
            }
            let result = peer.call_with_timeout("command.invoke", json!({"name": name, "args": args, "caller": author,
                "principal":context.attribution.effective().unwrap_or(principal::UNDECLARED),"actor":context.attribution.actor,"initiator":context.attribution.initiator.as_deref().or(context.attribution.effective()),"author":author,"label":context.attribution.label, "invocation":scope.token()}), deadline).await;
            drop(scope);
            result
        }
    }
}

/// One tab per live terminal. The recovery path when a layout is broken or confusing.
async fn layout_reset(core: &Shared, author: &Caller, on_behalf_of:Option<Arc<str>>, attribution:Attribution) -> RpcResult {
    apply_prepared(core, author, on_behalf_of, Some("vqro.layout.reset".into()), 128, attribution, |c| {
        let mut ops = Vec::new();
        let mut tabs = Vec::new();
        for r in c.resources.values().filter(|r| r.facts.kind == "terminal" && r.facts.state == "live") {
            let view = format!("term-{}", r.id);
            ops.push(Op::Set { key: format!("view/{view}"), value: json!({"kind": "terminal", "resource": r.id}), expect: None });
            let title = r.facts.name.clone().or_else(|| r.facts.argv.first().cloned()).unwrap_or_else(|| r.id.clone());
            tabs.push(json!({"title": title, "node": {"view": view}}));
        }
        if tabs.is_empty() {
            ops.push(Op::Set {
                key: "view/vqro-empty".into(),
                value: json!({"kind": "panel", "title": "vqro", "items": [{"text": "No terminals yet."}, {"text": "Open the command palette", "command": "client.palette"}]}),
                expect: None,
            });
            tabs.push(json!({"title": "vqro", "node": {"view": "vqro-empty"}}));
        }
        ops.push(Op::Set { key: "layout/main".into(), value: json!({"tabs": tabs, "active": 0}), expect: None });
        ops.push(Op::Set { key: "workspace/active".into(), value: json!({"layout": "main"}), expect: None });
        // Nothing is deleted: views other layouts use stay valid.
        Ok(ops)
    }).await
}
