#[allow(dead_code)] #[path="../src/bin/vqro-bridge-pi/native.rs"] mod native;
#[allow(dead_code)] #[path="../src/bin/vqro-bridge-pi/admission.rs"] mod admission;
#[allow(dead_code)] #[path="../src/bin/vqro-bridge-pi/contract.rs"] mod contract;
#[path="../src/bin/vqro-bridge-pi/configuration.rs"] mod configuration;
use configuration::Configuration;
use serde_json::{json,Value};
fn state()->Value{json!({"model":{"provider":"custom/provider","id":"model/one","name":"Native name"},"thinkingLevel":"low","steeringMode":"one-at-a-time","followUpMode":"all","autoCompactionEnabled":true})}
fn make()->Configuration{let mut c=Configuration::default();c.observe_models(&json!({"models":[state()["model"].clone(),{"provider":"p","id":"second","name":"Second"}]}));c.observe_levels(&json!({"levels":["off","low","high"]}));c.rebuild(&state());c}
fn option<'a>(c:&'a Configuration,id:&str)->&'a Value{c.options().iter().find(|row|row["id"]==id).unwrap()}
#[test]
fn real_array_and_container_metadata_keep_native_ids_unambiguous(){
    let c=make();let o=option(&c,"model");let change=c.prepare("model",&o["value"],Some(&o["rev"]),&state()).unwrap();
    assert_eq!(change.command,"set_model");assert_eq!(change.args,json!({"provider":"custom/provider","modelId":"model/one"}));assert_eq!(change.id,"model");assert!(c.matches(&change));
    let mut array=Configuration::default();array.observe_models(&json!([state()["model"].clone()]));array.observe_levels(&json!(["low"]));array.rebuild(&state());assert_eq!(option(&array,"thinking_level")["choices"],json!([{"value":"low","label":"low"}]));
}
#[test]
fn stale_revision_and_unoffered_or_wrong_type_values_reject_before_transport(){
    let c=make();
    assert_eq!(c.prepare("model",&json!("made-up"),None,&state()).err().unwrap().kind,"invalid");
    assert_eq!(c.prepare("steering_mode",&json!("all"),Some(&json!(99)),&state()).err().unwrap().kind,"stale");
    assert_eq!(c.prepare("auto_compaction",&json!(0),None,&state()).err().unwrap().kind,"invalid");
    assert_eq!(c.prepare("unknown",&Value::Null,None,&state()).err().unwrap().kind,"unsupported");
    assert_eq!(c.prepare("thinking_level",&json!("xhigh"),None,&state()).err().unwrap().kind,"invalid");
}
#[test]
fn option_revisions_change_only_with_reported_value_or_descriptor(){
    let mut c=make();let before=option(&c,"steering_mode")["rev"].clone();c.rebuild(&state());assert_eq!(option(&c,"steering_mode")["rev"],before);
    let change=c.prepare("steering_mode",&json!("all"),None,&state()).unwrap();assert!(!c.matches(&change));
    let mut next=state();next["steeringMode"]=json!("all");c.rebuild(&next);assert!(c.matches(&change));assert!(option(&c,"steering_mode")["rev"].as_u64()>before.as_u64());
}
#[test]
fn unresolved_setter_masks_old_values_and_prevents_another_claim(){
    let mut c=make();c.pending=true;c.rebuild(&state());assert!(option(&c,"model")["value"].is_null());assert!(option(&c,"model")["incomplete"].is_array());
    assert_eq!(c.prepare("steering_mode",&json!("all"),None,&state()).err().unwrap().kind,"busy");
    c.pending=false;c.rebuild(&state());assert!(!option(&c,"model")["value"].is_null());
}
#[test]
fn catalog_cut_is_explicit_and_current_model_remains_declared(){
    let mut c=Configuration::default();let rows:Vec<_>=(0..100).map(|n|json!({"provider":"p","id":format!("model-{n}"),"name":"bounded"})).collect();c.observe_models(&json!({"models":rows}));c.rebuild(&state());
    assert!(c.incomplete);assert_eq!(option(&c,"model")["choices"].as_array().unwrap().len(),configuration::MAX_CHOICES+1);
    assert!(c.prepare("model",&option(&c,"model")["value"],None,&state()).is_ok());
    let mut unknown=Configuration::default();unknown.observe_models(&json!({"wrong":[]}));unknown.observe_levels(&json!({"wrong":[]}));unknown.rebuild(&json!({}));assert!(unknown.options().is_empty());assert!(unknown.incomplete);
}
