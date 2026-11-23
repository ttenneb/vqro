#![cfg(unix)] // Unix-socket/signal fixtures; on Windows the lib's unit tests run (docs/windows.md).
//! Production bridge binary over the provider wire against a private socket core stub and native
//! fake app-server. No installed runtime, model, live core, backend or credential store is opened.
#[path = "support/python.rs"] mod test_python;
use base64::Engine;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::UnixListener;
use tokio::process::{Child, Command};
use tokio::sync::watch;
use vqro::rpc::{Incoming, Peer};

type Facts = Arc<Mutex<BTreeMap<String, Value>>>;
const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;
#[path = "codex_provider/items.rs"]
mod items;

#[path = "support/frame_metadata.rs"]
mod frame_metadata;

struct Core {
    peer: Peer,
    generation: u64,
    facts: Facts,
    events: Arc<Mutex<Vec<Value>>>,
    auto_credit: Arc<Mutex<BTreeSet<u64>>>,
    reject_syncs: Arc<std::sync::atomic::AtomicBool>,
    owner: Arc<Mutex<Value>>,
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
        let events = Arc::new(Mutex::new(Vec::new()));
        let recorded = events.clone();
        let auto_credit = Arc::new(Mutex::new(BTreeSet::new()));
        let credited = auto_credit.clone();
        let reject_syncs = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let reject = reject_syncs.clone();
        let owner = Arc::new(Mutex::new(Value::Null));
        let owner_read = owner.clone();
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
                                assert_eq!(
                                    params["capabilities"]["host"]["kind"],
                                    json!(["server"])
                                );
                                assert_eq!(
                                    params["capabilities"]["agent.send"]["correlation"],
                                    json!(["acceptance"])
                                );
                                assert!(params["capabilities"]["items.stream"].is_object());
                                json!({"provider":params["name"],"generation":generation,"resume_token":"resume-native",
                                    "limits":{"stream_window_bytes":262144,"max_record_bytes":1048576}})
                            }
                            "workspace.get" => {
                                assert_eq!(params["key"], vqro::principal::OWNER_KEY);
                                recorded
                                    .lock()
                                    .unwrap()
                                    .push(json!({"method":"workspace.get","params":params}));
                                let owner = owner_read.lock().unwrap().clone();
                                if owner["fixture_error"] == true {
                                    server.respond(
                                        id,
                                        Err(vqro::rpc::RpcError::new(
                                            "unavailable",
                                            "fixture owner unavailable",
                                        )),
                                    );
                                    continue;
                                }
                                owner
                            }
                            "provider.sync" => {
                                recorded
                                    .lock()
                                    .unwrap()
                                    .push(json!({"method":"provider.sync","params":params}));
                                assert_eq!(params["generation"], generation);
                                if reject.load(std::sync::atomic::Ordering::Relaxed) {
                                    server.respond(
                                        id,
                                        Err(vqro::rpc::RpcError::invalid("fixture refuses sync")),
                                    );
                                    continue;
                                }
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
                        if method.starts_with("stream.") {
                            recorded
                                .lock()
                                .unwrap()
                                .push(json!({"method":method,"params":params}));
                            changed.send_modify(|n| *n += 1);
                            if method == "stream.data" {
                                let id = params["stream"].as_u64().unwrap();
                                if credited.lock().unwrap().contains(&id) {
                                    let bytes =
                                        B64.decode(params["data"].as_str().unwrap()).unwrap().len();
                                    server.notify(
                                        "stream.credit",
                                        json!({"generation":generation,"stream":id,"bytes":bytes}),
                                    );
                                }
                            }
                        }
                    }
                }
            }
        });
        Self {
            peer,
            generation,
            facts,
            events,
            auto_credit,
            reject_syncs,
            owner,
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
            "author":"client:wire:99","actor":"actor:test","principal":"p:test","initiator":"caller:original",
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
            test_python::python(),
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

// Session ACK does not wait for optional startup catalogs. In this fixture the collector's
// last request is mcpServerStatus/list, which the fake rejects (so there is no next page).
// Establish the request boundary before a zero-IPC assertion; do not filter any later calls.
async fn native_after_startup_requests(root: &tempfile::TempDir) -> String {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let trace = std::fs::read_to_string(root.path().join("native.jsonl")).unwrap();
            if trace.lines().any(|line| {
                serde_json::from_str::<Value>(line)
                    .ok()
                    .is_some_and(|row| row["method"] == "mcpServerStatus/list")
            }) {
                return trace;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("fixture startup request sequence did not finish")
}

#[tokio::test]
async fn injectable_frame_metadata_is_rejected_before_any_native_ipc() {
    let root = tempfile::tempdir().unwrap();
    let listener = UnixListener::bind(root.path().join("core.sock")).unwrap();
    let child = bridge(&root, 1, 0, &[]);
    let mut core = Core::accept(&listener, 1, "bootstrap").await;
    core.wait(|facts| facts.len() == 1).await;
    core.effect(
        "new",
        "endpoint-1",
        "agent.session",
        json!({"v":0,"action":"new"}),
        1000,
    )
    .await;
    let before = native_after_startup_requests(&root).await;
    for (index, (label, mut args)) in frame_metadata::invalid_inputs(&send("invalid-frame"))
        .into_iter()
        .enumerate()
    {
        args["delivery"] = json!(format!("invalid-frame-{index}"));
        let result = core
            .effect(
                &format!("invalid-frame-{index}"),
                "endpoint-1",
                "agent.send",
                args,
                1000,
            )
            .await;
        assert_eq!(result["outcome"], "rejected", "{label}");
        assert_eq!(result["error"]["kind"], "invalid", "{label}");
        assert_eq!(
            std::fs::read_to_string(root.path().join("native.jsonl")).unwrap(),
            before,
            "{label} reached native IPC"
        );
    }
    finish(child).await;
    core.close().await;
}

#[tokio::test]
async fn owner_human_is_literal_only_after_valid_current_public_owner_read() {
    let root = tempfile::tempdir().unwrap();
    let listener = UnixListener::bind(root.path().join("core.sock")).unwrap();
    let child = bridge(&root, 1, 0, &[]);
    let mut core = Core::accept(&listener, 1, "bootstrap").await;
    core.wait(|facts| facts.len() == 1).await;
    core.effect(
        "new",
        "endpoint-1",
        "agent.session",
        json!({"v":0,"action":"new"}),
        1000,
    )
    .await;
    for (index, owner) in [
        json!({"value":"p:test"}),
        Value::Null,
        json!({"value":"owner-unset"}),
        json!({"value":"different-person"}),
        json!({"fixture_error":true}),
    ]
    .into_iter()
    .enumerate()
    {
        *core.owner.lock().unwrap() = owner;
        let mut args = send(&format!("owner-{index}"));
        args["origin"] = json!({"kind":"human","owner_proven":true}); // This caller claim proves nothing.
        args["quoted"] = json!({"label":"untrusted quote"});
        let result = core
            .effect(
                &format!("owner-{index}"),
                "endpoint-1",
                "agent.send",
                args.clone(),
                1000,
            )
            .await;
        assert_eq!(result["outcome"], "accepted");
        assert_eq!(result["data"]["receipt"]["origin"]["principal"], "p:test");
        let rows: Vec<Value> = std::fs::read_to_string(root.path().join("native.jsonl"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let native = &rows
            .iter()
            .filter(|row| row["method"] == "turn/start")
            .last()
            .unwrap()["params"]["input"];
        let native = native.as_array().unwrap();
        assert_eq!(native.last().unwrap()["text"], args["content"][0]["text"]);
        if index == 0 {
            assert_eq!(
                native.len(),
                1,
                "proved owner-human gets no sender/quoted metadata block"
            );
        } else {
            assert_eq!(
                native.len(),
                3,
                "unknown/invalid/unavailable owner remains attributed"
            );
            assert_eq!(
                native[0]["text"],
                "from p:test via actor:test\nasserted kind: human"
            );
        }
    }
    assert_eq!(
        core.events
            .lock()
            .unwrap()
            .iter()
            .filter(|row| row["method"] == "workspace.get")
            .count(),
        5
    );
    finish(child).await;
    core.close().await;
}

#[tokio::test]
async fn provider_wire_binds_omitted_identity_and_rejects_spoofing_before_native_input() {
    let root = tempfile::tempdir().unwrap();
    let listener = UnixListener::bind(root.path().join("core.sock")).unwrap();
    let child = bridge(&root, 1, 0, &[]);
    let mut core = Core::accept(&listener, 1, "bootstrap").await;
    core.wait(|facts| facts.len() == 1).await;
    core.effect(
        "new",
        "endpoint-1",
        "agent.session",
        json!({"v":0,"action":"new"}),
        1000,
    )
    .await;
    let native_before = native_after_startup_requests(&root).await;
    for (index, field) in ["principal", "actor", "initiator"].into_iter().enumerate() {
        let mut args = send(&format!("forged-{index}"));
        args["origin"][field] = json!("not-the-recorded-caller");
        let result = core
            .effect(
                &format!("forged-{index}"),
                "endpoint-1",
                "agent.send",
                args,
                1000,
            )
            .await;
        assert_eq!(result["outcome"], "rejected");
        assert_eq!(result["error"]["kind"], "forbidden");
    }
    let before = std::fs::read_to_string(root.path().join("native.jsonl")).unwrap();
    assert_eq!(
        before, native_before,
        "spoof refusals perform zero native calls"
    );
    assert!(
        !before
            .lines()
            .any(|line| serde_json::from_str::<Value>(line).unwrap()["method"] == "turn/start")
    );
    let mut args = send("ui-kind-only");
    args["origin"] = json!({"kind":"human"});
    let result = core
        .effect("ui-kind-only", "endpoint-1", "agent.send", args, 1000)
        .await;
    assert_eq!(result["outcome"], "accepted");
    assert_eq!(
        result["data"]["receipt"]["origin"],
        json!({"principal":"p:test","actor":"actor:test",
        "initiator":"caller:original","kind":"human","claimed":["kind"]})
    );
    let ledger = std::fs::read_to_string(root.path().join("native.jsonl")).unwrap();
    let calls: Vec<Value> = ledger
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .filter(|row: &Value| row["method"] == "turn/start")
        .collect();
    assert_eq!(calls.len(), 1);
    let text = calls[0]["params"]["input"][0]["text"].as_str().unwrap();
    assert_eq!(text, "from p:test via actor:test\nasserted kind: human");
    assert!(
        !text.contains("client:wire:99"),
        "actual author is not substituted for principal or actor"
    );
    assert!(text.contains("asserted kind: human"));
    finish(child).await;
    core.close().await;
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
        initial["endpoint-1"]["data"]["agent"]["caps"]["host"]["kind"],
        "server"
    );
    assert_eq!(
        initial["endpoint-1"]["data"]["agent"]["host"]["provider"],
        "codex-test"
    );
    assert_eq!(
        initial["endpoint-1"]["data"]["agent"]["host"]["incarnation"],
        initial["endpoint-1"]["incarnation"]
    );
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
    let bound_origin = json!({"principal":"p:test","actor":"actor:test","initiator":"caller:original",
        "kind":"agent","agent":"r7","claimed":["kind","agent"]});
    assert_eq!(receipt["data"]["receipt"]["origin"], bound_origin);
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
    assert_eq!(
        receipt["data"]["origin"], bound_origin,
        "late receipt retains the first core-bound caller across reconnect"
    );
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
async fn session_observed_reply_waits_for_confirmed_core_facts() {
    let root = tempfile::tempdir().unwrap();
    let listener = UnixListener::bind(root.path().join("core.sock")).unwrap();
    let child = bridge(&root, 1, 0, &[]);
    let mut core = Core::accept(&listener, 1, "bootstrap").await;
    core.wait(|f| f.len() == 1).await;
    let before = core
        .events
        .lock()
        .unwrap()
        .iter()
        .filter(|e| e["method"] == "provider.sync")
        .count();
    let result = core
        .effect(
            "new-fenced",
            "endpoint-1",
            "agent.session",
            json!({"v":0,"action":"new"}),
            1000,
        )
        .await;
    assert_eq!(result["outcome"], "observed");
    let after = core
        .events
        .lock()
        .unwrap()
        .iter()
        .filter(|e| e["method"] == "provider.sync")
        .count();
    let session = core.facts.lock().unwrap()["endpoint-1"]["data"]["agent"]["session"].clone();
    finish(child).await;
    core.close().await;
    assert!(
        after > before,
        "an upsert notification alone cannot fence final audit after facts commit"
    );
    assert_eq!(session, result["data"]["session"]);
}

async fn future_model_readback_case(extra: &[&str], status: &str, actual: Option<&str>) {
    let root = tempfile::tempdir().unwrap();
    let listener = UnixListener::bind(root.path().join("core.sock")).unwrap();
    let child = bridge(&root, 1, 0, extra);
    let mut core = Core::accept(&listener, 1, "bootstrap").await;
    core.wait(|facts| facts.len() == 1).await;
    core.effect(
        "new",
        "endpoint-1",
        "agent.session",
        json!({"v":0,"action":"new"}),
        1000,
    )
    .await;
    let before = native_after_startup_requests(&root).await;
    let configured = core
        .effect(
            "intent",
            "endpoint-1",
            "agent.configure",
            json!({"v":0,"id":"model","value":"fake-alternate"}),
            1000,
        )
        .await;
    assert_eq!(configured["outcome"], "accepted");
    core.wait(|facts| {
        facts
            .get("endpoint-1")
            .is_some_and(|row| row["data"]["agent"]["config"][0]["pending"] == true)
    })
    .await;
    assert_eq!(
        std::fs::read_to_string(root.path().join("native.jsonl")).unwrap(),
        before,
        "intent must perform zero native calls"
    );
    assert_eq!(
        core.facts.lock().unwrap()["endpoint-1"]["data"]["agent"]["session"]["model"]["id"],
        "fake-model"
    );
    let sent=core.effect("future-send","endpoint-1","agent.send",json!({"v":0,"mode":"admit","delivery":"future-one","origin":{"principal":"p:test","actor":"actor:test","kind":"agent"},"content":[{"type":"text","text":"one"}]}),1000).await;
    assert_eq!(sent["outcome"], "accepted");
    core.wait(|facts| {
        facts
            .get("endpoint-1")
            .is_some_and(|row| row["data"]["agent"]["config"][0]["status"] == status)
    })
    .await;
    let facts = core.facts.lock().unwrap()["endpoint-1"]["data"]["agent"].clone();
    assert_eq!(facts["config"][0]["value"], "fake-alternate");
    assert_eq!(facts["config"][0]["pending"], status != "confirmed");
    if let Some(model) = actual {
        assert_eq!(facts["session"]["model"]["id"], model);
    } else {
        assert!(facts["session"].get("model").is_none());
    }
    if extra.contains(&"--reject-model-read") {
        assert_eq!(
            facts["config"][0]["diagnostic"]["native_error"]["code"],
            -32600
        );
        assert_eq!(
            facts["config"][0]["diagnostic"]["native_error"]["message"],
            "fixture native model read rejected"
        );
    }
    finish(child).await;
    core.close().await;
    let rows: Vec<Value> = std::fs::read_to_string(root.path().join("native.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(
        rows.iter()
            .filter(|row| row["method"] == "turn/start")
            .count(),
        1
    );
    assert_eq!(
        rows.iter()
            .filter(|row| row["method"] == "thread/read")
            .count(),
        1
    );
    assert!(rows.iter().all(|row| row["method"] != "thread/resume"
        && !row["method"].as_str().unwrap_or("").starts_with("config/")));
}
#[tokio::test]
async fn future_native_read_error_is_metadata_unknown_not_failed_or_replayed_delivery() {
    future_model_readback_case(&["--reject-model-read"], "unknown", None).await;
}
#[tokio::test]
async fn future_native_mismatch_reports_actual_configured_model_without_claiming_applied() {
    future_model_readback_case(&["--ignore-model-override"], "mismatch", Some("fake-model")).await;
}
#[tokio::test]
async fn future_native_missing_readback_never_manufactures_the_selected_model() {
    future_model_readback_case(&["--omit-model-readback"], "unknown", None).await;
}

#[tokio::test]
async fn session_publication_refusal_keeps_native_observation_and_never_replays() {
    let root = tempfile::tempdir().unwrap();
    let listener = UnixListener::bind(root.path().join("core.sock")).unwrap();
    let child = bridge(&root, 1, 0, &[]);
    let mut core = Core::accept(&listener, 1, "bootstrap").await;
    core.wait(|f| f.len() == 1).await;
    core.reject_syncs
        .store(true, std::sync::atomic::Ordering::Relaxed);
    let incarnation = core.facts.lock().unwrap()["endpoint-1"]["incarnation"].clone();
    let result = core.peer.call_with_timeout("effect.invoke",json!({"generation":1,"effect":"new-refused",
        "ref":"endpoint-1","incarnation":incarnation,"op":"agent.session","args":{"v":0,"action":"new"},"deadline_ms":1000}),Duration::from_secs(2)).await;
    finish(child).await;
    core.close().await;
    let error = result.unwrap_err().data.unwrap();
    assert_eq!(error["outcome"], "indeterminate");
    assert_eq!(error["native_outcome"], "observed");
    assert_eq!(error["native_data"]["session"]["id"], "thread-1");
    let calls: Vec<Value> = std::fs::read_to_string(root.path().join("native.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(
        calls
            .iter()
            .filter(|v| v["method"] == "thread/start")
            .count(),
        1,
        "publication failure cannot replay a native session effect"
    );
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
