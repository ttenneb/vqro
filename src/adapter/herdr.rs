//! herdr-core adapter. Speaks herdr-core's framed protocol through `hc connect` (stdio), which works
//! the same locally, over ssh (`ssh box hc connect`), and on Windows (named pipe behind `hc`).
//!
//! Frames: `[channel u32 LE][kind u8][len u32 LE][payload]`; channel 0 carries JSON-RPC.
//! See ~/Projects/herdr-core/schema/README.md. Input goes through attach streams, which herdr-core
//! delivers immediately (no send/keys quiet delays).

use std::collections::{HashMap, HashSet};

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdout, Command};
use tokio::sync::mpsc;

use super::*;

const DATA: u8 = 0;
const CREDIT: u8 = 1;
const CLOSE: u8 = 2;
const RESIZE: u8 = 3;

pub fn capabilities() -> Value {
    json!({
        "terminal.spawn": {"env": true, "cwd": true}, "terminal.input": {"stream": true, "effect": false}, "terminal.output": {"raw": true}, "terminal.resize": {},
        "terminal.kill": {}, "terminal.list": {}, "terminal.exit": {"exit_code": true}, "status.osc7501": {},
        "persist.detach": {}, "events.resync": {}, "remote.machines": {"listing": true, "terminal_effects": false}
    })
}

/// Start the adapter. It connects with `argv` (default `["hc", "connect"]`) and reconnects if the
/// connection drops. Events go to `events` tagged with the adapter name.
pub fn start(name: String, argv: Vec<String>, events: mpsc::UnboundedSender<(String, AdapterEvent)>) -> AdapterHandle {
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let handle = AdapterHandle { name: name.clone(), session: None, context: None, capabilities: capabilities(), cmds: cmd_tx };
    tokio::spawn(run(name, argv, events, cmd_rx));
    handle
}

struct Frame {
    channel: u32,
    kind: u8,
    payload: Vec<u8>,
}

enum Pending {
    Subscribe,
    Refresh(tokio::time::Instant),
    Spawn(Reply<ResourceFacts>),
    Kill(Reply<EffectResult>),
    Attach { reply: Reply<Stream>, history: u32 },
}

const MAX_NATIVE_PENDING: usize = 1024;

fn prune_pending(pending: &mut HashMap<u64, Pending>, late_attaches: &mut HashSet<u64>) {
    pending.retain(|id, call| {
        let cancelled = match call {
            Pending::Subscribe | Pending::Refresh(_) => false,
            Pending::Spawn(reply) => reply.is_closed(),
            Pending::Kill(reply) => reply.is_closed(),
            Pending::Attach { reply, .. } => {
                if reply.is_closed() { late_attaches.insert(*id); true } else { false }
            }
        };
        !cancelled
    });
}

async fn run(
    name: String,
    argv: Vec<String>,
    events: mpsc::UnboundedSender<(String, AdapterEvent)>,
    mut cmds: mpsc::UnboundedReceiver<AdapterCmd>,
) {
    let mut connection = 0;
    loop {
        connection += 1;
        match session(&name, &argv, &events, &mut cmds, connection).await {
            Ok(()) => return, // the core dropped us
            Err(e) => {
                let _ = events.send((name.clone(), AdapterEvent::Connected(false, Some(e.to_string()))));
            }
        }
        // Answer commands while disconnected instead of letting them hang.
        let retry = tokio::time::sleep(std::time::Duration::from_secs(3));
        tokio::pin!(retry);
        loop {
            tokio::select! {
                _ = &mut retry => break,
                cmd = cmds.recv() => match cmd {
                    None => return,
                    Some(AdapterCmd::Shutdown(reply)) => { let _ = reply.send(Ok(())); return; }
                    Some(c) => reject_command(c, "herdr-core is not connected"),
                }
            }
        }
    }
}

