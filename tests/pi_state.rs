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
#[allow(dead_code)] #[path="../src/bin/vqro-bridge-pi/startup.rs"] mod startup;
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
        let owned=json!({"role":"user","content":"owned A","timestamp":1000});
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
    event(&mut a,json!({"type":"message_start","message":{"role":"user","content":"owned A","timestamp":1000}}));
    assert_eq!(entities(&a,"message")[0]["value"]["origin"],origin,"A is correlated while open");
    event(&mut a,json!({"type":"message_end","message":{"role":"user","content":"native B, not sent by bridge"}}));
    let rows=entities(&a,"message");assert_eq!(rows.len(),1,"B merged into the open entity");
    let value=&rows[0]["value"];assert_eq!(value["content"][0]["text"],"native B, not sent by bridge");
    assert!(value.get("origin").is_none() && value.get("delivery").is_none(),"{value}");
    event(&mut a,json!({"type":"message_start","message":{"role":"user","content":"owned A","timestamp":1000}}));
    assert!(entities(&a,"message").iter().all(|row|row["value"].get("origin").is_none()),"the binding is gone");
    // Byte-identical text from another message (its own timestamp): its own entity, without origin; A keeps A's.
    let mut a=make();
    a.origins.arm(origins::Entry::new("d-origin",&origin,&json!({"message":"ok"})));
    event(&mut a,json!({"type":"response","command":"prompt","success":true,"data":{"disposition":"started"}}));
    event(&mut a,json!({"type":"message_start","message":{"role":"user","content":"ok","timestamp":1000}}));
    assert_eq!(entities(&a,"message")[0]["value"]["origin"],origin);
    event(&mut a,json!({"type":"message_end","message":{"role":"user","content":"ok","timestamp":2000}}));
    let rows=entities(&a,"message");assert_eq!(rows.len(),2,"another message, another entity: {rows:?}");
    assert_eq!(rows[0]["value"]["origin"],origin,"A keeps its origin");
    assert!(rows[1]["value"].get("origin").is_none(),"identical text, another message");
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
        let message=json!({"role":"user","content":text,"timestamp":1000});
        event(&mut a,json!({"type":"message_start","message":message}));event(&mut a,json!({"type":"message_end","message":message}));
        let rows=entities(&a,"message");assert_eq!(rows.len(),1,"slack {slack}: the message entity exists");
        let value=&rows[0]["value"];
        assert!(vqro::limits::encoded_bytes(value)<=state::ENTITY_BYTES,"slack {slack}: entity over the bound");
        assert_eq!((&value["origin"],&value["delivery"]),(&origin,&json!("d-origin")),"slack {slack}: origin kept");
        if value.get("incomplete").is_some(){over+=1;}
    }
    assert!(over>0 && over<60,"some sizes need the content-omitted fallback, some fit whole: {over}");
}

// spike-rev-b review controls on 06c8004f (R1 append path, R2 unknown timestamp).
#[test]
fn uncertain_update_cannot_keep_origin_via_text_append() {
    let mut a = make();
    let origin = json!({"principal":"p:owner","kind":"human"});
    a.origins.arm(origins::Entry::new("d-origin", &origin, &json!({"message":"hello"})));
    event(&mut a, json!({"type":"response","command":"prompt","success":true,
                        "data":{"disposition":"started"}}));
    event(&mut a, json!({"type":"message_start","message":{"role":"user",
                        "content":[{"type":"text","text":"hello"}],"timestamp":1000}}));
    assert_eq!(entities(&a,"message")[0]["value"]["origin"], origin);
    // A delta with no timestamp cannot be told apart from A, so it merges into the open entity (role alone matches):
    // the binding drops (no valid timestamp) and the entity must be re-put, not appended with A's provenance kept.
    event(&mut a, json!({"type":"message_update","message":{"role":"user",
                        "content":[{"type":"text","text":"hello world"}]},
                        "assistantMessageEvent":{"type":"text_delta","contentIndex":0,"delta":" world"}}));
    let rows = entities(&a,"message");
    assert_eq!(rows.len(),1);
    let value = &rows[0]["value"];
    assert_eq!(value["content"][0]["text"],"hello world");
    assert!(value.get("origin").is_none() && value.get("delivery").is_none(),
            "dropped binding must re-put, not append while retaining old top-level provenance: {value}");
}
#[test]
fn missing_timestamp_is_not_same_message_identity() {
    let mut a = make();
    let origin = json!({"principal":"p:owner","kind":"human"});
    a.origins.arm(origins::Entry::new("d-origin", &origin, &json!({"message":"same text"})));
    event(&mut a, json!({"type":"response","command":"prompt","success":true,
                        "data":{"disposition":"started"}}));
    event(&mut a, json!({"type":"message_start","message":{"role":"user","content":"same text"}}));
    // An unqualified standalone end: no timestamp distinguishes it from an unrelated identical message.
    event(&mut a, json!({"type":"message_end","message":{"role":"user","content":"same text"}}));
    let rows = entities(&a,"message");
    assert!(rows[0]["value"].get("origin").is_none() && rows[0]["value"].get("delivery").is_none(),
            "missing==missing must not qualify continuation identity: {}", rows[0]["value"]);
}

