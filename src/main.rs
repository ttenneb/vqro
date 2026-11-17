use anyhow::{bail, Result};
use clap::{Parser, Subcommand};
use serde_json::Value;

use prim::{client, ext, paths, server, tui};

#[derive(Parser)]
#[command(name = "prim", version, about = "Workspace primitives over any multiplexer")]
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
        /// Command that connects to herdr-core (stdio), e.g. "ssh box hc connect".
        /// Default: $PRIM_HERDR, else `hc connect` from PATH or herdr-core's standard install.
        #[arg(long)]
        herdr: Option<String>,
        /// Use the in-memory fake backend instead (a test fixture).
        #[arg(long)]
        fake: bool,
    },
    /// Open the terminal UI (starts the core in the background if it isn't running).
    Tui,
    /// Stop a background core.
    Stop,
    /// Show whether the core is running.
    Status,
    /// Call a core method: `prim call workspace.snapshot` or `prim call command.invoke '{"name":"tabs.open"}'`.
    Call { method: String, params: Option<String> },
    /// Run a command by name, with optional JSON args.
    Run { name: String, args: Option<String> },
}

#[tokio::main]
async fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::Serve { safe, herdr, fake } => serve(safe, herdr, fake).await,
        Cmd::Tui => {
            client::ensure_core().await?;
            tui::run(&paths::socket_path()?).await
        }
        Cmd::Stop => stop(),
        Cmd::Status => {
            match client::connect(&paths::socket_path()?, "cli").await {
                Ok((peer, _, hello)) => {
                    println!("running: {hello}");
                    let mine = client::build_stamp();
                    if let Ok(list) = peer.call("client.list", serde_json::Value::Null).await {
                        for c in list["clients"].as_array().into_iter().flatten() {
                            if c["client"] == "cli" {
                                continue;
                            }
                            let stale = match (c["build"].as_u64(), mine) {
                                (Some(b), Some(m)) if b < m => " (older build: restarts on its next reconnect)",
                                (None, _) if c["client"].is_string() => " (old build without self-restart: quit and reopen it)",
                                _ => "",
                            };
                            println!("client: {} {}{}", c["author"].as_str().unwrap_or("?"), c["version"].as_str().unwrap_or(""), stale);
                        }
                    }
                }
                Err(e) => println!("not running ({e})"),
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

async fn call(method: &str, params: Value) -> Result<()> {
    let (peer, _incoming, _) = client::connect(&paths::socket_path()?, "cli").await?;
    match peer.call(method, params).await {
        Ok(v) => {
            println!("{}", serde_json::to_string_pretty(&v)?);
            Ok(())
        }
        Err(e) => bail!("{e}"),
    }
}

fn herdr_command(explicit: Option<String>) -> Vec<String> {
    let cmd = explicit.or_else(|| std::env::var("PRIM_HERDR").ok()).unwrap_or_else(|| {
        let on_path = std::env::var("PATH").unwrap_or_default().split(':').any(|d| std::path::Path::new(d).join("hc").exists());
        let installed = std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".local/share/herdr-core/current/Contents/MacOS/herdr-core");
        if !on_path && installed.exists() {
            format!("{} connect", installed.display())
        } else {
            "hc connect".into()
        }
    });
    cmd.split_whitespace().map(String::from).collect()
}

fn stop() -> Result<()> {
    let pidfile = paths::state_dir().join("serve.pid");
    let pid = std::fs::read_to_string(&pidfile).map_err(|_| anyhow::anyhow!("no running core (no {})", pidfile.display()))?;
    let status = std::process::Command::new("kill").arg(pid.trim()).status()?;
    if !status.success() {
        bail!("could not stop pid {}", pid.trim());
    }
    let _ = std::fs::remove_file(pidfile);
    println!("stopped core (pid {})", pid.trim());
    Ok(())
}

async fn serve(safe: bool, herdr: Option<String>, fake: bool) -> Result<()> {
    let socket = paths::socket_path()?;
    let backend = if fake { prim::adapter::Backend::Fake } else { prim::adapter::Backend::Herdr(herdr_command(herdr)) };
    let core = server::start(socket.clone(), paths::state_dir(), safe, backend, Some(paths::extensions_dir()))?;
    let pidfile = paths::state_dir().join("serve.pid");
    std::fs::write(&pidfile, std::process::id().to_string())?;
    eprintln!("prim {} listening on {}{}", env!("CARGO_PKG_VERSION"), socket.display(), if safe { " (safe mode)" } else { "" });
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    tokio::select! {
        r = tokio::signal::ctrl_c() => r?,
        _ = term.recv() => {}
    }
    ext::stop_all(&core);
    let _ = std::fs::remove_file(&socket);
    let _ = std::fs::remove_file(&pidfile);
    Ok(())
}
