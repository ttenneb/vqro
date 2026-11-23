#![cfg(unix)] // Unix-socket/signal fixtures; on Windows the lib's unit tests run (docs/windows.md).
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

/// Dependencies are reported from static manifests and runtime status; enable/disable replies say what is left
/// unmet or broken, and nothing is enabled or disabled as a side effect (docs/research/declarations-and-ui-clients.md).
#[tokio::test]
async fn dependencies_are_reported_never_acted_on(){
 let root=tempfile::Builder::new().prefix("pmd").tempdir_in("/tmp").unwrap();let socket=root.path().join("p");let dir=root.path().join("ext");std::fs::create_dir_all(dir.join("manifests")).unwrap();
 std::fs::write(dir.join("manifests/tabs.json"),r#"{"name":"tabs","provides":["command/tabs.show"]}"#).unwrap();
 std::fs::write(dir.join("manifests/agents.json"),r#"{"name":"agents","needs":[{"cap":"command/tabs.show","suggest":"tabs"},"command/vqro.revert"],"works_with":[{"cap":"data/themes","suggest":"themes"}],"requires":{"client_features":["items"]}}"#).unwrap();
 let core=server::start(socket.clone(),root.path().join("st"),false,Backend::Extensions,Some(dir.clone())).unwrap();
 let(p,_,_)=client::connect(&socket,"manifest-test").await.unwrap();
 p.call("workspace.apply",json!({"ops":[{"op":"set","key":"extension/tabs","value":{"argv":["/never-run"],"enabled":false}},{"op":"set","key":"extension/agents","value":{"argv":["/never-run"],"enabled":false}}]})).await.unwrap();
 let facts=p.call("extension.list",Value::Null).await.unwrap()["extensions"].clone();
 let deps=&facts["agents"]["dependencies"];assert_eq!(deps["status"],"missing");
 assert_eq!(deps["needs"][0]["status"],"off");assert_eq!(deps["needs"][0]["providers"][0]["name"],"tabs");assert_eq!(deps["needs"][1]["status"],"satisfied");
 assert_eq!(deps["needs"][2]["cap"],"ui/vqro.items");assert_eq!(deps["works_with"][0]["status"],"missing");
 assert_eq!(facts["tabs"]["dependencies"]["used_by"][0]["name"],"agents");
 // Turning agents on reports its unmet need and does not turn tabs on.
 let reply=p.call("command.invoke",json!({"name":"vqro.extension.enable","args":{"name":"agents"}})).await.unwrap();
 assert_eq!(reply["needs_unmet"][0]["cap"],"command/tabs.show");
 assert_eq!(p.call("extension.list",Value::Null).await.unwrap()["extensions"]["tabs"]["enabled"],false);
 // Turning tabs off while an enabled agents needs it reports the break and leaves agents enabled.
 p.call("command.invoke",json!({"name":"vqro.extension.enable","args":{"name":"tabs"}})).await.unwrap();
 let reply=p.call("command.invoke",json!({"name":"vqro.extension.disable","args":{"name":"tabs"}})).await.unwrap();
 assert_eq!(reply["breaks"],json!([{"name":"agents","cap":"command/tabs.show"}]));
 assert_eq!(p.call("extension.list",Value::Null).await.unwrap()["extensions"]["agents"]["enabled"],true);
 // Clients declare features and profiles in hello; UI needs resolve against rendering clients.
 let raw=tokio::net::UnixStream::connect(&socket).await.unwrap();let(r,mut w)=raw.into_split();
 use tokio::io::{AsyncBufReadExt,AsyncWriteExt};
 w.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"hello\",\"params\":{\"client\":\"text\",\"profiles\":[\"base\"],\"features\":[\"items\"]}}\n").await.unwrap();
 let mut line=String::new();tokio::io::BufReader::new(r).read_line(&mut line).await.unwrap();assert!(line.contains("\"result\""),"{line}");
 let clients=p.call("client.list",Value::Null).await.unwrap()["clients"].clone();
 assert!(clients.as_array().unwrap().iter().any(|c|c["client"]=="text" && c["features"]==json!(["items"]) && c["profiles"]==json!(["base"])));
 let need=p.call("extension.list",Value::Null).await.unwrap()["extensions"]["agents"]["dependencies"]["needs"][2].clone();
 assert_eq!(need["unknown_in"],json!(["manifest-test"]),"legacy clients without features are unknown, not lacking: {need}");
 assert_eq!(need["lacking_in"],json!([]));
 server::shutdown(&core).await;
}
