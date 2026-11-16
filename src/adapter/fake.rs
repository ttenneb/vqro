//! Deterministic fake backend: a test fixture, not a product (D16). Terminals echo their input.

use std::collections::HashMap;

use serde_json::{json, Value};
use tokio::sync::mpsc;

use super::*;

pub fn start(name: String, events: mpsc::UnboundedSender<(String, AdapterEvent)>) -> AdapterHandle {
    let (cmd_tx, mut cmds) = mpsc::unbounded_channel::<AdapterCmd>();
    let handle = AdapterHandle { name: name.clone(), capabilities: json!({"terminal.spawn": {}, "terminal.input": {}, "terminal.output": {"raw": true}, "terminal.kill": {}}), cmds: cmd_tx };
    tokio::spawn(async move {
        let mut terms: HashMap<String, ResourceFacts> = HashMap::new();
        let mut n = 0;
        let _ = events.send((name.clone(), AdapterEvent::Connected(true, None)));
        let _ = events.send((name.clone(), AdapterEvent::Snapshot(vec![])));
        while let Some(cmd) = cmds.recv().await {
            match cmd {
                AdapterCmd::Spawn(req, reply) => {
                    n += 1;
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
                    terms.insert(f.backend_ref.clone(), f.clone());
                    let _ = events.send((name.clone(), AdapterEvent::Upsert(f.clone())));
                    let _ = reply.send(Ok(f));
                }
                AdapterCmd::Kill(r, reply) => match terms.get_mut(&r) {
                    None => drop(reply.send(Err(format!("no terminal {r}")))),
                    Some(f) => {
                        f.state = "finished".into();
                        f.exit = json!({"reason": "killed"});
                        let _ = events.send((name.clone(), AdapterEvent::Upsert(f.clone())));
                        let _ = reply.send(Ok(json!({"reason": "killed"})));
                    }
                },
                AdapterCmd::Attach { backend_ref, reply, .. } => {
                    if !terms.contains_key(&backend_ref) {
                        let _ = reply.send(Err(format!("no terminal {backend_ref}")));
                        continue;
                    }
                    let (in_tx, mut in_rx) = mpsc::unbounded_channel();
                    let (out_tx, out_rx) = mpsc::unbounded_channel();
                    let _ = out_tx.send(StreamOutput::Data(format!("fake {backend_ref}\r\n").into_bytes()));
                    tokio::spawn(async move {
                        while let Some(i) = in_rx.recv().await {
                            match i {
                                StreamInput::Data(d) => {
                                    if out_tx.send(StreamOutput::Data(d)).is_err() {
                                        break;
                                    }
                                }
                                StreamInput::Resize(..) => {}
                                StreamInput::Close => {
                                    let _ = out_tx.send(StreamOutput::Closed(json!({"reason": "detached"})));
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
