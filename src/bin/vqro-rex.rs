//! vqro-rex: Rex (Superlogical's terminal multiplexer) as an out-of-process provider (`docs/spec/provider-v0.md`,
//! section 9). A separate, optional extension: not in the default extension list.
//!
//! Connects to the core at `$VQRO_SOCKET` and to one Rex server over its local socket (`vqro::rex`): by default a
//! vqro-owned server under the core's state directory (started if needed, outliving this provider), or the user's
//! own server with `--rex-socket PATH` (never started or stopped by vqro). Every terminal block in every session is
//! published as a `terminal` resource.
//!
//! - Identity: instance = the server's `host_id` (stable across server restarts); ref = the block id (never reused);
//!   incarnation = the server's `instance_id` when the block was seen, because after a server restart the same
//!   block runs a new program (so its old vqro id retires as `replaced`).
//! - Facts come from Rex's API and events: title, cwd, status (OSC 7501 records, stored by Rex), exit code.
//! - Streams: Rex's terminal data channel. Its initial Ghostty snapshot becomes a labelled VT repaint
//!   (`vqro::tui::snapshot_to_vt`, history bounded); then the program's raw output, live, with Rex's own metadata
//!   records removed. Nothing is replayed.
//! - Input goes through `com.superlogical.terminal.write`, which takes UTF-8 text: other bytes are refused for that
//!   stream (declared: `terminal.input {bytes: "utf8"}`), never altered.
//! - Spawn takes argv, cwd, name and size; Rex has no per-terminal environment, so a spawn with `env` is rejected.
//!
//! `vqro-rex [--rex-socket PATH] [--session LABEL] [--rex BIN] [--name NAME] [--history LINES]`;
//! `vqro-rex server status|stop` administers the owned server.
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Map, Value};
use tokio::sync::Notify;
use vqro::provider_client::{Config, EffectRequest, Initial, OpenRequest, Outcome, Provider, ProviderError, Request, StreamCommand};
use vqro::rex::{self, control, data, Control, Event};

#[path = "vqro-rex/server.rs"]
mod owned_server;
#[path = "vqro-rex/facts.rs"]
mod facts;
use facts::Change;
#[cfg(all(test, unix))]
#[path = "vqro-rex/regression_tests.rs"]
mod regression_tests;

const TERMINAL: &str = "com.superlogical.terminal";
const FLAVOR: &str = "com.superlogical.terminal.shell";
const CALL_TIMEOUT: Duration = Duration::from_secs(10);
const RECONCILE_EVERY: Duration = Duration::from_secs(5);
/// Clears and homes the viewer's screen before the repaint (a screen snapshot, not replay).
const REPAINT_PREFIX: &[u8] = b"\x1b[H\x1b[2J";

struct Args {
    rex: PathBuf,
    socket: Option<PathBuf>,
    session: String,
    name: Option<String>,
    history: usize,
}

fn args() -> Result<Args, String> {
    let mut a = Args { rex: "rex".into(), socket: None, session: "vqro".into(), name: None, history: 2000 };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut value = || it.next().ok_or_else(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--rex" => a.rex = value()?.into(),
            "--rex-socket" => a.socket = Some(value()?.into()),
            "--session" => a.session = value()?,
            "--name" => a.name = Some(value()?),
            "--history" => a.history = value()?.parse().map_err(|_| "--history needs a number")?,
            "-h" | "--help" => {
                println!("vqro-rex [--rex-socket PATH] [--session LABEL] [--rex BIN] [--name NAME] [--history LINES]\n\
                    Without --rex-socket: a vqro-owned Rex server under the core's state directory (started if needed).\n\
                    --session: the Rex session new terminals go to (created when first needed; default vqro).\n\
                    Connects to $VQRO_SOCKET.\nvqro-rex server status|stop [--state-dir PATH] [--rex BIN]\n\
                    Administration requires a marked vqro-owned directory; --rex-socket external servers cannot be stopped.\n\
                    Stop checks sessions/terminals; clients outside vqro attaching after the check would be stopped too.");
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument {other}")),
        }
    }
    Ok(a)
}

struct Block {
    revision: u64,
    session: String,
    facts: Value,
}

struct State {
    /// Global allocator never resets/reuses a revision within this registration; revisions live in Block.
    next_revision: u64,
    removed: HashSet<String>,
    facts_ready: bool,
    ending: bool,
    pending: HashMap<String, facts::Pending>,
    blocks: BTreeMap<String, Block>,
    /// Session id -> label, for sessions this connection is attached to.
    sessions: HashMap<String, String>,
    killed: HashSet<String>,
    /// Open streams per block, told when the block goes.
    streams: HashMap<String, Vec<Arc<Notify>>>,
}

struct App {
    provider: Provider,
    control: Control,
    socket: PathBuf,
    /// None for --rex-socket: external servers never use owned coordination/admin paths.
    owned_dir: Option<PathBuf>,
    spawn_label: String,
    /// The server run (`instance_id`) this registration describes: every block's incarnation.
    run: String,
    history: usize,
    state: Mutex<State>,
    reconcile: Notify,
    /// Serializes reconciles (the event task and the timer may both ask).
    reconciling: tokio::sync::Mutex<()>,
    /// Order fact commits and their outgoing publications; never held during native queries.
    publishing: tokio::sync::Mutex<()>,
}

fn unknown(e: control::Error) -> ProviderError {
    ProviderError::new("backend", format!("outcome unknown: {e}"))
}

