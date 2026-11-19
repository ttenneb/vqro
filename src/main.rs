use anyhow::{bail, Result};
use clap::{Parser, Subcommand};
use serde_json::Value;
use std::io::Write;

use vqro::{client, paths, server, tui, ui_launch};

#[derive(Parser)]
#[command(name = "vqro", version, about = "Workspace primitives over any multiplexer")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the core.
    Serve {
        /// Start with all extensions stopped.
        #[arg(long)]
        safe: bool,
        /// Also run the in-memory fake backend in process (a test fixture). Real backends are
        /// provider extensions (extension/herdr runs vqro-herdr; extension/tmux runs vqro-tmux).
        #[arg(long)]
        fake: bool,
    },
    /// Open the terminal UI (starts the core in the background if it isn't running). Same as `vqro ui`.
    Tui,
    /// Open a UI client: the built-in terminal UI by default (always available for recovery), or a client
    /// described by `<extensions dir>/clients/<name>.json`. Starts the core if it isn't running.
    Ui {
        /// Client name (`tui` is the built-in one).
        client: Option<String>,
        /// List the available clients.
        #[arg(long)]
        list: bool,
        /// Extra arguments for the client.
        #[arg(last = true)]
        args: Vec<String>,
    },
    /// Stop a background core.
    Stop,
    /// Show whether the core is running.
    Status,
    /// Call a core method: `vqro call workspace.snapshot` or `vqro call command.invoke '{"name":"tabs.open"}'`.
    Call { method: String, params: Option<String> },
    /// Run a command by name, with optional JSON args.
    Run { name: String, args: Option<String> },
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = client::executable_path(); // Remember the invoked install path before cwd/PATH can change.
    match run().await {
        Err(error) if error.downcast_ref::<std::io::Error>().is_some_and(|e| e.kind() == std::io::ErrorKind::BrokenPipe) => Ok(()),
        result => result,
    }
}

async fn run() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::Serve { safe, fake } => serve(safe, fake).await,
        Cmd::Tui => {
            client::ensure_core().await?;
            tui::run(&paths::socket_path()?).await
        }
        Cmd::Ui { client: name, list, args } => ui(name, list, args).await,
        Cmd::Stop => stop().await,
        Cmd::Status => {
            match connect_cli().await {
                Ok((peer, _, hello)) => {
                    writeln!(std::io::stdout(), "running: {hello}")?;
                    let mine = client::build_stamp();
                    match peer.call_with_timeout("client.list", Value::Null, std::time::Duration::from_secs(2)).await {
                        Ok(list) => for c in list["clients"].as_array().into_iter().flatten() {
                            if c["client"] == "cli" {
                                continue;
                            }
                            let stale = match (c["build"].as_u64(), mine) {
                                (Some(b), Some(m)) if b < m => " (older build: restarts on its next reconnect)",
                                (None, _) if c["client"].is_string() => " (old build without self-restart: quit and reopen it)",
                                _ => "",
                            };
                            writeln!(std::io::stdout(), "client: {} {}{}", c["author"].as_str().unwrap_or("?"), c["version"].as_str().unwrap_or(""), stale)?;
                        },
                        Err(e) => writeln!(std::io::stderr(), "client list unavailable: {e}")?,
                    }
                }
                Err(e) if e.downcast_ref::<vqro::rpc::RpcError>().is_some() => writeln!(std::io::stdout(), "core status unavailable ({e})")?,
                Err(e) => writeln!(std::io::stdout(), "not running ({e})")?,
            }
            Ok(())
        }
        Cmd::Call { method, params } => {
            let params: Value = match params { Some(p) => serde_json::from_str(&p)?, None => Value::Null };
            call(&method, params).await
        }
        Cmd::Run { name, args } => {
            let args: Value = match args { Some(a) => serde_json::from_str(&a)?, None => Value::Null };
            call("command.invoke", serde_json::json!({"name": name, "args": args})).await
        }
    }
}

