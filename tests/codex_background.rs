use serde_json::json;
use vqro::codex::Endpoint;

#[test]
fn unrelated_foreground_completion_never_answers_a_pending_native_request() {
    let mut e = Endpoint::new("slot".into(), "i".into(), "host".into(), "e1".into());
    e.set_session(&json!({"id":"s","status":{"type":"idle"}}), "e2".into())
        .unwrap();
    e.notification(
        "turn/started",
        &json!({"threadId":"s","turn":{"id":"foreground"}}),
    );
    let request = e
        .request(
            json!(7),
            "item/tool/requestUserInput",
            &json!({
                "threadId":"s", "turnId":"background", "itemId":"b", "isBlocking":false,
                "questions":[{"id":"q","header":"Question","question":"Text?","options":null}]
            }),
        )
        .unwrap();
    e.notification(
        "turn/completed",
        &json!({"threadId":"s","turn":{"id":"foreground","status":"completed"}}),
    );
    assert_eq!(
        e.facts()["data"]["agent"]["requests"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let answer = e
        .prepare_answer(
            &json!({"v":0,"epoch":"e2","request":request.id,"revision":1,
        "response":{"action":"accept","content":{"q":[""]}}}),
        )
        .unwrap();
    assert_eq!(
        answer.result.unwrap(),
        json!({"answers":{"q":{"answers":[""]}}})
    );
}

#[test]
fn explicit_native_resolution_revokes_an_old_request_without_completing_the_agent() {
    let mut e = Endpoint::new("slot".into(), "i".into(), "host".into(), "e1".into());
    e.set_session(
        &json!({"id":"s","status":{"type":"active","activeFlags":[]}}),
        "e2".into(),
    )
    .unwrap();
    let request = e
        .request(
            json!(7),
            "item/fileChange/requestApproval",
            &json!({"threadId":"s","turnId":"t","itemId":"b"}),
        )
        .unwrap();
    e.notification(
        "serverRequest/resolved",
        &json!({"threadId":"s","requestId":7}),
    );
    assert!(e.prepare_answer(&json!({"v":0,"epoch":"e2","request":request.id,"revision":1,"response":{"choice":"accept"}})).is_err());
    assert_eq!(e.state, "running");
    assert_eq!(e.lifecycle, "live");
}

#[test]
fn pending_requests_stay_within_the_facts_bound_and_host_exit_retires_them() {
    let mut e = Endpoint::new("slot".into(), "i".into(), "host".into(), "e1".into());
    e.set_session(&json!({"id":"s","status":{"type":"idle"}}), "e2".into())
        .unwrap();
    for i in 0..100 {
        let _ = e.request(
            json!(i),
            "item/commandExecution/requestApproval",
            &json!({
                "threadId":"s","itemId":"i","turnId":"t","command":"x".repeat(512)
            }),
        );
    }
    assert!(serde_json::to_vec(&e.facts()["data"]).unwrap().len() < 65536);
    let changes = e.host_exited("native exit");
    assert!(
        changes
            .iter()
            .filter(|c| c.kind == "request")
            .all(|c| c.value["status"] == "cancelled")
    );
    assert!(
        e.facts()["data"]["agent"]["requests"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}
