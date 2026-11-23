//! Fake JSONL Codex only. No installed runtime, account/profile reads or model calls.
use super::*;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

struct Fake {
    runtime: Arc<Runtime>,
    methods: Arc<Mutex<Vec<String>>>,
    foreign: Arc<AtomicBool>,
    drain: tokio::task::JoinHandle<()>,
    server: tokio::task::JoinHandle<()>,
}
impl Drop for Fake {
    fn drop(&mut self) {
        self.runtime.native.close();
        self.drain.abort();
        self.server.abort();
    }
}
async fn fake(mode: &'static str) -> Fake {
    let (client, server) = tokio::io::duplex(64 * 1024);
    let (read, write) = tokio::io::split(client);
    let (peer, incoming) = Peer::spawn(read, write);
    let runtime = Runtime::new(peer, 1, "inc".into(), None);
    runtime.set_cutover_setup("shared").unwrap();
    runtime.slots[0]
        .endpoint
        .lock()
        .unwrap()
        .set_session(
            &json!({"id":"thread-a","status":{"type":"idle"}}),
            "epoch-a".into(),
        )
        .unwrap();
    let methods = Arc::new(Mutex::new(Vec::new()));
    let calls = methods.clone();
    let foreign = Arc::new(AtomicBool::new(false));
    let native_foreign = foreign.clone();
    let server = tokio::spawn(async move {
        let (read, mut write) = tokio::io::split(server);
        let mut lines = BufReader::new(read).lines();
        let mut switched = mode == "already_active";
        while let Ok(Some(line)) = lines.next_line().await {
            let request: Value = serde_json::from_str(&line).unwrap();
            let method = request["method"].as_str().unwrap();
            calls.lock().unwrap().push(method.to_owned());
            let active = if switched { "saved-b" } else { "saved-a" };
            let sessions = |active: &str| {
                json!({"activeSessionId":active,"sessions":[
                {"sessionId":"saved-a","isActive":active=="saved-a","selectedWorkspaceAccountId":"workspace-a","workspaces":[{"accountId":"workspace-a"}],"email":"never-publish@example.test"},
                {"sessionId":"saved-b","isActive":active=="saved-b","selectedWorkspaceAccountId":"workspace-b","workspaces":[{"accountId":"workspace-b"}],"accessToken":"never-publish"},
                {"sessionId":"saved-c","isActive":active=="saved-c","selectedWorkspaceAccountId":"workspace-c","workspaces":[{"accountId":"workspace-c"}]}]})
            };
            let result = match method {
                "accountSession/list" => {
                    let snapshot = sessions(if mode == "wrong_readback" {
                        "saved-a"
                    } else if native_foreign.load(Ordering::Acquire) {
                        "saved-c"
                    } else {
                        active
                    });
                    if mode == "foreign_during_readback"
                        && switched
                        && !native_foreign.load(Ordering::Acquire)
                    {
                        // The response is a B snapshot, but C is now active. Queue the C
                        // notification before replying so production drain ordering is deterministic.
                        native_foreign.store(true, Ordering::Release);
                        write.write_all(format!("{}\n",json!({"method":"account/updated","params":{"authMode":"chatgpt"}})).as_bytes()).await.unwrap();
                    }
                    snapshot
                }
                "accountSession/switch" => {
                    assert_eq!(
                        request["params"],
                        json!({"sessionId":"saved-b","accountId":"workspace-b","allowBrowserLogin":false})
                    );
                    switched = true;
                    write
                        .write_all(
                            format!(
                                "{}\n",
                                json!({"method":"thread/closed","params":{"threadId":"thread-a"}})
                            )
                            .as_bytes(),
                        )
                        .await
                        .unwrap();
                    write
                        .write_all(
                            format!(
                                "{}\n",
                                json!({"method":"account/updated","params":{"authMode":"chatgpt"}})
                            )
                            .as_bytes(),
                        )
                        .await
                        .unwrap();
                    if mode == "browser" {
                        json!({"login_id":"sso","auth_url":"never-publish"})
                    } else {
                        sessions("saved-b")
                    }
                }
                "thread/resume" => {
                    assert_eq!(request["params"], json!({"threadId":"thread-a"}));
                    json!({"thread":{"id":if mode == "wrong_thread" {"other"} else {"thread-a"},"status":{"type":"idle"}}})
                }
                other => panic!("unexpected native request {other}"),
            };
            write
                .write_all(format!("{}\n", json!({"id":request["id"],"result":result})).as_bytes())
                .await
                .unwrap();
        }
    });
    let this = runtime.clone();
    let drain = tokio::spawn(async move { this.drain_native(incoming).await });
    Fake {
        runtime,
        methods,
        foreign,
        server,
        drain,
    }
}
async fn command(f: &Fake, name: &str, extra: Value) -> Result<Outcome, ProviderError> {
    let mut p = json!({"operation":"claimed-once","setup_id":"shared"});
    p.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    f.runtime
        .invoke(
            "endpoint-1",
            "inc",
            "agent.command",
            json!({"v":0,"name":name,"args":p}),
            Instant::now() + Duration::from_secs(2),
        )
        .await
}
async fn fence(f: &Fake) {
    assert!(matches!(
        command(
            f,
            "_account.fence",
            json!({"members":[{"session":"thread-a","epoch":"epoch-a"}]})
        )
        .await
        .unwrap(),
        Outcome::Observed(_)
    ));
}
fn target() -> Value {
    json!({"login_ref":{"sessionId":"saved-b","accountId":"workspace-b"},"account":{"id":"b","label":"B","kind":"subscription"}})
}

