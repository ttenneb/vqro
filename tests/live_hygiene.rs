//! Collision hygiene is cooperative, not a new workspace-authority boundary.
use prim::{client,server::{self,Backend}};
use serde_json::{json,Value};
#[tokio::test]
async fn unverified_names_cannot_replace_live_commands_or_views_and_sizes_reconcile(){
 let root=tempfile::Builder::new().prefix("plh").tempdir_in("/tmp").unwrap();let socket=root.path().join("s");let core=server::start(socket.clone(),root.path().join("st"),false,Backend::Extensions,None).unwrap();
 let(a,_arx,_)=client::connect(&socket,"first").await.unwrap();let(b,_brx,_)=client::connect(&socket,"second").await.unwrap();
 let ha=a.call("hello",json!({"extension":"claimed"})).await.unwrap();let hb=b.call("hello",json!({"extension":"claimed"})).await.unwrap();assert_ne!(ha["you"],hb["you"]);assert!(!ha["you"].as_str().unwrap().starts_with("ext:"));
 let forged=b.call("hello",json!({"client":"ext:claimed"})).await.unwrap();assert!(!forged["you"].as_str().unwrap().starts_with("ext:"));
 a.call("command.register",json!({"name":"same-command"})).await.unwrap();assert_eq!(b.call("command.register",json!({"name":"same-command"})).await.unwrap_err().kind(),"conflict");
 for i in 0..12{a.call("view.publish",json!({"view":format!("v{i}"),"content":{"text":"x".repeat(64*1024)}})).await.unwrap();}
 let before=core.lock().unwrap().live_bytes;
 assert_eq!(b.call("view.publish",json!({"view":"v0","content":{"text":"hijack"}})).await.unwrap_err().kind(),"conflict");
 assert_eq!(b.call("view.publish",json!({"view":"v0","content":null})).await.unwrap_err().kind(),"conflict");assert_eq!(core.lock().unwrap().live_bytes,before);
 a.call("view.publish",json!({"view":"v0","content":{"text":"small"}})).await.unwrap();
 {let c=core.lock().unwrap();assert!(c.live_bytes<before);assert_eq!(c.live_bytes,c.live.iter().map(|(key,(value,_,_,bytes))|{assert_eq!(*bytes,prim::limits::encoded_bytes(&(key,value)));*bytes}).sum::<usize>());}
 a.call("view.publish",json!({"view":"v0","content":null})).await.unwrap();
 a.close();tokio::time::timeout(std::time::Duration::from_secs(3),async{loop{if core.lock().unwrap().live.is_empty(){break;}tokio::time::sleep(std::time::Duration::from_millis(5)).await;}}).await.unwrap();assert_eq!(core.lock().unwrap().live_bytes,0);
 b.call("view.publish",json!({"view":"v0","content":{"text":"available after disconnect"}})).await.unwrap();
 b.call("view.publish",json!({"view":"v0","content":Value::Null})).await.unwrap();assert_eq!(core.lock().unwrap().live_bytes,0);
 server::shutdown(&core).await;
}
