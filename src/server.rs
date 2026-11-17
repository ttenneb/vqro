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
use crate::ext;
use crate::provider::{self, Route};

pub use crate::adapter::Backend;

pub const PROTOCOL: u64 = 1;
/// Requested OS send buffer per client connection (see `serve`).
pub const CONNECTION_SEND_BUFFER: usize = 16 * 1024;
const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

struct Conn {
    id: u64,
    peer: Peer,
    author: Mutex<String>,
    extension: Mutex<Option<String>>,
    streams: Mutex<HashMap<u64, StreamBinding>>,
    next_stream: AtomicU64,
    subscribed: Mutex<bool>,
    requests: Arc<tokio::sync::Semaphore>,
    mutations:Arc<tokio::sync::Semaphore>,
    wire: crate::provider_wire::Connection,
}

impl Conn {
    fn author(&self) -> String {
        self.author.lock().unwrap().clone()
    }
}

struct StreamBinding {
    input: crate::queue::Sender<StreamInput>,
    route: Route,
}

/// Open the store, bind the socket, start the adapter and extensions, and serve in the background.
/// `defaults`: a directory with `defaults.json` listing extensions to install on first run.
pub fn start(socket: std::path::PathBuf, state: std::path::PathBuf, safe: bool, backend: Backend, defaults: Option<std::path::PathBuf>) -> anyhow::Result<Shared> {
    crate::store::create_dir_all_durable(&state)?;
    let mut store=crate::store::Store::open_locked(state.join("workspace.json"))?;
    if socket.exists() {
        if std::os::unix::net::UnixStream::connect(&socket).is_ok() {
            anyhow::bail!("prim is already running at {}", socket.display());
        }
        std::fs::remove_file(&socket)?;
    }
    if let Some(dir) = defaults {
        install_defaults(&mut store, &dir);
    }
    let core: Shared = Arc::new(Mutex::new(Core::new(store, Some(state), socket.clone(), safe)?));
    let listener = UnixListener::bind(&socket)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))?;
    }
    let (tx, rx) = mpsc::unbounded_channel();
    if let Some(handle) = backend.start(tx) {
        crate::core::add_adapter(&core, handle, rx, true);
    }
    ext::reconcile(&core);
    let c = core.clone();
    tokio::spawn(async move {
        if let Err(e) = serve(c, listener).await {
            eprintln!("prim: server stopped: {e}");
        }
    });
    Ok(core)
}

/// Declare the default extensions listed in `<dir>/defaults.json`:
/// `{"version": 2, "extensions": [{"name", "argv", "description"?, "kind"?, "enabled"?, "default"?, "since"?}]}`
/// (`.py` paths relative to `dir`; a bare `prim-*` program resolves next to this binary when present).
/// Each entry is offered once per state dir: all on first run, and later only entries whose `since` is
/// newer than the version recorded in `doc/prim/defaults` (migrations of existing installs). An entry
/// that already exists is left as it is, and removing a default extension sticks.
fn install_defaults(store: &mut crate::store::Store, dir: &std::path::Path) {
    let Ok(bytes) = std::fs::read(dir.join("defaults.json")) else { return };
    let Ok(list) = serde_json::from_slice::<Value>(&bytes) else { return };
    let version = list["version"].as_u64().unwrap_or(1);
    let recorded = store.get("doc/prim/defaults").map(|e| e.value.clone());
    // No record: nothing installed yet. A record without a version predates versioned defaults (1).
    let installed = recorded.as_ref().map(|r| r["version"].as_u64().unwrap_or(1)).unwrap_or(0);
    if installed >= version {
        return;
    }
    let sibling = std::env::current_exe().ok().and_then(|exe| exe.parent().map(std::path::Path::to_path_buf));
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
                } else if a.starts_with("prim-") && !a.contains('/') {
                    match sibling.as_ref().map(|d| d.join(a)).filter(|p| p.exists()) {
                        Some(path) => json!(path.to_string_lossy()),
                        None => json!(a),
                    }
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
    ops.push(Op::Set { key: "doc/prim/defaults".into(), value: json!({"installed": names, "from": dir.to_string_lossy(), "version": version}), expect: None });
    let note = if installed == 0 { "install default extensions" } else { "add new default extensions" };
    if let Err(e) = store.apply("prim:defaults", Some(note.into()), ops) {
        eprintln!("prim: could not install default extensions: {e}");
    }
}

