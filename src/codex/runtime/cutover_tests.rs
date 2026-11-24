//! Fake JSONL Codex only. No installed runtime, account/profile reads or model calls.
use super::*;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

struct Fake {
    runtime: Arc<Runtime>,
    methods: Arc<Mutex<Vec<String>>>,
    foreign: Arc<AtomicBool>,
    active_b: Arc<AtomicBool>,
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
    let active_b = Arc::new(AtomicBool::new(false));
    let native_b = active_b.clone();
    let server = tokio::spawn(async move {
        let (read, mut write) = tokio::io::split(server);
        let mut lines = BufReader::new(read).lines();
        let mut switched = mode == "already_active";
        let mut switched_c = false;
        while let Ok(Some(line)) = lines.next_line().await {
            let request: Value = serde_json::from_str(&line).unwrap();
            let method = request["method"].as_str().unwrap();
            calls.lock().unwrap().push(method.to_owned());
            let active = if switched_c {
                "saved-c"
            } else if switched || native_b.load(Ordering::Acquire) {
                "saved-b"
            } else {
                "saved-a"
            };
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
                    let c =
                        mode.starts_with("quota_") && request["params"]["sessionId"] == "saved-c";
                    assert_eq!(
                        request["params"],
                        if c {
                            json!({"sessionId":"saved-c","accountId":"workspace-c","allowBrowserLogin":false})
                        } else {
                            json!({"sessionId":"saved-b","accountId":"workspace-b","allowBrowserLogin":false})
                        }
                    );
                    switched_c = c;
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
                        sessions(if switched_c { "saved-c" } else { "saved-b" })
                    }
                }
                "turn/start" => {
                    if mode == "unknown_admission" {
                        write.write_all(format!("{}\n",json!({"id":request["id"],"error":{"code":-32000,"message":"unknown native admission"}})).as_bytes()).await.unwrap();
                        continue;
                    }
                    assert!(matches!(
                        mode,
                        "admitted_unstarted" | "quota_resume_stale" | "quota_ready_send"
                    ));
                    json!({"turn":{"id":"not-yet-started","status":"inProgress"}})
                }
                "account/rateLimits/read" => {
                    if mode == "quota_fail" {
                        write.write_all(format!("{}\n",json!({"id":request["id"],"error":{"code":-32601,"message":"quota read unavailable"}})).as_bytes()).await.unwrap();
                        continue;
                    }
                    if mode == "quota_newer_false" || mode == "quota_newer_null" {
                        write.write_all(format!("{}\n",json!({"method":"account/rateLimits/updated","params":{"ordinaryUsageAllowed":if mode=="quota_newer_false" {Some(false)}else{None},"accountId":"workspace-b"}})).as_bytes()).await.unwrap();
                    }
                    json!({"ordinaryUsageAllowed":if mode=="quota_walk" {Some(switched_c)}else if mode=="quota_wrong_scope" || mode.starts_with("quota_newer_") || mode=="quota_resume_stale" || mode=="quota_ready_send" {Some(true)}else{None},"accountId":if mode=="quota_wrong_scope" {"workspace-a"}else if switched_c {"workspace-c"}else{"workspace-b"}})
                }
                "thread/resume" => {
                    if mode == "quota_resume_stale" {
                        write.write_all(format!("{}\n",json!({"method":"account/rateLimits/updated","params":{"accountId":"workspace-b","ordinaryUsageAllowed":false}})).as_bytes()).await.unwrap();
                    }
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
        active_b,
        server,
        drain,
    }
}
async fn command(f: &Fake, name: &str, extra: Value) -> Result<Outcome, ProviderError> {
    if name == "_account.switch" && extra.get("prepared_ticket").is_none() {
        let prepared = raw_command(f, "_account.prepare_switch", extra.clone()).await?;
        let Outcome::Observed(Some(data)) = &prepared else {
            return Ok(prepared);
        };
        let mut commit = extra;
        commit["prepared_ticket"] = data["prepared_ticket"].clone();
        return raw_command(f, name, commit).await;
    }
    raw_command(f, name, extra).await
}

async fn raw_command(f: &Fake, name: &str, extra: Value) -> Result<Outcome, ProviderError> {
    let mut p =
        json!({"operation":"claimed-once","setup_id":"shared","authorized_targets":[target()]});
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

fn limit(f: &Fake, turn: &str) {
    let mut endpoint = f.runtime.slots[0].endpoint.lock().unwrap();
    endpoint.notification("error",&json!({"threadId":"thread-a","turnId":turn,"willRetry":false,"error":{"codexErrorInfo":"usageLimitExceeded"}}));
}

#[tokio::test]
async fn auth_change_cannot_relabel_a_limit_as_verified_b() {
    let f = fake("ok").await;
    let source = json!({"login_ref":{"sessionId":"saved-a","accountId":"workspace-a"},"account":{"id":"a","label":"A","kind":"subscription"}});
    command(&f, "_account.bind", source).await.unwrap();
    limit(&f, "limit-under-a");
    assert_eq!(
        f.runtime.fact("endpoint-1").unwrap()["data"]["agent"]["availability"]["native"]["turnId"],
        "limit-under-a"
    );
    f.active_b.store(true, Ordering::Release);
    f.runtime.incoming(Incoming::Notification {
        method: "account/updated".into(),
        params: json!({"authMode":"chatgpt"}),
    });
    command(&f, "_account.bind", target()).await.unwrap();
    let mut facts = f.runtime.facts();
    facts.push(f.runtime.fact("endpoint-1").unwrap());
    facts.extend(f.runtime.take_dirty_facts());
    for fact in facts {
        assert_eq!(fact["data"]["agent"]["account"]["id"], "b");
        assert!(
            fact["data"]["agent"]["availability"].is_null(),
            "A quota is not B quota or evidence of recovery"
        );
    }
    limit(&f, "limit-under-a");
    assert!(
        f.runtime.fact("endpoint-1").unwrap()["data"]["agent"]["availability"].is_null(),
        "late A error is retired, not B evidence"
    );
    f.runtime.slots[0].endpoint.lock().unwrap().notification(
        "turn/started",
        &json!({"threadId":"thread-a","turn":{"id":"limit-under-b","status":"inProgress"}}),
    );
    limit(&f, "limit-under-b");
    assert_eq!(
        f.runtime.fact("endpoint-1").unwrap()["data"]["agent"]["availability"]["native"]["turnId"],
        "limit-under-b"
    );
    limit(&f, "limit-under-a");
    assert_eq!(
        f.runtime.fact("endpoint-1").unwrap()["data"]["agent"]["availability"]["native"]["turnId"],
        "limit-under-b"
    );
}

#[tokio::test]
async fn same_auth_readonly_bind_preserves_current_native_limit() {
    let f = fake("ok").await;
    let source = json!({"login_ref":{"sessionId":"saved-a","accountId":"workspace-a"},"account":{"id":"a","label":"A","kind":"subscription"}});
    command(&f, "_account.bind", source.clone()).await.unwrap();
    limit(&f, "limit-under-a");
    command(&f, "_account.bind", source).await.unwrap();
    let fact = f.runtime.fact("endpoint-1").unwrap();
    assert_eq!(fact["data"]["agent"]["account"]["id"], "a");
    assert_eq!(
        fact["data"]["agent"]["availability"]["native"]["turnId"],
        "limit-under-a"
    );
}

#[tokio::test]
async fn owned_admitted_unstarted_and_unknown_inputs_prevent_fencing() {
    for mode in ["admitted_unstarted", "unknown_admission"] {
        let f = fake(mode).await;
        let result=f.runtime.invoke("endpoint-1","inc","agent.send",json!({"v":0,"mode":"admit","delivery":"own-input","origin":{"principal":"p","actor":"a","kind":"agent"},"content":[{"type":"text","text":"new owned input"}]}),Instant::now()+Duration::from_secs(2)).await;
        if mode == "admitted_unstarted" {
            assert!(matches!(result.unwrap(), Outcome::Accepted(_)));
        } else {
            assert!(result.is_err());
        }
        tokio::task::yield_now().await;
        let result = command(
            &f,
            "_account.fence",
            json!({"members":[{"session":"thread-a","epoch":"epoch-a"}]}),
        )
        .await
        .unwrap();
        let Outcome::Observed(Some(value)) = result else {
            panic!("expected local fence")
        };
        assert_eq!(
            value["quiescent"], false,
            "{mode}: native ACK is not turnStarted or terminal"
        );
        assert!(matches!(
            command(&f, "_account.switch", target()).await.unwrap(),
            Outcome::Rejected { .. }
        ));
        let fact = f.runtime.fact("endpoint-1").unwrap();
        assert_eq!(
            fact["data"]["agent"]["_codex"]["input_ledger"]["source"],
            "vqro-ledger"
        );
        assert_eq!(
            fact["data"]["agent"]["_codex"]["input_ledger"]["pending"],
            1
        );
        assert!(fact["data"]["agent"]["queue"].is_null());
        f.runtime.incoming(Incoming::Notification {
            method: "turn/started".into(),
            params: json!({"threadId":"thread-a","turn":{"id":"not-yet-started"}}),
        });
        f.runtime.incoming(Incoming::Notification {method:"turn/completed".into(),params:json!({"threadId":"thread-a","turn":{"id":"not-yet-started","status":"completed"}})});
        let Outcome::Observed(Some(status)) =
            command(&f, "_account.status", json!({})).await.unwrap()
        else {
            panic!("status")
        };
        assert_eq!(
            status["quiescent"],
            mode == "admitted_unstarted",
            "unknown start has no run correlation; never guess or replay"
        );
        assert_eq!(
            f.methods
                .lock()
                .unwrap()
                .iter()
                .filter(|m| m.as_str() == "turn/start")
                .count(),
            1
        );
        assert!(
            !f.methods
                .lock()
                .unwrap()
                .iter()
                .any(|m| m == "accountSession/switch")
        );
    }
}

#[tokio::test]
async fn own_ledger_handles_early_events_unknown_steers_and_receipt_eviction() {
    let f = fake("ok").await;
    let mut e = f.runtime.slots[0].endpoint.lock().unwrap();
    e.reserve_owned_input("early", None).unwrap();
    e.observe_owned_input_run("early-run", false);
    e.finish_owned_input("early", "admitted", Some(&json!({"run":"early-run"})));
    assert!(e.own_inputs_quiescent());
    e.reserve_owned_input("steer", Some("existing-run".into()))
        .unwrap();
    e.finish_owned_input("steer", "unknown", None);
    e.observe_owned_input_run("existing-run", false);
    assert!(!e.own_inputs_quiescent());
    e.observe_owned_input_run("other-run", true);
    assert!(!e.own_inputs_quiescent());
    e.observe_owned_input_run("existing-run", true);
    assert!(e.own_inputs_quiescent());
    e.reserve_owned_input("unresolved", None).unwrap();
    e.finish_owned_input("unresolved", "unknown", None);
    e.record_receipt("unresolved", "unknown", None, &json!({}));
    for n in 0..=crate::codex::RECEIPTS {
        e.record_receipt(&format!("done-{n}"), "rejected", None, &json!({}));
    }
    assert!(!e.receipts.contains_key("unresolved"));
    assert_eq!(e.input_ledger()["pending"], 1);
    assert_eq!(e.input_ledger()["unknown"], 1);
    assert!(e.may_switch(&json!({"v":0})).is_err());
}

#[tokio::test]
async fn own_input_ack_after_auth_change_cannot_establish_b_quota_from_a() {
    let f = fake("ok").await;
    f.runtime.slots[0]
        .endpoint
        .lock()
        .unwrap()
        .reserve_owned_input("inflight-a", None)
        .unwrap();
    f.active_b.store(true, Ordering::Release);
    f.runtime.incoming(Incoming::Notification {
        method: "account/updated".into(),
        params: json!({"authMode":"chatgpt"}),
    });
    command(&f, "_account.bind", target()).await.unwrap();
    f.runtime.incoming(Incoming::Notification {
        method: "turn/started".into(),
        params: json!({"threadId":"thread-a","turn":{"id":"late-a"}}),
    });
    limit(&f, "late-a");
    assert!(f.runtime.fact("endpoint-1").unwrap()["data"]["agent"]["availability"].is_null());
    f.runtime.slots[0]
        .endpoint
        .lock()
        .unwrap()
        .finish_owned_input("inflight-a", "admitted", Some(&json!({"run":"late-a"})));
    f.runtime.incoming(Incoming::Notification {
        method: "turn/completed".into(),
        params: json!({"threadId":"thread-a","turn":{"id":"late-a","status":"failed"}}),
    });
    f.runtime.incoming(Incoming::Notification {
        method: "turn/started".into(),
        params: json!({"threadId":"thread-a","turn":{"id":"fresh-b"}}),
    });
    limit(&f, "late-a");
    assert!(f.runtime.fact("endpoint-1").unwrap()["data"]["agent"]["availability"].is_null());
    limit(&f, "fresh-b");
    let fact = f.runtime.fact("endpoint-1").unwrap();
    assert_eq!(fact["data"]["agent"]["account"]["id"], "b");
    assert_eq!(
        fact["data"]["agent"]["availability"]["native"]["turnId"],
        "fresh-b"
    );
}

fn target_c() -> Value {
    json!({"login_ref":{"sessionId":"saved-c","accountId":"workspace-c"},"account":{"id":"c","label":"C","kind":"subscription"}})
}

#[tokio::test]
async fn quota_walk_requires_blocked_inspection_and_exact_authorized_prefix() {
    for mode in [
        "quota_walk",
        "quota_unknown",
        "quota_fail",
        "quota_wrong_scope",
        "quota_newer_false",
        "quota_newer_null",
    ] {
        let f = fake(mode).await;
        command(&f,"_account.fence",json!({"members":[{"session":"thread-a","epoch":"epoch-a"}],"authorized_targets":[target(),target_c()]})).await.unwrap();
        command(&f, "_account.switch", target()).await.unwrap();
        assert!(matches!(
            command(&f, "_account.switch", target_c()).await.unwrap(),
            Outcome::Rejected { .. }
        ));
        let Outcome::Observed(Some(quota)) =
            command(&f, "_account.inspect", json!({})).await.unwrap()
        else {
            panic!("quota read")
        };
        assert_eq!(
            quota["ordinary_usage_allowed"],
            if mode == "quota_walk" {
                json!(false)
            } else {
                Value::Null
            }
        );
        if mode == "quota_walk" {
            assert!(matches!(
                command(&f, "_account.switch", target()).await.unwrap(),
                Outcome::Rejected { .. }
            ));
            command(&f, "_account.switch", target_c()).await.unwrap();
            let Outcome::Observed(Some(quota)) =
                command(&f, "_account.inspect", json!({})).await.unwrap()
            else {
                panic!("Cquota")
            };
            assert_eq!(quota["ordinary_usage_allowed"], true);
        } else {
            assert!(matches!(
                command(&f, "_account.switch", target_c()).await.unwrap(),
                Outcome::Rejected { .. }
            ));
        }
        if mode == "quota_fail" || mode == "quota_walk" {
            command(
                &f,
                "_account.restore",
                json!({"accounts":[target(),target_c(),restore_accounts()["accounts"][1].clone()]}),
            )
            .await
            .unwrap();
        } else {
            command(&f, "_account.resume", json!({})).await.unwrap();
        }
        command(&f, "_account.release", json!({})).await.unwrap();
        let methods = f.methods.lock().unwrap();
        assert_eq!(
            methods
                .iter()
                .filter(|m| m.as_str() == "accountSession/switch")
                .count(),
            if mode == "quota_walk" { 2 } else { 1 }
        );
        assert!(
            !methods
                .iter()
                .any(|m| m == "turn/start" || m == "turn/steer")
        );
    }
}

#[tokio::test]
async fn ordered_native_plan_rejects_duplicates_overflow_and_unreserved_first_target() {
    let f = fake("quota_walk").await;
    for plan in [
        json!([]),
        json!([target(), target()]),
        json!([target(), target_c(), target(), target_c()]),
    ] {
        assert!(matches!(command(&f,"_account.fence",json!({"members":[{"session":"thread-a","epoch":"epoch-a"}],"authorized_targets":plan})).await.unwrap(),Outcome::Rejected {..}));
    }
    fence(&f).await;
    assert!(matches!(
        command(&f, "_account.switch", target_c()).await.unwrap(),
        Outcome::Rejected { .. }
    ));
    assert!(
        !f.methods
            .lock()
            .unwrap()
            .iter()
            .any(|m| m == "accountSession/switch")
    );
    command(&f, "_account.switch", target()).await.unwrap();
}

#[tokio::test]
async fn inspected_true_snapshot_does_not_authorize_auto_after_resume_negative() {
    let f = fake("quota_resume_stale").await;
    fence(&f).await;
    command(&f, "_account.switch", target()).await.unwrap();
    let Outcome::Observed(Some(quota)) = command(&f, "_account.inspect", json!({})).await.unwrap()
    else {
        panic!("inspect")
    };
    assert_eq!(quota["ordinary_usage_allowed"], true);
    let positive_caps =
        f.runtime.fact("endpoint-1").unwrap()["data"]["agent"]["caps"]["rev"].clone();
    command(&f, "_account.resume", json!({})).await.unwrap();
    command(&f, "_account.release", json!({})).await.unwrap();
    let fact = f.runtime.fact("endpoint-1").unwrap();
    let result=f.runtime.invoke("endpoint-1","inc","agent.send",json!({"v":0,"mode":"admit","delivery":"automatic-new-turn","caps_rev":positive_caps,"epoch":fact["data"]["agent"]["epoch"],"guard":{"session":"thread-a"},"origin":{"principal":"p","actor":"vqro.accounts","kind":"tool"},"content":[{"type":"text","text":"new continuation"}]}),Instant::now()+Duration::from_secs(2)).await.unwrap();
    assert!(
        matches!(result, Outcome::Rejected { .. }),
        "processed newer negative must invalidate the positive AUTO snapshot before native input"
    );
}

async fn guarded_input(f: &Fake, delivery: &str, caps: Value) -> Outcome {
    let fact = f.runtime.fact("endpoint-1").unwrap();
    f.runtime.invoke("endpoint-1","inc","agent.send",json!({"v":0,"mode":"admit","delivery":delivery,"caps_rev":caps,"epoch":fact["data"]["agent"]["epoch"],"guard":{"session":"thread-a"},"origin":{"principal":"p","actor":"a","kind":"tool"},"content":[{"type":"text","text":"new input"}]}),Instant::now()+Duration::from_secs(2)).await.unwrap()
}

#[tokio::test(start_paused = true)]
async fn released_permission_rev_guards_auto_but_not_ordinary_human_input() {
    for changed in ["none", "false", "null", "auth", "expiry", "model"] {
        let f = fake("quota_ready_send").await;
        fence(&f).await;
        command(&f, "_account.switch", target()).await.unwrap();
        command(&f, "_account.inspect", json!({})).await.unwrap();
        command(&f, "_account.resume", json!({})).await.unwrap();
        let Outcome::Observed(Some(released)) =
            command(&f, "_account.release", json!({})).await.unwrap()
        else {
            panic!("release")
        };
        let caps = released["admissions"][0]["caps_rev"].clone();
        assert!(caps.is_u64());
        match changed {
            "false" | "null" => f.runtime.incoming(Incoming::Notification {
                method: "account/rateLimits/updated".into(),
                params: json!({"ordinaryUsageAllowed":if changed=="false" {Some(false)}else{None}}),
            }),
            "auth" => f.runtime.incoming(Incoming::Notification {
                method: "account/updated".into(),
                params: json!({"authMode":"chatgpt"}),
            }),
            "expiry" => tokio::time::advance(Duration::from_secs(31)).await,
            "model" => f.runtime.slots[0].endpoint.lock().unwrap().config_rev += 1,
            _ => {}
        }
        let outcome = guarded_input(&f, "auto", caps).await;
        if changed == "none" {
            assert!(matches!(outcome, Outcome::Accepted(_)));
        } else {
            assert!(matches!(outcome, Outcome::Rejected { .. }), "{changed}");
            let fact = f.runtime.fact("endpoint-1").unwrap();
            assert!(fact["data"]["agent"]["caps"]["agent.send"]["native_included_usage"].is_null());
            assert!(
                matches!(
                    guarded_input(&f, "human", fact["data"]["agent"]["caps"]["rev"].clone()).await,
                    Outcome::Accepted(_)
                ),
                "ordinary input remains available after {changed}"
            );
        }
    }
}

#[tokio::test]
async fn prepared_commit_has_no_later_discovery_and_is_consumed_once() {
    let f = fake("ok").await;
    fence(&f).await;
    assert!(matches!(
        raw_command(&f, "_account.switch", target()).await.unwrap(),
        Outcome::Rejected { .. }
    ));
    let Outcome::Observed(Some(data)) = raw_command(&f, "_account.prepare_switch", target())
        .await
        .unwrap()
    else {
        panic!("prepare")
    };
    let before = f.methods.lock().unwrap().len();
    assert!(matches!(
        raw_command(&f, "_account.switch", json!({"prepared_ticket":"foreign"}))
            .await
            .unwrap(),
        Outcome::Rejected { .. }
    ));
    assert_eq!(f.methods.lock().unwrap().len(), before);
    raw_command(
        &f,
        "_account.switch",
        json!({"prepared_ticket":data["prepared_ticket"]}),
    )
    .await
    .unwrap();
    assert_eq!(
        f.methods.lock().unwrap()[before],
        "accountSession/switch",
        "no discovery after callback final authority"
    );
    let count = f
        .methods
        .lock()
        .unwrap()
        .iter()
        .filter(|m| m.as_str() == "accountSession/switch")
        .count();
    assert!(matches!(
        raw_command(
            &f,
            "_account.switch",
            json!({"prepared_ticket":data["prepared_ticket"]})
        )
        .await
        .unwrap(),
        Outcome::Rejected { .. }
    ));
    assert_eq!(
        f.methods
            .lock()
            .unwrap()
            .iter()
            .filter(|m| m.as_str() == "accountSession/switch")
            .count(),
        count
    );
}

#[tokio::test]
async fn auth_revision_change_after_prepare_rejects_commit_before_credentials() {
    let f = fake("ok").await;
    fence(&f).await;
    let Outcome::Observed(Some(data)) = raw_command(&f, "_account.prepare_switch", target())
        .await
        .unwrap()
    else {
        panic!("prepare")
    };
    f.runtime.incoming(Incoming::Notification {
        method: "account/updated".into(),
        params: json!({"authMode":"chatgpt"}),
    });
    assert!(matches!(
        raw_command(
            &f,
            "_account.switch",
            json!({"prepared_ticket":data["prepared_ticket"]})
        )
        .await
        .unwrap(),
        Outcome::Rejected { .. }
    ));
    assert!(
        !f.methods
            .lock()
            .unwrap()
            .iter()
            .any(|m| m == "accountSession/switch")
    );
    command(&f, "_account.abort", json!({})).await.unwrap();
}

#[tokio::test]
async fn processed_quota_change_before_spawned_input_worker_rejects_prepared_caps() {
    use std::future::Future;
    let f = fake("quota_ready_send").await;
    fence(&f).await;
    command(&f, "_account.switch", target()).await.unwrap();
    command(&f, "_account.inspect", json!({})).await.unwrap();
    command(&f, "_account.resume", json!({})).await.unwrap();
    let Outcome::Observed(Some(data)) = command(&f, "_account.release", json!({})).await.unwrap()
    else {
        panic!("release")
    };
    let future = guarded_input(&f, "auto-worker", data["admissions"][0]["caps_rev"].clone());
    tokio::pin!(future);
    let mut context = std::task::Context::from_waker(std::task::Waker::noop());
    assert!(future.as_mut().poll(&mut context).is_pending());
    assert!(
        f.runtime.slots[0].admission.try_lock().is_err(),
        "prepared worker owns admission"
    );
    f.runtime.incoming(Incoming::Notification {
        method: "account/rateLimits/updated".into(),
        params: json!({"ordinaryUsageAllowed":false}),
    });
    assert!(
        matches!(future.await, Outcome::Rejected { .. }),
        "worker must recheck captured guard before native input"
    );
    assert!(!f.methods.lock().unwrap().iter().any(|m| m == "turn/start"));
}

#[tokio::test]
async fn worker_rejects_global_revision_even_before_endpoint_invalidation_runs() {
    let f = fake("quota_ready_send").await;
    fence(&f).await;
    command(&f, "_account.switch", target()).await.unwrap();
    command(&f, "_account.inspect", json!({})).await.unwrap();
    command(&f, "_account.resume", json!({})).await.unwrap();
    let Outcome::Observed(Some(data)) = command(&f, "_account.release", json!({})).await.unwrap()
    else {
        panic!("release")
    };
    f.runtime.account_revision.fetch_add(1, Ordering::AcqRel); // Incoming increments before taking endpoint locks.
    assert!(matches!(
        guarded_input(
            &f,
            "auto-counter",
            data["admissions"][0]["caps_rev"].clone()
        )
        .await,
        Outcome::Rejected { .. }
    ));
    assert!(!f.methods.lock().unwrap().iter().any(|m| m == "turn/start"));
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
    let call = |members: Value| json!({"v":0,"name":"_account.fence","args":{"operation":"group","setup_id":"shared","members":members,"authorized_targets":[target()]}});
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
