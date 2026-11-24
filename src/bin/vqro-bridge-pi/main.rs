//! Headless Pi agent-v0 provider. Backends own the bridge; this bridge owns one native child.
mod native;
mod admission;
mod contract;
mod state;
mod retirement;
mod queue;
mod requests;
mod configuration;
mod commands;
mod panel;
mod views;
mod origins;
mod startup;
mod testing;
mod account_alias;
#[cfg(test)]
mod error_episode_tests;
#[path="../launcher_account/mod.rs"]
mod launcher_account;
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
    #[arg(long)] testing_plugin:Option<std::path::PathBuf>,
    #[arg(long)] account_extension:Option<std::path::PathBuf>,
    #[arg(last=true,required=true)] native:Vec<String>,
}
struct Shared {agent:Mutex<Agent>,fallback:panel::Fallback,connected:std::sync::atomic::AtomicBool,native:OnceLock<Native>,starts:OnceLock<Starts>,changed:Notify,metadata_refresh:Notify,cwd:String,provider_name:String,instance:String,ui_slots:Arc<tokio::sync::Semaphore>}
impl Shared {
    fn facts(&self,name:&str)->Value {self.agent.lock().unwrap().facts(name,&self.cwd)}
    fn wake(&self) {self.changed.notify_waiters();}
}
fn agent_tool_principal(provided:&str,owner:&Value)->bool{
    if provided.is_empty() || provided=="undeclared"{return false;}
    match owner.get("value"){Some(Value::Null)=>provided!="owner",Some(Value::String(owner))=>provided!=owner,_=>false}
}
fn fresh()->String {let mut bytes=[0;16];getrandom::fill(&mut bytes).expect("fresh provider identity");bytes.iter().map(|b|format!("{b:02x}")).collect()}
fn rejected(kind:impl Into<String>,message:impl Into<String>)->Result<Outcome,ProviderError> {Ok(Outcome::Rejected {kind:kind.into(),message:message.into()})}
fn native_failure(error:native::Error)->Result<Outcome,ProviderError> {
    if error.may_have_acted {Err(ProviderError::new("backend",error.to_string()))}else{rejected(error.kind,error.message)}
}
/// Run the launched binary once with only `--version` (bounded time and output); never delays readiness. A failure
/// leaves the version absent and `runtime` incomplete.
async fn startup_version(shared:Arc<Shared>,argv:Vec<String>,cwd:std::path::PathBuf,env:HashMap<String,String>){
    let Some((program,rest))=argv.split_first() else {return;};
    use tokio::io::AsyncReadExt;
    let mut command=vqro::program::background_command(program);
    native::native_env(&mut command,&env); // same filtering as the RPC child: no bootstrap credential, no Herdr identity
    command.args(rest).arg("--version").current_dir(&cwd).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null()).kill_on_drop(true);
    // Read at most 4 KiB + 1 while reading (never buffer an unbounded stream), all within 5 s; then reap the child.
    let read=async {
        let mut child=command.spawn().ok()?;
        let mut stdout=child.stdout.take()?;let mut buffer=Vec::new();
        (&mut stdout).take(4097).read_to_end(&mut buffer).await.ok()?;
        if buffer.len()>4096 {let _=child.kill().await;return None;}
        let status=child.wait().await.ok()?;
        status.success().then_some(buffer)
    };
    let output=tokio::time::timeout(Duration::from_secs(5),read).await.ok().flatten();
    let mut agent=shared.agent.lock().unwrap();
    match output {
        Some(out)=>agent.startup_version(&String::from_utf8_lossy(&out)),
        None=>agent.startup_failed(&["runtime"]),
    }
    drop(agent);shared.wake();
}
/// Whether Pi may run input expanders/transformers for this launch. Always true: only Pi's own parsing (of argv that may
/// pass through a wrapper such as a `pi1` script) decides whether `--no-extensions` / `--no-prompt-templates` took
/// effect, and Pi does not report it over RPC, so the argv can never prove `input_transform: none`.
fn launch_may_transform(_argv:&[String])->bool {true}
fn controlled(mut argv:Vec<String>)->Result<Vec<String>> {
    anyhow::ensure!(!argv.is_empty(),"explicit native argv required");
    for arg in &argv[1..] {
        anyhow::ensure!(!(arg=="-e" || arg.starts_with("--extension") || arg.starts_with("--prompt-template") || arg=="-p" || arg=="--print"),"literal route forbids explicit interceptors/templates/print mode");
    }
    // Pi loads its skills, extensions and prompt templates (customization slice 1, human Q1 b). Literal sends are
    // protected in contract::send (no leading /name Pi would expand or run) and labelled (literal none,
    // input_transform may, receipt may_transform); explicit interceptor/template/print argv stays refused above.
    argv.extend(["--mode","rpc"].map(str::to_string));
    Ok(argv)
}
#[tokio::main(flavor="current_thread")]
async fn main()->Result<()> {
    let args=Args::parse();
    let endpoint=std::env::var("VQRO_SOCKET").context("VQRO_SOCKET is required")?;
    let token=std::env::var("VQRO_BRIDGE_TOKEN").context("runtime bootstrap credential is required")?;
    let instance=args.instance.unwrap_or_else(fresh);let name=args.name.unwrap_or_else(||format!("pi-{instance}"));
    anyhow::ensure!(!instance.is_empty() && instance.len()<=64 && !name.is_empty() && name.len()<=128,"provider identity exceeds bounds");
    // The runtime's own --version from the exact binary the launcher starts (agent-v0 startup, version_source cli).
    let version_argv=args.native.clone();
    let expanders_on=launch_may_transform(&args.native);
    let mut argv=controlled(args.native)?;
    let alias_metadata=std::env::var("VQRO_PI_ACCOUNT").ok();
    let expected_alias=account_alias::attach(&mut argv,args.account_extension.as_deref(),alias_metadata.as_deref())?;
    let testing_plugin = match args.testing_plugin {
        Some(path) => {
            let admitted = vqro::paths::extensions_dir().and_then(|root|
                testing::Plugin::validate(&path, &root, std::env::var("VQRO_PRINCIPAL").ok()));
            match admitted {
                Ok(plugin) => { plugin.append(&mut argv); Some(plugin) }
                Err(error) => { eprintln!("NOT VERIFIED: testing plugin refused: {error}; launching without testing hooks"); None }
            }
        }
        None => None,
    };
    let cwd=args.cwd.unwrap_or(std::env::current_dir()?).canonicalize()?;
    let signals=admission::Signals::new();
    let account_raw=std::env::var("VQRO_ACCOUNT").ok();
    let mut agent=Agent::with_cwd(instance.clone(),signals.clone(),cwd.to_string_lossy().into_owned());
    agent.account=launcher_account::parse(account_raw.as_deref()).map_err(anyhow::Error::msg)?;
    agent.testing_plugin=testing_plugin;
    let shared=Arc::new(Shared {agent:Mutex::new(agent),fallback:panel::Fallback::tty(),connected:std::sync::atomic::AtomicBool::new(false),native:OnceLock::new(),starts:OnceLock::new(),changed:Notify::new(),metadata_refresh:Notify::new(),cwd:cwd.to_string_lossy().into_owned(),provider_name:name.clone(),instance:instance.clone(),ui_slots:Arc::new(tokio::sync::Semaphore::new(24))});
    shared.agent.lock().unwrap().expanders_on=expanders_on;
    let weak=Arc::downgrade(&shared);
    let native=Native::spawn(&argv,&cwd,&std::env::vars().collect::<HashMap<_,_>>(),Arc::new(move|event|{
        if let Some(shared)=weak.upgrade(){
            if let native::Event::ProtocolError(message)=&event {eprintln!("Pi native protocol: {message}");}
            let mut agent=shared.agent.lock().unwrap();agent.signals.observe(&event);agent.observe(&event);
            drop(agent);shared.wake();drive_ui_cancels(&shared);
            if matches!(&event,native::Event::Record(record) if matches!(record["type"].as_str(),Some("agent_end"|"agent_settled"|"auto_compaction_end")) || (record["type"]=="response" && record["command"]=="get_state" && record["success"]==true)){shared.metadata_refresh.notify_one();}
        }
    })).await?;
    let _=shared.native.set(native.clone());let _=shared.starts.set(Starts::new(native.clone(),signals));
    if shared.fallback.enabled(){tokio::spawn(monitor(shared.clone()));}
    let startup=native.call("get_state",json!({}),Duration::from_secs(5)).await;
    if !matches!(startup,Ok(ref reply) if reply.success && reply.data.is_object()
        && account_alias::ready(expected_alias.as_deref(),&reply.data)) {
        native.stop();let _=tokio::time::timeout(Duration::from_secs(2),native.exited()).await;
        anyhow::bail!("native Pi did not confirm startup/account binding; no agent endpoint registered");
    }
    // Real Pi uses a commands/models container, older fixtures may return the array directly.
    match native.call("get_commands",json!({}),Duration::from_secs(3)).await {
        Ok(reply)=>{if !reply.success || contract::rows(&reply.data,"commands").is_none(){shared.agent.lock().unwrap().incomplete.insert("commands".into());}}
        // No reply at all (timeout, protocol): the startup inventories are unknown, named incomplete.
        Err(_)=>shared.agent.lock().unwrap().startup_failed(&["skills","prompts","commands"]),
    }
    tokio::spawn(startup_version(shared.clone(),version_argv,cwd.clone(),std::env::vars().collect()));
    tokio::spawn(refresh_usage(shared.clone(),native.clone()));
    shared.metadata_refresh.notify_one();
    let (metadata_native,metadata_shared)=(native.clone(),shared.clone());
    tokio::spawn(async move{for command in ["get_available_models","get_available_thinking_levels"]{
        if !matches!(metadata_native.call(command,json!({}),Duration::from_secs(3)).await,Ok(reply) if reply.success){metadata_shared.agent.lock().unwrap().incomplete.insert("config".into());metadata_shared.wake();}
    }});
    let life=Arc::new(Notify::new());
    if std::env::var("VQRO_EXTENSION_LIFETIME").as_deref()==Ok("stdin") {
        let notify=life.clone();std::thread::spawn(move||{use std::io::Read;let mut buf=[0;4096];while matches!(std::io::stdin().read(&mut buf),Ok(n) if n>0){} notify.notify_one();});
    }
    let mut credential=token;let mut failures=0u32;let mut registrations=0u64;
    while native.alive() {
        let mut config=Config::new(&name,&instance,shared.agent.lock().unwrap().provider_capabilities());
        config.role="runtime".into();config.token=Some(credential.clone());config.max_effect_ids=2048;config.max_item_streams=4;config.max_streams=1;config.call_timeout=Duration::from_secs(5);
        config.software=json!({"name":"vqro-bridge-pi","version":env!("CARGO_PKG_VERSION")});
        let connected=async {let socket=vqro::transport::connect(&endpoint).await.map_err(|e|ProviderError::new("unavailable",format!("core transport: {e}")))?;Provider::connect(socket,config).await};
        let result=tokio::select! {result=connected=>result,_=life.notified()=>{native.stop();break;},_=native.exited()=>break};
        let mut healthy=None;
        match result {
            Ok((provider,mut requests))=>{
                // Remember acknowledged rotation before any later sync/view setup can fail.
                if let Some(token)=provider.resume_token(){credential=token.into();}
                if let Ok(principal)=std::env::var("VQRO_PRINCIPAL"){
                    let evidenced=match provider.call("workspace.get",json!({"key":"doc/principals/owner"})).await{Ok(owner)=>agent_tool_principal(&principal,&owner),Err(_)=>false};
                    let mut agent=shared.agent.lock().unwrap();agent.tool_principal_env=evidenced;agent.refresh();
                }
                if registrations>0{shared.agent.lock().unwrap().cancel_requests("reconnect_not_verifiable");drive_ui_cancels(&shared);}
                registrations+=1;
                let setup=async{
                    {let mut agent=shared.agent.lock().unwrap();agent.view=None;agent.view_error=None;}
                    provider.full_sync(vec![shared.facts(&name)]).await?;
                    let view=views::claim(&provider,&name,&instance).await;
                    {let mut agent=shared.agent.lock().unwrap();agent.view=view.view;agent.view_error=view.error;}
                    provider.upsert(shared.facts(&name)).await
                }.await;
                match setup {
                    Ok(())=>{
                        healthy=Some(Instant::now());shared.connected.store(true,std::sync::atomic::Ordering::Relaxed);shared.wake();
                        let publish=tokio::spawn(publish(shared.clone(),provider.clone(),name.clone()));
                        loop {
                            tokio::select! {
                                request=requests.recv()=>match request {
                                    Some(Request::Effect(effect))=>{let s=shared.clone();let p=provider.clone();tokio::spawn(async move{let result=invoke(&s,&p,&effect).await;effect.respond(result);});}
                                    Some(Request::Open(open))=>{
                                        if open.stream.r#ref!="agent" || open.stream.incarnation!="" || open.items.is_none(){open.respond(Err(ProviderError::new("unsupported","one agent item channel only")));continue;}
                                        let cursor=shared.agent.lock().unwrap().journal.open(open.items.as_ref().and_then(|items|items.get("after")));
                                        if let Some(stream)=open.respond_items(cursor.reset(),cursor.epoch()){tokio::spawn(viewer(shared.clone(),stream,cursor));}
                                    }
                                    None=>break,
                                },
                                _=native.exited()=>{
                                    announce_end(&shared,&provider,&name).await;
                                    break;
                                }
                                _=life.notified()=>{native.stop();if tokio::time::timeout(Duration::from_secs(2),native.exited()).await.is_ok(){announce_end(&shared,&provider,&name).await;}else{eprintln!("Pi stop pending; no native end evidence yet");}break;}
                            }
                        }
                        publish.abort();
                    }
                    Err(error)=>{if definitive(&error){native.stop();anyhow::bail!("registration setup refused: {error}");}}
                }
                shared.connected.store(false,std::sync::atomic::Ordering::Relaxed);shared.wake();provider.close();
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
// Coalesced read-only observations: one bounded call at a time; no input, retry loop or native lifetime ownership.
async fn refresh_usage(shared:Arc<Shared>,native:Native){
    loop{
        tokio::select!{_=native.exited()=>break,_=shared.metadata_refresh.notified()=>{}}
        let (epoch,revision)={let agent=shared.agent.lock().unwrap();(agent.epoch().to_owned(),agent.usage_revision())};
        let reply=native.call("get_session_stats",json!({}),Duration::from_secs(3)).await;
        let data=reply.as_ref().ok().filter(|reply|reply.success).map(|reply|&reply.data);
        shared.agent.lock().unwrap().usage_result(&epoch,revision,data);shared.wake();
    }
}
async fn announce_end(shared:&Shared,provider:&Provider,name:&str) {
    let view={let agent=shared.agent.lock().unwrap();agent.view.as_ref().map(|view|(view.clone(),agent.transcript()))};
    if let Some((view,content))=view{if !matches!(tokio::time::timeout(Duration::from_secs(1),provider.call("view.publish",json!({"view":view,"content":content}))).await,Ok(Ok(_))){eprintln!("Pi ended; final transcript publication unconfirmed");}}
    match tokio::time::timeout(Duration::from_secs(5),retirement::retire(provider,shared.facts(name),name)).await {
        Ok(Ok(()))=>{},Ok(Err(error))=>eprintln!("Pi ended; end evidence not confirmed: {error}"),
        Err(_)=>eprintln!("Pi ended; end evidence not confirmed before core deadline"),
    }
}
fn definitive(error:&ProviderError)->bool {
    if error.remote && ((error.kind=="forbidden" && error.data["reason"]=="safe_mode") || error.kind=="durability_unconfirmed"){return false;}
    error.remote || !matches!(error.kind.as_str(),"unavailable"|"timeout")
}
async fn monitor(shared:Arc<Shared>){
    let mut signals=shared.agent.lock().unwrap().signals.changes();
    loop{
        let wake=shared.changed.notified();tokio::pin!(wake);wake.as_mut().enable();let _=signals.borrow_and_update();
        let (mut facts,content)={let agent=shared.agent.lock().unwrap();(agent.facts(&shared.provider_name,&shared.cwd),agent.transcript())};
        facts["_core_connected"]=json!(shared.connected.load(std::sync::atomic::Ordering::Relaxed));shared.fallback.show(&facts,&content);
        if !shared.native.get().unwrap().alive(){break;}
        tokio::select!{_=wake=>{},result=signals.changed()=>{if result.is_err(){break;}},_=shared.native.get().unwrap().exited()=>{}}
    }
}
async fn publish(shared:Arc<Shared>,provider:Provider,name:String) {
    let mut signals=shared.agent.lock().unwrap().signals.changes();let mut last=shared.facts(&name);let mut last_content=None;
    let mut retries=0u32;let mut retry_at=Instant::now()+Duration::from_millis(250);
    loop {
        let wake=shared.changed.notified();tokio::pin!(wake);wake.as_mut().enable();
        let _=signals.borrow_and_update();
        if retries<3 && Instant::now()>=retry_at && shared.agent.lock().unwrap().view.is_none(){
            retries+=1;retry_at=Instant::now()+Duration::from_millis(250u64<<retries);
            let setup=views::claim(&provider,&name,&shared.instance).await;
            let mut agent=shared.agent.lock().unwrap();agent.view=setup.view;agent.view_error=setup.error;last_content=None;drop(agent);shared.wake();
        }
        let (facts,view,content)={let mut agent=shared.agent.lock().unwrap();agent.refresh();(agent.facts(&name,&shared.cwd),agent.view.clone(),agent.transcript())};
        let needs_view=view.is_none();
        if facts!=last {if provider.upsert(facts.clone()).await.is_err(){break;}last=facts;}
        if let Some(view)=view{if last_content.as_ref()!=Some(&content){
            match provider.call("view.publish",json!({"view":view,"content":content})).await{
                Ok(_)=>last_content=Some(content),Err(error)=>{let mut agent=shared.agent.lock().unwrap();agent.view=None;agent.view_error=Some(format!("Transcript view unavailable ({}); use its backend terminal.",error.kind.chars().take(64).collect::<String>()));retry_at=Instant::now()+Duration::from_millis(250u64<<retries);drop(agent);shared.wake();}
            }
        }}
        tokio::select! {_=wake=>{},result=signals.changed()=>{if result.is_err(){break;}},_=provider.closed()=>break,_=tokio::time::sleep_until(retry_at),if needs_view && retries<3=>{}}
    }
}
async fn viewer(shared:Arc<Shared>,mut stream:ProviderStream,mut cursor:ItemCursor) {
    loop {
        let wake=shared.changed.notified();tokio::pin!(wake);wake.as_mut().enable();
        let next={let mut agent=shared.agent.lock().unwrap();cursor.next(&mut agent.journal,65536)};
        match next {
            Next::Data(bytes)=>if stream.send(&bytes).await.is_err(){break;},
            Next::Gap(reason)=>{let _=stream.gap(reason).await;break;},
            Next::Idle=>tokio::select! {_=wake=>{},_=stream.next()=>break},
        }
    }
}
async fn invoke(shared:&Arc<Shared>,provider:&Provider,effect:&EffectRequest)->Result<Outcome,ProviderError> {
    if effect.r#ref.as_deref()!=Some("agent") || !effect.incarnation.is_empty() {return rejected("not_found","unknown endpoint incarnation");}
    // C2 lifts root/args causal links into canonical SDK metadata while leaving args unchanged.
    // Reuse its public annotation mechanism so root-only links reach receipts without invention.
    let mut args=effect.args.clone();effect.attribution.causality.annotate(&mut args);let args=&args;
    {
        let agent=shared.agent.lock().unwrap();let current=agent.signals.current();
        let context=contract::Context {caps_rev:agent.caps()["rev"].as_u64().unwrap(),epoch:agent.epoch(),session:if agent.session_pending{None}else{current.metadata["sessionId"].as_str()},owner_proven:false,
            images:agent.caps()["agent.send"]["content"]["types"].as_array().is_some_and(|types|types.iter().any(|kind|kind=="image")),literal:true,slash_names:None};
        if let Err(error)=contract::common(args,&context){return rejected(error.kind,error.message);}
    }
    match effect.op.as_str() {
        "agent.receipt"=>{
            let delivery=match contract::id(&args["delivery"]){Ok(delivery)=>delivery,Err(error)=>return rejected(error.kind,error.message)};
            let agent=shared.agent.lock().unwrap();Ok(Outcome::Observed(Some(agent.receipts.get(delivery).cloned().unwrap_or_else(||json!({"stage":"unknown"})))))
        }
        "agent.send"=>{
            let bound=match contract::bind_origin(args,&effect.attribution){Ok(bound)=>bound,Err(error)=>return rejected(error.kind,error.message)};
            // A caller's human claim is insufficient. Resolve the current owner through the public
            // core primitive; absent/invalid/unavailable ownership stays conservatively attributed.
            let owner_proven=match tokio::time::timeout_at(effect.deadline,provider.call("workspace.get",json!({"key":vqro::principal::OWNER_KEY}))).await {
                Ok(Ok(owner))=>contract::recorded_owner(&effect.attribution,&owner),_=>false,
            };
            send(shared,&bound,effect.deadline,owner_proven).await
        },
        "agent.queue"=>queue_op(shared,args,effect.deadline).await,
        "agent.respond"=>respond(shared,args,effect.deadline).await,
        "agent.configure"=>configure(shared,provider,args,effect.deadline).await,
        "agent.session"=>session_new(shared,provider,args,effect.deadline).await,
        "agent.command"=>command(shared,provider,args,effect.deadline).await,
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
            match tokio::time::timeout_at(effect.deadline,native.exited()).await {Ok(exit)=>Ok(Outcome::Observed(Some(stop_report(covers,Some(&exit))))),Err(_)=>Ok(Outcome::Dispatched(Some(stop_report(covers,None))))}
        }
        _=>rejected("unsupported","undeclared agent operation"),
    }
}
fn stop_report(covers:&[Value],exit:Option<&native::Exit>)->Value {json!({"covers":covers,"covers_evidence":"requested","exit":exit.map(|exit|json!({"code":exit.code})),"pending":"unknown"})}
fn native_message(reply:&native::Reply)->String {reply.error.as_str().unwrap_or("native operation failed; outcome unknown").chars().take(1024).collect()}
/// Pi's prompt ACK carries `data.disposition` (`started | queued | handled`, rpc-mode.js prompt case). Only `queued`
/// is evidence of placement (held by the runtime); `started` is not yet insertion and `handled` is not declared, so
/// both stay `admitted` (placement unknown), as does an ACK without a disposition.
fn ack_stage(reply:&native::Reply)->&'static str {if reply.data["disposition"]=="queued" {"queued"} else {"admitted"}}
fn record_receipt(shared:&Shared,delivery:&str,value:Value){shared.agent.lock().unwrap().receipt(delivery,value);shared.wake();}
async fn send(shared:&Arc<Shared>,args:&Value,deadline:Instant,owner_proven:bool)->Result<Outcome,ProviderError> {
    let prepared={
        let agent=shared.agent.lock().unwrap();let current=agent.signals.current();
        if agent.configuration.pending{return rejected("busy","native configuration change is unresolved");}
        if args["mode"]=="start" && agent.state()!="idle"{return rejected("busy","endpoint is not evidenced idle");}
        if agent.caps()["agent.send"]["modes"].get(args["mode"].as_str().unwrap_or("")).is_none(){return rejected("unsupported","mode not in current capabilities");}
        // While extensions are loaded they can register commands at any time (Pi's registerCommand stays callable and the
        // runner reads live maps), and Pi reports no change: the start-up get_commands list is no proof that a
        // /name is free. The slash namespace is then unknown: every leading /name is refused (contract::send).
        let slash_names=if agent.expanders_on{None}else{agent.commands.slash_names()};
        let context=contract::Context {caps_rev:agent.caps()["rev"].as_u64().unwrap(),epoch:agent.epoch(),session:current.metadata["sessionId"].as_str(),owner_proven,
            images:agent.caps()["agent.send"]["content"]["types"].as_array().is_some_and(|types|types.iter().any(|kind|kind=="image")),literal:true,
            slash_names:slash_names.as_deref()};
        let prepared=match contract::send(args,&context){Ok(prepared)=>prepared,Err(error)=>return rejected(error.kind,error.message)};
        if agent.receipts.contains_key(&prepared.delivery){return rejected("conflict","delivery already recorded; lookup is not a native insertion key");}
        prepared
    };
    shared.agent.lock().unwrap().invalidate_queue();shared.wake();
    let delivery=prepared.delivery;let native_args=prepared.args;let command=prepared.command;let mode=prepared.mode;
    let base=json!({"delivery":delivery,"origin":prepared.origin,"quoted":prepared.quoted,"rendering":prepared.rendering,"cause":prepared.cause,"correlation":prepared.correlation});
    // With extensions loaded Pi never names them all: the receipt says input may have been changed, by unknown items.
    let base={let mut base=base;if shared.agent.lock().unwrap().expanders_on{base["may_transform"]=json!("unknown");}base};
    let weak=Arc::downgrade(shared);let d=delivery.clone();let metadata=base.clone();
    let receipt:admission::Receipt=Arc::new(move|id,result|{
        if let Some(shared)=weak.upgrade(){let mut value=metadata.clone();value["native"]=json!({"request":id});
            value["stage"]=json!(match &result {Ok(reply) if reply.success=>ack_stage(reply),Err(error) if !error.may_have_acted=>"rejected",_=>"unknown"});
            record_receipt(&shared,&d,value);
        }
    });
    // Inside the start gate, just before the write: this send's ACK is the next native prompt response (origins.rs).
    let entry=origins::Entry::new(&delivery,&base["origin"],&native_args);let weak=Arc::downgrade(shared);
    let arm:admission::Arm=Box::new(move||{if let Some(shared)=weak.upgrade(){shared.agent.lock().unwrap().origins.arm(entry);}});
    let result=match mode.as_str() {
        "start"=>shared.starts.get().unwrap().start_armed(native_args,deadline,receipt,arm).await,
        "admit"|"boundary"|"settle"=>shared.starts.get().unwrap().admit_armed(native_args,deadline,receipt,arm).await,
        _=>direct(shared.native.get().unwrap(),command,native_args,deadline,receipt).await,
    };
    match result {
        Admission::Acknowledged {native,reply}=>{
            if !reply.success{return Err(ProviderError::new("backend",native_message(&reply)));}
            let mut value=base;value["stage"]=json!(ack_stage(&reply));value["native"]=json!({"request":native});Ok(Outcome::Accepted(Some(json!({"receipt":value}))))
        }
        Admission::Dispatched {native}=>{let mut value=base;value["stage"]=json!("dispatched");value["native"]=json!({"request":native});record_receipt(shared,&delivery,value.clone());Ok(Outcome::Dispatched(Some(json!({"receipt":value}))))}
        Admission::Refused(error)=>{shared.agent.lock().unwrap().origins.disarm(&delivery);rejected(error.kind,error.message)},Admission::Unknown(error)=>native_failure(error),
    }
}
async fn publish_fenced(shared:&Shared,provider:&Provider)->Result<(),ProviderError>{
    provider.upsert(shared.facts(&shared.provider_name)).await?;
    provider.call("resource.list",json!({"provider":shared.provider_name,"backend_ref":"agent"})).await?;Ok(())
}
enum Mutation {Configure(configuration::Change),NewSession {before:Option<String>,epoch:String,guard:Option<tokio::sync::OwnedMutexGuard<()>>},Command {name:String,command:&'static str,args:Value,operation:String,guard:Option<tokio::sync::OwnedMutexGuard<()>>}}
impl Mutation {
    fn request(&self)->(&str,Value){match self{Self::Configure(change)=>(change.command,change.args.clone()),Self::NewSession{..}=>("new_session",json!({})),Self::Command{command,args,..}=>(command,args.clone())}}
    fn id(&self)->&str{match self{Self::Configure(change)=>&change.id,Self::NewSession{..}=>"session.new",Self::Command{name,..}=>name}}
    fn operation(&self)->Option<&str>{if let Self::Command{operation,..}=self{Some(operation)}else{None}}
    fn release_guard(&mut self){match self{Self::NewSession{guard,..}|Self::Command{guard,..}=>{guard.take();},_=>{}}}
}
async fn finish_mutation(shared:Arc<Shared>,provider:Provider,mut change:Mutation,reply:Result<native::Reply,native::Error>)->Result<Outcome,ProviderError>{
    let reply=match reply{Ok(reply)=>reply,Err(error) if !error.may_have_acted=>{let mut agent=shared.agent.lock().unwrap();change.release_guard();agent.configuration_pending(false);drop(agent);shared.wake();return rejected(error.kind,error.message);},Err(error)=>{shared.agent.lock().unwrap().incomplete.insert("config".into());shared.wake();return Err(ProviderError::new("backend",format!("native mutation outcome unknown: {error}")));}};
    // An ACK is not the reported value. Keep this one bounded workflow alive after the caller deadline,
    // including a late native reply; never retry the setter and never expose stale model/batch semantics.
    let native=shared.native.get().unwrap();
    let command_data=match &change{Mutation::Command{command,..}=>commands::safe_result(command,&reply.data),_=>Value::Null};
    if matches!(&change,Mutation::Configure(change) if change.command=="set_model") || matches!(&change,Mutation::Command{command:"cycle_model",..}){
        let levels=match native.submit("get_available_thinking_levels",json!({}),Instant::now()+Duration::from_secs(3)){Ok(mut ticket)=>ticket.reply().await,Err(error)=>Err(error)};
        if !matches!(levels,Ok(ref reply) if reply.success){shared.agent.lock().unwrap().configuration.observe_levels(&Value::Null);}
    }
    if let Mutation::Command{name,operation,..}=&change{
        let result=if vqro::limits::encoded_bytes(&command_data)<=48*1024{json!({"result":command_data})}else{json!({"incomplete":["result"]})};
        shared.agent.lock().unwrap().control_result(operation,json!({"command":name,"native_success":reply.success,"native":result}));shared.wake();
    }
    let read=match native.submit("get_state",json!({}),Instant::now()+Duration::from_secs(3)){
        Ok(mut ticket)=>ticket.reply().await,
        Err(error)=>Err(error),
    };
    if !matches!(read,Ok(ref value) if value.success && value.data.is_object()){
        shared.agent.lock().unwrap().incomplete.insert("config".into());shared.wake();
        return if reply.success{Ok(Outcome::Accepted(Some(json!({"id":change.id(),"value_evidence":"unknown"}))))}else{Err(ProviderError::new("backend","native mutation failed; current state unverified"))};
    }
    let (observed,data)={let mut agent=shared.agent.lock().unwrap();change.release_guard();agent.configuration_pending(false);
        match &change{
            Mutation::Configure(change)=>{let observed=agent.configuration.matches(change);(observed,json!({"id":change.id,"value":change.value,"value_evidence":if observed{"reported"}else{"not_matched"}}))},
            Mutation::Command{name,command,operation,..}=>{let mut data=json!({"name":name,"operation":operation});if vqro::limits::encoded_bytes(&command_data)<=48*1024{data["result"]=command_data.clone();}else{data["incomplete"]=json!(["result"]);} (commands::observed_result(command,&reply.data),data)},
            Mutation::NewSession{before,epoch,..}=>{
                let current=agent.session.as_ref().and_then(|session|session["id"].as_str());
                let changed=current.is_some() && current!=before.as_deref() && reply.data["cancelled"]!=true;
                if reply.success && changed{agent.ensure_session_epoch(epoch);}
                (changed,json!({"session":agent.session,"cancelled":reply.data.get("cancelled")}))
            }
        }
    };shared.wake();shared.metadata_refresh.notify_one();drive_ui_cancels(&shared);
    publish_fenced(&shared,&provider).await.map_err(|_|ProviderError::new("backend","native state changed; facts publication not confirmed"))?;
    if !reply.success{return Err(ProviderError::new("backend",native_message(&reply)));}
    if observed{Ok(Outcome::Observed(Some(data)))}else{Ok(Outcome::Accepted(Some(data)))}
}
async fn configure(shared:&Arc<Shared>,provider:&Provider,args:&Value,deadline:Instant)->Result<Outcome,ProviderError>{
    if Instant::now()>=deadline{return rejected("timeout","configuration deadline elapsed before write");}
    let id=match contract::id(&args["id"]){Ok(id)=>id,Err(error)=>return rejected(error.kind,error.message)};
    if args.get("value").is_none(){return rejected("invalid","configuration value required");}
    let change={let mut agent=shared.agent.lock().unwrap();
        if !agent.facts(&shared.provider_name,&shared.cwd)["data"]["agent"]["config"].as_array().is_some_and(|rows|rows.iter().any(|row|row["id"]==id)) && agent.configuration.options().iter().any(|row|row["id"]==id){return rejected("too_large","configuration facts are currently incomplete");}
        let change=match agent.configuration.prepare(id,&args["value"],args.get("rev"),&agent.signals.current().metadata){Ok(change)=>change,Err(error)=>return rejected(error.kind,error.message)};
        agent.configuration_pending(true);change
    };shared.wake();
    dispatch_mutation(shared,provider,Mutation::Configure(change),deadline).await
}
async fn session_new(shared:&Arc<Shared>,provider:&Provider,args:&Value,deadline:Instant)->Result<Outcome,ProviderError>{
    if args["action"]!="new"{return rejected("unsupported","session action is not declared");}
    if args.as_object().unwrap().keys().any(|key|!["v","caps_rev","epoch","guard","cause","correlation","action"].contains(&key.as_str())){return rejected("unsupported","native new session does not support these arguments");}
    if Instant::now()>=deadline{return rejected("timeout","session deadline elapsed before write");}
    let guard=match shared.starts.get().unwrap().session_guard(){Ok(guard)=>guard,Err(error)=>return rejected(error.kind,error.message)};
    let change={let mut agent=shared.agent.lock().unwrap();
        if agent.configuration.pending || agent.signals.current().preflights>0{return rejected("busy","a native mutation/start preflight is unresolved");}
        let change=Mutation::NewSession {before:agent.session.as_ref().and_then(|session|session["id"].as_str()).map(str::to_string),epoch:agent.epoch().into(),guard:Some(guard)};agent.session_pending=true;agent.configuration_pending(true);change
    };shared.wake();dispatch_mutation(shared,provider,change,deadline).await
}
async fn command(shared:&Arc<Shared>,provider:&Provider,args:&Value,deadline:Instant)->Result<Outcome,ProviderError>{
    let name=match contract::id(&args["name"]){Ok(name)=>name,Err(error)=>return rejected(error.kind,error.message)};
    if args.as_object().unwrap().keys().any(|key|!["v","caps_rev","epoch","guard","cause","correlation","name","args"].contains(&key.as_str())){return rejected("unsupported","undeclared command arguments");}
    let prepared={let agent=shared.agent.lock().unwrap();
        if agent.configuration.pending{return rejected("busy","native mutation is unresolved");}
        if !agent.facts(&shared.provider_name,&shared.cwd)["data"]["agent"]["commands"].as_array().is_some_and(|rows|rows.iter().any(|row|row["name"]==name)){return rejected("unsupported","command is not currently advertised");}
        match agent.commands.prepare(name,args.get("args")){Ok(prepared)=>prepared,Err(error)=>return rejected(error.kind,error.message)}
    };
    match prepared{
        commands::Prepared::NewSession=>session_new(shared,provider,&json!({"action":"new"}),deadline).await,
        commands::Prepared::Rpc{name,command,args}=>{
            if Instant::now()>=deadline{return rejected("timeout","command deadline elapsed before write");}
            let guard=match shared.starts.get().unwrap().session_guard(){Ok(guard)=>guard,Err(error)=>return rejected(error.kind,error.message)};
            {let mut agent=shared.agent.lock().unwrap();if agent.configuration.pending{return rejected("busy","native mutation is unresolved");}agent.configuration_pending(true);}shared.wake();
            dispatch_mutation(shared,provider,Mutation::Command{name,command,args,operation:format!("command-{}",fresh()),guard:Some(guard)},deadline).await
        }
        commands::Prepared::Prompt{message}=>{
            let operation=format!("command-{}",fresh());let weak=Arc::downgrade(shared);let op=operation.clone();let command_name=name.to_string();
            let receipt:admission::Receipt=Arc::new(move|native,result|{if let Some(shared)=weak.upgrade(){
                let mut value=json!({"command":command_name,"native_id":native});
                match result{Ok(reply)=>{value["native_success"]=json!(reply.success);if vqro::limits::encoded_bytes(&reply.data)<=48*1024{value["result"]=reply.data;}else{value["incomplete"]=json!(["result"]);}},Err(error)=>{value["outcome"]=json!("unknown");value["error"]=json!(error.kind);}}
                shared.agent.lock().unwrap().control_result(&op,value);shared.wake();
            }});
            // A command's prompt has no core-bound sender: its response must not resolve anyone's origin.
            let weak=Arc::downgrade(shared);let arm:admission::Arm=Box::new(move||{if let Some(shared)=weak.upgrade(){shared.agent.lock().unwrap().origins.arm(None);}});
            match shared.starts.get().unwrap().admit_armed(json!({"message":message}),deadline,receipt,arm).await{
                Admission::Acknowledged{reply,..} if reply.success=>Ok(Outcome::Accepted(Some(json!({"name":name,"operation":operation})))),
                Admission::Acknowledged{reply,..}=>Err(ProviderError::new("backend",native_message(&reply))),
                Admission::Dispatched{..}=>Ok(Outcome::Dispatched(Some(json!({"name":name,"operation":operation})))),
                Admission::Refused(error)=>rejected(error.kind,error.message),Admission::Unknown(error)=>native_failure(error),
            }
        }
    }
}
async fn dispatch_mutation(shared:&Arc<Shared>,provider:&Provider,mut change:Mutation,deadline:Instant)->Result<Outcome,ProviderError>{
    let id=change.id().to_string();let operation=change.operation().map(str::to_string);let (command,args)=change.request();
    let native=shared.native.get().unwrap();
    let mut ticket=match native.submit(command,args,deadline){Ok(ticket)=>ticket,Err(error)=>{let mut agent=shared.agent.lock().unwrap();change.release_guard();agent.configuration_pending(false);drop(agent);shared.wake();return native_failure(error);}};
    let reply=tokio::select!{biased;reply=ticket.reply()=>Some(reply),_=tokio::time::sleep_until(deadline)=>None};
    if reply.is_none() && ticket.cancel_before_write(){let mut agent=shared.agent.lock().unwrap();change.release_guard();agent.configuration_pending(false);drop(agent);shared.wake();return rejected("timeout","native mutation deadline elapsed before write");}
    if let Some(Err(error))=&reply{if !error.may_have_acted{let mut agent=shared.agent.lock().unwrap();change.release_guard();agent.configuration_pending(false);drop(agent);shared.wake();return rejected(error.kind,error.message.clone());}}
    let written=ticket.written();let acknowledged=reply.as_ref().is_some_and(|reply|matches!(reply,Ok(reply) if reply.success));
    let (s,p)=(shared.clone(),provider.clone());let result=tokio::spawn(async move{
        let reply=match reply{Some(reply)=>reply,None=>ticket.reply().await};finish_mutation(s,p,change,reply).await
    });
    match tokio::time::timeout_at(deadline,result).await{
        Ok(Ok(result))=>result,Ok(Err(_))=>Err(ProviderError::new("backend","native mutation workflow failed; outcome unknown")),
        Err(_) if acknowledged=>Ok(Outcome::Accepted(Some(json!({"id":id,"operation":operation,"value_evidence":"unknown"})))),
        Err(_) if written=>Ok(Outcome::Dispatched(Some(json!({"id":id,"operation":operation,"value_evidence":"unknown"})))),
        Err(_)=>Err(ProviderError::new("backend","native mutation write incomplete at deadline; outcome unknown")),
    }
}
async fn ui_fence(shared:Arc<Shared>,native:Native,id:String) {
    if matches!(native.call("get_state",json!({}),Duration::from_secs(2)).await,Ok(reply) if reply.success){shared.agent.lock().unwrap().answer_fenced(&id);shared.wake();}
    else{eprintln!("Pi UI response resolution unverified; never replayed");}
}
fn drive_ui_cancels(shared:&Arc<Shared>){
    let Some(native)=shared.native.get().cloned() else{return;};
    let (actions,overflow)={let mut agent=shared.agent.lock().unwrap();(agent.take_ui_cancels(),agent.ui_overflow)};
    if overflow{eprintln!("Pi UI observation/control bound exceeded");native.stop();return;}
    for (id,wire_id) in actions{
        if shared.agent.lock().unwrap().requests.would_cancel_another(&id,&wire_id){shared.agent.lock().unwrap().answer_fenced(&id);continue;}
        let Ok(permit)=shared.ui_slots.clone().try_acquire_owned()else{native.stop();return;};
        let (shared,native)=(shared.clone(),native.clone());
        tokio::spawn(async move{let _permit=permit;
            if native.notify("extension_ui_response",json!({"id":wire_id,"cancelled":true}),Instant::now()+Duration::from_secs(2)).await.is_ok(){
                shared.agent.lock().unwrap().answer_written(&id,true);shared.wake();ui_fence(shared,native,id).await;
            }else{native.stop();}
        });
    }
}
async fn respond(shared:&Arc<Shared>,args:&Value,deadline:Instant)->Result<Outcome,ProviderError>{
    if args.get("epoch").is_none(){return rejected("invalid","answer requires epoch");}
    let id=match contract::id(&args["request"]){Ok(id)=>id,Err(error)=>return rejected(error.kind,error.message)};
    let Some(revision)=args["revision"].as_u64()else{return rejected("invalid","request revision required");};
    let answer={let mut agent=shared.agent.lock().unwrap();
        let Some((_,entity))=agent.journal.entity("request",id)else{return rejected("stale","request is not retained/pending");};let entity=entity.clone();
        match agent.requests.claim(id,revision,&args["response"],&entity){Ok(answer)=>answer,Err(error)=>return rejected(error.kind,error.message)}
    };
    let native=shared.native.get().unwrap().clone();
    match native.notify("extension_ui_response",answer.wire,deadline).await{
        Ok(())=>{
            shared.agent.lock().unwrap().answer_written(&answer.request,answer.cancelled);shared.wake();
            if let Ok(permit)=shared.ui_slots.clone().try_acquire_owned(){let shared=shared.clone();let id=answer.request.clone();tokio::spawn(async move{let _permit=permit;ui_fence(shared,native,id).await;});}
            Ok(Outcome::Dispatched(Some(json!({"request":answer.request,"revision":revision}))))
        }
        Err(error) if !error.may_have_acted=>{shared.agent.lock().unwrap().requests.rollback(&answer.request);native_failure(error)},
        Err(error)=>{shared.agent.lock().unwrap().answer_written(&answer.request,answer.cancelled);shared.wake();native_failure(error)}
    }
}
async fn queue_op(shared:&Arc<Shared>,args:&Value,deadline:Instant)->Result<Outcome,ProviderError> {
    if args.get("epoch").is_none(){return rejected("invalid","queue action requires an epoch");}
    if args.get("id").is_some() || args.get("content").is_some(){return rejected("unsupported","native queue IDs/replacement are unavailable");}
    match args["action"].as_str(){
        Some("list")=>{let agent=shared.agent.lock().unwrap();return Ok(Outcome::Observed(Some(json!({"queue":agent.queue,"incomplete":agent.queue.is_none()}))));}
        Some("clear")=>{},_=>return rejected("unsupported","undeclared queue action"),
    }
    let operation=format!("queue-{}",fresh());
    let (epoch,version)={let agent=shared.agent.lock().unwrap();(agent.epoch().to_string(),agent.queue_version)};
    let weak=Arc::downgrade(shared);let op=operation.clone();let cause=args.get("cause").cloned();let correlation=args.get("correlation").cloned();
    let receipt:admission::Receipt=Arc::new(move|native,result|{
        if let Some(shared)=weak.upgrade(){
            let mut value=json!({"operation":op,"op":"agent.queue","action":"clear","epoch":epoch,"native":{"request":native},"cause":cause,"correlation":correlation});
            let mut agent=shared.agent.lock().unwrap();
            match result {
                Ok(reply) if reply.success=>{
                    value["outcome"]=json!("observed");value["result"]=queue::removed(&reply.data);
                    // Do not erase a newer native queue update or another input's invalidation.
                    if agent.epoch()==epoch && agent.queue_version==version{agent.queue=Some(json!([]));agent.queue_version=agent.queue_version.wrapping_add(1);}
                }
                Err(error) if !error.may_have_acted=>value["outcome"]=json!("rejected"),
                _=>value["outcome"]=json!("unknown"),
            }
            agent.control_result(&op,value);drop(agent);shared.wake();
        }
    });
    match direct(shared.native.get().unwrap(),"clear_queue",json!({}),deadline,receipt).await {
        Admission::Acknowledged {reply,..} if reply.success=>{let mut value=queue::removed(&reply.data);value["operation"]=json!(operation);Ok(Outcome::Observed(Some(value)))},
        Admission::Acknowledged {reply,..}=>Err(ProviderError::new("backend",native_message(&reply))),
        Admission::Dispatched {..}=>Ok(Outcome::Dispatched(Some(json!({"operation":operation})))),
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

#[cfg(test)]
#[path="tests.rs"] mod tests;
