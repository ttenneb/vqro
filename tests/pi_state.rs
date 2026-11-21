//! Pi normalization against the shared item producer/viewer. No second document/replay implementation.
#[allow(dead_code)] #[path="../src/bin/vqro-bridge-pi/native.rs"] mod native;
#[allow(dead_code)] #[path="../src/bin/vqro-bridge-pi/admission.rs"] mod admission;
#[allow(dead_code)] #[path="../src/bin/vqro-bridge-pi/contract.rs"] mod contract;
#[allow(dead_code)] #[path="../src/bin/vqro-bridge-pi/queue.rs"] mod queue;
#[allow(dead_code)] #[path="../src/bin/vqro-bridge-pi/requests.rs"] mod requests;
#[allow(dead_code)] #[path="../src/bin/vqro-bridge-pi/configuration.rs"] mod configuration;
#[allow(dead_code)] #[path="../src/bin/vqro-bridge-pi/commands.rs"] mod commands;
#[allow(dead_code)] #[path="../src/bin/vqro-bridge-pi/panel.rs"] mod panel;
#[allow(dead_code)] #[path="../src/bin/vqro-bridge-pi/origins.rs"] mod origins;
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

/// #119 P2 (vqro-lead-secondary's control): a correlated origin belongs to the one entity its user message_start
/// created. A later uncorrelated user message, even a standalone native message_end, never inherits it; A keeps it.
#[test]
fn correlated_origin_never_leaks_to_a_later_uncorrelated_user_message() {
    for with_start in [true, false] {
        let mut a=make();
        let origin=json!({"principal":"p:owner","kind":"human"});
        a.origins.arm(origins::Entry::new("d-origin", &origin, &json!({"message":"owned A"})));
        event(&mut a,json!({"type":"response","command":"prompt","success":true,"data":{"disposition":"started"}}));
        let owned=json!({"role":"user","content":"owned A"});
        event(&mut a,json!({"type":"message_start","message":owned}));
        event(&mut a,json!({"type":"message_end","message":owned}));
        let unrelated=json!({"role":"user","content":"native B, not sent by bridge"});
        if with_start {event(&mut a,json!({"type":"message_start","message":unrelated}));}
        event(&mut a,json!({"type":"message_end","message":unrelated}));
        let rows=entities(&a,"message");
        let text=|row:&&Value,t:&str|row["value"]["content"][0]["text"]==t;
        let owned_row=rows.iter().find(|row|text(row,"owned A")).unwrap();
        assert_eq!((&owned_row["value"]["origin"],&owned_row["value"]["delivery"]),(&origin,&json!("d-origin")),"A keeps its origin");
        let row=rows.iter().find(|row|text(row,"native B, not sent by bridge")).unwrap();
        assert!(row["value"].get("origin").is_none() && row["value"].get("delivery").is_none(),"uncorrelated native B with_start={with_start}: {row}");
        assert!(a.receipts.is_empty(),"correlation never writes receipts");
    }
}

