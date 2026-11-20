//! Headless Pi agent-v0 provider. Backends own the bridge; this bridge owns one native child.
mod native;
mod admission;
mod contract;
mod state;
use std::collections::HashMap;
use std::sync::{Arc,Mutex,OnceLock};
use std::time::Duration;
use anyhow::{Context,Result};
use clap::Parser;
use serde_json::{json,Value};
use tokio::sync::Notify;
use tokio::time::Instant;
use vqro::provider_client::{Config,EffectRequest,Outcome,Provider,ProviderError,ProviderStream,Request};
use vqro::provider_client::items::{ItemCursor,Next};
use admission::{Admission,Starts};
use native::Native;
use state::Agent;

#[derive(Parser)]
struct Args {
    #[arg(long)] name:Option<String>,
    #[arg(long)] instance:Option<String>,
    #[arg(long)] cwd:Option<std::path::PathBuf>,
    #[arg(last=true,required=true)] native:Vec<String>,
}
struct Shared {agent:Mutex<Agent>,native:OnceLock<Native>,starts:OnceLock<Starts>,changed:Notify,cwd:String}
impl Shared {
    fn facts(&self,name:&str)->Value {self.agent.lock().unwrap().facts(name,&self.cwd)}
    fn wake(&self) {self.changed.notify_waiters();}
}
fn fresh()->String {let mut bytes=[0;16];getrandom::fill(&mut bytes).expect("fresh provider identity");bytes.iter().map(|b|format!("{b:02x}")).collect()}
fn rejected(kind:impl Into<String>,message:impl Into<String>)->Result<Outcome,ProviderError> {Ok(Outcome::Rejected {kind:kind.into(),message:message.into()})}
fn native_failure(error:native::Error)->Result<Outcome,ProviderError> {
    if error.may_have_acted {Err(ProviderError::new("backend",error.to_string()))}else{rejected(error.kind,error.message)}
}
fn controlled(mut argv:Vec<String>)->Result<Vec<String>> {
    anyhow::ensure!(!argv.is_empty(),"explicit native argv required");
    for arg in &argv[1..] {
        anyhow::ensure!(!(arg=="-e" || arg.starts_with("--extension") || arg.starts_with("--prompt-template") || arg=="-p" || arg=="--print"),"literal route forbids explicit interceptors/templates/print mode");
    }
    argv.extend(["--mode","rpc","--no-extensions","--no-skills","--no-prompt-templates"].map(str::to_string));
    Ok(argv)
}
#[tokio::main(flavor="current_thread")]
async fn main()->Result<()> {
    let args=Args::parse();
    let endpoint=std::env::var("VQRO_SOCKET").context("VQRO_SOCKET is required")?;
    let token=std::env::var("VQRO_BRIDGE_TOKEN").context("runtime bootstrap credential is required")?;
    let instance=args.instance.unwrap_or_else(fresh);let name=args.name.unwrap_or_else(||format!("pi-{instance}"));
    anyhow::ensure!(!instance.is_empty() && instance.len()<=64 && !name.is_empty() && name.len()<=128,"provider identity exceeds bounds");
    let argv=controlled(args.native)?;
    let cwd=args.cwd.unwrap_or(std::env::current_dir()?);
    let signals=admission::Signals::new();
    let shared=Arc::new(Shared {agent:Mutex::new(Agent::new(instance.clone(),signals.clone())),native:OnceLock::new(),starts:OnceLock::new(),changed:Notify::new(),cwd:cwd.to_string_lossy().into_owned()});
    let weak=Arc::downgrade(&shared);
    let native=Native::spawn(&argv,&cwd,&std::env::vars().collect::<HashMap<_,_>>(),Arc::new(move|event|{
        if let Some(shared)=weak.upgrade(){
            if let native::Event::ProtocolError(message)=&event {eprintln!("Pi native protocol: {message}");}
            let cancel=match &event {native::Event::Record(record) if record["type"]=="extension_ui_request" && record["id"].is_string()=>Some(record["id"].clone()),_=>None};
            let mut agent=shared.agent.lock().unwrap();agent.signals.observe(&event);agent.observe(&event);
            if cancel.is_some(){agent.incomplete.insert("requests".into());}
            drop(agent);shared.wake();
            if let (Some(id),Some(native))=(cancel,shared.native.get()){
                let native=native.clone();tokio::spawn(async move{if native.notify("extension_ui_response",json!({"id":id,"cancelled":true}),Instant::now()+Duration::from_secs(2)).await.is_err(){native.stop();}});
            }
        }
    })).await?;
    let _=shared.native.set(native.clone());let _=shared.starts.set(Starts::new(native.clone(),signals));
    let startup=native.call("get_state",json!({}),Duration::from_secs(5)).await;
    if !matches!(startup,Ok(ref reply) if reply.success && reply.data.is_object()) {
        native.stop();let _=tokio::time::timeout(Duration::from_secs(2),native.exited()).await;
        anyhow::bail!("native Pi did not report startup state");
    }
    // Real Pi uses a commands/models container, older fixtures may return the array directly.
    if let Ok(reply)=native.call("get_commands",json!({}),Duration::from_secs(3)).await {
        if !reply.success || contract::rows(&reply.data,"commands").is_none(){shared.agent.lock().unwrap().incomplete.insert("commands".into());}
    }
    let life=Arc::new(Notify::new());
    if std::env::var("VQRO_EXTENSION_LIFETIME").as_deref()==Ok("stdin") {
        let notify=life.clone();std::thread::spawn(move||{use std::io::Read;let mut buf=[0;4096];while matches!(std::io::stdin().read(&mut buf),Ok(n) if n>0){} notify.notify_one();});
    }
    let mut credential=token;let mut failures=0u32;
    while native.alive() {
        let mut config=Config::new(&name,&instance,shared.agent.lock().unwrap().provider_capabilities());
        config.role="runtime".into();config.token=Some(credential.clone());config.max_effect_ids=2048;config.max_item_streams=4;config.max_streams=1;config.call_timeout=Duration::from_secs(5);
        config.software=json!({"name":"vqro-bridge-pi","version":env!("CARGO_PKG_VERSION")});
        let connected=async {let socket=tokio::net::UnixStream::connect(&endpoint).await.map_err(|e|ProviderError::new("unavailable",format!("core transport: {e}")))?;Provider::connect(socket,config).await};
        let result=tokio::select! {result=connected=>result,_=life.notified()=>{native.stop();break;},_=native.exited()=>break};
        let mut healthy=None;
        match result {
            Ok((provider,mut requests))=>{
                // Remember acknowledged rotation before any later sync/view setup can fail.
                if let Some(token)=provider.resume_token(){credential=token.into();}
                match provider.full_sync(vec![shared.facts(&name)]).await {
                    Ok(())=>{
                        healthy=Some(Instant::now());
                        let publish=tokio::spawn(publish(shared.clone(),provider.clone(),name.clone()));
                        loop {
                            tokio::select! {
                                request=requests.recv()=>match request {
                                    Some(Request::Effect(effect))=>{let s=shared.clone();tokio::spawn(async move{let result=invoke(&s,&effect).await;effect.respond(result);});}
                                    Some(Request::Open(open))=>{
                                        if open.stream.r#ref!="agent" || open.stream.incarnation!="" || open.items.is_none(){open.respond(Err(ProviderError::new("unsupported","one agent item channel only")));continue;}
                                        let cursor=shared.agent.lock().unwrap().journal.open(open.items.as_ref().and_then(|items|items.get("after")));
                                        if let Some(stream)=open.respond_items(cursor.reset(),cursor.epoch()){tokio::spawn(viewer(shared.clone(),stream,cursor));}
                                    }
                                    None=>break,
                                },
                                _=native.exited()=>{
                                    let _=provider.full_sync(vec![shared.facts(&name)]).await;
                                    // Let already-dispatched effect responders finish; no new native writes.
                                    tokio::time::sleep(Duration::from_millis(100)).await;break;
                                }
                                _=life.notified()=>{native.stop();let _=tokio::time::timeout(Duration::from_secs(2),native.exited()).await;let _=provider.full_sync(vec![shared.facts(&name)]).await;break;}
                            }
                        }
                        publish.abort();
                    }
                    Err(error)=>{if definitive(&error){native.stop();anyhow::bail!("registration setup refused: {error}");}}
                }
                provider.close();
            }
            Err(error)=>{
                if definitive(&error){native.stop();let _=tokio::time::timeout(Duration::from_secs(2),native.exited()).await;anyhow::bail!("runtime registration refused: {error}");}
                eprintln!("Pi bridge core unavailable; no input is replayed ({})",error.kind);
            }
        }
        if healthy.is_some_and(|start:Instant|start.elapsed()>=Duration::from_secs(30)){failures=0;}
        failures=failures.saturating_add(1);
        let ceiling=(0.25*2f64.powi(failures.saturating_sub(1).min(7) as i32)).min(30.0);
        let mut bytes=[0u8;8];getrandom::fill(&mut bytes).ok();let fraction=u64::from_ne_bytes(bytes) as f64/u64::MAX as f64;
        tokio::select! {_=tokio::time::sleep(Duration::from_secs_f64(ceiling*(0.5+fraction*0.5)))=>{},_=life.notified()=>{native.stop();break;},_=native.exited()=>break}
    }
    native.stop();let _=tokio::time::timeout(Duration::from_secs(2),native.exited()).await;
    let (tail,total)=native.stderr_tail();eprintln!("Pi bridge ended; native pending {}; stderr retained {} of {total} bytes",native.pending(),tail.len());
    Ok(())
}
fn definitive(error:&ProviderError)->bool {
    if error.remote && ((error.kind=="forbidden" && error.data["reason"]=="safe_mode") || error.kind=="durability_unconfirmed"){return false;}
    error.remote || !matches!(error.kind.as_str(),"unavailable"|"timeout")
}
async fn publish(shared:Arc<Shared>,provider:Provider,name:String) {
    let mut signals=shared.agent.lock().unwrap().signals.changes();let mut last=shared.facts(&name);
    loop {
        let wake=shared.changed.notified();tokio::pin!(wake);wake.as_mut().enable();
        tokio::select! {_=wake=>{},result=signals.changed()=>{if result.is_err(){break;}},_=provider.closed()=>break}
        let facts={let mut agent=shared.agent.lock().unwrap();agent.refresh();agent.facts(&name,&shared.cwd)};
        if facts!=last {if provider.upsert(facts.clone()).await.is_err(){break;}last=facts;}
    }
}
async fn viewer(shared:Arc<Shared>,mut stream:ProviderStream,mut cursor:ItemCursor) {
    loop {
        let wake=shared.changed.notified();tokio::pin!(wake);wake.as_mut().enable();
        let next={let agent=shared.agent.lock().unwrap();cursor.next(&agent.journal,65536)};
        match next {
            Next::Data(bytes)=>if stream.send(&bytes).await.is_err(){break;},
            Next::Gap(reason)=>{let _=stream.gap(reason).await;break;},
            Next::Idle=>tokio::select! {_=wake=>{},_=stream.next()=>break},
        }
    }
}
async fn invoke(shared:&Arc<Shared>,effect:&EffectRequest)->Result<Outcome,ProviderError> {
    if effect.r#ref.as_deref()!=Some("agent") || !effect.incarnation.is_empty() {return rejected("not_found","unknown endpoint incarnation");}
    let args=&effect.args;
    {
        let agent=shared.agent.lock().unwrap();let current=agent.signals.current();
        let context=contract::Context {caps_rev:agent.caps()["rev"].as_u64().unwrap(),epoch:agent.epoch(),session:current.metadata["sessionId"].as_str(),owner_proven:false,
            images:agent.caps()["agent.send"]["content"]["types"].as_array().is_some_and(|types|types.iter().any(|kind|kind=="image")),literal:true};
        if let Err(error)=contract::common(args,&context){return rejected(error.kind,error.message);}
    }
    match effect.op.as_str() {
        "agent.receipt"=>{
            let delivery=match contract::id(&args["delivery"]){Ok(delivery)=>delivery,Err(error)=>return rejected(error.kind,error.message)};
            let agent=shared.agent.lock().unwrap();Ok(Outcome::Observed(Some(agent.receipts.get(delivery).cloned().unwrap_or_else(||json!({"stage":"unknown"})))))
        }
        "agent.send"=>send(shared,args,effect.deadline).await,
        "agent.interrupt"=>{
            if shared.agent.lock().unwrap().signals.current().idle(){return Ok(Outcome::Observed(Some(json!({"noop":true}))));}
            let result=shared.native.get().unwrap().call("abort",json!({}),effect.deadline.saturating_duration_since(Instant::now())).await;
            match result {Ok(reply) if reply.success=>Ok(Outcome::Accepted(None)),Ok(reply)=>Err(ProviderError::new("backend",native_message(&reply))),Err(error)=>native_failure(error)}
        }
        "agent.stop"=>{
            let Some(covers)=args["covers"].as_array() else{return rejected("invalid","covers array required");};
            if covers.iter().any(|cover|!matches!(cover.as_str(),Some("foreground"|"queued"))){return rejected("unsupported","stop does not cover that work");}
            if effect.deadline<=Instant::now(){return rejected("timeout","deadline before stop dispatch");}
            let native=shared.native.get().unwrap();native.stop();
            match tokio::time::timeout_at(effect.deadline,native.exited()).await {Ok(exit)=>Ok(Outcome::Observed(Some(json!({"exit":{"code":exit.code},"pending":"unknown"})))),Err(_)=>Ok(Outcome::Dispatched(None))}
        }
        _=>rejected("unsupported","undeclared agent operation"),
    }
}
fn native_message(reply:&native::Reply)->String {reply.error.as_str().unwrap_or("native operation failed; outcome unknown").chars().take(1024).collect()}
fn record_receipt(shared:&Shared,delivery:&str,value:Value){shared.agent.lock().unwrap().receipt(delivery,value);shared.wake();}
async fn send(shared:&Arc<Shared>,args:&Value,deadline:Instant)->Result<Outcome,ProviderError> {
    let prepared={
        let agent=shared.agent.lock().unwrap();let current=agent.signals.current();
        if agent.caps()["agent.send"]["modes"].get(args["mode"].as_str().unwrap_or("")).is_none(){return rejected("unsupported","mode not in current capabilities");}
        let context=contract::Context {caps_rev:agent.caps()["rev"].as_u64().unwrap(),epoch:agent.epoch(),session:current.metadata["sessionId"].as_str(),owner_proven:false,
            images:agent.caps()["agent.send"]["content"]["types"].as_array().is_some_and(|types|types.iter().any(|kind|kind=="image")),literal:true};
        let prepared=match contract::send(args,&context){Ok(prepared)=>prepared,Err(error)=>return rejected(error.kind,error.message)};
        if agent.receipts.contains_key(&prepared.delivery){return rejected("conflict","delivery already recorded; lookup is not a native insertion key");}
        prepared
    };
    let delivery=prepared.delivery;let native_args=prepared.args;let command=prepared.command;let mode=prepared.mode;
    let base=json!({"delivery":delivery,"origin":prepared.origin,"quoted":prepared.quoted,"rendering":prepared.rendering,"cause":prepared.cause,"correlation":prepared.correlation});
    let weak=Arc::downgrade(shared);let d=delivery.clone();let metadata=base.clone();
    let receipt:admission::Receipt=Arc::new(move|id,result|{
        if let Some(shared)=weak.upgrade(){let mut value=metadata.clone();value["native"]=json!({"request":id});
            value["stage"]=json!(match &result {Ok(reply) if reply.success=>"admitted",Err(error) if !error.may_have_acted=>"rejected",_=>"unknown"});
            record_receipt(&shared,&d,value);
        }
    });
    let result=match mode.as_str() {
        "start"=>shared.starts.get().unwrap().start(native_args,deadline,receipt).await,
        "admit"=>shared.starts.get().unwrap().admit(native_args,deadline,receipt).await,
        _=>direct(shared.native.get().unwrap(),command,native_args,deadline,receipt).await,
    };
    match result {
        Admission::Acknowledged {native,reply}=>{
            if !reply.success{return Err(ProviderError::new("backend",native_message(&reply)));}
            let mut value=base;value["stage"]=json!("admitted");value["native"]=json!({"request":native});Ok(Outcome::Accepted(Some(json!({"receipt":value}))))
        }
        Admission::Dispatched {native}=>{let mut value=base;value["stage"]=json!("dispatched");value["native"]=json!({"request":native});record_receipt(shared,&delivery,value.clone());Ok(Outcome::Dispatched(Some(json!({"receipt":value}))))}
        Admission::Refused(error)=>rejected(error.kind,error.message),Admission::Unknown(error)=>native_failure(error),
    }
}
async fn direct(native:&Native,command:&str,args:Value,deadline:Instant,receipt:admission::Receipt)->Admission {
    let mut ticket=match native.submit(command,args,deadline){Ok(ticket)=>ticket,Err(error)=>return if error.may_have_acted{Admission::Unknown(error)}else{Admission::Refused(admission::Refusal {kind:error.kind,message:error.message})}};
    let id=ticket.id.clone();
    let result=tokio::select! {result=ticket.reply()=>Some(result),_=tokio::time::sleep_until(deadline)=>None};
    if let Some(result)=result {receipt(id.clone(),result.clone());return match result{Ok(reply)=>Admission::Acknowledged {native:id,reply},Err(error)=>Admission::Unknown(error)};}
    if ticket.cancel_before_write(){return Admission::Refused(admission::Refusal {kind:"timeout",message:"deadline before native dispatch".into()});}
    let written=ticket.written();
    tokio::spawn(async move{let result=ticket.reply().await;receipt(ticket.id.clone(),result);});
    if written {Admission::Dispatched {native:id}}else{Admission::Unknown(native::Error {kind:"timeout",message:"native write incomplete at deadline; outcome unknown".into(),may_have_acted:true})}
}
