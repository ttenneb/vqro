use vqro::{client,server::{self,Backend}};
use serde_json::{json,Value};
#[tokio::test]
async fn metadata_is_static_pid_free_and_readable_when_off_failed_or_safe(){
 let root=tempfile::Builder::new().prefix("pmf").tempdir_in("/tmp").unwrap();let socket=root.path().join("p");let dir=root.path().join("ext");std::fs::create_dir_all(dir.join("manifests")).unwrap();
 std::fs::write(dir.join("manifests/off.json"),r#"{"name":"off","title":"Readable while off","future_field":42}"#).unwrap();
 std::fs::write(dir.join("failed.json"),r#"{"name":"failed","description":"Readable after failure"}"#).unwrap();
 let core=server::start(socket.clone(),root.path().join("st"),false,Backend::Extensions,Some(dir.clone())).unwrap();let(p,_,_)=client::connect(&socket,"manifest-test").await.unwrap();
 p.call("workspace.apply",json!({"ops":[{"op":"set","key":"extension/off","value":{"argv":["/never-run"],"enabled":false}},{"op":"set","key":"extension/failed","value":{"argv":["vqro-manifest-test-nonexistent"],"manifest":"failed.json"}}]})).await.unwrap();
 let facts=p.call("extension.list",Value::Null).await.unwrap()["extensions"].clone();assert_eq!(facts["off"]["manifest"]["title"],"Readable while off");assert_eq!(facts["off"]["display_status"],"off");assert_eq!(facts["failed"]["display_status"],"failed");
 assert!(facts["failed"].get("pid").is_none());assert!(facts["failed"]["generation"].is_u64());assert!(!core.lock().unwrap().exts["off"].running);
 std::fs::write(dir.join("manifests/off.json"),r#"{"name":"off","title":"Reloaded without executing"}"#).unwrap();
 assert_eq!(p.call("extension.list",Value::Null).await.unwrap()["extensions"]["off"]["manifest"]["title"],"Reloaded without executing");
 p.call("command.invoke",json!({"name":"vqro.safe_mode","args":{"on":true}})).await.unwrap();
 let snapshot=p.call("workspace.snapshot",Value::Null).await.unwrap();assert_eq!(snapshot["extensions"]["failed"]["manifest"]["description"],"Readable after failure");assert_eq!(snapshot["extensions"]["failed"]["display_status"],"stopped-by-safe-mode");assert!(snapshot["extensions"]["failed"]["last_exit"].is_string());
 server::shutdown(&core).await;
}
