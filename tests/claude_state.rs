//! No executable, model, credential or transcript: synthetic native records only.
use serde_json::{Value, json};
use vqro::claude_code::state::State;
use vqro::native_process::Event;
fn observe(state:&mut State, value:Value){state.observe(&Event::Record(value));}
fn state()->State{let mut s=State::new("boot".into());s.initialized(&json!({"claude_code_version":"2.1.329"}));observe(&mut s,json!({"type":"system","subtype":"init","session_id":"s1"}));s}
#[test]
fn completed_content_blocks_sharing_api_id_do_not_overwrite_each_other() {
    let mut s=state();
    for (uuid,text) in [("a1","first"),("a2","second")] {
        observe(&mut s,json!({"type":"assistant","uuid":uuid,"session_id":"s1","parent_tool_use_id":null,
            "message":{"id":"api-id","content":[{"type":"text","text":text}]}}));
    }
    assert_eq!(s.journal.entity("message","a1.0").unwrap().1["content"][0]["text"],"first");
    assert_eq!(s.journal.entity("message","a2.0").unwrap().1["content"][0]["text"],"second");
    observe(&mut s,json!({"type":"assistant","uuid":"child-message","session_id":"s1","agent_id":"child1","parent_tool_use_id":"tool1","message":{"content":[{"type":"text","text":"child only"}]}}));
    assert!(s.journal.entity("message","child-message.0").is_none());
    assert!(s.incomplete.contains("children"));
}
#[test]
fn tools_and_native_permission_withdrawals_are_correlated_not_guessed_from_order() {
    let mut s=state();
    observe(&mut s,json!({"type":"assistant","uuid":"a1","session_id":"s1","message":{"content":[{"type":"tool_use","id":"tool1","name":"Bash","input":{"command":"echo safe"}}]}}));
    observe(&mut s,json!({"type":"control_request","request_id":"p1","request":{"subtype":"can_use_tool","tool_name":"Bash","tool_use_id":"tool1","input":{"command":"echo safe"}}}));
    assert_eq!(s.requests.len(),1);
    observe(&mut s,json!({"type":"user","uuid":"result-message","session_id":"s1","message":{"content":[{"type":"tool_result","tool_use_id":"tool1","content":"safe","is_error":false}]}}));
    assert_eq!(s.journal.entity("tool","tool-tool1").unwrap().1["status"],"completed");
    assert!(s.active_tools.is_empty());assert!(s.requests.is_empty());assert!(!s.requests.has_unconfirmed());
}
#[test]
fn receipt_transform_snapshot_survives_echo_lookup_and_epoch_change() {
    let mut s=state();
    let epoch=s.epoch().to_owned();s.inputs.insert("input1".into(),("delivery1".into(),epoch.clone()));
    s.save_receipt("delivery1",json!({"delivery":"delivery1","stage":"dispatched","may_transform":"unknown","origin":{"principal":"sender"}}));
    observe(&mut s,json!({"type":"user","uuid":"input1","session_id":"s1","isReplay":true,"message":{"content":"body"}}));
    assert_eq!(s.receipt("delivery1")["stage"],"admitted");
    assert_eq!(s.receipt("delivery1")["may_transform"],"unknown");
    observe(&mut s,json!({"type":"conversation_reset","new_conversation_id":"s2","session_id":"s1"}));
    assert_ne!(s.epoch(),epoch);
    observe(&mut s,json!({"type":"result","uuid":"old-result","session_id":"s1","user_message_uuid":"input1","terminal_reason":"completed","is_error":false}));
    assert_eq!(s.receipt("delivery1")["stage"],"admitted");
    assert_eq!(s.receipt("delivery1")["may_transform"],"unknown");
}
#[test]
fn native_multi_input_completion_binds_each_member_but_does_not_invent_idle() {
    let mut s=state();let epoch=s.epoch().to_owned();
    for id in ["input1","input2"] {s.inputs.insert(id.into(),(id.into(),epoch.clone()));s.save_receipt(id,json!({"delivery":id,"stage":"dispatched","may_transform":"unknown"}));}
    observe(&mut s,json!({"type":"system","subtype":"session_state_changed","state":"running","session_id":"s1"}));
    observe(&mut s,json!({"type":"result","uuid":"result1","session_id":"s1","user_message_uuid":"input2","user_message_uuids":["input1","input2"],"terminal_reason":"completed","is_error":false}));
    assert_eq!(s.receipt("input1")["stage"],"completed");assert_eq!(s.receipt("input2")["stage"],"completed");
    assert_eq!(s.status(),"running");
    observe(&mut s,json!({"type":"system","subtype":"session_state_changed","state":"idle","session_id":"s1"}));
    assert_eq!(s.status(),"idle");
}
#[test]
fn pending_snapshots_on_control_wrapper_are_not_lost_or_rendered_twice() {
    let mut s=state();
    let question=json!({"type":"control_request","request_id":"p1","request":{"subtype":"can_use_tool","tool_name":"Bash","tool_use_id":"tool1","input":{}}});
    observe(&mut s,json!({"type":"control_response","response":{"subtype":"success","request_id":"n1","response":{},"pending_permission_requests":[question.clone()]}}));
    observe(&mut s,question);assert_eq!(s.requests.len(),1);
}
