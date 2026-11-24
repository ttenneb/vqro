//! Synthetic native DTO → Agent → facts controls. No native process/model/auth.
use super::{admission, native, Agent};
use serde_json::{json, Value};

fn observe(agent:&mut Agent, record:Value) {
    let event=native::Event::Record(record);
    agent.signals.observe(&event);
    agent.observe(&event);
}
fn failed(message:&str, stop:&str)->Value {
    let mut agent=Agent::new("error-witness-test".into(),admission::Signals::new());
    observe(&mut agent,json!({"type":"response","command":"get_state","success":true,"data":{
        "sessionId":"01234567-89ab-4cde-8fab-0123456789ab","isStreaming":false,
        "model":{"id":"model","provider":"native-alias","api":"pi-virtual"}}}));
    observe(&mut agent,json!({"type":"agent_start"}));
    observe(&mut agent,json!({"type":"message_end","message":{"role":"assistant","content":[],
        "timestamp":123456,"stopReason":stop,"errorMessage":message}}));
    observe(&mut agent,json!({"type":"agent_settled","aborted":false}));
    agent.facts("fixture-provider","fixture-cwd")
}
#[test]
fn native_error_witness_is_not_native_quota_classification() {
    let facts=failed("You have hit your ChatGPT usage limit.","error");
    assert_eq!(facts["data"]["agent"]["completion"]["native"],json!({
        "stopReason":"error","timestamp":123456,"error_message_complete":true}));
    assert_ne!(facts["data"]["agent"]["availability"]["reason"],"quota");
    assert_eq!(facts["data"]["agent"]["caps"]["agent.availability"]["reasons"],json!(["_retrying"]));
    assert_eq!(facts["data"]["agent"]["session"]["model"]["native"],
               json!({"provider":"native-alias","api":"pi-virtual"}));
}
#[test]
fn truncation_and_success_never_create_a_complete_native_error() {
    let truncated=failed(&"x".repeat(1025),"error");
    assert_eq!(truncated["data"]["agent"]["completion"]["native"]["error_message_complete"],false);
    let successful=failed("You have hit your ChatGPT usage limit.","stop");
    assert!(successful["data"]["agent"]["completion"]["native"].is_null());
}
