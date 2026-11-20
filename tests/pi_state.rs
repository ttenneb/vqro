//! Pi normalization against the shared item producer/viewer. No second document/replay implementation.
#[allow(dead_code)] #[path="../src/bin/vqro-bridge-pi/native.rs"] mod native;
#[allow(dead_code)] #[path="../src/bin/vqro-bridge-pi/admission.rs"] mod admission;
#[allow(dead_code)] #[path="../src/bin/vqro-bridge-pi/contract.rs"] mod contract;
#[path="../src/bin/vqro-bridge-pi/state.rs"] mod state;
use admission::Signals;
use native::{Event,Exit};
use serde_json::{json,Value};
use state::Agent;
use vqro::provider_client::items::{ItemView,Next};
fn event(agent:&mut Agent,value:Value) {let event=Event::Record(value);agent.signals.observe(&event);agent.observe(&event);}
fn metadata(agent:&mut Agent,session:&str,batch:&str) {event(agent,json!({"type":"response","id":"meta","command":"get_state","success":true,
    "data":{"sessionId":session,"isStreaming":false,"isCompacting":false,"steeringMode":batch,"followUpMode":"all","model":{"input":["text","image"]}}}));}
fn make()->Agent {let mut agent=Agent::new("private".into(),Signals::new());metadata(&mut agent,"s1","one-at-a-time");agent}
fn entities(agent:&Agent,kind:&str)->Vec<Value> {String::from_utf8(agent.journal.snapshot()).unwrap().lines().map(|line|serde_json::from_str::<Value>(line).unwrap()).filter(|row|row["op"]=="put" && row["kind"]==kind).collect()}
fn message(agent:&mut Agent,text:&str,reason:&str) {event(agent,json!({"type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":text}],"stopReason":reason}}));}

#[test]
fn modules_are_versioned_descriptor_maps_for_resource_and_provider() {
    let a=make();
    for caps in [a.caps().clone(),json!(a.provider_capabilities())] {
        let modules=&caps["agent"]["modules"];
        assert!(modules.is_object(),"module lists are not part of agent-v0");
        assert_eq!(modules["stop"]["v"],0);
        assert!(modules.get("queue").is_none(),"absence must not invent support");
        assert!(modules.as_object().unwrap().values().all(|descriptor|descriptor.is_object() && descriptor["v"].as_u64().is_some()));
    }
}

#[test]
fn message_deltas_are_append_records_and_materialize_identically() {
    let mut a=make();event(&mut a,json!({"type":"agent_start"}));
    event(&mut a,json!({"type":"message_start","message":{"role":"assistant","content":[]}}));
    event(&mut a,json!({"type":"message_update","message":{"role":"assistant","content":[{"type":"text","text":"he"}]},"assistantMessageEvent":{"type":"text_start","contentIndex":0}}));
    let mut cursor=a.journal.open(Some(&json!({"epoch":a.epoch(),"seq":a.journal.seq()})));
    event(&mut a,json!({"type":"message_update","message":{"role":"assistant","content":[{"type":"text","text":"hello"}]},"assistantMessageEvent":{"type":"text_delta","contentIndex":0,"delta":"llo"}}));
    let Next::Data(bytes)=cursor.next(&a.journal,65536) else {panic!("delta");};
    assert_eq!(serde_json::from_slice::<Value>(&bytes).unwrap()["op"],"append");
    message(&mut a,"hello","stop");event(&mut a,json!({"type":"agent_settled","aborted":false}));
    let messages=entities(&a,"message");assert_eq!(messages.len(),1);
    assert_eq!(messages[0]["value"]["content"][0]["text"],"hello");assert_eq!(messages[0]["value"]["final"],true);
    assert!(messages[0]["value"].get("delivery").is_none(),"no correlation inferred from timing or text");
    let mut view=ItemView::new();view.feed(&a.journal.snapshot());
    assert_eq!(view.digest(),a.journal.digest());assert_eq!(view.errors.total(),0);
    assert_eq!(a.facts("pi","private")["data"]["agent"]["completion"]["reason"],"end_turn");
}