pub async fn serve(core: Shared, listener: UnixListener) -> anyhow::Result<()> {
    let next = AtomicU64::new(1);
    let shutdown = core.lock().unwrap().shutdown.clone();
    let mut connections = tokio::task::JoinSet::new();
    let mut peers = HashMap::new();
    loop {
        let (stream, _) = tokio::select! {
            biased;
            _ = shutdown.requested() => break,
            done = connections.join_next(), if !connections.is_empty() => { if let Some(Ok(id)) = done { peers.remove(&id); } continue; }
            accepted = listener.accept() => match accepted { Ok(stream) => stream, Err(error) => { eprintln!("prim: listener failed: {error}"); break; } },
        };
        // Bounded outbound: bytes in the OS send buffer can't be revoked when a slow viewer's streams
        // are closed, so keep that buffer small (the OS may round it up; Linux doubles it). Measured
        // with the scale harness: a 64 KiB/s viewer's first echo after attaching to output floods fell
        // from 5.6 s with the 208 KiB default to 0.5-0.65 s; much smaller buffers slow such a reader
        // down. Draining viewers' throughput is unchanged. Best effort: failure keeps the OS default.
        let _ = socket2::SockRef::from(&stream).set_send_buffer_size(CONNECTION_SEND_BUFFER);
        let id = next.fetch_add(1, Ordering::Relaxed);
        let (r, w) = stream.into_split();
        let (peer, incoming) = Peer::spawn(r, w);
        let mutations=Arc::new(tokio::sync::Semaphore::new(crate::rpc::MAX_PENDING_CALLS));
        peers.insert(id,(peer.clone(),mutations.clone()));
        let conn = Arc::new(Conn {
            id,
            wire: crate::provider_wire::Connection::new(id, peer.clone()),
            peer,
            author: Mutex::new(format!("client:{id}")),
            extension: Mutex::new(None),
            streams: Mutex::new(HashMap::new()),
            next_stream: AtomicU64::new(1),
            subscribed: Mutex::new(false),
            requests: Arc::new(tokio::sync::Semaphore::new(crate::rpc::MAX_PENDING_CALLS)),
            mutations,
        });
        let c = core.clone();
        connections.spawn(async move { connection(c, conn, incoming).await; id });
    }
    // A listener failure also enters the same stop state as an explicit shutdown request.
    shutdown.request();
    // Keep the endpoint bound until cleanup finishes, so a replacement cannot race our unlink.
    let commits=crate::commit::shutdown(&core,std::time::Duration::from_secs(3)).await;
    if !commits.completed { eprintln!("prim: workspace shutdown deadline: {} queued commits rejected without effects; {} active outcomes indeterminate; do not replay",commits.rejected,commits.active); }
    // A worker receipt is not yet a transport reply. Mutation handlers retain these permits
    // through reply flushing, including replies issued on provider/extension connections.
    let replies=tokio::time::timeout(std::time::Duration::from_secs(1),async {
        let mut held=Vec::new();
        for (_,mutations) in peers.values() {
            held.push(mutations.clone().acquire_many_owned(crate::rpc::MAX_PENDING_CALLS as u32).await.map_err(|_|())?);
        }
        Ok::<_,()>(held)
    }).await;
    if !matches!(replies,Ok(Ok(_))) { eprintln!("prim: workspace reply delivery indeterminate at shutdown deadline; inspect history, never replay"); }
    ext::shutdown(&core).await;
    let handles: Vec<_> = core.lock().unwrap().providers.values().map(|p| p.handle.clone()).collect();
    let mut providers = tokio::task::JoinSet::new();
    for handle in handles { providers.spawn(async move { (handle.name.clone(), handle.shutdown().await) }); }
    if tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while let Some(result) = providers.join_next().await {
            if let Ok((name, Err(error))) = result { eprintln!("prim: provider {name} shutdown unconfirmed: {error}"); }
        }
    }).await.is_err() { eprintln!("prim: provider shutdown deadline; cleanup unconfirmed"); providers.abort_all(); }
    { let mut c = core.lock().unwrap(); let names: Vec<_> = c.providers.keys().cloned().collect();
      for name in names { c.on_adapter_event(&name, crate::adapter::AdapterEvent::Connected(false, None)); } }
    for (peer,_) in peers.values() { peer.close(); }
    if tokio::time::timeout(std::time::Duration::from_secs(1), async { while connections.join_next().await.is_some() {} }).await.is_err() { connections.abort_all(); }
    let socket = core.lock().unwrap().socket.clone();
    let _ = std::fs::remove_file(socket);
    drop(listener);
    { let mut c=core.lock().unwrap();c.store.release_state();shutdown.finish(); }
    Ok(())
}

