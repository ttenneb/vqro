//! prim-herdr: herdr-core as an out-of-process provider (`docs/spec/provider-v0.md`, section 9).
//!
//! Wraps the existing in-process adapter (`prim::adapter::herdr`, which speaks herdr-core's framed
//! protocol through `hc connect`) and bridges its commands and events to `prim::provider_client`.
//! Nothing herdr-specific is duplicated here: this file only translates.
//!
//! - Instance: herdr-core's `core.id` (stable across core restarts, new after its state is wiped).
//!   Refs are terminal addresses (`t3`); incarnation is the terminal's creation time.
//! - Provider availability follows the backend: when herdr-core is lost, the core connection is
//!   closed (the core marks the provider disconnected); when the adapter reconnects, the provider
//!   registers again (new generation) and sends a full sync. Nothing is resent.
//! - Effects: `terminal.spawn` (argv, cwd, name, env, cols, rows, placement) and
//!   `terminal.kill` (observed with finished facts when herdr-core's reply says finished). herdr-core error replies and lost replies are `backend` errors (outcome unknown),
//!   as the in-process core treats them.
//! - Streams: herdr-core attach channels with backend history as initial data. Credit goes back to
//!   herdr-core only after `provider_client` has sent the bytes within the core's window.
//!
//! `prim-herdr [--hc PATH] [--session NAME] [--name NAME]`; connects to `$PRIM_SOCKET`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use prim::adapter::{self, AdapterEvent, AdapterHandle, ResourceFacts, SpawnRequest, StreamInput, StreamOutput};
use prim::provider_client::{Config, EffectRequest, Initial, OpenRequest, Outcome, Provider, ProviderError, ProviderStream, Request, StreamCommand};
use serde_json::{json, Map, Value};
use tokio::sync::mpsc;

struct Args {
    hc: String,
    session: Option<String>,
    name: Option<String>,
}

fn args() -> Result<Args, String> {
    let mut a = Args { hc: "hc".into(), session: None, name: None };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut value = || it.next().ok_or_else(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--hc" => a.hc = value()?,
            "--session" => a.session = Some(value()?),
            "--name" => a.name = Some(value()?),
            "-h" | "--help" => {
                println!("prim-herdr [--hc PATH] [--session NAME] [--name NAME]\nConnects to $PRIM_SOCKET; reaches herdr-core with `hc connect`.");
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument {other}")),
        }
    }
    Ok(a)
}

fn capabilities() -> Map<String, Value> {
    json!({
        "terminal.spawn": {"env": true, "cwd": true, "name": true},
        "terminal.input": {},
        "terminal.output": {"raw": true, "history": true, "repaint": "none"},
        "terminal.resize": {"owner": "latest_viewer"},
        "terminal.kill": {},
        "terminal.list": {},
        "terminal.exit": {"exit_code": true},
        "status.osc7501": {"stored": true, "lossy": false},
        "events.resync": {},
        "persist.detach": {},
        "remote.machines": {},
        "identity": {"refs_survive_backend_restart": true},
    })
    .as_object()
    .unwrap()
    .clone()
}

/// Adapter facts as section 9.3 facts.
fn facts(f: &ResourceFacts) -> Value {
    let finished = f.state == "finished";
    json!({
        "ref": f.backend_ref, "incarnation": f.incarnation, "kind": f.kind, "state": f.state,
        "name": f.name, "title": f.title, "cwd": f.cwd, "argv": f.argv,
        "status": if f.status.is_object() { f.status.clone() } else { Value::Null },
        "exit": if finished && f.exit.is_object() { f.exit.clone() } else { Value::Null },
        "data": if f.data.is_object() { f.data.clone() } else { json!({}) },
    })
}

fn unknown(e: adapter::AdapterError) -> ProviderError {
    match e {
        adapter::AdapterError::Rejected(m) => ProviderError::new("unavailable", format!("not sent: {m}")),
        adapter::AdapterError::RejectedKind { kind, message } => ProviderError::new(&kind, message),
        adapter::AdapterError::Backend(m) => ProviderError::new("backend", format!("herdr-core error; outcome unknown: {m}")),
        adapter::AdapterError::Indeterminate(m) | adapter::AdapterError::Timeout(m) => ProviderError::new("backend", format!("outcome unknown: {m}")),
    }
}