async fn ui(name: Option<String>, list: bool, args: Vec<String>) -> Result<()> {
    let dir = paths::extensions_dir();
    if list {
        for c in ui_launch::list(&dir) {
            writeln!(std::io::stdout(), "{}\t{}\t{}", c["name"].as_str().unwrap_or(""), c["title"].as_str().unwrap_or(""), c["summary"].as_str().unwrap_or(""))?;
        }
        return Ok(());
    }
    let name = name.unwrap_or_else(|| "tui".into());
    if ui_launch::BUILTIN.contains(&name.as_str()) {
        client::ensure_core().await?;
        return tui::run(&paths::socket_path()?).await;
    }
    let manifest = ui_launch::find(&dir, &name)?;
    client::ensure_core().await?;
    let status = ui_launch::command(&dir, &manifest, &paths::socket_path()?, &args).status()
        .map_err(|e| anyhow::anyhow!("cannot start client {name}: {e}; `vqro ui` runs the built-in one"))?;
    if !status.success() {
        bail!("client {name} exited with {status}; `vqro ui` runs the built-in one");
    }
    Ok(())
}

async fn connect_cli() -> Result<(vqro::rpc::Peer, vqro::rpc::IncomingReceiver, Value)> {
    let socket = paths::socket_path()?;
    tokio::time::timeout(std::time::Duration::from_secs(2), client::connect(&socket, "cli")).await
        .map_err(|_| vqro::rpc::RpcError::new("timeout", "core hello deadline expired"))?
}

async fn call(method: &str, params: Value) -> Result<()> {
    let (peer, _incoming, _) = connect_cli().await?;
    // Snapshot recovery is read-only and should report promptly. Keep the existing
    // deadline for arbitrary commands/effects; never replay an uncertain request.
    let deadline = if method == "workspace.snapshot" { std::time::Duration::from_secs(5) } else { vqro::rpc::DEFAULT_CALL_TIMEOUT };
    match peer.call_with_timeout(method, params, deadline).await {
        Ok(v) => {
            writeln!(std::io::stdout(), "{}", serde_json::to_string_pretty(&v)?)?;
            Ok(())
        }
        Err(e) => bail!("{e}"),
    }
}

async fn stop() -> Result<()> {
    let request = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        let (peer, _incoming, _) = client::connect(&paths::socket_path()?, "cli").await?;
        peer.call_with_timeout("core.shutdown", Value::Null, std::time::Duration::from_secs(1)).await.map_err(anyhow::Error::from)
    }).await;
    match request {
        Ok(Ok(value)) if value["outcome"] == "accepted" => { writeln!(std::io::stdout(), "core shutdown accepted")?; return Ok(()); }
        Ok(Ok(_)) => bail!("core answered without accepting shutdown; pid fallback not used"),
        Ok(Err(error)) if error.downcast_ref::<vqro::rpc::RpcError>().is_some_and(|e| !matches!(e.kind(), "timeout" | "disconnected")) => return Err(error),
        _ => {} // No answer: legacy Unix fallback only; never bypass a protocol refusal.
    }
    stop_pidfile()
}

fn stop_pidfile() -> Result<()> {
    let pidfile = paths::state_dir().join("serve.pid");
    let pid = std::fs::read_to_string(&pidfile).map_err(|_| anyhow::anyhow!("no running core (no {})", pidfile.display()))?;
    let number: u32 = pid.trim().parse().map_err(|_| anyhow::anyhow!("invalid core pidfile"))?;
    if number <= 1 { bail!("refusing invalid core pid {number}"); }
    let status = std::process::Command::new("kill").arg(number.to_string()).status()?;
    if !status.success() {
        bail!("could not stop pid {}", pid.trim());
    }
    let _ = std::fs::remove_file(pidfile);
    writeln!(std::io::stdout(), "stopped core (pid {})", pid.trim())?;
    Ok(())
}

async fn serve(safe: bool, fake: bool) -> Result<()> {
    let socket = paths::socket_path()?;
    let backend = if fake { vqro::adapter::Backend::Fake } else { vqro::adapter::Backend::Extensions };
    let core = server::start(socket.clone(), paths::state_dir(), safe, backend, Some(paths::extensions_dir()))?;
    let pidfile = paths::state_dir().join("serve.pid");
    std::fs::write(&pidfile, std::process::id().to_string())?;
    eprintln!("vqro {} listening on {}{}", env!("CARGO_PKG_VERSION"), socket.display(), if safe { " (safe mode)" } else { "" });
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let lifetime = core.lock().unwrap().shutdown.clone();
    tokio::select! {
        r = tokio::signal::ctrl_c() => r?,
        _ = term.recv() => {},
        _ = lifetime.stopped() => {},
    }
    server::shutdown(&core).await;
    // The server owns socket unlink. Do not remove a replacement core's fresh pidfile.
    if std::fs::read_to_string(&pidfile).ok().as_deref() == Some(&std::process::id().to_string()) { let _ = std::fs::remove_file(&pidfile); }
    Ok(())
}