async fn session(
    name: &str,
    argv: &[String],
    events: &mpsc::UnboundedSender<(String, AdapterEvent)>,
    cmds: &mut mpsc::UnboundedReceiver<AdapterCmd>,
    connection: u64,
) -> anyhow::Result<()> {
    let mut child = Command::new(&argv[0])
        // A backend may auto-start its own service: do not propagate supervisor credentials to terminals.
        .env_remove("PRIM_EXTENSION_TOKEN").env_remove("PRIM_EXTENSION_LIFETIME").env_remove("PRIM_EXTENSION")
        .env_remove("PRIM_STATE_DIR").env_remove("PRIM_EXTENSIONS_DIR").env_remove("PRIM_SOCKET").env_remove("PRIM_SOCKET_SOURCE")
        .args(&argv[1..])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

    // Plain-mode handshake, then framed.
    let hello = json!({"jsonrpc": "2.0", "id": 0, "method": "hello", "params": {"framed": true}});
    stdin.write_all(format!("{hello}\n").as_bytes()).await?;
    let mut line = Vec::new();
    let hello_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    let mut hello_reader = (&mut stdout).take(1024 * 1024 + 1);
    let mut before_hello = std::collections::VecDeque::new();
    loop {
        tokio::select! {
            result = tokio::time::timeout_at(hello_deadline, hello_reader.read_until(b'\n', &mut line)) => { result??; break; }
            command = cmds.recv() => match command {
                Some(AdapterCmd::Shutdown(reply)) => { let _ = child.kill().await; let _ = reply.send(Ok(())); return Ok(()); }
                Some(command) if before_hello.len() < MAX_NATIVE_PENDING => before_hello.push_back(command),
                Some(command) => reject_command(command, "native pre-hello queue is full"),
                None => return Ok(()),
            }
        }
    }
    drop(hello_reader);
    if line.len() > 1024 * 1024 { anyhow::bail!("herdr-core hello exceeds 1 MiB"); }
    let reply: Value = serde_json::from_slice(&line)?;
    if reply.get("result").and_then(|r| r.get("framed")) != Some(&Value::Bool(true)) {
        anyhow::bail!("herdr-core did not accept framed mode");
    }

    let instance = reply["result"]["core"]["id"].as_str().filter(|id| !id.is_empty())
        .ok_or_else(|| anyhow::anyhow!("herdr-core hello has no stable core.id"))?.to_string();
    let info = ProviderInfo { connection, instance, role: "backend".into(), software: json!({"name": "herdr-core", "version": reply["result"]["version"], "display_name": reply["result"]["core"]["name"]}) };

    let (frame_tx, mut frame_out) = crate::queue::channel::<Vec<u8>>(1024 * 1024);
    let writer = tokio::spawn(async move {
        while let Some(bytes) = frame_out.recv().await {
            if stdin.write_all(&bytes.value).await.is_err() || stdin.flush().await.is_err() {
                break;
            }
        }
    });
    let (in_tx, mut frames_in) = crate::queue::channel::<Frame>(32 * 1024 * 1024);
    let reader = tokio::spawn(read_frames(stdout, in_tx));

    let mut next_id: u64 = 1;
    let mut pending: HashMap<u64, Pending> = HashMap::new();
    let mut late_attaches = HashSet::new();
    let mut maintenance = tokio::time::interval(std::time::Duration::from_millis(100));
    let subscribe_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    let mut streams: HashMap<u32, crate::queue::Sender<StreamOutput>> = HashMap::new();
    let mut records: HashMap<String, Value> = HashMap::new();
    let mut events_channel: Option<u32> = None;
    let mut refresh_needed = false;

    let call = |id: &mut u64, method: &str, params: Value| -> (u64, Vec<u8>) {
        let this = *id;
        *id += 1;
        let msg = json!({"jsonrpc": "2.0", "id": this, "method": method, "params": params}).to_string();
        (this, frame(0, DATA, msg.as_bytes()))
    };

    let (id, bytes) = call(&mut next_id, "events.subscribe", json!({}));
    pending.insert(id, Pending::Subscribe);
    send_frame(&frame_tx, bytes)?;
    let _ = events.send((name.to_string(), AdapterEvent::Registered(info)));

    loop {
        tokio::select! {
            _ = maintenance.tick() => {
                prune_pending(&mut pending, &mut late_attaches);
                if pending.values().any(|call| matches!(call, Pending::Refresh(deadline) if *deadline <= tokio::time::Instant::now())) {
                    anyhow::bail!("herdr-core remote inventory refresh timed out");
                }
                if refresh_needed && pending.len() < MAX_NATIVE_PENDING && !pending.values().any(|call| matches!(call, Pending::Refresh(_))) {
                    let (id, bytes) = call(&mut next_id, "terminal.list", json!({"all": true}));
                    pending.insert(id, Pending::Refresh(tokio::time::Instant::now() + std::time::Duration::from_secs(5)));
                    send_frame(&frame_tx, bytes)?;
                    refresh_needed = false;
                }
                // Closing this native connection also releases any channels whose attach response
                // never arrived. Bounded tombstones prevent either leaked channels or unbounded IDs.
                if late_attaches.len() >= MAX_NATIVE_PENDING { anyhow::bail!("too many expired native attachments"); }
            }
            _ = tokio::time::sleep_until(subscribe_deadline), if events_channel.is_none() => { anyhow::bail!("herdr-core initial subscription timed out"); }
            cmd = async { if let Some(command) = before_hello.pop_front() { Some(command) } else { cmds.recv().await } } => {
                let Some(cmd) = cmd else { return Ok(()) };
                let Some(cmd) = command_for(cmd, connection) else { continue };
                prune_pending(&mut pending, &mut late_attaches);
                if late_attaches.len() >= MAX_NATIVE_PENDING { anyhow::bail!("too many expired native attachments"); }
                if pending.len() >= MAX_NATIVE_PENDING { reject_command(cmd, "native pending limit; not sent"); continue; }
                match cmd {
                    AdapterCmd::Bound { .. } => unreachable!("command_for removes bindings"),
                    AdapterCmd::Shutdown(reply) => {
                        writer.abort(); reader.abort(); let _ = writer.await; let _ = reader.await;
                        if tokio::time::timeout(std::time::Duration::from_millis(500), child.wait()).await.is_err() { let _ = child.kill().await; }
                        let _ = reply.send(Ok(())); return Ok(());
                    }
                    AdapterCmd::Spawn(req, reply) => {
                        if req.placement.is_some() { reject_command(AdapterCmd::Spawn(req, reply), "placement is not supported by the in-process herdr adapter"); continue; }
                        let mut p = json!({"no_parent": true});
                        if !req.argv.is_empty() { p["argv"] = json!(req.argv); }
                        if !req.env.is_empty() { p["env"] = json!(req.env); }
                        if let Some(cols) = req.cols { p["cols"] = json!(cols); }
                        if let Some(rows) = req.rows { p["rows"] = json!(rows); }
                        if let Some(cwd) = req.cwd { p["cwd"] = json!(cwd); }
                        if let Some(n) = req.name { p["name"] = json!(n); }
                        let (id, bytes) = call(&mut next_id, "terminal.run", p);
                        pending.insert(id, Pending::Spawn(reply));
                        send_frame(&frame_tx, bytes)?;
                    }
                    cmd @ AdapterCmd::Invoke { .. } => reject_command(cmd, "operation is not supported by the in-process herdr adapter"),
                    AdapterCmd::Kill(r, reply) => {
                        if remote_effect(&r) { let _ = reply.send(Err(remote_unsupported())); continue; }
                        let (id, bytes) = call(&mut next_id, "terminal.kill", json!({"terminal": r}));
                        pending.insert(id, Pending::Kill(reply));
                        send_frame(&frame_tx, bytes)?;
                    }
                    AdapterCmd::Attach { backend_ref, cols, rows, history, reply } => {
                        if remote_effect(&backend_ref) { let _ = reply.send(Err(remote_unsupported())); continue; }
                        let (id, bytes) = call(&mut next_id, "terminal.attach", json!({"terminal": backend_ref, "cols": cols, "rows": rows, "history": history}));
                        pending.insert(id, Pending::Attach { reply, history });
                        send_frame(&frame_tx, bytes)?;
                    }
                }
            }
            f = frames_in.recv() => {
                let Some(item) = f else { break };
                let f = item.value;
                if f.channel == 0 || Some(f.channel) == events_channel {
                    if f.kind != DATA { continue; }
                    let Ok(msg) = serde_json::from_slice::<Value>(&f.payload) else { continue };
                    if let Some(method) = msg.get("method").and_then(|m| m.as_str()) {
                        let params = msg.get("params").cloned().unwrap_or(Value::Null);
                        if remote_notification(method, &params) {
                            // Alias-only updates cannot safely bind machine identity. Refresh the
                            // native snapshot (including Machine.id) instead of guessing/replaying.
                            refresh_needed = true;
                        } else {
                            handle_notification(name, method, params, &mut records, events);
                        }
                        if Some(f.channel) == events_channel {
                            let (ack, consumed) = tokio::sync::oneshot::channel();
                            if events.send((name.to_string(), AdapterEvent::Consumed(ack))).is_ok() {
                                let writer = frame_tx.clone();
                                let (channel, bytes) = (f.channel, f.payload.len() as u32);
                                tokio::spawn(async move {
                                    if consumed.await.is_ok() { let _ = writer.send(frame(channel, CREDIT, &bytes.to_le_bytes()), 13).await; }
                                });
                            }
                        }
                        continue;
                    }
                    let Some(id) = msg.get("id").and_then(|i| i.as_u64()) else { continue };
                    let Some(p) = pending.remove(&id) else {
                        if late_attaches.remove(&id) {
                            if let Some(channel) = msg["result"]["channel"].as_u64() { send_frame(&frame_tx, frame(channel as u32, CLOSE, &[]))?; }
                        }
                        continue;
                    };
                    let result: Result<Value, AdapterError> = match msg.get("error") {
                        Some(e) => Err(AdapterError::Backend(e.get("message").and_then(|m| m.as_str()).unwrap_or("error").to_string())),
                        None => Ok(msg.get("result").cloned().unwrap_or(Value::Null)),
                    };
                    match p {
                        Pending::Refresh(_) => {
                            let snapshot = result.map_err(|e| anyhow::anyhow!("remote inventory refresh failed: {e}"))?;
                            // Keep newer local event facts; this query refreshes only remote rows.
                            records.retain(|address, _| !address.contains('/'));
                            records.extend(inventory(&snapshot).into_iter().filter(|(address, _)| address.contains('/')));
                            let _ = events.send((name.to_string(), AdapterEvent::Snapshot(records.values().map(facts).collect())));
                        }
                        Pending::Subscribe => {
                            let r = result.map_err(|e| anyhow::anyhow!("events.subscribe failed: {e}"))?;
                            events_channel = r.get("channel").and_then(|c| c.as_u64()).map(|c| c as u32);
                            records = inventory(&r);
                            let _ = events.send((name.to_string(), AdapterEvent::Snapshot(records.values().map(facts).collect())));
                        }
                        Pending::Spawn(reply) => { let _ = reply.send(result.map(|r| facts(&r))); }
                        Pending::Kill(reply) => {
                            let result = result.map(|record| {
                                if record["state"] != "finished" { return EffectResult::accepted(record); }
                                let observed = facts(&record);
                                // A kill reply is an independent native fact. Some hc subscribers
                                // never receive finished while an attachment is open. Do not let an
                                // old-incarnation reply replace a newer record already seen here.
                                let current = records.get(&observed.backend_ref).is_some_and(|r| facts(r).incarnation == observed.incarnation);
                                if current && observed.valid() {
                                    records.insert(observed.backend_ref.clone(), record.clone());
                                    let _ = events.send((name.to_string(), AdapterEvent::Upsert(observed.clone())));
                                    EffectResult { outcome: "observed", data: record, facts: Some(observed) }
                                } else { EffectResult { outcome: "observed", data: record, facts: None } }
                            });
                            let _ = reply.send(result);
                        }
                        Pending::Attach { reply, history } => match result {
                            Err(e) => { let _ = reply.send(Err(e)); }
                            Ok(r) => {
                                let Some(ch) = r.get("channel").and_then(|c| c.as_u64()).map(|c| c as u32) else {
                                    let _ = reply.send(Err(AdapterError::Indeterminate("attach returned no channel".into())));
                                    continue;
                                };
                                if reply.is_closed() { send_frame(&frame_tx, frame(ch, CLOSE, &[]))?; continue; }
                                let (out_tx, out_rx) = crate::queue::channel(STREAM_BUFFER_BYTES);
                                let (inp_tx, mut inp_rx) = crate::queue::channel::<StreamInput>(STREAM_BUFFER_BYTES);
                                streams.insert(ch, out_tx);
                                let ftx = frame_tx.clone();
                                tokio::spawn(async move {
                                    while let Some(i) = inp_rx.recv().await {
                                        let bytes = match i.value {
                                            StreamInput::Data(d) => frame(ch, DATA, &d),
                                            StreamInput::Resize(c, r) => {
                                                let mut p = c.to_le_bytes().to_vec();
                                                p.extend_from_slice(&r.to_le_bytes());
                                                frame(ch, RESIZE, &p)
                                            }
                                            StreamInput::Consumed(n) => frame(ch, CREDIT, &n.to_le_bytes()),
                                            StreamInput::Close => break,
                                        };
                                        let n = bytes.len();
                                        if ftx.send(bytes, n).await.is_err() { break; }
                                    }
                                    let close = frame(ch, CLOSE, &[]);
                                    let _ = ftx.send(close, 9).await;
                                });
                                let _ = reply.send(Ok(Stream { initial: if history > 0 { "history" } else { "none" }, input: inp_tx, output: out_rx, terminal: None }));
                            }
                        },
                    }
                } else if let Some(out) = streams.get(&f.channel) {
                    match f.kind {
                        DATA => {
                            let n = f.payload.len() as u32;
                            if out.try_send(StreamOutput::Data(f.payload), n as usize).is_err() {
                                // A stalled viewer must not block other channels on this backend.
                                // Dropping its sender closes output after the bounded backlog drains.
                                let _ = send_frame(&frame_tx, frame(f.channel, CLOSE, &[]));
                                streams.remove(&f.channel);
                            }
                            // CREDIT comes back through StreamInput::Consumed only after the core
                            // successfully hands this chunk to its bounded connection writer.
                        }
                        CLOSE => {
                            let reason = serde_json::from_slice(&f.payload).unwrap_or(json!({"reason": "closed"}));
                            let _ = out.try_send(StreamOutput::Closed(reason), f.payload.len().max(64));
                            streams.remove(&f.channel);
                        }
                        _ => {}
                    }
                }
            }
            _ = child.wait() => break,
        }
    }
    for (_, out) in streams.drain() {
        let _ = out.try_send(StreamOutput::Closed(json!({"reason": "backend_disconnected"})), 128);
    }
    for (_, p) in pending.drain() {
        match p {
            Pending::Spawn(r) => drop(r.send(Err(AdapterError::Indeterminate("herdr-core disconnected".into())))),
            Pending::Kill(r) => drop(r.send(Err(AdapterError::Indeterminate("herdr-core disconnected".into())))),
            Pending::Attach { reply, .. } => drop(reply.send(Err(AdapterError::Indeterminate("herdr-core disconnected".into())))),
            Pending::Subscribe | Pending::Refresh(_) => {}
        }
    }
    anyhow::bail!("herdr-core connection ended")
}

