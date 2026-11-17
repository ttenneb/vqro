//! The core protocol server: JSON-RPC lines over a local socket. See `docs/spec/core-protocol.md`.

use std::collections::HashMap;
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

pub const PROTOCOL: u64 = 1;
const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

struct Conn {
    id: u64,
    peer: Peer,
    author: Mutex<String>,
    extension: Mutex<Option<String>>,
    streams: Mutex<HashMap<u64, mpsc::UnboundedSender<StreamInput>>>,
    next_stream: AtomicU64,
    subscribed: Mutex<bool>,
}

impl Conn {
    fn author(&self) -> String {
        self.author.lock().unwrap().clone()
    }
}

pub enum Backend {
    /// herdr-core through a command speaking its stdio protocol, e.g. `hc connect`.
    Herdr(Vec<String>),
    /// The in-memory fixture.
    Fake,
}

/// Open the store, bind the socket, start the adapter and extensions, and serve in the background.
/// `defaults`: a directory with `defaults.json` listing extensions to install on first run.
pub fn start(socket: std::path::PathBuf, state: std::path::PathBuf, safe: bool, backend: Backend, defaults: Option<std::path::PathBuf>) -> anyhow::Result<Shared> {
    crate::store::create_dir_all_durable(&state)?;
    if socket.exists() {
        if std::os::unix::net::UnixStream::connect(&socket).is_ok() {
            anyhow::bail!("prim is already running at {}", socket.display());
        }
        std::fs::remove_file(&socket)?;
    }
    let mut store = crate::store::Store::open(state.join("workspace.json"))?;
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
    let handle = match backend {
        Backend::Fake => crate::adapter::fake::start("fake".into(), tx),
        Backend::Herdr(argv) => crate::adapter::herdr::start("herdr".into(), argv, tx),
    };
    crate::core::add_adapter(&core, handle, rx, true);
    ext::reconcile(&core);
    let c = core.clone();
    tokio::spawn(async move {
        if let Err(e) = serve(c, listener).await {
            eprintln!("prim: server stopped: {e}");
        }
    });
    Ok(core)
}

/// First run only: declare the default extensions listed in `<dir>/defaults.json`
/// (`{"extensions": [{"name": "tabs", "argv": ["python3", "tabs.py"]}]}`, paths relative to `dir`).
/// Marked done in `doc/prim/defaults`, so removing a default extension sticks.
fn install_defaults(store: &mut crate::store::Store, dir: &std::path::Path) {
    if store.get("doc/prim/defaults").is_some() {
        return;
    }
    let Ok(bytes) = std::fs::read(dir.join("defaults.json")) else { return };
    let Ok(list) = serde_json::from_slice::<Value>(&bytes) else { return };
    let mut ops = Vec::new();
    let mut names = Vec::new();
    for e in list["extensions"].as_array().into_iter().flatten() {
        let (Some(name), Some(argv)) = (e["name"].as_str(), e["argv"].as_array()) else { continue };
        let argv: Vec<Value> = argv
            .iter()
            .filter_map(|a| a.as_str())
            .map(|a| if a.ends_with(".py") && !a.starts_with('/') { json!(dir.join(a).to_string_lossy()) } else { json!(a) })
            .collect();
        if store.get(&format!("extension/{name}")).is_none() {
            ops.push(Op::Set { key: format!("extension/{name}"), value: json!({"argv": argv}), expect: None });
        }
        names.push(name.to_string());
    }
    ops.push(Op::Set { key: "doc/prim/defaults".into(), value: json!({"installed": names, "from": dir.to_string_lossy()}), expect: None });
    if let Err(e) = store.apply("prim:defaults", Some("install default extensions".into()), ops) {
        eprintln!("prim: could not install default extensions: {e}");
    }
}

