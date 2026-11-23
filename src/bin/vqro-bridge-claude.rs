//! Explicit native Claude stream-json bridge. No inference prompt, auth lookup or input replay at startup.
#[path="launcher_account/mod.rs"]
mod launcher_account;
use std::collections::HashMap;
use std::time::Duration;
use anyhow::{Context,Result};
use clap::Parser;
use tokio::sync::watch;
use vqro::claude_code::{launch,runtime::Shared};
use vqro::provider_client::{Config,Provider};

#[derive(Parser)]
#[command(about="One native Claude Code agent over its headless stream/control protocol")]
struct Args {
    #[arg(long)] name:Option<String>,
    #[arg(long)] instance:Option<String>,
    #[arg(long)] cwd:Option<std::path::PathBuf>,
    #[arg(last=true,required=true)] native:Vec<String>,
}
fn fresh()->Result<String>{let mut bytes=[0u8;16];getrandom::fill(&mut bytes).map_err(|_|anyhow::anyhow!("cannot allocate provider incarnation"))?;Ok(bytes.iter().map(|b|format!("{b:02x}")).collect())}
fn lifetime()->Result<watch::Receiver<bool>>{
    let (sender,receiver)=watch::channel(false);
    if std::env::var("VQRO_EXTENSION_LIFETIME").as_deref()==Ok("stdin"){
        let sender=sender.clone();std::thread::spawn(move||{
            use std::io::Read;let mut bytes=[0u8;4096];
            while matches!(std::io::stdin().read(&mut bytes),Ok(n) if n>0){}
            sender.send_replace(true);
        });
    }
    #[cfg(unix)] {
        use tokio::signal::unix::{signal,SignalKind};
        let mut term=signal(SignalKind::terminate())?;let mut interrupt=signal(SignalKind::interrupt())?;
        tokio::spawn(async move{tokio::select!{_=term.recv()=>{},_=interrupt.recv()=>{}}sender.send_replace(true);});
    }
    #[cfg(not(unix))] tokio::spawn(async move{let _=tokio::signal::ctrl_c().await;sender.send_replace(true);});
    Ok(receiver)
}
#[tokio::main(flavor="multi_thread",worker_threads=2)]
async fn main(){if let Err(error)=run(Args::parse()).await{eprintln!("vqro-bridge-claude: {error}");std::process::exit(1);}}
async fn run(args:Args)->Result<()> {
    let endpoint=std::env::var("VQRO_SOCKET").context("VQRO_SOCKET is required; launch as a runtime bridge")?;
    let mut credential=std::env::var("VQRO_BRIDGE_TOKEN").context("runtime bootstrap credential is required")?;
    let boot=fresh()?;let label=args.instance.unwrap_or_else(||boot.clone());
    anyhow::ensure!(!label.is_empty() && label.len()<=64 && label.bytes().all(|b|b.is_ascii_alphanumeric() || b"-_.".contains(&b)),"invalid provider instance label");
    let name=args.name.unwrap_or_else(||format!("claude-{label}"));
    anyhow::ensure!(!name.is_empty() && name.len()<=128,"invalid provider name");
    let instance=format!("{label}:{boot}");
    let cwd=args.cwd.unwrap_or(std::env::current_dir()?).canonicalize()?;
    let argv=launch::controlled(args.native).map_err(anyhow::Error::msg)?;
    let mut life=lifetime()?;
    let shared=Shared::new(boot,name.clone(),instance.clone());
    let account=launcher_account::parse(std::env::var("VQRO_ACCOUNT").ok().as_deref()).map_err(anyhow::Error::msg)?;
    shared.state.lock().unwrap().account=account;
    let mut env:HashMap<_,_>=std::env::vars().collect();
    // A restart/resume must not request the native interrupted-turn rescue path. A later explicit
    // continuation is a NEW attributed delivery, never an automatic replay of old user content.
    env.remove("CLAUDE_CODE_RESUME_INTERRUPTED_TURN");env.remove("CLAUDE_CODE_RESUME_REASON");
    let native=shared.spawn_native(&argv,&cwd,&env).await.context("cannot launch explicit native Claude executable")?;
    let outgoing=tokio::spawn(shared.clone().drain_outgoing());
    let mut failures=0u32;
    while native.alive() && !*life.borrow() {
        let mut config=Config::new(&name,&instance,shared.state.lock().unwrap().provider_caps());
        config.role="runtime".into();config.token=Some(credential.clone());config.max_effect_ids=2048;config.max_in_flight=32;
        config.max_item_streams=4;config.max_streams=1;config.call_timeout=Duration::from_secs(5);
        config.software=serde_json::json!({"name":"vqro-bridge-claude","version":env!("CARGO_PKG_VERSION")});
        let connect=async {
            let io=vqro::transport::connect(&endpoint).await.map_err(|_|vqro::provider_client::ProviderError::new("unavailable","core endpoint unavailable"))?;
            Provider::connect(io,config).await
        };
        let connected=tokio::select!{result=connect=>Some(result),_=life.changed()=>None,_=native.exited()=>None};
        let Some(connected)=connected else{break;};
        match connected {
            Ok((provider,mut requests))=>{
                if let Some(token)=provider.resume_token(){credential=token.into();}
                match provider.full_sync(vec![shared.facts()]).await {
                    Ok(())=>{
                        failures=0;
                        let publish=tokio::spawn(shared.clone().publish(provider.clone()));
                        // Re-initialize the SAME process after core reconnect to get native pending
                        // snapshots. Never resend a user input or a previously dispatched answer.
                        let initialize=tokio::spawn(shared.clone().initialize());
                        loop {
                            tokio::select! {
                                request=requests.recv()=>match request{Some(request)=>shared.serve_request(&provider,request).await,None=>break},
                                _=native.exited()=>{publish.abort();initialize.abort();shared.finish(&provider).await;break;},
                                _=life.changed()=>{publish.abort();initialize.abort();native.stop();let _=tokio::time::timeout(Duration::from_secs(5),native.exited()).await;shared.finish(&provider).await;break;},
                            }
                        }
                        publish.abort();initialize.abort();
                    }
                    Err(_)=>{failures+=1;}
                }
                provider.close();
            }
            Err(error)=>{
                failures+=1;
                if matches!(error.kind.as_str(),"forbidden"|"invalid"|"unsupported"){native.stop();break;}
            }
        }
        if failures>=10{native.stop();break;}
        if native.alive() && !*life.borrow(){
            tokio::select!{_=tokio::time::sleep(Duration::from_millis(250))=>{},_=life.changed()=>break,_=native.exited()=>break}
        }
    }
    if *life.borrow(){native.stop();}
    let _=tokio::time::timeout(Duration::from_secs(5),native.exited()).await;
    let _=tokio::time::timeout(Duration::from_secs(2),native.drained()).await;
    outgoing.abort();
    Ok(())
}
