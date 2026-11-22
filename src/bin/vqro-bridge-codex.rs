//! Explicitly launched, co-located Codex app-server bridge. One child, many logical endpoint slots.
//! No model call, implicit installation, credential lookup, native restart or input replay at startup.
use clap::Parser;
use serde_json::json;
use std::io::Read;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;
use vqro::codex::observation::{Observation, VIEWERS};
use vqro::codex::runtime::Runtime;
use vqro::provider_client::{Config, Provider, ProviderError, Request};
use vqro::rpc::Peer;

#[derive(Parser)]
#[command(
    about = "A structured Codex app-server bridge; native argv is explicit, stdin is shutdown-only"
)]
struct Args {
    #[arg(long, default_value_t = 1)]
    slots: usize,
    /// Stable human-readable provider label, not the native execution incarnation.
    #[arg(long)]
    instance: Option<String>,
    /// Explicit launcher tag for one endpoint; must match configured VQRO_PRINCIPAL.
    /// Never inferred from a host or exported as a native tool-env capability.
    #[arg(long)]
    endpoint_principal: Option<String>,
    #[arg(last = true, required = true)]
    native: Vec<String>,
}

fn nonce() -> Result<String, String> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes).map_err(|e| e.to_string())?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() {
    if let Err(error) = run(Args::parse()).await {
        eprintln!("vqro-bridge-codex: {error}");
        std::process::exit(1);
    }
}

async fn run(args: Args) -> Result<(), String> {
    if !(1..=200).contains(&args.slots) {
        return Err("--slots must be in 1..=200".into());
    }
    let core = std::env::var("VQRO_SOCKET")
        .map_err(|_| "VQRO_SOCKET is required; launch through resource.spawn bridge:true")?;
    let token = std::env::var("VQRO_BRIDGE_TOKEN")
        .map_err(|_| "VQRO_BRIDGE_TOKEN is required; ordinary shells are not runtime providers")?;
    let endpoint_principal = if let Some(principal) = args.endpoint_principal {
        if args.slots != 1 {
            return Err("--endpoint-principal requires exactly one endpoint (--slots 1)".into());
        }
        if std::env::var("VQRO_PRINCIPAL").ok().as_deref() != Some(principal.as_str())
            || principal == vqro::principal::UNDECLARED
            || vqro::principal::validate_assertable(&principal).is_err()
        {
            return Err(
                "--endpoint-principal must match an explicit assertable VQRO_PRINCIPAL".into(),
            );
        }
        Some(principal)
    } else {
        None
    };
    let incarnation = nonce()?;
    let label = args.instance.unwrap_or_else(|| incarnation.clone());
    if label.is_empty()
        || label.len() > 64
        || !label
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
    {
        return Err("--instance must be 1..64 ASCII letters, digits, -, _ or .".into());
    }
    let mut command = tokio::process::Command::new(&args.native[0]);
    command
        .args(&args.native[1..])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    for (name, _) in std::env::vars_os() {
        let key = name.to_string_lossy();
        if key.starts_with("HERDR_") || key.starts_with("VQRO_") {
            command.env_remove(name);
        }
    }
    // Reachability is not a bridge credential. Native tools may intentionally call the public CLI.
    command
        .env("VQRO_SOCKET", &core)
        .env("VQRO_SOCKET_SOURCE", "terminal");
    let mut child = command
        .spawn()
        .map_err(|e| format!("cannot launch explicit app-server argv: {e}"))?;
    let (native, native_messages) =
        Peer::spawn(child.stdout.take().unwrap(), child.stdin.take().unwrap());
    native
        .set_max_record_bytes(1 << 20)
        .map_err(|e| e.to_string())?;
    let stderr = child.stderr.take().unwrap();
    let stderr_drain = tokio::spawn(async move {
        // Do not retain arbitrary native diagnostics (which can include authentication material).
        let mut stderr = stderr;
        let _ = tokio::io::copy(&mut stderr, &mut tokio::io::sink()).await;
    });
    let (stop, mut stopped) = watch::channel(false);
    let (exited, mut exit) = watch::channel::<Option<String>>(None);
    let provider_name = format!("codex-{label}");
    let provider_instance = format!("{label}:{incarnation}");
    let observation = Observation::new(args.slots, incarnation.clone());
    let runtime = Runtime::with_host_principal(
        native.clone(),
        args.slots,
        incarnation.clone(),
        provider_name.clone(),
        provider_instance.clone(),
        Some(observation.observer()),
        endpoint_principal,
    );
    let model = runtime.clone();
    let child_native = native.clone();
    let child_monitor = tokio::spawn(async move {
        let status = tokio::select! {
            status = child.wait() => status,
            _ = stopped.changed() => {
                child_native.close(); // EOF first; only our explicitly owned child can be killed.
                match tokio::time::timeout(Duration::from_secs(3),child.wait()).await {
                    Ok(status) => status,
                    Err(_) => { let _ = child.kill().await; child.wait().await }
                }
            }
        };
        let reason = match status {
            Ok(status) => {
                let reason = format!("native app-server exited: {status}");
                model.host_exited(&reason);
                reason
            }
            Err(error) => format!("native child wait failed; end is unconfirmed: {error}"),
        };
        child_native.close();
        exited.send_replace(Some(reason));
    });
    let model = runtime.clone();
    let native_drain = tokio::spawn(async move {
        model.drain_native(native_messages).await;
    });
    // This detached OS thread is intentionally not Tokio's uncancellable stdin blocking-pool task:
    // native exit must let the process exit even while the terminal's stdin remains open.
    let stdin_stop = stop.clone();
    std::thread::spawn(move || {
        let mut input = std::io::stdin().lock();
        let mut bytes = [0_u8; 1024];
        loop {
            match input.read(&mut bytes) {
                Ok(0) | Err(_) => {
                    stdin_stop.send_replace(true);
                    break;
                }
                Ok(_) => {} // Never forward keys or text into the native RPC stream.
            }
        }
    });
    let signal_stop = stop.clone();
    let signal = tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            signal_stop.send_replace(true);
        }
    });
    let init = tokio::select! {
        result = native.call_with_timeout("initialize",json!({"clientInfo":{"name":"vqro-bridge-codex","title":"vqro","version":env!("CARGO_PKG_VERSION")},"capabilities":{"experimentalApi":false}}),Duration::from_secs(15)) => result.map_err(|e|e.to_string()),
        _ = exit.changed() => Err("native app-server exited during initialization".into()),
    };
    let result = match init {
        Ok(init) => {
            runtime.initialize_startup(&init);
            native.notify("initialized", json!({}));
            if runtime
                .initialize_models(tokio::time::Instant::now() + Duration::from_secs(5))
                .await
                .is_err()
            {
                eprintln!(
                    "native model discovery unavailable; model configuration is not advertised"
                );
            }
            let mut config = Config::new(provider_name, provider_instance, runtime.capabilities());
            config.role = "runtime".into();
            config.token = Some(token);
            config.max_in_flight = 128;
            config.max_item_streams = args.slots * VIEWERS;
            config.software = json!({"name":"vqro-bridge-codex","version":env!("CARGO_PKG_VERSION"),"runtime":init["userAgent"].as_str().map(|s|s.chars().take(128).collect::<String>())});
            serve(
                &core,
                runtime.clone(),
                observation.clone(),
                config,
                &mut exit,
                stop.subscribe(),
            )
            .await
        }
        Err(error) => Err(error),
    };
    stop.send_replace(true);
    let _ = child_monitor.await;
    native.close();
    let _ = native_drain.await;
    stderr_drain.abort();
    signal.abort();
    result
}

