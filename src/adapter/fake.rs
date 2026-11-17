//! Deterministic fake backend: a test fixture, not a product (D16). Terminals echo their input.
//!
//! Load profile (for scale tests): the fake reads a few options from a spawned terminal's argv,
//! so a test can shape load through the public `resource.spawn` alone:
//! - `--status-every-ms N`: an agent-like terminal. Every N ms its status changes, as an OSC 7501
//!   report would: `{"state": "working"|"idle", "n": <count>, "at_ms": <unix ms when emitted>}`.
//!   With `--status-phase-ms P` the first change comes P ms after spawn instead of N, so agents spawned
//!   together need not tick in lock-step.
//! - `--output-bytes-per-sec N`: a high-output terminal. While attached, each stream also gets N
//!   bytes per second of printable output, in chunks every 10 ms, besides echoing input.
//!
//! Anything else in argv is ignored, as before.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde_json::{json, Value};
use tokio::sync::mpsc;

use super::*;

/// The load options a fake terminal was spawned with.
#[derive(Clone, Copy, Default)]
struct Load {
    status_every_ms: Option<u64>,
    status_phase_ms: Option<u64>,
    output_bytes_per_sec: Option<u64>,
}

impl Load {
    fn from_argv(argv: &[String]) -> Load {
        let num = |flag: &str| argv.iter().position(|a| a == flag).and_then(|i| argv.get(i + 1)).and_then(|v| v.parse().ok()).filter(|n| *n > 0);
        Load { status_every_ms: num("--status-every-ms"), status_phase_ms: num("--status-phase-ms"), output_bytes_per_sec: num("--output-bytes-per-sec") }
    }
}

struct Term {
    facts: ResourceFacts,
    load: Load,
    /// Cleared when the terminal finishes; stops its load tasks.
    alive: Arc<AtomicBool>,
}

fn unix_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