fn handle_notification(
    name: &str,
    method: &str,
    params: Value,
    records: &mut HashMap<String, Value>,
    events: &mpsc::UnboundedSender<(String, AdapterEvent)>,
) {
    let send = |e| drop(events.send((name.to_string(), e)));
    if remote_notification(method, &params) { return; }
    match method {
        "missed" => {
            *records = inventory(&params);
            send(AdapterEvent::Snapshot(records.values().map(facts).collect()));
        }
        "event" => {
            let Some(t) = params.get("terminal").and_then(|t| t.as_str()).map(String::from) else { return };
            let ty = params.get("type").and_then(|t| t.as_str()).unwrap_or("");
            let data = params.get("data").cloned().unwrap_or(Value::Null);
            match ty {
                "created" | "restarted" => {
                    records.insert(t.clone(), data);
                }
                "removed" => {
                    let incarnation = records.remove(&t).map(|record| facts(&record).incarnation).unwrap_or_default();
                    send(AdapterEvent::Removed { reference: t, incarnation });
                    return;
                }
                _ => {
                    let Some(rec) = records.get_mut(&t) else { return };
                    match ty {
                        "finished" => {
                            rec["state"] = json!("finished");
                            merge(rec, &data);
                        }
                        "lost" | "keep" | "title" | "labels" | "size" | "turn" => merge(rec, &data),
                        "foreground" => rec["foreground"] = data.clone(),
                        "status" => rec["status"] = data.get("status").cloned().unwrap_or(Value::Null),
                        _ => return,
                    }
                }
            }
            if let Some(rec) = records.get(&t) {
                send(AdapterEvent::Upsert(facts(rec)));
            }
        }
        _ => {}
    }
}

