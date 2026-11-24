//! Private native subprocess/real SDK component tests. No live core, agent or model.
use super::*;
#[path="../../../tests/support/frame_metadata.rs"] mod frame_metadata;
use tokio::io::{AsyncBufReadExt,AsyncWriteExt,BufReader,DuplexStream,ReadHalf,WriteHalf};
#[test]
fn configured_account_is_launch_metadata_not_native_auth_state(){
    let mut agent=Agent::new("fixture".into(),admission::Signals::new());
    assert!(agent.facts("fixture","/fake")["data"]["agent"].get("account").is_none());
    let account=json!({"id":"work","label":"Work","kind":"subscription","sources":{"id":"vqro:launcher","label":"vqro:launcher","kind":"vqro:launcher"}});
    agent.account=launcher_account::parse(Some(&account.to_string())).unwrap();
    assert_eq!(agent.facts("fixture","/fake")["data"]["agent"]["account"],account);
    agent.new_session_epoch();
    assert_eq!(agent.facts("fixture","/fake")["data"]["agent"]["account"],account);
    let mut command=tokio::process::Command::new("not-executed");
    native::native_env(&mut command,&HashMap::from([("VQRO_ACCOUNT".into(),account.to_string()),("PI_CODING_AGENT_DIR".into(),"/fake/profile".into())]));
    let env:HashMap<_,_>=command.as_std().get_envs().map(|(k,v)|(k.to_string_lossy().into_owned(),v.map(|v|v.to_string_lossy().into_owned()))).collect();
    assert!(!env.contains_key("VQRO_ACCOUNT"));
    assert_eq!(env["PI_CODING_AGENT_DIR"].as_deref(),Some("/fake/profile"));
}
#[test]
fn stop_reply_preserves_requested_coverage_without_claiming_wider_effects(){
    let covers=vec![json!("foreground")];
    for exit in [None,Some(native::Exit {code:Some(0),success:true,observed:true})]{
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
struct Core {r:BufReader<ReadHalf<DuplexStream>>,w:WriteHalf<DuplexStream>,requests:Option<tokio::sync::mpsc::UnboundedReceiver<Request>>}
impl Core {
    async fn recv(&mut self)->Value{let mut line=String::new();assert!(tokio::time::timeout(Duration::from_secs(3),self.r.read_line(&mut line)).await.unwrap().unwrap()>0);serde_json::from_str(&line).unwrap()}
    async fn reply(&mut self,row:&Value,result:Value){self.w.write_all(format!("{}\n",json!({"jsonrpc":"2.0","id":row["id"],"result":result})).as_bytes()).await.unwrap();self.w.flush().await.unwrap();}
}
async fn fixture(root:&std::path::Path,apply:bool)->(Arc<Shared>,Provider,Core){
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
    fixture_native(root,&["/usr/bin/python3".into(),"-c".into(),script]).await
}
/// The bridge over any native argv (an inline script, or tests/fixtures/fake_pi.py), connected to a duplex core.
async fn fixture_native(root:&std::path::Path,argv:&[String])->(Arc<Shared>,Provider,Core){
    let signals=admission::Signals::new();let shared=Arc::new(Shared {agent:Mutex::new(Agent::new("instance".into(),signals)),fallback:Default::default(),connected:std::sync::atomic::AtomicBool::new(false),native:OnceLock::new(),starts:OnceLock::new(),changed:Notify::new(),metadata_refresh:Notify::new(),cwd:root.to_string_lossy().into_owned(),provider_name:"pi-test".into(),instance:"instance".into(),ui_slots:Arc::new(tokio::sync::Semaphore::new(24))});
    let initial=json!({"sessionId":"fixture","isStreaming":false,"isCompacting":false,"steeringMode":"one-at-a-time","followUpMode":"all"});
    {let event=native::Event::Record(json!({"type":"response","command":"get_state","success":true,"data":initial}));let mut agent=shared.agent.lock().unwrap();agent.signals.observe(&event);agent.observe(&event);}
    let weak=Arc::downgrade(&shared);
    let native=Native::spawn(argv,root,&HashMap::new(),Arc::new(move|event|{if let Some(shared)=weak.upgrade(){let mut agent=shared.agent.lock().unwrap();agent.signals.observe(&event);agent.observe(&event);drop(agent);shared.wake();}})).await.unwrap();shared.starts.set(Starts::new(native.clone(),shared.agent.lock().unwrap().signals.clone())).ok().unwrap();shared.native.set(native).ok().unwrap();
    let (a,b)=tokio::io::duplex(65536);let (r,w)=tokio::io::split(b);let mut core=Core {r:BufReader::new(r),w,requests:None};
    let config=Config::new("pi-test","instance",shared.agent.lock().unwrap().provider_capabilities());let connect=tokio::spawn(Provider::connect(a,config));
    let hello=core.recv().await;core.reply(&hello,json!({"server":"fixture"})).await;let register=core.recv().await;core.reply(&register,json!({"generation":1})).await;
    let (provider,requests)=connect.await.unwrap().unwrap();core.requests=Some(requests);let p=provider.clone();let facts=shared.facts("pi-test");let sync=tokio::spawn(async move{p.full_sync(vec![facts]).await});let request=core.recv().await;core.reply(&request,json!({})).await;sync.await.unwrap().unwrap();
    (shared,provider,core)
}
async fn dispose(shared:&Shared,provider:&Provider){let native=shared.native.get().unwrap();native.stop();tokio::time::timeout(Duration::from_secs(2),native.exited()).await.unwrap();provider.close();}
#[tokio::test]
async fn c2_effect_identity_mismatch_is_forbidden_before_any_native_write(){
    let root=tempfile::tempdir().unwrap();let (shared,provider,mut core)=fixture(root.path(),true).await;
    for (index,field) in [Some("principal"),Some("actor"),Some("initiator"),None].into_iter().enumerate(){
        let mut args=json!({"v":0,"mode":"start","delivery":format!("d{index}"),"content":[{"type":"text","text":"exact literal"}],"origin":{"principal":"p:sender","actor":"extension:coord","kind":"agent","agent":"r99"},"quoted":{"label":"asserted source"}});
        if let Some(field)=field{args["origin"][field]=json!("owner");}else{args["origin"].as_object_mut().unwrap().remove("principal");args["origin"].as_object_mut().unwrap().remove("actor");}
        let wire=json!({"jsonrpc":"2.0","id":index,"method":"effect.invoke","params":{"generation":1,"effect":format!("e{index}"),"op":"agent.send","ref":"agent","incarnation":"","args":args,"deadline_ms":2000,"principal":"p:sender","actor":"extension:coord","initiator":"p:original","cause":{"kind":"delivery","id":"original-delivery","provenance":"asserted","extra":"preserved"},"correlation":"root-only-thread"}});
        core.w.write_all(format!("{wire}\n").as_bytes()).await.unwrap();let request=core.requests.as_mut().unwrap().recv().await.unwrap();let Request::Effect(effect)=request else{panic!("not an effect")};
        let result=if field.is_none(){
            let respond_owner=async {let get=core.recv().await;assert_eq!(get["method"],"workspace.get");assert_eq!(get["params"]["key"],vqro::principal::OWNER_KEY);core.reply(&get,json!({"value":"actual-owner"})).await;};
            let (result,())=tokio::join!(invoke(&shared,&provider,&effect),respond_owner);result.unwrap()
        }else{invoke(&shared,&provider,&effect).await.unwrap()};
        if field.is_some(){assert!(matches!(result,Outcome::Rejected{ref kind,..} if kind=="forbidden"));assert!(!root.path().join("ledger").exists());assert!(shared.agent.lock().unwrap().receipts.is_empty());}
        else{let Outcome::Accepted(Some(value))=&result else{panic!("not accepted: {result:?}")};assert_eq!(value["receipt"]["origin"]["principal"],"p:sender");assert_eq!(value["receipt"]["origin"]["actor"],"extension:coord");assert_eq!(value["receipt"]["origin"]["initiator"],"p:original");assert_eq!(value["receipt"]["origin"]["claimed"],json!(["kind","agent"]));assert_eq!(value["receipt"]["quoted"],args["quoted"]);assert_eq!(value["receipt"]["cause"],wire["params"]["cause"]);assert_eq!(value["receipt"]["correlation"],"root-only-thread");}
        effect.respond(Ok(result));let reply=core.recv().await;assert_eq!(reply["id"],index);
    }
    let ledger=std::fs::read_to_string(root.path().join("ledger")).unwrap();let rows:Vec<Value>=ledger.lines().map(|line|serde_json::from_str(line).unwrap()).collect();let prompts:Vec<_>=rows.iter().filter(|row|row["type"]=="prompt").collect();assert_eq!(prompts.len(),1);assert!(prompts[0]["message"].as_str().unwrap().ends_with("\n\nexact literal"));assert_eq!(prompts[0]["message"],"Quoted provenance (asserted, not the effective sender): asserted source\nfrom p:sender via extension:coord\nasserted kind: agent\nasserted agent: r99\n\nexact literal");dispose(&shared,&provider).await;
}
#[tokio::test]
async fn injectable_frame_metadata_is_rejected_before_any_native_write(){
    let root=tempfile::tempdir().unwrap();let (shared,provider,mut core)=fixture(root.path(),true).await;
    let base=json!({"v":0,"mode":"start","delivery":"invalid-frame","content":[{"type":"text","text":"literal body"}],"origin":{"kind":"agent"}});
    for (index,(label,mut args)) in frame_metadata::invalid_inputs(&base).into_iter().enumerate(){
        args["delivery"]=json!(format!("invalid-frame-{index}"));
        let wire=json!({"jsonrpc":"2.0","id":index,"method":"effect.invoke","params":{"generation":1,"effect":format!("invalid-frame-{index}"),"op":"agent.send","ref":"agent","incarnation":"","args":args,"deadline_ms":2000,"principal":"p:sender","actor":"extension:coord","initiator":"p:original"}});
        core.w.write_all(format!("{wire}\n").as_bytes()).await.unwrap();
        let request=core.requests.as_mut().unwrap().recv().await.unwrap();let Request::Effect(effect)=request else{panic!("not an effect")};
        let respond_owner=async{let get=core.recv().await;assert_eq!(get["method"],"workspace.get");core.reply(&get,json!({"value":"other-owner"})).await;};
        let (result,())=tokio::join!(invoke(&shared,&provider,&effect),respond_owner);let result=result.unwrap();
        assert!(matches!(result,Outcome::Rejected{ref kind,..} if kind=="invalid"),"{label}: {result:?}");
        assert!(!root.path().join("ledger").exists(),"{label} reached native input");
        effect.respond(Ok(result));let reply=core.recv().await;assert_eq!(reply["id"],index);
    }
    assert!(shared.agent.lock().unwrap().receipts.is_empty());
    dispose(&shared,&provider).await;
}

#[tokio::test]
async fn owner_human_effect_reaches_native_as_exact_typed_text(){
    let root=tempfile::tempdir().unwrap();let (shared,provider,mut core)=fixture(root.path(),true).await;
    let typed="  Hello?\n\n{\"literal\": true}\t  ";
    let wire=json!({"jsonrpc":"2.0","id":99,"method":"effect.invoke","params":{"generation":1,"effect":"owner-send","op":"agent.send","ref":"agent","incarnation":"","deadline_ms":2000,
        "principal":"person:alice","actor":"extension:agents","initiator":"person:alice",
        "args":{"v":0,"mode":"start","delivery":"owner-send","origin":{"kind":"human"},"content":[{"type":"text","text":typed}]}}});
    core.w.write_all(format!("{wire}\n").as_bytes()).await.unwrap();
    let Request::Effect(effect)=core.requests.as_mut().unwrap().recv().await.unwrap() else{panic!("not effect")};
    let owner_read=async {let get=core.recv().await;assert_eq!(get["method"],"workspace.get");assert_eq!(get["params"]["key"],vqro::principal::OWNER_KEY);core.reply(&get,json!({"value":"person:alice"})).await;};
    let (outcome,())=tokio::join!(invoke(&shared,&provider,&effect),owner_read);
    assert!(matches!(outcome.unwrap(),Outcome::Accepted(_)));
    let rows:Vec<Value>=std::fs::read_to_string(root.path().join("ledger")).unwrap().lines().map(|row|serde_json::from_str(row).unwrap()).collect();
    let prompts:Vec<_>=rows.iter().filter(|row|row["type"]=="prompt").collect();assert_eq!(prompts.len(),1);
    assert_eq!(prompts[0]["message"].as_str().unwrap().as_bytes(),typed.as_bytes());
    effect.respond(Ok(Outcome::Accepted(None)));let reply=core.recv().await;assert_eq!(reply["id"],99);
    dispose(&shared,&provider).await;
}
#[tokio::test]
async fn optional_view_retries_are_three_per_connection_and_do_not_block_native_reader(){
    let root=tempfile::tempdir().unwrap();let (shared,provider,mut core)=fixture(root.path(),true).await;let task=tokio::spawn(publish(shared.clone(),provider.clone(),"pi-test".into()));let mut attempts=0;
    while attempts<3{
        let row=core.recv().await;
        match row["method"].as_str().unwrap(){
            "resource.list"=>{assert!(shared.native.get().unwrap().call("get_state",json!({}),Duration::from_secs(1)).await.unwrap().success);core.reply(&row,json!({"resources":[{"id":"r42","provider":"pi-test","backend_ref":"agent","kind":"agent"}]})).await;},
            "workspace.get"=>core.reply(&row,Value::Null).await,
            "workspace.apply"=>{attempts+=1;core.w.write_all(format!("{}\n",json!({"jsonrpc":"2.0","id":row["id"],"error":{"code":-32000,"message":"optional io","data":{"kind":"io"}}})).as_bytes()).await.unwrap();},
            "resource.upsert"=>{},other=>panic!("unexpected {other}"),
        }
    }
    tokio::select!{_=tokio::time::sleep(Duration::from_millis(2200))=>{},row=core.recv()=>panic!("unbounded optional view retry: {row}")}
    assert!(shared.native.get().unwrap().alive());assert!(shared.agent.lock().unwrap().view_error.is_some());task.abort();let _=task.await;dispose(&shared,&provider).await;
}
#[tokio::test]
async fn optional_view_io_overload_and_unavailability_never_stop_native_controls(){
    for kind in ["io","overloaded","unavailable"]{
        let root=tempfile::tempdir().unwrap();let (shared,provider,mut core)=fixture(root.path(),true).await;let p=provider.clone();
        let task=tokio::spawn(async move{views::claim(&p,"pi-test","instance").await});
        let list=core.recv().await;core.reply(&list,json!({"resources":[{"id":"r42","provider":"pi-test","backend_ref":"agent","kind":"agent"}]})).await;
        let get=core.recv().await;core.reply(&get,Value::Null).await;let apply=core.recv().await;assert_eq!(apply["method"],"workspace.apply");
        core.w.write_all(format!("{}\n",json!({"jsonrpc":"2.0","id":apply["id"],"error":{"code":-32000,"message":"optional storage failure","data":{"kind":kind}}})).as_bytes()).await.unwrap();
        let setup=task.await.unwrap();assert!(setup.view.is_none());assert!(setup.error.unwrap().contains(kind));assert!(!provider.is_closed());
        let native=shared.native.get().unwrap();assert!(native.alive());assert!(native.call("get_state",json!({}),Duration::from_secs(2)).await.unwrap().success);dispose(&shared,&provider).await;
    }
}
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

/// An agent.send effect from a non-owner agent sender, through the bridge's public provider path.
async fn send(shared:&Arc<Shared>,provider:&Provider,core:&mut Core,id:u64,mode:&str,text:&str)->Outcome{
    let wire=json!({"jsonrpc":"2.0","id":id,"method":"effect.invoke","params":{"generation":1,"effect":format!("e{id}"),"op":"agent.send","ref":"agent","incarnation":"","deadline_ms":2000,
        "principal":"p:sender","actor":"extension:agents","initiator":"p:sender",
        "args":{"v":0,"mode":mode,"delivery":format!("d{id}"),"origin":{"kind":"agent"},"content":[{"type":"text","text":text}]}}});
    core.w.write_all(format!("{wire}\n").as_bytes()).await.unwrap();
    let Request::Effect(effect)=core.requests.as_mut().unwrap().recv().await.unwrap() else{panic!("not effect")};
    let owner=async{let get=core.recv().await;assert_eq!(get["method"],"workspace.get");core.reply(&get,json!({"value":"other-owner"})).await;};
    let (outcome,())=tokio::join!(invoke(shared,provider,&effect),owner);let outcome=outcome.unwrap();
    effect.respond(Ok(outcome.clone()));let reply=core.recv().await;assert_eq!(reply["id"],id);
    outcome
}
async fn until(shared:&Arc<Shared>,what:&str,check:impl Fn(&Arc<Shared>)->bool){
    let deadline=Instant::now()+Duration::from_secs(3);
    while !check(shared){assert!(Instant::now()<deadline,"timed out waiting for {what}");tokio::time::sleep(Duration::from_millis(5)).await;}
}

/// Relay-13: a follow-up (settle) or steer (boundary) sent while Pi is idle was parked in Pi's queue and never ran.
/// Every mode is now Pi's `prompt` with `streamingBehavior`: idle starts a run, busy queues. One native write each.
#[tokio::test]
async fn settle_and_boundary_start_an_idle_pi_and_queue_on_a_busy_one_exactly_once(){
    fn prompts(trace:&std::path::Path)->Vec<Value>{
        std::fs::read_to_string(trace).unwrap_or_default().lines().map(|row|serde_json::from_str::<Value>(row).unwrap())
            .filter(|row|row.get("type").is_some()).filter(|row|row["type"]!="get_state" && row["type"]!="get_session_stats" && row["type"]!="get_messages" && row["type"]!="get_commands").collect()
    }
    let fake=format!("{}/tests/fixtures/fake_pi.py",env!("CARGO_MANIFEST_DIR"));
    for (mode,behavior) in [("settle","followUp"),("boundary","steer")]{
        let root=tempfile::tempdir().unwrap();let trace=root.path().join("trace");
        let (shared,provider,mut core)=fixture_native(root.path(),&["/usr/bin/python3".into(),fake.clone(),"--trace-file".into(),trace.to_string_lossy().into_owned()]).await;
        assert_eq!(shared.agent.lock().unwrap().caps()["agent.send"]["modes"][mode]["idle"],"start","{mode}: an idle Pi starts this mode natively");
        assert_eq!(shared.agent.lock().unwrap().state(),"idle");
        // Idle: one native prompt that starts a run; nothing is parked.
        let outcome=send(&shared,&provider,&mut core,1,mode,&format!("{mode} while idle")).await;
        let Outcome::Accepted(Some(value))=&outcome else{panic!("{mode}: not accepted: {outcome:?}")};
        assert_eq!(value["receipt"]["stage"],"admitted","{mode}: Pi said started, which is not yet insertion evidence");
        until(&shared,"the run to start",|shared|shared.agent.lock().unwrap().state()!="idle").await;
        let sent=prompts(&trace);
        assert_eq!(sent.len(),1,"{mode}: exactly one native write: {sent:?}");
        assert_eq!((sent[0]["type"].as_str(),sent[0]["streamingBehavior"].as_str()),(Some("prompt"),Some(behavior)));
        assert!(sent[0]["message"].as_str().unwrap().ends_with(&format!("{mode} while idle")));
        // Busy: the same native op queues (Pi decides atomically); still exactly one write, and it is in Pi's queue.
        until(&shared,"the start preflight to settle",|shared|shared.starts.get().unwrap().session_guard().is_ok()).await;
        let outcome=send(&shared,&provider,&mut core,2,mode,&format!("{mode} while busy")).await;
        let Outcome::Accepted(Some(value))=&outcome else{panic!("{mode}: not accepted: {outcome:?}")};
        assert_eq!(value["receipt"]["stage"],"queued","{mode}: Pi's queued disposition is placement evidence");
        assert_eq!(shared.agent.lock().unwrap().receipts["d2"]["stage"],"queued");
        until(&shared,"Pi's queue update",|shared|shared.agent.lock().unwrap().queue.as_ref().is_some_and(|queue|!queue.as_array().unwrap().is_empty())).await;
        let queued=shared.agent.lock().unwrap().queue.clone().unwrap();
        assert_eq!(queued.as_array().unwrap().len(),1,"{mode}: one queued entry: {queued}");
        assert_eq!(queued[0]["mode"],mode,"{mode}: queued in Pi's own queue for this mode: {queued}");
        assert!(queued[0]["preview"].as_str().unwrap().contains(&format!("{mode} while busy")),"{queued}");
        let sent=prompts(&trace);
        assert_eq!(sent.len(),2,"{mode}: one more native write: {sent:?}");
        assert_eq!((sent[1]["type"].as_str(),sent[1]["streamingBehavior"].as_str()),(Some("prompt"),Some(behavior)));
        assert!(sent.iter().all(|row|row["type"]!="follow_up" && row["type"]!="steer"),"queue-only commands park input on an idle Pi");
        dispose(&shared,&provider).await;
    }
}

/// The core-bound origin reaches a user message only when the bridge sent it and the evidence holds (origins.rs):
/// started = the run's starting message with exact text; queued = oldest bridge entry of that queue, exact text, no
/// identical text from anyone else. Natively produced, rewritten, cleared, pre-switch and pre-restart: no origin.
#[tokio::test]
async fn user_messages_carry_the_bridge_origin_only_under_evidence(){
    fn users(shared:&Arc<Shared>)->Vec<Value>{
        let snapshot=String::from_utf8(shared.agent.lock().unwrap().journal.snapshot()).unwrap();
        let mut rows:Vec<Value>=snapshot.lines().map(|line|serde_json::from_str::<Value>(line).unwrap())
            .filter(|row|row["op"]=="put" && row["kind"]=="message" && row["value"]["role"]=="user").map(|row|row["value"].clone()).collect();
        rows.sort_by_key(|value|value["position"].as_u64());rows
    }
    fn last(shared:&Arc<Shared>,body:&str)->Value{
        users(shared).into_iter().rev().find(|value|value["content"][0]["text"].as_str().is_some_and(|text|text.ends_with(body))).unwrap_or_else(||panic!("no user message {body:?}"))
    }
    async fn native(shared:&Arc<Shared>,verb:&str,args:Value){shared.native.get().unwrap().call(verb,args,Duration::from_secs(2)).await.unwrap();}
    async fn free(shared:&Arc<Shared>){until(shared,"the start gate",|shared|shared.starts.get().unwrap().session_guard().is_ok()).await;}
    fn receipt_origin(outcome:&Outcome)->Value{let Outcome::Accepted(Some(value))=outcome else{panic!("not accepted: {outcome:?}")};value["receipt"]["origin"].clone()}
    let fake=format!("{}/tests/fixtures/fake_pi.py",env!("CARGO_MANIFEST_DIR"));
    let root=tempfile::tempdir().unwrap();
    let (shared,provider,mut core)=fixture_native(root.path(),&["/usr/bin/python3".into(),fake.clone(),"--user-messages".into()]).await;
    // fake_pi's session id differs from the fixture's initial metadata: settle that epoch change before sending.
    native(&shared,"get_state",json!({})).await;

    // started: the run's starting message is the bridge's exact text.
    let origin=receipt_origin(&send(&shared,&provider,&mut core,1,"settle","started from the pane").await);
    assert_eq!(origin["principal"],"p:sender");
    until(&shared,"the starting message",|shared|users(shared).len()==1).await;
    let started=last(&shared,"started from the pane");
    assert_eq!((&started["origin"],&started["delivery"]),(&origin,&json!("d1")),"{started}");
    // A user message the bridge did not send (an extension's, or typed natively): no origin.
    native(&shared,"test_user",json!({"text":"typed natively"})).await;
    assert!(last(&shared,"typed natively").get("origin").is_none());

    // queued: the oldest bridge entry of the delivering queue, exact text.
    free(&shared).await;let steer=receipt_origin(&send(&shared,&provider,&mut core,2,"boundary","steer one").await);
    free(&shared).await;send(&shared,&provider,&mut core,3,"settle","follow one").await;
    let follow_text=shared.agent.lock().unwrap().queue.clone().unwrap()[1]["preview"].as_str().unwrap().to_string();
    // A Pi extension queues byte-identical text behind it: Pi then holds two, the bridge sent one, so neither is named.
    native(&shared,"test_inject",json!({"queue":"followUp","text":follow_text})).await;
    native(&shared,"test_drain",json!({"queue":"steering"})).await;
    let steered=last(&shared,"steer one");assert_eq!((&steered["origin"],&steered["delivery"]),(&steer,&json!("d2")),"{steered}");
    native(&shared,"test_drain",json!({"queue":"followUp"})).await;native(&shared,"test_drain",json!({"queue":"followUp"})).await;
    let follows:Vec<Value>=users(&shared).into_iter().filter(|v|v["content"][0]["text"].as_str().is_some_and(|t|t.ends_with("follow one"))).collect();
    assert_eq!(follows.len(),2);assert!(follows.iter().all(|v|v.get("origin").is_none()),"ambiguous identical text: no origin: {follows:?}");

    // clear_queue drops the bridge entry: the same text delivered later is not attributed.
    free(&shared).await;send(&shared,&provider,&mut core,4,"settle","to be cleared").await;
    let cleared=shared.agent.lock().unwrap().queue.clone().unwrap()[0]["preview"].as_str().unwrap().to_string();
    native(&shared,"clear_queue",json!({})).await;
    native(&shared,"test_inject",json!({"queue":"followUp","text":cleared})).await;native(&shared,"test_drain",json!({"queue":"followUp"})).await;
    assert!(last(&shared,"to be cleared").get("origin").is_none());

    // A new session drops everything pending.
    free(&shared).await;send(&shared,&provider,&mut core,5,"settle","before the switch").await;
    shared.agent.lock().unwrap().new_session_epoch();
    native(&shared,"test_drain",json!({"queue":"followUp"})).await;
    assert!(last(&shared,"before the switch").get("origin").is_none());

    // Pi rewrites the started text (as it appends image-resize hints): no exact match, no origin.
    native(&shared,"test_end",json!({})).await;native(&shared,"test_settle",json!({})).await;
    until(&shared,"idle",|shared|shared.agent.lock().unwrap().state()=="idle").await;
    native(&shared,"test_rewrite",json!({"hint":"\n\n[image resized]"})).await;
    free(&shared).await;send(&shared,&provider,&mut core,6,"start","rewritten by Pi").await;
    until(&shared,"the rewritten message",|shared|users(shared).iter().any(|v|v["content"][0]["text"].as_str().is_some_and(|t|t.ends_with("[image resized]")))).await;
    assert!(users(&shared).iter().filter(|v|v["content"][0]["text"].as_str().is_some_and(|t|t.ends_with("[image resized]"))).all(|v|v.get("origin").is_none()));

    // Receipts are unchanged by correlation (text is not stage evidence).
    let receipts=shared.agent.lock().unwrap().receipts.clone();
    assert_eq!((receipts["d1"]["stage"].as_str(),receipts["d2"]["stage"].as_str()),(Some("admitted"),Some("queued")));
    dispose(&shared,&provider).await;

    // A restarted bridge has nothing pending: a message queued before it is delivered without origin.
    let root=tempfile::tempdir().unwrap();
    let (shared,provider,_core)=fixture_native(root.path(),&["/usr/bin/python3".into(),fake,"--user-messages".into()]).await;
    native(&shared,"test_inject",json!({"queue":"followUp","text":"from before a restart"})).await;native(&shared,"test_drain",json!({"queue":"followUp"})).await;
    assert!(last(&shared,"from before a restart").get("origin").is_none());
    dispose(&shared,&provider).await;
}

/// The version is the launched binary's own --version output, first line, bounded; failure leaves it absent.
#[tokio::test]
async fn startup_version_is_the_launched_binarys_own_output(){
    let root=tempfile::tempdir().unwrap();let (shared,provider,_core)=fixture(root.path(),true).await;
    startup_version(shared.clone(),vec!["/bin/echo".into(),"pi 9.9.9".into()],root.path().into(),HashMap::new()).await;
    let value=|shared:&Arc<Shared>|shared.agent.lock().unwrap().journal.entity("startup","current").map(|(_,v)|v.clone()).unwrap();
    assert_eq!(value(&shared)["runtime"],json!({"name":"Pi","version":"pi 9.9.9 --version","version_source":"cli:--version"}));
    startup_version(shared.clone(),vec!["/bin/false".into()],root.path().into(),HashMap::new()).await;
    assert!(value(&shared)["incomplete"].as_array().unwrap().iter().any(|f|f=="runtime"));
    dispose(&shared,&provider).await;
}

/// as-reviewer R1 on f50a03d3: a --version that floods stdout is read only up to the bound, then given up.
#[tokio::test]
async fn startup_version_reads_a_bounded_amount_of_output(){
    let root=tempfile::tempdir().unwrap();let (shared,provider,_core)=fixture(root.path(),true).await;
    let started=Instant::now();
    startup_version(shared.clone(),vec!["/bin/sh".into(),"-c".into(),"exec yes".into()],root.path().into(),HashMap::new()).await; // floods stdout
    assert!(started.elapsed()<Duration::from_secs(4),"stopped at the byte bound, not at the timeout");
    let value=shared.agent.lock().unwrap().journal.entity("startup","current").map(|(_,v)|v.clone()).unwrap();
    assert!(value["incomplete"].as_array().unwrap().iter().any(|f|f=="runtime"));
    assert!(value["runtime"].get("version").is_none());
    dispose(&shared,&provider).await;
}

/// lead-secondary's control on c237ee66: the --version child gets the native child's filtered environment, never the
/// bridge's bootstrap credential or Herdr control identity.
#[tokio::test]
async fn startup_version_child_gets_the_filtered_native_environment(){
    let root=tempfile::tempdir().unwrap();let (shared,provider,_core)=fixture(root.path(),true).await;
    let env:HashMap<String,String>=[("VQRO_BRIDGE_TOKEN","review-only-no-authority"),("HERDR_PANE_ID","p1"),("VQRO_SOCKET","/s.sock"),("PATH","/usr/bin:/bin")]
        .into_iter().map(|(k,v)|(k.to_string(),v.to_string())).collect();
    startup_version(shared.clone(),vec!["/bin/sh".into(),"-c".into(),"echo \"${VQRO_BRIDGE_TOKEN:-none}/${HERDR_PANE_ID:-none}/${VQRO_SOCKET:-none}\"".into()],root.path().into(),env).await;
    let value=shared.agent.lock().unwrap().journal.entity("startup","current").map(|(_,v)|v.clone()).unwrap();
    assert_eq!(value["runtime"]["version"],"none/none//s.sock","{value}");
    dispose(&shared,&provider).await;
}

/// Customization slice 1 (human Q1 b): Pi launches with its skills, extensions and prompt templates; explicit
/// interceptor/template/print argv injection is still refused.
#[test]
fn controlled_launch_loads_skills_extensions_and_templates(){
    let argv=controlled(vec!["pi".into()]).unwrap();
    assert_eq!(argv,["pi","--mode","rpc"]);
    assert!(!argv.iter().any(|arg|arg.starts_with("--no-")));
    for forbidden in ["-e","--extension=x","--prompt-template=y","-p","--print"] {assert!(controlled(vec!["pi".into(),forbidden.into()]).is_err(),"{forbidden}");}
}

/// While extensions are loaded, every receipt says the input may have been changed, by items Pi does not name.
#[tokio::test]
async fn receipts_say_input_may_be_transformed_by_unknown_extensions(){
    let root=tempfile::tempdir().unwrap();
    let fake=format!("{}/tests/fixtures/fake_pi.py",env!("CARGO_MANIFEST_DIR"));
    let (shared,provider,mut core)=fixture_native(root.path(),&["/usr/bin/python3".into(),fake]).await;
    shared.native.get().unwrap().call("get_state",json!({}),Duration::from_secs(2)).await.unwrap();
    let Outcome::Accepted(Some(value))=send(&shared,&provider,&mut core,1,"settle","hello").await else{panic!("not accepted")};
    assert_eq!(value["receipt"]["may_transform"],"unknown");
    assert_eq!(shared.agent.lock().unwrap().receipts["d1"]["may_transform"],"unknown");
    dispose(&shared,&provider).await;
}

/// lead-secondary on 124de297: argv can never prove input_transform none (a wrapper may pass anything on; Pi's own
/// parsing decides, e.g. a value consumed by --model, or text after --).
#[test]
fn launch_argv_never_proves_no_input_transforms(){
    for argv in [vec!["pi","--no-extensions","--no-prompt-templates"],vec!["pi","-ne","-np"],vec!["pi","--model","--no-extensions","-np"],
                 vec!["pi","--","--no-extensions","--no-prompt-templates"],vec!["pi1","--no-extensions=true","--no-prompt-templates"]] {
        let argv:Vec<String>=argv.into_iter().map(str::to_string).collect();
        assert!(launch_may_transform(&argv),"{argv:?}");
    }
}

/// lead-secondary on 0cba2157: a command a Pi extension registers after start-up (never in get_commands) cannot slip
/// through: with extensions loaded the slash namespace is unknown, so a send of "/late" is refused before any native
/// write, also after an epoch change; agent.command stays the explicit route.
#[tokio::test]
async fn a_late_registered_command_name_is_refused_while_extensions_are_loaded(){
    let root=tempfile::tempdir().unwrap();
    let fake=format!("{}/tests/fixtures/fake_pi.py",env!("CARGO_MANIFEST_DIR"));
    let (shared,provider,mut core)=fixture_native(root.path(),&["/usr/bin/python3".into(),fake]).await;
    shared.native.get().unwrap().call("get_state",json!({}),Duration::from_secs(2)).await.unwrap();
    shared.native.get().unwrap().call("get_commands",json!({}),Duration::from_secs(2)).await.unwrap();
    assert!(shared.agent.lock().unwrap().commands.slash_names().is_some_and(|names|!names.iter().any(|n|n=="late")),"/late is not advertised");
    // The proven owner's own plain text: the only text Pi could read as a leading /name (others are framed).
    async fn owner_send(shared:&Arc<Shared>,provider:&Provider,core:&mut Core,id:u64,mode:&str,text:&str)->Outcome{
        let wire=json!({"jsonrpc":"2.0","id":id,"method":"effect.invoke","params":{"generation":1,"effect":format!("o{id}"),"op":"agent.send","ref":"agent","incarnation":"","deadline_ms":2000,
            "principal":"person:owner","actor":"extension:agents","initiator":"person:owner",
            "args":{"v":0,"mode":mode,"delivery":format!("o{id}"),"origin":{"kind":"human"},"content":[{"type":"text","text":text}]}}});
        core.w.write_all(format!("{wire}\n").as_bytes()).await.unwrap();
        let Request::Effect(effect)=core.requests.as_mut().unwrap().recv().await.unwrap() else{panic!("not effect")};
        let owner=async{let get=core.recv().await;core.reply(&get,json!({"value":"person:owner"})).await;};
        let (outcome,())=tokio::join!(invoke(shared,provider,&effect),owner);let outcome=outcome.unwrap();
        effect.respond(Ok(outcome.clone()));let reply=core.recv().await;assert_eq!(reply["id"],id);outcome
    }
    let refused=|outcome:&Outcome|matches!(outcome,Outcome::Rejected{kind,message} if kind=="unsupported" && message.contains("may run as a Pi command"));
    let outcome=owner_send(&shared,&provider,&mut core,1,"start","/late").await;assert!(refused(&outcome),"{outcome:?}");
    shared.agent.lock().unwrap().new_session_epoch();
    let outcome=owner_send(&shared,&provider,&mut core,2,"settle","/late again").await;assert!(refused(&outcome),"{outcome:?}");
    assert!(shared.agent.lock().unwrap().receipts.is_empty(),"nothing reached Pi");
    dispose(&shared,&provider).await;
}
