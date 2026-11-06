//! Barrier characterization of generic worker rules, not task/product acceptance.
use super::*;
use crate::action_invocation::InvocationCaller;
use crate::api::schema::{ExtensionActionInvokeParams, ExtensionServiceCapability};
use serde_json::{json, Value};

struct PrivateRoot(PathBuf);
impl PrivateRoot {
    fn new() -> Self {
        use std::os::unix::fs::DirBuilderExt;
        let root = std::env::temp_dir().join(format!(
            "vqro-generic-{}-{}",
            std::process::id(),
            next_service_generation()
        ));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .unwrap();
        Self(root)
    }
    fn path(&self) -> &std::path::Path {
        &self.0
    }
}
impl Drop for PrivateRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Fixture {
    root: PrivateRoot,
}

impl Fixture {
    fn new() -> Self {
        Self {
            root: PrivateRoot::new(),
        }
    }

    fn manager(&self) -> ExtensionServiceManager {
        self.manager_with_startup(Duration::ZERO)
    }

    fn manager_with_startup(&self, delay: Duration) -> ExtensionServiceManager {
        self.manager_with_options(delay, false)
    }

    fn manager_with_options(&self, delay: Duration, client: bool) -> ExtensionServiceManager {
        let script = r#"
import json,sys,time,pathlib
root=pathlib.Path(sys.argv[1])
print(json.dumps({'type':'hello','protocol':2,'service_id':'test.generic','requested_capabilities':['action_invoke'],'methods':['action.invoke.v1']}),flush=True)
json.loads(sys.stdin.readline())
for line in sys.stdin:
 request=json.loads(line)
 payload=request['params']['payload']
 tag=payload['tag']
 with (root/'calls').open('a') as output: output.write(tag+'\n')
 (root/('entered-'+tag)).touch()
 if payload.get('block'):
  while not (root/('release-'+tag)).exists(): time.sleep(.002)
 print(json.dumps({'type':'response','id':request['id'],'result':{'ok':True,'tag':tag,'caller':request['params']['caller'],'method':request['method']}}),flush=True)
"#;
        let script = if client {
            script.replace(
                "'methods':['action.invoke.v1']",
                "'methods':['action.invoke.v1','action.invoke.client.v1']",
            )
        } else {
            script.to_owned()
        };
        let (event_tx, _event_rx) = tokio::sync::mpsc::channel(1);
        manager_for_test_spec(
            ExtensionServiceSpec {
                id: "test.generic".into(),
                command: vec![
                    "python3".into(),
                    "-B".into(),
                    "-c".into(),
                    format!(
                        "import time; time.sleep({});\n{script}",
                        delay.as_secs_f64()
                    ),
                    self.root.path().display().to_string(),
                ],
                cwd: self.root.path().into(),
                priority: 1,
                granted_capabilities: vec![ExtensionServiceCapability::ActionInvoke],
            },
            event_tx,
        )
    }

