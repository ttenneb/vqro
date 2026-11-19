//! Production bridge binary over the provider wire against a private socket core stub and native
//! fake app-server. No installed runtime, model, live core, backend or credential store is opened.
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::UnixListener;
use tokio::process::{Child, Command};
use tokio::sync::watch;
use vqro::rpc::{Incoming, Peer};

type Facts = Arc<Mutex<BTreeMap<String, Value>>>;

struct Core {
    peer: Peer,
    generation: u64,
    facts: Facts,
    changed: watch::Receiver<u64>,
    reader: tokio::task::JoinHandle<()>,
}

impl Core {
    async fn accept(listener: &UnixListener, generation: u64, expected_token: &str) -> Self {
        let (stream, _) = tokio::time::timeout(Duration::from_secs(5), listener.accept())
            .await
            .unwrap()
            .unwrap();
        let (read, write) = stream.into_split();
        let (peer, mut incoming) = Peer::spawn(read, write);
        let facts: Facts = Arc::new(Mutex::new(BTreeMap::new()));
        let observed = facts.clone();
        let (changed, changed_rx) = watch::channel(0);
        let server = peer.clone();
        let expected_token = expected_token.to_string();
        let reader = tokio::spawn(async move {
            while let Some(message) = incoming.recv().await {
                match message {
                    Incoming::Request { id, method, params } => {
                        let result = match method.as_str() {
                            "hello" => json!({"server":"private-core","epoch":"fake-core"}),
                            "provider.register" => {
                                assert_eq!(params["role"], "runtime");
                                assert_eq!(params["token"], expected_token);
                                // Intermediate builds must not claim missing item observation.
                                assert!(params["capabilities"].get("items.stream").is_none());
                                json!({"provider":params["name"],"generation":generation,"resume_token":"resume-native",
                                    "limits":{"stream_window_bytes":262144,"max_record_bytes":1048576}})
                            }
                            "provider.sync" => {
                                assert_eq!(params["generation"], generation);
                                for fact in params["resources"].as_array().unwrap() {
                                    observed
                                        .lock()
                                        .unwrap()
                                        .insert(fact["ref"].as_str().unwrap().into(), fact.clone());
                                }
                                changed.send_modify(|n| *n += 1);
                                json!({})
                            }
                            other => panic!("unexpected provider request {other}"),
                        };
                        server.respond(id, Ok(result));
                    }
                    Incoming::Notification { method, params } => {
                        assert_eq!(params["generation"], generation);
                        if method == "resource.upsert" {
                            let fact = &params["facts"];
                            observed
                                .lock()
                                .unwrap()
                                .insert(fact["ref"].as_str().unwrap().into(), fact.clone());
                            changed.send_modify(|n| *n += 1);
                        }
                    }
                }
            }
        });
        Self {
            peer,
            generation,
            facts,
            changed: changed_rx,
            reader,
        }
    }

    async fn wait(&mut self, predicate: impl Fn(&BTreeMap<String, Value>) -> bool) {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if predicate(&self.facts.lock().unwrap()) {
                    return;
                }
                self.changed.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
    }

    async fn effect(
        &self,
        effect: &str,
        reference: &str,
        op: &str,
        args: Value,
        deadline_ms: u64,
    ) -> Value {
        let incarnation = self.facts.lock().unwrap()[reference]["incarnation"].clone();
        self.peer.call_with_timeout("effect.invoke",json!({"generation":self.generation,"effect":effect,
            "ref":reference,"incarnation":incarnation,"op":op,"args":args,"deadline_ms":deadline_ms}),Duration::from_secs(5)).await.unwrap()
    }

    async fn close(self) {
        self.peer.close();
        self.reader.await.unwrap();
    }
}

fn bridge(root: &tempfile::TempDir, slots: usize, delay_ms: usize, extra: &[&str]) -> Child {
    let mut command = Command::new(env!("CARGO_BIN_EXE_vqro-bridge-codex"));
    command
        .env_clear()
        .env("HOME", root.path())
        .env("PATH", "/usr/bin:/bin")
        .env("VQRO_SOCKET", root.path().join("core.sock"))
        .env("VQRO_BRIDGE_TOKEN", "bootstrap")
        .env("VQRO_STATE_DIR", root.path().join("state"))
        .env("VQRO_EXTENSIONS_DIR", root.path().join("extensions"))
        .env("XDG_RUNTIME_DIR", root.path().join("run"))
        .env("HC_SOCKET", root.path().join("hc.sock"))
        .env("HC_SESSION", "codex-test")
        .args([
            "--slots",
            &slots.to_string(),
            "--instance",
            "test",
            "--",
            "/usr/bin/python3",
        ])
        .arg(format!(
            "{}/tests/fixtures/fake_codex.py",
            env!("CARGO_MANIFEST_DIR")
        ))
        .arg("--ledger")
        .arg(root.path().join("native.jsonl"))
        .args(["--reply-delay-ms", &delay_ms.to_string()])
        .args(extra)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    command.spawn().unwrap()
}

