//! Safe admission and raw legacy recovery; no live backend or user state.
use prim::{store::{Store,Op},limits,core::Core};
use serde_json::{json,Value};
fn nested(depth:usize)->Value {(0..depth).fold(Value::Null,|v,_|Value::Array(vec![v]))}
fn set(key:&str,value:Value)->Op {Op::Set {key:key.into(),value,expect:None}}
fn raw(depth:usize)->String {format!("{}0{}","[".repeat(depth),"]".repeat(depth))}
fn backups(root:&std::path::Path)->Vec<std::path::PathBuf> {std::fs::read_dir(root).unwrap().filter_map(|e|{let p=e.unwrap().path();p.file_name().unwrap().to_string_lossy().starts_with("workspace.quarantine-").then_some(p)}).collect()}
#[test]
fn native_fact_fields_have_the_same_opaque_value_depth_bound() {
 let base=json!({"backend_ref":"fixture","kind":"terminal","state":"live","data":{}});
 for field in ["status","exit","data"] {
  let mut value=base.clone();value[field]=if field=="data" {json!({"nested":nested(64)})}else{nested(65)};
  let facts:prim::adapter::ResourceFacts=serde_json::from_value(value).unwrap();assert!(!facts.valid(),"{field} escaped native depth admission");
 }
 let mut value=base;value["status"]=nested(64);let facts:prim::adapter::ResourceFacts=serde_json::from_value(value).unwrap();assert!(facts.valid());
}
#[test]
fn depth_boundary_survives_history_revert_and_restart() {
 let root=tempfile::tempdir().unwrap();let path=root.path().join("workspace.json");let mut s=Store::open(path.clone()).unwrap();
 s.apply("user",None,vec![set("doc/depth",nested(limits::JSON_DEPTH))]).unwrap();
 let changed=s.apply("user",Some("literal brackets [[[{".into()),vec![set("doc/depth",json!(1))]).unwrap();
 drop(s);let mut s=Store::open(path.clone()).unwrap();s.revert("user",changed.id).unwrap();
 assert_eq!(s.get("doc/depth").unwrap().value,nested(limits::JSON_DEPTH));
 let disk=std::fs::read(&path).unwrap();
 let error=s.apply("user",None,vec![set("doc/bad",nested(limits::JSON_DEPTH+1))]).unwrap_err();
 assert_eq!(error.kind(),"too_large");assert_eq!(error.data.unwrap()["unit"],"container_depth");
 assert_eq!(std::fs::read(&path).unwrap(),disk);assert!(s.get("doc/bad").is_none());
 drop(s);Store::open(path).unwrap();
}
#[test]
fn legacy_arbitrary_depth_is_quarantined_without_building_a_recursive_tree() {
 let root=tempfile::tempdir().unwrap();let path=root.path().join("workspace.json");
 let original=format!(r#"{{"rev":7,"next_txn":9,"entries":{{"doc/deep":{{"version":7,"value":{}}},"doc/healthy":{{"version":4,"value":{{"kept":true}}}}}},"history":[]}}"#,raw(10000));
 std::fs::write(&path,&original).unwrap();let mut s=Store::open(path.clone()).unwrap();
 assert!(s.get("doc/deep").is_none());assert_eq!(s.get("doc/healthy").unwrap().value,json!({"kept":true}));
 assert_eq!(std::fs::read_to_string(&path).unwrap(),original,"load never rewrites raw source");assert!(backups(root.path()).is_empty());
 assert_eq!(s.apply("user",None,vec![set("doc/new",json!(2))]).unwrap().id,9);
 let backup=backups(root.path());assert_eq!(backup.len(),1);assert_eq!(std::fs::read_to_string(&backup[0]).unwrap(),original);
 #[cfg(unix)] {use std::os::unix::fs::PermissionsExt;assert_eq!(std::fs::metadata(&backup[0]).unwrap().permissions().mode()&0o777,0o600);}
 s.apply("user",None,vec![set("doc/new",json!(3))]).unwrap();assert_eq!(backups(root.path()).len(),1);
 drop(s);let s=Store::open(path).unwrap();assert_eq!(s.get("doc/healthy").unwrap().value,json!({"kept":true}));
 let c=Core::new(s,None,"unused".into(),false).unwrap();assert!(c.snapshot()["recovery_notice"].as_str().unwrap().contains("Raw source backup"));
}
#[test]
fn legacy_unsafe_preimage_is_an_explicit_undo_boundary_with_raw_backup() {
 let root=tempfile::tempdir().unwrap();let path=root.path().join("workspace.json");
 let original=format!(r#"{{"rev":7,"next_txn":4,"entries":{{"doc/a":{{"version":7,"value":1}}}},"history":[{{"id":3,"author":"ext:fixture","on_behalf_of":"user","at_ms":1,"changes":[{{"key":"doc/a","before":{{"version":6,"value":{}}},"after_version":7}}]}}]}}"#,raw(123));
 std::fs::write(&path,&original).unwrap();let mut s=Store::open(path.clone()).unwrap();
 assert!(!s.history(1)[0].undoable);assert_eq!(s.history(1)[0].on_behalf_of.as_deref(),Some("user"));
 assert_eq!(s.revert("user",3).unwrap_err().kind(),"unavailable");assert_eq!(s.get("doc/a").unwrap().value,1);
 s.apply("user",None,vec![set("doc/b",json!(2))]).unwrap();assert_eq!(std::fs::read_to_string(&backups(root.path())[0]).unwrap(),original);
 drop(s);let s=Store::open(path).unwrap();assert!(!s.history(2)[1].undoable);assert_eq!(s.get("doc/a").unwrap().value,1);
}
#[test]
fn quarantined_declarations_preserve_healthy_layout_references() {
 let root=tempfile::tempdir().unwrap();let path=root.path().join("workspace.json");
 let original=format!(r#"{{"rev":7,"next_txn":4,"entries":{{"view/bad":{{"version":4,"value":{}}},"layout/healthy":{{"version":5,"value":{{"view":"bad"}}}},"layout/bad":{{"version":6,"value":{}}},"workspace/active":{{"version":7,"value":{{"layout":"bad"}}}},"doc/healthy":{{"version":3,"value":"kept"}}}},"history":[]}}"#,raw(123),raw(123));
 std::fs::write(&path,&original).unwrap();let mut s=Store::open(path).unwrap();
 assert_eq!(s.get("layout/healthy").unwrap().value,json!({"view":"bad"}));assert_eq!(s.get("doc/healthy").unwrap().value,"kept");
 assert_eq!(s.get("view/bad").unwrap().value["kind"],"panel");
 s.apply("user",None,vec![set("doc/another",json!(true))]).unwrap();
 assert_eq!(std::fs::read_to_string(&backups(root.path())[0]).unwrap(),original);
}
#[tokio::test]
async fn rpc_rejects_deep_entries_live_content_and_provider_metadata_without_poisoning_snapshot() {
 let root=tempfile::Builder::new().prefix("pdepth").tempdir_in("/tmp").unwrap();let socket=root.path().join("p");
 let core=prim::server::start(socket.clone(),root.path().join("st"),false,prim::server::Backend::Fake,None).unwrap();
 let(peer,_incoming,_)=prim::client::connect(&socket,"depth-test").await.unwrap();
 let error=peer.call("workspace.apply",json!({"ops":[{"op":"set","key":"doc/deep","value":nested(65)}]})).await.unwrap_err();assert_eq!(error.kind(),"too_large");
 let error=peer.call("view.publish",json!({"view":"deep","content":{"nested":nested(64)}})).await.unwrap_err();assert_eq!(error.kind(),"too_large");
 assert!(!core.lock().unwrap().live.contains_key("deep"));
 let(provider,_requests,_)=prim::client::connect(&socket,"depth-provider").await.unwrap();
 let mut config=json!({"name":"depth-provider","role":"backend","instance":"fixture","software":{"name":"fixture","version":"1"},"provider_version":"0","capabilities":{}});
 config["capabilities"]=json!({"op":{"deep":nested(64)}});
 assert_eq!(provider.call("provider.register",config.clone()).await.unwrap_err().kind(),"too_large");assert!(!core.lock().unwrap().providers.contains_key("depth-provider"));
 config["capabilities"]=json!({});let generation=provider.call("provider.register",config).await.unwrap()["generation"].clone();
 assert_eq!(provider.call("provider.sync",json!({"generation":generation,"final":true,"resources":[{"ref":"deep","kind":"terminal","state":"live","data":{"nested":nested(64)}}]})).await.unwrap_err().kind(),"too_large");
 assert!(!core.lock().unwrap().resources.values().any(|r|r.adapter=="depth-provider"));
 let snapshot=peer.call("workspace.snapshot",Value::Null).await.unwrap();assert!(snapshot["entries"].get("doc/deep").is_none());
 provider.close();peer.close();prim::server::shutdown(&core).await;
}
#[test]
fn raw_depth_scanner_does_not_quarantine_brackets_or_escaped_quotes_in_strings() {
 let root=tempfile::tempdir().unwrap();let path=root.path().join("workspace.json");
 let mut s=Store::open(path.clone()).unwrap();let text="[[[{{{\\\"".repeat(1000);
 s.apply("user",None,vec![set("doc/text",json!(text))]).unwrap();drop(s);
 let s=Store::open(path).unwrap();assert_eq!(s.get("doc/text").unwrap().value,text);assert!(backups(root.path()).is_empty());
}