pub async fn serve(core: Shared, listener: UnixListener) -> anyhow::Result<()> {
    let next = AtomicU64::new(1);
    loop {
        let (stream, _) = listener.accept().await?;
        let id = next.fetch_add(1, Ordering::Relaxed);
        let (r, w) = stream.into_split();
        let (peer, incoming) = Peer::spawn(r, w);
        tokio::spawn(connection(core.clone(), Arc::new(Conn {
            id,
            peer,
            author: Mutex::new(format!("client:{id}")),
            extension: Mutex::new(None),
            streams: Mutex::new(HashMap::new()),
            next_stream: AtomicU64::new(1),
            subscribed: Mutex::new(false),
        }), incoming));
    }
}

async fn connection(core: Shared, conn: Arc<Conn>, mut incoming: mpsc::UnboundedReceiver<Incoming>) {
    while let Some(msg) = incoming.recv().await {
        match msg {
            // Stream input is handled in order, inline: keystrokes must not be reordered.
            Incoming::Request { id, method, params } if method.starts_with("stream.") => {
                conn.peer.respond(id, stream_op(&conn, &method, params));
            }
            Incoming::Notification { method, params } if method.starts_with("stream.") => {
                let _ = stream_op(&conn, &method, params);
            }
            Incoming::Request { id, method, params } => {
                let (core, conn) = (core.clone(), conn.clone());
                tokio::spawn(async move {
                    let result = dispatch(&core, &conn, &method, params).await;
                    conn.peer.respond(id, result);
                });
            }
            Incoming::Notification { .. } => {}
        }
    }
    // Disconnected: close streams, and mark this connection's commands unavailable.
    for (_, s) in conn.streams.lock().unwrap().drain() {
        let _ = s.send(StreamInput::Close);
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
    adapter: Option<String>,
}

#[derive(Deserialize)]
struct Attach {
    resource: String,
    cols: Option<u16>,
    rows: Option<u16>,
    /// Scrollback lines to replay first, if the backend can.
    history: Option<u32>,
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
}

async fn dispatch(core: &Shared, conn: &Arc<Conn>, method: &str, p: Value) -> RpcResult {
    match method {
        "hello" => {
            let h: Hello = params(p)?;
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
                "seq": c.seq, "safe_mode": c.safe_mode, "socket": c.socket, "state_dir": c.state_dir,
            }))
        }
        "workspace.snapshot" => Ok(core.lock().unwrap().snapshot()),
        "workspace.apply" => {
            let a: Apply = params(p)?;
            let touched_ext = a.ops.iter().any(|o| op_key(o).starts_with("extension/"));
            let summary = {
                let mut c = core.lock().unwrap();
                let result = c.store.apply(&conn.author(), a.note, a.ops);
                c.finish_txn(result).map(|txn| Core::txn_summary(&txn))
            };
            if touched_ext && adopted(&summary) {
                ext::reconcile(core);
            }
            summary
        }
        "workspace.history" => {
            let limit = p.get("limit").and_then(|l| l.as_u64()).unwrap_or(20) as usize;
            Ok(json!({"txns": core.lock().unwrap().store.history(limit)}))
        }
        "workspace.revert" => {
            let txn = p.get("txn").and_then(|t| t.as_u64()).ok_or_else(|| RpcError::invalid("needs `txn`"))?;
            revert(core, &conn.author(), Some(txn))
        }
        "events.subscribe" => subscribe(core, conn, p.get("from").and_then(|f| f.as_u64())),
        "resource.list" => Ok(json!({"resources": core.lock().unwrap().resources.values().map(|r| r.to_json()).collect::<Vec<_>>()})),
        "resource.spawn" => {
            let s: Spawn = params(p)?;
            let id = spawn(core, s).await?;
            let c = core.lock().unwrap();
            Ok(json!({"resource": c.resource(&id)?.to_json()}))
        }
        "resource.kill" => {
            let id = p.get("resource").and_then(|r| r.as_str()).ok_or_else(|| RpcError::invalid("needs `resource`"))?;
            let (adapter, r) = {
                let c = core.lock().unwrap();
                let res = c.resource(id)?;
                (c.adapter(Some(&res.adapter))?, res.facts.backend_ref.clone())
            };
            let result = adapter.kill(r).await.map_err(|e| RpcError::new("backend", e))?;
            Ok(json!({"outcome": "accepted", "backend": result}))
        }
        "resource.attach" => attach(core, conn, params(p)?).await,
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
            invoke(core, &conn.author(), &i.name, i.args).await
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
    let tx = streams.get(&s.stream).ok_or_else(|| RpcError::new("not_found", format!("no stream {}", s.stream)))?;
    let input = match method {
        "stream.input" => {
            let bytes = match (s.data, s.text) {
                (Some(d), _) => B64.decode(d).map_err(|_| RpcError::invalid("`data` must be base64"))?,
                (None, Some(t)) => t.into_bytes(),
                _ => return Err(RpcError::invalid("needs `data` (base64) or `text`")),
            };
            StreamInput::Data(bytes)
        }
        "stream.resize" => StreamInput::Resize(s.cols.unwrap_or(80), s.rows.unwrap_or(24)),
        "stream.close" => StreamInput::Close,
        _ => return Err(RpcError::method_not_found(method)),
    };
    tx.send(input).map_err(|_| RpcError::new("closed", "stream is closed"))?;
    Ok(json!({"outcome": "dispatched"}))
}