/// A stray update/end for another message (any role) never writes into the open entity: it gets its own.
#[test]
fn a_record_for_another_message_never_writes_into_the_open_one() {
    let text=|v:&Value|v["value"]["content"][0]["text"].as_str().unwrap_or("").to_string();
    for (b_role,b_time,b_text) in [("user",2000,"user B"),("assistant",2000,"assistant B"),("assistant",1000,"assistant B same time")] {
        for final_b in [false,true] {
            let mut a=make();
            event(&mut a,json!({"type":"message_start","message":{"role":"user","content":[{"type":"text","text":"A"}],"timestamp":1000}}));
            let b=json!({"role":b_role,"content":[{"type":"text","text":b_text}],"timestamp":b_time});
            if final_b {event(&mut a,json!({"type":"message_end","message":b}));}
            else {event(&mut a,json!({"type":"message_update","message":b,"assistantMessageEvent":{"type":"text_delta","contentIndex":0,"delta":b_text}}));}
            let rows=entities(&a,"message");
            let a_row=rows.iter().find(|r|r["id"]=="m1").unwrap();
            assert_eq!((text(a_row),&a_row["value"]["role"]),("A".to_string(),&json!("user")),"{b_role}/{b_time}/final={final_b}: A untouched: {rows:?}");
            if b_role=="user" && b_time==1000 {continue;}
            assert!(rows.iter().any(|r|r["id"]!="m1" && text(r)==b_text && r["value"]["role"]==b_role),"{b_role}/{b_time}: B has its own entity: {rows:?}");
        }
    }
    // The same message's own update/end (same role and timestamp) still writes into its entity.
    let mut a=make();
    event(&mut a,json!({"type":"message_start","message":{"role":"assistant","content":[{"type":"text","text":"he"}],"timestamp":1000}}));
    event(&mut a,json!({"type":"message_update","message":{"role":"assistant","content":[{"type":"text","text":"hello"}],"timestamp":1000},"assistantMessageEvent":{"type":"text_delta","contentIndex":0,"delta":"llo"}}));
    event(&mut a,json!({"type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":"hello"}],"timestamp":1000,"stopReason":"stop"}}));
    let rows=entities(&a,"message");assert_eq!(rows.len(),1);assert_eq!(text(&rows[0]),"hello");
}

/// as-reviewer's control on 44a8d0ec: a stray record between A's start and A's end does not lose A: A's later
/// update/end continue A's entity, and B stays its own.
#[test]
fn an_interleaved_stray_record_does_not_lose_the_open_message() {
    let mut a=make();
    event(&mut a,json!({"type":"message_start","message":{"role":"assistant","content":[{"type":"text","text":"he"}],"timestamp":1000}}));
    event(&mut a,json!({"type":"message_end","message":{"role":"user","content":[{"type":"text","text":"B"}],"timestamp":2000}}));
    event(&mut a,json!({"type":"message_update","message":{"role":"assistant","content":[{"type":"text","text":"hello"}],"timestamp":1000},"assistantMessageEvent":{"type":"text_delta","contentIndex":0,"delta":"llo"}}));
    event(&mut a,json!({"type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":"hello"}],"timestamp":1000,"stopReason":"stop"}}));
    let rows=entities(&a,"message");
    assert_eq!(rows.len(),2,"{rows:?}");
    let m1=rows.iter().find(|r|r["id"]=="m1").unwrap();
    assert_eq!((&m1["value"]["content"][0]["text"],&m1["value"]["final"],&m1["value"]["role"]),(&json!("hello"),&json!(true),&json!("assistant")),"{rows:?}");
    assert!(rows.iter().any(|r|r["id"]!="m1" && r["value"]["content"][0]["text"]=="B" && r["value"]["role"]=="user"));
}

