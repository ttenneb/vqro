//! No executable, model, credential or transcript: synthetic native records only.
use serde_json::{Value, json};
use vqro::claude_code::state::State;
use vqro::native_process::Event;
fn observe(state: &mut State, value: Value) {
    state.observe(&Event::Record(value));
}
fn state() -> State {
    let mut s = State::new("boot".into());
    s.initialized(&json!({"claude_code_version":"2.1.329"}));
    observe(
        &mut s,
        json!({"type":"system","subtype":"init","session_id":"s1"}),
    );
    s
}
#[test]
fn completed_content_blocks_sharing_api_id_do_not_overwrite_each_other() {
    let mut s = state();
    for (uuid, text) in [("a1", "first"), ("a2", "second")] {
        observe(
            &mut s,
            json!({"type":"assistant","uuid":uuid,"session_id":"s1","parent_tool_use_id":null,
            "message":{"id":"api-id","content":[{"type":"text","text":text}]}}),
        );
    }
    assert_eq!(
        s.journal.entity("message", "a1.0").unwrap().1["content"][0]["text"],
        "first"
    );
    assert_eq!(
        s.journal.entity("message", "a2.0").unwrap().1["content"][0]["text"],
        "second"
    );
    observe(
        &mut s,
        json!({"type":"assistant","uuid":"child-message","session_id":"s1","agent_id":"child1","parent_tool_use_id":"tool1","message":{"content":[{"type":"text","text":"child only"}]}}),
    );
    assert!(s.journal.entity("message", "child-message.0").is_none());
    assert!(s.incomplete.contains("children"));
}
#[test]
fn tools_and_native_permission_withdrawals_are_correlated_not_guessed_from_order() {
    let mut s = state();
    observe(
        &mut s,
        json!({"type":"assistant","uuid":"a1","session_id":"s1","message":{"content":[{"type":"tool_use","id":"tool1","name":"Bash","input":{"command":"echo safe"}}]}}),
    );
    observe(
        &mut s,
        json!({"type":"control_request","request_id":"p1","request":{"subtype":"can_use_tool","tool_name":"Bash","tool_use_id":"tool1","input":{"command":"echo safe"}}}),
    );
    assert_eq!(s.requests.len(), 1);
    observe(
        &mut s,
        json!({"type":"user","uuid":"result-message","session_id":"s1","message":{"content":[{"type":"tool_result","tool_use_id":"tool1","content":"safe","is_error":false}]}}),
    );
    assert_eq!(
        s.journal.entity("tool", "tool-tool1").unwrap().1["status"],
        "completed"
    );
    assert!(s.active_tools.is_empty());
    assert!(s.requests.is_empty());
    assert!(!s.requests.has_unconfirmed());
}
#[test]
fn receipt_transform_snapshot_survives_echo_lookup_and_epoch_change() {
    let mut s = state();
    let epoch = s.epoch().to_owned();
    s.inputs
        .insert("input1".into(), ("delivery1".into(), epoch.clone()));
    s.save_receipt("delivery1",json!({"delivery":"delivery1","stage":"dispatched","may_transform":"unknown","origin":{"principal":"sender"}}));
    observe(
        &mut s,
        json!({"type":"user","uuid":"input1","session_id":"s1","isReplay":true,"message":{"content":"body"}}),
    );
    assert_eq!(s.receipt("delivery1")["stage"], "admitted");
    assert_eq!(s.receipt("delivery1")["may_transform"], "unknown");
    observe(
        &mut s,
        json!({"type":"conversation_reset","new_conversation_id":"s2","session_id":"s1"}),
    );
    assert_ne!(s.epoch(), epoch);
    observe(
        &mut s,
        json!({"type":"result","uuid":"old-result","session_id":"s1","user_message_uuid":"input1","terminal_reason":"completed","is_error":false}),
    );
    assert_eq!(s.receipt("delivery1")["stage"], "admitted");
    assert_eq!(s.receipt("delivery1")["may_transform"], "unknown");
}
#[test]
fn native_multi_input_completion_binds_each_member_but_does_not_invent_idle() {
    let mut s = state();
    let epoch = s.epoch().to_owned();
    for id in ["input1", "input2"] {
        s.inputs.insert(id.into(), (id.into(), epoch.clone()));
        s.save_receipt(
            id,
            json!({"delivery":id,"stage":"dispatched","may_transform":"unknown"}),
        );
    }
    observe(
        &mut s,
        json!({"type":"system","subtype":"session_state_changed","state":"running","session_id":"s1"}),
    );
    observe(
        &mut s,
        json!({"type":"result","uuid":"result1","session_id":"s1","user_message_uuid":"input2","user_message_uuids":["input1","input2"],"terminal_reason":"completed","is_error":false}),
    );
    assert_eq!(s.receipt("input1")["stage"], "completed");
    assert_eq!(s.receipt("input2")["stage"], "completed");
    assert_eq!(s.status(), "running");
    assert_eq!(s.completion.as_ref().unwrap()["reason"], "end_turn");
    assert_eq!(
        s.receipt("input1")["native"]["run"],
        "result1",
        "native run identifiers must not be adapter-generated turn IDs"
    );
    observe(
        &mut s,
        json!({"type":"system","subtype":"session_state_changed","state":"idle","session_id":"s1"}),
    );
    assert_eq!(s.status(), "idle");
}
#[test]
fn newer_native_work_invalidates_idle_limit_episode_without_inventing_queue_empty() {
    let mut s = state();
    observe(
        &mut s,
        json!({"type":"system","subtype":"session_state_changed","state":"running","session_id":"s1"}),
    );
    observe(
        &mut s,
        json!({"type":"rate_limit_event","session_id":"s1","rate_limit_info":{"status":"rejected","rateLimitType":"five_hour"}}),
    );
    observe(
        &mut s,
        json!({"type":"result","uuid":"limited-result","session_id":"s1","terminal_reason":"blocking_limit","is_error":true,"queued_turn_count":0}),
    );
    observe(
        &mut s,
        json!({"type":"system","subtype":"session_state_changed","state":"idle","session_id":"s1"}),
    );
    assert_eq!(
        s.availability.value().unwrap()["native"]["episode"],
        "limited-result"
    );
    assert_eq!(s.availability.value().unwrap()["state"], "available");
    assert!(
        s.facts("claude-test", "instance")["data"]["agent"]["queue"].is_null(),
        "partial native count0 is not a complete empty queue witness"
    );
    observe(
        &mut s,
        json!({"type":"system","subtype":"session_state_changed","state":"running","session_id":"s1"}),
    );
    observe(
        &mut s,
        json!({"type":"system","subtype":"session_state_changed","state":"idle","session_id":"s1"}),
    );
    assert!(
        s.availability.value().unwrap()["native"]
            .get("episode")
            .is_none(),
        "start/cancel/idle cannot revive old limit"
    );
}
#[test]
fn owned_ledger_is_not_native_queue_and_keeps_unresolved_old_epochs() {
    let mut s = state();
    assert_eq!(s.input_ledger()["pending"], 0);
    let epoch = s.epoch().to_owned();
    s.inputs
        .insert("input1".into(), ("delivery1".into(), epoch));
    s.save_receipt(
        "delivery1",
        json!({"stage":"dispatched","may_transform":"unknown"}),
    );
    assert_eq!(s.input_ledger()["pending"], 1);
    observe(
        &mut s,
        json!({"type":"conversation_reset","new_conversation_id":"s2","session_id":"s1"}),
    );
    assert_eq!(
        s.input_ledger()["pending"],
        1,
        "new epoch must not turn unresolved old input into an empty ledger"
    );
    assert!(s.facts("claude-test", "instance")["data"]["agent"]["queue"].is_null());
}
#[test]
fn only_complete_native_background_level_establishes_known_empty() {
    let mut s = state();
    assert!(s.background.is_none());
    observe(
        &mut s,
        json!({"type":"system","subtype":"background_tasks_changed","session_id":"s1","tasks":[]}),
    );
    assert_eq!(s.background, Some(Vec::new()));
    observe(
        &mut s,
        json!({"type":"system","subtype":"background_tasks_changed","session_id":"s1","tasks":[{"task_id":"native-task"}]}),
    );
    assert!(
        s.background.is_none(),
        "unprojected nonempty level cannot become known empty"
    );
    observe(
        &mut s,
        json!({"type":"system","subtype":"background_tasks_changed","session_id":"s1","tasks":[]}),
    );
    assert_eq!(s.background, Some(Vec::new()));
}
#[test]
fn pending_snapshots_on_control_wrapper_are_not_lost_or_rendered_twice() {
    let mut s = state();
    let question = json!({"type":"control_request","request_id":"p1","request":{"subtype":"can_use_tool","tool_name":"Bash","tool_use_id":"tool1","input":{}}});
    observe(
        &mut s,
        json!({"type":"control_response","response":{"subtype":"success","request_id":"n1","response":{},"pending_permission_requests":[question.clone()]}}),
    );
    observe(&mut s, question);
    assert_eq!(s.requests.len(), 1);
}