/// Rex's program-status records as vqro's OSC 7501-shaped status: the root record's fields at the top, the others
/// under `records` by id. Rex stores and validates them; unknown fields are dropped, values are not reinterpreted.
fn status_from_records(records: &[Value]) -> Value {
    let pick = |r: &Value| {
        let mut out = Map::new();
        for k in ["state", "app", "kind", "progress", "title", "msg"] {
            if let Some(v) = r.get(k).filter(|v| !v.is_null()) {
                out.insert(k.into(), v.clone());
            }
        }
        out
    };
    let mut root: Option<Map<String, Value>> = None;
    let mut children = Map::new();
    for r in records {
        match r.get("id").and_then(Value::as_str).filter(|id| !id.is_empty()) {
            None => root = Some(pick(r)),
            Some(id) => {
                children.insert(id.to_string(), Value::Object(pick(r)));
            }
        }
    }
    if root.is_none() && children.is_empty() {
        return Value::Null;
    }
    let mut status = root.unwrap_or_default();
    if !children.is_empty() {
        status.insert("records".into(), Value::Object(children));
    }
    Value::Object(status)
}

/// Validate supported fields before replacing the stored table; unknown native fields are ignored.
fn validated_status(records: &[Value]) -> Option<Value> {
    let valid = records.iter().all(|record| {
        let Some(record) = record.as_object() else { return false };
        record.get("id").is_none_or(|v| v.is_null() || v.as_str().is_some_and(|s| !s.is_empty()))
            && ["state", "app", "kind", "title", "msg"].iter().all(|key|
                record.get(*key).is_none_or(|v| v.is_null() || v.is_string()))
            && record.get("progress").is_none_or(|v| v.is_null() || v.is_number())
    });
    valid.then(|| status_from_records(records))
}

fn integer(value: &Value) -> bool { value.is_i64() || value.is_u64() }

