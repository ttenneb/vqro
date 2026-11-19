//! vqro-claude-hook: the tier-1 Claude Code plugin (`coordination-v0.md` §7, `agent-v0.md` §2.2).
//!
//! Claude Code runs this binary as a hook command. It never types into the terminal and never touches the editor.
//! It pulls the agent's inbox at Claude Code's own safe points and reports only what it has evidence for:
//!
//! | Hook | Point | What it does |
//! |---|---|---|
//! | `SessionStart` | — | new consumer; `inbox.reconcile` open claims against the transcript; status unknown |
//! | `UserPromptSubmit`, `PreToolUse` | — | status working; marks the session busy and retires an armed idle waiter |
//! | `PermissionRequest` | — | status blocked |
//! | `PostToolBatch` | `turn` | `inbox.take {at: turn}` → `additionalContext` before the next model call |
//! | `Stop` | `settle` | `inbox.take {at: settle}` → `additionalContext` (the turn continues once) |
//! | `Stop`, `Notification` `idle_prompt` (`wait`, `asyncRewake`, interactive only) | `idle` | waits for the inbox wake, takes, exits 2 → Claude wakes |
//! | `StopFailure`, `Notification` (`idle_prompt`) | — | status error / idle |
//! | `SessionEnd` | — | last chance to turn transcript evidence into `inserted` / `read` |
//!
//! Insertion is `insert: context` (a system reminder), so a hand-off is reported `supplied`. When the transcript
//! later shows the entry carrying the delivery id it is reported `inserted`, and `read` once a later assistant
//! message completed. Hooks inside subagents (`agent_id`) are ignored: their context is not the agent's.
//!
//! Environment: `VQRO_SOCKET` (core endpoint), `VQRO_AGENT` (this agent's `rN`, injected by its launcher). Without
//! both the plugin does nothing. Every failure is silent towards Claude Code (a hook must never break the session)
//! and logged to the state directory. Status goes to `agents.terminal_status` (pending a standard status primitive:
//! hooks have no controlling terminal and Claude Code's `terminalSequence` allowlist excludes OSC 7501).

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
    use std::os::unix::net::UnixStream;
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    const MARK: &str = "vqro message ";
    const TAKE_LIMIT: u64 = 16;
    /// Content block types this plugin inserts without loss (`additionalContext` is a string).
    const CONTENT: &[&str] = &["text", "resource_link"];
    const CALL_MS: u64 = 3000;
    const MAX_CONTEXT: usize = 9000; // Claude Code moves >10,000 characters out to a file

    struct Core {
        r: BufReader<UnixStream>,
        w: UnixStream,
        next: u64,
        events: Vec<Value>,
    }

    impl Core {
        fn connect(path: &str) -> Result<Core, String> {
            let w = UnixStream::connect(path).map_err(|e| format!("connect: {e}"))?;
            w.set_read_timeout(Some(Duration::from_millis(CALL_MS))).ok();
            let r = BufReader::new(w.try_clone().map_err(|e| e.to_string())?);
            let mut core = Core { r, w, next: 0, events: Vec::new() };
            core.call("hello", json!({"client": "vqro-claude-hook", "version": env!("CARGO_PKG_VERSION")}))?;
            Ok(core)
        }

        fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
            self.next += 1;
            let id = self.next;
            let line = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string() + "\n";
            self.w.write_all(line.as_bytes()).map_err(|e| format!("write: {e}"))?;
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
                Ok(0) => Err("core closed the connection".into()),
                Ok(_) => serde_json::from_str(&line).map_err(|e| format!("bad reply: {e}")),
                Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => Err("timeout".into()),
                Err(e) => Err(format!("read: {e}")),
            }
        }

        /// An inbox command (`coordination-v0.md` §5.3) through the core's command registry.
        fn inbox(&mut self, name: &str, args: Value) -> Result<Value, String> {
            self.call("command.invoke", json!({"name": name, "args": args, "deadline_ms": CALL_MS}))
        }
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
            let line = json!({"t": now_ms(), "pid": std::process::id(), "session": self.session, "event": what, "detail": detail}).to_string() + "\n";
            if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(self.dir.join("hook.log")) {
                let _ = f.write_all(line.as_bytes());
            }
        }
        /// Run `f` with the session state locked; the state is written back afterwards.
        fn with_state<T>(&self, f: impl FnOnce(&mut State) -> T) -> T {
            let lock = OpenOptions::new().create(true).truncate(false).write(true).open(self.dir.join(format!("{}.lock", sanitize(&self.session)))).ok();
            if let Some(l) = &lock {
                let _ = l.lock();
            }
            let mut state = State { v: fs::read_to_string(self.state_path()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(json!({})) };
            let out = f(&mut state);
            let tmp = self.state_path().with_extension("tmp");
            if fs::write(&tmp, state.v.to_string()).is_ok() {
                let _ = fs::rename(&tmp, self.state_path());
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
        let _ = fs::create_dir_all(&dir);
        let ctx = Ctx { agent, socket, dir, session, input };
        match mode {
            "wait" => wait(&ctx),
            _ => {
                hook(&ctx);
                0
            }
        }
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
                if let Some(text) = take(ctx, &mut core, "turn") {
                    emit(&json!({"hookSpecificOutput": {"hookEventName": "PostToolBatch", "additionalContext": text}}));
                }
            }
            "Stop" => {
                // Claude Code ends the turn after 8 consecutive continuations; stay well below so a taken record
                // is never handed to a continuation Claude Code then overrides.
                let continued = ctx.with_state(|s| s.get("stop_continuations").as_u64().unwrap_or(0));
                let text = if continued < 4 { take(ctx, &mut core, "settle") } else { None };
                match text {
                    Some(text) => {
                        ctx.with_state(|s| {
                            s.set("stop_continuations", json!(continued + 1));
                            s.set("busy", json!(true));
                        });
                        status(ctx, &mut core, "working", &event); // the turn continues with the message
                        emit(&json!({"hookSpecificOutput": {"hookEventName": "Stop", "additionalContext": text}}));
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

    fn emit(output: &Value) {
        let _ = std::io::stdout().write_all(output.to_string().as_bytes());
    }

    fn session_start(ctx: &Ctx, core: &mut Core) {
        let consumer = format!("claude:{}:{}", ctx.session, random_hex());
        let transcript = ctx.input.get("transcript_path").and_then(Value::as_str).map(PathBuf::from);
        let records: Vec<Value> = transcript
            .map(|t| evidence(&t))
            .unwrap_or_default()
            .into_iter()
            .map(|(delivery, entry, _)| json!({"delivery": delivery, "entry": entry}))
            .collect();
        let op = format!("{consumer}:reconcile");
        // Claude Code's native per-session inbox (an `admit` route, evaluated separately); recorded for diagnosis.
        ctx.log("session", json!({"consumer": consumer, "messaging_socket": std::env::var("CLAUDE_CODE_MESSAGING_SOCKET").ok(),
            "entrypoint": std::env::var("CLAUDE_CODE_ENTRYPOINT").ok()}));
        match core.inbox("inbox.reconcile", json!({"op": op, "agent": ctx.agent, "consumer": consumer, "records": records})) {
            Ok(r) => ctx.log("reconciled", r),
            Err(e) => ctx.log("reconcile_failed", json!(e)),
        }
        ctx.with_state(|s| {
            s.set("consumer", json!(consumer));
            s.set("busy", json!(true)); // not idle evidence: only Stop and the idle_prompt notification are
            s.set("open", json!([]));
            s.set("stop_continuations", json!(0));
        });
    }

    /// One batch take at a safe point. Returns the context text to hand to Claude Code, after reporting
    /// `supplied` for each record (the reminder is given to the runtime, with no durable record yet).
    fn take(ctx: &Ctx, core: &mut Core, at: &str) -> Option<String> {
        let consumer = ctx.with_state(|s| s.get("consumer").as_str().map(String::from))?;
        let op = format!("{consumer}:take:{}", random_hex());
        // Context insertion is text only: records with other block types stay held (`unsupported_content`) rather
        // than being flattened or dropped.
        let reply = match core.inbox("inbox.take", json!({"op": op, "agent": ctx.agent, "consumer": consumer, "at": at, "limit": TAKE_LIMIT, "content": CONTENT})) {
            Ok(r) => r,
            Err(e) => {
                ctx.log("take_failed", json!({"at": at, "op": op, "error": e}));
                return None;
            }
        };
        let records = reply.get("records").and_then(Value::as_array).cloned().unwrap_or_default();
        if records.is_empty() {
            return None;
        }
        let mut text = String::new();
        for r in &records {
            let delivery = r.get("delivery").and_then(Value::as_str).unwrap_or("?");
            let from = r.get("from").map(|v| v.as_str().map(String::from).unwrap_or_else(|| v.to_string())).unwrap_or_default();
            let mode = r.get("mode").and_then(Value::as_str).unwrap_or("?");
            // Factual framing (Claude Code treats imperative out-of-band text as possible prompt injection).
            text.push_str(&format!("{MARK}{delivery} from {from} (mode {mode}, delivered by the vqro inbox):\n{}\n\n", content_text(r.get("content"))));
        }
        if text.len() > MAX_CONTEXT {
            ctx.log("context_too_large", json!({"bytes": text.len()})); // the evidence marker stays in the preview
        }
        ctx.with_state(|s| {
            let mut open = s.open();
            for r in &records {
                open.push(json!({"delivery": r["delivery"], "claim": r["claim"], "stage": "supplied", "at": at}));
            }
            s.set("open", json!(open));
        });
        for r in &records {
            report(ctx, core, r["delivery"].as_str().unwrap_or(""), &r["claim"], "supplied");
        }
        ctx.log("taken", json!({"at": at, "deliveries": records.iter().map(|r| r["delivery"].clone()).collect::<Vec<_>>()}));
        Some(text.trim_end().to_string())
    }

    fn report(ctx: &Ctx, core: &mut Core, delivery: &str, claim: &Value, stage: &str) -> bool {
        let op = format!("{delivery}:{stage}");
        match core.inbox("inbox.report", json!({"op": op, "delivery": delivery, "claim": claim, "stage": stage})) {
            Ok(_) => true,
            Err(e) => {
                ctx.log("report_failed", json!({"delivery": delivery, "stage": stage, "error": e}));
                false
            }
        }
    }

    /// Upgrade open hand-offs from transcript evidence: the entry carrying the delivery id → `inserted`; a later
    /// completed assistant message → `read`. The transcript is written asynchronously, so absence proves nothing.
    fn advance_evidence(ctx: &Ctx, core: &mut Core) {
        let Some(transcript) = ctx.input.get("transcript_path").and_then(Value::as_str) else { return };
        let open = ctx.with_state(|s| s.open());
        if open.is_empty() {
            return;
        }
        let seen = evidence(Path::new(transcript));
        let mut still = Vec::new();
        for mut o in open {
            let delivery = o["delivery"].as_str().unwrap_or("").to_string();
            if let Some((_, entry, read)) = seen.iter().find(|(d, _, _)| *d == delivery) {
                if o["stage"] == "supplied" && report(ctx, core, &delivery, &o["claim"], "inserted") {
                    ctx.log("inserted", json!({"delivery": delivery, "entry": entry}));
                    o["stage"] = json!("inserted");
                }
                if *read && o["stage"] == "inserted" && report(ctx, core, &delivery, &o["claim"], "read") {
                    ctx.log("read", json!({"delivery": delivery}));
                    continue;
                }
            }
            still.push(o);
        }
        ctx.with_state(|s| s.set("open", json!(still)));
    }

    /// Transcript entry types that put hook context into the conversation (measured on 2.1.329). The entries that
    /// merely record a hook's stdout/stderr (`hook_success`, `hook_non_blocking_error`) are not evidence: an
    /// asyncRewake exit 2 from a `SessionStart` hook is recorded as `hook_non_blocking_error` and never reaches the
    /// model.
    const INSERTED: &[&str] = &["hook_additional_context", "async_hook_response", "hook_async_rewake"];

    /// (delivery, entry uuid, read) for every delivery id in a model-visible transcript entry. `read`: a later
    /// `api-request-blob` (the request payload Claude Code sent) contains it and a later assistant entry finished
    /// without error.
    fn evidence(transcript: &Path) -> Vec<(String, Value, bool)> {
        let Ok(text) = fs::read_to_string(transcript) else { return Vec::new() };
        let lines: Vec<Value> = text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
        let ids = |row: &Value| -> Vec<String> {
            let s = row.to_string();
            let mut rest = s.as_str();
            let mut out = Vec::new();
            while let Some(p) = rest.find(MARK) {
                let tail = &rest[p + MARK.len()..];
                let id: String = tail.chars().take_while(|c| c.is_ascii_alphanumeric() || "-_:.".contains(*c)).collect();
                rest = &tail[id.len()..];
                if !id.is_empty() && !out.contains(&id) {
                    out.push(id);
                }
            }
            out
        };
        fn kind(row: &Value) -> &str {
            match row.get("type").and_then(Value::as_str) {
                Some("attachment") => row.pointer("/attachment/type").and_then(Value::as_str).unwrap_or(""),
                Some(t) => t,
                None => "",
            }
        }
        let mut out: Vec<(String, Value, bool)> = Vec::new();
        for (i, row) in lines.iter().enumerate() {
            let k = kind(row);
            let model_visible = INSERTED.contains(&k) || (k == "user" && row.get("isMeta").is_none_or(|m| m != true));
            if !model_visible {
                continue;
            }
            for id in ids(row) {
                if out.iter().any(|(d, _, _)| *d == id) {
                    continue;
                }
                let read = lines[i + 1..].iter().position(|r| kind(r) == "api-request-blob" && ids(r).contains(&id)).is_some_and(|j| {
                    lines[i + 1 + j..].iter().any(|r| {
                        kind(r) == "assistant" && r.pointer("/message/stop_reason").and_then(Value::as_str).is_some_and(|s| s != "error")
                    })
                });
                out.push((id, row.get("uuid").cloned().unwrap_or(json!(i)), read));
            }
        }
        out
    }

    fn status(ctx: &Ctx, core: &mut Core, state: &str, event: &str) {
        let args = json!({"agent": ctx.agent, "state": state, "app": "claude", "source": event, "session": ctx.session});
        if let Err(e) = core.inbox("agents.terminal_status", args) {
            ctx.log("status_failed", json!({"state": state, "error": e}));
        }
    }

    /// `asyncRewake` waiter for the `idle` point: arm, take once, else wait for the inbox wake (counts only),
    /// then take at `idle`. Exit 2 with the text on stderr wakes Claude Code; exit 0 does nothing.
    fn wait(ctx: &Ctx) -> i32 {
        // Non-interactive sessions (`-p`, SDK) wait for a running asyncRewake hook before they exit (measured with
        // 2.1.329), and their idle is the end of the process: there is no idle point to serve.
        if std::env::var("CLAUDE_CODE_ENTRYPOINT").as_deref() != Ok("cli") {
            return 0;
        }
        let me = std::process::id() as u64;
        ctx.with_state(|s| s.set("waiter", json!(me)));
        ctx.log("waiter_armed", json!({}));
        let mut core = match Core::connect(&ctx.socket) {
            Ok(c) => c,
            Err(e) => {
                ctx.log("waiter_core_unavailable", json!(e));
                return 0;
            }
        };
        let view = format!("inbox-wake-{}", ctx.agent);
        if let Err(e) = core.call("events.subscribe", json!({"types": ["view.content"], "view_content": {"names": [view]}})) {
            ctx.log("waiter_subscribe_failed", json!(e));
            return 0;
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(23 * 3600);
        let mut woken = true; // take once when armed, then only after a wake event: never poll
        loop {
            // Only an idle, current waiter may take: a new turn or a newer waiter retires this one.
            let (busy, current) = ctx.with_state(|s| (s.get("busy").as_bool().unwrap_or(false), s.get("waiter").as_u64() == Some(me)));
            if !current || std::time::Instant::now() > deadline {
                ctx.log("waiter_retired", json!({}));
                return 0;
            }
            if !busy && woken {
                woken = false;
                core.events.clear();
                if let Some(text) = take(ctx, &mut core, "idle") {
                    ctx.with_state(|s| s.set("busy", json!(true)));
                    ctx.log("waiter_wake", json!({}));
                    eprint!("{text}");
                    return 2;
                }
            }
            // Wait for the next wake event; a short timeout re-checks retirement (no content is read).
            if !core.events.is_empty() {
                woken = true;
            } else {
                match core.read() {
                    Ok(row) => {
                        if row.get("method").and_then(Value::as_str) == Some("event") {
                            woken = true;
                        }
                    }
                    Err(e) if e == "timeout" => {}
                    Err(e) => {
                        ctx.log("waiter_lost_core", json!(e));
                        return 0;
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

    /// Content is always an array of typed blocks; only `CONTENT` types are ever taken.
    fn content_text(content: Option<&Value>) -> String {
        let blocks = content.and_then(Value::as_array).cloned().unwrap_or_default();
        blocks
            .iter()
            .map(|b| match b.get("type").and_then(Value::as_str) {
                Some("text") => b.get("text").and_then(Value::as_str).unwrap_or("").to_string(),
                Some("resource_link") => {
                    let uri = b.get("uri").and_then(Value::as_str).unwrap_or("");
                    format!("[link: {} <{uri}>]", b.get("name").and_then(Value::as_str).unwrap_or(uri))
                }
                other => format!("[unsupported {} block]", other.unwrap_or("?")), // not taken: see CONTENT
            })
            .collect::<Vec<_>>()
            .join("\n")
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

    fn random_hex() -> String {
        let mut b = [0u8; 8];
        let _ = getrandom::fill(&mut b);
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    fn now_ms() -> u128 {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0)
    }
}
