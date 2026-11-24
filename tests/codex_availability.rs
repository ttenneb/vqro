//! Native-only availability controls; no installed Codex, account, credentials or model calls.
use serde_json::{Value, json};
use vqro::codex::Endpoint;

fn endpoint() -> Endpoint {
    let mut e = Endpoint::new("e".into(), "inc".into(), "host".into(), "epoch".into());
    e.set_session(
        &json!({"id":"s1","status":{"type":"idle"}}),
        "epoch-1".into(),
    )
    .unwrap();
    e.notification("turn/started", &json!({"threadId":"s1","turn":{"id":"t1"}}));
    e
}
fn error(info: Value, retry: Value) -> Value {
    json!({"threadId":"s1","turnId":"t1","willRetry":retry,"error":{"codexErrorInfo":info,"message":"secret must not be published"}})
}
fn availability(e: &Endpoint) -> Value {
    e.facts()["data"]["agent"]["availability"].clone()
}

#[test]
fn native_categories_block_live_without_parsing_error_text() {
    for (info, reason) in [
        ("usageLimitExceeded", "quota"),
        ("rateLimitExceeded", "rate_limited"),
        ("unauthorized", "auth"),
    ] {
        let mut e = endpoint();
        assert_eq!(
            e.notification("error", &error(json!(info), json!(false)))
                .len(),
            1
        );
        assert_eq!(availability(&e)["state"], "blocked");
        assert_eq!(availability(&e)["reason"], reason);
        assert_eq!(availability(&e)["native"]["codexErrorInfo"], info);
        assert!(!e.facts().to_string().contains("secret"));
        assert_eq!(e.lifecycle, "live");
    }
    for info in [
        Value::Null,
        json!("futureError"),
        json!("contextWindowExceeded"),
        json!("sessionBudgetExceeded"),
        json!({"httpConnectionFailed":{"httpStatusCode":429}}),
    ] {
        let mut e = endpoint();
        e.notification("error", &error(info, json!(false)));
        assert!(availability(&e).is_null());
    }
    for retry in [Value::Null, json!("false"), json!(0)] {
        let mut e = endpoint();
        e.notification("error", &error(json!("usageLimitExceeded"), retry));
        assert!(availability(&e).is_null());
    }
}

#[test]
fn null_percentages_credits_and_reset_times_never_clear_quota() {
    let mut e = endpoint();
    e.notification("error", &error(json!("usageLimitExceeded"), json!(false)));
    let before = availability(&e);
    for update in [
        json!({}),
        json!({"ordinaryUsageAllowed":null}),
        json!({"ordinaryUsageAllowed":false}),
        json!({"ordinaryUsageAllowed":"true"}),
        json!({"rateLimits":{"primary":{"usedPercent":0,"resetsAt":0},"credits":{"hasCredits":true,"unlimited":true},"rateLimitReachedType":null,"spendControlReached":null}}),
    ] {
        e.notification("account/rateLimits/updated", &update);
        assert_eq!(availability(&e), before, "{update}");
    }
    e.notification(
        "account/rateLimits/updated",
        &json!({"ordinaryUsageAllowed":true}),
    );
    assert_eq!(availability(&e)["state"], "available");
}

#[test]
fn retrying_is_native_until_that_turn_ends_not_a_quota_clear() {
    for status in ["completed", "failed", "interrupted"] {
        let mut e = endpoint();
        e.notification("error", &error(json!("usageLimitExceeded"), json!(true)));
        assert_eq!(availability(&e)["reason"], "_retrying");
        e.notification(
            "account/rateLimits/updated",
            &json!({"ordinaryUsageAllowed":true}),
        );
        e.notification("account/login/completed", &json!({"success":true}));
        e.notification(
            "turn/completed",
            &json!({"threadId":"s1","turn":{"id":"other","status":"completed"}}),
        );
        assert_eq!(availability(&e)["state"], "blocked");
        e.notification(
            "turn/completed",
            &json!({"threadId":"s1","turn":{"id":"t1","status":status}}),
        );
        assert_eq!(availability(&e)["state"], "available");
    }
}

