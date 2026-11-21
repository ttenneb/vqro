//! Reuse vqro's bounded symmetric RPC peer against a subprocess speaking native app-server JSONL.
use serde_json::{Value, json};
use std::process::Stdio;
use std::time::Duration;
use tokio::process::{Child, Command};
use vqro::rpc::{Incoming, IncomingReceiver, Peer};

async fn fixture(hold: bool) -> (tempfile::TempDir, Child, Peer, IncomingReceiver) {
    fixture_delay(hold, 0).await
}

async fn fixture_delay(
    hold: bool,
    delay_ms: u64,
) -> (tempfile::TempDir, Child, Peer, IncomingReceiver) {
    fixture_delays(hold, delay_ms, 0).await
}

async fn fixture_delays(
    hold: bool,
    delay_ms: u64,
    config_ms: u64,
) -> (tempfile::TempDir, Child, Peer, IncomingReceiver) {
    fixture_options(hold, delay_ms, config_ms, false).await
}
async fn fixture_options(
    hold: bool,
    delay_ms: u64,
    config_ms: u64,
    read_gate: bool,
) -> (tempfile::TempDir, Child, Peer, IncomingReceiver) {
    let root = tempfile::tempdir().unwrap();
    let mut child = Command::new("python3")
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", root.path())
        .env("CODEX_FAKE_HOLD", if hold { "1" } else { "0" })
        .env("CODEX_FAKE_REPLY_DELAY_MS", delay_ms.to_string())
        .env("CODEX_FAKE_MODEL_READ_DELAY_MS", config_ms.to_string())
        .env(
            "CODEX_FAKE_MODEL_READ_GATE",
            if read_gate {
                root.path().join("model-release").into_os_string()
            } else {
                std::ffi::OsString::new()
            },
        )
        .env("AGENT_CONFORMANCE_LEDGER", root.path().join("ledger.jsonl"))
        .arg(format!(
            "{}/tests/fixtures/fake_codex.py",
            env!("CARGO_MANIFEST_DIR")
        ))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let (peer, incoming) = Peer::spawn(child.stdout.take().unwrap(), child.stdin.take().unwrap());
    peer.set_max_record_bytes(1 << 20).unwrap();
    let init = peer
        .call(
            "initialize",
            json!({"clientInfo":{"name":"vqro-test","version":"0"}}),
        )
        .await
        .unwrap();
    assert_eq!(init["userAgent"], "fake-codex/fixture-1");
    peer.notify("initialized", json!({}));
    (root, child, peer, incoming)
}
async fn cleanup(mut child: Child, peer: Peer) {
    peer.close();
    tokio::time::timeout(Duration::from_secs(2), child.wait())
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn one_native_host_three_threads_literal_admission_and_observed_completion() {
    let (root, child, peer, mut incoming) = fixture(false).await;
    let mut sessions = Vec::new();
    for _ in 0..3 {
        let r = peer.call("thread/start", json!({})).await.unwrap();
        sessions.push(r["thread"]["id"].clone());
    }
    assert_ne!(sessions[0], sessions[1]);
    assert_ne!(sessions[1], sessions[2]);
    let text = "/literal\n\u{1b}[31m 💡";
    let accepted = peer
        .call(
            "turn/start",
            json!({"threadId":sessions[0],"input":[{"type":"text","text":text}]}),
        )
        .await
        .unwrap();
    let run = accepted["turn"]["id"].clone();
    tokio::time::timeout(Duration::from_secs(2), async {
        while let Some(message) = incoming.recv().await {
            if let Incoming::Notification { method, params } = message {
                if method == "turn/completed" && params["turn"]["id"] == run {
                    assert_eq!(params["turn"]["status"], "completed");
                    return;
                }
            }
        }
        panic!("native stream closed before completion");
    })
    .await
    .unwrap();
    let bytes = std::fs::read_to_string(root.path().join("ledger.jsonl")).unwrap();
    let ledger: Vec<Value> = bytes
        .lines()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect();
    let sends: Vec<_> = ledger
        .iter()
        .filter(|v| v["method"] == "turn/start")
        .collect();
    assert_eq!(sends.len(), 1);
    assert_eq!(sends[0]["params"]["input"][0]["text"], text);
    cleanup(child, peer).await;
}

#[tokio::test]
async fn lost_native_reply_is_one_write_and_control_remains_available() {
    let (root, child, peer, mut incoming) = fixture(true).await;
    let session = peer.call("thread/start", json!({})).await.unwrap()["thread"]["id"].clone();
    let result = peer
        .call_with_timeout(
            "turn/start",
            json!({"threadId":session,"input":[{"type":"text","text":"go"}]}),
            Duration::from_millis(100),
        )
        .await;
    assert_eq!(result.unwrap_err().kind(), "timeout");
    let run = tokio::time::timeout(Duration::from_secs(2), async {
        while let Some(message) = incoming.recv().await {
            if let Incoming::Notification { method, params } = message {
                if method == "turn/started" {
                    return params["turn"]["id"].clone();
                }
            }
        }
        panic!("native turn was not observed")
    })
    .await
    .unwrap();
    peer.call("turn/interrupt", json!({"threadId":session,"turnId":run}))
        .await
        .unwrap();
    let ledger = std::fs::read_to_string(root.path().join("ledger.jsonl")).unwrap();
    assert_eq!(
        ledger
            .lines()
            .filter(|line| serde_json::from_str::<Value>(line).unwrap()["method"] == "turn/start")
            .count(),
        1
    );
    cleanup(child, peer).await;
}

fn deadline() -> tokio::time::Instant {
    tokio::time::Instant::now() + Duration::from_secs(2)
}

async fn open_runtime(
    peer: Peer,
    mut incoming: IncomingReceiver,
    slots: usize,
) -> (
    std::sync::Arc<vqro::codex::runtime::Runtime>,
    tokio::task::JoinHandle<()>,
) {
    let runtime = vqro::codex::runtime::Runtime::new(peer, slots, "inc".into(), None);
    let reader = runtime.clone();
    let drain = tokio::spawn(async move {
        while let Some(message) = incoming.recv().await {
            reader.incoming(message);
        }
    });
    (runtime, drain)
}

#[tokio::test]
async fn runtime_slots_share_host_and_switch_sessions_without_changing_ref() {
    use vqro::provider_client::Outcome;
    let (_root, child, peer, incoming) = fixture(false).await;
    let (runtime, drain) = open_runtime(peer.clone(), incoming, 3).await;
    for i in 1..=3 {
        let outcome = runtime
            .invoke(
                &format!("endpoint-{i}"),
                "inc",
                "agent.session",
                json!({"v":0,"action":"new"}),
                deadline(),
            )
            .await
            .unwrap();
        assert!(matches!(outcome, Outcome::Observed(_)));
    }
    let before = runtime.facts();
    assert_ne!(
        before[0]["data"]["agent"]["session"],
        before[1]["data"]["agent"]["session"]
    );
    assert_eq!(
        before[0]["data"]["agent"]["host"],
        before[2]["data"]["agent"]["host"]
    );
    runtime
        .invoke(
            "endpoint-1",
            "inc",
            "agent.session",
            json!({"v":0,"action":"new"}),
            deadline(),
        )
        .await
        .unwrap();
    let after = runtime.facts();
    assert_eq!(before[0]["ref"], after[0]["ref"]);
    assert_ne!(
        before[0]["data"]["agent"]["session"],
        after[0]["data"]["agent"]["session"]
    );
    assert_ne!(
        before[0]["data"]["agent"]["epoch"],
        after[0]["data"]["agent"]["epoch"]
    );
    cleanup(child, peer).await;
    drain.await.unwrap();
}

#[tokio::test]
async fn provider_deadline_dispatches_once_then_native_receipt_arrives_later() {
    use vqro::provider_client::Outcome;
    let (root, child, peer, incoming) = fixture_delay(false, 200).await;
    let (runtime, drain) = open_runtime(peer.clone(), incoming, 1).await;
    runtime
        .invoke(
            "endpoint-1",
            "inc",
            "agent.session",
            json!({"v":0,"action":"new"}),
            deadline(),
        )
        .await
        .unwrap();
    let args = json!({"v":0,"mode":"admit","delivery":"delivery-1","content":[{"type":"text","text":"hello"}],"origin":{"principal":"p:test","actor":"actor:test","kind":"tool"}});
    let result = runtime
        .invoke(
            "endpoint-1",
            "inc",
            "agent.send",
            args.clone(),
            tokio::time::Instant::now() + Duration::from_millis(50),
        )
        .await
        .unwrap();
    assert!(
        matches!(result,Outcome::Dispatched(Some(ref data)) if data["receipt"]["delivery"]=="delivery-1")
    );
    let mut second_args = args.clone();
    second_args["delivery"] = json!("delivery-2");
    let second = runtime
        .invoke("endpoint-1", "inc", "agent.send", second_args, deadline())
        .await
        .unwrap();
    assert!(matches!(second,Outcome::Rejected{ref kind,..} if kind=="overloaded"));
    let mut changed = runtime.changes();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let receipt = runtime
                .invoke(
                    "endpoint-1",
                    "inc",
                    "agent.receipt",
                    json!({"v":0,"delivery":"delivery-1"}),
                    deadline(),
                )
                .await
                .unwrap();
            if matches!(receipt,Outcome::Observed(Some(ref r)) if r["stage"]=="admitted") {
                break;
            }
            changed.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    let ledger = std::fs::read_to_string(root.path().join("ledger.jsonl")).unwrap();
    assert_eq!(
        ledger
            .lines()
            .filter(|line| serde_json::from_str::<Value>(line).unwrap()["method"] == "turn/start")
            .count(),
        1
    );
    cleanup(child, peer).await;
    drain.await.unwrap();
}

#[tokio::test]
async fn future_intent_is_pending_without_native_write_then_bound_per_thread_readback() {
    use vqro::provider_client::Outcome;
    let (root, child, peer, incoming) = fixture(false).await;
    let (runtime, drain) = open_runtime(peer.clone(), incoming, 2).await;
    runtime.initialize_models(deadline()).await.unwrap();
    for id in ["endpoint-1", "endpoint-2"] {
        runtime
            .invoke(
                id,
                "inc",
                "agent.session",
                json!({"v":0,"action":"new"}),
                deadline(),
            )
            .await
            .unwrap();
    }
    let before = runtime.facts();
    let option = &before[0]["data"]["agent"]["config"][0];
    let ledger = root.path().join("ledger.jsonl");
    let original = std::fs::read_to_string(&ledger).unwrap();
    for (value, rev, kind) in [
        (json!("invented"), option["rev"].clone(), "invalid"),
        (json!("fake-alternate"), json!(0), "stale"),
    ] {
        let result = runtime
            .invoke(
                "endpoint-1",
                "inc",
                "agent.configure",
                json!({"v":0,"id":"model","value":value,"rev":rev}),
                deadline(),
            )
            .await
            .unwrap();
        assert!(matches!(result,Outcome::Rejected{kind:ref actual,..} if actual==kind));
        assert_eq!(std::fs::read_to_string(&ledger).unwrap(), original);
    }
    let result = runtime
        .invoke(
            "endpoint-1",
            "inc",
            "agent.configure",
            json!({"v":0,"id":"model","value":"fake-alternate","rev":option["rev"]}),
            deadline(),
        )
        .await
        .unwrap();
    assert!(matches!(result, Outcome::Accepted(_)));
    assert_eq!(
        std::fs::read_to_string(&ledger).unwrap(),
        original,
        "intent performs no native RPC"
    );
    let after = runtime.facts();
    assert_eq!(
        after[0]["data"]["agent"]["session"]["model"]["id"],
        "fake-model"
    );
    assert_eq!(
        after[0]["data"]["agent"]["config"][0]["value"],
        "fake-alternate"
    );
    assert_eq!(after[0]["data"]["agent"]["config"][0]["pending"], true);
    assert_eq!(
        after[0]["data"]["agent"]["config"][0]["applies_from"],
        "next_turn"
    );
    assert_eq!(
        after[1]["data"]["agent"]["session"]["model"]["id"],
        "fake-model"
    );
    assert_eq!(
        before[0]["data"]["agent"]["epoch"],
        after[0]["data"]["agent"]["epoch"]
    );
    for (i, id) in ["endpoint-1", "endpoint-2"].into_iter().enumerate() {
        let result=runtime.invoke(id,"inc","agent.send",json!({"v":0,"mode":"admit","delivery":format!("model-d{i}"),"origin":{"principal":"p","actor":"actor","kind":"agent"},"content":[{"type":"text","text":"literal"}]}),deadline()).await.unwrap();
        assert!(matches!(result, Outcome::Accepted(_)));
    }
    let rows: Vec<Value> = std::fs::read_to_string(&ledger)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let discovery: Vec<_> = rows
        .iter()
        .filter(|row| row["method"] == "model/list")
        .collect();
    assert_eq!(discovery.len(), 1);
    assert_eq!(discovery[0]["params"]["includeHidden"], false);
    let configured: Vec<_> = rows
        .iter()
        .filter(|row| row["method"] == "thread/resume")
        .collect();
    assert!(configured.is_empty());
    assert_eq!(
        runtime.facts()[0]["data"]["agent"]["session"]["model"]["id"],
        "fake-alternate"
    );
    assert_eq!(
        runtime.facts()[0]["data"]["agent"]["config"][0]["pending"],
        false
    );
    let reads: Vec<_> = rows
        .iter()
        .filter(|row| row["method"] == "thread/read")
        .collect();
    assert_eq!(reads.len(), 1);
    assert_eq!(
        reads[0]["params"],
        json!({"threadId":"thread-1","includeTurns":false})
    );
    let turns: Vec<_> = rows
        .iter()
        .filter(|row| row["method"] == "turn/start")
        .collect();
    assert_eq!(turns[0]["params"]["model"], "fake-alternate");
    assert!(turns[1]["params"].get("model").is_none());
    assert!(
        rows.iter()
            .all(|row| !row["method"].as_str().unwrap_or("").starts_with("config/"))
    );
    cleanup(child, peer).await;
    drain.await.unwrap();
}

#[tokio::test]
async fn late_model_readback_clears_pending_without_replaying_admitted_input() {
    use vqro::provider_client::Outcome;
    let (root, child, peer, incoming) = fixture_options(false, 0, 0, true).await;
    let (runtime, drain) = open_runtime(peer.clone(), incoming, 1).await;
    runtime.initialize_models(deadline()).await.unwrap();
    runtime
        .invoke(
            "endpoint-1",
            "inc",
            "agent.session",
            json!({"v":0,"action":"new"}),
            deadline(),
        )
        .await
        .unwrap();
    let mut changed = runtime.changes();
    let result = runtime
        .invoke(
            "endpoint-1",
            "inc",
            "agent.configure",
            json!({"v":0,"id":"model","value":"fake-alternate"}),
            tokio::time::Instant::now() + Duration::from_millis(80),
        )
        .await
        .unwrap();
    assert!(matches!(result, Outcome::Accepted(_)));
    assert_eq!(
        runtime.facts()[0]["data"]["agent"]["session"]["model"]["id"],
        "fake-model"
    );
    let result=tokio::time::timeout(Duration::from_secs(1),runtime.invoke("endpoint-1","inc","agent.send",json!({"v":0,"mode":"admit","delivery":"late-model","origin":{"principal":"p","actor":"actor","kind":"agent"},"content":[{"type":"text","text":"one"}]}),deadline())).await.expect("native ACK reply must not wait for unreleased readback").unwrap();
    assert!(matches!(result, Outcome::Accepted(_)));
    assert_eq!(
        runtime.facts()[0]["data"]["agent"]["config"][0]["pending"],
        true
    );
    assert_eq!(
        runtime.facts()[0]["data"]["agent"]["session"]["model"]["id"],
        "fake-model"
    );
    std::fs::write(root.path().join("model-release"), b"release").unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if runtime.facts()[0]["data"]["agent"]["config"][0]["pending"] == false {
                break;
            }
            changed.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    let rows = std::fs::read_to_string(root.path().join("ledger.jsonl")).unwrap();
    assert_eq!(
        rows.lines()
            .filter(|line| serde_json::from_str::<Value>(line).unwrap()["method"] == "thread/read")
            .count(),
        1
    );
    cleanup(child, peer).await;
    drain.await.unwrap();
}

#[tokio::test]
async fn lost_turn_ack_never_infers_model_or_replays_input_after_disconnect() {
    use vqro::provider_client::Outcome;
    let (root, child, peer, incoming) = fixture(true).await;
    let (runtime, drain) = open_runtime(peer.clone(), incoming, 1).await;
    runtime.initialize_models(deadline()).await.unwrap();
    runtime
        .invoke(
            "endpoint-1",
            "inc",
            "agent.session",
            json!({"v":0,"action":"new"}),
            deadline(),
        )
        .await
        .unwrap();
    let mut changed = runtime.changes();
    let result = runtime
        .invoke(
            "endpoint-1",
            "inc",
            "agent.configure",
            json!({"v":0,"id":"model","value":"fake-alternate"}),
            tokio::time::Instant::now() + Duration::from_millis(80),
        )
        .await
        .unwrap();
    assert!(matches!(result, Outcome::Accepted(_)));
    let sent=runtime.invoke("endpoint-1","inc","agent.send",json!({"v":0,"mode":"admit","delivery":"not-replayed","origin":{"principal":"p","actor":"actor","kind":"agent"},"content":[{"type":"text","text":"one"}]}),tokio::time::Instant::now()+Duration::from_millis(80)).await.unwrap();
    assert!(matches!(sent, Outcome::Dispatched(_)));
    let pending_rev = runtime.facts()[0]["data"]["agent"]["config"][0]["rev"]
        .as_u64()
        .unwrap();
    peer.close();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if runtime.facts()[0]["data"]["agent"]["config"][0]["rev"]
                .as_u64()
                .unwrap()
                > pending_rev
            {
                break;
            }
            changed.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    assert!(
        runtime.facts()[0]["data"]["agent"]["session"]
            .get("model")
            .is_none()
    );
    let result=runtime.invoke("endpoint-1","inc","agent.send",json!({"v":0,"mode":"admit","delivery":"not-replayed","origin":{"principal":"p","actor":"actor","kind":"agent"},"content":[{"type":"text","text":"must not send"}]}),deadline()).await.unwrap();
    assert!(matches!(result, Outcome::Rejected { .. }));
    assert_eq!(
        runtime.facts()[0]["data"]["agent"]["config"][0]["pending"],
        true
    );
    assert_eq!(
        runtime.facts()[0]["data"]["agent"]["config"][0]["status"],
        "unknown"
    );
    let rows: Vec<Value> = std::fs::read_to_string(root.path().join("ledger.jsonl"))
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
    assert!(
        rows.iter()
            .all(|row| row["method"] != "thread/read" && row["method"] != "thread/resume")
    );
    cleanup(child, peer).await;
    drain.await.unwrap();
}

#[tokio::test]
async fn generic_causality_is_checked_before_native_write_and_echoed_without_upgrade() {
    use vqro::provider_client::Outcome;
    let (root, child, peer, incoming) = fixture(false).await;
    let (runtime, drain) = open_runtime(peer.clone(), incoming, 1).await;
    runtime
        .invoke(
            "endpoint-1",
            "inc",
            "agent.session",
            json!({"v":0,"action":"new"}),
            deadline(),
        )
        .await
        .unwrap();
    let ledger = root.path().join("ledger.jsonl");
    let before = std::fs::read_to_string(&ledger).unwrap();
    let mut args = json!({"v":0,"mode":"admit","delivery":"causal-delivery","origin":{"principal":"p:test","actor":"actor:test","kind":"tool"},
        "content":[{"type":"text","text":"literal input"}],"correlation":"c".repeat(256)});
    for cause in [
        json!(null),
        json!({"kind":"effect","id":"id","provenance":"native"}),
        json!({"kind":"future","id":"id","provenance":"future","extra":"x".repeat(4096)}),
    ] {
        args["cause"] = cause;
        let result = runtime
            .invoke("endpoint-1", "inc", "agent.send", args.clone(), deadline())
            .await
            .unwrap();
        assert!(matches!(result, Outcome::Rejected { ref kind, .. } if kind == "invalid"));
        assert_eq!(
            std::fs::read_to_string(&ledger).unwrap(),
            before,
            "invalid cause must not reach native IPC"
        );
    }
    let mut cause = json!({"kind":"future","id":"id","provenance":"native","extra":""});
    let overhead = vqro::limits::encoded_bytes(&cause);
    cause["extra"] = json!("x".repeat(4096 - overhead));
    assert_eq!(vqro::limits::encoded_bytes(&cause), 4096);
    args["cause"] = cause.clone();
    let accepted = runtime
        .invoke("endpoint-1", "inc", "agent.send", args.clone(), deadline())
        .await
        .unwrap();
    assert!(matches!(accepted, Outcome::Accepted(_)));
    let receipt = runtime
        .invoke(
            "endpoint-1",
            "inc",
            "agent.receipt",
            json!({"v":0,"delivery":"causal-delivery"}),
            deadline(),
        )
        .await
        .unwrap();
    let Outcome::Observed(Some(receipt)) = receipt else {
        panic!("expected actual receipt");
    };
    assert_eq!(
        receipt["cause"], cause,
        "caller provenance is echoed, not promoted or rewritten"
    );
    assert_eq!(receipt["correlation"], args["correlation"]);
    let requests: Vec<Value> = std::fs::read_to_string(&ledger)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(
        requests
            .iter()
            .filter(|row| row["method"] == "turn/start")
            .count(),
        1
    );
    cleanup(child, peer).await;
    drain.await.unwrap();
}

#[tokio::test]
async fn two_concurrent_valid_answers_write_exactly_one_native_response() {
    use vqro::provider_client::Outcome;
    let (root, child, peer, incoming) = fixture(false).await;
    let (runtime, drain) = open_runtime(peer.clone(), incoming, 1).await;
    runtime
        .invoke(
            "endpoint-1",
            "inc",
            "agent.session",
            json!({"v":0,"action":"new"}),
            deadline(),
        )
        .await
        .unwrap();
    let session = runtime.facts()[0]["data"]["agent"]["session"]["id"].clone();
    runtime.incoming(Incoming::Request{id:json!(700),method:"item/commandExecution/requestApproval".into(),params:json!({"threadId":session,"turnId":"turn-question","itemId":"tool-question","command":"printf ok"})});
    let facts = runtime.facts();
    let agent = &facts[0]["data"]["agent"];
    let args = json!({"v":0,"epoch":agent["epoch"],"request":agent["requests"][0]["request"],"revision":1,"response":{"choice":"accept"}});
    let (a, b) = tokio::join!(
        runtime.invoke(
            "endpoint-1",
            "inc",
            "agent.respond",
            args.clone(),
            deadline()
        ),
        runtime.invoke("endpoint-1", "inc", "agent.respond", args, deadline())
    );
    let outcomes = [a.unwrap(), b.unwrap()];
    assert_eq!(
        outcomes
            .iter()
            .filter(|o| matches!(o, Outcome::Dispatched(_)))
            .count(),
        1
    );
    assert_eq!(
        outcomes
            .iter()
            .filter(|o| matches!(o,Outcome::Rejected{kind,..} if kind=="stale"))
            .count(),
        1
    );
    // A following native RPC response is a barrier after the flushed answer, not an answer ACK.
    peer.call("thread/list", json!({"limit":64})).await.unwrap();
    let ledger = std::fs::read_to_string(root.path().join("ledger.jsonl")).unwrap();
    assert_eq!(
        ledger
            .lines()
            .filter(|l| {
                let v: Value = serde_json::from_str(l).unwrap();
                v["id"] == 700 && v.get("method").is_none()
            })
            .count(),
        1
    );
    cleanup(child, peer).await;
    drain.await.unwrap();
}
