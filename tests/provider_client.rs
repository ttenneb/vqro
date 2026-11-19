//! Provider-side helper against an in-test fake core speaking provider-v0 section 9.

use std::time::Duration;

use base64::Engine;
use vqro::provider_client::{Config, Initial, Outcome, Provider, ProviderError, Request, StreamCommand};
use serde_json::{json, Map, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, ReadHalf, WriteHalf};
use tokio::sync::mpsc::UnboundedReceiver;

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

struct Core {
    r: BufReader<ReadHalf<DuplexStream>>,
    w: WriteHalf<DuplexStream>,
}

impl Core {
    async fn recv(&mut self) -> Value {
        let mut line = String::new();
        let n = tokio::time::timeout(Duration::from_secs(2), self.r.read_line(&mut line)).await.expect("message in time").unwrap();
        assert!(n > 0, "provider closed the connection");
        serde_json::from_str(&line).unwrap()
    }
    async fn nothing(&mut self) {
        let mut line = String::new();
        assert!(tokio::time::timeout(Duration::from_millis(150), self.r.read_line(&mut line)).await.is_err(), "unexpected: {line}");
    }
    async fn send(&mut self, v: Value) {
        self.w.write_all(format!("{v}\n").as_bytes()).await.unwrap();
    }
    async fn reply(&mut self, req: &Value, result: Value) {
        self.send(json!({"jsonrpc": "2.0", "id": req["id"], "result": result})).await;
    }
}

fn caps() -> Map<String, Value> {
    json!({"terminal.spawn": {}, "terminal.input": {}, "terminal.output": {"raw": true}, "terminal.resize": {}})
        .as_object()
        .unwrap()
        .clone()
}

/// A registered provider (generation 7, window 10) and the fake core's end.
async fn registered() -> (Provider, UnboundedReceiver<Request>, Core) {
    let (a, b) = tokio::io::duplex(1 << 20);
    let (r, w) = tokio::io::split(b);
    let mut core = Core { r: BufReader::new(r), w };
    let mut config = Config::new("tmux@test", "inst-1", caps());
    config.extension = Some("tmux".into());
    let connect = tokio::spawn(Provider::connect(a, config));
    let hello = core.recv().await;
    assert_eq!(hello["method"], "hello");
    assert_eq!(hello["params"]["extension"], "tmux");
    assert!(hello["params"].get("generation").is_none());
    core.reply(&hello, json!({"server": "vqro"})).await;
    let reg = core.recv().await;
    assert_eq!(reg["method"], "provider.register");
    assert_eq!(reg["params"]["name"], "tmux@test");
    assert_eq!(reg["params"]["instance"], "inst-1");
    assert_eq!(reg["params"]["role"], "backend");
    assert_eq!(reg["params"]["capabilities"]["terminal.output"], json!({"raw": true}));
    assert_eq!(reg["params"]["limits"]["stream_window_bytes"], 262144);
    core.reply(&reg, json!({"provider": "tmux@test", "generation": 7, "limits": {"stream_window_bytes": 10, "max_record_bytes": 1048576}})).await;
    let (p, rx) = connect.await.unwrap().unwrap();
    assert_eq!(p.generation(), 7);
    assert_eq!(p.window(), 10);
    assert_eq!(p.registration()["generation"], 7);
    assert_eq!(p.resume_token(), None);
    (p, rx, core)
}

fn facts(r: &str) -> Value {
    json!({"ref": r, "incarnation": "", "kind": "terminal", "state": "live", "argv": ["sh"]})
}