/// as-reviewer R1 on 1fc694be: a stray message opening its own entity must not unbind A, and A returning with
/// changed text must not keep A's old published origin through the append path.
#[test]
fn a_stray_message_never_rebinds_and_a_changed_return_never_keeps_the_origin() {
    let mut a=make();
    let origin=json!({"principal":"p:owner","kind":"human"});
    a.origins.arm(origins::Entry::new("d-origin",&origin,&json!({"message":"hello"})));
    event(&mut a,json!({"type":"response","command":"prompt","success":true,"data":{"disposition":"started"}}));
    event(&mut a,json!({"type":"message_start","message":{"role":"user","content":[{"type":"text","text":"hello"}],"timestamp":1000}}));
    event(&mut a,json!({"type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":"B"}],"timestamp":2000}}));
    assert_eq!(entities(&a,"message").iter().find(|r|r["id"]=="m1").unwrap()["value"]["origin"],origin,"B does not unbind A");
    event(&mut a,json!({"type":"message_update","message":{"role":"user","content":[{"type":"text","text":"hello world"}],"timestamp":1000},
        "assistantMessageEvent":{"type":"text_delta","contentIndex":0,"delta":" world"}}));
    let m1=entities(&a,"message").into_iter().find(|r|r["id"]=="m1").unwrap();
    assert_eq!(m1["value"]["content"][0]["text"],"hello world");
    assert!(m1["value"].get("origin").is_none() && m1["value"].get("delivery").is_none(),"changed text keeps no origin: {m1}");
}

/// as-reviewer R2 on 1fc694be: a newer same-role message without a timestamp does not shadow an open message with an
/// exact identity.
#[test]
fn an_exact_identity_wins_over_a_timestamp_missing_message() {
    let mut a=make();
    event(&mut a,json!({"type":"message_start","message":{"role":"assistant","content":[{"type":"text","text":"he"}],"timestamp":1000}}));
    event(&mut a,json!({"type":"message_start","message":{"role":"assistant","content":[{"type":"text","text":"B"}]}}));
    event(&mut a,json!({"type":"message_update","message":{"role":"assistant","content":[{"type":"text","text":"hello"}],"timestamp":1000},
        "assistantMessageEvent":{"type":"text_delta","contentIndex":0,"delta":"llo"}}));
    event(&mut a,json!({"type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":"hello"}],"timestamp":1000,"stopReason":"stop"}}));
    let rows=entities(&a,"message");
    let text=|id:&str|rows.iter().find(|r|r["id"]==id).unwrap()["value"]["content"][0]["text"].clone();
    assert_eq!((text("m1"),text("m2")),(json!("hello"),json!("B")),"{rows:?}");
}

/// vqro-lead-secondary's #129 control: an ignored toolResult start/end never splits the open assistant message.
#[test]
fn an_ignored_tool_result_start_does_not_split_the_open_message() {
    let mut a=make();
    event(&mut a,json!({"type":"message_start","message":{"role":"assistant","content":[{"type":"text","text":"he"}],"timestamp":1000}}));
    let result=json!({"role":"toolResult","content":[{"type":"text","text":"ok"}],"timestamp":1500});
    event(&mut a,json!({"type":"message_start","message":result}));event(&mut a,json!({"type":"message_end","message":result}));
    event(&mut a,json!({"type":"message_update","message":{"role":"assistant","content":[{"type":"text","text":"hello"}],"timestamp":1000},"assistantMessageEvent":{"type":"text_delta","contentIndex":0,"delta":"llo"}}));
    event(&mut a,json!({"type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":"hello"}],"timestamp":1000,"stopReason":"stop"}}));
    let rows=entities(&a,"message");
    assert_eq!(rows.len(),1,"{rows:?}");
    assert_eq!((&rows[0]["value"]["content"][0]["text"],&rows[0]["value"]["final"]),(&json!("hello"),&json!(true)));
}