#[test]
fn auth_clears_only_native_login_success_or_successful_turn() {
    let mut e = endpoint();
    e.notification("error", &error(json!("unauthorized"), json!(false)));
    for (method, p) in [
        ("account/login/completed", json!({"success":false})),
        ("account/login/completed", json!({"success":null})),
        ("account/updated", json!({"authMode":"chatgpt"})),
        (
            "modelProvider/authRecoveryCompleted",
            json!({"threadId":"s1","turnId":"t1","provider":"openai","message":"done"}),
        ),
        (
            "account/rateLimits/updated",
            json!({"ordinaryUsageAllowed":true}),
        ),
    ] {
        e.notification(method, &p);
        assert_eq!(availability(&e)["state"], "blocked");
    }
    e.notification("account/login/completed", &json!({"success":true}));
    assert_eq!(availability(&e)["state"], "available");
    let mut e = endpoint();
    e.notification("error", &error(json!("usageLimitExceeded"), json!(false)));
    e.notification("account/login/completed", &json!({"success":true}));
    e.notification(
        "turn/completed",
        &json!({"threadId":"s1","turn":{"id":"t1","status":"failed"}}),
    );
    assert_eq!(availability(&e)["state"], "blocked");
    e.notification("turn/started", &json!({"threadId":"s1","turn":{"id":"t2"}}));
    e.notification(
        "turn/completed",
        &json!({"threadId":"s1","turn":{"id":"t2","status":"completed"}}),
    );
    assert_eq!(availability(&e)["state"], "available");
}

#[test]
fn stale_foreign_and_ended_signals_cannot_block_or_revive() {
    let mut e = endpoint();
    let mut p = error(json!("usageLimitExceeded"), json!(false));
    p["threadId"] = json!("foreign");
    e.notification("error", &p);
    assert!(availability(&e).is_null());
    p["threadId"] = json!("s1");
    p["turnId"] = json!("foreign");
    e.notification("error", &p);
    assert!(availability(&e).is_null());
    e.notification("error", &error(json!("usageLimitExceeded"), json!(false)));
    e.set_session(
        &json!({"id":"s2","status":{"type":"idle"}}),
        "epoch-2".into(),
    )
    .unwrap();
    assert!(availability(&e).is_null());
    e.notification("error", &error(json!("usageLimitExceeded"), json!(false)));
    assert!(availability(&e).is_null());
    e.host_exited("fixture EOF");
    e.notification(
        "account/rateLimits/updated",
        &json!({"ordinaryUsageAllowed":true}),
    );
    assert!(availability(&e).is_null());
    assert_eq!(e.lifecycle, "ended");
}

#[test]
fn each_persistent_cause_requires_its_own_native_clear_even_across_retrying() {
    for reason in ["auth", "_retrying"] {
        let mut e = endpoint();
        e.notification("error", &error(json!("usageLimitExceeded"), json!(false)));
        e.notification(
            "turn/completed",
            &json!({"threadId":"s1","turn":{"id":"t1","status":"failed"}}),
        );
        e.notification("turn/started", &json!({"threadId":"s1","turn":{"id":"t2"}}));
        let mut next = error(json!("unauthorized"), json!(reason == "_retrying"));
        next["turnId"] = json!("t2");
        e.notification("error", &next);
        assert_eq!(availability(&e)["reason"], reason);
        if reason == "auth" {
            e.notification("account/login/completed", &json!({"success":true}));
        } else {
            e.notification(
                "turn/completed",
                &json!({"threadId":"s1","turn":{"id":"t2","status":"failed"}}),
            );
        }
        assert_eq!(
            availability(&e)["state"],
            "blocked",
            "{reason} clear must not imply quota recovery"
        );
        assert_eq!(availability(&e)["reason"], "quota");
        e.notification(
            "account/rateLimits/updated",
            &json!({"ordinaryUsageAllowed":true}),
        );
        assert_eq!(availability(&e)["state"], "available");
    }
}