#[tokio::test]
async fn runtime_registration_retains_opaque_resume_metadata_through_later_setup_failure() {
    let (a, b) = tokio::io::duplex(1 << 20);
    let (r, w) = tokio::io::split(b);
    let mut core = Core { r: BufReader::new(r), w };
    let mut config = Config::new("runtime@test", "runtime-1", caps());
    config.role = "runtime".into();
    config.token = Some("bootstrap-opaque".into());
    config.extension = Some("runtime@test".into());
    let connect = tokio::spawn(Provider::connect(a, config));
    let hello = core.recv().await;
    core.reply(&hello, json!({"server":"vqro"})).await;
    let register = core.recv().await;
    assert_eq!(register["params"]["token"], "bootstrap-opaque");
    assert_eq!(register["params"]["role"], "runtime");
    let reply = json!({"provider":"runtime@test", "generation":9, "resume_token":"opaque:rotated credential",
        "future":{"version":7}, "limits":{"stream_window_bytes":42,"max_record_bytes":1048576}});
    core.reply(&register, reply.clone()).await;
    let (provider, _rx) = connect.await.unwrap().unwrap();
    assert_eq!(provider.registration(), &reply);
    assert_eq!(provider.resume_token(), Some("opaque:rotated credential"));
    assert_eq!(provider.clone().registration(), &reply);
    let p = provider.clone();
    let sync = tokio::spawn(async move { p.full_sync(vec![facts("agent")]).await });
    let request = core.recv().await;
    core.send(json!({"jsonrpc":"2.0", "id":request["id"], "error":{"code":-32000,"message":"temporary refusal",
        "data":{"kind":"forbidden","reason":"safe_mode","outcome":"rejected","future":{"n":3}}}})).await;
    let error = sync.await.unwrap().unwrap_err();
    assert!(error.remote);
    assert_eq!(error.data["reason"], "safe_mode");
    assert_eq!(error.data["future"]["n"], 3);
    assert_eq!(provider.resume_token(), Some("opaque:rotated credential"), "later setup failure must not lose observed rotation");
}

#[tokio::test]
async fn structured_remote_errors_differ_from_local_transport_failures() {
    let (provider, _rx, mut core) = registered().await;
    let p = provider.clone();
    let call = tokio::spawn(async move { p.call("probe", json!({})).await });
    let request = core.recv().await;
    let data = json!({"kind":"timeout", "outcome":"indeterminate", "reason":"native_deadline", "extra":[1,2]});
    core.send(json!({"jsonrpc":"2.0", "id":request["id"], "error":{"code":-32000,"message":"reported by core","data":data}})).await;
    let remote = call.await.unwrap().unwrap_err();
    assert_eq!(remote.kind, "timeout");
    assert!(remote.remote);
    assert_eq!(remote.data, data);
    provider.close();
    let local = provider.call("probe", json!({})).await.unwrap_err();
    assert_eq!(local.kind, "unavailable");
    assert!(!local.remote);
    assert_eq!(local.data, json!({}));
}

#[tokio::test]
async fn dispatched_is_a_single_truthful_outcome_and_error_data_roundtrips() {
    let (_provider, mut rx, mut core) = registered().await;
    core.send(json!({"jsonrpc":"2.0","id":1,"method":"effect.invoke","params":{"generation":7,
        "effect":"one","op":"terminal.spawn","args":{},"deadline_ms":1000}})).await;
    let Some(Request::Effect(effect)) = rx.recv().await else { panic!("effect"); };
    effect.respond(Ok(Outcome::Dispatched(Some(json!({"receipt":{"stage":"dispatched"}})))));
    let reply = core.recv().await;
    assert_eq!(reply["result"], json!({"outcome":"dispatched","data":{"receipt":{"stage":"dispatched"}}}));
    core.nothing().await;
    core.send(json!({"jsonrpc":"2.0","id":2,"method":"effect.invoke","params":{"generation":7,
        "effect":"two","op":"terminal.spawn","args":{},"deadline_ms":1000}})).await;
    let Some(Request::Effect(effect)) = rx.recv().await else { panic!("effect"); };
    effect.respond(Err(ProviderError::new("invalid", "bad input").with_data(json!({"reason":"fixture", "kind":"not-authoritative"}))));
    let reply = core.recv().await;
    assert_eq!(reply["error"]["data"], json!({"kind":"invalid", "reason":"fixture"}));
}

