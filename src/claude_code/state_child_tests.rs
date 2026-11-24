use super::*;
#[test]
fn slice3_recent_child_scope_survives_context_free_changed_unwritten_snapshot() {
    let mut s = state();
    let child = start(&mut s);
    let mut first = question("changing-child", true);
    first["request"]["requires_user_interaction"] = json!(true);
    s.record(&first);
    // Old automatic response is definitely unwritten. A changed full snapshot may omit optional
    // producer context; an unsettled known-child native identity must not become a ROOT form.
    let changed = question("changing-child", false);
    s.record(&changed);
    let summary = s.requests.summaries();
    assert_eq!(summary.len(), 1);
    assert_eq!(
        summary[0]["child"], child,
        "RECENT native question scope must survive missing optional metadata"
    );
    assert_eq!(s.status(), "idle");
}
#[test]
fn known_root_unsupported_tombstone_keeps_root_unknown_but_child_does_not() {
    let mut root = state();
    let mut q = question("root-unconfirmed", false);
    q["request"]["requires_user_interaction"] = json!(true);
    root.record(&q);
    assert!(root.requests.has_unconfirmed());
    assert_eq!(
        root.status(),
        "unknown",
        "known ROOT unsupported/possible-write tombstone MUST retain unknown parent phase"
    );
    let mut child = state();
    start(&mut child);
    let mut q = question("child-unconfirmed", true);
    q["request"]["requires_user_interaction"] = json!(true);
    child.record(&q);
    assert!(child.requests.has_unconfirmed());
    assert_eq!(
        child.status(),
        "idle",
        "only known child tombstones are outside root phase"
    );
}
#[test]
fn structured_plan_output_is_correlated_and_input_intent_is_not_projected() {
    let mut s = state();
    s.record(&json!({"type":"assistant","uuid":"p1","message":{"content":[{"type":"tool_use","id":"p-tool","name":"TodoWrite","input":{"todos":[{"content":"intent"}]}}]}}));
    s.record(&json!({"type":"user","uuid":"p2","message":{"content":[{"type":"tool_result","tool_use_id":"p-tool","content":"not parsed"}]},"tool_use_result":{"newTodos":[{"content":"actual output","status":"completed","activeForm":"done"}]}}));
    let value = s.journal.entity("plan", "plan1").unwrap().1;
    assert_eq!(value["items"][0]["text"], "actual output");
    assert_eq!(value["tool"], "tool-p-tool");
    assert!(
        s.work.rows().is_empty(),
        "model task tracking is NOT native execution work"
    );
    s.record(&json!({"type":"assistant","uuid":"p3","message":{"content":[{"type":"tool_use","id":"other","name":"TodoWrite","input":{}}]}}));
    s.record(&json!({"type":"user","uuid":"p4","message":{"content":[{"type":"tool_result","tool_use_id":"p-tool"},{"type":"tool_result","tool_use_id":"other"}]},"tool_use_result":{"newTodos":[{"content":"ambiguous must not apply","status":"pending"}]}}));
    let value = s.journal.entity("plan", "plan1").unwrap().1;
    assert_eq!(value["items"][0]["text"], "actual output");
    assert_eq!(value["incomplete"], true);
}
#[test]
fn goal_clear_removes_old_objective_without_inferred_completion() {
    let mut s = state();
    s.record(&json!({"type":"active_goal","session_id":"ROOT","value":{"condition":"actual native objective","iterations":1}}));
    assert_eq!(
        s.journal.entity("goal", "current").unwrap().1["objective"],
        "actual native objective"
    );
    s.record(&json!({"type":"active_goal","session_id":"ROOT","value":null}));
    let value = s.journal.entity("goal", "current").unwrap().1;
    assert!(value.get("objective").is_none());
    assert_eq!(value["status"], "unknown");
}
fn state() -> State {
    let mut s = State::new("boot".into());
    s.initialized(&json!({"claude_code_version":"2.1.329"}));
    s.record(&json!({"type":"system","subtype":"init","session_id":"ROOT"}));
    s.record(&json!({"type":"system","subtype":"session_state_changed","session_id":"ROOT","state":"idle"}));
    s
}
fn start(s: &mut State) -> String {
    s.record(&json!({"type":"system","subtype":"task_started","task_id":"a","run_id":"1","task_type":"local_agent","subagent_type":"worker","spawn_depth":1,"tool_use_id":"spawn-a"}));
    s.work
        .rows()
        .into_iter()
        .find(|r| r.kind == "child")
        .unwrap()
        .id
}
fn question(id: &str, child: bool) -> Value {
    let mut f = json!({"type":"control_request","request_id":id,"request":{"subtype":"can_use_tool","tool_name":"Bash","tool_use_id":"same-tool","input":{}}});
    if child {
        f["request"]["agent_id"] = json!("a");
    }
    f
}
#[test]
fn child_messages_never_mutate_parent_session_turn_receipt_or_availability() {
    let mut s = state();
    let child = start(&mut s);
    let epoch = s.epoch().to_owned();
    s.put("message","same.0",json!({"position":1,"role":"user","content":[{"type":"text","text":"parent"}],"final":true}));
    s.inputs
        .insert("same".into(), ("delivery".into(), epoch.clone()));
    s.receipts
        .insert("delivery".into(), json!({"stage":"staged"}));
    let receipt = s.receipts.clone();
    let availability = s.availability.value();
    let status = s.status();
    s.record(&json!({"type":"assistant","agent_id":"a","session_id":"CHILD","uuid":"same","message":{"id":"api","model":"native-child-model","content":[{"type":"text","text":"child"}]}}));
    s.record(&json!({"type":"user","agent_id":"a","session_id":"CHILD","uuid":"same","isReplay":true,"message":{"content":[{"type":"text","text":"echo"}]}}));
    let id = s.work.identity(&child, "message", "same.0", false).unwrap();
    let value = s.journal.entity("message", &id).unwrap().1;
    assert_eq!(value["child"], child);
    assert!(value.get("delivery").is_none());
    assert_eq!(
        s.journal.entity("message", "same.0").unwrap().1["content"][0]["text"],
        "parent"
    );
    assert_eq!(s.epoch(), epoch);
    assert_eq!(s.session.as_deref(), Some("ROOT"));
    assert!(s.run.is_none());
    assert_eq!(s.status(), status);
    assert_eq!(s.receipts, receipt);
    assert_eq!(s.availability.value(), availability);
    s.record(&json!({"type":"conversation_reset","agent_id":"a"}));
    assert_eq!(s.epoch(), epoch);
}
#[test]
fn child_tool_ids_and_settlement_do_not_touch_parent_tools_or_questions() {
    let mut s = state();
    let child = start(&mut s);
    s.record(&json!({"type":"assistant","uuid":"root","message":{"content":[{"type":"tool_use","id":"same-tool","name":"Bash","input":{}}]}}));
    s.record(&question("root-question", false));
    s.record(&question("child-question", true));
    s.record(&json!({"type":"assistant","agent_id":"a","uuid":"child","message":{"content":[{"type":"tool_use","id":"same-tool","name":"ChildTool","input":{}}]}}));
    s.record(&json!({"type":"user","agent_id":"a","uuid":"result","message":{"content":[{"type":"tool_result","tool_use_id":"same-tool","content":"child done"}]}}));
    let id = s.work.identity(&child, "tool", "same-tool", false).unwrap();
    assert_eq!(
        s.journal.entity("tool", &id).unwrap().1["status"],
        "completed"
    );
    assert_eq!(
        s.journal.entity("tool", "tool-same-tool").unwrap().1["status"],
        "pending"
    );
    assert!(s.active_tools.contains("same-tool"));
    assert_eq!(s.requests.len(), 1);
    assert!(!s.requests.root_is_empty());
    let pending = s.requests.summaries();
    assert!(pending[0]["child"].is_null());
}
#[test]
fn child_question_is_scoped_but_root_phase_stays_idle_and_restart_fence_remains_global() {
    let mut s = state();
    let child = start(&mut s);
    s.record(&question("child-question", true));
    assert_eq!(s.status(), "idle");
    assert!(s.requests.root_is_empty());
    assert!(!s.requests.is_empty());
    let summary = s.requests.summaries();
    assert_eq!(summary[0]["child"], child);
    let id = summary[0]["request"].as_str().unwrap();
    assert_eq!(s.journal.entity("request", id).unwrap().1["child"], child);
}
#[test]
fn unrelated_root_tool_progress_cannot_rearm_child_automatic_response() {
    let mut s = state();
    start(&mut s);
    let mut q = question("unsupported", true);
    q["request"]["requires_user_interaction"] = json!(true);
    s.record(&q);
    assert_eq!(s.outgoing.len(), 1);
    s.record(&json!({"type":"tool_progress","tool_use_id":"same-tool"}));
    s.record(&q);
    assert_eq!(
        s.outgoing.len(),
        1,
        "root progress is not independent settlement of a child tool"
    );
    s.record(&json!({"type":"tool_progress","agent_id":"a","tool_use_id":"same-tool"}));
    s.record(&q);
    assert_eq!(
        s.outgoing.len(),
        2,
        "only matching child settlement retires that native tombstone"
    );
}
#[test]
fn unresolvable_child_question_is_never_an_approvable_root_form() {
    let mut s = state();
    let mut q = question("unknown-scope", false);
    q["parent_tool_use_id"] = json!("x".repeat(129));
    s.record(&q);
    assert!(s.requests.summaries().is_empty());
    assert_eq!(s.outgoing.len(), 1);
    assert_eq!(s.status(), "idle");
    s.record(&json!({"type":"tool_progress","tool_use_id":"same-tool"}));
    s.record(&q);
    assert_eq!(
        s.outgoing.len(),
        1,
        "root progress cannot settle unknown child scope"
    );
}