fn merge(rec: &mut Value, data: &Value) {
    if let (Some(r), Some(d)) = (rec.as_object_mut(), data.as_object()) {
        for (k, v) in d {
            if k != "record" {
                r.insert(k.clone(), v.clone());
            }
        }
    }
}

fn inventory(snapshot: &Value) -> HashMap<String, Value> {
    let mut records = HashMap::new();
    let mut seen = HashSet::new();
    for original in snapshot["terminals"].as_array().into_iter().flatten() {
        let Some(address) = original["address"].as_str() else { continue };
        let mut record = original.clone();
        if let Some((alias, terminal)) = address.split_once('/') {
            // Only direct machine/terminal addresses are specified by hc's schema.
            if terminal.contains('/') { continue; }
            let machine = snapshot["machines"].as_array().into_iter().flatten().find(|machine|
                machine["name"].as_str() == Some(alias) && machine["id"].as_str().is_some_and(|id| !id.is_empty()));
            let Some(machine) = machine else { continue };
            record["_prim_ref"] = json!(json!(["hc-machine-v1", machine["id"], machine.get("session").unwrap_or(&Value::Null), terminal]).to_string());
            record["_prim_machine"] = json!({"id": machine["id"], "name": alias, "session": machine.get("session").unwrap_or(&Value::Null)});
        }
        let reference = record["_prim_ref"].as_str().unwrap_or(address);
        if seen.insert(reference.to_string()) { records.insert(address.to_string(), record); }
    }
    records
}