#[test]
fn errors_unknown_reasons_and_native_end_are_not_fabricated_success() {
    let mut a=make();event(&mut a,json!({"type":"agent_start"}));
    event(&mut a,json!({"type":"message_end","message":{"role":"assistant","content":[],"stopReason":"error","errorMessage":"native failure"}}));
    event(&mut a,json!({"type":"agent_settled","aborted":false}));
    assert_eq!(a.facts("pi","private")["status"]["phase"],"error");
    assert_eq!(a.completion.as_ref().unwrap()["error"]["message"],"native failure");
    event(&mut a,json!({"type":"agent_start"}));message(&mut a,"unknown","_native_new_reason");event(&mut a,json!({"type":"agent_settled"}));
    assert_eq!(a.completion.as_ref().unwrap()["reason"],"_native_new_reason");
    let end=Event::Exited(Exit {code:Some(0),success:true});a.signals.observe(&end);a.observe(&end);
    event(&mut a,json!({"type":"agent_start"}));
    assert_eq!(a.facts("pi","private")["data"]["agent"]["lifecycle"],"ended");assert!(a.run.is_none());
}

#[test]
fn entity_retention_is_bounded_and_receipts_survive_pruning() {
    let mut a=make();
    a.receipt("pending",json!({"stage":"dispatched"}));
    for i in 0..300 {a.receipt(&format!("d{i}"),json!({"stage":"admitted"}));}
    assert_eq!(a.receipts.len(),257);assert!(!a.receipts.contains_key("d0"));assert!(a.receipts.contains_key("pending"));
    for _ in 0..600 {message(&mut a,&"x".repeat(1800),"stop");}
    assert!(a.journal.len()<=state::ENTITY_COUNT);assert!(a.journal.entity_bytes()<=state::ENTITY_TOTAL);
    assert!(a.journal.entity("state","current").is_some());assert!(a.journal.entity("session","current").is_some());
    assert_eq!(a.receipts["d299"]["stage"],"admitted");
    a.receipt("d299",json!({"stage":"dispatched"}));assert_eq!(a.receipts["d299"]["stage"],"admitted");
    let mut view=ItemView::new();view.feed(&a.journal.snapshot());assert_eq!(view.errors.total(),0);assert_eq!(view.digest(),a.journal.digest());
}

#[test]
fn session_switch_changes_epoch_not_endpoint_or_receipt_lookup() {
    let mut a=make();let old=a.epoch().to_string();let endpoint=a.facts("pi","private")["ref"].clone();
    a.receipt("d1",json!({"stage":"admitted"}));message(&mut a,"old session","stop");
    metadata(&mut a,"s2","all");
    assert_ne!(a.epoch(),old);assert_eq!(a.facts("pi","private")["ref"],endpoint);
    let begin:Value=serde_json::from_str(String::from_utf8(a.journal.snapshot()).unwrap().lines().next().unwrap()).unwrap();
    assert_eq!(begin["coverage"],"tail");
    assert!(entities(&a,"message").is_empty());assert_eq!(a.receipts["d1"]["stage"],"admitted");
    assert_eq!(a.caps()["agent.send"]["modes"]["boundary"]["batch"],"all");
    assert_eq!(a.provider_capabilities()["agent.send"]["modes"]["boundary"]["batch"],json!(["one","all"]));
}

#[test]
fn missing_idle_evidence_and_oversized_native_output_are_explicit() {
    let mut a=Agent::new("private".into(),Signals::new());assert_eq!(a.state(),"unknown");
    let mut m=make();message(&mut m,&"x".repeat(100000),"stop");
    let messages=entities(&m,"message");assert_eq!(messages[0]["value"]["incomplete"],json!(["content"]));
    assert!(m.incomplete.contains("items"));
    assert!(m.journal.entity_bytes()<=state::ENTITY_TOTAL);
    event(&mut a,json!({"type":"agent_settled","aborted":true}));assert!(a.completion.is_none(),"settled without a run is not completion evidence");
}