async fn finish(mut child: Child) {
    drop(child.stdin.take());
    let status = tokio::time::timeout(Duration::from_secs(6), child.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(status.success(), "bridge failed: {status}");
}

fn send(delivery: &str) -> Value {
    json!({"v":0,"mode":"admit","delivery":delivery,"content":[{"type":"text","text":"literal /no-command\nline2"}],"origin":{"principal":"p:test","actor":"actor:test","kind":"agent","agent":"r7"}})
}

#[tokio::test]
async fn binary_registers_three_endpoints_and_rejects_undeclared_modes_before_native_input() {
    let root = tempfile::tempdir().unwrap();
    let listener = UnixListener::bind(root.path().join("core.sock")).unwrap();
    let child = bridge(&root, 3, 0, &[]);
    let mut core = Core::accept(&listener, 1, "bootstrap").await;
    core.wait(|facts| facts.len() == 3).await;
    let initial = core.facts.lock().unwrap().clone();
    assert_eq!(
        initial["endpoint-1"]["data"]["agent"]["host"],
        initial["endpoint-3"]["data"]["agent"]["host"]
    );
    for i in 1..=3 {
        let result = core
            .effect(
                &format!("new-{i}"),
                &format!("endpoint-{i}"),
                "agent.session",
                json!({"v":0,"action":"new"}),
                1000,
            )
            .await;
        assert_eq!(result["outcome"], "observed");
    }
    let mut wrong = send("wrong");
    wrong["mode"] = json!("start");
    let rejected = core
        .effect("wrong", "endpoint-1", "agent.send", wrong, 1000)
        .await;
    assert_eq!(rejected["outcome"], "rejected");
    assert_eq!(rejected["error"]["kind"], "unsupported");
    let accepted = core
        .effect("send", "endpoint-1", "agent.send", send("d1"), 1000)
        .await;
    assert_eq!(accepted["outcome"], "accepted");
    assert_eq!(accepted["data"]["receipt"]["stage"], "admitted");
    let ledger = std::fs::read_to_string(root.path().join("native.jsonl")).unwrap();
    assert_eq!(
        ledger
            .lines()
            .filter(|line| serde_json::from_str::<Value>(line).unwrap()["method"] == "turn/start")
            .count(),
        1
    );
    finish(child).await;
    core.wait(|facts| {
        facts
            .values()
            .all(|f| f["data"]["agent"]["lifecycle"] == "ended")
    })
    .await;
    core.close().await;
}

#[tokio::test]
async fn core_reconnect_keeps_native_process_session_and_late_receipt_without_replay() {
    let root = tempfile::tempdir().unwrap();
    let listener = UnixListener::bind(root.path().join("core.sock")).unwrap();
    let child = bridge(&root, 1, 400, &[]);
    let mut first = Core::accept(&listener, 1, "bootstrap").await;
    first.wait(|facts| facts.len() == 1).await;
    first
        .effect(
            "new",
            "endpoint-1",
            "agent.session",
            json!({"v":0,"action":"new"}),
            1000,
        )
        .await;
    first
        .wait(|facts| facts["endpoint-1"]["data"]["agent"]["session"].is_object())
        .await;
    let session = first.facts.lock().unwrap()["endpoint-1"]["data"]["agent"]["session"].clone();
    let receipt = first
        .effect("send", "endpoint-1", "agent.send", send("delayed"), 50)
        .await;
    assert_eq!(receipt["outcome"], "dispatched");
    first.close().await;
    let mut second = Core::accept(&listener, 2, "resume-native").await;
    second.wait(|facts| facts.len() == 1).await;
    assert_eq!(
        second.facts.lock().unwrap()["endpoint-1"]["data"]["agent"]["session"],
        session
    );
    second
        .wait(|facts| facts["endpoint-1"]["data"]["agent"]["state"] == "idle")
        .await;
    let receipt = second
        .effect(
            "lookup",
            "endpoint-1",
            "agent.receipt",
            json!({"v":0,"delivery":"delayed"}),
            1000,
        )
        .await;
    assert_eq!(receipt["data"]["stage"], "admitted");
    let ledger = std::fs::read_to_string(root.path().join("native.jsonl")).unwrap();
    let calls: Vec<Value> = ledger
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(
        calls.iter().filter(|v| v["method"] == "initialize").count(),
        1
    );
    assert_eq!(
        calls
            .iter()
            .filter(|v| v["method"] == "thread/start")
            .count(),
        1
    );
    assert_eq!(
        calls.iter().filter(|v| v["method"] == "turn/start").count(),
        1
    );
    finish(child).await;
    second
        .wait(|facts| facts["endpoint-1"]["data"]["agent"]["lifecycle"] == "ended")
        .await;
    second.close().await;
}

#[tokio::test]
async fn native_exit_does_not_wait_for_still_open_bridge_stdin() {
    let root = tempfile::tempdir().unwrap();
    let _listener = UnixListener::bind(root.path().join("core.sock")).unwrap();
    let mut child = bridge(&root, 1, 0, &["--exit-after-init"]);
    let _keep_stdin_open = child.stdin.take().unwrap();
    let status = tokio::time::timeout(Duration::from_secs(5), child.wait())
        .await
        .unwrap()
        .unwrap();
    // Depending on whether native exit precedes initialization response processing, startup reports
    // success or a definite startup failure. Neither path can hang behind the OS stdin reader.
    assert!(status.success() || status.code() == Some(1));
}
