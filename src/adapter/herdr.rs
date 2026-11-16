//! herdr-core adapter. Speaks herdr-core's framed protocol through `hc connect` (stdio), which works
//! the same locally, over ssh (`ssh box hc connect`), and on Windows (named pipe behind `hc`).
//!
//! Frames: `[channel u32 LE][kind u8][len u32 LE][payload]`; channel 0 carries JSON-RPC.
//! See ~/Projects/herdr-core/schema/README.md. Input goes through attach streams, which herdr-core
//! delivers immediately (no send/keys quiet delays).

use std::collections::HashMap;

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
        "terminal.spawn": {}, "terminal.input": {}, "terminal.output": {"raw": true}, "terminal.resize": {},
        "terminal.kill": {}, "terminal.list": {}, "terminal.exit": {}, "status.osc7501": {},
        "persist.detach": {}, "events.resync": {}, "remote.machines": {}
    })
}

/// Start the adapter. It connects with `argv` (default `["hc", "connect"]`) and reconnects if the
/// connection drops. Events go to `events` tagged with the adapter name.
pub fn start(name: String, argv: Vec<String>, events: mpsc::UnboundedSender<(String, AdapterEvent)>) -> AdapterHandle {
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let handle = AdapterHandle { name: name.clone(), capabilities: capabilities(), cmds: cmd_tx };
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
    Spawn(Reply<ResourceFacts>),
    Kill(Reply<Value>),
    Attach(Reply<Stream>),
}

async fn run(
    name: String,
    argv: Vec<String>,
    events: mpsc::UnboundedSender<(String, AdapterEvent)>,
    mut cmds: mpsc::UnboundedReceiver<AdapterCmd>,
) {
    loop {
        match session(&name, &argv, &events, &mut cmds).await {
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
                    Some(c) => fail(c, "herdr-core is not connected"),
                }
            }
        }
    }
}

fn fail(cmd: AdapterCmd, why: &str) {
    match cmd {
        AdapterCmd::Spawn(_, r) => drop(r.send(Err(why.into()))),
        AdapterCmd::Kill(_, r) => drop(r.send(Err(why.into()))),
        AdapterCmd::Attach { reply, .. } => drop(reply.send(Err(why.into()))),
    }
}

