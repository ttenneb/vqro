//! Synthetic native process + in-memory core only. No Claude executable/model/auth/profile access.
#![cfg(unix)]
use base64::Engine;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, ReadHalf, WriteHalf};
use tokio::sync::mpsc::UnboundedReceiver;
use vqro::claude_code::runtime::Shared;
use vqro::provider_client::{Config, Provider, Request};
struct Core {
    r: BufReader<ReadHalf<DuplexStream>>,
    w: WriteHalf<DuplexStream>,
    serial: u64,
    items: Vec<u8>,
}
impl Core {
    async fn recv(&mut self) -> Value {
        let mut line = String::new();
        let n = tokio::time::timeout(Duration::from_secs(5), self.r.read_line(&mut line))
            .await
            .expect("fake core reply deadline")
            .unwrap();
        assert!(n > 0, "bridge closed fake core connection");
        serde_json::from_str(&line).unwrap()
    }
    async fn send(&mut self, value: Value) {
        self.w
            .write_all(format!("{value}\n").as_bytes())
            .await
            .unwrap();
    }
    async fn reply(&mut self, request: &Value, result: Value) {
        self.send(json!({"jsonrpc":"2.0","id":request["id"],"result":result}))
            .await;
    }
    async fn item_data(&mut self, frame: &Value) {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(frame["params"]["data"].as_str().unwrap())
            .unwrap();
        self.items.extend_from_slice(&bytes);
        self.send(json!({"jsonrpc":"2.0","method":"stream.credit","params":{"generation":7,"stream":44,"bytes":bytes.len()}})).await;
    }
    async fn open_items(
        &mut self,
        shared: &Arc<Shared>,
        provider: &Provider,
        requests: &mut UnboundedReceiver<Request>,
    ) {
        self.serial += 1;
        let id = self.serial;
        let incarnation = shared.state.lock().unwrap().incarnation().to_owned();
        self.send(json!({"jsonrpc":"2.0","id":id,"method":"stream.open","params":{"generation":7,"stream":44,"ref":"agent","incarnation":incarnation,"channel":"items","window_bytes":262144}})).await;
        let request = requests.recv().await.unwrap();
        shared.serve_request(provider, request).await;
        let response = self.recv().await;
        assert_eq!(response["id"], id);
        assert!(response.get("result").is_some(), "{response}");
    }
    async fn invoke(
        &mut self,
        shared: &Arc<Shared>,
        provider: &Provider,
        requests: &mut UnboundedReceiver<Request>,
        op: &str,
        args: Value,
    ) -> Value {
        self.serial += 1;
        let id = self.serial;
        let incarnation = shared.state.lock().unwrap().incarnation().to_owned();
        self.send(json!({"jsonrpc":"2.0","id":id,"method":"effect.invoke","params":{"generation":7,
            "effect":format!("effect-{id}"),"op":op,"ref":"agent","incarnation":incarnation,"args":args,"deadline_ms":5000,
            "author":"fixture-client","actor":"client:fixture","principal":"owner"}})).await;
        let request = tokio::time::timeout(Duration::from_secs(5), requests.recv())
            .await
            .unwrap()
            .expect("effect request");
        shared.serve_request(provider, request).await;
        loop {
            let frame = self.recv().await;
            if frame["method"] == "workspace.get" {
                assert_eq!(frame["params"]["key"], "doc/principals/owner");
                self.reply(&frame, json!({"value":"owner"})).await;
            } else if frame["method"] == "stream.data" {
                self.item_data(&frame).await;
            } else if frame["id"] == id {
                return frame;
            } else {
                panic!("unexpected fake core frame: {frame}");
            }
        }
    }
}
async fn registered(shared: &Arc<Shared>) -> (Provider, UnboundedReceiver<Request>, Core) {
    registered_with_capacity(shared, 1 << 20).await
}
async fn registered_with_capacity(
    shared: &Arc<Shared>,
    capacity: usize,
) -> (Provider, UnboundedReceiver<Request>, Core) {
    let (a, b) = tokio::io::duplex(capacity);
    let (r, w) = tokio::io::split(b);
    let mut core = Core {
        r: BufReader::new(r),
        w,
        serial: 100,
        items: Vec::new(),
    };
    let mut config = Config::new(
        "claude-fixture",
        "fixture-instance",
        shared.state.lock().unwrap().provider_caps(),
    );
    config.role = "runtime".into();
    config.call_timeout = Duration::from_secs(5);
    let connect = tokio::spawn(Provider::connect(a, config));
    let hello = core.recv().await;
    assert_eq!(hello["method"], "hello");
    core.reply(&hello, json!({"server":"synthetic-core"})).await;
    let register = core.recv().await;
    assert_eq!(register["method"], "provider.register");
    assert_eq!(register["params"]["role"], "runtime");
    core.reply(&register,json!({"provider":"claude-fixture","generation":7,"limits":{"stream_window_bytes":262144,"max_record_bytes":1048576}})).await;
    let (p, rx) = connect.await.unwrap().unwrap();
    let sync_provider = p.clone();
    let facts = shared.facts();
    let sync = tokio::spawn(async move {
        sync_provider.full_sync(vec![facts]).await.unwrap();
    });
    let request = core.recv().await;
    assert_eq!(request["method"], "provider.sync");
    core.reply(&request, json!({})).await;
    sync.await.unwrap();
    (p, rx, core)
}
async fn until(
    shared: &Arc<Shared>,
    mut predicate: impl FnMut(&vqro::claude_code::state::State) -> bool,
) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let changed = shared.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if predicate(&shared.state.lock().unwrap()) {
                return;
            }
            changed.await;
        }
    })
    .await
    .expect("native fixture observation deadline");
}
fn send_args(shared: &Arc<Shared>, delivery: &str, blocks: Vec<Value>) -> Value {
    let epoch = shared.state.lock().unwrap().epoch().to_owned();
    json!({"v":0,"epoch":epoch,"mode":"admit","delivery":delivery,"origin":{"kind":"human"},"content":blocks})
}
fn inputs(path: &std::path::Path) -> Vec<Value> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter_map(|value| value.get("in").cloned())
        .collect()
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn synthetic_process_provider_vertical_slice() {
    let root = tempfile::tempdir().unwrap();
    let log = root.path().join("synthetic-wire.jsonl");
    struct Dump(std::path::PathBuf);
    impl Drop for Dump {
        fn drop(&mut self) {
            if std::thread::panicking() {
                eprintln!(
                    "synthetic wire evidence:\n{}",
                    std::fs::read_to_string(&self.0).unwrap_or_default()
                );
            }
        }
    }
    let _dump = Dump(log.clone());
    let shared = Shared::new(
        "fixture-boot".into(),
        "claude-fixture".into(),
        "fixture-instance".into(),
    );
    let env = HashMap::from([
        ("PATH".into(), std::env::var("PATH").unwrap()),
        ("HOME".into(), root.path().to_str().unwrap().into()),
        ("FAKE_CLAUDE_LOG".into(), log.to_str().unwrap().into()),
        (
            "VQRO_BRIDGE_TOKEN".into(),
            "synthetic-bootstrap-not-for-native".into(),
        ),
        ("HERDR_SOCKET".into(), "synthetic-not-a-socket".into()),
        ("CLAUDE_CODE_RESUME_INTERRUPTED_TURN".into(), "1".into()),
    ]);
    let argv = vec![
        "python3".into(),
        "-B".into(),
        format!(
            "{}/tests/fixtures/fake_claude_stream.py",
            env!("CARGO_MANIFEST_DIR")
        ),
    ];
    let native = shared.spawn_native(&argv, root.path(), &env).await.unwrap();
    let outgoing = tokio::spawn(shared.clone().drain_outgoing());
    shared.clone().initialize().await;
    match shared.spawn_native(&argv, root.path(), &env).await {
        Err(error) => assert_eq!(error.kind, "conflict"),
        Ok(extra) => {
            extra.stop();
            panic!("a second native child replaced the owned slot");
        }
    }
    until(&shared, |s| {
        s.ready && s.background.as_ref().is_some_and(Vec::is_empty)
    })
    .await;
    assert!(
        shared.state.lock().unwrap().session.is_none(),
        "initialize must not invent a native session"
    );
    assert!(
        inputs(&log).iter().all(|v| v["type"] == "control_request"),
        "startup must not write a prompt"
    );
    let (provider, mut requests, mut core) = registered(&shared).await;
    // Compose the real request incarnation token with the actual native writer, not only a
    // book-only assertion. A reused/withdrawn question must send ZERO old consent bytes.
    let mut book = vqro::claude_code::requests::Requests::default();
    let mut question = json!({"type":"control_request","request_id":"race-question","request":{"subtype":"can_use_tool","tool_name":"Bash","tool_use_id":"race-tool","input":{"command":"old"}}});
    let id = book.observe(&question, None).changes.remove(0).id;
    let old = book
        .answer(
            &id,
            1,
            &json!({"action":"accept","content":{"behavior":"allow"}}),
        )
        .unwrap();
    let token = book.answer_token(&id).unwrap();
    question["request"]["input"]["command"] = json!("replacement");
    book.observe(&question, None);
    let error = native
        .notify_guarded(
            "control_response",
            old,
            tokio::time::Instant::now() + Duration::from_secs(2),
            token,
        )
        .await
        .unwrap_err();
    assert!(!error.may_have_acted);
    let mut unsupported = question.clone();
    unsupported["request_id"] = json!("race-auto");
    unsupported["request"]["requires_user_interaction"] = json!(true);
    let old = book.observe(&unsupported, None).outgoing.remove(0);
    unsupported["request"]["requires_user_interaction"] = json!(false);
    book.observe(&unsupported, None);
    let error = native
        .notify_guarded(
            "control_response",
            old.frame,
            tokio::time::Instant::now() + Duration::from_secs(2),
            old.token,
        )
        .await
        .unwrap_err();
    assert!(!error.may_have_acted);
    native
        .call(
            "initialize",
            json!({"supportedDialogKinds":[]}),
            Duration::from_secs(2),
        )
        .await
        .unwrap();
    assert!(
        !inputs(&log).iter().any(|v| v["type"] == "control_response"),
        "unwritten old approval/auto-cancellation leaked to reused native ID"
    );
    let args = send_args(
        &shared,
        "delivery1",
        vec![
            json!({"type":"text","text":"/clear @private"}),
            json!({"type":"text","text":"second exact block"}),
        ],
    );
    let sent = core
        .invoke(
            &shared,
            &provider,
            &mut requests,
            "agent.send",
            args.clone(),
        )
        .await;
    assert_eq!(sent["result"]["outcome"], "dispatched");
    until(&shared, |s| {
        s.receipt("delivery1")["stage"] == "completed" && s.status() == "idle"
    })
    .await;
    let records = inputs(&log);
    let first = records.iter().find(|v| v["type"] == "user").unwrap();
    assert_eq!(
        first["message"]["content"], args["content"],
        "proved owner body stays separate/exact"
    );
    assert_eq!(first["client_composed"], true);
    assert_eq!(first["message"]["role"], "user");
    assert_eq!(
        shared.state.lock().unwrap().receipt("delivery1")["may_transform"],
        "unknown"
    );
    assert_eq!(shared.state.lock().unwrap().input_ledger()["pending"], 0);
    let before = inputs(&log).len();
    let duplicate = core
        .invoke(&shared, &provider, &mut requests, "agent.send", args)
        .await;
    assert_eq!(duplicate["result"]["outcome"], "rejected");
    assert_eq!(
        inputs(&log).len(),
        before,
        "duplicate delivery never writes again"
    );
    let lookup = core
        .invoke(
            &shared,
            &provider,
            &mut requests,
            "agent.receipt",
            json!({"v":0,"delivery":"delivery1"}),
        )
        .await;
    assert_eq!(lookup["result"]["data"]["stage"], "completed");
    assert_eq!(lookup["result"]["data"]["may_transform"], "unknown");
    core.open_items(&shared, &provider, &mut requests).await;

    let args = send_args(
        &shared,
        "permission",
        vec![json!({"type":"text","text":"[[permission]]"})],
    );
    assert_eq!(
        core.invoke(&shared, &provider, &mut requests, "agent.send", args)
            .await["result"]["outcome"],
        "dispatched"
    );
    until(&shared, |s| !s.requests.is_empty()).await;
    let (epoch, request) = {
        let state = shared.state.lock().unwrap();
        (
            state.epoch().to_owned(),
            state.requests.summaries()[0]["request"].clone(),
        )
    };
    let invalid = json!({"v":0,"epoch":epoch,"request":request,"revision":1,"response":{"action":"accept","content":{"behavior":"allow","updatedPermissions":[]}}});
    let before = inputs(&log).len();
    assert_eq!(
        core.invoke(&shared, &provider, &mut requests, "agent.respond", invalid)
            .await["result"]["outcome"],
        "rejected"
    );
    assert_eq!(
        inputs(&log).len(),
        before,
        "invalid form is a zero-write refusal"
    );
    let answer = json!({"v":0,"epoch":epoch,"request":request,"revision":1,"response":{"action":"accept","content":{"behavior":"allow"}}});
    assert_eq!(
        core.invoke(
            &shared,
            &provider,
            &mut requests,
            "agent.respond",
            answer.clone()
        )
        .await["result"]["outcome"],
        "dispatched"
    );
    until(&shared, |s| {
        s.receipt("permission")["stage"] == "completed"
            && s.requests.is_empty()
            && !s.requests.has_unconfirmed()
    })
    .await;
    let before = inputs(&log).len();
    assert_eq!(
        core.invoke(&shared, &provider, &mut requests, "agent.respond", answer)
            .await["result"]["outcome"],
        "rejected"
    );
    assert_eq!(inputs(&log).len(), before);

    let args = send_args(
        &shared,
        "unsupported-question",
        vec![json!({"type":"text","text":"[[unsupported_question]]"})],
    );
    assert_eq!(
        core.invoke(&shared, &provider, &mut requests, "agent.send", args)
            .await["result"]["outcome"],
        "dispatched"
    );
    until(&shared, |s| {
        s.receipt("unsupported-question")["stage"] == "completed"
    })
    .await;
    assert!(
        inputs(&log)
            .iter()
            .any(|v| v["type"] == "control_response" && v["response"]["subtype"] == "error"),
        "interaction-only native questions are cancelled, never auto-approved"
    );

    let args = send_args(
        &shared,
        "hang",
        vec![json!({"type":"text","text":"[[hang]]"})],
    );
    assert_eq!(
        core.invoke(&shared, &provider, &mut requests, "agent.send", args)
            .await["result"]["outcome"],
        "dispatched"
    );
    until(&shared, |s| s.status() == "running").await;
    let epoch = shared.state.lock().unwrap().epoch().to_owned();
    let stopped = core
        .invoke(
            &shared,
            &provider,
            &mut requests,
            "agent.interrupt",
            json!({"v":0,"epoch":epoch}),
        )
        .await;
    assert_eq!(stopped["result"]["outcome"], "accepted");
    assert_eq!(stopped["result"]["data"]["pending"], "unknown");
    until(&shared, |s| {
        s.receipt("hang")["stage"] == "completed" && s.status() == "idle"
    })
    .await;
    assert!(
        shared.facts()["data"]["agent"]["queue"].is_null(),
        "interrupt's partial still_queued[] is not canonical queue[]"
    );

    let args = send_args(
        &shared,
        "limit",
        vec![json!({"type":"text","text":"[[limit]]"})],
    );
    assert_eq!(
        core.invoke(&shared, &provider, &mut requests, "agent.send", args)
            .await["result"]["outcome"],
        "dispatched"
    );
    until(&shared, |s| {
        s.receipt("limit")["stage"] == "completed" && s.status() == "idle"
    })
    .await;
    let availability = shared.state.lock().unwrap().availability.value().unwrap();
    assert_eq!(availability["state"], "available");
    assert_eq!(availability["native"]["subscription_limit"], true);
    assert_eq!(
        shared.facts()["data"]["agent"]["_claude"]["input_ledger"]["pending"],
        0
    );

    let args = send_args(
        &shared,
        "tail",
        vec![json!({"type":"text","text":"[[exit_tail]]"})],
    );
    assert_eq!(
        core.invoke(&shared, &provider, &mut requests, "agent.send", args)
            .await["result"]["outcome"],
        "dispatched"
    );
    tokio::time::timeout(Duration::from_secs(5), native.exited())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), native.drained())
        .await
        .unwrap();
    assert_eq!(
        shared.state.lock().unwrap().receipt("tail")["stage"],
        "completed",
        "final stdout survives process-exit ordering"
    );
    assert!(shared.state.lock().unwrap().exit.is_some());
    let final_shared = shared.clone();
    let final_provider = provider.clone();
    let finish = tokio::spawn(async move {
        final_shared.finish(&final_provider).await.unwrap();
        final_provider.close();
    });
    let (mut closed, mut final_facts) = (false, false);
    while !closed || !final_facts {
        let frame = core.recv().await;
        match frame["method"].as_str() {
            Some("stream.data") => core.item_data(&frame).await,
            Some("stream.closed") => {
                assert_eq!(
                    frame["params"]["reason"]["reason"], "native_reader_closed",
                    "not a generic provider-drop marker"
                );
                closed = true;
            }
            Some("provider.sync") => {
                assert!(
                    closed,
                    "final publication acknowledgement preceded the item terminal cut"
                );
                assert_eq!(
                    frame["params"]["resources"][0]["data"]["agent"]["lifecycle"],
                    "ended"
                );
                assert!(
                    !finish.is_finished(),
                    "intentional disconnect happened before final ACK"
                );
                core.reply(&frame, json!({})).await;
                final_facts = true;
            }
            _ => panic!("unexpected final frame {frame}"),
        }
    }
    finish.await.unwrap();
    let rendered = String::from_utf8(core.items).unwrap();
    assert!(
        rendered.contains("first synthetic block") && rendered.contains("second synthetic block")
    );
    assert!(
        rendered.contains("tail"),
        "last receipt must precede stream close"
    );
    outgoing.abort();
    provider.close();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn normal_publisher_and_acknowledged_shutdown_preserve_blocked_writer_tail() {
    let shared = Shared::new(
        "publish-boot".into(),
        "claude-fixture".into(),
        "fixture-instance".into(),
    );
    shared
        .state
        .lock()
        .unwrap()
        .initialized(&json!({"claude_code_version":"2.1.329"}));
    // A one-byte transport blocks the SDK writer as soon as the test stops reading. The
    // actual shutdown task below must wait for the acknowledged cut, not merely enqueue.
    let (provider, mut requests, mut core) = registered_with_capacity(&shared, 1).await;
    core.open_items(&shared, &provider, &mut requests).await;
    let publisher = tokio::spawn(shared.clone().publish(provider.clone()));
    shared
        .state
        .lock()
        .unwrap()
        .observe(&vqro::native_process::Event::Exited(
            vqro::native_process::Exit {
                code: Some(0),
                success: true,
            },
        ));
    shared.wake();
    loop {
        let frame = core.recv().await;
        if frame["method"] == "stream.data" {
            core.item_data(&frame).await;
            continue;
        }
        assert_eq!(frame["method"], "resource.upsert");
        let lifecycle = &frame["params"]["facts"]["data"]["agent"]["lifecycle"];
        assert_ne!(
            lifecycle, "ended",
            "normal publisher retired the endpoint before the final-read barrier"
        );
        if lifecycle == "ending" {
            break;
        }
    }
    publisher.abort();
    let _ = publisher.await; // same retirement/join as the bridge's real exit path
    // Deterministic exit-before-final-read schedule: only now release final native records.
    {
        let mut state = shared.state.lock().unwrap();
        state.observe(&vqro::native_process::Event::Record(json!({"type":"assistant","uuid":"late-message","message":{"content":[{"type":"text","text":"late native tail"}]}})));
        state.observe(&vqro::native_process::Event::Record(json!({"type":"result","uuid":"late-result","terminal_reason":"completed","is_error":false})));
    }
    shared.wake();
    let final_shared = shared.clone();
    let final_provider = provider.clone();
    let finish = tokio::spawn(async move {
        final_shared.finish(&final_provider).await.unwrap();
        final_provider.close(); // actual production order: ACK cut first, disconnect second
    });
    let (mut closed, mut ended) = (false, false);
    while !closed || !ended {
        let frame = core.recv().await;
        match frame["method"].as_str() {
            Some("stream.data") => core.item_data(&frame).await,
            Some("stream.closed") => closed = true,
            Some("provider.sync") => {
                assert_eq!(
                    frame["params"]["resources"][0]["data"]["agent"]["lifecycle"],
                    "ended"
                );
                assert!(closed, "ended preceded the item terminal cut");
                assert!(
                    !finish.is_finished(),
                    "runtime returned/disconnected without final ACK"
                );
                core.reply(&frame, json!({})).await;
                ended = true;
            }
            _ => panic!("unexpected lifecycle frame {frame}"),
        }
    }
    finish.await.unwrap();
    assert!(
        String::from_utf8(core.items)
            .unwrap()
            .contains("late native tail")
    );
    provider.close();
}
