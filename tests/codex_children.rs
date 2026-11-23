//! Native observation only; no model, login, or child-control calls.
use serde_json::{Value, json};
use vqro::codex::{Change, Endpoint};

fn endpoint() -> Endpoint {
    let mut endpoint = Endpoint::new(
        "endpoint-1".into(),
        "inc".into(),
        "host".into(),
        "e1".into(),
    );
    endpoint
        .set_session(&json!({"id":"main","status":{"type":"idle"}}), "e2".into())
        .unwrap();
    endpoint
}
fn child(endpoint: &mut Endpoint, id: &str, parent: &str) -> Vec<Change> {
    endpoint.notification(
        "thread/started",
        &json!({"thread":{"id":id,"parentThreadId":parent,
        "agentNickname":"Ada","agentRole":"explorer","status":{"type":"idle"},
        "source":{"subAgent":{"thread_spawn":{"parent_thread_id":parent,"depth":1}}}}}),
    )
}
fn find<'a>(changes: &'a [Change], kind: &str) -> &'a Change {
    changes.iter().find(|change| change.kind == kind).unwrap()
}

#[test]
fn whole_plan_uses_generic_shape_and_turn_identity() {
    let mut e = endpoint();
    let a = e.notification("turn/plan/updated", &json!({"threadId":"main","turnId":"t1","explanation":"next",
        "plan":[{"step":"inspect","status":"inProgress"},{"step":"build","status":"newNativeState"}]}));
    let plan = find(&a, "plan");
    assert_eq!(plan.value["update"], "whole");
    assert_eq!(plan.value["source"], "native");
    assert_eq!(
        plan.value["items"][0],
        json!({"text":"inspect","status":"in_progress"})
    );
    assert_eq!(plan.value["items"][1]["status"], "unknown");
    assert_eq!(plan.value["items"][1]["native"]["status"], "newNativeState");
    assert_eq!(plan.value["explanation"], "next");
    let b = e.notification(
        "turn/plan/updated",
        &json!({"threadId":"main","turnId":"t2","plan":[]}),
    );
    assert_ne!(plan.id, find(&b, "plan").id);
    assert!(
        find(&b, "plan").value["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn native_child_lineage_and_unknown_status_are_honest() {
    let mut e = endpoint();
    let a = child(&mut e, "worker", "main");
    let entity = find(&a, "child");
    assert_eq!(entity.value["parent"], Value::Null);
    assert_eq!(entity.value["native"]["thread_id"], "worker");
    assert_eq!(entity.value["name"], "Ada");
    assert_eq!(entity.value["role"], "explorer");
    assert_eq!(entity.value["depth"], 1);
    assert_eq!(entity.value["items"], "scoped");
    assert!(entity.value.get("endpoint").is_none());
    assert_eq!(entity.value["status"], "waiting"); // idle child is not completed
    let changed = e.notification(
        "thread/status/changed",
        &json!({"threadId":"worker","status":{"type":"future"}}),
    );
    assert_eq!(find(&changed, "child").value["status"], "unknown");
    assert_eq!(find(&changed, "child").value["native"]["status"], "future");
    assert_eq!(e.state, "idle");
}

#[test]
fn scoped_child_messages_do_not_collide_with_main_or_sibling_ids() {
    let mut e = endpoint();
    let first = child(&mut e, "one", "main");
    child(&mut e, "two", "main");
    let item = json!({"id":"same","type":"agentMessage","text":"hello"});
    let main = e.notification("item/started", &json!({"threadId":"main","item":item}));
    let one = e.notification("item/started", &json!({"threadId":"one","item":item}));
    let two = e.notification("item/started", &json!({"threadId":"two","item":item}));
    assert_eq!(
        find(&one, "message").value["child"],
        find(&first, "child").id
    );
    assert_ne!(find(&main, "message").id, find(&one, "message").id);
    assert_ne!(find(&one, "message").id, find(&two, "message").id);
    let delta = e.notification(
        "item/agentMessage/delta",
        &json!({"threadId":"one","itemId":"same","delta":"!"}),
    );
    assert_eq!(find(&delta, "message").id, find(&one, "message").id);
    assert_eq!(find(&delta, "message").append.as_ref().unwrap().1, "!");
}

#[test]
fn descendant_and_session_fences_reject_unrelated_or_old_children() {
    let mut e = endpoint();
    let first = child(&mut e, "one", "main");
    let nested = child(&mut e, "nested", "one");
    assert_eq!(
        find(&nested, "child").value["parent"],
        find(&first, "child").id
    );
    assert!(child(&mut e, "stranger", "other").is_empty());
    e.set_session(&json!({"id":"new","status":{"type":"idle"}}), "e3".into())
        .unwrap();
    assert!(
        e.notification(
            "item/started",
            &json!({"threadId":"one","item":{"id":"m","type":"agentMessage","text":"late"}})
        )
        .is_empty()
    );
}

#[test]
fn collab_tool_completion_is_not_child_completion() {
    let mut e = endpoint();
    let result = e.notification("item/completed", &json!({"threadId":"main","item":{
        "id":"spawn","type":"collabAgentToolCall","tool":"spawnAgent","status":"completed",
        "senderThreadId":"main","receiverThreadIds":["worker"],"agentsStates":{"worker":{"status":"running"}}}}));
    assert_eq!(find(&result, "tool").value["status"], "completed");
    assert_eq!(find(&result, "child").value["status"], "running");
    assert_eq!(find(&result, "child").value["parent"], Value::Null);
    assert!(find(&result, "tool").value["native"].is_object());
}

#[test]
fn activity_has_native_child_id_not_a_fabricated_task() {
    let mut e = endpoint();
    let result = e.notification(
        "item/started",
        &json!({"threadId":"main","item":{
        "id":"activity","type":"subAgentActivity","kind":"started","agentThreadId":"worker",
        "agentPath":"/0/1","model":"native-model"}}),
    );
    let child = find(&result, "child");
    assert_eq!(child.value["native"]["thread_id"], "worker");
    assert_eq!(child.value["status"], "running");
    assert_eq!(child.value["model"]["id"], "native-model");
    assert!(child.value.get("parent").is_none()); // reference/activity is not lineage
    assert!(!result.iter().any(|c| c.kind == "task"));
}

#[test]
fn child_request_remains_an_ordinary_parent_stream_request() {
    let mut e = endpoint();
    let registered = child(&mut e, "worker", "main");
    let request = e
        .request(
            json!(71),
            "item/commandExecution/requestApproval",
            &json!({
        "threadId":"worker","turnId":"ct1","itemId":"cmd","command":"echo hi"}),
        )
        .unwrap();
    assert_eq!(request.value["child"], find(&registered, "child").id);
    let answered = e
        .prepare_answer(
            &json!({"v":0,"epoch":"e2","request":request.id,"revision":1,
        "response":{"choice":"decline"}}),
        )
        .unwrap();
    assert_eq!(answered.native_id, json!(71));
    assert_eq!(answered.result.unwrap()["decision"], "decline");
}

#[test]
fn child_usage_and_waiting_flags_do_not_mutate_parent_usage() {
    let mut e = endpoint();
    child(&mut e, "worker", "main");
    let parent_usage = e.facts()["data"]["agent"]["usage"].clone();
    let wait=e.notification("thread/status/changed",&json!({"threadId":"worker","status":{"type":"active","activeFlags":["waitingOnApproval"]}}));
    assert_eq!(find(&wait, "child").value["status"], "waiting");
    let usage=e.notification("thread/tokenUsage/updated",&json!({"threadId":"worker","tokenUsage":{"modelContextWindow":64000,"total":{"inputTokens":11,"outputTokens":5,"cachedInputTokens":3}}}));
    assert_eq!(
        find(&usage, "child").value["usage"]["tokens"],
        json!({"input":11,"output":5,"cache_read":3})
    );
    assert_eq!(e.facts()["data"]["agent"]["usage"], parent_usage);
}

#[test]
fn conflicting_or_cyclic_lineage_cannot_rebind_a_child() {
    let mut e = endpoint();
    let a = child(&mut e, "one", "main");
    child(&mut e, "two", "one");
    assert!(child(&mut e, "one", "two").is_empty());
    let state = e.notification(
        "thread/status/changed",
        &json!({"threadId":"one","status":{"type":"idle"}}),
    );
    assert_eq!(
        find(&state, "child").value["parent"],
        find(&a, "child").value["parent"]
    );
}

#[test]
fn child_request_resolution_is_explicit_and_session_switch_fences_answers() {
    let mut e = endpoint();
    child(&mut e, "worker", "main");
    let request = e
        .request(
            json!(44),
            "item/fileChange/requestApproval",
            &json!({"threadId":"worker","turnId":"t","itemId":"f"}),
        )
        .unwrap();
    let resolved = e.notification(
        "serverRequest/resolved",
        &json!({"threadId":"worker","requestId":44}),
    );
    assert_eq!(find(&resolved, "request").value["status"], "cancelled");
    assert!(find(&resolved, "request").value["child"].is_string());
    assert!(e.prepare_answer(&json!({"v":0,"epoch":"e2","request":request.id,"revision":1,"response":{"choice":"accept"}})).is_err());
    let another = e
        .request(
            json!(45),
            "item/fileChange/requestApproval",
            &json!({"threadId":"worker","itemId":"f"}),
        )
        .unwrap();
    e.set_session(&json!({"id":"new","status":{"type":"idle"}}), "e3".into())
        .unwrap();
    assert!(e.prepare_answer(&json!({"v":0,"epoch":"e3","request":another.id,"revision":1,"response":{"choice":"accept"}})).is_err());
}

#[test]
fn native_goals_are_distinct_from_plans_and_keep_limit_status() {
    let mut e = endpoint();
    let goal = e.notification(
        "thread/goal/updated",
        &json!({"threadId":"main","goal":{
        "threadId":"main","objective":"ship changes","status":"usageLimited","tokenBudget":1000,
        "tokensUsed":20,"timeUsedSeconds":7,"updatedAt":123}}),
    );
    let mapped = find(&goal, "goal");
    assert_eq!(mapped.id, "current");
    assert_eq!(mapped.value["status"], "blocked");
    assert_eq!(mapped.value["native"]["status"], "usageLimited");
    assert_eq!(mapped.value["objective"], "ship changes");
    assert_eq!(mapped.value["budget"]["tokens"], 1000);
    assert_eq!(mapped.value["used"], json!({"tokens":20,"seconds":7}));
    assert_eq!(mapped.value["updated_ms"], 123000);
    assert!(!goal.iter().any(|change| change.kind == "plan"));
    let future = e.notification(
        "thread/goal/updated",
        &json!({"threadId":"main","goal":{"threadId":"main","status":"future"}}),
    );
    assert_eq!(find(&future, "goal").value["status"], "unknown");
    assert_eq!(find(&future, "goal").value["native"]["status"], "future");
}

#[test]
fn child_goal_is_owned_and_clearing_does_not_claim_completed() {
    let mut e = endpoint();
    let child = child(&mut e, "worker", "main");
    let id = find(&child, "child").id.clone();
    let goal=e.notification("thread/goal/updated",&json!({"threadId":"worker","goal":{"threadId":"worker","objective":"inspect","status":"complete"}}));
    assert_eq!(find(&goal, "goal").id, id);
    assert_eq!(find(&goal, "goal").value["owner"], id);
    assert_eq!(find(&goal, "goal").value["status"], "completed");
    let clear = e.notification("thread/goal/cleared", &json!({"threadId":"worker"}));
    assert_eq!(find(&clear, "goal").value["native"]["cleared"], true);
    assert_eq!(find(&clear, "goal").value["status"], "unknown");
    assert!(find(&clear, "goal").value.get("objective").is_none());
    assert!(
        e.notification(
            "thread/goal/updated",
            &json!({"threadId":"main","goal":{"threadId":"other","status":"active"}})
        )
        .is_empty()
    );
}

#[test]
fn children_and_scoped_identity_budgets_do_not_leak_unscoped_items() {
    let mut e = endpoint();
    for n in 0..256 {
        assert!(!child(&mut e, &format!("worker-{n}"), "main").is_empty());
    }
    assert!(child(&mut e, "overflow", "main").is_empty());
    let item = json!({"id":"m","type":"agentMessage","text":"not adopted"});
    assert!(
        e.notification("item/started", &json!({"threadId":"overflow","item":item}))
            .is_empty()
    );
    // Exhaust the shared bounded namespace; any emitted child item remains tagged.
    for n in 0..4200 {
        let changes=e.notification("item/started",&json!({"threadId":"worker-0","item":{"id":format!("{n:0>120}"),"type":"agentMessage","text":"child"}}));
        assert!(changes.iter().all(|c| c.value["child"].is_string()));
    }
    assert!(e.notification("item/started",&json!({"threadId":"worker-0","item":{"id":"x".repeat(120),"type":"agentMessage","text":"child"}})).is_empty());
}

#[test]
fn malformed_item_without_thread_or_session_is_ignored() {
    let mut e = Endpoint::new("ep".into(), "inc".into(), "host".into(), "epoch".into());
    assert!(
        e.notification(
            "item/started",
            &json!({"item":{"id":"m","type":"agentMessage"}})
        )
        .is_empty()
    );
}

#[test]
fn root_native_ids_cannot_impersonate_generated_child_items_or_deltas() {
    let mut e = endpoint();
    child(&mut e, "worker", "main");
    let child_items = e.notification(
        "item/started",
        &json!({"threadId":"worker","item":{"id":"m","type":"agentMessage","text":"child"}}),
    );
    let generated = find(&child_items, "message").id.clone();
    let root_items = e.notification(
        "item/started",
        &json!({"threadId":"main","item":{"id":generated,"type":"agentMessage","text":"root"}}),
    );
    assert_ne!(find(&root_items, "message").id, generated);
    let root_delta = e.notification(
        "item/agentMessage/delta",
        &json!({"threadId":"main","itemId":generated,"delta":"root delta"}),
    );
    assert_eq!(
        find(&root_delta, "message").id,
        find(&root_items, "message").id
    );
    assert_ne!(find(&root_delta, "message").id, generated);
    let child_tool=e.notification("item/started",&json!({"threadId":"worker","item":{"id":"t","type":"commandExecution","status":"inProgress"}}));
    let generated_tool = find(&child_tool, "tool").id.clone();
    let root_tool=e.notification("item/started",&json!({"threadId":"main","item":{"id":generated_tool,"type":"commandExecution","status":"inProgress"}}));
    assert_ne!(find(&root_tool, "tool").id, generated_tool);
}

#[test]
fn child_resolution_cannot_cancel_main_request_with_same_native_id() {
    let mut e = endpoint();
    child(&mut e, "worker", "main");
    let request = e
        .request(
            json!(71),
            "item/fileChange/requestApproval",
            &json!({"threadId":"main","itemId":"tool"}),
        )
        .unwrap();
    assert!(
        e.notification(
            "serverRequest/resolved",
            &json!({"threadId":"worker","requestId":71})
        )
        .is_empty()
    );
    let answer=e.prepare_answer(&json!({"v":0,"epoch":"e2","request":request.id,"revision":1,"response":{"choice":"decline"}})).unwrap();
    assert_eq!(answer.native_id, json!(71));
}

#[test]
fn late_idle_thread_started_does_not_revive_terminal_collab_children() {
    for (native, status) in [
        ("completed", "completed"),
        ("errored", "failed"),
        ("shutdown", "cancelled"),
    ] {
        let mut e = endpoint();
        let terminal=e.notification("item/completed",&json!({"threadId":"main","item":{
            "id":"spawn","type":"collabAgentToolCall","tool":"spawnAgent","status":"completed",
            "senderThreadId":"main","receiverThreadIds":["worker"],"agentsStates":{"worker":{"status":native}}}}));
        assert_eq!(find(&terminal, "child").value["status"], status);
        let late = child(&mut e, "worker", "main");
        assert_eq!(find(&late, "child").value["status"], status);
        assert_eq!(find(&late, "child").value["native"]["status"], native);
        let changed = e.notification(
            "thread/status/changed",
            &json!({"threadId":"worker","status":{"type":"idle"}}),
        );
        assert_eq!(find(&changed, "child").value["status"], status);
        let fresh = e.notification(
            "turn/started",
            &json!({"threadId":"worker","turn":{"id":"fresh"}}),
        );
        assert_eq!(find(&fresh, "child").value["status"], "running");
    }
}

#[test]
fn oversized_child_or_plan_inputs_are_bounded() {
    let mut e = endpoint();
    assert!(child(&mut e, &"x".repeat(129), "main").is_empty());
    let plan = e.notification("turn/plan/updated", &json!({"threadId":"main","turnId":"t","plan":[{"step":"x".repeat(100000),"status":"pending"}]}));
    assert!(
        plan.is_empty()
            || serde_json::to_vec(&find(&plan, "plan").value)
                .unwrap()
                .len()
                < vqro::codex::ENTITY_BYTES
    );
}