async fn serve(
    core: &str,
    runtime: Arc<Runtime>,
    observation: Arc<Observation>,
    mut config: Config,
    exit: &mut watch::Receiver<Option<String>>,
    mut stopped: watch::Receiver<bool>,
) -> Result<(), String> {
    let mut backoff = Duration::from_millis(125);
    loop {
        if exit.borrow().is_some() || *stopped.borrow() {
            return Ok(());
        }
        let connected = tokio::select! {
            result=async {
                let socket=tokio::net::UnixStream::connect(core).await.map_err(|e|ProviderError::new("unavailable",e.to_string()))?;
                Provider::connect(socket,config.clone()).await
            } => result,
            _=exit.changed()=>return Ok(()),
            _=stopped.changed()=>return Ok(()),
        };
        let (provider, mut requests) = match connected {
            Ok(pair) => pair,
            Err(error) => {
                if let Some(token) = error.data["resume_token"].as_str() {
                    config.token = Some(token.into());
                }
                let safe_mode = error.remote && error.data["reason"] == "safe_mode";
                if error.remote
                    && !safe_mode
                    && matches!(
                        error.kind.as_str(),
                        "forbidden" | "conflict" | "invalid" | "unsupported"
                    )
                {
                    return Err(format!("runtime registration refused: {error}"));
                }
                wait_backoff(backoff, exit, &mut stopped).await;
                backoff = (backoff * 2).min(Duration::from_secs(15));
                continue;
            }
        };
        if let Some(token) = provider.resume_token() {
            config.token = Some(token.into());
        }
        let publisher = Arc::new(tokio::sync::Mutex::new(()));
        let mut changed = runtime.changes();
        // Dirty bits are cleared before the cut. Any subsequent native mutation makes them dirty
        // again and the subscribed watch wakes publication after this complete inventory sync.
        runtime.take_dirty_facts();
        let synced = tokio::select! {
            result=provider.full_sync(runtime.facts())=>result,
            _=exit.changed()=>{provider.close();return Ok(());},
            _=stopped.changed()=>{provider.close();return Ok(());},
        };
        if synced.is_err() {
            provider.close();
            wait_backoff(backoff, exit, &mut stopped).await;
            continue;
        }
        let connected_at = tokio::time::Instant::now();
        loop {
            if exit.borrow().is_some() || *stopped.borrow() {
                // Shutdown is not end evidence. Keep the registration long enough to publish the
                // actual owned-child wait result, including stdin-EOF shutdown, before disconnect.
                if exit.borrow().is_none() {
                    let _ = tokio::time::timeout(Duration::from_secs(4), exit.changed()).await;
                }
                let _ = tokio::time::timeout(
                    Duration::from_secs(2),
                    provider.full_sync(runtime.facts()),
                )
                .await;
                provider.close();
                return Ok(());
            }
            tokio::select! {
                request=requests.recv()=>match request {
                    Some(Request::Effect(request))=>{
                        let runtime=runtime.clone();let connection=provider.clone();let publisher=publisher.clone();
                        tokio::spawn(async move {
                            if connection.is_closed(){request.respond(Err(ProviderError::new("unavailable","originating core connection closed; no native effect started")));return;}
                            let args=if request.op=="agent.send" {
                                match vqro::codex::bind_origin(&request.args,&request.attribution) {
                                    Ok(args)=>args,
                                    Err(error)=>{request.respond(Ok(vqro::provider_client::Outcome::Rejected {kind:error.kind().into(),message:error.message}));return;}
                                }
                            } else {request.args.clone()};
                            let mut context=vqro::codex::SendContext::default();
                            if request.op=="agent.send" && args["origin"]["kind"]=="human" {
                                // Same public owner proof as Pi; unknown/unavailable ownership stays framed.
                                context.owner_proven=match tokio::time::timeout_at(request.deadline,
                                    connection.call("workspace.get",json!({"key":vqro::principal::OWNER_KEY}))).await {
                                    Ok(Ok(owner))=>vqro::codex::recorded_owner(&request.attribution,&owner),_=>false,
                                };
                            }
                            if connection.is_closed(){request.respond(Err(ProviderError::new("unavailable","originating core connection closed; no native effect started")));return;}
                            let result=runtime.invoke_with_context(request.r#ref.as_deref().unwrap_or(""),&request.incarnation,&request.op,args,request.deadline,context).await;
                            // An upsert notification can arrive before a response but commit after
                            // its audit. The public sync acknowledgement fences the current inventory
                            // before an observed session change; never repeat the native operation.
                            if matches!(result,Ok(vqro::provider_client::Outcome::Observed(_)))
                                && ((request.op=="agent.session" && matches!(request.args["action"].as_str(),Some("new"|"resume"))) || request.op=="agent.configure") {
                                let publication=tokio::time::timeout_at(request.deadline,async {
                                    let _publisher=publisher.lock().await;
                                    connection.full_sync(runtime.facts()).await
                                }).await;
                                let failure=match publication {Ok(Ok(()))=>None,Ok(Err(e))=>Some(e.kind),Err(_)=>Some("timeout".into())};
                                if let Some(kind)=failure {
                                    let native_data=match &result {Ok(vqro::provider_client::Outcome::Observed(data))=>data.clone(),_=>None};
                                    request.respond(Err(ProviderError::new("backend","native session/configuration observed; core facts publication was not confirmed")
                                        .with_data(json!({"outcome":"indeterminate","native_outcome":"observed","native_data":native_data,"publication_kind":kind}))));
                                    return;
                                }
                            }
                            request.respond(result);
                        });
                    },
                    Some(Request::Open(request))=>{observation.open(request);},
                    None=>break,
                },
                _=changed.changed()=>{
                    let _publisher=publisher.lock().await;
                    for facts in runtime.take_dirty_facts(){if provider.upsert(facts).await.is_err(){provider.close();break;}}
                },
                _=provider.closed()=>break,
                _=exit.changed()=>{},
                _=stopped.changed()=>{},
            }
        }
        if connected_at.elapsed() >= Duration::from_secs(30) {
            backoff = Duration::from_millis(125);
        }
        provider.close();
        wait_backoff(backoff, exit, &mut stopped).await;
        backoff = (backoff * 2).min(Duration::from_secs(15));
    }
}

async fn wait_backoff(
    delay: Duration,
    exit: &mut watch::Receiver<Option<String>>,
    stopped: &mut watch::Receiver<bool>,
) {
    let mut random = [0_u8; 1];
    let _ = getrandom::fill(&mut random);
    let delay = delay / 2 + delay.mul_f64(f64::from(random[0]) / 510.0);
    tokio::select! {_=tokio::time::sleep(delay)=>{},_=exit.changed()=>{},_=stopped.changed()=>{}}
}
