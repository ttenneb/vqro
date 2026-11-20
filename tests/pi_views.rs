#[path="../src/bin/vqro-bridge-pi/views.rs"] mod views;
use serde_json::{json,Value};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt,AsyncWriteExt,BufReader,DuplexStream,ReadHalf,WriteHalf};
use vqro::provider_client::{Config,Provider};
struct Core{r:BufReader<ReadHalf<DuplexStream>>,w:WriteHalf<DuplexStream>}
impl Core{
    async fn recv(&mut self)->Value{let mut line=String::new();assert!(tokio::time::timeout(Duration::from_secs(2),self.r.read_line(&mut line)).await.unwrap().unwrap()>0);serde_json::from_str(&line).unwrap()}
    async fn reply(&mut self,row:&Value,result:Value){self.w.write_all(format!("{}\n",json!({"jsonrpc":"2.0","id":row["id"],"result":result})).as_bytes()).await.unwrap();}
    async fn failure(&mut self,row:&Value,kind:&str){self.w.write_all(format!("{}\n",json!({"jsonrpc":"2.0","id":row["id"],"error":{"code":-32000,"message":"optional view failure","data":{"kind":kind}}})).as_bytes()).await.unwrap();}
    async fn conflict(&mut self,row:&Value){self.w.write_all(format!("{}\n",json!({"jsonrpc":"2.0","id":row["id"],"error":{"code":-32000,"message":"CAS conflict","data":{"kind":"conflict"}}})).as_bytes()).await.unwrap();}
}
async fn connected()->(Provider,Core){
    let(a,b)=tokio::io::duplex(65536);let(r,w)=tokio::io::split(b);let mut core=Core{r:BufReader::new(r),w};let config=Config::new("pi-test","instance",json!({"agent.send":{}}).as_object().unwrap().clone());let task=tokio::spawn(Provider::connect(a,config));
    let hello=core.recv().await;core.reply(&hello,json!({})).await;let register=core.recv().await;core.reply(&register,json!({"generation":1})).await;let(provider,_requests)=task.await.unwrap().unwrap();(provider,core)
}
#[tokio::test]
async fn every_optional_view_storage_failure_is_local_not_a_provider_refusal(){
    for kind in ["io","overloaded","unavailable"]{
        let(provider,mut core)=connected().await;let p=provider.clone();let task=tokio::spawn(async move{views::claim(&p,"pi-test","instance").await});
        let list=core.recv().await;core.reply(&list,json!({"resources":[{"id":"r42","provider":"pi-test","backend_ref":"agent","kind":"agent"}]})).await;
        let get=core.recv().await;core.reply(&get,Value::Null).await;let apply=core.recv().await;assert_eq!(apply["method"],"workspace.apply");core.failure(&apply,kind).await;
        let setup=task.await.unwrap();assert!(setup.view.is_none());assert!(setup.error.unwrap().contains(kind));assert!(!provider.is_closed(),"optional storage errors cannot disconnect the provider");provider.close();
    }
}
#[tokio::test]
async fn scoped_view_claim_reuses_only_owned_markers_and_fences_cas_races(){
    for scenario in ["new","owned","foreign","raced-foreign"]{
        let(provider,mut core)=connected().await;let p=provider.clone();let task=tokio::spawn(async move{views::claim(&p,"pi-test","instance").await});
        let list=core.recv().await;assert_eq!(list["method"],"resource.list");assert_eq!(list["params"],json!({"provider":"pi-test","backend_ref":"agent","generation":1}));
        core.reply(&list,json!({"resources":[{"id":"r42","provider":"pi-test","backend_ref":"agent","kind":"agent"}]})).await;
        let get=core.recv().await;assert_eq!(get["method"],"workspace.get");assert_eq!(get["params"]["key"],"view/agent-r42");
        let owned=json!({"version":7,"value":{"kind":"panel","bridge":{"instance":"instance","ref":"agent"}}});let foreign=json!({"version":8,"value":{"kind":"panel","bridge":{"instance":"someone-else","ref":"agent"}}});
        core.reply(&get,match scenario{"owned"=>owned,"foreign"=>foreign.clone(),_=>Value::Null}).await;
        if scenario=="new" || scenario=="raced-foreign"{
            let apply=core.recv().await;assert_eq!(apply["method"],"workspace.apply");assert_eq!(apply["params"]["ops"][0]["expect"],0);assert_eq!(apply["params"]["ops"][0]["value"]["bridge"],json!({"instance":"instance","ref":"agent"}));
            if scenario=="new"{core.reply(&apply,json!({})).await;}else{core.conflict(&apply).await;let reread=core.recv().await;assert_eq!(reread["method"],"workspace.get");core.reply(&reread,foreign).await;}
        }
        let setup=task.await.unwrap();if ["new","owned"].contains(&scenario){assert_eq!(setup.view.as_deref(),Some("agent-r42"));assert!(setup.error.is_none());}else{assert!(setup.view.is_none());assert!(setup.error.unwrap().contains("someone else"));}provider.close();
    }
}
