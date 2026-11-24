//! Explicit native Claude stream-json bridge. Named credentials resolve only in private helper children.
//! No inference prompt or input replay at startup.
#[path = "launcher_account/mod.rs"]
mod launcher_account;
use anyhow::{Context, Result};
use clap::Parser;
use std::collections::HashMap;
use std::time::Duration;
use tokio::sync::watch;
use vqro::claude_code::{launch, runtime::Shared};
use vqro::provider_client::{Config, Provider};

#[derive(Parser)]
#[command(about = "One native Claude Code agent over its headless stream/control protocol")]
struct Args {
    #[arg(long)]
    name: Option<String>,
    #[arg(long)]
    instance: Option<String>,
    #[arg(long)]
    cwd: Option<std::path::PathBuf>,
    /// Explicit single-endpoint launcher tag; never a native tool-env standing claim.
    #[arg(long)]
    endpoint_principal: Option<String>,
    /// Private execution-host interpreter/helper argv as a JSON array; no public secret resolver.
    #[arg(long)]
    restart_helper: Option<String>,
    /// Private named subscription descriptor; only an opaque keychain reference, never a token.
    #[arg(long)]
    launch_credential: Option<String>,
    #[arg(last = true, required = true)]
    native: Vec<String>,
}
fn runtime_config(shared: &Shared, name: &str, instance: &str, credential: &str) -> Config {
    let mut config = Config::new(name, instance, shared.state.lock().unwrap().provider_caps());
    config.role = "runtime".into();
    config.token = Some(credential.into());
    config.max_effect_ids = 2048;
    config.max_in_flight = 32;
    config.max_item_streams = 4;
    config.max_streams = 1;
    config.call_timeout = Duration::from_secs(5);
    config.software =
        serde_json::json!({"name":"vqro-bridge-claude","version":env!("CARGO_PKG_VERSION")});
    config
}
async fn connect_provider(
    endpoint: &str,
    config: Config,
) -> std::result::Result<
    (
        Provider,
        tokio::sync::mpsc::UnboundedReceiver<vqro::provider_client::Request>,
    ),
    vqro::provider_client::ProviderError,
> {
    let io = vqro::transport::connect(endpoint).await.map_err(|_| {
        vqro::provider_client::ProviderError::new("unavailable", "core endpoint unavailable")
    })?;
    Provider::connect(io, config).await
}
fn fresh() -> Result<String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes)
        .map_err(|_| anyhow::anyhow!("cannot allocate provider incarnation"))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}
