//! vqro-claude-hook: the tier-1 Claude Code plugin (`coordination-v0.md` §7, `agent-v0.md` §2.2).
//!
//! Claude Code runs this binary as a hook command. It never types into the terminal and never touches the editor.
//! It pulls the agent's inbox at Claude Code's own safe points and reports only what it has evidence for:
//!
//! | Hook | Point | What it does |
//! |---|---|---|
//! | `SessionStart` (`startup`, `resume`) | — | new consumer; `reconcile` (open claims become `unknown`); status unknown |
//! | `SessionStart` (`compact`, `clear`) | — | same process and consumer: nothing is reconciled |
//! | `UserPromptSubmit`, `PreToolUse` | — | status working; marks the session busy and retires an armed idle waiter |
//! | `PermissionRequest` | — | status blocked |
//! | `PostToolBatch` | `turn` | `take {at: turn}` → `additionalContext` before the next model call |
//! | `Stop` | `settle` | `take {at: settle}` → `additionalContext` (the turn continues once) |
//! | `Stop`, `Notification` `idle_prompt` (`wait`, `asyncRewake`, interactive only) | `idle` | waits for the inbox wake, takes, exits 2 → Claude wakes |
//! | `StopFailure`, `Notification` (`idle_prompt`) | — | status error / idle |
//! | `SessionEnd` | — | last chance to turn transcript evidence into `read` |
//!
//! The endpoint's declaration (written by its launcher in `doc/agents/terminals/<rN>`) is
//! `pull {points: [turn, settle, idle], insert: context, evidence: none}`. Senders must accept `render: ["context"]`.
//! - A hand-off is a system reminder with no durable entry at that moment. It is reported `supplied` only after the
//!   hook's output has been written.
//! - `read` follows only from the transcript: the plugin's own context entry carrying a per-hand-off nonce (known
//!   only to this plugin, never to senders), then the request payload carrying that nonce, then a finished
//!   assistant entry. Tool results, human prompts and quoted text are never evidence.
//! - Content is rendered as a quote under the plugin's own header, so a sender cannot forge another header.
//! - Hooks inside subagents (`agent_id`) are ignored: their context is not the agent's.
//!
//! Wire: `<prefix>inbox.window/reconcile/take/report` via `command.invoke`, prefix `coord.` (the default
//! coordination extension) or `$VQRO_COORD_PREFIX`. Mutation ops carry the window epoch. A take whose reply is lost
//! is repeated with the same op, which returns its claims (never content): they are reported `released`. The idle
//! waiter wakes on the `coord-status` view's `wake.pending[<rN>]` and reconnects after a core restart.
//!
//! Environment: `VQRO_SOCKET` (core endpoint), `VQRO_AGENT` (this agent's `rN`), `VQRO_PRINCIPAL` (the plugin's
//! principal, bound at hello), all injected by its launcher. Without the first two
//! both the plugin does nothing. Every failure is silent towards Claude Code (a hook must never break the session)
//! and logged to the private state directory. Status: hooks have no controlling terminal and Claude Code's
//! `terminalSequence` allowlist excludes OSC 7501, so tier-1 status goes to `agents.status {terminal, app, state,
//! seq}` (K3; `$VQRO_CLAUDE_STATUS_COMMAND` overrides the name, `off` disables it). The endpoint is the terminal.

#[cfg(unix)]
fn main() {
    std::process::exit(unix::main());
}

#[cfg(not(unix))]
fn main() {
    eprintln!("vqro-claude-hook: only Unix endpoints are implemented (Windows named pipes: not yet)");
}

#[cfg(unix)]
mod unix {
    use serde_json::{json, Value};
    use std::fs::{self, OpenOptions};
    use std::io::{BufRead, BufReader, Read, Write};
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    use std::os::unix::net::UnixStream;
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    const TAKE_LIMIT: u64 = 16;
    /// Content block types inserted without loss: `additionalContext` is plain text.
    const CONTENT: &[&str] = &["text"];
    const CALL_MS: u64 = 3000;
    /// Claude Code moves `additionalContext` over 10,000 characters out to a file; whole records only, below that.
    const MAX_CONTEXT: usize = 9000;
    /// The largest record this plugin takes (`take {max_bytes}`: compact UTF-8 JSON bytes of its content). Larger
    /// records stay held `too_large_for_consumer` and are never claimed. Quoting adds at most 3 bytes per line and a
    /// line costs at least 2 JSON bytes, so the body renders within 1.5 x 5,000 + 3 bytes; the header is at most
    /// ~800 (`clip`). One record therefore always fits `MAX_CONTEXT` (bytes >= UTF-16 characters).
    const RECORD_MAX: usize = 5000;
    /// Header fields are clipped so a header stays bounded whatever the ids are.
    const FIELD_MAX: usize = 120;
    /// The coordination extension's live status view; `wake.pending[<rN>]` is a count, never content.
    const WAKE_VIEW: &str = "coord-status";
    const LOG_BYTES: u64 = 1 << 20;

    fn prefix() -> String {
        std::env::var("VQRO_COORD_PREFIX").unwrap_or_else(|_| "coord.".into())
    }

    struct Core {
        r: BufReader<UnixStream>,
        w: UnixStream,
        next: u64,
        events: Vec<Value>,
        /// The op id of the last mutation sent (a lost take is inspected by it).
        last_op: String,
    }