#[tokio::test]
async fn unverified_launch_hint_b_never_labels_native_a_usage_as_b() {
    let f = fake("ok").await;
    *f.runtime.launcher_account.lock().unwrap() = Some(target()["account"].clone());
    f.runtime.slots[0].endpoint.lock().unwrap().usage_limits.observe(&json!({"accountId":"workspace-a","rateLimits":{"primary":{"usedPercent":10,"windowDurationMins":300}}}));
    let fact = f.runtime.fact("endpoint-1").unwrap();
    assert!(fact["data"]["agent"]["account"]["id"].is_null());
    assert!(fact["data"]["agent"]["usage_limits"][0]["scope"]["account"].is_null());
}

#[tokio::test]
async fn native_binding_refuses_inactive_b_and_maps_current_a_readonly() {
    let f = fake("ok").await;
    let result = command(&f, "_account.bind", target()).await.unwrap();
    let Outcome::Rejected { message, .. } = result else {
        panic!("inactive selection must be refused")
    };
    assert_eq!(
        message,
        "Codex agents share one login; switch the group instead"
    );
    assert!(f.runtime.fact("endpoint-1").unwrap()["data"]["agent"]["account"]["id"].is_null());
    let source = json!({"login_ref":{"sessionId":"saved-a","accountId":"workspace-a"},"account":{"id":"a","label":"A","kind":"subscription"}});
    assert!(matches!(
        command(&f, "_account.bind", source).await.unwrap(),
        Outcome::Observed(_)
    ));
    let fact = f.runtime.fact("endpoint-1").unwrap();
    assert_eq!(fact["data"]["agent"]["account"]["id"], "a");
    assert_eq!(
        fact["data"]["agent"]["account"]["native"]["login_ref"],
        json!({"sessionId":"saved-a","accountId":"workspace-a"})
    );
    assert!(
        f.methods
            .lock()
            .unwrap()
            .iter()
            .all(|m| m == "accountSession/list")
    );
}

fn restore_accounts() -> Value {
    json!({"accounts":[target(),{"login_ref":{"sessionId":"saved-a","accountId":"workspace-a"},"account":{"id":"a","label":"A","kind":"subscription"}}]})
}

#[tokio::test]
async fn revoked_consent_restores_current_a_or_b_without_any_credential_rollback_or_input() {
    for switched in [false, true] {
        let f = fake("ok").await;
        fence(&f).await;
        if switched {
            command(&f, "_account.switch", target()).await.unwrap();
        }
        let before = f.methods.lock().unwrap().len();
        assert!(matches!(
            command(&f, "_account.restore", restore_accounts())
                .await
                .unwrap(),
            Outcome::Observed(_)
        ));
        assert!(matches!(
            command(&f, "_account.release", json!({"revoked":true}))
                .await
                .unwrap(),
            Outcome::Observed(_)
        ));
        let fact = f.runtime.fact("endpoint-1").unwrap();
        assert_eq!(fact["data"]["agent"]["session"]["id"], "thread-a");
        assert_ne!(fact["data"]["agent"]["epoch"], "epoch-a");
        assert_eq!(
            fact["data"]["agent"]["account"]["id"],
            if switched { "b" } else { "a" }
        );
        assert!(f.runtime.cutover.lock().unwrap().is_none());
        assert!(f.runtime.slots[0].admission.try_lock().is_ok());
        assert_eq!(
            f.methods.lock().unwrap()[before..],
            ["accountSession/list", "thread/resume"]
        );
    }
}