/// A stray native user end merged into the still-open correlated message A replaces A's content: the entity then
/// holds text the bridge never wrote, so it loses the origin for good (never "B's words, A's sender").
#[test]
fn content_merged_into_an_open_correlated_message_drops_its_origin() {
    let mut a=make();
    let origin=json!({"principal":"p:owner","kind":"human"});
    a.origins.arm(origins::Entry::new("d-origin",&origin,&json!({"message":"owned A"})));
    event(&mut a,json!({"type":"response","command":"prompt","success":true,"data":{"disposition":"started"}}));
    event(&mut a,json!({"type":"message_start","message":{"role":"user","content":"owned A"}}));
    assert_eq!(entities(&a,"message")[0]["value"]["origin"],origin,"A is correlated while open");
    event(&mut a,json!({"type":"message_end","message":{"role":"user","content":"native B, not sent by bridge"}}));
    let rows=entities(&a,"message");assert_eq!(rows.len(),1,"B merged into the open entity");
    let value=&rows[0]["value"];assert_eq!(value["content"][0]["text"],"native B, not sent by bridge");
    assert!(value.get("origin").is_none() && value.get("delivery").is_none(),"{value}");
    event(&mut a,json!({"type":"message_start","message":{"role":"user","content":"owned A"}}));
    assert!(entities(&a,"message").iter().all(|row|row["value"].get("origin").is_none()),"the binding is gone");
    // Byte-identical text from another message (its own timestamp) merged into the open A: also no origin.
    let mut a=make();
    a.origins.arm(origins::Entry::new("d-origin",&origin,&json!({"message":"ok"})));
    event(&mut a,json!({"type":"response","command":"prompt","success":true,"data":{"disposition":"started"}}));
    event(&mut a,json!({"type":"message_start","message":{"role":"user","content":"ok","timestamp":1000}}));
    assert_eq!(entities(&a,"message")[0]["value"]["origin"],origin);
    event(&mut a,json!({"type":"message_end","message":{"role":"user","content":"ok","timestamp":2000}}));
    assert!(entities(&a,"message")[0]["value"].get("origin").is_none(),"identical text, another message");
    // The same message ending (same timestamp) keeps it.
    let mut a=make();
    a.origins.arm(origins::Entry::new("d-origin",&origin,&json!({"message":"ok"})));
    event(&mut a,json!({"type":"response","command":"prompt","success":true,"data":{"disposition":"started"}}));
    event(&mut a,json!({"type":"message_start","message":{"role":"user","content":"ok","timestamp":1000}}));
    event(&mut a,json!({"type":"message_end","message":{"role":"user","content":"ok","timestamp":1000}}));
    assert_eq!(entities(&a,"message")[0]["value"]["origin"],origin,"its own end keeps the origin");
}

/// The entity bound is checked after the correlated origin/delivery are merged; the content-omitted fallback keeps them.
#[test]
fn a_correlated_origin_never_pushes_a_message_over_the_entity_bound() {
    let origin=json!({"principal":"p:owner","kind":"human","actor":"extension:agents","initiator":"p:owner"});
    let mut over=0;
    for slack in (0..600).step_by(10) {
        let mut a=make();
        let text="x".repeat(state::ENTITY_BYTES-slack);
        a.origins.arm(origins::Entry::new("d-origin",&origin,&json!({"message":text})));
        event(&mut a,json!({"type":"response","command":"prompt","success":true,"data":{"disposition":"started"}}));
        let message=json!({"role":"user","content":text});
        event(&mut a,json!({"type":"message_start","message":message}));event(&mut a,json!({"type":"message_end","message":message}));
        let rows=entities(&a,"message");assert_eq!(rows.len(),1,"slack {slack}: the message entity exists");
        let value=&rows[0]["value"];
        assert!(vqro::limits::encoded_bytes(value)<=state::ENTITY_BYTES,"slack {slack}: entity over the bound");
        assert_eq!((&value["origin"],&value["delivery"]),(&origin,&json!("d-origin")),"slack {slack}: origin kept");
        if value.get("incomplete").is_some(){over+=1;}
    }
    assert!(over>0 && over<60,"some sizes need the content-omitted fallback, some fit whole: {over}");
}

#[test]
fn modules_are_versioned_descriptor_maps_for_resource_and_provider() {
    let a=make();
    for caps in [a.caps().clone(),json!(a.provider_capabilities())] {
        let modules=&caps["agent"]["modules"];
        assert!(modules.is_object(),"module lists are not part of agent-v0");
        assert_eq!(modules["stop"]["v"],0);
        assert_eq!(modules["queue"]["v"],0);
        assert!(modules.get("history").is_none(),"absence must not invent support");
        assert!(modules.as_object().unwrap().values().all(|descriptor|descriptor.is_object() && descriptor["v"].as_u64().is_some()));
    }
}

