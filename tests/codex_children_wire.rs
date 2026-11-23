//! Native Peer harness, no subprocess, runtime login, or model requests.
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use vqro::codex::{Change, runtime::Runtime};
use vqro::rpc::{Incoming, Peer};

type Records = Arc<Mutex<Vec<(usize, String, Change)>>>;
async fn harness() -> (
    Arc<Runtime>,
    Peer,
    Records,
    Arc<Mutex<Vec<Value>>>,
    tokio::task::JoinHandle<()>,
) {
    let (client, server) = tokio::io::duplex(128 * 1024);
    let (reader, writer) = tokio::io::split(client);
    let (peer, _incoming) = Peer::spawn(reader, writer);
    let records: Records = Arc::new(Mutex::new(Vec::new()));
    let observed = records.clone();
    let ledger = Arc::new(Mutex::new(Vec::new()));
    let log = ledger.clone();
    let server = tokio::spawn(async move {
        let (reader, mut writer) = tokio::io::split(server);
        let mut lines = BufReader::new(reader).lines();
        let mut threads = 0;
        while let Some(line) = lines.next_line().await.unwrap() {
            let call: Value = serde_json::from_str(&line).unwrap();
            log.lock().unwrap().push(call.clone());
            if call.get("method").is_none() {
                continue;
            } // a response to a native question
            let result = match call["method"].as_str().unwrap() {
                "thread/start" => {
                    threads += 1;
                    json!({"thread":{"id":format!("main-{threads}"),"status":{"type":"idle"}}})
                }
                "thread/list" => json!({"data":[],"nextCursor":null}),
                _ => json!({}),
            };
            if call.get("id").is_some() {
                writer
                    .write_all(format!("{}\n", json!({"id":call["id"],"result":result})).as_bytes())
                    .await
                    .unwrap();
                writer.flush().await.unwrap();
            }
        }
    });
    let runtime = Runtime::new(
        peer.clone(),
        2,
        "inc".into(),
        Some(Arc::new(move |index, epoch, changes, _| {
            observed.lock().unwrap().extend(
                changes
                    .into_iter()
                    .map(|change| (index, epoch.into(), change)),
            );
        })),
    );
    for index in 1..=2 {
        runtime
            .invoke(
                &format!("endpoint-{index}"),
                "inc",
                "agent.session",
                json!({"v":0,"action":"new"}),
                tokio::time::Instant::now() + Duration::from_secs(2),
            )
            .await
            .unwrap();
    }
    records.lock().unwrap().clear();
    (runtime, peer, records, ledger, server)
}
fn notification(runtime: &Runtime, method: &str, params: Value) {
    runtime.incoming(Incoming::Notification {
        method: method.into(),
        params,
    });
}

#[tokio::test]
async fn unbound_child_is_observed_only_in_proven_parent_stream_without_native_control() {
    let (runtime, peer, records, ledger, server) = harness().await;
    notification(
        &runtime,
        "thread/started",
        json!({"thread":{"id":"child","parentThreadId":"main-1","status":{"type":"active"}}}),
    );
    notification(
        &runtime,
        "item/started",
        json!({"threadId":"child","item":{"id":"m","type":"agentMessage","text":"child text"}}),
    );
    notification(
        &runtime,
        "thread/started",
        json!({"thread":{"id":"foreign","parentThreadId":"unbound","status":{"type":"active"}}}),
    );
    notification(
        &runtime,
        "item/started",
        json!({"threadId":"foreign","item":{"id":"m","type":"agentMessage","text":"not ours"}}),
    );
    let rows = records.lock().unwrap();
    assert!(rows.iter().all(|(index, _, _)| *index == 0));
    let child = rows
        .iter()
        .find(|(_, _, c)| c.kind == "child")
        .unwrap()
        .2
        .id
        .clone();
    let message = rows.iter().find(|(_, _, c)| c.kind == "message").unwrap();
    assert_eq!(message.2.value["child"], child);
    assert_eq!(message.2.value["content"][0]["text"], "child text");
    assert_eq!(
        rows.iter().filter(|(_, _, c)| c.kind == "message").count(),
        1
    );
    drop(rows);
    assert!(ledger.lock().unwrap().iter().all(|call| !matches!(
        call["method"].as_str(),
        Some("turn/start" | "turn/interrupt" | "thread/resume" | "thread/queue/list")
    )));
    peer.close();
    server.abort();
}

#[tokio::test]
async fn child_requests_are_parent_scoped_not_cancelled_as_unbound() {
    let (runtime, peer, records, _ledger, server) = harness().await;
    notification(
        &runtime,
        "thread/started",
        json!({"thread":{"id":"child","parentThreadId":"main-2","status":{"type":"active"}}}),
    );
    runtime.incoming(Incoming::Request {
        id: json!(77),
        method: "item/commandExecution/requestApproval".into(),
        params: json!({"threadId":"child","turnId":"ct","itemId":"cmd","command":"echo hello"}),
    });
    let rows = records.lock().unwrap();
    let request = rows.iter().find(|(_, _, c)| c.kind == "request").unwrap();
    assert_eq!(request.0, 1);
    assert!(request.2.value["child"].is_string());
    assert_eq!(request.2.value["params"]["threadId"], "child");
    assert_eq!(request.2.value["status"], "pending");
    drop(rows);
    peer.close();
    server.abort();
}

#[tokio::test]
async fn session_list_explicitly_includes_subagent_sources_without_experimental_filters() {
    let (runtime, peer, _records, ledger, server) = harness().await;
    runtime
        .invoke(
            "endpoint-1",
            "inc",
            "agent.session",
            json!({"v":0,"action":"list"}),
            tokio::time::Instant::now() + Duration::from_secs(2),
        )
        .await
        .unwrap();
    let rows = ledger.lock().unwrap();
    let call = rows.iter().find(|c| c["method"] == "thread/list").unwrap();
    assert!(
        call["params"]["sourceKinds"]
            .as_array()
            .unwrap()
            .iter()
            .any(|kind| kind == "subAgentThreadSpawn")
    );
    assert!(call["params"].get("parentThreadId").is_none());
    assert!(call["params"].get("ancestorThreadId").is_none());
    drop(rows);
    peer.close();
    server.abort();
}
