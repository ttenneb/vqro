//! Reuse vqro's bounded symmetric RPC peer against a subprocess speaking native app-server JSONL.
use serde_json::{Value, json};
use std::process::Stdio;
use std::time::Duration;
use tokio::process::{Child, Command};
use vqro::rpc::{Incoming, IncomingReceiver, Peer};

async fn fixture(hold: bool) -> (tempfile::TempDir, Child, Peer, IncomingReceiver) {
    let root = tempfile::tempdir().unwrap();
    let mut child = Command::new("python3")
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", root.path())
        .env("CODEX_FAKE_HOLD", if hold { "1" } else { "0" })
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
