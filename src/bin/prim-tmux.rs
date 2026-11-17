//! prim-tmux: the tmux backend as an out-of-process provider (`docs/spec/provider-v0.md`, section 9).
//!
//! Connects to the core at `$PRIM_SOCKET`, registers one provider for one tmux server, and publishes
//! its panes as `terminal` resources. One control client per session (tmux sends output only for panes
//! linked into a client's session); panes are re-listed after structural notifications and every few
//! seconds, because tmux has no event history or pane-exited notification. OSC 7501 status is observed
//! in the output stream, so it is lossy (`status.osc7501 {lossy: true}`): lost while paused, never
//! stored by tmux. Streams start with a labelled repaint (`capture-pane`), never replayed history; a
//! native pause ends the stream with a gap, the pane is resumed, and the viewer reopens.
//!
//! Server: `--tmux-socket PATH` attaches to an existing server (it is never killed); without it, a
//! private server is started under /tmp and killed on exit. Never the default tmux server.
//! Exit codes are not reported (tmux removes exited panes); a vanished pane is `gone` with reason
//! `exited`, or `killed` if this provider killed it.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use prim::provider_client::{Config, EffectRequest, Initial, OpenRequest, Outcome, Provider, ProviderError, Request, StreamCommand};
use prim::tmux::{self, Client, GapReason, Notification, PaneEvent, PaneId};
use serde_json::{json, Map, Value};
use tokio::sync::Notify;

/// tmux replaces control characters (tabs too) and, without a UTF-8 locale, non-ASCII bytes in format
/// output, so fields are split on a printable ASCII token that names and titles are unlikely to contain.
const SEP: &str = "|@prim@|";
const PANE_FORMAT: &str = "#{pane_id}|@prim@|#{session_name}|@prim@|#{window_id}|@prim@|#{window_name}|@prim@|#{pane_title}|@prim@|#{pane_current_path}|@prim@|#{pane_current_command}|@prim@|#{pane_pid}";
const RECONCILE_EVERY: Duration = Duration::from_secs(3);
/// Clears and homes the viewer's screen before a repaint (a screen snapshot, not replay).
const REPAINT_PREFIX: &[u8] = b"\x1b[H\x1b[2J";

struct Args {
    tmux: PathBuf,
    socket: Option<PathBuf>,
    session: Option<String>,
    name: Option<String>,
}

fn args() -> Result<Args, String> {
    let mut a = Args { tmux: "tmux".into(), socket: None, session: None, name: None };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut value = || it.next().ok_or_else(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--tmux" => a.tmux = value()?.into(),
            "--tmux-socket" => a.socket = Some(value()?.into()),
            "--session" => a.session = Some(value()?),
            "--name" => a.name = Some(value()?),
            "-h" | "--help" => {
                println!("prim-tmux [--tmux-socket PATH] [--session NAME] [--tmux BIN] [--name NAME]\nConnects to $PRIM_SOCKET.");
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument {other}")),
        }
    }
    Ok(a)
}

/// A tmux CLI call against our server (not through a control client): sessions and setup.
/// `-N`: never starts a server.
fn tmux_cli(bin: &PathBuf, socket: &PathBuf, args: &[&str]) -> Result<String, String> {
    tmux_run(bin, socket, &["-N"], args)
}

