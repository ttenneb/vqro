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
#[test]
fn tool_principal_claim_excludes_workspace_owner_and_unknown_owner_evidence(){
    assert!(agent_tool_principal("p:agent",&json!({"value":"human-owner"})));
    assert!(!agent_tool_principal("human-owner",&json!({"value":"human-owner"})));
    assert!(!agent_tool_principal("owner",&json!({"value":null})));
    assert!(!agent_tool_principal("p:agent",&json!({})));
    let mut agent=Agent::new("fixture".into(),admission::Signals::new());assert!(agent.caps()["agent"].get("tool_principal").is_none());
    assert_eq!(agent.provider_capabilities()["agent"]["tool_principal"],json!(["env"]));agent.tool_principal_env=true;agent.refresh();assert_eq!(agent.caps()["agent"]["tool_principal"],"env");
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
 if row['type']=='new_session':state['sessionId']='fixture-next'
 if row['type']=='cycle_model':state['model']={{'provider':'p','id':'two','name':'Two','input':['text','image'],'headers':{{'Authorization':'fixture-secret'}}}}
 if row['type']=='get_state':data=state
 elif row['type']=='cycle_model':data={{'model':state['model'],'thinkingLevel':'low'}}
 elif row['type']=='compact':data={{'summary':'Native summary','firstKeptEntryId':'entry','tokensBefore':123}}
 else:data={{}}
 print(json.dumps({{'type':'response','id':row['id'],'command':row['type'],'success':True,'data':data}}),flush=True)
"#,root=json!(root.to_string_lossy()),apply=if apply{"True"}else{"False"});
    let weak=Arc::downgrade(&shared);
    let native=Native::spawn(&["/usr/bin/python3".into(),"-c".into(),script],root,&HashMap::new(),Arc::new(move|event|{if let Some(shared)=weak.upgrade(){let mut agent=shared.agent.lock().unwrap();agent.signals.observe(&event);agent.observe(&event);drop(agent);shared.wake();}})).await.unwrap();shared.starts.set(Starts::new(native.clone(),shared.agent.lock().unwrap().signals.clone())).ok().unwrap();shared.native.set(native).ok().unwrap();
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
async fn new_session_keeps_endpoint_and_host_and_fences_one_new_epoch_before_reply(){
    for known_prior in [true,false]{
        let root=tempfile::tempdir().unwrap();let (shared,provider,mut core)=fixture(root.path(),true).await;
        {let mut agent=shared.agent.lock().unwrap();if !known_prior{agent.session=None;}agent.journal.put("message","old",json!({"content":[]})).unwrap();agent.receipt("prior-delivery",json!({"stage":"admitted"}));}
        let before=shared.agent.lock().unwrap().epoch().to_string();let host=shared.facts("pi-test")["data"]["agent"]["host"].clone();
        let guard=shared.starts.get().unwrap().session_guard().unwrap();
        assert!(matches!(session_new(&shared,&provider,&json!({"action":"new"}),Instant::now()+Duration::from_secs(1)).await.unwrap(),Outcome::Rejected{kind,..} if kind=="busy"));
        assert!(!root.path().join("ledger").exists(),"an unresolved preflight must reject session changes before write");drop(guard);
        let (s,p)=(shared.clone(),provider.clone());let task=tokio::spawn(async move{session_new(&s,&p,&json!({"action":"new"}),Instant::now()+Duration::from_secs(3)).await});
        let facts=core.recv().await;let published=&facts["params"]["facts"];assert_eq!(published["ref"],"agent");assert_eq!(published["data"]["agent"]["host"],host);
        assert_eq!(published["data"]["agent"]["session"]["id"],"fixture-next");assert_ne!(published["data"]["agent"]["epoch"],before);assert!(published["data"]["agent"]["epoch"].as_str().unwrap().ends_with(".e2"));
        let fence=core.recv().await;assert_eq!(fence["method"],"resource.list");assert!(!task.is_finished());core.reply(&fence,json!({"resources":[]})).await;
        assert!(matches!(task.await.unwrap().unwrap(),Outcome::Observed(Some(data)) if data["session"]["id"]=="fixture-next"));
        {let agent=shared.agent.lock().unwrap();assert!(agent.journal.entity("message","old").is_none());assert!(agent.journal.entity("session","current").is_some());assert!(agent.receipts.contains_key("prior-delivery"));}
        dispose(&shared,&provider).await;
    }
}
#[tokio::test]
async fn builtin_commands_use_native_rpc_readback_fence_and_no_model_header_leak(){
    for name in ["cycle_model","compact"]{
        let root=tempfile::tempdir().unwrap();let (shared,provider,mut core)=fixture(root.path(),true).await;
        let (s,p)=(shared.clone(),provider.clone());let task=tokio::spawn(async move{command(&s,&p,&json!({"name":name}),Instant::now()+Duration::from_secs(3)).await});
        let facts=core.recv().await;assert_eq!(facts["method"],"resource.upsert");assert!(!facts.to_string().contains("fixture-secret"));
        if name=="cycle_model"{assert_eq!(facts["params"]["facts"]["data"]["agent"]["session"]["model"]["id"],"two");}
        let fence=core.recv().await;assert_eq!(fence["method"],"resource.list");assert!(!task.is_finished());core.reply(&fence,json!({"resources":[]})).await;
        let Outcome::Observed(Some(data))=task.await.unwrap().unwrap()else{panic!("reported command result");};assert!(!data.to_string().contains("fixture-secret"));
        let ledger=std::fs::read_to_string(root.path().join("ledger")).unwrap();let rows:Vec<Value>=ledger.lines().map(|line|serde_json::from_str(line).unwrap()).collect();assert_eq!(rows.iter().filter(|row|row["type"]==name).count(),1);assert!(!rows.iter().any(|row|row["type"]=="prompt"));
        dispose(&shared,&provider).await;
    }
}
#[tokio::test]
async fn explicit_native_prompt_command_shares_start_gate_and_is_only_accepted(){
    let root=tempfile::tempdir().unwrap();let (shared,provider,_core)=fixture(root.path(),true).await;
    shared.agent.lock().unwrap().commands.observe(&json!({"commands":[{"name":"example"}]}));
    let guard=shared.starts.get().unwrap().session_guard().unwrap();assert!(matches!(command(&shared,&provider,&json!({"name":"example"}),Instant::now()+Duration::from_secs(1)).await.unwrap(),Outcome::Rejected{kind,..} if kind=="busy"));assert!(!root.path().join("ledger").exists());drop(guard);
    assert!(matches!(command(&shared,&provider,&json!({"name":"example","args":"exact\nargs"}),Instant::now()+Duration::from_secs(2)).await.unwrap(),Outcome::Accepted(_)));
    let signals=shared.agent.lock().unwrap().signals.clone();let mut changed=signals.changes();tokio::time::timeout(Duration::from_secs(2),async{loop{if signals.current().preflights==0{break;}changed.changed().await.unwrap();}}).await.unwrap();
    let ledger=std::fs::read_to_string(root.path().join("ledger")).unwrap();let rows:Vec<Value>=ledger.lines().map(|line|serde_json::from_str(line).unwrap()).collect();assert_eq!(rows.iter().filter(|row|row["type"]=="prompt").count(),1);assert_eq!(rows.iter().find(|row|row["type"]=="prompt").unwrap()["message"],"/example exact\nargs");
    dispose(&shared,&provider).await;
}
#[tokio::test]
async fn setter_ack_without_matching_readback_is_only_accepted_and_waits_for_core_fence(){
    let root=tempfile::tempdir().unwrap();std::fs::write(root.path().join("go"),"").unwrap();let (shared,provider,mut core)=fixture(root.path(),false).await;
    let (s,p)=(shared.clone(),provider.clone());let task=tokio::spawn(async move{configure(&s,&p,&json!({"id":"steering_mode","value":"all"}),Instant::now()+Duration::from_secs(3)).await});
    assert_eq!(core.recv().await["method"],"resource.upsert");let fence=core.recv().await;assert_eq!(fence["method"],"resource.list");assert!(!task.is_finished());core.reply(&fence,json!({"resources":[]})).await;
    assert!(matches!(task.await.unwrap().unwrap(),Outcome::Accepted(Some(data)) if data["value_evidence"]=="not_matched"));
    assert_eq!(shared.agent.lock().unwrap().configuration.options().iter().find(|row|row["id"]=="steering_mode").unwrap()["value"],"one-at-a-time");dispose(&shared,&provider).await;
}