async fn session(
    name: &str,
    argv: &[String],
    events: &mpsc::UnboundedSender<(String, AdapterEvent)>,
    cmds: &mut mpsc::UnboundedReceiver<AdapterCmd>,
) -> anyhow::Result<()> {
    let mut child = Command::new(&argv[0])
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
    let mut line = String::new();
    stdout.read_line(&mut line).await?;
    let reply: Value = serde_json::from_str(&line)?;
    if reply.get("result").and_then(|r| r.get("framed")) != Some(&Value::Bool(true)) {
        anyhow::bail!("herdr-core did not accept framed mode: {line}");
    }

    let (frame_tx, mut frame_out) = mpsc::unbounded_channel::<Vec<u8>>();
    tokio::spawn(async move {
        while let Some(bytes) = frame_out.recv().await {
            if stdin.write_all(&bytes).await.is_err() || stdin.flush().await.is_err() {
                break;
            }
        }
    });
    let (in_tx, mut frames_in) = mpsc::unbounded_channel::<Frame>();
    tokio::spawn(read_frames(stdout, in_tx));

    let mut next_id: u64 = 1;
    let mut pending: HashMap<u64, Pending> = HashMap::new();
    let mut streams: HashMap<u32, mpsc::UnboundedSender<StreamOutput>> = HashMap::new();
    let mut records: HashMap<String, Value> = HashMap::new();
    let mut events_channel: Option<u32> = None;

    let call = |id: &mut u64, method: &str, params: Value| -> (u64, Vec<u8>) {
        let this = *id;
        *id += 1;
        let msg = json!({"jsonrpc": "2.0", "id": this, "method": method, "params": params}).to_string();
        (this, frame(0, DATA, msg.as_bytes()))
    };

    let (id, bytes) = call(&mut next_id, "events.subscribe", json!({}));
    pending.insert(id, Pending::Subscribe);
    frame_tx.send(bytes)?;
    let _ = events.send((name.to_string(), AdapterEvent::Connected(true, None)));

    loop {
        tokio::select! {
            cmd = cmds.recv() => {
                let Some(cmd) = cmd else { return Ok(()) };
                match cmd {
                    AdapterCmd::Spawn(req, reply) => {
                        let mut p = json!({"argv": req.argv, "no_parent": true});
                        if let Some(cwd) = req.cwd { p["cwd"] = json!(cwd); }
                        if let Some(n) = req.name { p["name"] = json!(n); }
                        let (id, bytes) = call(&mut next_id, "terminal.run", p);
                        pending.insert(id, Pending::Spawn(reply));
                        frame_tx.send(bytes)?;
                    }
                    AdapterCmd::Kill(r, reply) => {
                        let (id, bytes) = call(&mut next_id, "terminal.kill", json!({"terminal": r}));
                        pending.insert(id, Pending::Kill(reply));
                        frame_tx.send(bytes)?;
                    }
                    AdapterCmd::Attach { backend_ref, cols, rows, history, reply } => {
                        let (id, bytes) = call(&mut next_id, "terminal.attach", json!({"terminal": backend_ref, "cols": cols, "rows": rows, "history": history}));
                        pending.insert(id, Pending::Attach(reply));
                        frame_tx.send(bytes)?;
                    }
                }
            }
            f = frames_in.recv() => {
                let Some(f) = f else { break };
                if f.channel == 0 || Some(f.channel) == events_channel {
                    if f.kind != DATA { continue; }
                    let Ok(msg) = serde_json::from_slice::<Value>(&f.payload) else { continue };
                    if let Some(method) = msg.get("method").and_then(|m| m.as_str()) {
                        handle_notification(name, method, msg.get("params").cloned().unwrap_or(Value::Null), &mut records, events);
                        continue;
                    }
                    let Some(id) = msg.get("id").and_then(|i| i.as_u64()) else { continue };
                    let Some(p) = pending.remove(&id) else { continue };
                    let result: Result<Value, String> = match msg.get("error") {
                        Some(e) => Err(e.get("message").and_then(|m| m.as_str()).unwrap_or("error").to_string()),
                        None => Ok(msg.get("result").cloned().unwrap_or(Value::Null)),
                    };
                    match p {
                        Pending::Subscribe => {
                            let r = result.map_err(|e| anyhow::anyhow!("events.subscribe failed: {e}"))?;
                            events_channel = r.get("channel").and_then(|c| c.as_u64()).map(|c| c as u32);
                            let list = r.get("terminals").and_then(|t| t.as_array()).cloned().unwrap_or_default();
                            records = list.into_iter().filter_map(|t| Some((t.get("address")?.as_str()?.to_string(), t))).collect();
                            let _ = events.send((name.to_string(), AdapterEvent::Snapshot(records.values().map(facts).collect())));
                        }
                        Pending::Spawn(reply) => { let _ = reply.send(result.map(|r| facts(&r))); }
                        Pending::Kill(reply) => { let _ = reply.send(result); }
                        Pending::Attach(reply) => match result {
                            Err(e) => { let _ = reply.send(Err(e)); }
                            Ok(r) => {
                                let Some(ch) = r.get("channel").and_then(|c| c.as_u64()).map(|c| c as u32) else {
                                    let _ = reply.send(Err("attach returned no channel".into()));
                                    continue;
                                };
                                let (out_tx, out_rx) = mpsc::unbounded_channel();
                                let (inp_tx, mut inp_rx) = mpsc::unbounded_channel::<StreamInput>();
                                streams.insert(ch, out_tx);
                                let ftx = frame_tx.clone();
                                tokio::spawn(async move {
                                    while let Some(i) = inp_rx.recv().await {
                                        let bytes = match i {
                                            StreamInput::Data(d) => frame(ch, DATA, &d),
                                            StreamInput::Resize(c, r) => {
                                                let mut p = c.to_le_bytes().to_vec();
                                                p.extend_from_slice(&r.to_le_bytes());
                                                frame(ch, RESIZE, &p)
                                            }
                                            StreamInput::Close => { let _ = ftx.send(frame(ch, CLOSE, &[])); break; }
                                        };
                                        if ftx.send(bytes).is_err() { break; }
                                    }
                                });
                                let _ = reply.send(Ok(Stream { input: inp_tx, output: out_rx }));
                            }
                        },
                    }
                } else if let Some(out) = streams.get(&f.channel) {
                    match f.kind {
                        DATA => {
                            let n = f.payload.len() as u32;
                            if out.send(StreamOutput::Data(f.payload)).is_err() {
                                // The viewer is gone; tell the backend.
                                let _ = frame_tx.send(frame(f.channel, CLOSE, &[]));
                                streams.remove(&f.channel);
                            } else {
                                let _ = frame_tx.send(frame(f.channel, CREDIT, &n.to_le_bytes()));
                            }
                        }
                        CLOSE => {
                            let reason = serde_json::from_slice(&f.payload).unwrap_or(json!({"reason": "closed"}));
                            let _ = out.send(StreamOutput::Closed(reason));
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
        let _ = out.send(StreamOutput::Closed(json!({"reason": "backend_disconnected"})));
    }
    for (_, p) in pending.drain() {
        match p {
            Pending::Spawn(r) => drop(r.send(Err("herdr-core disconnected".into()))),
            Pending::Kill(r) => drop(r.send(Err("herdr-core disconnected".into()))),
            Pending::Attach(r) => drop(r.send(Err("herdr-core disconnected".into()))),
            Pending::Subscribe => {}
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
    match method {
        "missed" => {
            let list = params.get("terminals").and_then(|t| t.as_array()).cloned().unwrap_or_default();
            *records = list.into_iter().filter_map(|t| Some((t.get("address")?.as_str()?.to_string(), t))).collect();
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
                    records.remove(&t);
                    send(AdapterEvent::Removed(t));
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

/// herdr-core `Terminal` record to prim facts.
fn facts(t: &Value) -> ResourceFacts {
    let s = |k: &str| t.get(k).and_then(|v| v.as_str()).map(String::from);
    ResourceFacts {
        backend_ref: s("address").unwrap_or_default(),
        kind: "terminal".into(),
        name: s("name"),
        title: s("title"),
        state: s("state").unwrap_or_else(|| "live".into()),
        status: t.get("status").cloned().unwrap_or(Value::Null),
        exit: json!({"reason": t.get("reason"), "exit_code": t.get("exit_code"), "signal": t.get("signal")}),
        argv: t.get("argv").and_then(|a| a.as_array()).map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default(),
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

async fn read_frames(mut r: BufReader<ChildStdout>, tx: mpsc::UnboundedSender<Frame>) {
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
        if tx.send(Frame { channel, kind, payload }).is_err() {
            return;
        }
    }
}