fn op_key(op: &Op) -> &str {
    match op {
        Op::Set { key, .. } | Op::Delete { key, .. } => key,
    }
}

fn subscribe(core: &Shared, conn: &Arc<Conn>, from: Option<u64>) -> RpcResult {
    let mut subscribed = conn.subscribed.lock().unwrap();
    if *subscribed {
        return Err(RpcError::new("conflict", "already subscribed"));
    }
    *subscribed = true;
    let (backlog, mut rx, seq) = {
        let c = core.lock().unwrap();
        let from = from.unwrap_or(c.seq);
        (c.events_since(from), c.events.subscribe(), c.seq)
    };
    let peer = conn.peer.clone();
    match backlog {
        None => peer.notify("missed", json!({"seq": seq})),
        Some(evs) => evs.into_iter().for_each(|e| peer.notify("event", e)),
    }
    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                // The receiver was created under the same lock as `seq`, so everything here is newer.
                Ok((_, ev)) => peer.notify("event", ev),
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    // Too slow: the client must resnapshot.
                    peer.notify("missed", json!({}));
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
            if peer.is_closed() {
                break;
            }
        }
    });
    Ok(json!({"seq": seq}))
}

async fn spawn(core: &Shared, s: Spawn) -> Result<String, RpcError> {
    let adapter = core.lock().unwrap().adapter(s.adapter.as_deref())?;
    let argv = s.argv.unwrap_or_else(|| vec![std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into())]);
    if argv.is_empty() {
        return Err(RpcError::invalid("`argv` is empty"));
    }
    let facts = adapter.spawn(SpawnRequest { argv, cwd: s.cwd, name: s.name }).await.map_err(|e| RpcError::new("backend", e))?;
    Ok(core.lock().unwrap().upsert_resource(&adapter.name, facts))
}

async fn attach(core: &Shared, conn: &Arc<Conn>, a: Attach) -> RpcResult {
    let (adapter, r) = {
        let c = core.lock().unwrap();
        let res = c.resource(&a.resource)?;
        (c.adapter(Some(&res.adapter))?, res.facts.backend_ref.clone())
    };
    let mut stream = adapter.attach(r, a.cols.unwrap_or(80), a.rows.unwrap_or(24), a.history.unwrap_or(0)).await.map_err(|e| RpcError::new("backend", e))?;
    let sid = conn.next_stream.fetch_add(1, Ordering::Relaxed);
    conn.streams.lock().unwrap().insert(sid, stream.input.clone());
    let peer = conn.peer.clone();
    let conn2 = conn.clone();
    tokio::spawn(async move {
        while let Some(out) = stream.output.recv().await {
            match out {
                StreamOutput::Data(d) => peer.notify("stream.data", json!({"stream": sid, "data": B64.encode(d)})),
                StreamOutput::Closed(reason) => {
                    peer.notify("stream.closed", json!({"stream": sid, "reason": reason}));
                    break;
                }
            }
            if peer.is_closed() {
                let _ = stream.input.send(StreamInput::Close);
                break;
            }
        }
        conn2.streams.lock().unwrap().remove(&sid);
    });
    Ok(json!({"stream": sid, "resource": a.resource}))
}