/// Part A C1: the provider registration bounds what the resource may declare; boundary/settle idle is start in both.
#[test]
fn provider_queue_mode_bounds_cover_the_resource_declaration() {
    let a=make();
    let provider=json!(a.provider_capabilities());
    for mode in ["boundary","settle"] {
        let declared=&a.caps()["agent.send"]["modes"][mode];
        let bound=&provider["agent.send"]["modes"][mode];
        assert_eq!(declared["idle"],"start","{mode}");
        assert!(bound["idle"].as_array().is_some_and(|values|values.contains(&declared["idle"])),"{mode}: provider idle {} must allow {}",bound["idle"],declared["idle"]);
    }
}

/// agent-v0 7 availability: blocked only between Pi's auto_retry_start and auto_retry_end; declared in caps.
#[test]
fn pi_retry_publishes_blocked_until_its_native_end() {
    let mut a=make();
    let facts=|a:&Agent|a.facts("p","/w")["data"]["agent"]["availability"].clone();
    assert_eq!(a.caps()["agent.availability"],json!({"reasons":["_retrying"]}));
    assert!(json!(a.provider_capabilities())["agent.availability"]["reasons"].as_array().unwrap().contains(&json!("_retrying")));
    assert_eq!(facts(&a),Value::Null,"unknown until Pi reports");
    event(&mut a,json!({"type":"auto_retry_start","attempt":1,"maxAttempts":3,"delayMs":2000,"errorMessage":"429 rate limit"}));
    let blocked=facts(&a);
    assert_eq!((&blocked["state"],&blocked["reason"],&blocked["retry_after_ms"]),(&json!("blocked"),&json!("_retrying"),&json!(2000)));
    assert_eq!(blocked["native"]["errorMessage"],"429 rate limit");
    event(&mut a,json!({"type":"auto_retry_end","success":false,"attempt":3,"finalError":"still failing"}));
    assert_eq!(facts(&a)["state"],"available","Pi's retry end clears, success or not");
    // A new epoch forgets it.
    event(&mut a,json!({"type":"auto_retry_start","attempt":1,"maxAttempts":3,"delayMs":10,"errorMessage":"overloaded"}));
    a.new_session_epoch();assert_eq!(facts(&a),Value::Null);
    // Text alone (a final error with no retry) never blocks.
    let mut b=make();
    event(&mut b,json!({"type":"message_end","message":{"role":"assistant","content":[],"stopReason":"error","errorMessage":"rate limit exceeded"}}));
    assert_eq!(facts(&b),Value::Null,"no inference from error text");
}

/// lead-secondary's control on ec04745f: Pi's small native duration survives a tool entity whose content is omitted.
#[test]
fn native_duration_survives_omitted_oversize_tool_content() {
    let mut a=make();
    event(&mut a,json!({"type":"tool_execution_start","toolCallId":"large","toolName":"bash","args":{}}));
    event(&mut a,json!({"type":"tool_execution_end","toolCallId":"large","toolName":"bash","isError":false,"durationMs":1234,
        "result":{"content":[{"type":"text","text":"x".repeat(state::ENTITY_BYTES+1024)}]}}));
    let value=entities(&a,"tool").last().unwrap()["value"].clone();
    assert!(value.get("incomplete").is_some(),"the content was omitted: {value}");
    assert_eq!(value["duration_ms"],1234,"known small native duration must not disappear when output is omitted");
}