#[tokio::test]
async fn restore_never_replays_an_uncertain_switch_or_adopts_a_foreign_login() {
    for mode in ["browser", "already_active"] {
        let f = fake(mode).await;
        fence(&f).await;
        if mode == "browser" {
            assert!(command(&f, "_account.switch", target()).await.is_err());
        } else {
            f.foreign.store(true, Ordering::Release);
        }
        assert!(
            matches!(
                command(&f, "_account.restore", restore_accounts()).await,
                Err(_) | Ok(Outcome::Rejected { .. })
            ),
            "{mode}"
        );
        assert!(f.runtime.cutover.lock().unwrap().is_some());
        assert!(f.runtime.slots[0].admission.try_lock().is_err());
        assert!(
            !f.methods
                .lock()
                .unwrap()
                .iter()
                .any(|m| m == "thread/resume" || m == "turn/start")
        );
    }
}

#[tokio::test]
async fn all_slot_fence_native_switch_readback_and_exact_resume_never_sends_input() {
    let f = fake("ok").await;
    fence(&f).await;
    let send = f.runtime.invoke("endpoint-1","inc","agent.send",json!({"v":0,"mode":"admit","delivery":"d","content":[{"type":"text","text":"never resend failed input"}]}),Instant::now()+Duration::from_secs(1)).await.unwrap();
    assert!(matches!(send, Outcome::Rejected { .. }));
    assert!(matches!(
        command(&f, "_account.switch", target()).await.unwrap(),
        Outcome::Observed(_)
    ));
    assert!(matches!(
        command(&f, "_account.resume", json!({})).await.unwrap(),
        Outcome::Observed(_)
    ));
    assert!(matches!(
        command(&f, "_account.release", json!({})).await.unwrap(),
        Outcome::Observed(_)
    ));
    let fact = f.runtime.fact("endpoint-1").unwrap();
    assert_eq!(fact["data"]["agent"]["session"]["id"], "thread-a");
    assert_eq!(fact["data"]["agent"]["account"]["id"], "b");
    assert!(fact["data"]["agent"]["account"]["rev"].as_u64().unwrap() > 0);
    assert_eq!(
        *f.methods.lock().unwrap(),
        [
            "accountSession/list",
            "accountSession/switch",
            "accountSession/list",
            "thread/resume"
        ]
    );
    assert!(!fact.to_string().contains("never-publish"));
    assert!(matches!(
        command(
            &f,
            "_account.fence",
            json!({"members":[{"session":"thread-a","epoch":"epoch-a"}]})
        )
        .await
        .unwrap(),
        Outcome::Rejected { .. }
    ));
}

#[tokio::test]
async fn active_turn_is_fenced_but_switch_waits_for_native_terminal_evidence() {
    let f = fake("ok").await;
    {
        let mut e = f.runtime.slots[0].endpoint.lock().unwrap();
        e.state = "running".into();
        e.run = Some("turn-a".into());
    }
    fence(&f).await;
    assert!(matches!(
        command(&f, "_account.switch", target()).await.unwrap(),
        Outcome::Rejected { .. }
    ));
    assert!(f.methods.lock().unwrap().is_empty());
    f.runtime.incoming(Incoming::Notification {
        method: "turn/completed".into(),
        params: json!({"threadId":"thread-a","turn":{"id":"turn-a","status":"failed"}}),
    });
    assert!(matches!(
        command(&f, "_account.switch", target()).await.unwrap(),
        Outcome::Observed(_)
    ));
}