use std::sync::{Arc, Mutex};
use tokio::time::{Duration, Instant};
use vqro::codex::runtime::Runtime;
use vqro::rpc::{Incoming, Peer};
struct Wire {
    runtime: Arc<Runtime>,
    native: Peer,
    calls: Arc<Mutex<Vec<String>>>,
    permission: Arc<Mutex<Value>>,
    read_notice: Arc<tokio::sync::Notify>,
    server: tokio::task::JoinHandle<()>,
    reader: tokio::task::JoinHandle<()>,
}
impl Drop for Wire {
    fn drop(&mut self) {
        self.runtime.native.close();
        self.native.close();
        self.server.abort();
        self.reader.abort();
    }
}
async fn wire(permission: Value) -> Wire {
    let (a, b) = tokio::io::duplex(65536);
    let (ar, aw) = tokio::io::split(a);
    let (br, bw) = tokio::io::split(b);
    let (peer, incoming) = Peer::spawn(ar, aw);
    let (native, mut requests) = Peer::spawn(br, bw);
    let runtime = Runtime::new(peer, 1, "inc".into(), None);
    let drain = runtime.clone();
    let reader = tokio::spawn(async move {
        drain.drain_native(incoming).await;
    });
    let calls = Arc::new(Mutex::new(Vec::new()));
    let trace = calls.clone();
    let permission = Arc::new(Mutex::new(permission));
    let permission_read = permission.clone();
    let read_notice = Arc::new(tokio::sync::Notify::new());
    let read_seen = read_notice.clone();
    let server_peer = native.clone();
    let server = tokio::spawn(async move {
        while let Some(i) = requests.recv().await {
            if let Incoming::Request {
                id,
                method,
                params: _,
            } = i
            {
                trace.lock().unwrap().push(method.clone());
                let response = match method.as_str() {
                    "thread/start" => {
                        json!({"thread":{"id":"s1","status":{"type":"idle"}},"model":"fixture"})
                    }
                    "turn/start" => json!({"turn":{"id":"t1"}}),
                    "account/rateLimits/read" => {
                        let response = permission_read.lock().unwrap().clone();
                        read_seen.notify_one();
                        response
                    }
                    other => panic!("unexpected native method {other}"),
                };
                if response["_fixture_no_response"] == true {
                    continue;
                }
                if response["_fixture_account_change"] == true {
                    server_peer.notify("account/updated", json!({"authMode":"chatgpt"}));
                }
                if let Some(permission) = response.get("_fixture_permission_change") {
                    server_peer.notify(
                        "account/rateLimits/updated",
                        json!({"ordinaryUsageAllowed":permission,"rateLimits":{}}),
                    );
                }
                server_peer.respond(id, Ok(response));
            }
        }
    });
    let w = Wire {
        runtime,
        native,
        calls,
        permission,
        read_notice,
        server,
        reader,
    };
    w.runtime
        .invoke(
            "endpoint-1",
            "inc",
            "agent.session",
            json!({"v":0,"action":"new"}),
            Instant::now() + Duration::from_secs(2),
        )
        .await
        .unwrap();
    w
}
async fn until(w: &Wire, f: impl Fn(&Value) -> bool) {
    let mut changes = w.runtime.changes();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if f(&w.runtime.facts()[0]) {
                break;
            }
            changes.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
}
#[tokio::test]
async fn native_retry_notifications_do_not_replay_the_one_admitted_input() {
    let w = wire(json!({"ordinaryUsageAllowed":true})).await;
    let outcome=w.runtime.invoke("endpoint-1","inc","agent.send",json!({"v":0,"mode":"admit","delivery":"once","origin":{"principal":"p","actor":"a","kind":"agent"},"content":[{"type":"text","text":"one input"}]}),Instant::now()+Duration::from_secs(2)).await.unwrap();
    assert!(matches!(
        outcome,
        vqro::provider_client::Outcome::Accepted(_)
    ));
    w.native
        .notify("turn/started", json!({"threadId":"s1","turn":{"id":"t1"}}));
    for _ in 0..16 {
        w.native
            .notify("error", error(json!("rateLimitExceeded"), json!(true)));
    }
    until(&w, |f| {
        f["data"]["agent"]["availability"]["reason"] == "_retrying"
    })
    .await;
    w.native.notify(
        "turn/completed",
        json!({"threadId":"s1","turn":{"id":"t1","status":"failed"}}),
    );
    until(&w, |f| {
        f["data"]["agent"]["availability"]["state"] == "available"
    })
    .await;
    let calls = w.calls.lock().unwrap();
    assert_eq!(calls.iter().filter(|m| *m == "turn/start").count(), 1);
    assert_eq!(
        calls
            .iter()
            .filter(|m| *m == "account/rateLimits/read")
            .count(),
        0
    );
}
#[tokio::test]
async fn bounded_native_permission_read_publishes_recovery_without_input() {
    let w = wire(json!({"ordinaryUsageAllowed":true,"rateLimits":{}})).await;
    w.native
        .notify("error", error(json!("usageLimitExceeded"), json!(false)));
    until(&w, |f| {
        f["data"]["agent"]["availability"]["state"] == "available"
    })
    .await;
    assert_eq!(
        w.calls.lock().unwrap().as_slice(),
        ["thread/start", "account/rateLimits/read"]
    );
    assert_eq!(
        w.runtime.take_dirty_facts()[0]["data"]["agent"]["availability"]["state"],
        "available"
    );
}
#[tokio::test]
async fn null_native_read_and_many_percentage_updates_stay_blocked_without_extra_reads() {
    let w=wire(json!({"ordinaryUsageAllowed":null,"rateLimits":{"primary":{"usedPercent":0,"resetsAt":0}}})).await;
    w.native
        .notify("error", error(json!("rateLimitExceeded"), json!(false)));
    until(&w, |f| {
        f["data"]["agent"]["availability"]["state"] == "blocked"
    })
    .await;
    w.read_notice.notified().await;
    // A following notification is not permission, even when every usage percentage is zero.
    for _ in 0..32 {
        w.native.notify(
            "account/rateLimits/updated",
            json!({"ordinaryUsageAllowed":null,"rateLimits":{"primary":{"usedPercent":0}}}),
        );
    }
    for _ in 0..64 {
        tokio::task::yield_now().await;
    }
    assert_eq!(
        w.runtime.facts()[0]["data"]["agent"]["availability"]["state"],
        "blocked"
    );
    assert_eq!(
        w.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|m| *m == "account/rateLimits/read")
            .count(),
        1
    );
    w.native.notify(
        "account/rateLimits/updated",
        json!({"ordinaryUsageAllowed":true}),
    );
    until(&w, |f| {
        f["data"]["agent"]["availability"]["state"] == "available"
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn cadence_is_at_most_one_read_per_sixty_seconds_and_elapsed_time_never_clears() {
    let w = wire(json!({"ordinaryUsageAllowed":null})).await;
    w.native
        .notify("error", error(json!("usageLimitExceeded"), json!(false)));
    w.read_notice.notified().await;
    for _ in 0..32 {
        tokio::task::yield_now().await;
    }
    tokio::time::advance(Duration::from_secs(59)).await;
    for _ in 0..32 {
        tokio::task::yield_now().await;
    }
    assert_eq!(
        w.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|m| *m == "account/rateLimits/read")
            .count(),
        1
    );
    assert_eq!(
        w.runtime.facts()[0]["data"]["agent"]["availability"]["state"],
        "blocked"
    );
    *w.permission.lock().unwrap() = json!({"ordinaryUsageAllowed":null,"rateLimits":{"primary":{"usedPercent":0,"resetsAt":0}}});
    tokio::time::advance(Duration::from_secs(1)).await;
    w.read_notice.notified().await;
    for _ in 0..32 {
        tokio::task::yield_now().await;
    }
    assert_eq!(
        w.runtime.facts()[0]["data"]["agent"]["availability"]["state"],
        "blocked"
    );
    assert_eq!(
        w.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|m| *m == "account/rateLimits/read")
            .count(),
        2
    );
    *w.permission.lock().unwrap() = json!({"ordinaryUsageAllowed":true});
    tokio::time::advance(Duration::from_secs(60)).await;
    until(&w, |f| {
        f["data"]["agent"]["availability"]["state"] == "available"
    })
    .await;
    assert_eq!(
        w.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|m| *m == "account/rateLimits/read")
            .count(),
        3
    );
    tokio::time::advance(Duration::from_secs(600)).await;
    for _ in 0..32 {
        tokio::task::yield_now().await;
    }
    assert_eq!(
        w.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|m| *m == "account/rateLimits/read")
            .count(),
        3
    );
}