/// Pi's own tool_execution_end.durationMs becomes duration_ms; nothing else is timed, and nothing is invented.
#[test]
fn pi_tool_duration_is_the_native_duration_ms() {
    let mut a=make();
    let tool=|a:&Agent,title:&str|entities(a,"tool").into_iter().map(|r|r["value"].clone()).find(|v|v["title"]==title).unwrap();
    event(&mut a,json!({"type":"tool_execution_start","toolCallId":"c1","toolName":"bash","args":{}}));
    assert!(tool(&a,"bash").get("duration_ms").is_none());
    event(&mut a,json!({"type":"tool_execution_end","toolCallId":"c1","toolName":"bash","result":{"content":[]},"isError":false,"durationMs":1234}));
    let done=tool(&a,"bash");
    assert_eq!(done["duration_ms"],1234);
    assert!(done.get("started_ms").is_none() && done.get("ended_ms").is_none() && done.get("times").is_none(),"{done}");
    for (id,name,value) in [("c2","none",Value::Null),("c3","negative",json!(-5)),("c4","fraction",json!(1.5)),("c5","text",json!("9"))] {
        let mut end=json!({"type":"tool_execution_end","toolCallId":id,"toolName":name,"result":{"content":[]},"isError":false});
        if !value.is_null(){end["durationMs"]=value;}
        event(&mut a,end);
        assert!(tool(&a,name).get("duration_ms").is_none(),"{name}");
    }
}

/// as-reviewer R2/R3 on f50a03d3: the start-up model is the first observation; a failed get_commands is incomplete.
#[test]
fn startup_model_is_the_first_observation_and_failed_inventories_are_incomplete() {
    let startup=|a:&Agent|entities(a,"startup").into_iter().find(|r|r["id"]=="current").unwrap()["value"].clone();
    let mut a=Agent::with_cwd("private".into(),Signals::new(),"/w".into());
    let state=|model:&str|json!({"type":"response","id":"s","command":"get_state","success":true,"data":{"sessionId":"S","isStreaming":false,
        "isCompacting":false,"steeringMode":"all","followUpMode":"all","model":{"id":model}}});
    event(&mut a,state("modelA"));event(&mut a,state("modelB"));
    assert_eq!(startup(&a)["model"],"modelA","start-up model is not rewritten by later get_state");
    event(&mut a,json!({"type":"response","id":"c","command":"get_commands","success":false,"error":"boom"}));
    let value=startup(&a);
    for field in ["skills","prompts","commands"] {assert!(value.get(field).is_none() && value["incomplete"].as_array().unwrap().contains(&json!(field)),"{field}: {value}");}
}

/// Startup strings are never shortened (spec e8232d24): an over-bound path/description is omitted from its entry, an
/// over-bound name omits the entry, an over-long list loses entries from its end; each field is named incomplete.
#[test]
fn startup_never_shortens_a_string_or_path() {
    let startup=|a:&Agent|entities(a,"startup").into_iter().find(|r|r["id"]=="current").unwrap()["value"].clone();
    let mut a=Agent::with_cwd("private".into(),Signals::new(),"/w".into());
    let long_path=format!("/{}", "p".repeat(5000));
    let mut rows=vec![
        json!({"name":"skill:long-path","source":"skill","sourceInfo":{"path":long_path}}),
        json!({"name":"skill:long-desc","description":"d".repeat(2000),"source":"skill","sourceInfo":{"path":"/ok/SKILL.md"}}),
        json!({"name":format!("skill:{}","n".repeat(200)),"source":"skill"}),
    ];
    for i in 0..300 {rows.push(json!({"name":format!("p{i}"),"source":"prompt"}));}
    event(&mut a,json!({"type":"response","id":"c","command":"get_commands","success":true,"data":{"commands":rows}}));
    let value=startup(&a);
    assert_eq!(value["skills"],json!([{"name":"long-path"},{"name":"long-desc","path":"/ok/SKILL.md"}]),"omitted, never shortened");
    assert_eq!(value["prompts"].as_array().unwrap().len(),256);assert_eq!(value["prompts"][255]["name"],"p255","entries lost from the end");
    let incomplete=value["incomplete"].as_array().unwrap();
    assert!(incomplete.contains(&json!("skills")) && incomplete.contains(&json!("prompts")));
    let text=value.to_string();assert!(!text.contains(&"p".repeat(4097)) && !text.contains(&"d".repeat(1025)),"no shortened string published");
    // A vqro fact always carries its source.
    assert_eq!((&value["cwd"],&value["sources"]["cwd"]),(&json!("/w"),&json!("vqro:launch")));
}

