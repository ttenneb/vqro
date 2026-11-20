#[allow(dead_code)] #[path="../src/bin/vqro-bridge-pi/native.rs"] mod native;
#[allow(dead_code)] #[path="../src/bin/vqro-bridge-pi/admission.rs"] mod admission;
#[path="../src/bin/vqro-bridge-pi/requests.rs"] mod requests;
use requests::{Incoming,Requests};
use serde_json::{json,Value};
fn row(method:&str)->Value {match method {
    "select"=>json!({"type":"extension_ui_request","id":"native","method":"select","title":"Choose","options":["one","two"]}),
    "confirm"=>json!({"type":"extension_ui_request","id":"native","method":"confirm","title":"A question, not a permission","message":"Continue?"}),
    _=>json!({"type":"extension_ui_request","id":"native","method":"input","title":"Text"}),
}}
fn dialog(registry:&mut Requests,row:&Value)->(String,Value){let Incoming::Dialog{id,value,..}=registry.observe(row)else{panic!("dialog");};(id,value)}
#[test]
fn scope_is_a_native_dialog_operation_not_inferred_session_or_run(){
    let mut registry=Requests::default();let (id,entity)=dialog(&mut registry,&row("select"));
    assert_eq!(entity["scope"],json!({"operation":format!("ui-{id}")}));assert_eq!(entity["method"],"choice");
    assert_eq!(entity["choices"][1],json!({"id":"c1","label":"two"}));
    assert_eq!(entity["revision"],1);assert_eq!(entity["incomplete"],false);
}
#[test]
fn first_valid_answer_wins_not_the_first_invalid_attempt(){
    let mut registry=Requests::default();let (id,entity)=dialog(&mut registry,&row("select"));
    assert_eq!(registry.claim(&id,1,&json!({"choice":"missing"}),&entity).err().unwrap().kind,"invalid");
    let answer=registry.claim(&id,1,&json!({"choice":"c1"}),&entity).unwrap();assert_eq!(answer.wire,json!({"id":"native","value":"two"}));assert_eq!(answer.request,id);assert!(!answer.cancelled);
    assert_eq!(registry.claim(&id,1,&json!({"choice":"c0"}),&entity).err().unwrap().kind,"stale");
    assert!(registry.has_unconfirmed());registry.finish(&id);assert!(!registry.has_unconfirmed());assert!(registry.ids().is_empty());
}
#[test]
fn cancel_false_and_empty_text_are_distinct_native_answers(){
    for (method,response,expected,cancelled) in [("confirm",json!({"value":false}),json!({"id":"native","confirmed":false}),false),("input",json!({"value":""}),json!({"id":"native","value":""}),false),("confirm",json!({"action":"cancel"}),json!({"id":"native","cancelled":true}),true)]{
        let mut registry=Requests::default();let (id,entity)=dialog(&mut registry,&row(method));let answer=registry.claim(&id,1,&response,&entity).unwrap();assert_eq!(answer.wire,expected);assert_eq!(answer.cancelled,cancelled);
        registry.rollback(&id);assert!(registry.claim(&id,1,&response,&entity).is_ok(),"only a known no-write failure permits rollback");
    }
}
#[test]
fn reused_native_id_never_reuses_standard_identity_or_accepts_old_revision(){
    let mut registry=Requests::default();let (old,old_entity)=dialog(&mut registry,&row("confirm"));
    let Incoming::Dialog{id:new,value,replaced}=registry.observe(&row("confirm"))else{panic!("replacement");};
    assert_ne!(old,new);assert_eq!(replaced,Some(old.clone()));
    assert_eq!(registry.claim(&old,1,&json!({"value":true}),&old_entity).err().unwrap().kind,"stale");
    assert_eq!(registry.claim(&new,2,&json!({"value":true}),&value).err().unwrap().kind,"stale");
    assert!(registry.would_cancel_another(&old,&json!("native")));
    let mut timed=row("confirm");timed["timeout"]=json!(5);
    let Incoming::Cancel{replaced,..}=registry.observe(&timed)else{panic!("timed replacement");};assert_eq!(replaced,Some(new));
}
#[test]
fn timed_unknown_oversized_and_excess_dialogs_cancel_without_approval(){
    let mut registry=Requests::default();
    for bad in [json!({"type":"extension_ui_request","id":"timed","method":"confirm","title":"T","message":"?","timeout":10}),json!({"type":"extension_ui_request","id":"u","method":"permission","title":"No inference"}),json!({"type":"extension_ui_request","id":"big","method":"editor","title":"T","prefill":"x".repeat(20000)})]{
        let Incoming::Cancel{id,native,reason,..}=registry.observe(&bad)else{panic!("unrepresentable");};assert!(!id.is_empty());assert!(!native.is_null());assert!(!reason.is_empty());
    }
    for i in 0..requests::MAX_PENDING{let mut request=row("confirm");request["id"]=json!(format!("n{i}"));dialog(&mut registry,&request);}
    let mut extra=row("confirm");extra["id"]=json!("extra");assert!(matches!(registry.observe(&extra),Incoming::Cancel{..}));
    assert_eq!(registry.ids().len(),requests::MAX_PENDING);
    let cancelled=registry.cancel_all();assert_eq!(cancelled.len(),requests::MAX_PENDING);assert!(registry.ids().is_empty());
}
#[test]
fn native_fire_and_forget_ui_is_not_a_pending_question(){
    let mut registry=Requests::default();for method in ["notify","setStatus","setWidget","setTitle","set_editor_text"]{
        let Incoming::Notice{id,value}=registry.observe(&json!({"type":"extension_ui_request","id":"info","method":method}))else{panic!("notice");};assert!(!id.is_empty());assert_eq!(value["method"],method);
    }assert!(registry.ids().is_empty());
}