fn remote_notification(method: &str, params: &Value) -> bool {
    method == "event" && params["terminal"].as_str().is_some_and(|address| address.contains('/'))
}

fn remote_effect(reference: &str) -> bool {
    reference.contains('/') || reference.starts_with('[')
}

fn remote_unsupported() -> AdapterError {
    AdapterError::RejectedKind { kind: "unsupported".into(), message: "remote terminal effects require identity-safe herdr-core addressing; aliases may be renamed/reused".into() }
}

/// herdr-core `Terminal` record to prim facts.
fn facts(t: &Value) -> ResourceFacts {
    let s = |k: &str| t.get(k).and_then(|v| v.as_str()).map(String::from);
    ResourceFacts {
        backend_ref: s("_prim_ref").or_else(|| s("address")).unwrap_or_default(),
        incarnation: t.get("created_ms").and_then(Value::as_u64).map(|ms| ms.to_string()).unwrap_or_default(),
        kind: "terminal".into(),
        name: s("name"),
        title: s("title"),
        state: s("state").unwrap_or_else(|| "live".into()),
        status: t.get("status").cloned().unwrap_or(Value::Null),
        exit: json!({"reason": t.get("reason"), "exit_code": t.get("exit_code"), "signal": t.get("signal")}),
        argv: t.get("argv").and_then(|a| a.as_array()).map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default(),
        data: t.get("_prim_machine").map(|machine| json!({"machine": machine, "hc_address": t["address"]})).unwrap_or_else(|| json!({})),
        // The foreground process's folder (read about twice a second), else where it started.
        cwd: t.get("foreground").and_then(|f| f.get("cwd")).and_then(|c| c.as_str()).or_else(|| t.get("cwd").and_then(|c| c.as_str())).map(String::from),
    }
}

