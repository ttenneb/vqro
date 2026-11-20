//! Private native subprocess/real SDK component tests. No live core, agent or model.
use super::*;
use tokio::io::{AsyncBufReadExt,AsyncWriteExt,BufReader,DuplexStream,ReadHalf,WriteHalf};
#[test]
fn stop_reply_preserves_requested_coverage_without_claiming_wider_effects(){
    let covers=vec![json!("foreground")];
    for exit in [None,Some(native::Exit {code:Some(0),success:true})]{
        let result=stop_report(&covers,exit.as_ref());assert_eq!(result["covers"],json!(["foreground"]));
        assert_eq!(result["covers_evidence"],"requested");assert_eq!(result["pending"],"unknown");
    }
}
// Same SDK duplex handshake/fence pattern as tests/pi_retirement.rs.
struct Core {r:BufReader<ReadHalf<DuplexStream>>,w:WriteHalf<DuplexStream>}
impl Core {
    async fn recv(&mut self)->Value{let mut line=String::new();assert!(tokio::time::timeout(Duration::from_secs(3),self.r.read_line(&mut line)).await.unwrap().unwrap()>0);serde_json::from_str(&line).unwrap()}
    async fn reply(&mut self,row:&Value,result:Value){self.w.write_all(format!("{}\n",json!({"jsonrpc":"2.0","id":row["id"],"result":result})).as_bytes()).await.unwrap();self.w.flush().await.unwrap();}
}
async fn fixture(root:&std::path::Path,apply:bool)->(Arc<Shared>,Provider,Core){
    let signals=admission::Signals::new();let shared=Arc::new(Shared {agent:Mutex::new(Agent::new("instance".into(),signals)),native:OnceLock::new(),starts:OnceLock::new(),changed:Notify::new(),cwd:root.to_string_lossy().into_owned(),provider_name:"pi-test".into(),ui_slots:Arc::new(tokio::sync::Semaphore::new(24))});
    let initial=json!({"sessionId":"fixture","isStreaming":false,"isCompacting":false,"steeringMode":"one-at-a-time","followUpMode":"all"});
    {let event=native::Event::Record(json!({"type":"response","command":"get_state","success":true,"data":initial}));let mut agent=shared.agent.lock().unwrap();agent.signals.observe(&event);agent.observe(&event);}
    let script=format!(r#"import sys,json,time,pathlib
root=pathlib.Path({root})
state={{"sessionId":"fixture","isStreaming":False,"isCompacting":False,"steeringMode":"one-at-a-time","followUpMode":"all"}}
for line in sys.stdin:
 row=json.loads(line)
 with (root/'ledger').open('a') as f:f.write(json.dumps(row)+'\n')
 if row['type']=='set_steering_mode':
  while not (root/'go').exists():time.sleep(.002)
  if {apply}:state['steeringMode']=row['mode']
 data=state if row['type']=='get_state' else {{}}
 print(json.dumps({{'type':'response','id':row['id'],'command':row['type'],'success':True,'data':data}}),flush=True)
"#,root=json!(root.to_string_lossy()),apply=if apply{"True"}else{"False"});
    let weak=Arc::downgrade(&shared);
    let native=Native::spawn(&["/usr/bin/python3".into(),"-c".into(),script],root,&HashMap::new(),Arc::new(move|event|{if let Some(shared)=weak.upgrade(){let mut agent=shared.agent.lock().unwrap();agent.signals.observe(&event);agent.observe(&event);drop(agent);shared.wake();}})).await.unwrap();shared.native.set(native).ok().unwrap();
    let (a,b)=tokio::io::duplex(65536);let (r,w)=tokio::io::split(b);let mut core=Core {r:BufReader::new(r),w};
    let config=Config::new("pi-test","instance",shared.agent.lock().unwrap().provider_capabilities());let connect=tokio::spawn(Provider::connect(a,config));
    let hello=core.recv().await;core.reply(&hello,json!({"server":"fixture"})).await;let register=core.recv().await;core.reply(&register,json!({"generation":1})).await;
    let (provider,_requests)=connect.await.unwrap().unwrap();let p=provider.clone();let facts=shared.facts("pi-test");let sync=tokio::spawn(async move{p.full_sync(vec![facts]).await});let request=core.recv().await;core.reply(&request,json!({})).await;sync.await.unwrap().unwrap();
    (shared,provider,core)
}
async fn dispose(shared:&Shared,provider:&Provider){let native=shared.native.get().unwrap();native.stop();tokio::time::timeout(Duration::from_secs(2),native.exited()).await.unwrap();provider.close();}
#[tokio::test]
async fn late_configuration_ack_is_not_replayed_and_fresh_facts_are_fenced(){
    let root=tempfile::tempdir().unwrap();let (shared,provider,mut core)=fixture(root.path(),true).await;
    let args=json!({"id":"steering_mode","value":"all"});let result=configure(&shared,&provider,&args,Instant::now()+Duration::from_millis(100)).await.unwrap();assert!(matches!(result,Outcome::Dispatched(_)));
    assert!(shared.agent.lock().unwrap().configuration.pending);assert_eq!(shared.agent.lock().unwrap().state(),"unknown");
    assert!(matches!(configure(&shared,&provider,&args,Instant::now()+Duration::from_secs(1)).await.unwrap(),Outcome::Rejected{kind,..} if kind=="busy"));
    std::fs::write(root.path().join("go"),"").unwrap();
    let facts=core.recv().await;assert_eq!(facts["method"],"resource.upsert");let config=&facts["params"]["facts"]["data"]["agent"]["config"];
    assert!(config.as_array().unwrap().iter().any(|row|row["id"]=="steering_mode" && row["value"]=="all"));
    let fence=core.recv().await;assert_eq!(fence["method"],"resource.list");core.reply(&fence,json!({"resources":[]})).await;
    let ledger=std::fs::read_to_string(root.path().join("ledger")).unwrap();let rows:Vec<Value>=ledger.lines().map(|line|serde_json::from_str(line).unwrap()).collect();assert_eq!(rows.iter().filter(|row|row["type"]=="set_steering_mode").count(),1);assert_eq!(rows.iter().filter(|row|row["type"]=="get_state").count(),1);
    assert!(!shared.agent.lock().unwrap().configuration.pending);dispose(&shared,&provider).await;
}
#[tokio::test]
async fn setter_ack_without_matching_readback_is_only_accepted_and_waits_for_core_fence(){
    let root=tempfile::tempdir().unwrap();std::fs::write(root.path().join("go"),"").unwrap();let (shared,provider,mut core)=fixture(root.path(),false).await;
    let (s,p)=(shared.clone(),provider.clone());let task=tokio::spawn(async move{configure(&s,&p,&json!({"id":"steering_mode","value":"all"}),Instant::now()+Duration::from_secs(3)).await});
    assert_eq!(core.recv().await["method"],"resource.upsert");let fence=core.recv().await;assert_eq!(fence["method"],"resource.list");assert!(!task.is_finished());core.reply(&fence,json!({"resources":[]})).await;
    assert!(matches!(task.await.unwrap().unwrap(),Outcome::Accepted(Some(data)) if data["value_evidence"]=="not_matched"));
    assert_eq!(shared.agent.lock().unwrap().configuration.options().iter().find(|row|row["id"]=="steering_mode").unwrap()["value"],"one-at-a-time");dispose(&shared,&provider).await;
}
