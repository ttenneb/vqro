//! vqro-herdr: herdr-core as an out-of-process provider (`docs/spec/provider-v0.md`, section 9).
//!
//! Wraps the existing in-process adapter (`vqro::adapter::herdr`, which speaks herdr-core's framed
//! protocol through `hc connect`) and bridges its commands and events to `vqro::provider_client`.
//! Nothing herdr-specific is duplicated here: this file only translates.
//!
//! - Instance: herdr-core's `core.id` (stable across core restarts, new after its state is wiped).
//!   Local refs stay `t3`; remote refs encode Machine.id/session/terminal, never aliases.
//!   Incarnation is the terminal creation time. Remote effects are unsupported until hc has id-safe addressing.
//! - Provider availability follows the backend: when herdr-core is lost, the core connection is
//!   closed (the core marks the provider disconnected); when the adapter reconnects, the provider
//!   registers again (new generation) and sends a full sync. Nothing is resent.
//! - Effects: `terminal.spawn` (argv, cwd, name, env, cols, rows, placement) and
//!   `terminal.kill` (observed with finished facts when herdr-core's reply says finished),
//!   `resource.forget` (herdr-core `terminal.rm` of a finished terminal, then `resource.gone`). herdr-core error replies and lost replies are `backend` errors (outcome unknown),
//!   as the in-process core treats them.
//! - Streams: herdr-core attach channels with backend history as initial data. Credit goes back to
//!   herdr-core only after `provider_client` has sent the bytes within the core's window.
//!
//! `vqro-herdr [--hc PATH] [--session NAME] [--name NAME]`; connects to `$VQRO_SOCKET`. Without `--hc`:
//! `$VQRO_HERDR` (a whole command), else `hc connect` on PATH, else herdr-core's standard install.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use vqro::adapter::{self, AdapterEvent, AdapterHandle, ResourceFacts, SpawnRequest, StreamInput, StreamOutput};
use vqro::provider_client::{Config, EffectRequest, Initial, OpenRequest, Outcome, Provider, ProviderError, ProviderStream, Request, StreamCommand};
use serde_json::{json, Map, Value};
use tokio::sync::mpsc;

struct Args {
    hc: Option<String>,
    session: Option<String>,
    name: Option<String>,
    hc_args: Vec<String>,
}