    impl Core {
        fn connect(path: &str) -> Result<Core, String> {
            let w = UnixStream::connect(path).map_err(|e| format!("transport: connect: {e}"))?;
            w.set_read_timeout(Some(Duration::from_millis(CALL_MS))).ok();
            let r = BufReader::new(w.try_clone().map_err(|e| format!("transport: {e}"))?);
            let mut core = Core { r, w, next: 0, events: Vec::new(), last_op: String::new() };
            // The launcher injects the plugin's principal (C2: bound at hello, never per call).
            let mut hello = json!({"client": "vqro-claude-hook", "version": env!("CARGO_PKG_VERSION")});
            if let Ok(principal) = std::env::var("VQRO_PRINCIPAL") {
                hello["identity"] = json!({"kind": "principal", "principal": principal});
            }
            core.call("hello", hello)?;
            Ok(core)
        }

        fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
            self.next += 1;
            let id = self.next;
            let line = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string() + "\n";
            self.w.write_all(line.as_bytes()).map_err(|e| format!("transport: write: {e}"))?;
            loop {
                let row = self.read()?;
                if row.get("id").and_then(Value::as_u64) == Some(id) {
                    if let Some(err) = row.get("error") {
                        let kind = err.pointer("/data/kind").and_then(Value::as_str).unwrap_or("error");
                        return Err(format!("{kind}: {}", err.get("message").and_then(Value::as_str).unwrap_or("")));
                    }
                    return Ok(row.get("result").cloned().unwrap_or(Value::Null));
                }
                if row.get("method").and_then(Value::as_str) == Some("event") {
                    self.events.push(row);
                }
            }
        }

        fn read(&mut self) -> Result<Value, String> {
            let mut line = String::new();
            match self.r.read_line(&mut line) {
                Ok(0) => Err("transport: core closed the connection".into()),
                Ok(_) => serde_json::from_str(&line).map_err(|e| format!("transport: bad reply: {e}")),
                Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => Err("transport: timeout".into()),
                Err(e) => Err(format!("transport: read: {e}")),
            }
        }

        fn coord(&mut self, name: &str, args: Value) -> Result<Value, String> {
            self.call("command.invoke", json!({"name": format!("{}{name}", prefix()), "args": args, "deadline_ms": CALL_MS}))
        }