#[tokio::test(start_paused = true)]
async fn read_timeout_stays_blocked_and_never_resends_input() {
    let w = wire(json!({"_fixture_no_response":true})).await;
    w.native
        .notify("error", error(json!("usageLimitExceeded"), json!(false)));
    w.read_notice.notified().await;
    tokio::time::advance(Duration::from_secs(4)).await;
    for _ in 0..32 {
        tokio::task::yield_now().await;
    }
    assert_eq!(
        w.runtime.facts()[0]["data"]["agent"]["availability"]["state"],
        "blocked"
    );
    *w.permission.lock().unwrap() = json!({"ordinaryUsageAllowed":true});
    tokio::time::advance(Duration::from_secs(56)).await;
    until(&w, |f| {
        f["data"]["agent"]["availability"]["state"] == "available"
    })
    .await;
    assert_eq!(
        w.calls.lock().unwrap().as_slice(),
        [
            "thread/start",
            "account/rateLimits/read",
            "account/rateLimits/read"
        ]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actual_peer_wire_order_fences_the_old_read_on_two_workers() {
    for permission in [json!(false), Value::Null].into_iter().cycle().take(256) {
        let w = wire(json!({"ordinaryUsageAllowed":true,"_fixture_permission_change":permission}))
            .await;
        w.native
            .notify("error", error(json!("usageLimitExceeded"), json!(false)));
        w.read_notice.notified().await;
        for _ in 0..64 {
            tokio::task::yield_now().await;
        }
        assert_eq!(
            w.runtime.facts()[0]["data"]["agent"]["availability"]["state"],
            "blocked",
            "wire-ordered newer {permission} must fence old true"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn newer_false_or_null_permission_push_fences_an_older_true_read() {
    for permission in [json!(false), Value::Null] {
        let w = wire(json!({"ordinaryUsageAllowed":true,"_fixture_permission_change":permission}))
            .await;
        w.native
            .notify("error", error(json!("usageLimitExceeded"), json!(false)));
        w.read_notice.notified().await;
        for _ in 0..64 {
            tokio::task::yield_now().await;
        }
        assert_eq!(
            w.runtime.facts()[0]["data"]["agent"]["availability"]["state"],
            "blocked",
            "newer {permission} must fence old true"
        );
        *w.permission.lock().unwrap() = json!({"ordinaryUsageAllowed":true});
        tokio::time::advance(Duration::from_secs(60)).await;
        until(&w, |f| {
            f["data"]["agent"]["availability"]["state"] == "available"
        })
        .await;
    }
}

#[tokio::test(start_paused = true)]
async fn account_change_discards_old_permission_without_inventing_recovery() {
    let w = wire(json!({"ordinaryUsageAllowed":true,"_fixture_account_change":true})).await;
    w.native
        .notify("error", error(json!("usageLimitExceeded"), json!(false)));
    w.read_notice.notified().await;
    for _ in 0..64 {
        tokio::task::yield_now().await;
    }
    assert!(w.runtime.facts()[0]["data"]["agent"]["availability"].is_null());
    *w.permission.lock().unwrap() = json!({"ordinaryUsageAllowed":true});
    tokio::time::advance(Duration::from_secs(60)).await;
    for _ in 0..64 {
        tokio::task::yield_now().await;
    }
    assert!(
        w.runtime.facts()[0]["data"]["agent"]["availability"].is_null(),
        "neither elapsed time nor A's old true read proves B recovery"
    );
    assert_eq!(
        w.calls.lock().unwrap().as_slice(),
        ["thread/start", "account/rateLimits/read"]
    );
}