#[tokio::test]
async fn registers_then_syncs_before_upserts_with_generation() {
    let (p, _rx, mut core) = registered().await;
    assert_eq!(p.upsert(facts("%1")).await.unwrap_err().kind, "conflict", "upsert before the first sync");
    assert_eq!(p.sync(vec![json!({"ref": "%1"})], true).await.unwrap_err().kind, "invalid", "facts are validated");
    let p2 = p.clone();
    let sync = tokio::spawn(async move { p2.full_sync(vec![facts("%1"), facts("%2")]).await });
    let s = core.recv().await;
    assert_eq!(s["method"], "provider.sync");
    assert_eq!(s["params"]["generation"], 7);
    assert_eq!(s["params"]["final"], true);
    assert_eq!(s["params"]["resources"].as_array().unwrap().len(), 2);
    core.reply(&s, json!({})).await;
    sync.await.unwrap().unwrap();
    p.upsert(facts("%3")).await.unwrap();
    let u = core.recv().await;
    assert_eq!((u["method"].as_str(), u["params"]["generation"].as_u64(), u["params"]["facts"]["ref"].as_str()), (Some("resource.upsert"), Some(7), Some("%3")));
    assert!(u.get("id").is_none(), "upsert is a notification");
    p.gone("%3", "", json!({"reason": "exited"})).await.unwrap();
    let g = core.recv().await;
    assert_eq!(g["params"]["reason"]["reason"], "exited");
}

#[tokio::test]
async fn effects_reply_once_with_duplicate_unsupported_and_stale_checks() {
    let (_p, mut rx, mut core) = registered().await;
    let invoke = |id: u64, effect: &str, op: &str, generation: u64, deadline: u64| {
        json!({"jsonrpc": "2.0", "id": id, "method": "effect.invoke", "params": {"generation": generation, "effect": effect,
            "op": op, "args": {"argv": ["sh"]}, "deadline_ms": deadline}})
    };
    core.send(invoke(1, "e1", "terminal.spawn", 7, 5000)).await;
    let Some(Request::Effect(e)) = rx.recv().await else { panic!("effect") };
    assert_eq!((e.effect.as_str(), e.op.as_str(), e.args["argv"][0].as_str()), ("e1", "terminal.spawn", Some("sh")));
    assert!(e.deadline > tokio::time::Instant::now() + Duration::from_secs(4));
    e.respond(Ok(Outcome::Observed(Some(json!({"facts": facts("%9")})))));
    let r = core.recv().await;
    assert_eq!((r["id"].as_u64(), r["result"]["outcome"].as_str()), (Some(1), Some("observed")));
    assert_eq!(r["result"]["data"]["facts"]["ref"], "%9");

    let kind = |v: &Value| v["error"]["data"]["kind"].as_str().map(String::from);
    core.send(invoke(2, "e1", "terminal.spawn", 7, 5000)).await;
    assert_eq!(kind(&core.recv().await).as_deref(), Some("conflict"), "a duplicate id never runs twice");
    core.send(invoke(3, "e3", "agent.steer", 7, 5000)).await;
    assert_eq!(kind(&core.recv().await).as_deref(), Some("unsupported"));
    core.send(invoke(4, "e4", "terminal.spawn", 6, 5000)).await;
    assert_eq!(kind(&core.recv().await).as_deref(), Some("stale_generation"));
    core.send(invoke(5, "e5", "terminal.spawn", 7, 0)).await;
    assert_eq!(kind(&core.recv().await).as_deref(), Some("invalid"));
    core.send(json!({"jsonrpc": "2.0", "id": 6, "method": "provider.bogus", "params": {"generation": 7}})).await;
    let r = core.recv().await;
    assert_eq!((kind(&r).as_deref(), r["error"]["code"].as_i64()), (Some("unknown_method"), Some(-32601)));

    // A handler that drops the request still answers once: an error, never "indeterminate".
    core.send(invoke(7, "e7", "terminal.spawn", 7, 5000)).await;
    drop(rx.recv().await.unwrap());
    let r = core.recv().await;
    assert_eq!((r["id"].as_u64(), kind(&r).as_deref()), (Some(7), Some("backend")));
    // Rejected is an outcome (no effect), not an error.
    core.send(invoke(8, "e8", "terminal.spawn", 7, 5000)).await;
    let Some(Request::Effect(e)) = rx.recv().await else { panic!() };
    e.respond(Ok(Outcome::Rejected { kind: "backend".into(), message: "no such session".into() }));
    let r = core.recv().await;
    assert_eq!((r["result"]["outcome"].as_str(), r["result"]["error"]["kind"].as_str()), (Some("rejected"), Some("backend")));
}