#[test]
fn message_deltas_are_append_records_and_materialize_identically() {
    let mut a=make();event(&mut a,json!({"type":"agent_start"}));
    event(&mut a,json!({"type":"message_start","message":{"role":"assistant","content":[]}}));
    event(&mut a,json!({"type":"message_update","message":{"role":"assistant","content":[{"type":"text","text":"he"}]},"assistantMessageEvent":{"type":"text_start","contentIndex":0}}));
    let after=json!({"epoch":a.epoch(),"seq":a.journal.seq()});
    let mut cursor=a.journal.open(Some(&after));
    event(&mut a,json!({"type":"message_update","message":{"role":"assistant","content":[{"type":"text","text":"hello"}]},"assistantMessageEvent":{"type":"text_delta","contentIndex":0,"delta":"llo"}}));
    let Next::Data(bytes)=cursor.next(&mut a.journal,65536) else {panic!("delta");};
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
fn two_incremental_snapshot_cuts_keep_overlapping_native_updates() {
    let mut a=make();for _ in 0..64 {message(&mut a,&"x".repeat(1800),"stop");}
    let mut one=a.journal.open(None);let mut two=a.journal.open(None);
    let mut v1=ItemView::new();let mut v2=ItemView::new();
    let Next::Data(first)=one.next(&mut a.journal,4096) else{panic!("snapshot piece");};v1.feed(&first);
    for _ in 0..40 {message(&mut a,"post-cut native update","stop");}
    for (cursor,view) in [(&mut one,&mut v1),(&mut two,&mut v2)] {
        for _ in 0..256 {match cursor.next(&mut a.journal,4096){Next::Data(bytes)=>{let _=view.feed(&bytes);},Next::Idle=>break,Next::Gap(reason)=>panic!("unexpected {reason}")}}
        assert_eq!(view.errors.total(),0);assert_eq!(view.digest(),a.journal.digest());
    }
}

#[test]
fn errors_unknown_reasons_and_native_end_are_not_fabricated_success() {
    let mut a=make();event(&mut a,json!({"type":"agent_start"}));
    event(&mut a,json!({"type":"message_end","message":{"role":"assistant","content":[],"stopReason":"error","errorMessage":"native failure"}}));
    event(&mut a,json!({"type":"agent_settled","aborted":false}));
    assert_eq!(a.facts("pi","private")["status"]["state"],"error");
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
    metadata(&mut a,"s2","all");let cut=a.epoch().to_string();a.ensure_session_epoch(&old);assert_eq!(a.epoch(),cut,"explicit session fence must not advance an already observed cut twice");
    assert_ne!(a.epoch(),old);assert_eq!(a.facts("pi","private")["ref"],endpoint);
    let begin:Value=serde_json::from_str(String::from_utf8(a.journal.snapshot()).unwrap().lines().next().unwrap()).unwrap();
    assert_eq!(begin["coverage"],"tail");
    assert!(entities(&a,"message").is_empty());assert_eq!(a.receipts["d1"]["stage"],"admitted");
    assert_eq!(a.caps()["agent.send"]["modes"]["boundary"]["batch"],"all");
    assert_eq!(a.provider_capabilities()["agent.send"]["modes"]["boundary"]["batch"],json!(["one","all"]));
}

#[test]
fn pending_dialogs_survive_pruning_and_dispatched_answers_do_not_invent_idle(){
    let mut a=make();
    event(&mut a,json!({"type":"extension_ui_request","id":"native-q","method":"select","title":"Choose","options":["one","two"]}));
    let summaries=a.request_summaries();let id=summaries[0]["request"].as_str().unwrap().to_string();
    assert_eq!(summaries[0]["choices"][1]["label"],"two");assert_eq!(a.state(),"requires_action");
    for _ in 0..600{message(&mut a,&"x".repeat(1500),"stop");}
    let entity=a.journal.entity("request",&id).unwrap().1.clone();
    let answer=a.requests.claim(&id,1,&json!({"choice":"c0"}),&entity).unwrap();assert_eq!(answer.wire["value"],"one");
    a.answer_written(&id,false);assert_eq!(a.state(),"unknown");assert!(a.request_summaries().is_empty());
    metadata(&mut a,"s1","one-at-a-time");assert_eq!(a.state(),"unknown","metadata alone is not an answer-resolution witness");
    a.answer_fenced(&id);assert_eq!(a.state(),"idle");
    assert_eq!(a.journal.entity("request",&id).unwrap().1["native"]["response"],"dispatched");
}

#[test]
fn fallback_is_derived_from_the_journal_and_native_retries_are_system_notices(){
    let mut a=make();message(&mut a,"native output","stop");event(&mut a,json!({"type":"auto_retry_start","attempt":2,"delayMs":125,"errorMessage":"native failure"}));
    let panel=a.transcript();assert!(panel.to_string().contains("assistant> native output"));assert!(panel.to_string().contains("system> Native retry scheduled (attempt 2) after 125ms"));
    let notice=entities(&a,"message").into_iter().find(|row|row["value"]["role"]=="system").unwrap();let id=notice["id"].as_str().unwrap();a.journal.remove("message",id).unwrap();assert!(!a.transcript().to_string().contains("Native retry scheduled"),"fallback cannot retain its own copy after canonical removal");
    a.view=Some("agent-r1".into());assert_eq!(a.facts("pi","private")["data"]["views"],json!(["agent-r1"]));
}

#[test]
fn incremental_and_final_tool_media_use_the_same_standard_projection_as_messages(){
    let mut a=make();event(&mut a,json!({"type":"tool_execution_start","toolCallId":"media","toolName":"image-tool","args":{}}));
    for (kind,field) in [("tool_execution_update","partialResult"),("tool_execution_end","result")]{
        let mut record=json!({"type":kind,"toolCallId":"media","toolName":"image-tool","isError":false});record[field]=json!({"content":[{"type":"image","mimeType":"image/png","data":"AQ==","extra":"preserved"}]});event(&mut a,record);
        let tools=entities(&a,"tool");let block=&tools[0]["value"]["content"][0];assert_eq!(block["mime"],"image/png");assert!(block.get("mimeType").is_none());assert_eq!(block["data"],"AQ==");assert_eq!(block["extra"],"preserved");
    }
}

#[test]
fn unresolved_configuration_never_publishes_stale_batch_or_image_semantics(){
    let mut a=make();let rev=a.caps()["rev"].clone();assert!(a.caps()["agent.send"]["content"]["types"].as_array().unwrap().iter().any(|v|v=="image"));
    a.configuration_pending(true);assert_eq!(a.state(),"unknown");assert!(a.caps()["agent.send"]["modes"].get("boundary").is_none());
    assert_eq!(a.caps()["agent.send"]["content"]["types"],json!(["text"]));assert_ne!(a.caps()["rev"],rev);
    metadata(&mut a,"s1","all");assert_eq!(a.state(),"unknown");
    a.configuration_pending(false);assert_eq!(a.state(),"idle");assert_eq!(a.caps()["agent.send"]["modes"]["boundary"]["batch"],"all");
}

#[test]
fn timed_dialog_cancel_is_once_and_notifications_are_not_questions(){
    let mut a=make();
    event(&mut a,json!({"type":"extension_ui_request","id":"timed","method":"confirm","title":"T","message":"?","timeout":10}));
    let cancels=a.take_ui_cancels();assert_eq!(cancels.len(),1);assert!(a.take_ui_cancels().is_empty());assert_eq!(a.state(),"unknown");
    a.answer_fenced(&cancels[0].0);assert_eq!(a.state(),"idle");
    event(&mut a,json!({"type":"extension_ui_request","id":"notice","method":"notify","message":"FYI"}));
    assert!(a.take_ui_cancels().is_empty());assert!(a.request_summaries().is_empty());assert!(a.transcript().to_string().contains("system> FYI"));
}

#[test]
fn notify_is_a_standard_notice_and_other_fire_and_forget_ui_stays_private(){
    let mut a=make();
    for (id,kind) in [("n-warn",json!("warning")),("n-err",json!("error")),("n-info",json!("info")),("n-future",json!("success"))]{
        event(&mut a,json!({"type":"extension_ui_request","id":id,"method":"notify","message":format!("{id} text\nsecond line"),"notifyType":kind}));
    }
    event(&mut a,json!({"type":"extension_ui_request","id":"n-plain","method":"notify","message":"plain"}));
    event(&mut a,json!({"type":"extension_ui_request","id":"status","method":"setStatus","statusKey":"k","statusText":"private status"}));
    event(&mut a,json!({"type":"extension_ui_request","id":"n-empty","method":"notify"}));
    let notices=entities(&a,"notice");
    let values:Vec<&Value>=notices.iter().map(|row|&row["value"]).collect();
    assert_eq!(values.len(),5,"every notify with text is a notice: {values:?}");
    assert_eq!(values.iter().map(|v|v["severity"].clone()).collect::<Vec<_>>(),vec![json!("warn"),json!("error"),json!("info"),Value::Null,Value::Null],
        "severity only from a known Pi notifyType; never invented");
    assert_eq!(values[0]["text"],"n-warn text\nsecond line");assert_eq!(values[4]["text"],"plain");
    assert_eq!(values[0]["native"],json!({"method":"notify","notifyType":"warning"}));
    assert_eq!(values[3]["native"],json!({"method":"notify","notifyType":"success"}),"an unknown native type is kept, not mapped");
    assert_eq!(values[4]["native"],json!({"method":"notify"}));
    let positions:Vec<u64>=values.iter().map(|v|v["position"].as_u64().unwrap()).collect();
    assert!(positions.windows(2).all(|w|w[0]<w[1]),"notices are placed on the timeline in arrival order");
    let private=entities(&a,"_ui");
    assert_eq!(private.len(),2,"setStatus and a notify without text stay private: {private:?}");
    assert!(private.iter().all(|row|row["value"]["method"]=="setStatus" || row["value"]["message"].is_null()));
    assert!(a.take_ui_cancels().is_empty());assert!(a.request_summaries().is_empty());
    let transcript=a.transcript().to_string();
    assert!(transcript.contains("system> n-warn text") && transcript.contains("system> second line") && transcript.contains("system> plain"));
    assert!(!transcript.contains("private status"));
}

#[test]
fn native_queue_membership_is_observed_not_inferred_from_admission() {
    let mut a=make();
    event(&mut a,json!({"type":"queue_update","steering":["queued"],"followUp":[]}));
    assert_eq!(a.queue.as_ref().unwrap()[0]["preview"],"queued");
    a.receipt("d",json!({"stage":"admitted"}));
    assert!(entities(&a,"message").is_empty());
    a.invalidate_queue();assert!(a.queue.is_none(),"another native input invalidates old membership");
    event(&mut a,json!({"type":"response","command":"get_state","success":true,"data":{"pendingMessageCount":0}}));
    assert_eq!(a.queue,Some(json!([])));
    a.control_result("op",json!({"outcome":"observed"}));assert!(a.journal.entity("_control","op").is_some());
}

#[test]
fn missing_idle_evidence_and_oversized_native_output_are_explicit() {
    let mut a=Agent::new("private".into(),Signals::new());assert_eq!(a.state(),"unknown");
    assert_eq!(a.facts("pi","private")["status"]["state"],"working");
    assert!(a.facts("pi","private")["status"].get("phase").is_none());
    let mut m=make();message(&mut m,&"x".repeat(100000),"stop");
    let messages=entities(&m,"message");assert_eq!(messages[0]["value"]["incomplete"],json!(["content"]));
    assert!(m.incomplete.contains("items"));
    assert!(m.journal.entity_bytes()<=state::ENTITY_TOTAL);
    event(&mut a,json!({"type":"agent_settled","aborted":true}));assert!(a.completion.is_none(),"settled without a run is not completion evidence");
}

fn native_session(agent:&mut Agent,id:&str,count:Value){event(agent,json!({"type":"response","command":"get_state","success":true,"data":{"sessionId":id,"messageCount":count,"model":{"id":"fixture","contextWindow":4096},"isStreaming":false,"isCompacting":false}}));}
fn coverage(agent:&Agent)->Value{serde_json::from_str::<Value>(String::from_utf8(agent.journal.snapshot()).unwrap().lines().next().unwrap()).unwrap()["coverage"].clone()}
fn stats()->Value{json!({"sessionId":"s1","tokens":{"input":11,"output":2,"cacheRead":3,"cacheWrite":4,"total":99999},"contextUsage":{"tokens":null,"contextWindow":4096,"percent":null},"cost":0.0123})}
fn usage(agent:&mut Agent,data:Option<&Value>){let epoch=agent.epoch().to_owned();agent.usage_result(&epoch,agent.usage_revision(),data);}

#[test]
fn native_user_timestamp_never_invents_a_sender_or_delivery(){
    let mut a=make();event(&mut a,json!({"type":"message_end","message":{"role":"user","timestamp":1795190400123u64,"content":"literal","origin":{"principal":"owner","kind":"human"}}}));
    let rows=entities(&a,"message");let user=&rows[0]["value"];
    assert_eq!(user["at_ms"],1795190400123u64);assert_eq!(user["content"][0]["text"],"literal");
    assert!(user.get("origin").is_none());assert!(user.get("delivery").is_none());
    for timestamp in [Value::Null,json!(-1),json!("1795190400123"),json!(1u64<<53)]{
        let mut a=make();event(&mut a,json!({"type":"message_end","message":{"role":"user","timestamp":timestamp,"content":"literal"}}));assert!(entities(&a,"message")[0]["value"].get("at_ms").is_none());
    }
}
#[test]
fn assistant_and_other_native_message_roles_keep_their_timestamp_even_when_omitted(){
    for role in ["assistant","system","_future_role"]{
        for text in ["reply".to_owned(),"x".repeat(100000)]{
            let mut a=make();event(&mut a,json!({"type":"message_end","message":{"role":role,"timestamp":1795190400123u64,"content":text}}));
            let rows=entities(&a,"message");let value=&rows[0]["value"];
            assert_eq!(value["at_ms"],1795190400123u64);assert_eq!(value["role"],role);
            assert!(value.get("origin").is_none());assert!(value.get("delivery").is_none());
            if text.len()>state::ENTITY_BYTES{assert_eq!(value["incomplete"],json!(["content"]));}
        }
    }
}
#[test]
fn assistant_missing_or_invalid_native_time_is_not_replaced_with_receive_time(){
    for timestamp in [Value::Null,json!(-1),json!("1795190400123"),json!(1.25),json!(1u64<<53)]{
        let mut a=make();event(&mut a,json!({"type":"message_end","message":{"role":"assistant","timestamp":timestamp,"content":"reply"}}));
        assert!(entities(&a,"message")[0]["value"].get("at_ms").is_none());
    }
}
#[test]
fn session_cwd_is_native_or_the_known_controlled_launch_directory(){
    let mut a=Agent::with_cwd("private".into(),Signals::new(),"/private/launch".into());native_session(&mut a,"s1",json!(0));
    assert_eq!(a.session.as_ref().unwrap()["cwd"],"/private/launch");
    assert_eq!(entities(&a,"session")[0]["value"]["cwd"],"/private/launch");
    assert_eq!(a.facts("pi","/private/launch")["data"]["agent"]["session"]["cwd"],"/private/launch");
    event(&mut a,json!({"type":"response","command":"get_state","success":true,"data":{"sessionId":"s1","cwd":"/native/reported"}}));assert_eq!(a.session.as_ref().unwrap()["cwd"],"/native/reported");
}
#[test]
fn native_usage_preserves_unknown_occupancy_and_unknown_cost_denominations(){
    let mut a=make();native_session(&mut a,"s1",json!(0));usage(&mut a,Some(&stats()));
    let facts=a.facts("pi","private");let value=&facts["data"]["agent"]["usage"];
    assert_eq!(value["context"],json!({"window":4096}));assert_eq!(value["tokens"],json!({"input":11,"output":2,"cache_read":3,"cache_write":4}));
    assert!(value.get("cost").is_none());assert!(value.get("_pi_cost").is_none());assert!(a.incomplete.contains("usage.cost"));
    let mut data=stats();data["contextUsage"]["tokens"]=json!(123);usage(&mut a,Some(&data));assert_eq!(a.facts("pi","private")["data"]["agent"]["usage"]["context"]["used"],123);
    data.as_object_mut().unwrap().remove("contextUsage");usage(&mut a,Some(&data));assert_eq!(a.facts("pi","private")["data"]["agent"]["usage"]["context"],json!({"window":4096}),"never substitute cumulative tokens for occupancy");
    usage(&mut a,None);assert!(a.facts("pi","private")["data"]["agent"]["usage"].is_null());
}
#[test]
fn stale_session_or_revision_usage_cannot_overwrite_a_newer_snapshot(){
    let mut a=make();native_session(&mut a,"s1",json!(0));let epoch=a.epoch().to_owned();let old=a.usage_revision();
    a.configuration_pending(true);a.usage_result(&epoch,old,Some(&stats()));assert!(a.facts("pi","private")["data"]["agent"]["usage"].is_null());
    a.configuration_pending(false);usage(&mut a,Some(&stats()));let good=a.facts("pi","private")["data"]["agent"]["usage"].clone();
    let mut late=stats();late["tokens"]["input"]=json!(888);a.usage_result(&epoch,old,Some(&late));assert_eq!(a.facts("pi","private")["data"]["agent"]["usage"],good);
    native_session(&mut a,"s2",json!(0));a.usage_result(&epoch,old,Some(&late));usage(&mut a,Some(&late));assert!(a.facts("pi","private")["data"]["agent"]["usage"].is_null(),"foreign session data is not current usage");
}
#[test]
fn full_history_requires_an_evidenced_empty_native_session(){
    let mut a=Agent::new("private".into(),Signals::new());assert_eq!(coverage(&a),"tail");
    native_session(&mut a,"s1",json!(0));assert_eq!(coverage(&a),"all_retained");assert_eq!(a.facts("pi","private")["data"]["agent"]["items"]["coverage"],coverage(&a));
    native_session(&mut a,"loaded",json!(3));assert_eq!(coverage(&a),"tail");native_session(&mut a,"loaded",json!(0));assert_eq!(coverage(&a),"tail","zero at a later observation is not proof of earlier completeness");
    native_session(&mut a,"new",json!(0));assert_eq!(coverage(&a),"all_retained");let epoch=a.epoch().to_owned();a.ensure_session_epoch(&epoch);assert_eq!(coverage(&a),"tail","new observation epoch/command ACK alone does not prove history");
}
#[test]
fn pruning_or_omission_downgrades_full_history_but_new_empty_session_recovers(){
    let mut a=make();native_session(&mut a,"fresh",json!(0));assert_eq!(coverage(&a),"all_retained");
    for _ in 0..600{message(&mut a,&"x".repeat(1500),"stop");}assert_eq!(coverage(&a),"tail");assert_eq!(a.facts("pi","private")["data"]["agent"]["items"]["coverage"],"tail");
    native_session(&mut a,"fresh",json!(0));assert_eq!(coverage(&a),"tail");native_session(&mut a,"next",json!(0));assert_eq!(coverage(&a),"all_retained");
    message(&mut a,&"x".repeat(100000),"stop");assert_eq!(coverage(&a),"tail");native_session(&mut a,"after-omission",json!(0));assert_eq!(coverage(&a),"all_retained");
}