fn args() -> Result<Args, String> {
    let mut a = Args { hc: None, session: None, name: None, hc_args: Vec::new() };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut value = || it.next().ok_or_else(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--hc" => a.hc = Some(value()?),
            "--hc-arg" => a.hc_args.push(value()?),
            "--session" => a.session = Some(value()?),
            "--name" => a.name = Some(value()?),
            "-h" | "--help" => {
                println!("vqro-herdr [--hc PATH] [--hc-arg ARG]... [--session NAME] [--name NAME]\nVQRO_HERDR may supply JSON argv. Paths/arguments are literal; no shell parsing.");
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
        "terminal.input": {"stream": true, "effect": false},
        "terminal.output": {"raw": true, "history": true, "repaint": "none"},
        "terminal.resize": {"owner": "latest_viewer"},
        "terminal.kill": {},
        "resource.forget": {},
        "terminal.list": {},
        "terminal.exit": {"exit_code": true},
        "status.osc7501": {"stored": true, "lossy": false},
        "events.resync": {},
        "persist.detach": {},
        "remote.machines": {"listing": true, "terminal_effects": false},
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
        "resource.forget" => forget(&handle, &known, &provider, e.r#ref.clone().unwrap_or_default(), &e.incarnation).await,
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

/// herdr-core `terminal.rm`: only a finished terminal, and only the incarnation the core named.
/// The `gone` is sent here too, so the core's inventory frees the slot even if herdr-core's own
/// `removed` event is late or missing (a second `gone` is a no-op).
async fn forget(handle: &AdapterHandle, known: &Known, provider: &Provider, r: String, incarnation: &str) -> Result<Outcome, ProviderError> {
    let current = known.lock().unwrap().get(&r).cloned();
    match current {
        Some(f) if f["incarnation"].as_str() != Some(incarnation) => return Err(ProviderError::new("not_found", format!("no terminal {r} with incarnation {incarnation:?}"))),
        Some(f) if f["state"] != "finished" => return Ok(Outcome::Rejected { kind: "invalid".into(), message: format!("terminal {r} is live; kill it first") }),
        _ => {}
    }
    match handle.invoke("resource.forget".into(), r.clone(), incarnation.into(), json!({})).await {
        Ok(_) => {}
        Err(adapter::AdapterError::RejectedKind { kind, message }) => return Ok(Outcome::Rejected { kind, message }),
        Err(e) => return Err(unknown(e)),
    }
    known.lock().unwrap().remove(&r);
    let _ = provider.gone(&r, incarnation, json!({"reason": "forgotten"})).await;
    Ok(Outcome::Observed(None))
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
    if std::env::var("VQRO_EXTENSION_LIFETIME").as_deref() == Ok("stdin") {
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
        eprintln!("vqro-herdr: {e}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), String> {
    let a = args()?;
    let endpoint = std::env::var("VQRO_SOCKET").map_err(|_| "VQRO_SOCKET is not set")?;
    let mut argv = herdr_command(a.hc.as_deref(), &a.hc_args)?;
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
                AdapterEvent::Connected(false, why) => eprintln!("vqro-herdr: herdr-core not reachable: {}", why.unwrap_or_default()),
                _ => {}
            }
        };
        let name = a.name.clone().unwrap_or_else(|| default_name(&info.instance));
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
                eprintln!("vqro-herdr: herdr-core lost; provider marked disconnected until it is back");
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
                    AdapterEvent::Consumed(ack) => { let _ = ack.send(()); Ok(()) }
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
                    AdapterEvent::Removed { reference, incarnation, reason } if synced => {
                        known.lock().unwrap().remove(&reference);
                        provider.gone(&reference, &incarnation, reason).await
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

/// Paths and arguments are never shell-parsed. VQRO_HERDR is JSON argv (or a
/// single literal executable); --hc PATH and repeated --hc-arg prefix connect.
fn herdr_command(hc: Option<&str>, extra: &[String]) -> Result<Vec<String>, String> {
    if let Some(hc) = hc {
        let mut argv = vec![hc.to_string()];
        argv.extend_from_slice(extra);
        argv.push("connect".into());
        return Ok(argv);
    }
    if !extra.is_empty() { return Err("--hc-arg requires --hc".into()); }
    if let Some(cmd) = std::env::var("VQRO_HERDR").ok().filter(|c| !c.trim().is_empty()) {
        return parse_hc_command(&cmd);
    }
    Ok(default_hc(std::env::var_os("PATH").as_deref(), &crate_home()))
}

fn crate_home() -> std::path::PathBuf {
    std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(Into::into).unwrap_or_default()
}

fn default_hc(path: Option<&std::ffi::OsStr>, home: &std::path::Path) -> Vec<String> {
    let executable = format!("hc{}", std::env::consts::EXE_SUFFIX);
    if path.is_some_and(|value| std::env::split_paths(value).any(|dir| dir.join(&executable).is_file())) {
        return vec!["hc".into(), "connect".into()];
    }
    for installed in [home.join(format!(".local/bin/herdr-core{}", std::env::consts::EXE_SUFFIX)),
                      home.join(".local/share/herdr-core/current/Contents/MacOS/herdr-core")] {
        if installed.is_file() { return vec![installed.to_string_lossy().into_owned(), "connect".into()]; }
    }
    vec!["hc".into(), "connect".into()]
}

fn parse_hc_command(cmd: &str) -> Result<Vec<String>, String> {
    let argv: Vec<String> = if cmd.trim_start().starts_with('[') {
        serde_json::from_str(cmd).map_err(|error| format!("VQRO_HERDR must be JSON argv: {error}"))?
    } else { vec![cmd.to_string()] };
    if argv.is_empty() || argv.iter().any(|arg| arg.is_empty() || arg.contains('\0')) {
        return Err("VQRO_HERDR needs nonempty literal arguments without NUL".into());
    }
    Ok(argv)
}

fn default_name(instance: &str) -> String {
    format!("herdr@{instance}")
}

#[cfg(test)]
mod platform_tests {
    use super::*;
    #[test]
    fn argv_keeps_spaced_executable_and_argument() {
        assert_eq!(parse_hc_command(r#"["/Program Files/hc","connect","--session","two words"]"#).unwrap(),
                   vec!["/Program Files/hc", "connect", "--session", "two words"]);
    }
    #[test]
    fn literal_program_and_prefix_arguments_keep_spaces() {
        assert_eq!(parse_hc_command("/Program Files/hc").unwrap(), vec!["/Program Files/hc"]);
        assert_eq!(herdr_command(Some("/SSH Tools/ssh"), &["host name".into(), "/Remote Tools/hc".into()]).unwrap(),
                   vec!["/SSH Tools/ssh", "host name", "/Remote Tools/hc", "connect"]);
        assert!(parse_hc_command("[]").is_err());
        assert!(parse_hc_command("[42]").is_err());
    }
    #[test]
    fn default_install_and_path_keep_spaces() {
        let home = tempfile::Builder::new().prefix("hc home with spaces ").tempdir().unwrap();
        let executable = home.path().join(format!(".local/bin/herdr-core{}", std::env::consts::EXE_SUFFIX));
        std::fs::create_dir_all(executable.parent().unwrap()).unwrap();
        std::fs::write(&executable, "fixture").unwrap();
        assert_eq!(default_hc(None, home.path()), vec![executable.to_string_lossy().into_owned(), "connect".into()]);
        let bin = home.path().join("bin with spaces"); std::fs::create_dir(&bin).unwrap();
        std::fs::write(bin.join(format!("hc{}", std::env::consts::EXE_SUFFIX)), "fixture").unwrap();
        let path = std::env::join_paths([&bin]).unwrap();
        assert_eq!(default_hc(Some(&path), home.path()), vec!["hc", "connect"]);
    }
    #[test]
    fn names_do_not_collide_for_distinct_backend_ids() {
        assert_ne!(default_name("backend-a"), default_name("backend-b"));
        assert_eq!(default_name("backend-a"), default_name("backend-a"));
    }
}