/// Signals and embedders use the same ordered stop as protocol callers.
pub async fn shutdown(core: &Shared) {
    let lifetime = core.lock().unwrap().shutdown.clone();
    lifetime.request(); lifetime.stopped().await;
}

async fn connection(core: Shared, conn: Arc<Conn>, mut incoming: crate::rpc::IncomingReceiver) {
    while let Some(msg) = incoming.recv().await {
        match msg {
            // Provider lifecycle and fact messages stay ordered on their connection. Native
            // replies are read independently by Peer, so a pending host-token wait cannot block it.
            Incoming::Request { id, method, params } if matches!(method.as_str(), "hello" | "provider.register" | "provider.sync") => {
                let result = dispatch(&core, &conn, &method, params).await;
                conn.peer.respond(id, result);
            }
            Incoming::Notification { method, params } if conn.wire.is_provider() || matches!(method.as_str(), "resource.upsert" | "resource.gone" | "stream.data" | "stream.closed" | "stream.gap" | "stream.error") => {
                let valid = conn.wire.validate(&core, &params);
                let result = valid.and_then(|_| {
                    if matches!(method.as_str(), "resource.upsert" | "resource.gone" | "stream.data" | "stream.closed" | "stream.gap" | "stream.error") {
                        conn.wire.notification(&core, &method, params)
                    } else { Ok(()) }
                });
                if result.is_err() { conn.peer.close(); }
            }
            // Stream input is handled in order, inline: keystrokes must not be reordered.
            Incoming::Request { id, method, params } if method.starts_with("stream.") => {
                let result = if conn.wire.is_provider() {
                    conn.wire.validate(&core, &params).and_then(|_| Err(RpcError::method_not_found(&method)))
                } else { stream_op(&conn, &method, params) };
                conn.peer.respond(id, result);
            }
            Incoming::Notification { method, params } if method.starts_with("stream.") => {
                let sid = params["stream"].clone();
                if let Err(error) = stream_op(&conn, &method, params) {
                    conn.peer.notify("stream.error", json!({"stream": sid, "error": error, "outcome": "rejected"}));
                }
            }
            Incoming::Request { id, method, params } => {
                let Ok(permit) = conn.requests.clone().try_acquire_owned() else {
                    conn.peer.respond(id, Err(provider::rejected("overloaded", "connection request limit; not dispatched")));
                    continue;
                };
                let (core, conn) = (core.clone(), conn.clone());
                tokio::spawn(async move {
                    let _permit = permit;
                    let stopping = method == "core.shutdown" || (method == "command.invoke" && params["name"] == "prim.shutdown");
                    let mutation=matches!(method.as_str(),"workspace.apply"|"workspace.revert") || (method=="command.invoke" && matches!(params["name"].as_str(),Some("prim.revert"|"prim.layout.reset"|"prim.extension.enable"|"prim.extension.disable")));
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
                            eprintln!("prim: workspace reply delivery indeterminate; inspect history, never replay");
                        }
                    }
                });
            }
            Incoming::Notification { .. } => {}
        }
    }
    // Disconnected: invalidate provider routes before closing its viewer streams and commands.
    conn.wire.disconnect(&core);
    for (_, s) in conn.streams.lock().unwrap().drain() {
        let _ = StreamInput::Close.send(&s.input);
    }
    let mut c = core.lock().unwrap();
    c.clients.remove(&conn.id);
    let gone: Vec<String> = c.live.iter().filter(|(_, (_, _, id))| *id == conn.id).map(|(k, _)| k.clone()).collect();
    for view in gone {
        c.live.remove(&view);
        c.emit("view.content", json!({"view": view, "content": null}));
    }
    let names: Vec<String> = c.commands.values().filter(|cmd| cmd.connection == Some(conn.id) && cmd.available).map(|cmd| cmd.name.clone()).collect();
    for name in names {
        if let Some(cmd) = c.commands.get_mut(&name) {
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
    version: Option<String>,
    /// Build identity (e.g. the binary's modification time), to spot out-of-date clients.
    build: Option<u64>,
}

#[derive(Deserialize)]
struct Apply {
    ops: Vec<Op>,
    note: Option<String>,
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

async fn dispatch(core: &Shared, conn: &Arc<Conn>, method: &str, p: Value) -> RpcResult {
    if conn.peer.is_closed() { return Err(provider::rejected("disconnected", "connection closed before dispatch")); }
    if core.lock().unwrap().shutdown.is_requested() && method != "core.shutdown" && !(method == "command.invoke" && p["name"] == "prim.shutdown") {
        return Err(provider::rejected("unavailable", "core is shutting down; not dispatched"));
    }
    if method != "provider.register" && conn.wire.is_provider() { conn.wire.validate(core, &p)?; }
    match method {
        "core.shutdown" => Ok(json!({"outcome":"accepted"})),
        "provider.register" => conn.wire.register(core, p).await,
        "provider.sync" => conn.wire.sync(core, p),
        "hello" => {
            let h: Hello = params(p)?;
            conn.wire.hello()?;
            if let Some(ext) = h.extension.clone() {
                *conn.author.lock().unwrap() = format!("ext:{ext}");
                *conn.extension.lock().unwrap() = Some(ext);
            } else if let Some(client) = h.client.clone() {
                *conn.author.lock().unwrap() = format!("{client}:{}", conn.id);
            }
            let mut c = core.lock().unwrap();
            let me = json!({"id": conn.id, "author": conn.author(), "client": h.client, "version": h.version, "build": h.build});
            c.clients.insert(conn.id, me);
            Ok(json!({
                "server": "prim", "version": env!("CARGO_PKG_VERSION"), "protocol": PROTOCOL, "you": conn.author(),
                "epoch": c.epoch, "seq": c.seq, "safe_mode": c.safe_mode, "socket": c.socket, "state_dir": c.state_dir,
            }))
        }
        "workspace.snapshot" => Ok(core.lock().unwrap().snapshot()),
        "workspace.apply" => {
            let a: Apply = params(p)?;
            apply(core, &conn.author(), a.note, a.ops).await
        }
        "workspace.history" => {
            let limit = p.get("limit").and_then(|l| l.as_u64()).unwrap_or(20) as usize;
            Ok(json!({"txns": core.lock().unwrap().store.history(limit)}))
        }
        "workspace.revert" => {
            let txn = p.get("txn").and_then(|t| t.as_u64()).ok_or_else(|| RpcError::invalid("needs `txn`"))?;
            revert(core, &conn.author(), Some(txn)).await
        }
        "events.subscribe" => subscribe(core, conn, params(p)?),
        "resource.list" => Ok(json!({"resources": core.lock().unwrap().resources.values().map(|r| r.to_json()).collect::<Vec<_>>()})),
        "resource.spawn" => {
            spawn(core, &conn.author(), params(p)?).await
        }
        "resource.kill" => {
            let id = p.get("resource").and_then(|r| r.as_str()).ok_or_else(|| RpcError::invalid("needs `resource`"))?;
            let (route, reference) = {
                let c = core.lock().unwrap();
                (c.resource_route(id, "terminal.kill")?, c.resource(id)?.facts.backend_ref.clone())
            };
            let (handle, operation, c) = (route.handle.clone(), route.clone(), core.clone());
            let deadline = provider::deadline(p.get("deadline_ms").map(|v| v.as_u64().ok_or_else(|| RpcError::invalid("deadline_ms must be an unsigned integer"))).transpose()?)?;
            let (effect, mut result) = effect(core, conn.author(), route, "resource.kill", deadline, |value: &crate::adapter::EffectResult| value.outcome, move |context| async move {
                let mut result = handle.for_effect(context).kill(reference).await.map_err(|e| e.rpc())?;
                if !operation.valid() { return Err(provider::indeterminate("provider changed before kill result was recorded")); }
                if let Some(facts) = result.facts.take() { c.lock().unwrap().upsert_resource(&operation.handle.name, facts)?; }
                Ok(result)
            }).await?;
            Ok(json!({"outcome": result.outcome, "effect": effect, "backend": result.data.take()}))
        }
        "resource.attach" => attach(core, conn, params(p)?).await,
        "resource.invoke" => invoke_resource(core, &conn.author(), p).await,
        "command.register" => {
            let r: Register = params(p)?;
            if r.name.starts_with("prim.") {
                return Err(RpcError::new("forbidden", "`prim.` commands belong to the core"));
            }
            let mut c = core.lock().unwrap();
            if let Some(existing) = c.commands.get(&r.name) {
                if existing.available && existing.owner != conn.author() {
                    return Err(RpcError::new("conflict", format!("{} is owned by {}", r.name, existing.owner)));
                }
            }
            let info = CommandInfo { name: r.name.clone(), title: r.title.unwrap_or_else(|| r.name.clone()), owner: conn.author(), connection: Some(conn.id), available: true, peer: Some(conn.peer.clone()) };
            let j = info.to_json();
            c.commands.insert(r.name, info);
            c.emit("command", j.clone());
            Ok(j)
        }
        "command.list" => Ok(json!({"commands": core.lock().unwrap().commands.values().map(|c| c.to_json()).collect::<Vec<_>>()})),
        "command.invoke" => {
            let i: Invoke = params(p)?;
            invoke_deadline(core, &conn.author(), &i.name, i.args, i.deadline_ms).await
        }
        "ui.notify" => {
            // A transient message for every client (toast). Not state.
            let text = p.get("text").and_then(|t| t.as_str()).ok_or_else(|| RpcError::invalid("needs `text`"))?.to_string();
            let mut c = core.lock().unwrap();
            c.emit("notify", json!({"text": text, "style": p.get("style"), "author": conn.author()}));
            Ok(json!({"outcome": "dispatched"}))
        }
        "view.publish" => {
            let view = p.get("view").and_then(|v| v.as_str()).ok_or_else(|| RpcError::invalid("needs `view`"))?.to_string();
            let content = p.get("content").cloned().unwrap_or(Value::Null);
            if !content.is_null() && !content.is_object() {
                return Err(RpcError::invalid("`content` is an object, or null to clear"));
            }
            let mut c = core.lock().unwrap();
            let old = c.live.get(&view).map(|(v, _, _)| crate::limits::encoded_bytes(&(&view, v))).unwrap_or(0);
            let next = if content.is_null() { 0 } else { crate::limits::encoded_bytes(&(&view, &content)) };
            if next > crate::limits::LIVE_VALUE_BYTES && next > old {
                return Err(crate::limits::too_large("live view", next, crate::limits::LIVE_VALUE_BYTES));
            }
            let total: usize = c.live.iter().map(|(key, (value, _, _))| crate::limits::encoded_bytes(&(key, value))).sum();
            let candidate = total.saturating_sub(old).saturating_add(next);
            if candidate > crate::limits::LIVE_BYTES && candidate > total {
                return Err(crate::limits::too_large("live content", candidate, crate::limits::LIVE_BYTES));
            }
            let unchanged = c.live.get(&view).map(|(v, _, _)| *v == content).unwrap_or(content.is_null());
            if content.is_null() {
                c.live.remove(&view);
            } else {
                c.live.insert(view.clone(), (content.clone(), conn.author(), conn.id));
            }
            if !unchanged {
                c.emit("view.content", json!({"view": view, "content": content, "author": conn.author()}));
            }
            Ok(json!({"outcome": "accepted"}))
        }
        "client.list" => Ok(json!({"clients": core.lock().unwrap().clients.values().cloned().collect::<Vec<_>>()})),
        "extension.list" => {
            let c = core.lock().unwrap();
            Ok(json!({"extensions": c.exts.iter().map(|(n, e)| (n.clone(), e.to_json())).collect::<serde_json::Map<_, _>>()}))
        }
        _ => Err(RpcError::method_not_found(method)),
    }
}

fn stream_op(conn: &Arc<Conn>, method: &str, p: Value) -> RpcResult {
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
            match rx.recv().await {
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
    core: &Shared, author: String, route: Route, method: &'static str, deadline: std::time::Duration, success: fn(&T) -> &'static str,
    work: impl FnOnce(crate::adapter::EffectContext) -> F,
) -> Result<(String, T), RpcError>
where F: std::future::Future<Output = Result<T, RpcError>> + Send + 'static {
    if !route.valid() { return Err(provider::rejected("unavailable", "provider changed before dispatch")); }
    let id = core.lock().unwrap().effect_id();
    let context = crate::adapter::EffectContext { effect: id.clone(), deadline: std::time::Instant::now() + deadline, target: route.target.clone() };
    let result = crate::rpc::with_deadline(method, deadline, route.guard(work(context))).await;
    let outcome = match &result {
        Ok(value) => success(value),
        Err(error) => error.data.as_ref().and_then(|d| d["outcome"].as_str()).unwrap_or("indeterminate"),
    };
    core.lock().unwrap().emit("effect", json!({"effect": id, "provider": route.handle.name, "generation": route.generation, "method": method, "author": author, "outcome": outcome, "error": result.as_ref().err()}));
    result.map(|value| (id.clone(), value)).map_err(|mut error| {
        if let Some(data) = error.data.as_mut() { data["effect"] = json!(id); }
        error
    })
}

async fn spawn(core: &Shared, author: &str, s: Spawn) -> RpcResult {
    let deadline = provider::deadline(s.deadline_ms)?;
    let route = core.lock().unwrap().provider_route(s.provider.as_deref(), "terminal.spawn")?;
    let argv = s.argv.unwrap_or_default();
    if (s.bridge || !s.env.is_empty()) && route.handle.capabilities["terminal.spawn"]["env"] == false {
        return Err(provider::rejected("unsupported", "provider explicitly does not support spawn environment"));
    }
    let reservation = if s.bridge { Some(crate::provider_wire::BridgeReservation::new(core)?) } else { None };
    let mut env = s.env;
    // Every terminal can reach prim (`prim call` from any pane): its opaque endpoint (D22), unless the
    // provider cannot pass an environment at all. The bridge credential stays opt-in.
    if route.handle.capabilities["terminal.spawn"]["env"] != false {
        env.entry("PRIM_SOCKET".into()).or_insert_with(|| core.lock().unwrap().socket.to_string_lossy().into_owned());
    }
    if let Some(reservation) = &reservation {
        env.insert("PRIM_BRIDGE_TOKEN".into(), reservation.token.clone());
        env.insert("PRIM_SOCKET".into(), core.lock().unwrap().socket.to_string_lossy().into_owned());
    }
    for dimension in [s.cols, s.rows].into_iter().flatten() {
        if !(1..=9999).contains(&dimension) { return Err(provider::rejected("invalid", "spawn dimensions must be between 1 and 9999")); }
    }
    let (c, operation) = (core.clone(), route.clone());
    let (effect, id) = effect(core, author.into(), route, "resource.spawn", deadline, |_| "observed", move |context| async move {
        let facts = operation.handle.for_effect(context).spawn(SpawnRequest { argv, cwd: s.cwd, name: s.name, env, cols: s.cols, rows: s.rows, placement: s.placement }).await.map_err(|e| e.rpc())?;
        let id = {
            let mut c = c.lock().unwrap();
            if !operation.valid() { return Err(provider::indeterminate("provider changed before spawn result was recorded")); }
            c.upsert_resource(&operation.handle.name, facts)?
        };
        if let Some(reservation) = reservation { reservation.bind(&id)?; }
        Ok(id)
    }).await?;
    Ok(json!({"outcome": "observed", "effect": effect, "resource": core.lock().unwrap().resource(&id)?.to_json()}))
}

async fn invoke_resource(core: &Shared, author: &str, p: Value) -> RpcResult {
    let id = p["resource"].as_str().ok_or_else(|| RpcError::invalid("needs resource"))?;
    let op = p["op"].as_str().ok_or_else(|| RpcError::invalid("needs op"))?.to_string();
    if op == "terminal.spawn" { return Err(provider::rejected("invalid", "terminal.spawn has no target; use resource.spawn")); }
    let args = p.get("args").cloned().unwrap_or_else(|| json!({}));
    if !args.is_object() { return Err(RpcError::invalid("args must be an object")); }
    let route = core.lock().unwrap().resource_route(id, &op)?;
    if route.handle.capabilities[&op]["effect"] == false {
        return Err(provider::rejected("unsupported", format!("provider declares {op} unavailable through effects")));
    }
    let (reference, incarnation) = route.target.clone().unwrap();
    let (handle, operation, c) = (route.handle.clone(), route.clone(), core.clone());
    let deadline = provider::deadline(p.get("deadline_ms").map(|v| v.as_u64().ok_or_else(|| RpcError::invalid("deadline_ms must be an unsigned integer"))).transpose()?)?;
    let (effect, result) = effect(core, author.into(), route, "resource.invoke", deadline, |value: &crate::adapter::EffectResult| value.outcome, move |context| async move {
        let mut result = handle.for_effect(context).invoke(op, reference, incarnation, args).await.map_err(|e| e.rpc())?;
        if !operation.valid() { return Err(provider::indeterminate("provider changed before effect result was recorded")); }
        if let Some(facts) = result.facts.take() { c.lock().unwrap().upsert_resource(&operation.handle.name, facts)?; }
        Ok(result)
    }).await?;
    Ok(json!({"outcome": result.outcome, "effect": effect, "data": result.data}))
}

async fn attach(core: &Shared, conn: &Arc<Conn>, a: Attach) -> RpcResult {
    let (cols, rows) = (a.cols.unwrap_or(80), a.rows.unwrap_or(24));
    if !(1..=9999).contains(&cols) || !(1..=9999).contains(&rows) { return Err(provider::rejected("invalid", "stream dimensions must be between 1 and 9999")); }
    let (route, reference) = {
        let c = core.lock().unwrap();
        (c.resource_route(&a.resource, "terminal.output")?, c.resource(&a.resource)?.facts.backend_ref.clone())
    };
    let handle = route.handle.clone();
    let (effect, mut stream) = effect(core, conn.author(), route.clone(), "resource.attach", provider::deadline(a.deadline_ms)?, |_| "accepted", move |context| async move {
        handle.for_effect(context).attach(reference, cols, rows, a.history.unwrap_or(0)).await.map_err(|e| e.rpc())
    }).await?;
    if !route.valid() { return Err(provider::indeterminate("provider changed before stream registration")); }
    let initial = stream.initial;
    let sid = conn.next_stream.fetch_add(1, Ordering::Relaxed);
    {
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
    Ok(json!({"outcome": "accepted", "effect": effect, "initial": initial, "stream": sid, "resource": a.resource}))
}

async fn revert(core: &Shared, author: &str, txn: Option<u64>) -> RpcResult {
    let author = author.to_string();
    crate::commit::submit(core, crate::limits::encoded_bytes(&author).saturating_add(128), move |c| {
        // Select default undo only when earlier queued commits have been adopted.
        let id = txn.or_else(|| c.store.latest_txn_matching(|t| !t.author.starts_with("ext:")))
            .ok_or_else(|| RpcError::new("not_found", "no transactions to revert"))?;
        let touched_ext = c.store.transaction(id).is_some_and(|t| t.changes.iter().any(|ch| ch.key.starts_with("extension/")));
        Ok(crate::commit::Plan { action:crate::commit::Action::Revert { author, txn:id }, after_adopt:if touched_ext { Some(ext::reconcile) } else { None } })
    }).await
}

async fn apply_prepared(
    core:&Shared, author:&str, note:Option<String>, bytes:usize,
    ops:impl FnOnce(&Core)->Result<Vec<Op>,RpcError> + Send + 'static,
) -> RpcResult {
    let bytes = bytes.saturating_add(crate::limits::encoded_bytes(&(author, &note)));
    let author = author.to_string();
    crate::commit::submit(core, bytes, move |c| {
        let ops = ops(c)?;
        let touched_ext = ops.iter().any(|o| op_key(o).starts_with("extension/"));
        Ok(crate::commit::Plan { action:crate::commit::Action::Apply { author, note, ops }, after_adopt:if touched_ext { Some(ext::reconcile) } else { None } })
    }).await
}

async fn apply(core:&Shared, author:&str, note:Option<String>, ops:Vec<Op>) -> RpcResult {
    let bytes = crate::limits::encoded_bytes(&ops);
    apply_prepared(core, author, note, bytes, move |_| Ok(ops)).await
}

/// Run a command: built-ins here, everything else forwarded to the connection that owns it.
pub async fn invoke(core: &Shared, author: &str, name: &str, args: Value) -> RpcResult {
    invoke_deadline(core, author, name, args, None).await
}

async fn invoke_deadline(core: &Shared, author: &str, name: &str, args: Value, deadline_ms: Option<u64>) -> RpcResult {
    let deadline = provider::deadline(deadline_ms)?;
    let arg = |k: &str| args.get(k).cloned().unwrap_or(Value::Null);
    match name {
        "prim.shutdown" => Ok(json!({"outcome":"accepted"})),
        "prim.layout.reset" => layout_reset(core, author).await,
        "prim.revert" => revert(core, author, arg("txn").as_u64()).await,
        "prim.safe_mode" => {
            let on = {
                let mut c = core.lock().unwrap();
                c.safe_mode = arg("on").as_bool().unwrap_or(!c.safe_mode);
                let on = c.safe_mode;
                c.emit("safe_mode", json!({"on": on}));
                on
            };
            // reconcile stops what safe mode does not want; provider extensions keep running.
            ext::reconcile(core);
            Ok(json!({"safe_mode": on}))
        }
        "prim.extension.disable" | "prim.extension.enable" => {
            let ext_name = arg("name").as_str().map(String::from).or_else(|| arg("text").as_str().map(String::from)).ok_or_else(|| RpcError::invalid("needs `name`"))?;
            let enabled = name == "prim.extension.enable";
            apply_prepared(core, author, Some(name.into()), crate::limits::encoded_bytes(&ext_name).saturating_add(128), move |c| {
                let key = format!("extension/{ext_name}");
                let mut value = c.store.get(&key).map(|e| e.value.clone()).ok_or_else(|| RpcError::new("not_found", format!("no extension {ext_name}")))?;
                value["enabled"] = json!(enabled);
                Ok(vec![Op::Set { key, value, expect:None }])
            }).await
        }
        "prim.extension.restart" => {
            let ext_name = arg("name").as_str().map(String::from).or_else(|| arg("text").as_str().map(String::from)).ok_or_else(|| RpcError::invalid("needs `name`"))?;
            ext::restart(core, &ext_name)?;
            Ok(json!({"restarted": ext_name}))
        }
        _ => {
            let peer = {
                let c = core.lock().unwrap();
                let cmd = c.commands.get(name).ok_or_else(|| RpcError::new("not_found", format!("no command {name}")))?;
                if !cmd.available {
                    return Err(RpcError::new("unavailable", format!("{name} is unavailable: {} is not connected", cmd.owner)));
                }
                cmd.peer.clone().ok_or_else(|| RpcError::new("unavailable", format!("{name} has no handler")))?
            };
            peer.call_with_timeout("command.invoke", json!({"name": name, "args": args, "caller": author}), deadline).await
        }
    }
}

/// One tab per live terminal. The recovery path when a layout is broken or confusing.
async fn layout_reset(core: &Shared, author: &str) -> RpcResult {
    apply_prepared(core, author, Some("prim.layout.reset".into()), 128, |c| {
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
                key: "view/prim-empty".into(),
                value: json!({"kind": "panel", "title": "prim", "items": [{"text": "No terminals yet."}, {"text": "Open the command palette", "command": "client.palette"}]}),
                expect: None,
            });
            tabs.push(json!({"title": "prim", "node": {"view": "prim-empty"}}));
        }
        ops.push(Op::Set { key: "layout/main".into(), value: json!({"tabs": tabs, "active": 0}), expect: None });
        ops.push(Op::Set { key: "workspace/active".into(), value: json!({"layout": "main"}), expect: None });
        // Nothing is deleted: views other layouts use stay valid.
        Ok(ops)
    }).await
}
