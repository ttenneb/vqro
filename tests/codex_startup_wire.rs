//! Actual JSONL Peer bytes, synthetic in-memory server only. No installed runtime/model calls.
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;
use vqro::codex::{Change, runtime::Runtime};
use vqro::provider_client::Outcome;
use vqro::rpc::Peer;

type Records = Arc<Mutex<Vec<(usize, String, Change)>>>;
struct Harness {
    runtime: Arc<Runtime>,
    peer: Peer,
    release: mpsc::Sender<()>,
    acknowledge: mpsc::Sender<()>,
    ledger: Arc<Mutex<Vec<Value>>>,
    records: Records,
    server: tokio::task::JoinHandle<()>,
    drain: tokio::task::JoinHandle<()>,
}
fn skills(ordinal: usize) -> Value {
    json!({"data":[{"cwd":"/native","errors":[],"skills":[{"name":format!("native-skill-{ordinal}"),"enabled":true,"path":"/native/SKILL.md"}]}]})
}
async fn reply<W: AsyncWriteExt + Unpin>(writer: &mut W, id: Value, result: Value) {
    writer
        .write_all(format!("{}\n", json!({"jsonrpc":"2.0","id":id,"result":result})).as_bytes())
        .await
        .unwrap();
    writer.flush().await.unwrap();
}
async fn harness(hold: bool, pages: usize, plugin_error: bool) -> Harness {
    harness_modes(hold, pages, plugin_error, false, false).await
}
async fn harness_modes(
    hold: bool,
    pages: usize,
    plugin_error: bool,
    hold_hooks: bool,
    hold_ack: bool,
) -> Harness {
    let (client, server) = tokio::io::duplex(128 * 1024);
    let (reader, writer) = tokio::io::split(client);
    let (peer, incoming) = Peer::spawn(reader, writer);
    let ledger = Arc::new(Mutex::new(Vec::new()));
    let log = ledger.clone();
    let (release, mut released) = mpsc::channel::<()>(1);
    let (acknowledge, mut acknowledged) = mpsc::channel::<()>(1);
    let server = tokio::spawn(async move {
        let (reader, mut writer) = tokio::io::split(server);
        let mut lines = BufReader::new(reader).lines();
        let mut held = Vec::new();
        let mut held_ack = Vec::new();
        let mut threads = 0;
        let mut catalogs = 0;
        loop {
            tokio::select! {
                Some(()) = released.recv() => {
                    for (id, result) in held.drain(..) { reply(&mut writer, id, result).await; }
                }
                Some(()) = acknowledged.recv() => {
                    for (id, result) in held_ack.drain(..) { reply(&mut writer, id, result).await; }
                }
                line = lines.next_line() => {
                    let Some(line) = line.unwrap() else { break; };
                    let request: Value = serde_json::from_str(&line).unwrap();
                    log.lock().unwrap().push(request.clone());
                    let id = request["id"].clone();
                    match request["method"].as_str().unwrap() {
                        "initialized" => {},
                        "initialize" => reply(&mut writer, id, json!({"userAgent":"native-opaque/fixture"})).await,
                        "thread/start" | "thread/resume" => {
                            let resume = request["method"] == "thread/resume";
                            threads += 1;
                            reply(&mut writer, id, json!({"thread":{"id":"native-thread","status":{"type":"idle"}},"model":if resume {"native-resume"} else {"native-start"},"cwd":"/native","instructionSources":["/native/AGENTS.md"]})).await;
                        }
                        "hooks/list" => {
                            assert_eq!(request["params"], json!({"threadId":"native-thread"}));
                            let rows = if hold_hooks { json!([{"key":"native-hook-key","eventName":"userPromptSubmit","source":"user","sourcePath":"/native/hooks.json","enabled":true,"trustStatus":"trusted","currentHash":"hash","isManaged":false,"displayOrder":0,"timeoutSec":5,"handlerType":"command","command":"private command"}]) } else { json!([]) };
                            let result=json!({"data":[{"cwd":"/native","hooks":rows,"warnings":[],"errors":[]}]});
                            if hold_hooks { held.push((id,result)); } else { reply(&mut writer,id,result).await; }
                        }
                        "skills/list" => {
                            catalogs += 1;
                            assert_eq!(request["params"]["cwds"], json!(["/native"]));
                            assert_eq!(request["params"]["forceReload"], false);
                            if hold { held.push((id, skills(catalogs))); } else { reply(&mut writer, id, skills(catalogs)).await; }
                        }
                        "plugin/installed" => {
                            assert_eq!(request["params"], json!({}));
                            if plugin_error {
                                writer.write_all(format!("{}\n", json!({"id":id,"error":{"code":-32601,"message":"sensitive native diagnostic"}})).as_bytes()).await.unwrap();
                            } else {
                                reply(&mut writer, id, json!({"marketplaces":[{"plugins":[{"name":"installed","installed":true,"enabled":false,"localVersion":"1","version":"99"}]}]})).await;
                            }
                        }
                        "mcpServerStatus/list" => {
                            assert_eq!(request["params"]["threadId"], "native-thread");
                            assert_eq!(request["params"]["limit"], 64);
                            let page = request["params"]["cursor"].as_str().and_then(|s|s.parse::<usize>().ok()).unwrap_or(0);
                            let next = if page + 1 < pages { json!((page+1).to_string()) } else { Value::Null };
                            reply(&mut writer, id, json!({"data":[{"name":format!("mcp-{page}"),"runtimeStatus":"connected","tools":{"private":"not a startup field"}}],"nextCursor":next})).await;
                        }
                        "turn/start" => {
                            let result=json!({"turn":{"id":"native-turn","status":"inProgress"}});
                            if hold_ack { held_ack.push((id,result)); } else { reply(&mut writer,id,result).await; }
                        }
                        other => panic!("unexpected native mutation/query: {other}"),
                    }
                }
            }
        }
        assert!(threads > 0);
    });
    let records: Records = Arc::new(Mutex::new(Vec::new()));
    let observed = records.clone();
    let runtime = Runtime::new(
        peer.clone(),
        1,
        "inc".into(),
        Some(Arc::new(move |index, epoch, changes, _| {
            observed
                .lock()
                .unwrap()
                .extend(changes.into_iter().map(|c| (index, epoch.into(), c)));
        })),
    );
    let reader = runtime.clone();
    let drain = tokio::spawn(async move {
        reader.drain_native(incoming).await;
    });
    let init = peer
        .call(
            "initialize",
            json!({"clientInfo":{"name":"private-test","version":"0"}}),
        )
        .await
        .unwrap();
    peer.notify("initialized", json!({}));
    runtime.initialize_startup(&init);
    Harness {
        runtime,
        peer,
        release,
        acknowledge,
        ledger,
        records,
        server,
        drain,
    }
}
impl Harness {
    fn latest(&self) -> Value {
        self.records
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|(_, _, c)| c.kind == "startup")
            .unwrap()
            .2
            .value
            .clone()
    }
    fn receipt(&self, delivery: &str) -> Value {
        self.records
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|(_, _, c)| c.kind == "receipt" && c.id == delivery)
            .map(|(_, _, c)| c.value.clone())
            .unwrap_or(Value::Null)
    }
    fn count(&self, method: &str) -> usize {
        self.ledger
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r["method"] == method)
            .count()
    }
    async fn session(&self, resume: bool) {
        let args = if resume {
            json!({"v":0,"action":"resume","session":"native-thread"})
        } else {
            json!({"v":0,"action":"new"})
        };
        let result = tokio::time::timeout(
            Duration::from_secs(1),
            self.runtime.invoke(
                "endpoint-1",
                "inc",
                "agent.session",
                args,
                tokio::time::Instant::now() + Duration::from_secs(5),
            ),
        )
        .await
        .expect("native catalogs must not delay session readiness")
        .unwrap();
        assert!(matches!(result, Outcome::Observed(_)));
    }
    async fn close(self) {
        self.peer.close();
        tokio::time::timeout(Duration::from_secs(3), self.server)
            .await
            .unwrap()
            .unwrap();
        tokio::time::timeout(Duration::from_secs(3), self.drain)
            .await
            .unwrap()
            .unwrap();
    }
}
async fn until(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(3), async {
        while !condition() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn held_hooks_do_not_delay_input_and_late_inventory_cannot_backdate_ack_labels() {
    let h = harness_modes(false, 1, false, true, true).await;
    h.session(false).await;
    until(|| h.count("hooks/list") == 1).await;
    h.runtime.take_dirty_facts();
    let result=h.runtime.invoke("endpoint-1","inc","agent.send",json!({"v":0,"mode":"admit","delivery":"before-hooks","origin":{"principal":"p","actor":"a","kind":"agent"},"content":[{"type":"text","text":"literal while hooks held"}]}),tokio::time::Instant::now()+Duration::from_millis(30)).await.unwrap();
    assert!(matches!(result, Outcome::Dispatched(_)));
    assert!(h.receipt("before-hooks").get("may_transform").is_none());
    h.release.send(()).await.unwrap();
    until(|| h.runtime.facts()[0]["data"]["agent"]["input_transform"]["state"] == "may").await;
    assert!(
        h.runtime
            .take_dirty_facts()
            .iter()
            .any(|f| f["data"]["agent"]["input_transform"]["state"] == "may"),
        "inventory-derived frame facts must be dirtied, not only item journal"
    );
    h.acknowledge.send(()).await.unwrap();
    until(|| h.receipt("before-hooks")["stage"] == "admitted").await;
    assert!(h.receipt("before-hooks").get("may_transform").is_none());
    assert_eq!(h.count("turn/start"), 1);
    assert_eq!(h.count("hooks/list"), 1);
    // The remaining optional catalogs can finish after ACK; drain them before closing the test Peer.
    until(|| h.latest().get("mcp").is_some()).await;
    h.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_may_labels_are_internal_and_preserved_in_delayed_ack_and_lookup() {
    let h = harness_modes(false, 1, false, true, true).await;
    h.session(false).await;
    until(|| h.count("hooks/list") == 1).await;
    h.release.send(()).await.unwrap();
    until(|| h.runtime.facts()[0]["data"]["agent"]["input_transform"]["state"] == "may").await;
    let result=h.runtime.invoke("endpoint-1","inc","agent.send",json!({"v":0,"mode":"admit","delivery":"after-hooks","may_transform":["forged"],"origin":{"principal":"p","actor":"a","kind":"agent"},"content":[{"type":"text","text":"literal with known possible hooks"}]}),tokio::time::Instant::now()+Duration::from_millis(30)).await.unwrap();
    assert!(matches!(result, Outcome::Dispatched(_)));
    assert_eq!(h.receipt("after-hooks")["may_transform"], "unknown");
    h.acknowledge.send(()).await.unwrap();
    until(|| h.receipt("after-hooks")["stage"] == "admitted").await;
    let lookup = h
        .runtime
        .invoke(
            "endpoint-1",
            "inc",
            "agent.receipt",
            json!({"v":0,"delivery":"after-hooks"}),
            tokio::time::Instant::now() + Duration::from_secs(1),
        )
        .await
        .unwrap();
    assert!(matches!(lookup,Outcome::Observed(Some(value)) if value["may_transform"]=="unknown"));
    assert!(!h.receipt("after-hooks").to_string().contains("forged"));
    assert_eq!(h.count("turn/start"), 1);
    until(|| h.latest().get("mcp").is_some()).await;
    h.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn held_catalog_never_delays_session_or_input_and_late_fields_update_in_place() {
    let h = harness(true, 1, false).await;
    h.session(false).await;
    until(|| h.count("skills/list") == 1).await;
    assert_eq!(h.latest()["model"], "native-start");
    assert!(h.latest().get("skills").is_none());
    let outcome = tokio::time::timeout(Duration::from_secs(1), h.runtime.invoke("endpoint-1", "inc", "agent.send", json!({"v":0,"mode":"admit","delivery":"one","origin":{"principal":"p","actor":"a","kind":"agent"},"content":[{"type":"text","text":"one synthetic input"}]}), tokio::time::Instant::now()+Duration::from_secs(5))).await.expect("held catalogs must not occupy native send gate").unwrap();
    assert!(!matches!(outcome, Outcome::Rejected { .. }));
    assert_eq!(h.count("turn/start"), 1);
    h.release.send(()).await.unwrap();
    until(|| h.latest().get("mcp").is_some()).await;
    let s = h.latest();
    assert_eq!(s["runtime"]["name"], "native-opaque/fixture");
    assert_eq!(s["skills_scope"], "available");
    assert_eq!(
        s["plugins"],
        json!([{"name":"installed","enabled":false,"version":"1"}])
    );
    assert_eq!(s["mcp"], json!([{"name":"mcp-0","status":"connected"}]));
    assert_eq!(h.count("initialize"), 1);
    assert_eq!(h.count("thread/start"), 1);
    assert_eq!(h.count("plugin/installed"), 1);
    assert_eq!(h.count("mcpServerStatus/list"), 1);
    assert!(!s.to_string().contains("not a startup field"));
    h.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn catalog_error_is_absent_and_native_pagination_stops_at_four_pages() {
    let h = harness(false, 8, true).await;
    h.session(false).await;
    until(|| h.latest().get("mcp").is_some()).await;
    let s = h.latest();
    assert!(s.get("plugins").is_none());
    let incomplete = s["incomplete"].as_array().unwrap();
    assert!(incomplete.contains(&json!("plugins")) && incomplete.contains(&json!("mcp")));
    assert_eq!(s["mcp"].as_array().unwrap().len(), 4);
    assert_eq!(h.count("mcpServerStatus/list"), 4);
    assert!(!s.to_string().contains("sensitive native diagnostic"));
    assert_eq!(h.count("turn/start"), 0);
    h.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn late_catalog_cannot_cross_an_epoch_even_when_resuming_the_same_native_thread() {
    let h = harness(true, 1, false).await;
    h.session(false).await;
    until(|| h.count("skills/list") == 1).await;
    h.session(true).await;
    until(|| h.count("skills/list") == 2).await;
    assert_eq!(h.latest()["model"], "native-resume");
    h.release.send(()).await.unwrap();
    until(|| h.latest().get("mcp").is_some()).await;
    assert_eq!(h.latest()["skills"][0]["name"], "native-skill-2");
    assert_eq!(h.latest()["model"], "native-resume");
    assert_eq!(
        h.count("plugin/installed"),
        1,
        "stale collector stops before any further native query"
    );
    assert_eq!(h.count("turn/start"), 0);
    h.close().await;
}