#[tokio::test]
async fn streams_wait_for_open_reply_and_credit_with_contiguous_offsets() {
    let (_p, mut rx, mut core) = registered().await;
    core.send(json!({"jsonrpc": "2.0", "id": 1, "method": "stream.open", "params": {"generation": 7, "stream": 5, "ref": "%1",
        "incarnation": "", "cols": 80, "rows": 24, "history_lines": 0, "window_bytes": 10}})).await;
    let Some(Request::Open(open)) = rx.recv().await else { panic!("open") };
    assert_eq!((open.cols, open.rows, open.stream.r#ref.as_str()), (80, 24, "%1"));
    let stream = open.respond(Ok(Initial::Repaint)).unwrap();
    let reply = core.recv().await;
    assert_eq!(reply["result"]["initial"], "repaint");

    let stream = std::sync::Arc::new(tokio::sync::Mutex::new(stream));
    let s2 = stream.clone();
    let sender = tokio::spawn(async move { s2.lock().await.send(b"0123456789ABCDEF").await });
    let d = core.recv().await;
    assert_eq!((d["method"].as_str(), d["params"]["offset"].as_u64()), (Some("stream.data"), Some(0)));
    assert_eq!(B64.decode(d["params"]["data"].as_str().unwrap()).unwrap(), b"0123456789");
    core.nothing().await; // window of 10 is used up
    core.send(json!({"jsonrpc": "2.0", "method": "stream.credit", "params": {"generation": 7, "stream": 5, "bytes": 1000}})).await;
    let d = core.recv().await;
    assert_eq!(d["params"]["offset"].as_u64(), Some(10), "offsets are contiguous");
    assert_eq!(B64.decode(d["params"]["data"].as_str().unwrap()).unwrap(), b"ABCDEF");
    sender.await.unwrap().unwrap();

    core.send(json!({"jsonrpc": "2.0", "method": "stream.input", "params": {"generation": 7, "stream": 5, "data": B64.encode(b"hi")}})).await;
    core.send(json!({"jsonrpc": "2.0", "method": "stream.input", "params": {"generation": 7, "stream": 5, "data": "!!not base64"}})).await;
    core.send(json!({"jsonrpc": "2.0", "method": "stream.resize", "params": {"generation": 7, "stream": 5, "cols": 100, "rows": 30}})).await;
    core.send(json!({"jsonrpc": "2.0", "method": "stream.credit", "params": {"generation": 7, "stream": 99, "bytes": 5}})).await;
    let mut s = stream.lock().await;
    assert_eq!(s.next().await, Some(StreamCommand::Input(b"hi".to_vec())));
    let e = core.recv().await;
    assert_eq!((e["method"].as_str(), e["params"]["error"]["kind"].as_str(), e["params"]["outcome"].as_str()), (Some("stream.error"), Some("invalid"), Some("rejected")));
    assert_eq!(s.next().await, Some(StreamCommand::Resize(100, 30)));

    // Only 6 of the 1000 credited bytes were outstanding: the excess is ignored, window stays 10.
    s.gap("paused").await.unwrap();
    let g = core.recv().await;
    assert_eq!((g["method"].as_str(), g["params"]["offset"].as_u64(), g["params"]["reason"]["gap"].as_str()), (Some("stream.gap"), Some(16), Some("paused")));
    // A gap ends output, not input: input the core sent before it heard the gap is still delivered,
    // until the core's stream.close.
    assert_eq!(s.send(b"late").await.unwrap_err(), ProviderError::new("unavailable", "stream ended"));
    core.send(json!({"jsonrpc": "2.0", "method": "stream.input", "params": {"generation": 7, "stream": 5, "data": B64.encode(b"crossed")}})).await;
    core.send(json!({"jsonrpc": "2.0", "method": "stream.close", "params": {"generation": 7, "stream": 5}})).await;
    assert_eq!(s.next().await, Some(StreamCommand::Input(b"crossed".to_vec())), "input that crossed the gap still arrives");
    assert_eq!(s.next().await, Some(StreamCommand::Close));
    core.nothing().await;
}

#[tokio::test]
async fn excess_credit_is_ignored() {
    let (_p, mut rx, mut core) = registered().await;
    core.send(json!({"jsonrpc": "2.0", "id": 1, "method": "stream.open", "params": {"generation": 7, "stream": 1, "ref": "%1",
        "incarnation": "", "cols": 80, "rows": 24, "history_lines": 0, "window_bytes": 10}})).await;
    let Some(Request::Open(open)) = rx.recv().await else { panic!() };
    let s = open.respond(Ok(Initial::None)).unwrap();
    core.recv().await;
    // Credit before anything was sent: nothing outstanding, so the window stays 10.
    core.send(json!({"jsonrpc": "2.0", "method": "stream.credit", "params": {"generation": 7, "stream": 1, "bytes": 100}})).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    let s = std::sync::Arc::new(s);
    let s2 = s.clone();
    let t = tokio::spawn(async move { s2.send(&[b'x'; 20]).await });
    assert_eq!(B64.decode(core.recv().await["params"]["data"].as_str().unwrap()).unwrap().len(), 10);
    core.nothing().await;
    t.abort();
}

#[tokio::test]
async fn open_errors_and_closed_streams() {
    let (_p, mut rx, mut core) = registered().await;
    let open = |id: u64, stream: u64| json!({"jsonrpc": "2.0", "id": id, "method": "stream.open", "params": {"generation": 7,
        "stream": stream, "ref": "%1", "incarnation": "", "cols": 80, "rows": 24, "history_lines": 0, "window_bytes": 10}});
    core.send(open(1, 3)).await;
    let Some(Request::Open(o)) = rx.recv().await else { panic!() };
    assert!(o.respond(Err(ProviderError::new("not_found", "no pane %1"))).is_none());
    assert_eq!(core.recv().await["error"]["data"]["kind"], "not_found");
    core.send(open(2, 3)).await;
    assert_eq!(core.recv().await["error"]["data"]["kind"], "conflict", "stream ids are never reused");
    core.send(open(3, 4)).await;
    let Some(Request::Open(o)) = rx.recv().await else { panic!() };
    let mut s = o.respond(Ok(Initial::None)).unwrap();
    core.recv().await;
    core.send(json!({"jsonrpc": "2.0", "method": "stream.close", "params": {"generation": 7, "stream": 4}})).await;
    assert_eq!(s.next().await, Some(StreamCommand::Close));
    s.close(json!({"reason": "closed"})).await.unwrap();
    let c = core.recv().await;
    assert_eq!((c["method"].as_str(), c["params"]["reason"]["reason"].as_str()), (Some("stream.closed"), Some("closed")));
    // Input after close is ignored, not an error.
    core.send(json!({"jsonrpc": "2.0", "method": "stream.input", "params": {"generation": 7, "stream": 4, "data": B64.encode(b"x")}})).await;
    core.nothing().await;
}

#[tokio::test]
async fn wrong_generation_notification_disconnects_and_ends_streams() {
    let (p, mut rx, mut core) = registered().await;
    core.send(json!({"jsonrpc": "2.0", "id": 1, "method": "stream.open", "params": {"generation": 7, "stream": 1, "ref": "%1",
        "incarnation": "", "cols": 80, "rows": 24, "history_lines": 0, "window_bytes": 10}})).await;
    let Some(Request::Open(o)) = rx.recv().await else { panic!() };
    let mut s = o.respond(Ok(Initial::None)).unwrap();
    core.recv().await;
    core.send(json!({"jsonrpc": "2.0", "method": "stream.credit", "params": {"generation": 6, "stream": 1, "bytes": 5}})).await;
    tokio::time::timeout(Duration::from_secs(2), p.closed()).await.unwrap();
    assert!(p.failure().unwrap().contains("wrong generation"));
    assert_eq!(s.next().await, None);
    assert_eq!(p.upsert(facts("%1")).await.unwrap_err().kind, "conflict");
    assert_eq!(p.call("provider.sync", json!({})).await.unwrap_err().kind, "unavailable");
}

#[tokio::test]
async fn oversized_record_from_core_is_a_protocol_error() {
    let (p, _rx, mut core) = registered().await;
    let huge = "x".repeat((1 << 20) + 10);
    core.send(json!({"jsonrpc": "2.0", "method": "stream.credit", "params": {"generation": 7, "stream": 1, "pad": huge}})).await;
    tokio::time::timeout(Duration::from_secs(2), p.closed()).await.unwrap();
    assert!(p.failure().unwrap().contains("1 MiB"));
}
