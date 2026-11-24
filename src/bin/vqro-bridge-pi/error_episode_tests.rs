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
        "sessionId":"01234567-89ab-4cde-8fab-0123456789ab","isStreaming":false,"isCompacting":false,
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
fn actual_error_facts_satisfy_the_generic_pane_menu_predicate() {
    use std::{io::Write, process::{Command, Stdio}};
    let facts=failed("synthetic native error","error");
    let agent=Agent::new("menu-capabilities".into(),admission::Signals::new());
    let root=std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let home=tempfile::tempdir().unwrap();
    let mut child=Command::new(std::env::var("VQRO_PYTHON").unwrap_or_else(|_|"python3".into()))
        .arg(root.join("tests/pi_error_menu_facts.py"))
        .env_clear().env("HOME",home.path()).env("PATH",std::env::var_os("PATH").unwrap_or_default())
        .env("PYTHONDONTWRITEBYTECODE","1")
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    child.stdin.take().unwrap().write_all(serde_json::to_string(&json!({"facts":facts,
        "capabilities":agent.provider_capabilities()})).unwrap().as_bytes()).unwrap();
    let output=child.wait_with_output().unwrap();
    assert!(output.status.success(),"{}",String::from_utf8_lossy(&output.stderr));
    let derived:Value=serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(derived["menu"]["command"],"accounts.offer");
    assert_eq!(derived["context"]["menu.agent_error"],true);
    assert_eq!(derived["context"]["menu.agent_app"],"pi");
    assert!(vqro::ui_model::workspace::when_holds(&derived["menu"],&derived["context"]),"{derived}");
}
#[test]
fn truncation_and_success_never_create_a_complete_native_error() {
    let truncated=failed(&"x".repeat(1025),"error");
    assert_eq!(truncated["data"]["agent"]["completion"]["native"]["error_message_complete"],false);
    let successful=failed("You have hit your ChatGPT usage limit.","stop");
    assert!(successful["data"]["agent"]["completion"]["native"].is_null());
}