/// A `file://` URL (OSC 7) or a plain path, as a path.
fn cwd_from_pwd(pwd: &str) -> Option<String> {
    if pwd.is_empty() {
        return None;
    }
    let Some(rest) = pwd.strip_prefix("file://") else { return Some(pwd.to_string()) };
    let path = &rest[rest.find('/')?..];
    // Percent-decoding, bytes as reported.
    let mut out = Vec::new();
    let b = path.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hex = |byte: u8| match byte {
                b'0'..=b'9' => Some(byte - b'0'),
                b'a'..=b'f' => Some(byte - b'a' + 10),
                b'A'..=b'F' => Some(byte - b'A' + 10),
                _ => None,
            };
            if let (Some(hi), Some(lo)) = (hex(b[i + 1]), hex(b[i + 2])) {
                out.push(hi * 16 + lo);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8(out).ok()
}

impl App {
    async fn block_call(&self, session: &str, block: &str, method: &str, args: Value) -> Result<Value, control::Error> {
        self.control.call(&format!("{TERMINAL}.{method}"), json!({"session_id": session, "block_id": block, "args": args})).await
    }

    async fn owned_lock(&self) -> Result<Option<std::fs::File>, control::Error> {
        match &self.owned_dir {
            Some(dir) => owned_server::lock(dir).await.map(Some).map_err(control::Error::Lost),
            None => Ok(None),
        }
    }

    async fn attach(&self, session: &str, label: &str) -> Result<(), control::Error> {
        let _lock = self.owned_lock().await?;
        self.attach_locked(session, label).await
    }

    /// Caller holds the owned lock, or this is an external server.
    async fn attach_locked(&self, session: &str, label: &str) -> Result<(), control::Error> {
        if self.state.lock().unwrap().sessions.contains_key(session) {
            return Ok(());
        }
        self.control.call("session.attach", json!({"session_id": session})).await?;
        self.state.lock().unwrap().sessions.insert(session.to_string(), label.to_string());
        Ok(())
    }

    /// Refresh stored event-derived facts too: a reconcile repairs dropped events. Failed or malformed
    /// queries retain prior facts, never invent an exit or erase a previously observed status.
    async fn refresh_facts(&self, session: &str, b: &Value, old: Option<Value>, argv: Option<Vec<String>>) -> Option<Value> {
        let id = b["block_id"].as_str().unwrap_or_default();
        let mut lifecycle_observed = old.is_some();
        let mut facts = old.unwrap_or_else(|| json!({"ref":id,"incarnation":self.run,"kind":"terminal",
            "state":"live","argv":argv,"data":{}}));
        facts["name"] = json!(b["label"].as_str().filter(|s| !s.is_empty()));
        for key in ["window_id", "placement", "host_id", "flavor"] {
            let field = match key { "window_id" => "window", "host_id" => "host", other => other };
            facts["data"][field] = b[key].clone();
        }
        facts["data"]["session"] = json!(session);
        let mut pwd_observed = false;
        for method in ["title", "pwd", "program_status", "process"] {
            let value = match self.block_call(session, id, method, json!({})).await {
                Ok(v) => v,
                Err(e) => { eprintln!("vqro-rex: refresh {id} {method}: {e}; keeping prior facts"); continue; }
            };
            match method {
                "title" if value.get("title").is_some_and(|v| v.is_string() || v.is_null()) =>
                    facts["title"] = json!(value["title"].as_str().filter(|s| !s.is_empty())),
                "pwd" => if let Some(cwd) = value["pwd"].as_str().and_then(cwd_from_pwd) {
                    facts["cwd"] = json!(cwd);
                    pwd_observed = true;
                },
                "program_status" => if let Some(status) = value["records"].as_array().and_then(|r| validated_status(r)) {
                    facts["status"] = status;
                } else { eprintln!("vqro-rex: malformed program_status {id}; keeping prior status"); },
                "process" if value.is_object() => {
                    for (native, field) in [("child", "program"), ("foreground", "foreground")] {
                        if let Some(process) = value.get(native) {
                            if process.is_null() { facts["data"][field] = Value::Null; }
                            else if let Some(name) = process.get("name").filter(|v| v.is_null() || v.is_string()) {
                                facts["data"][field] = name.clone();
                            }
                        }
                    }
                    if value.get("last_exit").is_none_or(Value::is_null) && value["child"].as_object().is_some_and(|child|
                        child.get("name").and_then(Value::as_str).is_some_and(|s| !s.is_empty())
                        || child.get("pid").and_then(Value::as_u64).is_some_and(|pid| pid > 0)) {
                        lifecycle_observed = true;
                    }
                    if !pwd_observed {
                        if let Some(cwd) = value["foreground"]["cwd"].as_str().or(value["child"]["cwd"].as_str()) {
                            facts["cwd"] = json!(cwd);
                        }
                    }
                    if integer(&value["last_exit"]["exit_code"]) {
                        lifecycle_observed = true;
                        facts["state"] = json!("finished");
                        facts["exit"] = json!({"reason":"exited","exit_code":value["last_exit"]["exit_code"]});
                    }
                }
                _ => eprintln!("vqro-rex: malformed {id} {method}; keeping prior facts"),
            }
        }
        lifecycle_observed.then_some(facts)
    }

    /// List every session's blocks and publish the differences. The first call is the full sync.
    async fn reconcile(self: &Arc<Self>, first: bool) -> Result<(), String> {
        let _one = self.reconciling.lock().await;
        let list = self.control.call("session.list", json!({})).await.map_err(|e| e.to_string())?;
        let sessions = list["sessions"].as_array().ok_or("session.list returned no sessions array; keeping prior facts")?;
        let listed: HashSet<String> = sessions.iter().map(|s| s["session_id"].as_str().filter(|s| !s.is_empty())
            .map(String::from).ok_or("session.list returned an invalid session id; keeping prior facts")).collect::<Result<_, _>>()?;
        let mut complete = HashSet::new();
        let mut seen: BTreeMap<String, (String, Value)> = BTreeMap::new();
        for s in sessions {
            let id = s["session_id"].as_str().unwrap();
            let label = s["label"].as_str().unwrap_or_default();
            if let Err(e) = self.attach(id, label).await {
                eprintln!("vqro-rex: attach {id}: {e}");
                continue;
            }
            let blocks = match self.control.call("session.list_blocks", json!({"session_id": id})).await {
                Ok(v) => v,
                Err(e) => { eprintln!("vqro-rex: list_blocks {id}: {e}; keeping prior facts"); continue; }
            };
            let Some(blocks) = blocks["blocks"].as_array().filter(|blocks| blocks.iter().all(|b|
                b["block_id"].as_str().is_some_and(|s| !s.is_empty()) && b["creator_name"].is_string())) else {
                eprintln!("vqro-rex: malformed list_blocks {id}; keeping prior facts");
                continue;
            };
            complete.insert(id.to_string());
            for b in blocks {
                if b["creator_name"] != TERMINAL {
                    continue;
                }
                if let Some(bid) = b["block_id"].as_str() {
                    seen.insert(bid.to_string(), (id.to_string(), b.clone()));
                }
            }
        }
        if first && complete.len() != listed.len() {
            return Err("incomplete initial session observation; refusing absence-based full sync".into());
        }
        let known: HashSet<String> = self.state.lock().unwrap().blocks.keys().cloned().collect();
        let mut deferred = false;
        for (bid, (session, b)) in &seen {
            let Some(observation) = self.write_facts(Change::Begin { bid:bid.clone() }).await? else { continue };
            let base = observation["revision"].as_u64();
            let token = observation["token"].as_u64();
            let old = observation["facts"].as_object().map(|_| observation["facts"].clone());
            let argv = observation["argv"].as_array().map(|a| a.iter().filter_map(Value::as_str).map(String::from).collect());
            let mut pending = facts::PendingGuard::new(self.clone(), bid.clone(), token);
            let Some(facts) = self.refresh_facts(session, b, old.clone(), argv).await else {
                eprintln!("vqro-rex: defer {bid}: no valid lifecycle observation");
                pending.clear().await;
                deferred = true;
                continue;
            };
            let applied = self.write_facts(Change::Refresh { bid:bid.clone(), session:session.clone(), base, token, facts }).await?;
            pending.clear().await;
            if applied.is_none() { self.reconcile.notify_one(); }
            deferred |= old.is_none() && applied.is_none();
        }
        if first && deferred { return Err("initial lifecycle observation incomplete; refusing full sync".into()); }
        let gone = {
            let mut st = self.state.lock().unwrap();
            st.sessions.retain(|id, _| listed.contains(id));
            known.into_iter().filter(|bid| st.blocks.get(bid).is_some_and(|b|
                !seen.contains_key(bid) && (!listed.contains(&b.session) || complete.contains(&b.session)))).collect::<Vec<_>>()
        };
        for bid in gone {
            self.write_facts(Change::Remove { bid, incarnation:self.run.clone(), reason:json!({"reason":"exited"}) }).await?;
        }
        if first { self.write_facts(Change::Sync).await?; }
        Ok(())
    }

    /// Apply one Rex event. Structural events ask for a reconcile; block events update facts directly.
    async fn event(self: &Arc<Self>, e: Value) {
        let (kind, payload) = (e["type"].as_str().unwrap_or_default(), &e["payload"]);
        let (kind, payload) = if kind == "session_event" {
            (payload["event"]["type"].as_str().unwrap_or_default().to_string(), payload["event"]["payload"].clone())
        } else {
            (kind.to_string(), payload.clone())
        };
        if kind != "block_event" {
            if matches!(kind.as_str(), "session_created" | "session_destroyed" | "block_created" | "block_closed"
                | "window_created" | "window_closed" | "session_view_changed" | "block_label_changed") || kind.contains("label") {
                self.reconcile.notify_one();
            }
            return;
        }
        let Some(bid) = payload["block_id"].as_str() else { return };
        let (name, body) = (payload["name"].as_str().unwrap_or_default(), &payload["payload"]);
        if !matches!(name, "title_changed" | "pwd_changed" | "child_exited" | "program_status_changed" | "program_status_removed" | "process_changed") { return; }
        if let Err(error) = self.write_facts(Change::Event { bid:bid.to_string(), name:name.to_string(), body:body.clone() }).await {
            eprintln!("vqro-rex: fact owner: {error}");
            return;
        }
        if matches!(name, "program_status_changed" | "program_status_removed") {
            let (app, bid) = (self.clone(), bid.to_string());
            tokio::spawn(async move { app.refresh_status(&bid).await });
        }
    }

    async fn refresh_status(self: &Arc<Self>, bid: &str) {
        let Some((session, revision)) = ({
            let st = self.state.lock().unwrap();
            st.blocks.get(bid).map(|b| (b.session.clone(), b.revision))
        }) else { return };
        let Ok(s) = self.block_call(&session, bid, "program_status", json!({})).await else { return };
        let Some(records) = s["records"].as_array() else {
            eprintln!("vqro-rex: malformed program_status {bid}; keeping prior status");
            return;
        };
        let Some(status) = validated_status(records) else {
            eprintln!("vqro-rex: invalid program_status {bid}; keeping prior status");
            return;
        };
        if let Err(error) = self.write_facts(Change::Status { bid:bid.to_string(), base:revision, status }).await {
            eprintln!("vqro-rex: fact owner: {error}");
        }
    }

    async fn effect(self: Arc<Self>, e: EffectRequest) {
        let result = match e.op.as_str() {
            "terminal.spawn" => self.spawn(&e.args).await,
            "terminal.kill" => self.kill(e.r#ref.as_deref(), &e.incarnation).await,
            "resource.forget" => self.forget(e.r#ref.as_deref(), &e.incarnation).await,
            other => Err(ProviderError::new("unsupported", format!("op {other} is not served"))),
        };
        if let Err(error) = self.write_facts(Change::Reply { request:e, result }).await {
            eprintln!("vqro-rex: effect publication: {error}");
        }
    }

    /// The session new terminals go to (created and attached when first needed).
    async fn spawn_session(&self) -> Result<String, control::Error> {
        let existing = self.state.lock().unwrap().sessions.iter().find(|(_, l)| **l == self.spawn_label).map(|(id, _)| id.clone());
        if let Some(id) = existing {
            return Ok(id);
        }
        let list = self.control.call("session.list", json!({})).await?;
        let found = list["sessions"].as_array().into_iter().flatten()
            .find(|s| s["label"] == self.spawn_label.as_str()).and_then(|s| s["session_id"].as_str().map(String::from));
        let id = match found {
            Some(id) => id,
            None => self.control.call("session.create", json!({"label": self.spawn_label})).await?["session_id"]
                .as_str().map(String::from).ok_or_else(|| control::Error::Lost("session.create returned no id".into()))?,
        };
        self.attach_locked(&id, &self.spawn_label).await?;
        Ok(id)
    }

    async fn spawn(self: &Arc<Self>, args: &Value) -> Result<Outcome, ProviderError> {
        let dimension = |key: &str, fallback: u64| -> Result<u64, ProviderError> {
            match args.get(key) {
                None | Some(Value::Null) => Ok(fallback),
                Some(v) => v.as_u64().filter(|n| (1..=9999).contains(n)).ok_or_else(|| ProviderError::new("invalid", format!("{key} must be 1..9999"))),
            }
        };
        let (cols, rows) = (dimension("cols", 80)?, dimension("rows", 24)?);
        let argv: Vec<String> = match &args["argv"] {
            Value::Null => vec![],
            Value::Array(a) if a.iter().all(Value::is_string) => a.iter().map(|v| v.as_str().unwrap().to_string()).collect(),
            _ => return Err(ProviderError::new("invalid", "argv must be strings")),
        };
        if args["env"].as_object().is_some_and(|e| !e.is_empty()) {
            return Ok(Outcome::Rejected { kind: "unsupported".into(),
                message: "rex has no per-terminal environment; nothing was started (set variables in the command instead)".into() });
        }
        for key in ["name", "cwd"] {
            if !(args[key].is_null() || args[key].is_string()) {
                return Err(ProviderError::new("invalid", format!("{key} must be a string")));
            }
        }
        if facts::argv_size(&argv) > facts::MAX_ARGV_BYTES {
            return Err(ProviderError::new("invalid", "argv exceeds the provider's 64 KiB storage budget; nothing was created"));
        }
        let owned_lock = self.owned_lock().await.map_err(unknown)?;
        let session = self.spawn_session().await.map_err(unknown)?;
        let mut options = json!({"size": {"columns": cols, "rows": rows}});
        if !argv.is_empty() {
            // argv means argv: no shell in between (Rex otherwise runs it through the login shell).
            options["shell"] = json!("none");
            options["command"] = json!(argv);
        }
        if let Some(cwd) = args["cwd"].as_str() {
            options["cwd"] = json!(cwd);
        }
        let mut block = json!({"flavor": FLAVOR, "options": options});
        if let Some(name) = args["name"].as_str() {
            block["label"] = json!(name);
        }
        let reply = match self.control.call("session.new_window", json!({"session_id": session, "layout": {"block": block}, "focus": false})).await {
            Ok(r) => r,
            Err(control::Error::Rejected { code, message }) => return Ok(Outcome::Rejected { kind: "backend".into(), message: format!("{code}: {message}") }),
            Err(e) => return Err(unknown(e)),
        };
        let bid = reply["block_ids"][0].as_str().ok_or_else(|| ProviderError::new("backend", format!("outcome unknown: rex returned {reply}")))?.to_string();
        drop(owned_lock);
        let reservation = self.write_facts(Change::Created { bid:bid.clone(), argv }).await
            .map_err(|e| ProviderError::new("backend", format!("terminal created: ref {bid}; pending metadata unavailable: {e}; do not retry spawn")))?;
        let mut pending = facts::PendingGuard::creator(self.clone(), bid.clone(), reservation.as_ref().and_then(|v| v["creator_token"].as_u64()));
        if let Err(e) = self.reconcile(false).await {
            eprintln!("vqro-rex: reconcile after spawn: {e}");
        }
        let snapshot = self.write_facts(Change::Snapshot { bid:bid.clone() }).await;
        pending.clear().await; // only this spawn completes its creator ownership, after its observation attempt
        let facts = snapshot.map_err(|e| ProviderError::new("backend", e))?
            .ok_or_else(|| ProviderError::new("backend", format!("terminal created: ref {bid}; follow-up lifecycle observation unavailable; outcome unknown, do not retry spawn")))?;
        Ok(Outcome::Observed(Some(json!({"facts": facts}))))
    }

    async fn kill(self: &Arc<Self>, bid: Option<&str>, incarnation: &str) -> Result<Outcome, ProviderError> {
        let bid = bid.ok_or_else(|| ProviderError::new("invalid", "kill needs ref"))?;
        let (session, base) = self.state.lock().unwrap().blocks.get(bid).filter(|_| incarnation == self.run).map(|b| (b.session.clone(), b.revision))
            .ok_or_else(|| ProviderError::new("not_found", format!("no terminal {bid}")))?;
        match self.control.call("block.close", json!({"session_id": session, "block_id": bid})).await {
            Ok(_) => {
                self.reconcile.notify_one();
                match self.write_facts(Change::Kill { bid:bid.to_string(), base }).await.map_err(|e| ProviderError::new("backend", e))? {
                    Some(facts) => Ok(Outcome::Observed(Some(json!({"facts":facts})))),
                    None => Ok(Outcome::Accepted(None)), // acknowledgement, not a current lifecycle witness
                }
            }
            Err(control::Error::Rejected { code, message }) => {
                Ok(Outcome::Rejected { kind: "backend".into(), message: format!("{code}: {message}") })
            }
            Err(e) => Err(unknown(e)),
        }
    }

    /// Rex closes a terminal's block itself after most exits; one that stays open after a quick failure is live in
    /// Rex (kill it first). Forget only clears a finished record the core still holds.
    async fn forget(self: &Arc<Self>, bid: Option<&str>, incarnation: &str) -> Result<Outcome, ProviderError> {
        let bid = bid.ok_or_else(|| ProviderError::new("invalid", "forget needs ref"))?;
        if incarnation == self.run && self.state.lock().unwrap().blocks.contains_key(bid) {
            return Ok(Outcome::Rejected { kind: "invalid".into(), message: format!("terminal {bid} is still open in rex; kill it first") });
        }
        self.write_facts(Change::Remove { bid:bid.to_string(), incarnation:incarnation.to_string(), reason:json!({"reason":"forgotten"}) })
            .await.map_err(|e| ProviderError::new("unavailable", e))?;
        Ok(Outcome::Observed(None))
    }

    /// Size the terminal for this viewer: claim the block (Rex follows its owner's size), then resize.
    async fn size(&self, session: &str, bid: &str, cols: u16, rows: u16) -> Result<(), control::Error> {
        let _lock = self.owned_lock().await?;
        self.size_locked(session, bid, cols, rows).await
    }

    async fn size_locked(&self, session: &str, bid: &str, cols: u16, rows: u16) -> Result<(), control::Error> {
        self.control.call("session.own_block", json!({"session_id": session, "block_ids": [bid], "mode": "take"})).await?;
        // Ownership already changed. Even a definite resize refusal is not a no-effect result for this
        // two-step operation; disclose that partial effect instead of emitting a rejected stream.error.
        self.block_call(session, bid, "resize", json!({"columns": cols, "rows": rows})).await.map(|_| ())
            .map_err(|e| control::Error::Lost(format!("ownership acquired; resize outcome unavailable: {e}")))
    }

    async fn open(self: Arc<Self>, open: OpenRequest) {
        let bid = open.stream.r#ref.clone();
        let found = (open.stream.incarnation == self.run).then(|| self.state.lock().unwrap().blocks.get(&bid).map(|b| b.session.clone())).flatten();
        let Some(session) = found else {
            open.respond(Err(ProviderError::new("not_found", format!("no terminal {bid}"))));
            return;
        };
        let owned_lock = match self.owned_lock().await {
            Ok(lock) => lock,
            Err(e) => { open.respond(Err(unknown(e))); return; }
        };
        if let Err(e) = self.size_locked(&session, &bid, open.cols, open.rows).await {
            open.respond(Err(unknown(e)));
            return;
        }
        let mut channel = match rex::open_data(&self.socket, &self.control, &session, &bid, CALL_TIMEOUT).await {
            Ok(c) => c,
            Err(e) => {
                open.respond(Err(ProviderError::new("backend", e)));
                return;
            }
        };
        // The repaint: the snapshot at the start of the channel, as VT. Output after it is live and complete.
        drop(owned_lock);
        let mut snapshot = data::Snapshot::default();
        let mut pong = None;
        let (repaint, following) = loop {
            match channel.reader.next(&mut pong).await {
                Ok(Some(rex::ws::Message::Binary(b))) => match snapshot.push(&b) {
                    Ok(Some(done)) => break done,
                    Ok(None) => {}
                    Err(e) => {
                        open.respond(Err(ProviderError::new("backend", e)));
                        return;
                    }
                },
                Ok(Some(rex::ws::Message::Text(_))) => {}
                Ok(None) => {
                    if let Some(p) = pong.take() {
                        let _ = channel.writer.pong(&p).await;
                    }
                }
                Err(e) => {
                    open.respond(Err(ProviderError::new("backend", format!("rex data channel: {e}"))));
                    return;
                }
            }
        };
        let history = usize::try_from(open.history_lines).unwrap_or(usize::MAX).min(self.history);
        let repaint = match tokio::task::spawn_blocking(move || vqro::tui::snapshot_to_vt(&repaint, history)).await {
            Ok(Ok(vt)) => vt,
            Ok(Err(e)) => {
                open.respond(Err(ProviderError::new("backend", format!("rex snapshot could not be decoded: {e}"))));
                return;
            }
            Err(e) => {
                open.respond(Err(ProviderError::new("backend", e.to_string())));
                return;
            }
        };
        // Publish the decode's bounded-history outcome through the facts owner, never as terminal text.
        match self.write_facts(Change::Repaint { bid:bid.clone(), history_truncated:repaint.history_truncated, restored_rows:repaint.restored_rows }).await {
            Ok(Some(_)) => {}
            Ok(None) => { open.respond(Err(ProviderError::new("not_found", "terminal disappeared during snapshot decode"))); return; }
            Err(error) => { open.respond(Err(ProviderError::new("backend", error))); return; }
        }
        let gone = Arc::new(Notify::new());
        self.state.lock().unwrap().streams.entry(bid.clone()).or_default().push(gone.clone());
        let Some(mut stream) = open.respond(Ok(Initial::Repaint)) else { return };
        let mut metadata = data::Metadata::new(channel.nonce);
        let mut first = REPAINT_PREFIX.to_vec();
        first.extend_from_slice(&repaint.vt);
        let mut ok = stream.send(&first).await.is_ok();
        if ok {
            match metadata.feed(&following) {
                Ok((out, _)) if !out.is_empty() => ok = stream.send(&out).await.is_ok(),
                Ok(_) => {}
                Err(_) => ok = false,
            }
        }
        // Input goes to Rex in order on its own task, so a long paste never stops output from draining.
        let (input_tx, mut input_rx) = tokio::sync::mpsc::channel::<String>(4);
        let (fail_tx, mut fail_rx) = tokio::sync::mpsc::unbounded_channel::<(bool, String)>();
        let writer = {
            let (app, session, bid) = (self.clone(), session.clone(), bid.clone());
            tokio::spawn(async move {
                while let Some(text) = input_rx.recv().await {
                    match app.block_call(&session, &bid, "write", json!({"data": text})).await {
                        Ok(_) => {}
                        Err(control::Error::Rejected { code, message }) => {
                            let _ = fail_tx.send((false, format!("{code}: {message}")));
                        }
                        Err(e) => {
                            // Maybe delivered: never claim rejected, never resend.
                            let _ = fail_tx.send((true, format!("input outcome unknown: {e}")));
                            return;
                        }
                    }
                }
            })
        };
        // Bytes of a UTF-8 character split across input chunks wait for the rest.
        let mut partial: Vec<u8> = Vec::new();
        let mut input_open = true;
        let (data_tx, mut data_rx) = tokio::sync::mpsc::channel::<Result<Vec<u8>, String>>(8);
        let reader = tokio::spawn(async move {
            let mut pong = None;
            loop {
                match channel.reader.next(&mut pong).await {
                    Ok(Some(rex::ws::Message::Binary(b))) => {
                        if data_tx.send(Ok(b)).await.is_err() { break }
                    }
                    Ok(Some(rex::ws::Message::Text(_))) => {}
                    Ok(None) => {
                        if let Some(p) = pong.take() {
                            if channel.writer.pong(&p).await.is_err() { break }
                        }
                    }
                    Err(e) => {
                        let _ = data_tx.send(Err(e.to_string())).await;
                        break;
                    }
                }
            }
            // A normal close, so Rex doesn't hold the stream for a resume.
            let _ = channel.writer.close().await;
        });
        while ok {
            tokio::select! {
                failure = fail_rx.recv() => match failure {
                    Some((false, msg)) => { let _ = stream.error(&ProviderError::new("backend", msg)).await; }
                    Some((true, msg)) => {
                        let _ = stream.close(json!({"reason": "closed", "message": msg})).await;
                        break;
                    }
                    None => {}
                },
                chunk = data_rx.recv() => match chunk {
                    Some(Ok(bytes)) => match metadata.feed(&bytes) {
                        Ok((out, _records)) => {
                            // Rex's own records (resize, theme, exit) are not program output; sizes and exits
                            // reach vqro through facts and events instead.
                            if !out.is_empty() && stream.send(&out).await.is_err() { break }
                        }
                        Err(e) => {
                            let _ = stream.close(json!({"reason": "closed", "message": e})).await;
                            break;
                        }
                    },
                    Some(Err(_)) | None => {
                        // The channel ended: the block closed (exit or kill), or Rex dropped it.
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        self.reconcile.notify_one();
                        let still = self.state.lock().unwrap().blocks.contains_key(&bid);
                        if still {
                            let _ = stream.gap("backend_dropped").await;
                        } else {
                            let why = if self.state.lock().unwrap().killed.contains(&bid) { "killed" } else { "exited" };
                            let _ = stream.close(json!({"reason": why})).await;
                        }
                        break;
                    }
                },
                cmd = stream.next() => match cmd {
                    Some(StreamCommand::Input(bytes)) => {
                        partial.extend_from_slice(&bytes);
                        let valid = match std::str::from_utf8(&partial) {
                            Ok(_) => partial.len(),
                            Err(e) if e.error_len().is_none() => e.valid_up_to(),
                            Err(_) => {
                                partial.clear();
                                let _ = stream.error(&ProviderError::new("unsupported", "rex takes text input only (UTF-8); these bytes were not sent")).await;
                                continue;
                            }
                        };
                        let text = String::from_utf8(partial.drain(..valid).collect()).unwrap();
                        if !text.is_empty() && input_tx.send(text).await.is_err() { break }
                    }
                    Some(StreamCommand::Resize(cols, rows)) => {
                        if let Err(e) = self.size(&session, &bid, cols, rows).await {
                            match e {
                                control::Error::Rejected { .. } => { let _ = stream.error(&ProviderError::new("backend", e.to_string())).await; }
                                control::Error::Lost(_) | control::Error::Timeout | control::Error::VerificationRefused(_) => {
                                    eprintln!("vqro-rex: resize outcome unknown: {e}");
                                    // stream.error always means rejected, regardless of kind. A backend gap
                                    // reports lost certainty without asserting that ownership/resize had no effect.
                                    let _ = stream.gap("backend_dropped").await;
                                }
                            }
                        }
                    }
                    Some(StreamCommand::Close) => {
                        let _ = stream.close(json!({"reason": "closed"})).await;
                        input_open = false;
                        break;
                    }
                    None => { input_open = false; break; }
                },
                _ = gone.notified() => {
                    let why = if self.state.lock().unwrap().killed.contains(&bid) { "killed" } else { "exited" };
                    let _ = stream.close(json!({"reason": why})).await;
                    input_open = false;
                    break;
                }
            }
            if stream.is_closed() {
                break;
            }
        }
        reader.abort();
        // A gap ends output, not input: deliver input the core already sent, until it closes the stream (bounded).
        if input_open {
            let drain = async {
                while let Some(cmd) = stream.next().await {
                    match cmd {
                        StreamCommand::Input(bytes) => {
                            partial.extend_from_slice(&bytes);
                            if let Ok(text) = std::str::from_utf8(&partial) {
                                if input_tx.send(text.to_string()).await.is_err() { return }
                                partial.clear();
                            }
                        }
                        StreamCommand::Close => return,
                        StreamCommand::Resize(..) => {}
                    }
                }
            };
            let _ = tokio::time::timeout(Duration::from_secs(30), drain).await;
        }
        drop(input_tx);
        let _ = writer.await;
        if let Some(list) = self.state.lock().unwrap().streams.get_mut(&bid) {
            list.retain(|n| !Arc::ptr_eq(n, &gone));
        }
    }
}

fn capabilities() -> Map<String, Value> {
    json!({
        "terminal.spawn": {"env": false, "cwd": true, "name": true},
        "terminal.input": {"stream": true, "effect": false, "bytes": "utf8"},
        "terminal.output": {"raw": true, "history": false, "repaint": "screen", "lossy_when_paused": false},
        "terminal.resize": {"owner": "latest_viewer", "scope": "terminal"},
        "terminal.kill": {},
        "resource.forget": {},
        "terminal.exit": {"exit_code": true},
        "status.osc7501": {"stored": true, "lossy": false},
        "layout.native": {},
        "identity": {"refs_survive_backend_restart": true},
    })
    .as_object()
    .unwrap()
    .clone()
}

/// Exit when the core's lifetime pipe closes (managed extensions, core-protocol.md).
fn watch_lifetime() {
    if std::env::var("VQRO_EXTENSION_LIFETIME").as_deref() == Ok("stdin") {
        std::thread::spawn(|| {
            use std::io::Read;
            let mut buf = [0u8; 4096];
            let mut stdin = std::io::stdin();
            while matches!(stdin.read(&mut buf), Ok(n) if n > 0) {}
            // Only the provider belongs to this lifetime. Rex and its terminals do not.
            std::process::exit(0);
        });
    }
}

fn main() {
    let argv: Vec<String> = std::env::args().collect();
    let server_command = argv.get(1).map(String::as_str) == Some("server");
    let mut builder = if server_command { tokio::runtime::Builder::new_current_thread() } else { tokio::runtime::Builder::new_multi_thread() };
    let runtime = builder.enable_all().build().expect("rex runtime");
    let result = if server_command {
        runtime.block_on(owned_server::command(&argv[2..]))
    } else {
        watch_lifetime();
        runtime.block_on(run())
    };
    std::process::exit(match result {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("vqro-rex: {e}");
            1
        }
    });
}

async fn run() -> Result<(), String> {
    let a = args()?;
    let endpoint = std::env::var("VQRO_SOCKET").map_err(|_| "VQRO_SOCKET is not set")?;
    let (socket, owned_dir) = match a.socket.clone() {
        Some(s) => (s, None),
        None => {
            let dir = owned_server::dir(None, false).await?;
            let socket = {
                let _lock = owned_server::lock(&dir).await?;
                owned_server::ensure(&a.rex, &dir)?
            };
            (socket, Some(dir))
        }
    };
    loop {
        registration(&a, &endpoint, &socket, owned_dir.as_deref()).await?;
        // The server went away (stopped, restarted or replaced). Wait for it; never start the user's server, and
        // don't restart an owned server someone stopped on purpose.
        loop {
            let lock = match &owned_dir { Some(dir) => Some(owned_server::lock(dir).await?), None => None };
            let reachable = match rex::transport::connect(&socket).await {
                Ok(_) => true,
                Err(error) if vqro::transport::is_server_verification_error(&error) =>
                    return Err(format!("Rex endpoint verification refused at {}: {error}; reconnect stopped", socket.display())),
                Err(_) => false,
            };
            drop(lock);
            if reachable { break; }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }
}

/// Verification refusal is permanent for this attempt: never normalize/bypass it or loop before registration.
async fn connect_control<T, F, Fut>(endpoint: &Path, mut attempt: F) -> Result<T, String>
where F: FnMut() -> Fut, Fut: std::future::Future<Output = Result<Result<T, control::Error>, String>> {
    loop {
        // Local setup/lock errors stay fatal; only native transient connection failures are retried.
        match attempt().await? {
            Ok(value) => return Ok(value),
            Err(control::Error::VerificationRefused(reason)) => return Err(format!(
                "Rex endpoint verification refused at {}: {reason}; startup stopped; the shared client requires the exact user SID as pipe owner", endpoint.display())),
            Err(error) => {
                eprintln!("vqro-rex: connect {}: {error}", endpoint.display());
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
    }
}

/// One registration for one run of the Rex server. Ok when that run ended (a new registration follows).
async fn registration(a: &Args, endpoint: &str, socket: &Path, owned_dir: Option<&Path>) -> Result<(), String> {
    let (control, mut events) = connect_control(socket, || async {
        let lock = match owned_dir {
            Some(dir) => Some(owned_server::lock(dir).await?),
            None => None,
        };
        let result = Control::connect(socket, "vqro-rex", CALL_TIMEOUT).await;
        drop(lock);
        Ok(result)
    }).await?;
    let boot = control.call("session.bootstrap", json!({"event_scope": "all", "attach_session_ids": []})).await.map_err(|e| e.to_string())?;
    let status = &boot["server_status"];
    let run = status["instance_id"].as_str().ok_or("rex reported no instance_id")?.to_string();
    let instance = status["host_id"].as_str().map(String::from).unwrap_or_else(|| run.clone());
    let version = control.call("server.version", json!({})).await.ok();
    let name = a.name.clone().unwrap_or_else(|| format!("rex@{}", instance.split('-').next().unwrap_or(&instance)));
    let mut config = Config::new(name, instance, capabilities());
    config.software = json!({"name": "rex", "version": version.as_ref().and_then(|v| v["version"].as_str()).unwrap_or_default(),
        "api_revision": control.welcome["api_revision"]});
    let core = vqro::transport::connect(endpoint).await.map_err(|e| format!("cannot reach the core: {e}"))?;
    let (provider, mut requests) = Provider::connect(core, config).await.map_err(|e| e.to_string())?;
    let app = Arc::new(App {
        provider: provider.clone(),
        control: control.clone(),
        socket: socket.to_path_buf(),
        owned_dir: owned_dir.map(Path::to_path_buf),
        spawn_label: a.session.clone(),
        run,
        history: a.history,
        state: Mutex::new(State { next_revision:0, removed:HashSet::new(), facts_ready:false, ending:false, pending:HashMap::new(), blocks: BTreeMap::new(), sessions: HashMap::new(), killed: HashSet::new(),
            streams: HashMap::new() }),
        reconcile: Notify::new(),
        reconciling: tokio::sync::Mutex::new(()),
        publishing: tokio::sync::Mutex::new(()),
    });
    if let Err(error) = app.reconcile(true).await {
        let _ = app.write_facts(Change::End).await;
        provider.close();
        control.close().await;
        return Err(error);
    }
    let events_task = {
        let app = app.clone();
        tokio::spawn(async move {
            while let Some(e) = events.recv().await {
                match e {
                    Event::Event(v) => app.event(v).await,
                    Event::Lagged => app.reconcile.notify_one(),
                }
            }
        })
    };
    let reconciler = {
        let app = app.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = app.reconcile.notified() => tokio::time::sleep(Duration::from_millis(30)).await,
                    _ = tokio::time::sleep(RECONCILE_EVERY) => {}
                }
                if let Err(e) = app.reconcile(false).await {
                    eprintln!("vqro-rex: reconcile: {e}");
                    if app.provider.is_closed() || app.control.is_closed() {
                        return;
                    }
                }
            }
        })
    };
    let ended = loop {
        tokio::select! {
            r = requests.recv() => match r {
                Some(Request::Effect(e)) => { tokio::spawn(app.clone().effect(e)); }
                Some(Request::Open(o)) => { tokio::spawn(app.clone().open(o)); }
                None => break Err(format!("disconnected from the core: {}", provider.failure().unwrap_or_default())),
            },
            _ = provider.closed() => break Err(format!("disconnected from the core: {}", provider.failure().unwrap_or_default())),
            why = control.closed() => { eprintln!("vqro-rex: rex server connection ended: {why}; ending this registration"); break Ok(()) }
        }
    };
    reconciler.abort();
    events_task.abort();
    let _ = app.write_facts(Change::End).await;
    provider.close();
    control.close().await;
    ended
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rex_status_records_become_vqro_status() {
        assert_eq!(status_from_records(&[]), Value::Null);
        let s = status_from_records(&[json!({"state": "working", "app": "cargo", "progress": 40, "updated_at": "x"}),
            json!({"id": "build/test", "state": "blocked", "kind": "question"})]);
        assert_eq!(s, json!({"state": "working", "app": "cargo", "progress": 40,
            "records": {"build/test": {"state": "blocked", "kind": "question"}}}));
    }

    #[test]
    fn pwd_urls_and_paths() {
        assert_eq!(cwd_from_pwd("file://host/home/a%20b"), Some("/home/a b".into()));
        assert_eq!(cwd_from_pwd("/tmp"), Some("/tmp".into()));
        assert_eq!(cwd_from_pwd(""), None);
    }

    #[test]
    fn capabilities_say_what_rex_does_not() {
        let c = capabilities();
        assert_eq!(c["terminal.spawn"]["env"], false, "rex has no per-terminal environment");
        assert_eq!(c["terminal.input"]["bytes"], "utf8");
        assert_eq!(c["status.osc7501"]["stored"], true);
    }
}
