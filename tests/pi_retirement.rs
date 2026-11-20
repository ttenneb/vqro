//! Real SDK/socket backpressure plus an owned native process; no live core, runtime or model.
#[allow(dead_code)] #[path="../src/bin/vqro-bridge-pi/native.rs"] mod native;
#[path="../src/bin/vqro-bridge-pi/retirement.rs"] mod retirement;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use serde_json::{json,Value};
use tokio::io::{AsyncBufReadExt,AsyncWriteExt,BufReader,DuplexStream,ReadHalf,WriteHalf};
use vqro::provider_client::{Config,Provider};

struct Core {r:BufReader<ReadHalf<DuplexStream>>,w:WriteHalf<DuplexStream>}
impl Core {
    async fn recv(&mut self)->Value {
        let mut line=String::new();let n=tokio::time::timeout(Duration::from_secs(2),self.r.read_line(&mut line)).await.unwrap().unwrap();
        assert!(n>0,"provider disconnected before end evidence");serde_json::from_str(&line).unwrap()
    }
    async fn reply(&mut self,request:&Value,result:Value){self.w.write_all(format!("{}\n",json!({"jsonrpc":"2.0","id":request["id"],"result":result})).as_bytes()).await.unwrap();self.w.flush().await.unwrap();}
}
fn facts(ended:bool)->Value {json!({"ref":"agent","incarnation":"","kind":"agent","state":if ended{"finished"}else{"live"},"data":{"agent":{"lifecycle":if ended{"ended"}else{"live"}},"blob":"x".repeat(2048)}})}
async fn registered()->(Provider,Core) {
    let (a,b)=tokio::io::duplex(256);let (r,w)=tokio::io::split(b);let mut core=Core {r:BufReader::new(r),w};
    let mut config=Config::new("pi-test","instance",json!({"agent.stop":{}}).as_object().unwrap().clone());
    config.outgoing_bytes=4096;config.call_timeout=Duration::from_secs(2);
    let connect=tokio::spawn(Provider::connect(a,config));
    let hello=core.recv().await;core.reply(&hello,json!({"server":"fixture"})).await;
    let register=core.recv().await;core.reply(&register,json!({"generation":7,"limits":{"stream_window_bytes":4096}})).await;
    let (provider,_requests)=connect.await.unwrap().unwrap();
    let p=provider.clone();let sync=tokio::spawn(async move{p.full_sync(vec![facts(false)]).await});
    let request=core.recv().await;core.reply(&request,json!({})).await;sync.await.unwrap().unwrap();
    (provider,core)
}
async fn blocked_publisher(provider:&Provider)->(tokio::task::JoinHandle<()>,tokio::sync::oneshot::Receiver<()>) {
    let p=provider.clone();let (first,first_done)=tokio::sync::oneshot::channel();let (second,second_done)=tokio::sync::oneshot::channel();
    let task=tokio::spawn(async move{p.upsert(facts(false)).await.unwrap();let _=first.send(());p.upsert(facts(false)).await.unwrap();let _=second.send(());});
    first_done.await.unwrap();tokio::task::yield_now().await;
    (task,second_done)
}

#[tokio::test]
async fn native_exit_while_core_is_slow_reports_gone_before_teardown() {
    let (provider,mut core)=registered().await;
    let (publisher,mut second)=blocked_publisher(&provider).await;
    assert!(matches!(second.try_recv(),Err(tokio::sync::oneshot::error::TryRecvError::Empty)),"publication must actually be blocked");
    let root=tempfile::tempdir().unwrap();
    let native=native::Native::spawn(&["/usr/bin/python3".into(),"-c".into(),"import time;time.sleep(60)".into()],root.path(),&HashMap::new(),Arc::new(|_|{})).await.unwrap();
    native.stop();tokio::time::timeout(Duration::from_secs(2),native.exited()).await.unwrap();
    // Exit is known while the provider writer and a publication are still backpressured.
    let p=provider.clone();let retirement=tokio::spawn(async move{retirement::retire(&p,facts(true),"pi-test").await});
    for _ in 0..2 {assert_eq!(core.recv().await["method"],"resource.upsert");}
    let final_facts=core.recv().await;assert_eq!(final_facts["method"],"provider.sync");assert_eq!(final_facts["params"]["resources"][0]["state"],"finished");core.reply(&final_facts,json!({})).await;
    let gone=core.recv().await;assert_eq!(gone["method"],"resource.gone");assert_eq!(gone["params"]["reason"]["reason"],"exited");assert!(gone.get("id").is_none());
    let fence=core.recv().await;assert_eq!(fence["method"],"resource.list");assert_eq!(fence["params"]["provider"],"pi-test");assert_eq!(fence["params"]["backend_ref"],"agent");
    assert!(!retirement.is_finished(),"enqueuing gone is not a core acknowledgement");
    core.reply(&fence,json!({"resources":[]})).await;
    retirement.await.unwrap().unwrap();publisher.await.unwrap();assert!(!provider.is_closed());provider.close();
}

#[tokio::test]
async fn cancelling_blocked_publication_does_not_cancel_the_socket_writer() {
    let (provider,mut core)=registered().await;
    let (publisher,mut second)=blocked_publisher(&provider).await;
    assert!(matches!(second.try_recv(),Err(tokio::sync::oneshot::error::TryRecvError::Empty)));
    publisher.abort();assert!(publisher.await.unwrap_err().is_cancelled());assert!(!provider.is_closed());
    let p=provider.clone();let end=tokio::spawn(async move{
        p.gone("agent","",json!({"reason":"exited"})).await?;
        p.call("resource.list",json!({"provider":"pi-test","backend_ref":"agent"})).await
    });
    assert_eq!(core.recv().await["method"],"resource.upsert");
    assert_eq!(core.recv().await["method"],"resource.gone");
    let fence=core.recv().await;assert_eq!(fence["method"],"resource.list");core.reply(&fence,json!({"resources":[]})).await;
    end.await.unwrap().unwrap();assert!(!provider.is_closed());provider.close();
}