/// Live facts by ref, as last published.
type Known = Arc<Mutex<HashMap<String, Value>>>;

async fn effect(handle: AdapterHandle, known: Known, provider: Provider, e: EffectRequest) {
    let result = match e.op.as_str() {
        "terminal.spawn" => spawn(&handle, &known, &e.args).await,
        "terminal.kill" => kill(&handle, &known, &provider, e.r#ref.clone().unwrap_or_default(), &e.incarnation).await,
        other => Err(ProviderError::new("unsupported", format!("op {other} is not served"))),
    };
    e.respond(result);
}

async fn kill(handle: &AdapterHandle, known: &Known, provider: &Provider, r: String, incarnation: &str) -> Result<Outcome, ProviderError> {
    let current = known.lock().unwrap().get(&r).cloned();
    let Some(mut f) = current.filter(|f| f["incarnation"].as_str() == Some(incarnation)) else {
        return Err(ProviderError::new("not_found", format!("no terminal {r} with incarnation {incarnation:?}")));
    };
    let reply = handle.kill(r.clone()).await.map_err(unknown)?.data;
    // herdr-core's kill reply is the terminal record. When it says finished, that is observed: publish
    // the finished facts now (herdr-core does not always send a `finished` event after a kill).
    if reply["state"] == "finished" {
        f["state"] = json!("finished");
        f["status"] = Value::Null;
        f["exit"] = json!({"reason": reply["reason"], "exit_code": reply["exit_code"], "signal": reply["signal"]});
        known.lock().unwrap().insert(r, f.clone());
        let _ = provider.upsert(f.clone()).await;
        return Ok(Outcome::Observed(Some(json!({"facts": f}))));
    }
    Ok(Outcome::Accepted(Some(json!({"backend": reply}))))
}

async fn spawn(handle: &AdapterHandle, known: &Known, args: &Value) -> Result<Outcome, ProviderError> {
    let mut argv: Vec<String> = match &args["argv"] {
        Value::Null => vec![],
        Value::Array(a) if a.iter().all(Value::is_string) => a.iter().map(|v| v.as_str().unwrap().to_string()).collect(),
        _ => return Err(ProviderError::new("invalid", "argv must be strings")),
    };
    let mut env = std::collections::BTreeMap::new();
    match &args["env"] {
        Value::Null => {}
        Value::Object(m) => {
            for (k, v) in m {
                let Some(v) = v.as_str() else { return Err(ProviderError::new("invalid", "env values must be strings")) };
                if k.is_empty() || k.contains('=') || k.contains('\0') || v.contains('\0') {
                    return Err(ProviderError::new("invalid", format!("bad env entry {k:?}")));
                }
                env.insert(k.clone(), v.to_string());
            }
        }
        _ => return Err(ProviderError::new("invalid", "env must be an object of strings")),
    }
    let dim = |k: &str| args[k].as_u64().filter(|v| (1..=9999).contains(v)).map(|v| v as u16);
    if argv.is_empty() {
        // The provider's default shell for its host (9.4).
        argv = vec![std::env::var("SHELL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "/bin/sh".into())];
    }
    let req = SpawnRequest { argv, cwd: args["cwd"].as_str().map(String::from), name: args["name"].as_str().map(String::from), env, cols: dim("cols"), rows: dim("rows"), placement: args.get("placement").filter(|p| !p.is_null()).cloned() };
    match handle.spawn(req).await {
        Ok(f) => {
            // Known now: the core may attach as soon as it has the reply, before the adapter's
            // `created` event reaches us.
            let row = facts(&f);
            known.lock().unwrap().entry(f.backend_ref.clone()).or_insert_with(|| row.clone());
            Ok(Outcome::Observed(Some(json!({"facts": row}))))
        }
        Err(e) => Err(unknown(e)),
    }
}

async fn open(handle: AdapterHandle, known: Known, open: OpenRequest) {
    let (r, inc) = (open.stream.r#ref.clone(), open.stream.incarnation.clone());
    let current = known.lock().unwrap().get(&r).map(|f| f["incarnation"].as_str().unwrap_or("").to_string());
    if current.as_deref() != Some(inc.as_str()) {
        open.respond(Err(ProviderError::new("not_found", format!("no terminal {r} with incarnation {inc:?}"))));
        return;
    }
    let history = open.history_lines.min(u32::MAX as u64) as u32;
    let attached = handle.attach(r.clone(), open.cols, open.rows, history).await;
    let backend = match attached {
        Ok(s) => s,
        Err(e) => {
            open.respond(Err(unknown(e)));
            return;
        }
    };
    let initial = if backend.initial == "history" { Initial::History } else { Initial::None };
    let Some(stream) = open.respond(Ok(initial)) else {
        let _ = StreamInput::Close.send(&backend.input);
        return;
    };
    pump(stream, backend).await;
}

/// Bridge one herdr attach channel and one provider stream until either ends.
async fn pump(mut stream: ProviderStream, mut backend: adapter::Stream) {
    loop {
        tokio::select! {
            out = backend.output.recv() => match out.map(|item| item.value) {
                Some(StreamOutput::Data(d)) => {
                    let n = d.len() as u32;
                    if stream.send(&d).await.is_err() {
                        break;
                    }
                    // Credit to herdr-core only for bytes sent within the core's window.
                    if StreamInput::Consumed(n).send(&backend.input).is_err() {
                        let _ = stream.gap("backend_dropped").await;
                        break;
                    }
                }
                Some(StreamOutput::Error(error)) => {
                    let _ = stream.error(&ProviderError::new(error["kind"].as_str().unwrap_or("backend"), error["message"].as_str().unwrap_or("native stream input rejected"))).await;
                }
                Some(StreamOutput::Closed(reason)) => {
                    if reason["reason"] == "backend_disconnected" {
                        let _ = stream.gap("backend_dropped").await;
                    } else if reason.get("reason").is_some_and(Value::is_string) {
                        let _ = stream.close(reason).await;
                    } else {
                        let _ = stream.close(json!({"reason": "closed", "backend": reason})).await;
                    }
                    return;
                }
                // The adapter dropped a stream whose bounded buffer we didn't drain in time.
                None => {
                    let _ = stream.gap("viewer_slow").await;
                    break;
                }
            },
            cmd = stream.next() => match cmd {
                Some(StreamCommand::Input(d)) => {
                    if StreamInput::Data(d).send(&backend.input).is_err() {
                        let _ = stream.error(&ProviderError::new("overloaded", "input queue full; not applied")).await;
                    }
                }
                Some(StreamCommand::Resize(c, r)) => {
                    if StreamInput::Resize(c, r).send(&backend.input).is_err() {
                        let _ = stream.error(&ProviderError::new("overloaded", "resize not applied")).await;
                    }
                }
                Some(StreamCommand::Close) => {
                    let _ = StreamInput::Close.send(&backend.input);
                    let _ = stream.close(json!({"reason": "closed"})).await;
                    return;
                }
                None => break,
            },
        }
    }
    let _ = StreamInput::Close.send(&backend.input);
}

/// Exit when the core's lifetime pipe closes (managed extensions, core-protocol.md).
fn watch_lifetime() {
    if std::env::var("PRIM_EXTENSION_LIFETIME").as_deref() == Ok("stdin") {
        std::thread::spawn(|| {
            use std::io::Read;
            let mut buf = [0u8; 4096];
            let mut stdin = std::io::stdin();
            while matches!(stdin.read(&mut buf), Ok(n) if n > 0) {}
            std::process::exit(0);
        });
    }
}

#[tokio::main]
async fn main() {
    watch_lifetime();
    if let Err(e) = run().await {
        eprintln!("prim-herdr: {e}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), String> {
    let a = args()?;
    let endpoint = std::env::var("PRIM_SOCKET").map_err(|_| "PRIM_SOCKET is not set")?;
    let mut argv = vec![a.hc.clone(), "connect".into()];
    if let Some(s) = &a.session {
        argv.extend(["--session".into(), s.clone()]);
    }
    let (events_tx, mut events) = mpsc::unbounded_channel();
    let handle = adapter::herdr::start("herdr".into(), argv, events_tx);
    let known: Known = Arc::new(Mutex::new(HashMap::new()));

    // One core registration per herdr-core connection (and instance).
    loop {
        // Wait for the adapter to reach herdr-core.
        let info = loop {
            match events.recv().await.ok_or("adapter stopped")?.1 {
                AdapterEvent::Registered(info) => break info,
                AdapterEvent::Connected(false, why) => eprintln!("prim-herdr: herdr-core not reachable: {}", why.unwrap_or_default()),
                _ => {}
            }
        };
        let name = a.name.clone().unwrap_or_else(|| format!("herdr@{}", hostname()));
        let mut config = Config::new(name, info.instance.clone(), capabilities());
        config.software = info.software.clone();
        let sock = tokio::net::UnixStream::connect(&endpoint).await.map_err(|e| format!("cannot reach the core: {e}"))?;
        let (provider, mut requests) = Provider::connect(sock, config).await.map_err(|e| e.to_string())?;
        let ended = serve(&handle, &known, &provider, &mut requests, &mut events).await;
        provider.close();
        known.lock().unwrap().clear();
        match ended {
            Ended::Core(why) => return Err(format!("disconnected from the core: {why}")),
            Ended::Backend => {
                eprintln!("prim-herdr: herdr-core lost; provider marked disconnected until it is back");
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }
    }
}

enum Ended {
    Core(String),
    Backend,
}

async fn serve(
    handle: &AdapterHandle, known: &Known, provider: &Provider, requests: &mut mpsc::UnboundedReceiver<Request>,
    events: &mut mpsc::UnboundedReceiver<(String, AdapterEvent)>,
) -> Ended {
    let mut synced = false;
    loop {
        tokio::select! {
            ev = events.recv() => {
                let Some((_, ev)) = ev else { return Ended::Core("adapter stopped".into()) };
                let result = match ev {
                    AdapterEvent::Snapshot(list) => {
                        let rows: Vec<Value> = list.iter().filter(|f| f.valid()).map(facts).collect();
                        *known.lock().unwrap() = rows.iter().map(|f| (f["ref"].as_str().unwrap().to_string(), f.clone())).collect();
                        synced = true;
                        provider.full_sync(rows).await
                    }
                    AdapterEvent::Upsert(f) if synced && f.valid() => {
                        let row = facts(&f);
                        known.lock().unwrap().insert(f.backend_ref.clone(), row.clone());
                        provider.upsert(row).await
                    }
                    AdapterEvent::Removed { reference, incarnation } if synced => {
                        known.lock().unwrap().remove(&reference);
                        provider.gone(&reference, &incarnation, json!({"reason": "closed"})).await
                    }
                    AdapterEvent::Connected(false, _) => return Ended::Backend,
                    // A new herdr-core connection (maybe a new instance): register again.
                    AdapterEvent::Registered(_) => return Ended::Backend,
                    _ => Ok(()),
                };
                if let Err(e) = result {
                    return Ended::Core(e.to_string());
                }
            }
            r = requests.recv() => match r {
                Some(Request::Effect(e)) => { tokio::spawn(effect(handle.clone(), known.clone(), provider.clone(), e)); }
                Some(Request::Open(o)) => { tokio::spawn(open(handle.clone(), known.clone(), o)); }
                None => return Ended::Core(provider.failure().unwrap_or_default()),
            },
            _ = provider.closed() => return Ended::Core(provider.failure().unwrap_or_default()),
        }
    }
}

fn hostname() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname").map(|h| h.trim().to_string()).unwrap_or_else(|_| "local".into())
}