fn tmux_run(bin: &PathBuf, socket: &PathBuf, flags: &[&str], args: &[&str]) -> Result<String, String> {
    let mut cmd = std::process::Command::new(bin);
    cmd.env_clear().env("PATH", std::env::var_os("PATH").unwrap_or_else(|| "/usr/bin:/bin".into()));
    for k in ["HOME", "TERM", "LANG"] {
        if let Some(v) = std::env::var_os(k) {
            cmd.env(k, v);
        }
    }
    let out = cmd.args(flags).arg("-S").arg(socket).args(["-f", "/dev/null"]).args(args).output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

struct PaneInfo {
    session: String,
    window: String,
    facts: Value,
    status: Value,
    watcher: Option<tokio::task::JoinHandle<()>>,
}

struct State {
    clients: BTreeMap<String, Client>,
    panes: BTreeMap<String, PaneInfo>,
    killed: HashSet<String>,
    argv: HashMap<String, Vec<String>>,
    /// Open stream count per pane, for closing them when the pane goes.
    streams: HashMap<String, Vec<Arc<Notify>>>,
    resuming: HashSet<String>,
}

struct App {
    provider: Provider,
    bin: PathBuf,
    socket: PathBuf,
    spawn_session: String,
    state: Mutex<State>,
    reconcile: Notify,
    /// The tmux server this registration describes (its identity at registration).
    instance: String,
    /// Set when that server is gone or replaced: this registration ends, a new one follows.
    lost: Notify,
    lost_flag: std::sync::atomic::AtomicBool,
}

/// Apply one OSC 7501 report (Program Status Protocol rev 0.4: `key=value` pairs joined by `:`,
/// `msg`/`title` base64) to a pane's status: `{state, app?, kind?, progress?, msg?, title?, records?}`
/// for the root record, child records under `records` by id. Each report replaces its record;
/// `state=clear` removes the record and those beneath it. Returns false if the report is discarded.
fn apply_status(status: &mut Value, payload: &[u8]) -> bool {
    use base64::Engine;
    if payload.len() > 4096 {
        return false;
    }
    let Ok(text) = std::str::from_utf8(payload) else { return false };
    let mut record = Map::new();
    for pair in text.split(':') {
        let Some((k, v)) = pair.split_once('=') else { continue };
        let (k, v) = (k.trim(), v.trim());
        if k.is_empty() || k.len() > 16 || !k.bytes().all(|b| b.is_ascii_lowercase())
            || !v.bytes().all(|b| b.is_ascii_alphanumeric() || b"_.,+/=-".contains(&b))
        {
            continue; // malformed pair: skipped, the rest still counts
        }
        let value = match k {
            "msg" | "title" => {
                let engine = base64::engine::GeneralPurpose::new(&base64::alphabet::STANDARD,
                    base64::engine::GeneralPurposeConfig::new().with_decode_padding_mode(base64::engine::DecodePaddingMode::Indifferent));
                let Ok(bytes) = engine.decode(v) else { return false };
                let Ok(s) = String::from_utf8(bytes) else { return false };
                if s.chars().any(char::is_control) {
                    return false;
                }
                json!(s)
            }
            "progress" => match v.parse::<u8>() {
                Ok(p) if p <= 100 => json!(p),
                _ => continue,
            },
            _ => json!(v),
        };
        record.insert(k.to_string(), value); // last value wins
    }
    let Some(state) = record.get("state").and_then(Value::as_str).map(String::from) else { return false };
    if !matches!(state.as_str(), "idle" | "working" | "done" | "blocked" | "error" | "clear") {
        return false;
    }
    let id = record.remove("id").and_then(|v| v.as_str().map(String::from));
    if id.as_deref().is_some_and(|id| id.len() > 128 || id.split('/').any(|s| s.is_empty() || s.len() > 32)) {
        return false;
    }
    let mut records = status.get("records").cloned().unwrap_or_else(|| json!({}));
    match (id, state == "clear") {
        (None, true) => *status = Value::Null,
        (None, false) => {
            let mut root = Value::Object(record);
            if records.as_object().is_some_and(|r| !r.is_empty()) {
                root["records"] = records;
            }
            *status = root;
        }
        (Some(id), clear) => {
            let map = records.as_object_mut().unwrap();
            if clear {
                map.retain(|k, _| k != &id && !k.starts_with(&format!("{id}/")));
            } else {
                map.insert(id, Value::Object(record));
            }
            if !status.is_object() {
                *status = json!({});
            }
            if map.is_empty() {
                status.as_object_mut().unwrap().remove("records");
            } else {
                status["records"] = records;
            }
        }
    }
    true
}

fn unknown(e: tmux::Error) -> ProviderError {
    ProviderError::new("backend", format!("outcome unknown: {e}"))
}

impl App {
    fn backend_lost(&self, why: &str) {
        if !self.lost_flag.swap(true, std::sync::atomic::Ordering::Relaxed) {
            eprintln!("prim-tmux: {why}; ending this registration");
            self.lost.notify_one();
        }
    }

    fn any_client(&self) -> Option<Client> {
        self.state.lock().unwrap().clients.values().next().cloned()
    }

    fn client_for(&self, pane: &str) -> Option<(Client, String)> {
        let st = self.state.lock().unwrap();
        let info = st.panes.get(pane)?;
        Some((st.clients.get(&info.session)?.clone(), info.window.clone()))
    }

    /// Connect a control client for every session we don't have yet.
    async fn attach_sessions(self: &Arc<Self>) -> Result<(), String> {
        let list = tmux_cli(&self.bin, &self.socket, &["list-sessions", "-F", "#{session_name}"])?;
        for session in list.lines().filter(|s| !s.is_empty()) {
            if self.state.lock().unwrap().clients.contains_key(session) {
                continue;
            }
            let mut config = tmux::Config::new(self.socket.clone(), session);
            config.binary = self.bin.clone();
            let client = Client::connect(config).await.map_err(|e| e.to_string())?;
            if client.instance() != self.instance {
                // Another server behind the same socket: pane ids may be reused, so these are not our
                // resources. End this registration; the next one gets the new instance (and new ids).
                client.close().await;
                self.backend_lost("the tmux server was replaced (new instance)");
                return Err("tmux server replaced".into());
            }
            let app = self.clone();
            let mut notes = client.notifications();
            let name = session.to_string();
            tokio::spawn(async move {
                loop {
                    match notes.recv().await {
                        Ok(Notification::Disconnected { .. }) | Ok(Notification::Exit(_)) => {
                            app.state.lock().unwrap().clients.remove(&name);
                            app.reconcile.notify_one();
                            return;
                        }
                        Ok(Notification::Subscription { .. }) | Ok(Notification::Pause(_)) | Ok(Notification::Continue(_)) => {}
                        // Structural (layout, windows, sessions, renames, unknown): re-list.
                        Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => app.reconcile.notify_one(),
                        Err(_) => return,
                    }
                }
            });
            self.state.lock().unwrap().clients.insert(session.to_string(), client);
        }
        Ok(())
    }

    /// List panes and publish the differences. The first call is the full sync.
    async fn reconcile(self: &Arc<Self>, first: bool) -> Result<(), String> {
        if let Err(e) = self.attach_sessions().await {
            eprintln!("prim-tmux: sessions: {e}");
        }
        let Some(client) = self.any_client() else {
            // No control client and none could attach: the server is gone (or has no session).
            self.backend_lost("the tmux server is unreachable");
            return Err("no tmux session to attach to".into());
        };
        let lines = client.command(&["list-panes", "-a", "-F", PANE_FORMAT]).await.map_err(|e| e.to_string())?;
        let mut seen: BTreeMap<String, (String, String, Value)> = BTreeMap::new();
        for line in lines {
            let line = String::from_utf8_lossy(&line).into_owned();
            let f: Vec<&str> = line.splitn(8, SEP).collect();
            if f.len() < 8 || seen.contains_key(f[0]) {
                continue;
            }
            let argv = self.state.lock().unwrap().argv.get(f[0]).cloned().unwrap_or_default();
            let facts = json!({
                "ref": f[0], "incarnation": "", "kind": "terminal", "state": "live",
                "name": f[3], "title": f[4], "cwd": if f[5].is_empty() { Value::Null } else { json!(f[5]) },
                "argv": argv,
                "data": {"session": f[1], "window": f[2], "command": f[6], "pid": f[7].parse::<u64>().ok()},
            });
            seen.insert(f[0].to_string(), (f[1].to_string(), f[2].to_string(), facts));
        }
        let mut upserts = Vec::new();
        let mut gone = Vec::new();
        let mut start = Vec::new();
        {
            let mut st = self.state.lock().unwrap();
            let known: Vec<String> = st.panes.keys().cloned().collect();
            for pane in known {
                if !seen.contains_key(&pane) {
                    let info = st.panes.remove(&pane).unwrap();
                    if let Some(w) = info.watcher {
                        w.abort();
                    }
                    let why = if st.killed.remove(&pane) { "killed" } else { "exited" };
                    for s in st.streams.remove(&pane).unwrap_or_default() {
                        s.notify_one();
                    }
                    st.argv.remove(&pane);
                    gone.push((pane, why));
                }
            }
            for (pane, (session, window, mut facts)) in seen {
                match st.panes.get_mut(&pane) {
                    Some(info) => {
                        // Its status watcher ended with a dropped control client: watch again.
                        if info.watcher.as_ref().is_none_or(|w| w.is_finished()) {
                            start.push(pane.clone());
                        }
                        facts["status"] = info.status.clone();
                        info.session = session;
                        info.window = window;
                        if info.facts != facts {
                            info.facts = facts.clone();
                            upserts.push(facts);
                        }
                    }
                    None => {
                        facts["status"] = Value::Null;
                        st.panes.insert(pane.clone(), PaneInfo { session, window, facts: facts.clone(), status: Value::Null, watcher: None });
                        start.push(pane);
                        upserts.push(facts);
                    }
                }
            }
        }
        for pane in start {
            self.watch_status(&pane);
        }
        if first {
            let all: Vec<Value> = self.state.lock().unwrap().panes.values().map(|p| p.facts.clone()).collect();
            self.provider.full_sync(all).await.map_err(|e| e.to_string())?;
        } else {
            for f in upserts {
                self.provider.upsert(f).await.map_err(|e| e.to_string())?;
            }
        }
        for (pane, why) in gone {
            self.provider.gone(&pane, "", json!({"reason": why})).await.map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    /// Observe OSC 7501 status for one pane, and resume it after a native pause.
    fn watch_status(self: &Arc<Self>, pane: &str) {
        let Some((client, _)) = self.client_for(pane) else { return };
        let Ok(id) = PaneId::parse(pane) else { return };
        let Ok(mut stream) = client.watch(id.clone()) else { return };
        let app = self.clone();
        let name = pane.to_string();
        let task = tokio::spawn(async move {
            loop {
                match stream.recv().await {
                    PaneEvent::Osc7501(payload) => {
                        let facts = {
                            let mut st = app.state.lock().unwrap();
                            let Some(info) = st.panes.get_mut(&name) else { return };
                            if !apply_status(&mut info.status, &payload) {
                                continue;
                            }
                            info.facts["status"] = info.status.clone();
                            info.facts.clone()
                        };
                        let _ = app.provider.upsert(facts).await;
                    }
                    PaneEvent::Gap(GapReason::Paused) => app.resume(&client, &id),
                    PaneEvent::Closed => return,
                    _ => {}
                }
            }
        });
        if let Some(info) = self.state.lock().unwrap().panes.get_mut(pane) {
            info.watcher = Some(task);
        } else {
            task.abort();
        }
    }

    /// Resume a natively paused pane once (capture, repaint to watchers, continue).
    fn resume(self: &Arc<Self>, client: &Client, pane: &PaneId) {
        if !self.state.lock().unwrap().resuming.insert(pane.to_string()) {
            return;
        }
        let (app, client, pane) = (self.clone(), client.clone(), pane.clone());
        tokio::spawn(async move {
            if let Err(e) = client.resume(&pane).await {
                eprintln!("prim-tmux: resume {pane}: {e}");
            }
            app.state.lock().unwrap().resuming.remove(pane.as_str());
        });
    }

    async fn effect(self: Arc<Self>, e: EffectRequest) {
        let result = match e.op.as_str() {
            "terminal.spawn" => self.spawn(&e.args).await,
            "terminal.kill" => self.kill(e.r#ref.as_deref(), &e.incarnation).await,
            other => Err(ProviderError::new("unsupported", format!("op {other} is not served"))),
        };
        e.respond(result);
    }

    async fn spawn(self: &Arc<Self>, args: &Value) -> Result<Outcome, ProviderError> {
        let argv: Vec<String> = match &args["argv"] {
            Value::Null => vec![],
            Value::Array(a) if a.iter().all(Value::is_string) => a.iter().map(|v| v.as_str().unwrap().to_string()).collect(),
            _ => return Err(ProviderError::new("invalid", "argv must be strings")),
        };
        let client = self.state.lock().unwrap().clients.get(&self.spawn_session).cloned().ok_or_else(|| ProviderError::new("backend", "spawn session is gone"))?;
        let target = format!("{}:", self.spawn_session);
        let mut cmd: Vec<String> = ["new-window", "-d", "-P", "-F", "#{pane_id}", "-t", &target].iter().map(|s| s.to_string()).collect();
        if let Some(name) = args["name"].as_str() {
            cmd.extend(["-n".into(), name.into()]);
        }
        if let Some(cwd) = args["cwd"].as_str() {
            cmd.extend(["-c".into(), cwd.into()]);
        }
        if let Some(env) = args["env"].as_object() {
            for (k, v) in env {
                let v = v.as_str().ok_or_else(|| ProviderError::new("invalid", "env values must be strings"))?;
                if k.is_empty() || k.contains('=') {
                    return Err(ProviderError::new("invalid", "bad env name"));
                }
                cmd.extend(["-e".into(), format!("{k}={v}")]);
            }
        }
        // An empty argv runs tmux's default-shell: the host's default shell (9.4).
        if !argv.is_empty() {
            cmd.push("--".into());
            cmd.extend(argv.iter().cloned());
        }
        let refs: Vec<&str> = cmd.iter().map(String::as_str).collect();
        let pane = match client.command(&refs).await {
            Ok(lines) => String::from_utf8_lossy(lines.first().map(Vec::as_slice).unwrap_or_default()).trim().to_string(),
            Err(tmux::Error::Rejected(lines)) => {
                let message = lines.iter().map(|l| String::from_utf8_lossy(l)).collect::<Vec<_>>().join("; ");
                return Ok(Outcome::Rejected { kind: "backend".into(), message });
            }
            Err(tmux::Error::Invalid(m)) => return Err(ProviderError::new("invalid", m)),
            Err(e) => return Err(unknown(e)),
        };
        if PaneId::parse(&pane).is_err() {
            return Err(ProviderError::new("backend", format!("outcome unknown: tmux returned {pane:?}")));
        }
        self.state.lock().unwrap().argv.insert(pane.clone(), argv);
        // Publish it now, so the core can map the reply's facts.
        if let Err(e) = self.reconcile(false).await {
            eprintln!("prim-tmux: reconcile after spawn: {e}");
        }
        let facts = self.state.lock().unwrap().panes.get(&pane).map(|p| p.facts.clone());
        match facts {
            Some(f) => Ok(Outcome::Observed(Some(json!({"facts": f})))),
            // Spawned, but already gone (a program that exits at once).
            None => Ok(Outcome::Observed(Some(json!({"facts": {"ref": pane, "incarnation": "", "kind": "terminal", "state": "finished",
                "exit": {"reason": "exited"}}})))),
        }
    }

    async fn kill(self: &Arc<Self>, pane: Option<&str>, incarnation: &str) -> Result<Outcome, ProviderError> {
        let pane = pane.ok_or_else(|| ProviderError::new("invalid", "kill needs ref"))?;
        let Some((client, _)) = self.client_for(pane).filter(|_| incarnation.is_empty()) else {
            return Err(ProviderError::new("not_found", format!("no pane {pane}")));
        };
        let id = PaneId::parse(pane).map_err(|_| ProviderError::new("invalid", "bad pane id"))?;
        self.state.lock().unwrap().killed.insert(pane.to_string());
        match client.kill(&id).await {
            Ok(()) => {
                self.reconcile.notify_one();
                let mut facts = self.state.lock().unwrap().panes.get(pane).map(|p| p.facts.clone()).unwrap_or(json!({"ref": pane, "kind": "terminal"}));
                facts["state"] = json!("finished");
                facts["exit"] = json!({"reason": "killed"});
                Ok(Outcome::Observed(Some(json!({"facts": facts}))))
            }
            Err(tmux::Error::Rejected(lines)) => {
                self.state.lock().unwrap().killed.remove(pane);
                Ok(Outcome::Rejected { kind: "backend".into(), message: lines.iter().map(|l| String::from_utf8_lossy(l)).collect::<Vec<_>>().join("; ") })
            }
            Err(e) => Err(unknown(e)),
        }
    }

    async fn open(self: Arc<Self>, open: OpenRequest) {
        let pane = open.stream.r#ref.clone();
        let found = if open.stream.incarnation.is_empty() { self.client_for(&pane) } else { None };
        let Some((client, window)) = found else {
            open.respond(Err(ProviderError::new("not_found", format!("no pane {pane}"))));
            return;
        };
        let id = PaneId::parse(&pane).expect("known panes have valid ids");
        let mut events = match client.watch(id.clone()) {
            Ok(e) => e,
            Err(e) => {
                open.respond(Err(ProviderError::new("backend", e.to_string())));
                return;
            }
        };
        let gone = Arc::new(Notify::new());
        self.state.lock().unwrap().streams.entry(pane.clone()).or_default().push(gone.clone());
        let Some(mut stream) = open.respond(Ok(Initial::Repaint)) else { return };
        // The capture lands in this subscription in output order; data before it is in the screen.
        let mut repaint = Some({
            let (client, id) = (client.clone(), id.clone());
            tokio::spawn(async move { client.repaint(&id).await })
        });
        let mut started = false;
        loop {
            tokio::select! {
                ev = events.recv() => match ev {
                    PaneEvent::Repaint { screen } if !started => {
                        started = true;
                        let mut bytes = REPAINT_PREFIX.to_vec();
                        bytes.extend_from_slice(&screen);
                        if stream.send(&bytes).await.is_err() { break; }
                    }
                    PaneEvent::Data { bytes, .. } if started => {
                        if stream.send(&bytes).await.is_err() { break; }
                    }
                    PaneEvent::Gap(GapReason::Paused | GapReason::RecoveryRequested) => {
                        let _ = stream.gap("paused").await;
                        self.resume(&client, &id);
                        break;
                    }
                    PaneEvent::Gap(GapReason::SubscriberLagged(_)) => {
                        let _ = stream.gap("viewer_slow").await;
                        break;
                    }
                    PaneEvent::Closed => {
                        // The feed ended: our kill, or the session's control client went away
                        // (e.g. a command timed out under load). A pane exit arrives via `gone`.
                        if self.state.lock().unwrap().killed.contains(&pane) {
                            let _ = stream.close(json!({"reason": "killed"})).await;
                        } else {
                            let _ = stream.gap("backend_dropped").await;
                        }
                        break;
                    }
                    _ => {}
                },
                cmd = stream.next() => match cmd {
                    Some(StreamCommand::Input(bytes)) => {
                        for chunk in bytes.chunks(16 * 1024) {
                            match client.input(&id, chunk).await {
                                Ok(()) => {}
                                Err(tmux::Error::Rejected(lines)) => {
                                    let msg = lines.iter().map(|l| String::from_utf8_lossy(l)).collect::<Vec<_>>().join("; ");
                                    let _ = stream.error(&ProviderError::new("backend", msg)).await;
                                    break;
                                }
                                Err(e) => {
                                    // Maybe delivered: never claim rejected, never resend.
                                    let _ = stream.close(json!({"reason": "closed", "message": format!("input outcome unknown: {e}")})).await;
                                    break;
                                }
                            }
                        }
                    }
                    Some(StreamCommand::Resize(cols, rows)) => {
                        if let Err(e) = client.resize_window(&window, cols, rows).await {
                            let _ = stream.error(&ProviderError::new("backend", e.to_string())).await;
                        }
                    }
                    Some(StreamCommand::Close) => {
                        let _ = stream.close(json!({"reason": "closed"})).await;
                        break;
                    }
                    None => break,
                },
                _ = gone.notified() => {
                    let why = if self.state.lock().unwrap().killed.contains(&pane) { "killed" } else { "exited" };
                    let _ = stream.close(json!({"reason": why})).await;
                    break;
                }
            }
            if stream.is_closed() {
                break;
            }
            if repaint.as_ref().is_some_and(|r| r.is_finished()) {
                // A successful capture's Repaint event is already queued for us; only a failed
                // capture (pane gone, tmux error) means the promised repaint can't come.
                let failed = match repaint.take().unwrap().await {
                    Ok(Ok(())) => None,
                    Ok(Err(e)) => Some(e.to_string()),
                    Err(e) => Some(e.to_string()),
                };
                if let (Some(e), false) = (failed, started) {
                    let _ = stream.close(json!({"reason": "closed", "message": format!("repaint failed: {e}")})).await;
                    break;
                }
            }
        }
        if let Some(list) = self.state.lock().unwrap().streams.get_mut(&pane) {
            list.retain(|n| !Arc::ptr_eq(n, &gone));
        }
    }
}

fn capabilities() -> Map<String, Value> {
    json!({
        "terminal.spawn": {"env": true, "cwd": true, "name": true},
        "terminal.input": {"stream": true, "effect": false},
        "terminal.output": {"raw": true, "history": false, "repaint": "screen", "lossy_when_paused": true},
        "terminal.resize": {"owner": "latest_viewer"},
        "terminal.kill": {},
        "terminal.exit": {"exit_code": false},
        "status.osc7501": {"stored": false, "lossy": true},
        "layout.native": {},
        "identity": {"refs_survive_backend_restart": false},
    })
    .as_object()
    .unwrap()
    .clone()
}

/// Exit when the core's lifetime pipe closes (managed extensions, core-protocol.md).
fn watch_lifetime() {
    if std::env::var("PRIM_EXTENSION_LIFETIME").as_deref() == Ok("stdin") {
        std::thread::spawn(|| {
            use std::io::Read;
            let mut buf = [0u8; 4096];
            let mut stdin = std::io::stdin();
            while matches!(stdin.read(&mut buf), Ok(n) if n > 0) {}
            LIFETIME_ENDED.store(true, std::sync::atomic::Ordering::SeqCst);
            // The core ended (graceful shutdown closes this pipe before any kill): clean up exactly
            // like a normal exit, i.e. stop our private server. A configured server is never ours.
            stop_private_server();
            std::process::exit(0);
        });
    }
}

/// The private server, if this provider started one: (tmux binary, socket). Read by the lifetime
/// thread, which exits the process without running destructors.
static PRIVATE: std::sync::Mutex<Option<(PathBuf, PathBuf)>> = std::sync::Mutex::new(None);
/// Set when the lifetime pipe closed: that thread owns the exit (cleanup, then status 0).
static LIFETIME_ENDED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn stop_private_server() {
    let private = PRIVATE.lock().unwrap_or_else(|e| e.into_inner()).take();
    if let Some((bin, socket)) = private {
        let _ = tmux_cli(&bin, &socket, &["kill-server"]);
        // kill-server returns before the server has exited; it removes its socket as it goes.
        for _ in 0..40 {
            if !socket.exists() && tmux_cli(&bin, &socket, &["list-sessions"]).is_err() {
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        if let Some(dir) = socket.parent() {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

struct PrivateServer {
    bin: PathBuf,
    socket: PathBuf,
    _dir: tempfile::TempDir,
}

impl Drop for PrivateServer {
    fn drop(&mut self) {
        PRIVATE.lock().unwrap_or_else(|e| e.into_inner()).take();
        let _ = tmux_cli(&self.bin, &self.socket, &["kill-server"]);
    }
}

/// `prim-tmux --watch-private PID TMUX SOCKET`: kill the private server once its provider is gone,
/// even if the provider was SIGKILLed (the extension supervisor's stop) and its cleanup never ran.
fn watch_private(args: &[String]) -> ! {
    let (pid, tmux, socket) = (&args[0], PathBuf::from(&args[1]), PathBuf::from(&args[2]));
    let alive = || {
        if Path::new("/proc/self").exists() {
            Path::new(&format!("/proc/{pid}")).exists()
        } else {
            std::process::Command::new("/bin/kill").args(["-0", pid]).status().is_ok_and(|s| s.success())
        }
    };
    while alive() {
        std::thread::sleep(Duration::from_millis(500));
    }
    let _ = tmux_cli(&tmux, &socket, &["kill-server"]);
    // The private socket's own temporary directory (created by the provider under /tmp).
    if let Some(dir) = socket.parent().filter(|d| d.file_name().is_some_and(|n| n.to_string_lossy().starts_with("prim-tmux"))) {
        let _ = std::fs::remove_dir_all(dir);
    }
    std::process::exit(0);
}

fn spawn_private_watchdog(tmux: &PathBuf, socket: &PathBuf) {
    let Ok(exe) = std::env::current_exe() else { return };
    let _ = std::process::Command::new(exe)
        .arg("--watch-private")
        .arg(std::process::id().to_string())
        .arg(tmux)
        .arg(socket)
        .env_remove("PRIM_EXTENSION_LIFETIME")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

fn default_name(instance: &str) -> String {
    // Native server-lifetime nonce: portable and independent of display hostname.
    format!("tmux@{}", instance.rsplit(':').next().unwrap_or(instance))
}

#[cfg(test)]
mod platform_tests {
    use super::*;
    #[test]
    fn distinct_server_nonces_have_distinct_default_names() {
        assert_ne!(default_name("1:100:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"), default_name("1:100:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"));
    }
}

#[tokio::main]
async fn main() {
    let argv: Vec<String> = std::env::args().collect();
    if argv.get(1).map(String::as_str) == Some("--watch-private") && argv.len() == 5 {
        watch_private(&argv[2..]);
    }
    watch_lifetime();
    let result = run().await;
    if LIFETIME_ENDED.load(std::sync::atomic::Ordering::SeqCst) {
        // Its own cleanup stops the private server, which can end run() first: let it finish and exit.
        loop {
            std::thread::park();
        }
    }
    let code = match result {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("prim-tmux: {e}");
            1
        }
    };
    std::process::exit(code);
}

async fn run() -> Result<(), String> {
    let a = args()?;
    let endpoint = std::env::var("PRIM_SOCKET").map_err(|_| "PRIM_SOCKET is not set")?;
    let mut private = None;
    let socket = match a.socket.clone() {
        Some(s) => s,
        None => {
            let dir = tempfile::Builder::new().prefix("prim-tmux").tempdir_in("/tmp").map_err(|e| e.to_string())?;
            let socket = dir.path().join("s");
            // Our own server: the one call that may start one.
            tmux_run(&a.tmux, &socket, &[], &["new-session", "-d", "-s", "prim"])?;
            private = Some(PrivateServer { bin: a.tmux.clone(), socket: socket.clone(), _dir: dir });
            *PRIVATE.lock().unwrap_or_else(|e| e.into_inner()) = Some((a.tmux.clone(), socket.clone()));
            spawn_private_watchdog(&a.tmux, &socket);
            socket
        }
    };
    // One core registration per tmux server instance. When a configured server goes away or is
    // replaced, the registration ends (the core marks our resources unavailable), and we register
    // again once a server answers: same instance keeps ids, a new instance retires them.
    loop {
        match registration(&a, &endpoint, &socket).await {
            Ok(()) => {
                if private.is_some() {
                    return Err("the private tmux server went away".into());
                }
                // Wait for a server behind the configured socket.
                while tmux_cli(&a.tmux, &socket, &["list-sessions", "-F", "#{session_name}"]).map(|s| s.trim().is_empty()).unwrap_or(true) {
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
            }
            Err(e) => {
                drop(private);
                return Err(e);
            }
        }
    }
}

/// Register once for the current tmux server and serve until the core goes (Err) or the server is
/// gone or replaced (Ok).
async fn registration(a: &Args, endpoint: &str, socket: &PathBuf) -> Result<(), String> {
    let sessions = loop {
        match tmux_cli(&a.tmux, socket, &["list-sessions", "-F", "#{session_name}"]) {
            Ok(s) if !s.trim().is_empty() => break s,
            _ => tokio::time::sleep(Duration::from_millis(500)).await,
        }
    };
    let spawn_session = a.session.clone().or_else(|| sessions.lines().next().map(String::from)).ok_or("the tmux server has no session")?;

    // Identity: the tmux server (pid, start time and a server-lifetime nonce).
    let mut config = tmux::Config::new(socket.clone(), spawn_session.clone());
    config.binary = a.tmux.clone();
    let first = Client::connect(config).await.map_err(|e| format!("attach {spawn_session}: {e}"))?;
    let instance = first.instance().to_string();
    // Re-attached uniformly by attach_sessions (which installs the notification task).
    first.close().await;
    let name = a.name.clone().unwrap_or_else(|| default_name(&instance));
    let mut pconfig = Config::new(name, instance.clone(), capabilities());
    let version = tmux_cli(&a.tmux, socket, &["-V"]).or_else(|_| {
        std::process::Command::new(&a.tmux).arg("-V").output().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).map_err(|e| e.to_string())
    });
    pconfig.software = json!({"name": "tmux", "version": version.unwrap_or_default().trim().trim_start_matches("tmux ")});
    let stream = tokio::net::UnixStream::connect(endpoint).await.map_err(|e| format!("cannot reach the core: {e}"))?;
    let (provider, mut requests) = Provider::connect(stream, pconfig).await.map_err(|e| e.to_string())?;

    let app = Arc::new(App {
        provider: provider.clone(),
        bin: a.tmux.clone(),
        socket: socket.clone(),
        spawn_session: spawn_session.clone(),
        state: Mutex::new(State {
            clients: BTreeMap::new(),
            panes: BTreeMap::new(),
            killed: HashSet::new(),
            argv: HashMap::new(),
            streams: HashMap::new(),
            resuming: HashSet::new(),
        }),
        reconcile: Notify::new(),
        instance,
        lost: Notify::new(),
        lost_flag: std::sync::atomic::AtomicBool::new(false),
    });
    let first_sync = app.reconcile(true).await;
    let reconciler = {
        let app = app.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = app.reconcile.notified() => tokio::time::sleep(Duration::from_millis(30)).await,
                    _ = tokio::time::sleep(RECONCILE_EVERY) => {}
                }
                if let Err(e) = app.reconcile(false).await {
                    eprintln!("prim-tmux: reconcile: {e}");
                    if app.provider.is_closed() {
                        return;
                    }
                }
            }
        })
    };
    let ended = if let Err(e) = first_sync {
        if app.lost_flag.load(std::sync::atomic::Ordering::Relaxed) { Ok(()) } else { Err(e) }
    } else {
        loop {
            tokio::select! {
                r = requests.recv() => match r {
                    Some(Request::Effect(e)) => { tokio::spawn(app.clone().effect(e)); }
                    Some(Request::Open(o)) => { tokio::spawn(app.clone().open(o)); }
                    None => break Err(format!("disconnected from the core: {}", provider.failure().unwrap_or_default())),
                },
                _ = provider.closed() => break Err(format!("disconnected from the core: {}", provider.failure().unwrap_or_default())),
                _ = app.lost.notified() => break Ok(()),
            }
        }
    };
    reconciler.abort();
    provider.close();
    let clients: Vec<Client> = app.state.lock().unwrap().clients.values().cloned().collect();
    for c in clients {
        c.close().await;
    }
    ended
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn osc7501_reports_replace_records_and_bad_reports_are_discarded() {
        let mut s = Value::Null;
        // "Installing updates"
        assert!(apply_status(&mut s, b"state=working:app=brew:msg=SW5zdGFsbGluZyB1cGRhdGVz:progress=40"));
        assert_eq!(s, json!({"state": "working", "app": "brew", "msg": "Installing updates", "progress": 40}));
        assert!(apply_status(&mut s, b"state=blocked:kind=auth:junk:X=1"), "malformed pairs are skipped");
        assert_eq!(s, json!({"state": "blocked", "kind": "auth"}), "a report replaces its record");
        assert!(!apply_status(&mut s, b"state=sleeping"), "unknown state: ignored");
        assert!(!apply_status(&mut s, b"state=done:msg=a"), "msg that is not base64: the whole report is discarded");
        assert!(!apply_status(&mut s, b"msg=aGk="), "state is required");
        assert_eq!(s["state"], "blocked");
        assert!(apply_status(&mut s, b"state=working:id=build/test:title=VGVzdHM="));
        assert_eq!(s["records"]["build/test"], json!({"state": "working", "title": "Tests"}));
        assert!(apply_status(&mut s, b"state=clear:id=build"));
        assert!(s.get("records").is_none(), "clear removes the record and those beneath it");
        assert!(apply_status(&mut s, b"state=clear"));
        assert_eq!(s, Value::Null);
    }
}