        /// A coordination mutation. Its op is `<window epoch>:<suffix>`: callers pass a deterministic suffix when a
        /// repeat must be recognised (reports, a lost take). An `expired_operation` created nothing, so one retry
        /// under a fresh epoch is safe.
        fn mutate(&mut self, ctx: &Ctx, name: &str, mut args: Value, suffix: &str) -> Result<Value, String> {
            self.last_op.clear(); // set only once this mutation is actually sent
            for attempt in 0..2 {
                let mut epoch = ctx.with_state(|s| s.get("epoch").as_str().map(String::from)).unwrap_or_default();
                if epoch.is_empty() {
                    epoch = self.coord("inbox.window", json!({}))?.get("epoch").and_then(Value::as_str).unwrap_or("").to_string();
                    ctx.with_state(|s| s.set("epoch", json!(epoch)));
                }
                args["op"] = json!(format!("{epoch}:{suffix}"));
                self.last_op = format!("{epoch}:{suffix}");
                match self.coord(name, args.clone()) {
                    Ok(reply) => {
                        if let Some(e) = reply.pointer("/window/epoch").and_then(Value::as_str) {
                            if e != epoch {
                                ctx.with_state(|s| s.set("epoch", json!(e)));
                            }
                        }
                        return Ok(reply);
                    }
                    Err(e) if attempt == 0 && e.starts_with("expired_operation") => {
                        self.last_op.clear(); // refused: it created nothing
                        ctx.with_state(|s| s.set("epoch", json!("")));
                    }
                    Err(e) => return Err(e),
                }
            }
            Err("expired_operation twice".into())
        }
    }

    /// An error after which the call may or may not have happened.
    fn uncertain(error: &str) -> bool {
        error.starts_with("transport:") || error.starts_with("timeout") || error.starts_with("unavailable") || error.starts_with("indeterminate")
    }

    /// Per-session plugin state, shared by the hook processes of one Claude Code session (file-locked).
    struct State {
        v: Value,
    }

    impl State {
        fn get(&self, key: &str) -> &Value {
            self.v.get(key).unwrap_or(&Value::Null)
        }
        fn set(&mut self, key: &str, value: Value) {
            if !self.v.is_object() {
                self.v = json!({});
            }
            self.v[key] = value;
        }
        fn open(&self) -> Vec<Value> {
            self.get("open").as_array().cloned().unwrap_or_default()
        }
    }

    struct Ctx {
        agent: String,
        socket: String,
        dir: PathBuf,
        session: String,
        input: Value,
    }

    impl Ctx {
        fn state_path(&self) -> PathBuf {
            self.dir.join(format!("{}.json", sanitize(&self.session)))
        }
        fn log(&self, what: &str, detail: Value) {
            let path = self.dir.join("hook.log");
            if fs::metadata(&path).map(|m| m.len() > LOG_BYTES).unwrap_or(false) {
                let _ = fs::rename(&path, self.dir.join("hook.log.1")); // bounded: one rotated file
            }
            let line = json!({"t": now_ms(), "pid": std::process::id(), "session": self.session, "event": what, "detail": detail}).to_string() + "\n";
            if let Ok(mut f) = OpenOptions::new().create(true).append(true).mode(0o600).open(path) {
                let _ = f.write_all(line.as_bytes());
            }
        }
        /// Run `f` with the session state locked; the state is written back afterwards.
        fn with_state<T>(&self, f: impl FnOnce(&mut State) -> T) -> T {
            let lock = OpenOptions::new().create(true).truncate(false).write(true).mode(0o600)
                .open(self.dir.join(format!("{}.lock", sanitize(&self.session)))).ok();
            if let Some(l) = &lock {
                let _ = l.lock();
            }
            let mut state = State { v: fs::read_to_string(self.state_path()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(json!({})) };
            let out = f(&mut state);
            let tmp = self.state_path().with_extension("tmp");
            if let Ok(mut file) = OpenOptions::new().create(true).truncate(true).write(true).mode(0o600).open(&tmp) {
                if file.write_all(state.v.to_string().as_bytes()).is_ok() {
                    let _ = fs::rename(&tmp, self.state_path());
                }
            }
            out
        }
    }

    pub fn main() -> i32 {
        let args: Vec<String> = std::env::args().collect();
        let mode = args.get(1).map(String::as_str).unwrap_or("hook");
        if mode == "settings" {
            println!("{}", serde_json::to_string_pretty(&settings(args.get(2).map(String::as_str).unwrap_or("vqro-claude-hook"))).unwrap());
            return 0;
        }
        let mut buf = String::new();
        let _ = std::io::stdin().read_to_string(&mut buf);
        let input: Value = serde_json::from_str(&buf).unwrap_or(json!({}));
        let (Ok(agent), Ok(socket)) = (std::env::var("VQRO_AGENT"), std::env::var("VQRO_SOCKET")) else { return 0 };
        if input.get("agent_id").is_some() {
            return 0; // a subagent's hook: its context is not the agent's conversation
        }
        let session = input.get("session_id").and_then(Value::as_str).unwrap_or("nosession").to_string();
        let dir = state_dir();
        // Private: delivery ids and consumers never become readable by other users.
        if fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir).is_err() {
            return 0;
        }
        if fs::symlink_metadata(&dir).is_ok_and(|m| m.is_dir()) {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)); // only succeeds if it is ours
        }
        if !private_dir(&dir) {
            return 0;
        }
        let ctx = Ctx { agent, socket, dir, session, input };
        match mode {
            "wait" => wait(&ctx),
            _ => {
                hook(&ctx);
                0
            }
        }
    }

    /// The state directory must be a real directory (no planted symlink) closed to group and others. A 0700
    /// directory someone else owns is unusable to us anyway: every write below would fail.
    fn private_dir(dir: &Path) -> bool {
        use std::os::unix::fs::PermissionsExt;
        fs::symlink_metadata(dir).is_ok_and(|m| m.is_dir() && m.permissions().mode() & 0o077 == 0)
    }

    fn hook(ctx: &Ctx) {
        let event = ctx.input.get("hook_event_name").and_then(Value::as_str).unwrap_or("").to_string();
        ctx.log("hook", json!({"hook": event}));
        let mut core = match Core::connect(&ctx.socket) {
            Ok(c) => c,
            Err(e) => return ctx.log("core_unavailable", json!({"hook": event, "error": e})),
        };
        // Evidence first: anything handed over earlier may now be visible in the transcript.
        advance_evidence(ctx, &mut core);
        match event.as_str() {
            "SessionStart" => {
                session_start(ctx, &mut core);
                status(ctx, &mut core, "unknown", &event);
            }
            "UserPromptSubmit" | "PreToolUse" => {
                retire_waiter(ctx);
                status(ctx, &mut core, "working", &event);
            }
            "PermissionRequest" => status(ctx, &mut core, "blocked", &event),
            "StopFailure" => {
                ctx.with_state(|s| s.set("busy", json!(false)));
                status(ctx, &mut core, "error", &event);
            }
            "Notification" => {
                if ctx.input.get("notification_type").and_then(Value::as_str) == Some("idle_prompt") {
                    ctx.with_state(|s| s.set("busy", json!(false)));
                    status(ctx, &mut core, "idle", &event);
                }
            }
            "PostToolBatch" => {
                ctx.with_state(|s| s.set("stop_continuations", json!(0)));
                if let Some(handoff) = take(ctx, &mut core, "turn") {
                    if emit(&json!({"hookSpecificOutput": {"hookEventName": "PostToolBatch", "additionalContext": handoff.text}})) {
                        supplied(ctx, &mut core, &handoff);
                    } else {
                        release(ctx, &mut core, &handoff);
                    }
                }
            }
            "Stop" => {
                // Claude Code ends the turn after 8 consecutive continuations; stay well below so a taken record
                // is never handed to a continuation Claude Code then overrides.
                let continued = ctx.with_state(|s| s.get("stop_continuations").as_u64().unwrap_or(0));
                let handoff = if continued < 4 { take(ctx, &mut core, "settle") } else { None };
                match handoff {
                    Some(handoff) => {
                        ctx.with_state(|s| {
                            s.set("stop_continuations", json!(continued + 1));
                            s.set("busy", json!(true));
                        });
                        status(ctx, &mut core, "working", &event); // the turn continues with the message
                        if emit(&json!({"hookSpecificOutput": {"hookEventName": "Stop", "additionalContext": handoff.text}})) {
                            supplied(ctx, &mut core, &handoff);
                        } else {
                            release(ctx, &mut core, &handoff);
                        }
                    }
                    None => {
                        ctx.with_state(|s| {
                            s.set("stop_continuations", json!(0));
                            s.set("busy", json!(false));
                        });
                        status(ctx, &mut core, "idle", &event);
                    }
                }
            }
            _ => {}
        }
    }

    /// Write the hook's JSON output; `true` once it is written and flushed.
    fn emit(output: &Value) -> bool {
        let mut out = std::io::stdout();
        out.write_all(output.to_string().as_bytes()).is_ok() && out.flush().is_ok()
    }

    fn session_start(ctx: &Ctx, core: &mut Core) {
        let source = ctx.input.get("source").and_then(Value::as_str).unwrap_or("startup");
        let mut existing = ctx.with_state(|s| s.get("consumer").as_str().map(String::from));
        // The latest session of this Claude Code process (the hooks' parent) in this agent: /clear starts a new
        // session id (2.1.329, claude_code_tui I6). Only the same process carries a consumer over: another session in
        // the same agent (a second Claude Code) never shares it.
        let process = std::os::unix::process::parent_id();
        let pointer = ctx.dir.join(format!("agent-{}.session", sanitize(&ctx.agent)));
        let mark = json!({"session": ctx.session, "process": process}).to_string();
        let previous = fs::read_to_string(&pointer).ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .filter(|p| p.get("process").and_then(Value::as_u64) == Some(u64::from(process)))
            .and_then(|p| p.get("session").and_then(Value::as_str).map(String::from))
            .filter(|p| !p.is_empty() && *p != ctx.session);
        // Compaction and /clear keep the process, its consumer and its window: not a restart. Under a new session id
        // they are carried over; that session's open hand-offs stay `supplied` (their context was cleared).
        if matches!(source, "compact" | "clear") {
            if let (None, Some(prev)) = (&existing, &previous) {
                let old = Ctx { agent: ctx.agent.clone(), socket: ctx.socket.clone(), dir: ctx.dir.clone(), session: prev.clone(), input: Value::Null };
                let (consumer, epoch) = old.with_state(|s| (s.get("consumer").clone(), s.get("epoch").clone()));
                if consumer.is_string() {
                    ctx.with_state(|s| {
                        s.set("consumer", consumer.clone());
                        s.set("epoch", epoch);
                        s.set("busy", json!(true));
                        s.set("open", json!([]));
                        s.set("stop_continuations", json!(0));
                    });
                    existing = consumer.as_str().map(String::from);
                }
            }
            if existing.is_some() {
                ctx.log("session_continued", json!({"source": source, "from_session": previous, "process": process}));
                write_private(&pointer, &mark);
                return;
            }
        }
        let consumer = format!("claude:{}:{}", ctx.session, random_hex());
        ctx.log("session", json!({"consumer": consumer, "source": source, "process": process,
            "messaging_socket": std::env::var("CLAUDE_CODE_MESSAGING_SOCKET").ok(),
            "entrypoint": std::env::var("CLAUDE_CODE_ENTRYPOINT").ok()}));
        ctx.with_state(|s| s.set("epoch", json!("")));
        // `evidence: none`: a new consumer proves nothing; the inbox makes every open claim `unknown`.
        // Bounded pages: follow `state: reconciling` until the inbox says `reconciled` (each page settles more claims).
        let mut changed = Vec::new();
        for page in 0..64 {
            match core.mutate(ctx, "inbox.reconcile", json!({"agent": ctx.agent, "consumer": consumer, "records": []}), &format!("{consumer}:reconcile:{page}")) {
                Ok(r) => {
                    changed.extend(r.get("records").and_then(Value::as_array).cloned().unwrap_or_default());
                    if r.get("state").and_then(Value::as_str) != Some("reconciling") {
                        ctx.log("reconciled", json!({"records": changed, "pages": page + 1}));
                        break;
                    }
                }
                Err(e) => {
                    ctx.log("reconcile_failed", json!({"page": page, "error": e}));
                    break;
                }
            }
        }
        ctx.with_state(|s| {
            s.set("consumer", json!(consumer));
            s.set("busy", json!(true)); // not idle evidence: only Stop and the idle_prompt notification are
            s.set("open", json!([]));
            s.set("stop_continuations", json!(0));
            s.set("recover", json!([])); // reconciliation settled every claim of the old consumer
            s.set("oversize_until_ms", json!(0));
        });
        write_private(&pointer, &mark);
    }

    /// Replace a small private file atomically (0600).
    fn write_private(path: &Path, text: &str) {
        let tmp = path.with_extension("tmp");
        if let Ok(mut file) = OpenOptions::new().create(true).truncate(true).write(true).mode(0o600).open(&tmp) {
            if file.write_all(text.as_bytes()).is_ok() {
                let _ = fs::rename(&tmp, path);
            }
        }
    }

    /// What a take hands to Claude Code: the context text and, per record, `[delivery, claim, nonce]`.
    struct Handoff {
        text: String,
        records: Vec<(String, Value, String)>,
    }

    /// One batch take at a safe point. Records this plugin cannot insert, and records beyond the context size
    /// limit, are released uninserted (they stay held; nothing is flattened, truncated or dropped).
    fn take(ctx: &Ctx, core: &mut Core, at: &str) -> Option<Handoff> {
        let consumer = ctx.with_state(|s| s.get("consumer").as_str().map(String::from))?;
        // `accept` names the block types context insertion carries without loss; the inbox keeps others held
        // (`unsupported_content`).
        // `max_bytes` (required) keeps records this plugin cannot hand over whole held (`too_large_for_consumer`):
        // they are never claimed, so they never block later mail.
        // A lost take whose claims are not yet released keeps the inbox from claiming more (`claim_in_flight`):
        // finish recovering it first, and take nothing until that succeeds.
        let pending: Vec<String> = ctx.with_state(|s| s.get("recover").as_array().cloned().unwrap_or_default())
            .iter().filter_map(|v| v.as_str().map(String::from)).collect();
        if !pending.is_empty() {
            let left: Vec<String> = pending.into_iter().filter(|op| !recover_take(ctx, op)).collect();
            ctx.with_state(|s| s.set("recover", json!(left)));
            if !left.is_empty() {
                ctx.log("take_deferred", json!({"at": at, "recovering": left.len()}));
                return None;
            }
        }
        if backing_off(ctx) {
            return None;
        }
        let args = json!({"agent": ctx.agent, "consumer": consumer, "at": at, "limit": TAKE_LIMIT, "accept": CONTENT, "max_bytes": RECORD_MAX});
        let suffix = format!("{}:take:{}", ctx.session, random_hex());
        let reply = match core.mutate(ctx, "inbox.take", args, &suffix) {
            Ok(r) => r,
            // Uncertain only if the take itself was sent (a failed window fetch sent nothing).
            Err(e) if uncertain(&e) && !core.last_op.is_empty() => {
                let op = core.last_op.clone();
                ctx.log("take_uncertain", json!({"error": e, "op": op}));
                if !recover_take(ctx, &op) {
                    // Remember it (bounded): the next point retries before taking anything.
                    ctx.with_state(|s| {
                        let mut ops = s.get("recover").as_array().cloned().unwrap_or_default();
                        ops.push(json!(op));
                        let skip = ops.len().saturating_sub(8);
                        s.set("recover", json!(ops[skip..]));
                    });
                }
                return None;
            }
            Err(e) => {
                ctx.log("take_failed", json!({"at": at, "error": e}));
                return None;
            }
        };
        let records = reply.get("records").and_then(Value::as_array).cloned().unwrap_or_default();
        if records.is_empty() {
            if let Some(held) = reply.get("held").and_then(Value::as_str) {
                ctx.log("held", json!({"at": at, "held": held}));
            }
            return None;
        }
        let mut text = String::new();
        let mut taken = Vec::new();
        let mut full = false;
        let mut oversized = 0;
        for r in &records {
            let delivery = r.get("delivery").and_then(Value::as_str).unwrap_or("").to_string();
            let insertable = r.get("content").and_then(Value::as_array).is_some_and(|b| b.iter().all(|b| b.get("type").and_then(Value::as_str).is_some_and(|t| CONTENT.contains(&t))));
            // No nonce, no hand-off: a guessable reference would let content forge evidence.
            let nonce = nonce();
            let piece = nonce.as_ref().map(|n| render(r, n)).unwrap_or_default();
            // A record over max_bytes means the inbox ignored the filter: release just it and go on. It never blocks
            // the records behind it (it fits no hand-off, so holding them back would starve them).
            let size = r.get("content").map(|c| serde_json::to_vec(c).map(|v| v.len()).unwrap_or(usize::MAX)).unwrap_or(0);
            let alone_too_big = size > RECORD_MAX || piece.len() > MAX_CONTEXT;
            if nonce.is_none() || !insertable || alone_too_big || full || text.len() + piece.len() > MAX_CONTEXT {
                // keep order among records that fit: nothing jumps ahead of one that only missed this batch
                full |= insertable && nonce.is_some() && !alone_too_big;
                oversized += usize::from(insertable && nonce.is_some() && alone_too_big);
                let why = if nonce.is_none() { "no_randomness" } else if !insertable { "unsupported_content" }
                    else if alone_too_big { "too_large_ignored_max_bytes" } else { "context_limit" };
                ctx.log("released", json!({"delivery": delivery, "why": why, "bytes": size}));
                report(ctx, core, &delivery, &r["claim"], "released");
                continue;
            }
            text.push_str(&piece);
            taken.push((delivery, r["claim"].clone(), nonce.unwrap_or_default()));
        }
        // An inbox that ignores max_bytes hands the same oversized record back at every point (a take and a
        // release each time). While a take yields nothing else, back off: 2 s doubling to 60 s.
        ctx.with_state(|s| {
            let previous = s.get("oversize_backoff_ms").as_u64().unwrap_or(0);
            if oversized > 0 && taken.is_empty() {
                let ms = (previous * 2).clamp(2000, 60_000);
                s.set("oversize_backoff_ms", json!(ms));
                s.set("oversize_until_ms", json!(now_ms() as u64 + ms));
            } else if previous > 0 {
                s.set("oversize_backoff_ms", json!(0));
                s.set("oversize_until_ms", json!(0));
            }
        });
        if oversized > 0 {
            ctx.log("max_bytes_ignored", json!({"oversized": oversized}));
        }
        if taken.is_empty() {
            return None;
        }
        ctx.log("taken", json!({"at": at, "deliveries": taken.iter().map(|t| t.0.clone()).collect::<Vec<_>>(), "chars": text.len()}));
        Some(Handoff { text: text.trim_end().to_string(), records: taken })
    }

    /// A take reply was lost: inspect that exact op on a fresh connection (`inbox.op`; never a new take, which a
    /// changed window epoch would make). It returns the claims the take made, never content; release them so the
    /// mail stays deliverable instead of claimed until restart. `true` once settled: the claims are released (or
    /// already settled), or the op made none; `false` to retry at the next point (the core or inbox did not answer).
    fn recover_take(ctx: &Ctx, op: &str) -> bool {
        let Ok(mut fresh) = Core::connect(&ctx.socket) else { return false };
        let reply = match fresh.coord("inbox.op", json!({"op": op})) {
            Ok(r) => r,
            Err(e) => {
                ctx.log("take_recovery_failed", json!({"op": op, "error": e}));
                return !uncertain(&e); // a definite refusal will not change by retrying
            }
        };
        let claims = reply.get("claims").and_then(Value::as_array).cloned().unwrap_or_default();
        let records = reply.get("records").and_then(Value::as_array).cloned().unwrap_or_default();
        let mut settled = true;
        for c in claims.iter().chain(records.iter()) {
            let delivery = c.get("delivery").and_then(Value::as_str).unwrap_or("");
            let suffix = format!("report:{delivery}:{}:released", c["claim"].as_str().unwrap_or(""));
            match fresh.mutate(ctx, "inbox.report", json!({"delivery": delivery, "claim": c["claim"], "stage": "released"}), &suffix) {
                Ok(_) => {}
                Err(e) if uncertain(&e) => settled = false,
                Err(e) => ctx.log("release_refused", json!({"delivery": delivery, "error": e})), // already settled
            }
        }
        ctx.log(if settled { "take_recovered" } else { "take_recovery_incomplete" }, json!({"op": op, "released": claims.len() + records.len()}));
        settled
    }

    /// True while backing off from an inbox that ignores max_bytes (see `take`).
    fn backing_off(ctx: &Ctx) -> bool {
        ctx.with_state(|s| s.get("oversize_until_ms").as_u64().unwrap_or(0)) > now_ms() as u64
    }

    /// The plugin's own header, then the content as a quote: content cannot pose as a header.
    fn render(r: &Value, nonce: &str) -> String {
        let delivery = clip(r.get("delivery").and_then(Value::as_str).unwrap_or("?"));
        let mode = clip(r.get("mode").and_then(Value::as_str).unwrap_or("?"));
        let from = clip(r.get("from").and_then(|v| v.as_str().or_else(|| v.get("principal").and_then(Value::as_str))).unwrap_or("?"));
        let origin = r.get("origin").filter(|o| o.is_object()).map(|o| {
            let kind = clip(o.get("kind").and_then(Value::as_str).unwrap_or("?"));
            let actor = o.get("actor").and_then(Value::as_str).map(|a| format!(" {}", clip(a))).unwrap_or_default();
            format!(", origin {kind}{actor}")
        }).unwrap_or_default();
        let body: String = content_text(r.get("content")).lines().map(|l| format!("> {l}\n")).collect();
        // Factual framing (Claude Code treats imperative out-of-band text as possible prompt injection).
        format!("vqro inbox message {delivery} [ref {nonce}] from {from}{origin} (mode {mode}); the quoted lines are the sender's content:\n{body}\n")
    }

    /// The output is written: record each hand-off as open (for `read` evidence) and report `supplied`.
    fn supplied(ctx: &Ctx, core: &mut Core, handoff: &Handoff) {
        ctx.with_state(|s| {
            let mut open = s.open();
            for (delivery, claim, nonce) in &handoff.records {
                open.push(json!({"delivery": delivery, "claim": claim, "nonce": nonce}));
            }
            s.set("open", json!(open));
        });
        for (delivery, claim, _) in &handoff.records {
            report(ctx, core, delivery, claim, "supplied");
        }
    }

    /// The output could not be written: nothing reached Claude Code, so the claims go back uninserted.
    fn release(ctx: &Ctx, core: &mut Core, handoff: &Handoff) {
        for (delivery, claim, _) in &handoff.records {
            report(ctx, core, delivery, claim, "released");
        }
    }

    fn report(ctx: &Ctx, core: &mut Core, delivery: &str, claim: &Value, stage: &str) -> bool {
        // Deterministic op: a repeat of the same report is recognised; a re-claimed delivery has a new claim.
        let suffix = format!("report:{delivery}:{}:{stage}", claim.as_str().unwrap_or(""));
        match core.mutate(ctx, "inbox.report", json!({"delivery": delivery, "claim": claim, "stage": stage}), &suffix) {
            Ok(_) => true,
            Err(e) => {
                ctx.log("report_failed", json!({"delivery": delivery, "stage": stage, "error": e}));
                false
            }
        }
    }

    /// Upgrade `supplied` hand-offs to `read` from transcript evidence (see `evidence`). The transcript is written
    /// asynchronously, so absence proves nothing. (`inserted` is not reported: the declaration is `evidence: none`,
    /// and `supplied` and `inserted` are separate evidence branches.)
    fn advance_evidence(ctx: &Ctx, core: &mut Core) {
        let Some(transcript) = ctx.input.get("transcript_path").and_then(Value::as_str) else { return };
        let open = ctx.with_state(|s| s.open());
        if open.is_empty() {
            return;
        }
        let rows: Vec<Value> = fs::read_to_string(transcript).map(|t| t.lines().filter_map(|l| serde_json::from_str(l).ok()).collect()).unwrap_or_default();
        let mut done = Vec::new();
        for o in &open {
            let (Some(delivery), Some(nonce)) = (o["delivery"].as_str(), o["nonce"].as_str()) else { continue };
            if let Some((entry, true)) = evidence(&rows, nonce) {
                if report(ctx, core, delivery, &o["claim"], "read") {
                    ctx.log("read", json!({"delivery": delivery, "entry": entry}));
                    done.push(delivery.to_string());
                }
            }
        }
        if !done.is_empty() {
            // Merge, never overwrite: an idle waiter may have added hand-offs meanwhile.
            ctx.with_state(|s| {
                let open: Vec<Value> = s.open().into_iter().filter(|o| !done.iter().any(|d| o["delivery"].as_str() == Some(d))).collect();
                s.set("open", json!(open));
            });
        }
    }

    fn kind(row: &Value) -> &str {
        match row.get("type").and_then(Value::as_str) {
            Some("attachment") => row.pointer("/attachment/type").and_then(Value::as_str).unwrap_or(""),
            Some(t) => t,
            None => "",
        }
    }

    /// The plugin's own conversation entries (measured on 2.1.329): `PostToolBatch`/`Stop` context is a
    /// `hook_additional_context` attachment; an asyncRewake wake is a `user` row whose origin is a
    /// `task-notification`. Tool results, human prompts, peer messages and hook stdout records
    /// (`hook_success`, `hook_non_blocking_error`) never count.
    fn own_entry(row: &Value) -> bool {
        match kind(row) {
            "hook_additional_context" => matches!(row.pointer("/attachment/hookEvent").and_then(Value::as_str), Some("PostToolBatch" | "Stop")),
            "user" => row.pointer("/origin/kind").and_then(Value::as_str) == Some("task-notification"),
            _ => false,
        }
    }

    /// `(entry uuid, read)` when an own entry carries this hand-off's nonce. `read`: a later `api-request-blob` (the
    /// request payload Claude Code sent) carries the nonce and a later assistant entry finished without error.
    fn evidence(rows: &[Value], nonce: &str) -> Option<(Value, bool)> {
        let tag = format!("[ref {nonce}]");
        let i = rows.iter().position(|r| own_entry(r) && r.to_string().contains(&tag))?;
        let read = rows[i + 1..].iter().position(|r| kind(r) == "api-request-blob" && r.to_string().contains(&tag)).is_some_and(|j| {
            rows[i + 1 + j..].iter().any(|r| kind(r) == "assistant" && r.pointer("/message/stop_reason").and_then(Value::as_str).is_some_and(|s| s != "error"))
        });
        Some((rows[i].get("uuid").cloned().unwrap_or(json!(i)), read))
    }

    /// Tier-1 status (K3 `agents.status`; hooks cannot emit OSC 7501). Lossy and advisory: last writer by `seq`
    /// (epoch ms) wins, so a restarted hook never goes backwards. A failure is logged once per distinct error.
    fn status(ctx: &Ctx, core: &mut Core, state: &str, event: &str) {
        let command = std::env::var("VQRO_CLAUDE_STATUS_COMMAND").unwrap_or_else(|_| "agents.status".into());
        if command == "off" {
            return;
        }
        let state = match state {
            "working" => "running",
            "blocked" => "requires_action",
            "idle" | "error" => "idle",
            _ => "unknown",
        };
        let args = json!({"terminal": ctx.agent, "app": "claude", "state": state, "seq": now_ms() as u64});
        let error = core.call("command.invoke", json!({"name": command, "args": args, "deadline_ms": CALL_MS})).err();
        let changed = ctx.with_state(|s| {
            let previous = s.get("status_error").as_str().map(String::from);
            s.set("status_error", json!(error));
            previous != error
        });
        if let (Some(e), true) = (error, changed) {
            ctx.log("status_failed", json!({"state": state, "hook": event, "error": e}));
        }
    }

    fn subscribe(ctx: &Ctx) -> Option<Core> {
        let mut core = Core::connect(&ctx.socket).map_err(|e| ctx.log("waiter_core_unavailable", json!(e))).ok()?;
        core.call("events.subscribe", json!({"types": ["view.content"], "view_content": {"names": [WAKE_VIEW]}}))
            .map_err(|e| ctx.log("waiter_subscribe_failed", json!(e))).ok()?;
        Some(core)
    }

    /// `asyncRewake` waiter for the `idle` point: arm, take once, else wait for the inbox wake (counts only),
    /// then take at `idle`. Exit 2 with the text on stderr wakes Claude Code; exit 0 does nothing. A lost core
    /// connection is re-established with backoff (a core restart must not leave an idle session deaf).
    fn wait(ctx: &Ctx) -> i32 {
        // Non-interactive sessions (`-p`, SDK) wait for a running asyncRewake hook before they exit (measured with
        // 2.1.329), and their idle is the end of the process: there is no idle point to serve.
        if std::env::var("CLAUDE_CODE_ENTRYPOINT").as_deref() != Ok("cli") {
            return 0;
        }
        let me = std::process::id() as u64;
        ctx.with_state(|s| s.set("waiter", json!(me)));
        ctx.log("waiter_armed", json!({}));
        let deadline = std::time::Instant::now() + Duration::from_secs(23 * 3600);
        let mut backoff = Duration::from_millis(500);
        let mut core: Option<Core> = None;
        let mut woken = true; // take once when armed or reconnected, then only after a wake event: never poll
        let mut retry = false; // ...except to finish a lost-take recovery or after an oversize backoff (every ~3 s)
        // Claude Code started this waiter; if it goes away (killed), nobody would read a rewake: exit.
        let parent = std::os::unix::process::parent_id();
        loop {
            if std::os::unix::process::parent_id() != parent {
                ctx.log("waiter_orphaned", json!({}));
                return 0;
            }
            // Only an idle, current waiter may take: a new turn or a newer waiter retires this one.
            let (busy, current) = ctx.with_state(|s| (s.get("busy").as_bool().unwrap_or(false), s.get("waiter").as_u64() == Some(me)));
            if !current || std::time::Instant::now() > deadline {
                ctx.log("waiter_retired", json!({}));
                return 0;
            }
            let Some(c) = core.as_mut() else {
                core = subscribe(ctx);
                if core.is_none() {
                    std::thread::sleep(backoff);
                    backoff = (backoff * 2).min(Duration::from_secs(5));
                } else {
                    backoff = Duration::from_millis(500);
                    woken = true;
                }
                continue;
            };
            if !busy && woken {
                woken = false;
                c.events.clear();
                if let Some(handoff) = take(ctx, c, "idle") {
                    ctx.with_state(|s| s.set("busy", json!(true)));
                    let mut err = std::io::stderr();
                    if err.write_all(handoff.text.as_bytes()).is_ok() && err.flush().is_ok() {
                        supplied(ctx, c, &handoff);
                        ctx.log("waiter_wake", json!({}));
                        return 2;
                    }
                    release(ctx, c, &handoff);
                    return 0;
                }
                // A pending recovery or an oversize backoff: try again later even without a new wake.
                retry = backing_off(ctx) || ctx.with_state(|s| s.get("recover").as_array().is_some_and(|a| !a.is_empty()));
            }
            // Wait for the next wake event; a short timeout re-checks retirement (no content is read).
            let pending = |row: &Value| row.pointer("/params/data/content/wake/pending").and_then(|p| p.get(&ctx.agent)).and_then(Value::as_u64).unwrap_or(0) > 0;
            if !c.events.is_empty() {
                woken = c.events.iter().any(pending) || retry;
                c.events.clear();
            } else {
                match c.read() {
                    Ok(row) => {
                        if row.get("method").and_then(Value::as_str) == Some("event") {
                            woken = pending(&row) || retry;
                        }
                    }
                    Err(e) if e == "transport: timeout" => woken |= retry,
                    Err(e) => {
                        ctx.log("waiter_lost_core", json!(e));
                        core = None;
                    }
                }
            }
        }
    }

    fn retire_waiter(ctx: &Ctx) {
        ctx.with_state(|s| {
            s.set("busy", json!(true));
            s.set("waiter", Value::Null); // the waiter notices it is no longer current and exits 0
        });
    }

    /// Content is always an array of typed blocks; only `CONTENT` (text) is ever rendered.
    fn content_text(content: Option<&Value>) -> String {
        let blocks = content.and_then(Value::as_array).cloned().unwrap_or_default();
        blocks.iter().filter_map(|b| b.get("text").and_then(Value::as_str)).collect::<Vec<_>>().join("\n")
    }

    /// The Claude Code `hooks` settings block for this plugin (merge into a settings file).
    fn settings(bin: &str) -> Value {
        let h = || json!({"type": "command", "command": bin, "args": ["hook"]});
        let wait = json!({"type": "command", "command": bin, "args": ["wait"], "asyncRewake": true, "timeout": 86400});
        let one = |hooks: Vec<Value>| json!([{"hooks": hooks}]);
        json!({"hooks": {
            "SessionStart": one(vec![h()]),
            "UserPromptSubmit": one(vec![h()]),
            "PreToolUse": one(vec![h()]),
            "PermissionRequest": one(vec![h()]),
            "PostToolBatch": one(vec![h()]),
            "Stop": one(vec![h(), wait.clone()]),
            "StopFailure": one(vec![h()]),
            "Notification": json!([{"matcher": "idle_prompt", "hooks": [h(), wait.clone()]}]),
            "SessionEnd": one(vec![h()]),
        }})
    }

    fn state_dir() -> PathBuf {
        if let Ok(d) = std::env::var("VQRO_CLAUDE_STATE") {
            return PathBuf::from(d);
        }
        if let Ok(d) = std::env::var("XDG_RUNTIME_DIR") {
            return PathBuf::from(d).join("vqro-claude");
        }
        std::env::temp_dir().join(format!("vqro-claude-{}", std::env::var("USER").unwrap_or_else(|_| "user".into())))
    }

    fn sanitize(s: &str) -> String {
        s.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).take(100).collect()
    }

    /// A one-line header field of bounded length (`FIELD_MAX` characters, then `…`).
    fn clip(s: &str) -> String {
        let one: String = s.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
        if one.chars().count() <= FIELD_MAX { one } else { one.chars().take(FIELD_MAX).collect::<String>() + "…" }
    }

    /// Ids for ops and consumers (unique, not secret).
    fn random_hex() -> String {
        let mut b = [0u8; 8];
        if getrandom::fill(&mut b).is_err() {
            return format!("{:016x}", now_ms() as u64 ^ u64::from(std::process::id()));
        }
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    /// The evidence nonce: 128 secret bits, or none (then nothing is handed over).
    fn nonce() -> Option<String> {
        let mut b = [0u8; 16];
        getrandom::fill(&mut b).ok()?;
        Some(b.iter().map(|x| format!("{x:02x}")).collect())
    }

    fn now_ms() -> u128 {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0)
    }
}