pub fn start(name: String, events: mpsc::UnboundedSender<(String, AdapterEvent)>) -> AdapterHandle {
    let (cmd_tx, mut cmds) = mpsc::unbounded_channel::<AdapterCmd>();
    let handle = AdapterHandle { name: name.clone(), capabilities: json!({"terminal.spawn": {}, "terminal.input": {}, "terminal.output": {"raw": true}, "terminal.kill": {}}), cmds: cmd_tx };
    // Status ticks from agent-like terminals come back through the adapter loop, so `terms` stays
    // the one source of truth for facts.
    let (tick_tx, mut ticks) = mpsc::unbounded_channel::<String>();
    tokio::spawn(async move {
        let mut terms: HashMap<String, Term> = HashMap::new();
        let mut n = 0;
        let _ = events.send((name.clone(), AdapterEvent::Connected(true, None)));
        let _ = events.send((name.clone(), AdapterEvent::Snapshot(vec![])));
        loop {
            let cmd = tokio::select! {
                Some(r) = ticks.recv() => {
                    if let Some(t) = terms.get_mut(&r).filter(|t| t.facts.state == "live") {
                        let count = t.facts.status.get("n").and_then(|n| n.as_u64()).unwrap_or(0) + 1;
                        let state = if count % 2 == 1 { "working" } else { "idle" };
                        t.facts.status = json!({"state": state, "n": count, "at_ms": unix_ms()});
                        let _ = events.send((name.clone(), AdapterEvent::Upsert(t.facts.clone())));
                    }
                    continue;
                }
                cmd = cmds.recv() => match cmd { Some(c) => c, None => break },
            };
            match cmd {
                AdapterCmd::Spawn(req, reply) => {
                    n += 1;
                    let load = Load::from_argv(&req.argv);
                    let f = ResourceFacts {
                        backend_ref: format!("f{n}"),
                        kind: "terminal".into(),
                        name: req.name,
                        title: None,
                        state: "live".into(),
                        status: Value::Null,
                        exit: Value::Null,
                        argv: req.argv,
                        cwd: req.cwd.clone(),
                    };
                    let alive = Arc::new(AtomicBool::new(true));
                    if let Some(ms) = load.status_every_ms {
                        let (tick, r, alive) = (tick_tx.clone(), f.backend_ref.clone(), alive.clone());
                        let first = tokio::time::Instant::now() + Duration::from_millis(load.status_phase_ms.unwrap_or(ms));
                        tokio::spawn(async move {
                            let mut every = tokio::time::interval_at(first, Duration::from_millis(ms));
                            while alive.load(Ordering::Relaxed) {
                                every.tick().await;
                                if tick.send(r.clone()).is_err() {
                                    break;
                                }
                            }
                        });
                    }
                    terms.insert(f.backend_ref.clone(), Term { facts: f.clone(), load, alive });
                    let _ = events.send((name.clone(), AdapterEvent::Upsert(f.clone())));
                    let _ = reply.send(Ok(f));
                }
                AdapterCmd::Kill(r, reply) => match terms.get_mut(&r) {
                    None => drop(reply.send(Err(format!("no terminal {r}")))),
                    Some(t) => {
                        t.alive.store(false, Ordering::Relaxed);
                        t.facts.state = "finished".into();
                        t.facts.exit = json!({"reason": "killed"});
                        let _ = events.send((name.clone(), AdapterEvent::Upsert(t.facts.clone())));
                        let _ = reply.send(Ok(json!({"reason": "killed"})));
                    }
                },
                AdapterCmd::Attach { backend_ref, reply, .. } => {
                    let Some(t) = terms.get(&backend_ref) else {
                        let _ = reply.send(Err(format!("no terminal {backend_ref}")));
                        continue;
                    };
                    let (in_tx, mut in_rx) = crate::queue::channel(STREAM_BUFFER_BYTES);
                    let (out_tx, out_rx) = crate::queue::channel(STREAM_BUFFER_BYTES);
                    let greeting = format!("fake {backend_ref}\r\n").into_bytes();
                    let n = greeting.len();
                    let _ = out_tx.try_send(StreamOutput::Data(greeting), n);
                    if let Some(bps) = t.load.output_bytes_per_sec {
                        flood(out_tx.clone(), bps, t.alive.clone());
                    }
                    tokio::spawn(async move {
                        while let Some(i) = in_rx.recv().await {
                            match i.value {
                                StreamInput::Data(d) => {
                                    let n = d.len();
                                    if out_tx.send(StreamOutput::Data(d), n).await.is_err() {
                                        break;
                                    }
                                }
                                StreamInput::Resize(..) | StreamInput::Consumed(_) => {}
                                StreamInput::Close => {
                                    let _ = out_tx.send(StreamOutput::Closed(json!({"reason": "detached"})), 64).await;
                                    break;
                                }
                            }
                        }
                    });
                    let _ = reply.send(Ok(Stream { input: in_tx, output: out_rx }));
                }
            }
        }
    });
    handle
}

/// Write `bps` bytes per second of printable lines to one stream until it closes or the terminal ends.
fn flood(out: crate::queue::Sender<StreamOutput>, bps: u64, alive: Arc<AtomicBool>) {
    const TICK_MS: u64 = 10;
    let per_tick = (bps.saturating_mul(TICK_MS) / 1000).clamp(1, 16 * 1024 * 1024) as usize;
    let line = b"0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ flood output\r\n";
    let chunk: Vec<u8> = line.iter().copied().cycle().take(per_tick.min(MAX_STREAM_CHUNK)).collect();
    tokio::spawn(async move {
        let mut every = tokio::time::interval(Duration::from_millis(TICK_MS));
        every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        while alive.load(Ordering::Relaxed) {
            every.tick().await;
            let mut remaining = per_tick;
            while remaining > 0 {
                let n = remaining.min(chunk.len());
                if out.send(StreamOutput::Data(chunk[..n].to_vec()), n).await.is_err() { return; }
                remaining -= n;
            }
        }
    });
}
