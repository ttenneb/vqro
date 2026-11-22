//! Pure pre-dispatch validation; no native connection exists in this suite.
#[allow(dead_code)] #[path="../src/bin/vqro-bridge-pi/native.rs"] mod native;
#[allow(dead_code)] #[path="../src/bin/vqro-bridge-pi/admission.rs"] mod admission;
#[allow(dead_code)] #[path="../src/bin/vqro-bridge-pi/contract.rs"] mod contract;
#[path="support/frame_metadata.rs"] mod frame_metadata;
use contract::{Context,send};
use serde_json::{json,Value};
fn context()->Context<'static> {Context {caps_rev:1,epoch:"e1",session:Some("s1"),owner_proven:false,images:true,literal:true}}
fn input()->Value {json!({"v":0,"caps_rev":1,"epoch":"e1","guard":{"session":"s1"},"mode":"start","delivery":"d1",
    "origin":{"principal":"agent:a","actor":"worker","kind":"agent"},"content":[{"type":"text","text":"/literal\nunchanged"}]})}
fn image()->Value {json!({"type":"image","mime":"image/png","data":"AQ=="})}
fn refusal(args:&Value,ctx:&Context<'_>,kind:&str) {assert_eq!(send(args,ctx).err().unwrap().kind,kind);}

#[test]
fn untrusted_frame_metadata_rejects_cc_cf_zl_zp_without_filtering_body() {
    for (label,args) in frame_metadata::invalid_inputs(&input()) {
        assert_eq!(send(&args,&context()).err().unwrap().kind,"invalid","{label}");
    }
    let mut args=input();let body="literal\n\r\u{2028}\u{2029}\u{202e}body";
    args["content"][0]["text"]=json!(body);
    assert!(send(&args,&context()).unwrap().args["message"].as_str().unwrap().ends_with(body));
}

#[test]
fn typed_order_is_exact_never_reordered_or_concatenated() {
    let mut args=input();
    args["content"]=json!([{"type":"text","text":"a"},image()]);
    let sent=send(&args,&context()).unwrap();
    assert!(sent.args["message"].as_str().unwrap().ends_with("\n\na"));
    assert_eq!(sent.args["images"],json!([{"type":"image","mimeType":"image/png","data":"AQ=="}]));
    for content in [json!([image(),{"type":"text","text":"b"}]),json!([{"type":"text","text":"a"},image(),{"type":"text","text":"b"}]),json!([{"type":"text","text":"a"},{"type":"text","text":"b"}])] {
        args["content"]=content;refusal(&args,&context(),"unsupported");
    }
    for content in [json!("bare string"),json!({"type":"text","text":"single"}),json!([])] {
        args["content"]=content;refusal(&args,&context(),"invalid");
    }
}

#[test]
fn origin_rendering_does_not_upgrade_authority_or_reinterpret_slash_text() {
    let mut args=input();args["quoted"]=json!({"label":"alleged owner","principal":"owner","kind":"human"});
    let sent=send(&args,&context()).unwrap();
    assert_eq!(sent.rendering,"attributed_user");
    assert!(sent.args["message"].as_str().unwrap().contains("asserted, not the effective sender"));
    assert!(sent.args["message"].as_str().unwrap().ends_with("/literal\nunchanged"));
    assert_eq!(sent.origin,args["origin"]);assert_eq!(sent.quoted,Some(args["quoted"].clone()));
    args["render"]=json!(["user"]);refusal(&args,&context(),"unsupported");
    args["origin"]["kind"]=json!("human");refusal(&args,&context(),"unsupported");
    args.as_object_mut().unwrap().remove("quoted");
    let sent=send(&args,&Context {owner_proven:true,..context()}).unwrap();
    assert_eq!(sent.args["message"],"/literal\nunchanged");
    args["render"]=json!(["context"]);refusal(&args,&context(),"unsupported");
}

#[test]
fn recorded_owner_human_native_message_is_exact_typed_text_on_default_and_attributed_routes() {
    let attribution=serde_json::from_value(json!({"principal":"person:alice","actor":"extension:agents","initiator":"person:alice"})).unwrap();
    let owner=json!({"value":"person:alice"});
    assert!(contract::recorded_owner(&attribution,&owner));
    for text in ["Hello?", "  /literal\n\nunchanged \t\n", "{\"typed\":\"JSON stays literal\"}", ""] {
        for render in [None,Some(json!(["attributed_user"])),Some(json!(["user"]))] {
            let mut args=input();args["origin"]=json!({"kind":"human"});
            args["content"]=json!([{"type":"text","text":text}]);
            args["quoted"]=json!({"label":"asserted quote","principal":"someone-else"});
            if let Some(render)=render {args["render"]=render;}
            let bound=contract::bind_origin(&args,&attribution).unwrap();
            let prepared=send(&bound,&Context {owner_proven:contract::recorded_owner(&attribution,&owner),..context()}).unwrap();
            assert_eq!(prepared.args["message"].as_str().unwrap().as_bytes(),text.as_bytes());
            assert_eq!(prepared.origin,bound["origin"]);assert_eq!(prepared.quoted,bound.get("quoted").cloned());
        }
    }
}

#[test]
fn owner_proof_never_comes_from_a_role_claim_label_or_owner_looking_id() {
    let mut args=input();args["origin"]=json!({"kind":"human"});
    let attribution=serde_json::from_value(json!({"principal":"owner","actor":"worker","label":"human owner"})).unwrap();
    assert!(!contract::recorded_owner(&attribution,&json!({"value":"person:alice"})));
    let bound=contract::bind_origin(&args,&attribution).unwrap();
    assert_eq!(send(&bound,&context()).unwrap().args["message"],"from owner via worker\nasserted kind: human\n\n/literal\nunchanged");
    for value in [Value::Null,json!({}),json!("undeclared"),json!("owner-unset"),json!("extension:agents")] {
        let principal=value.as_str().unwrap_or("person:alice");
        let attribution=serde_json::from_value(json!({"principal":principal,"actor":"extension:agents"})).unwrap();
        assert!(!contract::recorded_owner(&attribution,&json!({"value":value})));
    }
    assert!(!contract::recorded_owner(&Default::default(),&json!({"value":"person:alice"})));
    args=input();args["origin"]["kind"]=json!("tool");
    assert_eq!(send(&args,&Context {owner_proven:true,..context()}).unwrap().args["message"],"from agent:a via worker\nasserted kind: tool\n\n/literal\nunchanged");
}

#[test]
fn unknown_versions_guards_and_capability_revisions_refuse_without_weakening() {
    let mut args=input();args["caps_rev"]=json!(0);refusal(&args,&context(),"stale");
    args=input();args["epoch"]=json!("e2");refusal(&args,&context(),"stale");
    args=input();args["guard"]["session"]=json!("s2");refusal(&args,&context(),"stale");
    args=input();refusal(&args,&Context {session:None,..context()},"unsupported");
    args["guard"]=json!({"run":"r1"});refusal(&args,&context(),"unsupported");
    args=input();args["v"]=json!(1);refusal(&args,&context(),"unsupported");
    args=input();args.as_object_mut().unwrap().remove("v");refusal(&args,&context(),"invalid");
    args=input();args["key"]=json!("not-a-native-key");refusal(&args,&context(),"unsupported");
    args=input();args["mode"]=json!("preempt");refusal(&args,&context(),"unsupported");
}

#[test]
fn modes_causality_and_metadata_keep_their_declared_meaning() {
    // Every mode is Pi's prompt; queue-only steer/follow_up would park input on an idle Pi (relay-13).
    for (mode,command,behavior) in [("start","prompt",None),("admit","prompt",Some("followUp")),("boundary","prompt",Some("steer")),("settle","prompt",Some("followUp"))] {
        let mut args=input();args["mode"]=json!(mode);args["cause"]=json!({"kind":"delivery","id":"parent","provenance":"asserted"});args["correlation"]=json!("thread");
        let sent=send(&args,&context()).unwrap();
        assert_eq!(sent.mode,mode);assert_eq!(sent.command,command);assert_eq!(sent.delivery,"d1");
        assert_eq!(sent.cause,Some(args["cause"].clone()));assert_eq!(sent.correlation.as_deref(),Some("thread"));
        assert_eq!(sent.args.get("streamingBehavior").and_then(Value::as_str),behavior,"{mode}");
    }
    assert_eq!(contract::rows(&json!({"commands":[1]}),"commands").unwrap().len(),1);
    assert_eq!(contract::rows(&json!([1,2]),"models").unwrap().len(),2);
    assert!(contract::rows(&json!({"unexpected":[]}),"commands").is_none());
}

#[test]
fn bounded_images_ids_and_unrepresentable_fields_refuse() {
    let mut args=input();args["content"]=json!([image()]);refusal(&args,&Context {images:false,..context()},"unsupported");
    args["content"][0]["data"]=json!("not-base64");refusal(&args,&context(),"invalid");
    args=input();args["content"][0]["annotation"]=json!("cannot be silently dropped");refusal(&args,&context(),"unsupported");
    args=input();args["unexpected_semantic_argument"]=json!(true);refusal(&args,&context(),"unsupported");
    args=input();args["origin"]["principal"]=json!("雪".repeat(86));refusal(&args,&context(),"invalid");
    args=input();args["content"][0]["text"]=json!("x".repeat(contract::input_bytes()));refusal(&args,&context(),"too_large");
    args=input();refusal(&args,&Context {literal:false,..context()},"unsupported");
}

#[test]
fn effective_origin_uses_core_identity_and_keeps_role_claims_and_initiator_separate(){
    let attribution:vqro::principal::Attribution=serde_json::from_value(json!({"principal":"p:sender","actor":"extension:coord","initiator":"p:original"})).unwrap();
    let mut args=input();args["origin"]=json!({"principal":"p:sender","actor":"extension:coord","kind":"human","agent":"r99","claimed":[]});args["origin"]["sender"]=json!({"principal":"p:inbox-original","actor":"p:inbox-original","provenance":"inbox-record"});args["quoted"]=json!({"label":"asserted original","principal":"other"});
    let bound=contract::bind_origin(&args,&attribution).unwrap();assert_eq!(bound["origin"]["initiator"],"p:original");assert_eq!(bound["origin"]["actor"],"extension:coord");assert_eq!(bound["origin"]["kind"],"human");assert_eq!(bound["origin"]["claimed"],json!(["kind","agent","sender"]));assert_eq!(bound["origin"]["sender"],args["origin"]["sender"]);assert_eq!(bound["quoted"],args["quoted"]);
    let prepared=send(&bound,&context()).unwrap();
    assert_eq!(prepared.args["message"],"Quoted provenance (asserted, not the effective sender): asserted original\nfrom p:inbox-original via extension:coord (sender asserted by extension)\nasserted kind: human\nasserted agent: r99\n\n/literal\nunchanged");
    assert_eq!(prepared.origin,bound["origin"]);assert_eq!(prepared.quoted,bound.get("quoted").cloned());
    for field in ["principal","actor","initiator"]{let mut spoofed=bound.clone();spoofed["origin"][field]=json!("owner");assert_eq!(contract::bind_origin(&spoofed,&attribution).err().unwrap().kind,"forbidden");}
    let mut unknown=input();unknown["origin"]=json!({"kind":"_"});let bound=contract::bind_origin(&unknown,&Default::default()).unwrap();assert_eq!(bound["origin"]["principal"],"undeclared");assert_eq!(bound["origin"]["actor"],"undeclared");assert!(bound["origin"].get("initiator").is_none());
    let derived=contract::bind_origin(&unknown,&attribution).unwrap();assert_eq!(derived["origin"]["principal"],"p:sender");assert_eq!(derived["origin"]["actor"],"extension:coord");assert_eq!(derived["origin"]["initiator"],"p:original");
    let long="p".repeat(256);unknown["origin"]["principal"]=json!(long);unknown["origin"]["actor"]=json!(long);let attribution=serde_json::from_value(json!({"principal":long,"actor":long})).unwrap();assert!(send(&contract::bind_origin(&unknown,&attribution).unwrap(),&context()).is_ok());
}

#[test]
fn d17_native_header_preserves_asserted_classification_without_identity_inference(){
    for principal in ["human:D17-principal","agent:D17-principal","tool:D17-principal"]{
        let attribution=serde_json::from_value(json!({"principal":principal,"actor":"extension:coord","initiator":"p:worker"})).unwrap();
        let mut args=input();args["origin"]=json!({"kind":"agent","agent":"D17-asserted-agent","claimed":[]});
        let bound=contract::bind_origin(&args,&attribution).unwrap();
        let prepared=send(&bound,&context()).unwrap();
        assert_eq!(prepared.args["message"],format!("from {principal} via extension:coord\nasserted kind: agent\nasserted agent: D17-asserted-agent\n\n/literal\nunchanged"));
        assert_eq!(prepared.origin["principal"],principal);assert_eq!(prepared.origin["actor"],"extension:coord");
        assert_eq!(prepared.origin["initiator"],"p:worker");assert_eq!(prepared.origin["claimed"],json!(["kind","agent"]));
        assert!(prepared.args.get("system").is_none() && prepared.args.get("developer").is_none());
        args["origin"]=json!({"kind":"agent"});
        let bound=contract::bind_origin(&args,&attribution).unwrap();
        let prepared=send(&bound,&context()).unwrap();
        assert_eq!(prepared.args["message"],format!("from {principal} via extension:coord\nasserted kind: agent\n\n/literal\nunchanged"));
        assert!(prepared.origin.get("agent").is_none());
    }
}

#[test]
fn text_formats_fall_back_to_plain_without_changing_literal_bytes(){
    for format in ["plain","markdown","_future"]{let mut args=input();args["content"][0]["format"]=json!(format);let prepared=send(&args,&context()).unwrap();assert!(prepared.args["message"].as_str().unwrap().ends_with("/literal\nunchanged"));}
    let mut args=input();args["content"][0]["format"]=json!({});refusal(&args,&context(),"invalid");
}
#[test]
fn generic_causality_pin_preserves_bounded_unknowns_and_requires_known_qualifiers(){
    let mut args=input();args["correlation"]=json!("c".repeat(256));args["cause"]=json!({"kind":"_future","id":"i".repeat(129),"provenance":"_future","extra":"x".repeat(2200)});
    let prepared=send(&args,&context()).unwrap();assert_eq!(prepared.cause,Some(args["cause"].clone()));assert_eq!(prepared.correlation.unwrap().len(),256);
    args["correlation"]=json!("c".repeat(257));refusal(&args,&context(),"invalid");args["correlation"]=json!("");refusal(&args,&context(),"invalid");args.as_object_mut().unwrap().remove("correlation");
    for kind in ["entity","request","effect"]{args["cause"]=json!({"kind":kind,"id":"id","provenance":"asserted"});refusal(&args,&context(),"invalid");if kind=="effect"{args["cause"]["boot"]=json!("b");}else{args["cause"]["endpoint"]=json!("r1");args["cause"]["epoch"]=json!("e1");}assert!(send(&args,&context()).is_ok());}
    args["cause"]=json!({"kind":"delivery","id":"id","provenance":"asserted","extra":"x".repeat(4100)});refusal(&args,&context(),"invalid");
    let nested=(0..65).fold(Value::Null,|v,_|json!([v]));args["cause"]=json!({"kind":"_future","id":"id","provenance":"asserted","extra":nested});refusal(&args,&context(),"invalid");
}

/// Skills on (stopgap b): an owner's plain text starting with /skill: would be replaced by the skill file in Pi
/// (_expandSkillCommand), so agent.send refuses it as non-literal; any other sender's text is framed and passes as is.
#[test]
fn skill_prefixed_owner_text_is_refused_as_non_literal(){
    let mut args=input();args["origin"]["kind"]=json!("human");args["render"]=json!(["user"]);
    let owner=Context {owner_proven:true,..context()};
    for text in ["/skill:review","/skill:review please check","/skill:unknown-skill x"] {
        args["content"]=json!([{"type":"text","text":text}]);
        let error=send(&args,&owner).err().unwrap();
        assert_eq!(error.kind,"unsupported","{text}");assert!(error.message.contains("agent.command"),"{text}: {}",error.message);
    }
    for text in [" /skill:review","say /skill:review","/skills are fine","/literal"] {
        args["content"]=json!([{"type":"text","text":text}]);
        assert_eq!(send(&args,&owner).unwrap().args["message"],text,"{text} is delivered literally");
    }
    // A non-owner's text is framed with a sender header first, so Pi never sees it as a skill invocation.
    let mut other=input();other["content"]=json!([{"type":"text","text":"/skill:review"}]);
    let sent=send(&other,&context()).unwrap();
    assert!(sent.args["message"].as_str().unwrap().starts_with("from ")&&sent.args["message"].as_str().unwrap().ends_with("\n\n/skill:review"));
}