fn frame(channel: u32, kind: u8, payload: &[u8]) -> Vec<u8> {
    let mut b = Vec::with_capacity(9 + payload.len());
    b.extend_from_slice(&channel.to_le_bytes());
    b.push(kind);
    b.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    b.extend_from_slice(payload);
    b
}

fn send_frame(tx: &crate::queue::Sender<Vec<u8>>, bytes: Vec<u8>) -> Result<(), crate::queue::QueueError> {
    let n = bytes.len();
    tx.try_send(bytes, n)
}

#[cfg(test)]
mod qa_tests {
    use super::*;
    #[test]
    fn expired_native_calls_retire_and_attach_ids_remain_bounded_cleanup_tokens() {
        let mut pending = HashMap::new();
        let mut late = HashSet::new();
        for id in 0..100 {
            let (reply, receiver) = tokio::sync::oneshot::channel();
            pending.insert(id, Pending::Kill(reply)); drop(receiver);
        }
        let (reply, receiver) = tokio::sync::oneshot::channel();
        pending.insert(100, Pending::Attach { reply, history:0 }); drop(receiver);
        prune_pending(&mut pending, &mut late);
        assert!(pending.is_empty(), "effect timeout must retire native pending senders");
        assert_eq!(late, HashSet::from([100]), "late attachment channel still needs explicit native close");
    }
    fn remote_snapshot(alias: &str, id: &str, session: Option<&str>) -> Value {
        json!({"machines":[{"name":alias,"id":id,"session":session}],
               "terminals":[{"address":format!("{alias}/t7"),"created_ms":1,"state":"live"}]})
    }
    #[test]
    fn remote_alias_rename_keeps_ref_and_reuse_changes_it() {
        let first = inventory(&remote_snapshot("rohan", "machine-a", None));
        let renamed = inventory(&remote_snapshot("horse", "machine-a", None));
        let reused = inventory(&remote_snapshot("rohan", "machine-b", None));
        let session = inventory(&remote_snapshot("rohan", "machine-a", Some("other")));
        let get = |rows: &HashMap<String, Value>| facts(rows.values().next().unwrap());
        assert_eq!(get(&first).backend_ref, get(&renamed).backend_ref);
        assert_ne!(get(&first).backend_ref, get(&reused).backend_ref);
        assert_ne!(get(&first).backend_ref, get(&session).backend_ref);
        assert_eq!(get(&renamed).data["machine"]["name"], "horse");
    }
    #[test]
    fn alias_only_events_never_rebind_old_machine_facts() {
        let mut records = inventory(&remote_snapshot("rohan", "machine-a", None));
        let old_ref = facts(records.get("rohan/t7").unwrap()).backend_ref;
        let (events, mut receiver) = mpsc::unbounded_channel();
        let params = json!({"terminal":"rohan/t7","type":"created","data":{"address":"rohan/t7","created_ms":99}});
        assert!(remote_notification("event", &params));
        handle_notification("fixture", "event", params, &mut records, &events);
        assert_eq!(facts(records.get("rohan/t7").unwrap()).backend_ref, old_ref);
        assert_eq!(facts(records.get("rohan/t7").unwrap()).incarnation, "1");
        assert!(receiver.try_recv().is_err());
    }
    #[test]
    fn unknown_remote_identity_is_not_published_as_an_alias() {
        assert!(inventory(&json!({"terminals":[{"address":"unknown/t7"}]})).is_empty());
        let local = inventory(&json!({"terminals":[{"address":"t7","created_ms":1}]}));
        assert_eq!(facts(local.get("t7").unwrap()).backend_ref, "t7");
    }
    #[test]
    fn remote_effects_and_legacy_aliases_are_rejected_before_dispatch() {
        assert!(remote_effect("rohan/t7"));
        assert!(remote_effect(r#"["hc-machine-v1","machine-a",null,"t7"]"#));
        assert!(!remote_effect("t7"));
    }
    #[test]
    fn cancelled_queued_commands_are_not_dispatched() {
        let (reply, receiver) = tokio::sync::oneshot::channel(); drop(receiver);
        assert!(command_for(AdapterCmd::Kill("native".into(), reply), 1).is_none());
    }
}

async fn read_frames(mut r: BufReader<ChildStdout>, tx: crate::queue::Sender<Frame>) {
    let mut head = [0u8; 9];
    loop {
        if r.read_exact(&mut head).await.is_err() {
            return;
        }
        let channel = u32::from_le_bytes(head[0..4].try_into().unwrap());
        let kind = head[4];
        let len = u32::from_le_bytes(head[5..9].try_into().unwrap()) as usize;
        if len > 16 * 1024 * 1024 {
            return;
        }
        let mut payload = vec![0u8; len];
        if r.read_exact(&mut payload).await.is_err() {
            return;
        }
        if tx.send(Frame { channel, kind, payload }, len + 9).await.is_err() {
            return;
        }
    }
}