#[tokio::test]
async fn browser_or_wrong_readback_stays_fenced_and_consumes_switch_without_replay() {
    for mode in ["browser", "wrong_readback"] {
        let f = fake(mode).await;
        fence(&f).await;
        assert!(command(&f, "_account.switch", target()).await.is_err());
        let count = f.methods.lock().unwrap().len();
        assert!(matches!(
            command(&f, "_account.switch", target()).await.unwrap(),
            Outcome::Rejected { .. }
        ));
        assert!(matches!(
            command(&f, "_account.release", json!({})).await.unwrap(),
            Outcome::Rejected { .. }
        ));
        assert_eq!(f.methods.lock().unwrap().len(), count);
    }
}

#[tokio::test]
async fn already_active_host_still_performs_its_own_switch_and_exact_resume() {
    let f = fake("already_active").await;
    fence(&f).await;
    assert!(matches!(
        command(&f, "_account.switch", target()).await.unwrap(),
        Outcome::Observed(_)
    ));
    assert!(matches!(
        command(&f, "_account.resume", json!({})).await.unwrap(),
        Outcome::Observed(_)
    ));
    assert!(matches!(
        command(&f, "_account.release", json!({})).await.unwrap(),
        Outcome::Observed(_)
    ));
    assert_eq!(
        *f.methods.lock().unwrap(),
        [
            "accountSession/list",
            "accountSession/switch",
            "accountSession/list",
            "thread/resume"
        ]
    );
}

#[tokio::test]
async fn auth_change_after_fence_invalidates_the_old_source_before_any_switch() {
    let f = fake("ok").await;
    fence(&f).await;
    f.runtime.incoming(Incoming::Notification {
        method: "account/updated".into(),
        params: json!({"authMode":"chatgpt"}),
    });
    assert!(matches!(
        command(&f, "_account.switch", target()).await.unwrap(),
        Outcome::Rejected { .. }
    ));
    assert!(
        !f.methods
            .lock()
            .unwrap()
            .iter()
            .any(|m| m == "accountSession/switch")
    );
    assert!(matches!(
        command(&f, "_account.abort", json!({})).await.unwrap(),
        Outcome::Observed(_)
    ));
}

#[tokio::test]
async fn foreign_resume_identity_is_not_a_continued_conversation() {
    let f = fake("wrong_thread").await;
    fence(&f).await;
    command(&f, "_account.switch", target()).await.unwrap();
    assert!(command(&f, "_account.resume", json!({})).await.is_err());
    assert!(matches!(
        command(&f, "_account.release", json!({})).await.unwrap(),
        Outcome::Rejected { .. }
    ));
    assert_eq!(
        f.runtime.fact("endpoint-1").unwrap()["data"]["agent"]["session"]["id"],
        "thread-a"
    );
}

#[tokio::test]
async fn multi_slot_host_requires_every_thread_witness_and_fences_all_admissions() {
    let (client, _server) = tokio::io::duplex(1024);
    let (read, write) = tokio::io::split(client);
    let (peer, _) = Peer::spawn(read, write);
    let runtime = Runtime::new(peer, 2, "inc".into(), None);
    runtime.set_cutover_setup("shared").unwrap();
    for (i, slot) in runtime.slots.iter().enumerate() {
        slot.endpoint
            .lock()
            .unwrap()
            .set_session(
                &json!({"id":format!("thread-{i}"),"status":{"type":"idle"}}),
                format!("epoch-{i}"),
            )
            .unwrap();
    }
    let call = |members: Value| json!({"v":0,"name":"_account.fence","args":{"operation":"group","setup_id":"shared","members":members}});
    let deadline = Instant::now() + Duration::from_secs(1);
    let missing = runtime
        .invoke(
            "endpoint-1",
            "inc",
            "agent.command",
            call(json!([{"session":"thread-0","epoch":"epoch-0"}])),
            deadline,
        )
        .await
        .unwrap();
    assert!(matches!(missing, Outcome::Rejected { .. }));
    assert!(
        runtime
            .slots
            .iter()
            .all(|s| s.admission.clone().try_lock_owned().is_ok())
    );
    let all=runtime.invoke("endpoint-1","inc","agent.command",call(json!([{"session":"thread-0","epoch":"epoch-0"},{"session":"thread-1","epoch":"epoch-1"}])),deadline).await.unwrap();
    assert!(matches!(all, Outcome::Observed(_)));
    assert!(
        runtime
            .slots
            .iter()
            .all(|s| s.admission.clone().try_lock_owned().is_err())
    );
    runtime.native.close();
}