/// agent-v0 startup (id current): only Pi's own reports, each with its source; absent stays absent.
#[test]
fn startup_entity_carries_only_pis_own_reports_with_sources() {
    let startup=|a:&Agent|entities(a,"startup").into_iter().find(|r|r["id"]=="current").unwrap()["value"].clone();
    let mut a=Agent::with_cwd("private".into(),Signals::new(),"/work/repo".into());
    let first=startup(&a);
    assert_eq!((&first["runtime"],&first["cwd"],&first["sources"]["cwd"]),(&json!({"name":"Pi"}),&json!("/work/repo"),&json!("vqro:launch")));
    for absent in ["model","skills","instructions","plugins","mcp"] {assert!(first.get(absent).is_none(),"{absent} not reported yet");}
    event(&mut a,json!({"type":"response","id":"s","command":"get_state","success":true,"data":{"sessionId":"s1","isStreaming":false,"isCompacting":false,
        "steeringMode":"all","followUpMode":"all","model":{"provider":"p","id":"gpt-x","input":["text"]}}}));
    event(&mut a,json!({"type":"response","id":"c","command":"get_commands","success":true,"data":{"commands":[
        {"name":"skill:review","description":"Review code","source":"skill","sourceInfo":{"path":"/home/u/.pi/skills/review/SKILL.md","scope":"user"}},
        {"name":"fix","description":"Fix it","source":"prompt","sourceInfo":{"path":"/repo/.pi/prompts/fix.md"}},
        {"name":"deploy","source":"extension","sourceInfo":{"path":"x".repeat(5000)}},
        {"name":"odd","source":"something-else"}]}}));
    a.startup_version("1.5.2\n");
    let value=startup(&a);
    assert_eq!(value["runtime"],json!({"name":"Pi","version":"1.5.2","version_source":"cli:--version"}));
    assert_eq!((&value["model"],&value["sources"]["model"]),(&json!("gpt-x"),&json!("rpc:get_state")));
    assert_eq!(value["skills"],json!([{"name":"review","description":"Review code","path":"/home/u/.pi/skills/review/SKILL.md"}]));
    assert_eq!(value["skills_scope"],"available","Pi lists skills it can invoke; bodies load later");
    assert_eq!(value["prompts"],json!([{"name":"fix","description":"Fix it","path":"/repo/.pi/prompts/fix.md"}]));
    assert_eq!(value["commands"],json!([{"name":"deploy","kind":"extension"}]),"an over-bound path is cut, never truncated");
    assert_eq!(value["incomplete"],json!(["commands"]));
    assert_eq!(value["sources"]["skills"],"rpc:get_commands");
    for absent in ["instructions","plugins","mcp"] {assert!(value.get(absent).is_none(),"{absent}: Pi does not report it over RPC");}
    // A new epoch carries the same reports; a failed --version leaves the version absent and runtime incomplete.
    a.new_session_epoch();assert_eq!(startup(&a)["model"],"gpt-x");
    let mut b=Agent::with_cwd("private".into(),Signals::new(),"/w".into());b.startup_failed(&["runtime"]);
    let failed=startup(&b);assert_eq!((&failed["runtime"],&failed["incomplete"]),(&json!({"name":"Pi"}),&json!(["runtime"])));
    // An unusable get_commands reply: the lists are absent and incomplete, not empty.
    event(&mut b,json!({"type":"response","id":"c","command":"get_commands","success":true,"data":{"unexpected":[]}}));
    let broken=startup(&b);assert!(broken.get("skills").is_none());
    assert!(broken["incomplete"].as_array().unwrap().iter().any(|f|f=="skills"));
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
#[test]
fn zz_tmp_r100_repro() {
    let mut a=Agent::with_cwd("x".into(),Signals::new(),"/home/u/proj".into());
    println!("after with_cwd: {}", String::from_utf8(a.journal.snapshot()).unwrap());
    event(&mut a,json!({"type":"response","id":"n1","command":"get_state","success":true,"data":{"model":{"id":"gpt-6-luna","name":"GPT-6 Luna"},"thinkingLevel":"low","isStreaming":false,"isCompacting":false,"steeringMode":"one-at-a-time","followUpMode":"one-at-a-time","sessionId":"01a2","messageCount":0,"pendingMessageCount":0}}));
    println!("after get_state: {}", String::from_utf8(a.journal.snapshot()).unwrap());
}