fn lifetime(shared: &std::sync::Arc<Shared>) -> Result<watch::Receiver<bool>> {
    let (sender, receiver) = watch::channel(false);
    let weak = std::sync::Arc::downgrade(shared);
    if std::env::var("VQRO_EXTENSION_LIFETIME").as_deref() == Ok("stdin") {
        let sender = sender.clone();
        let weak = weak.clone();
        std::thread::spawn(move || {
            use std::io::Read;
            let mut bytes = [0u8; 4096];
            while matches!(std::io::stdin().read(&mut bytes),Ok(n) if n>0) {}
            if let Some(shared) = weak.upgrade() {
                shared.end_lifetime_blocking(&sender);
            }
        });
    }
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut term = signal(SignalKind::terminate())?;
        let mut interrupt = signal(SignalKind::interrupt())?;
        tokio::spawn(async move {
            tokio::select! {_=term.recv()=>{},_=interrupt.recv()=>{}}
            if let Some(shared) = weak.upgrade() {
                shared.end_lifetime(&sender).await;
            }
        });
    }
    #[cfg(not(unix))]
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        if let Some(shared) = weak.upgrade() {
            shared.end_lifetime(&sender).await;
        }
    });
    Ok(receiver)
}
#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() {
    if let Err(error) = run(Args::parse()).await {
        eprintln!("vqro-bridge-claude: {error}");
        std::process::exit(1);
    }
}
async fn run(args: Args) -> Result<()> {
    launch::credential_host_supported(args.launch_credential.is_some(), cfg!(windows))
        .map_err(anyhow::Error::msg)?;
    let endpoint = std::env::var("VQRO_SOCKET")
        .context("VQRO_SOCKET is required; launch as a runtime bridge")?;
    let mut credential =
        std::env::var("VQRO_BRIDGE_TOKEN").context("runtime bootstrap credential is required")?;
    if let Some(principal) = &args.endpoint_principal {
        anyhow::ensure!(
            std::env::var("VQRO_PRINCIPAL").ok().as_deref() == Some(principal.as_str())
                && principal != vqro::principal::UNDECLARED
                && vqro::principal::validate_assertable(principal).is_ok(),
            "endpoint principal must match explicit assertable VQRO_PRINCIPAL"
        );
    }
    let boot = fresh()?;
    let label = args.instance.unwrap_or_else(|| boot.clone());
    anyhow::ensure!(
        !label.is_empty()
            && label.len() <= 64
            && label
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b)),
        "invalid provider instance label"
    );
    let name = args.name.unwrap_or_else(|| format!("claude-{label}"));
    anyhow::ensure!(
        !name.is_empty() && name.len() <= 128,
        "invalid provider name"
    );
    let instance = format!("{label}:{boot}");
    let cwd = args
        .cwd
        .unwrap_or(std::env::current_dir()?)
        .canonicalize()?;
    let argv = launch::controlled(args.native).map_err(anyhow::Error::msg)?;
    let shared = Shared::new(boot, name.clone(), instance.clone());
    let mut life = lifetime(&shared)?;
    shared.set_lifetime(life.clone());
    let account = launcher_account::parse(std::env::var("VQRO_ACCOUNT").ok().as_deref())
        .map_err(anyhow::Error::msg)?;
    let initial = args
        .launch_credential
        .as_deref()
        .map(|raw| launch::Credential::parse(raw, account.as_ref()))
        .transpose()
        .map_err(anyhow::Error::msg)?;
    {
        let mut state = shared.state.lock().unwrap();
        state.account = account;
        state.principal = args.endpoint_principal;
    }
    let env: HashMap<_, _> = std::env::vars().collect();
    // A restart/resume must not request the native interrupted-turn rescue path. A later explicit
    // continuation is a NEW attributed delivery, never an automatic replay of old user content.
    let private_config = if let Some(helper) = args.restart_helper {
        let helper: Vec<String> =
            serde_json::from_str(&helper).context("invalid private restart helper argv")?;
        let config = vqro::claude_code::restart::Config {
            helper,
            native: argv.clone(),
            cwd: cwd.clone(),
            environment: env.clone(),
        };
        shared
            .configure_restart(config.clone())
            .map_err(anyhow::Error::msg)?;
        Some(config)
    } else {
        None
    };
    let mut first_connection = None;
    let startup_argv = if let Some(initial) = initial {
        let config = private_config
            .as_ref()
            .context("named Claude launch requires the private helper")?;
        // Authenticate the newly allocated backend host BEFORE any credential lookup.
        let connected = tokio::select! {biased;_=life.changed()=>anyhow::bail!("bridge lifetime ended before named launch"),result=connect_provider(&endpoint,runtime_config(&shared,&name,&instance,&credential))=>result};
        let (provider, requests) =
            connected.map_err(|_| anyhow::anyhow!("named Claude host registration unavailable"))?;
        let setup = async {
            let host = provider.registration()["host"].as_str().context("authenticated runtime host unavailable")?;
            let resources = provider.call("resource.list", serde_json::json!({"id":host})).await.map_err(|_| anyhow::anyhow!("authenticated launch host facts unavailable"))?;
            initial.verify_host(host, &resources).map_err(anyhow::Error::msg)?;
            tokio::select! {biased;_=life.changed()=>anyhow::bail!("bridge lifetime ended before named launch"),result=config.preflight_reference(&initial.reference,tokio::time::Instant::now()+Duration::from_secs(35))=>result.map_err(|_|anyhow::anyhow!("named Claude credential preflight unavailable; no native launch"))?};
            config.initial_launch(&initial.reference).map_err(anyhow::Error::msg)
        }.await;
        let startup = match setup {
            Ok(value) => value,
            Err(error) => {
                provider.close();
                return Err(error);
            }
        };
        shared.state.lock().unwrap().configured_binding = Some(initial);
        first_connection = Some((provider, requests));
        startup
    } else {
        argv.clone()
    };
    // Only the child startup argv is wrapped. Restart config retains canonical native argv.
    shared
        .spawn_native(&startup_argv, &cwd, &env)
        .await
        .context("cannot launch explicit native Claude executable")?;
    let outgoing = tokio::spawn(shared.clone().drain_outgoing());
    let mut failures = 0u32;
    while (shared.restarting() || shared.process().is_some_and(|native| native.alive()))
        && !*life.borrow()
    {
        let changed = shared.changed.notified();
        tokio::pin!(changed);
        changed.as_mut().enable();
        if shared.restarting() {
            tokio::select! {_=changed=>{},_=life.changed()=>break};
            continue;
        }
        let Some(native) = shared.process() else {
            break;
        };
        let connect = connect_provider(
            &endpoint,
            runtime_config(&shared, &name, &instance, &credential),
        );
        let connected = if let Some(first) = first_connection.take() {
            Some(Ok(first))
        } else {
            tokio::select! {result=connect=>Some(result),_=life.changed()=>None,_=native.exited()=>None}
        };
        let Some(connected) = connected else {
            if shared.restarting() && !*life.borrow() {
                continue;
            }
            break;
        };
        match connected {
            Ok((provider, mut requests)) => {
                if let Some(token) = provider.resume_token() {
                    credential = token.into();
                }
                match provider.full_sync(vec![shared.facts()]).await {
                    Ok(()) => {
                        failures = 0;
                        let publish = tokio::spawn(shared.clone().publish(provider.clone()));
                        // Re-initialize the SAME process after core reconnect to get native pending
                        // snapshots. Never resend a user input or a previously dispatched answer.
                        let initialize = tokio::spawn(shared.clone().initialize());
                        let mut ending = false;
                        loop {
                            let changed = shared.changed.notified();
                            tokio::pin!(changed);
                            changed.as_mut().enable();
                            if shared.restarting() {
                                tokio::select! {
                                    request=requests.recv()=>match request{Some(request)=>shared.serve_request(&provider,request).await,None=>break},
                                    _=changed=>{},
                                    _=life.changed()=>{if let Some(active)=shared.process(){active.stop();let _=tokio::time::timeout(Duration::from_secs(5),active.exited()).await;ending=true;}break;}
                                }
                                continue;
                            }
                            let Some(active) = shared.process() else {
                                break;
                            };
                            tokio::select! {
                                request=requests.recv()=>match request{Some(request)=>shared.serve_request(&provider,request).await,None=>break},
                                _=active.exited()=>{if shared.restarting()||shared.process().is_some_and(|current|current.alive()){continue;}ending=true;break;},
                                _=life.changed()=>{active.stop();let _=tokio::time::timeout(Duration::from_secs(5),active.exited()).await;ending=true;break;},
                            }
                        }
                        publish.abort();
                        initialize.abort();
                        // Cancellation alone is not retirement: join before queuing the final cut.
                        let _ = publish.await;
                        let _ = initialize.await;
                        if ending && shared.finish(&provider).await.is_err() {
                            eprintln!(
                                "vqro-bridge-claude: final publication unconfirmed; no retry"
                            );
                        }
                    }
                    Err(_) => {
                        failures += 1;
                    }
                }
                provider.close();
            }
            Err(error) => {
                failures += 1;
                if matches!(error.kind.as_str(), "forbidden" | "invalid" | "unsupported") {
                    if let Some(current) = shared.process() {
                        current.stop();
                    }
                    break;
                }
            }
        }
        if failures >= 10 {
            if let Some(current) = shared.process() {
                current.stop();
            }
            break;
        }
        if let Some(current) = shared
            .process()
            .filter(|current| current.alive() && !*life.borrow())
        {
            tokio::select! {_=tokio::time::sleep(Duration::from_millis(250))=>{},_=life.changed()=>break,_=current.exited()=>break}
        }
    }
    if let Some(current) = shared.process() {
        if *life.borrow() {
            current.stop();
        }
        let _ = tokio::time::timeout(Duration::from_secs(5), current.exited()).await;
        let _ = tokio::time::timeout(Duration::from_secs(2), current.drained()).await;
    }
    outgoing.abort();
    Ok(())
}