fn revert(core: &Shared, author: &str, txn: Option<u64>) -> RpcResult {
    let (summary, touched_ext) = {
        let mut c = core.lock().unwrap();
        let id = match txn {
            Some(t) => t,
            // Default: the last change not made by an extension, so "undo" isn't eaten by an
            // extension's own refreshes. Pass `txn` to revert anything else.
            None => c
                .store
                .history(usize::MAX)
                .into_iter()
                .find(|t| !t.author.starts_with("ext:"))
                .map(|t| t.id)
                .ok_or_else(|| RpcError::new("not_found", "no transactions to revert"))?,
        };
        let result = c.store.revert(author, id);
        let result = c.finish_txn(result);
        let touched_ext = adopted(&result) && c.store.history(1).first().is_some_and(|t| t.changes.iter().any(|ch| ch.key.starts_with("extension/")));
        (result.map(|t| Core::txn_summary(&t)), touched_ext)
    };
    if touched_ext {
        ext::reconcile(core);
    }
    summary
}

fn adopted<T>(result: &Result<T, RpcError>) -> bool {
    result.is_ok() || result.as_ref().is_err_and(|e| e.kind() == "durability_unconfirmed")
}

fn apply(core: &Shared, author: &str, note: &str, ops: Vec<Op>) -> RpcResult {
    let touched_ext = ops.iter().any(|o| op_key(o).starts_with("extension/"));
    let summary = {
        let mut c = core.lock().unwrap();
        let result = c.store.apply(author, Some(note.to_string()), ops);
        c.finish_txn(result).map(|txn| Core::txn_summary(&txn))
    };
    if touched_ext && adopted(&summary) {
        ext::reconcile(core);
    }
    summary
}

/// Run a command: built-ins here, everything else forwarded to the connection that owns it.
pub async fn invoke(core: &Shared, author: &str, name: &str, args: Value) -> RpcResult {
    let arg = |k: &str| args.get(k).cloned().unwrap_or(Value::Null);
    match name {
        "prim.layout.reset" => layout_reset(core, author),
        "prim.revert" => revert(core, author, arg("txn").as_u64()),
        "prim.safe_mode" => {
            let on = {
                let mut c = core.lock().unwrap();
                c.safe_mode = arg("on").as_bool().unwrap_or(!c.safe_mode);
                let on = c.safe_mode;
                c.emit("safe_mode", json!({"on": on}));
                on
            };
            if on {
                ext::stop_all(core);
            }
            ext::reconcile(core);
            Ok(json!({"safe_mode": on}))
        }
        "prim.extension.disable" | "prim.extension.enable" => {
            let ext_name = arg("name").as_str().map(String::from).or_else(|| arg("text").as_str().map(String::from)).ok_or_else(|| RpcError::invalid("needs `name`"))?;
            let key = format!("extension/{ext_name}");
            let mut value = core.lock().unwrap().store.get(&key).map(|e| e.value.clone()).ok_or_else(|| RpcError::new("not_found", format!("no extension {ext_name}")))?;
            value["enabled"] = json!(name == "prim.extension.enable");
            apply(core, author, name, vec![Op::Set { key, value, expect: None }])
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
            peer.call("command.invoke", json!({"name": name, "args": args, "caller": author})).await
        }
    }
}

/// One tab per live terminal. The recovery path when a layout is broken or confusing.
fn layout_reset(core: &Shared, author: &str) -> RpcResult {
    let ops = {
        let c = core.lock().unwrap();
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
        ops
    };
    apply(core, author, "prim.layout.reset", ops)
}
