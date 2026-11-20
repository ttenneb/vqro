//! Pure pre-dispatch validation; no native connection exists in this suite.
#[allow(dead_code)] #[path="../src/bin/vqro-bridge-pi/native.rs"] mod native;
#[allow(dead_code)] #[path="../src/bin/vqro-bridge-pi/admission.rs"] mod admission;
#[allow(dead_code)] #[path="../src/bin/vqro-bridge-pi/contract.rs"] mod contract;
use contract::{Context,send};
use serde_json::{json,Value};
fn context()->Context<'static> {Context {caps_rev:1,epoch:"e1",session:Some("s1"),owner_proven:false,images:true,literal:true}}
fn input()->Value {json!({"v":0,"caps_rev":1,"epoch":"e1","guard":{"session":"s1"},"mode":"start","delivery":"d1",
    "origin":{"principal":"agent:a","actor":"worker","kind":"agent"},"content":[{"type":"text","text":"/literal\nunchanged"}]})}
fn image()->Value {json!({"type":"image","mime":"image/png","data":"AQ=="})}
fn refusal(args:&Value,ctx:&Context<'_>,kind:&str) {assert_eq!(send(args,ctx).err().unwrap().kind,kind);}

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
    for (mode,command) in [("start","prompt"),("admit","prompt"),("boundary","steer"),("settle","follow_up")] {
        let mut args=input();args["mode"]=json!(mode);args["cause"]=json!({"kind":"delivery","id":"parent","provenance":"asserted"});args["correlation"]=json!("thread");
        let sent=send(&args,&context()).unwrap();
        assert_eq!(sent.mode,mode);assert_eq!(sent.command,command);assert_eq!(sent.delivery,"d1");
        assert_eq!(sent.cause,Some(args["cause"].clone()));assert_eq!(sent.correlation.as_deref(),Some("thread"));
        assert_eq!(sent.args.get("streamingBehavior").is_some(),mode=="admit");
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
    args=input();args["origin"]["principal"]=json!("雪".repeat(43));refusal(&args,&context(),"invalid");
    args=input();args["content"][0]["text"]=json!("x".repeat(contract::input_bytes()));refusal(&args,&context(),"too_large");
    args=input();refusal(&args,&Context {literal:false,..context()},"unsupported");
}