#[tokio::test]
async fn abort_only_releases_local_fences_before_any_native_switch_attempt() {
    let f = fake("ok").await;
    fence(&f).await;
    assert!(matches!(
        command(&f, "_account.abort", json!({})).await.unwrap(),
        Outcome::Observed(_)
    ));
    assert!(
        f.runtime.slots[0]
            .admission
            .clone()
            .try_lock_owned()
            .is_ok()
    );
    assert!(f.methods.lock().unwrap().is_empty());
    assert!(matches!(
        command(
            &f,
            "_account.fence",
            json!({"members":[{"session":"thread-a","epoch":"epoch-a"}]})
        )
        .await
        .unwrap(),
        Outcome::Rejected { .. }
    ));
    let other = fake("ok").await;
    fence(&other).await;
    command(&other, "_account.switch", target()).await.unwrap();
    assert!(matches!(
        command(&other, "_account.abort", json!({})).await.unwrap(),
        Outcome::Rejected { .. }
    ));
    assert!(
        other.runtime.slots[0]
            .admission
            .clone()
            .try_lock_owned()
            .is_err()
    );
}

#[tokio::test]
async fn newer_native_login_change_invalidates_projection_and_blocks_resume() {
    let f = fake("ok").await;
    fence(&f).await;
    command(&f, "_account.switch", target()).await.unwrap();
    f.foreign.store(true, Ordering::Release);
    f.runtime.incoming(Incoming::Notification {
        method: "account/updated".into(),
        params: json!({"authMode":"chatgpt"}),
    });
    assert!(
        f.runtime.fact("endpoint-1").unwrap()["data"]["agent"]
            .get("account")
            .is_none()
    );
    assert!(command(&f, "_account.resume", json!({})).await.is_err());
    assert!(
        !f.methods
            .lock()
            .unwrap()
            .iter()
            .any(|m| m == "thread/resume")
    );
}

#[tokio::test]
async fn queued_foreign_c_during_b_readback_is_not_b_confirmation() {
    let f = fake("foreign_during_readback").await;
    fence(&f).await;
    assert!(
        command(&f, "_account.switch", target()).await.is_err(),
        "a stale B readback must not bless C's newer auth revision"
    );
    assert!(
        f.runtime.fact("endpoint-1").unwrap()["data"]["agent"]
            .get("account")
            .is_none()
    );
    assert!(
        f.runtime.slots[0]
            .admission
            .clone()
            .try_lock_owned()
            .is_err()
    );
    assert!(matches!(
        command(&f, "_account.resume", json!({})).await.unwrap(),
        Outcome::Rejected { .. }
    ));
    assert!(
        !f.methods
            .lock()
            .unwrap()
            .iter()
            .any(|m| m == "thread/resume" || m == "turn/start")
    );
}

#[tokio::test]
async fn expected_shared_target_metadata_change_is_reconfirmed_not_false_foreign_failure() {
    let f = fake("ok").await;
    fence(&f).await;
    command(&f, "_account.switch", target()).await.unwrap();
    f.runtime.incoming(Incoming::Notification {
        method: "account/updated".into(),
        params: json!({"authMode":"chatgpt"}),
    });
    assert!(matches!(
        command(&f, "_account.resume", json!({})).await.unwrap(),
        Outcome::Observed(_)
    ));
    assert!(matches!(
        command(&f, "_account.release", json!({})).await.unwrap(),
        Outcome::Observed(_)
    ));
    assert_eq!(
        f.runtime.fact("endpoint-1").unwrap()["data"]["agent"]["account"]["id"],
        "b"
    );
    assert_eq!(
        f.methods
            .lock()
            .unwrap()
            .iter()
            .filter(|m| m.as_str() == "accountSession/switch")
            .count(),
        1
    );
}

#[tokio::test]
async fn saved_login_projection_contains_only_opaque_refs_and_active_flag() {
    let f = fake("ok").await;
    let Outcome::Observed(Some(result)) = command(&f, "_account.list", json!({})).await.unwrap()
    else {
        panic!()
    };
    assert_eq!(result["logins"].as_array().unwrap().len(), 3);
    assert!(!result.to_string().contains("never-publish"));
}