    fn wait_entered(&self, tag: &str) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !self.root.path().join(format!("entered-{tag}")).exists() {
            assert!(Instant::now() < deadline, "fixture never entered {tag}");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn release(&self, tag: &str) {
        std::fs::write(self.root.path().join(format!("release-{tag}")), "").unwrap();
    }

    fn calls(&self) -> Vec<String> {
        std::fs::read_to_string(self.root.path().join("calls"))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }
}

fn invoke(manager: &ExtensionServiceManager, tag: &str, block: bool) -> mpsc::Receiver<String> {
    let (tx, rx) = mpsc::channel();
    manager.request_generic_action(
        tag.into(),
        ExtensionActionInvokeParams {
            service_id: "test.generic".into(),
            payload: json!({"tag":tag,"block":block}),
        },
        InvocationCaller::Unknown,
        tx,
    );
    rx
}

fn reply(rx: mpsc::Receiver<String>) -> Value {
    serde_json::from_str(
        &rx.recv_timeout(Duration::from_secs(5))
            .expect("bounded generic result"),
    )
    .unwrap()
}

fn client_caller() -> InvocationCaller {
    InvocationCaller::ClientHumanUnverified {
        account: crate::action_invocation::InvocationAccount::Unix { uid: 1000 },
        client: crate::client_invocation::ClientIdentity {
            boot_id: "boot".into(),
            pid: 123,
            birth: 456,
            tty: crate::client_invocation::ClientTtyDescriptor {
                kind: "unix".into(),
                device: "1:2:3".into(),
            },
        },
    }
}

fn invoke_client(
    manager: &ExtensionServiceManager,
    tag: &str,
    block: bool,
) -> mpsc::Receiver<String> {
    let (tx, rx) = mpsc::channel();
    manager.request_generic_action(
        tag.into(),
        ExtensionActionInvokeParams {
            service_id: "test.generic".into(),
            payload: json!({"tag":tag,"block":block}),
        },
        client_caller(),
        tx,
    );
    rx
}

#[test]
fn client_caller_never_falls_back_to_legacy_provider_method() {
    let fixture = Fixture::new();
    let manager = fixture.manager();
    let value = reply(invoke_client(&manager, "no-client-method", false));
    assert_eq!(value["error"]["code"], "action_rejected");
    assert!(
        fixture.calls().is_empty(),
        "no caller sent to legacy-only provider"
    );
}

#[test]
fn client_caller_uses_explicit_negotiation_without_changing_legacy_envelope() {
    let fixture = Fixture::new();
    let manager = fixture.manager_with_options(Duration::ZERO, true);
    let value = reply(invoke_client(&manager, "client", false));
    assert_eq!(
        value["result"]["value"]["method"],
        "action.invoke.client.v1"
    );
    assert_eq!(
        value["result"]["value"]["caller"],
        serde_json::to_value(client_caller()).unwrap()
    );
    assert_eq!(
        reply(invoke(&manager, "legacy", false))["result"]["value"]["method"],
        "action.invoke.v1"
    );
}

#[test]
fn client_canceled_queued_action_never_reaches_provider() {
    let fixture = Fixture::new();
    let manager = fixture.manager_with_options(Duration::ZERO, true);
    let first = invoke(&manager, "first", true);
    fixture.wait_entered("first");
    let canceled = Arc::new(AtomicBool::new(false));
    let (tx, rx) = mpsc::channel();
    manager.request_client_action(
        "canceled".into(),
        ExtensionActionInvokeParams {
            service_id: "test.generic".into(),
            payload: json!({"tag":"canceled"}),
        },
        client_caller(),
        tx,
        manager.action_generation("test.generic").unwrap(),
        canceled.clone(),
    );
    canceled.store(true, Ordering::Release);
    fixture.release("first");
    assert!(reply(first).get("result").is_some());
    assert_eq!(reply(rx)["error"]["code"], "action_rejected");
    assert_eq!(fixture.calls(), ["first"]);
}

#[test]
fn client_known_reply_survives_later_provider_retirement() {
    let fixture = Fixture::new();
    let manager = fixture.manager_with_options(Duration::ZERO, true);
    let result = invoke_client(&manager, "known", true);
    fixture.wait_entered("known");
    manager.workers["test.generic"]
        .generation
        .fetch_add(1, Ordering::AcqRel);
    fixture.release("known");
    assert_eq!(reply(result)["result"]["value"]["tag"], "known");
}

fn wait_slots(expected: usize) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while EXTENSION_WORK_SLOTS.load(Ordering::Acquire) != expected {
        assert!(Instant::now() < deadline, "generic work slot leaked");
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn generic_expired_queued_action_never_executes_and_returns_slot() {
    let fixture = Fixture::new();
    let manager = fixture.manager();
    let baseline = EXTENSION_WORK_SLOTS.load(Ordering::Acquire);
    let first = invoke(&manager, "first", true);
    fixture.wait_entered("first");
    let worker = manager.workers.get("test.generic").unwrap();
    let (tx, rx) = mpsc::channel();
    // The real worker is blocked at the fixture barrier. Supply an already-expired
    // start deadline rather than asserting a wall-clock queue duration.
    worker
        .requests
        .try_send(ExtensionWorkerRequest::Generic(GenericActionRequest {
            client_source: false,
            canceled: None,
            generation: worker.generation.load(Ordering::Acquire),
            deadline: Instant::now(),
            params: json!({"caller":{"class":"unknown"},"payload":{"tag":"expired"}}),
            request_id: "expired".into(),
            respond_to: tx,
            _slot: ExtensionWorkSlotGuard::acquire().unwrap(),
        }))
        .unwrap();
    fixture.release("first");
    assert_eq!(reply(first)["result"]["value"]["tag"], "first");
    assert_eq!(reply(rx)["error"]["code"], "action_rejected");
    wait_slots(baseline);
    assert_eq!(fixture.calls(), ["first"]);
    assert_eq!(
        reply(invoke(&manager, "recovered", false))["result"]["value"]["tag"],
        "recovered"
    );
    wait_slots(baseline);
}

#[test]
fn generic_generation_change_fences_inflight_and_queued_actions() {
    let fixture = Fixture::new();
    let manager = fixture.manager();
    let first = invoke(&manager, "first", true);
    fixture.wait_entered("first");
    let queued = invoke(&manager, "old-generation", false);
    manager.workers["test.generic"]
        .generation
        .fetch_add(1, Ordering::AcqRel);
    fixture.release("first");
    assert_eq!(reply(first)["error"]["code"], "outcome_unknown");
    assert_eq!(reply(queued)["error"]["code"], "action_rejected");
    assert_eq!(
        reply(invoke(&manager, "new-generation", false))["result"]["value"]["tag"],
        "new-generation"
    );
    assert_eq!(fixture.calls(), ["first", "new-generation"]);
}

#[test]
fn generic_drop_with_full_channel_cancels_inflight_rejects_queue_and_recovers_slots() {
    let fixture = Fixture::new();
    let manager = fixture.manager();
    let baseline = EXTENSION_WORK_SLOTS.load(Ordering::Acquire);
    let first = invoke(&manager, "first", true);
    fixture.wait_entered("first");
    let queued = (0..8)
        .map(|index| invoke(&manager, &format!("queued-{index}"), false))
        .collect::<Vec<_>>();
    assert_eq!(
        reply(invoke(&manager, "full", false))["error"]["code"],
        "action_rejected"
    );
    drop(manager);
    assert_eq!(reply(first)["error"]["code"], "outcome_unknown");
    for response in queued {
        assert_eq!(reply(response)["error"]["code"], "action_rejected");
    }
    wait_slots(baseline);
    assert_eq!(fixture.calls(), ["first"]);
    let replacement = fixture.manager();
    assert_eq!(
        reply(invoke(&replacement, "replacement", false))["result"]["value"]["tag"],
        "replacement"
    );
    wait_slots(baseline);
}

#[test]
fn generic_lost_receiver_does_not_retry_and_releases_slot() {
    let fixture = Fixture::new();
    let manager = fixture.manager();
    let baseline = EXTENSION_WORK_SLOTS.load(Ordering::Acquire);
    let receiver = invoke(&manager, "once", true);
    fixture.wait_entered("once");
    drop(receiver);
    fixture.release("once");
    wait_slots(baseline);
    assert_eq!(fixture.calls(), ["once"]);
    let result = reply(invoke(&manager, "next", false));
    assert_eq!(result["result"]["value"]["caller"]["class"], "unknown");
    assert_eq!(fixture.calls(), ["once", "next"]);
}

#[test]
fn generic_action_can_outlive_unchanged_render_timeout() {
    let fixture = Fixture::new();
    let manager = fixture.manager();
    let response = invoke(&manager, "durable", true);
    fixture.wait_entered("durable");
    // Supporting timeout separation, not a performance/CI speed threshold.
    std::thread::sleep(crate::extension_runtime::REQUEST_TIMEOUT + Duration::from_millis(150));
    fixture.release("durable");
    assert_eq!(reply(response)["result"]["value"]["tag"], "durable");
}

#[test]
fn generic_slow_startup_overrun_is_unknown_without_action_dispatch() {
    let fixture = Fixture::new();
    let manager = fixture.manager_with_startup(Duration::from_millis(2600));
    let response = reply(invoke(&manager, "expired-after-startup", false));
    assert_eq!(response["error"]["code"], "outcome_unknown");
    assert!(
        fixture.calls().is_empty(),
        "expired deadline must not dispatch an action"
    );
}

#[test]
fn generic_payload_and_provider_admission_reject_before_spawn() {
    let fixture = Fixture::new();
    let manager = fixture.manager();
    for request in [
        ExtensionActionInvokeParams {
            service_id: "missing".into(),
            payload: json!({}),
        },
        ExtensionActionInvokeParams {
            service_id: "test.generic".into(),
            payload: json!({"large":"x".repeat(65537)}),
        },
    ] {
        let (tx, rx) = mpsc::channel();
        manager.request_generic_action("rejected".into(), request, InvocationCaller::Unknown, tx);
        assert_eq!(reply(rx)["error"]["code"], "action_rejected");
    }
    assert!(fixture.calls().is_empty());
}
