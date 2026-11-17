//! Extension supervisor (D15). prim runs its own extensions as plain processes declared by
//! `extension/<name>` entries: started with `PRIM_SOCKET` and `PRIM_EXTENSION`, restarted after a
//! crash within a budget (3 crashes in 5 minutes), then marked `failed` until changed or restarted.
//! Changing the entry replaces the extension live. Terminals and agents are never touched: they
//! belong to backends.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tokio::process::Command;
use tokio::sync::oneshot;

use crate::core::Shared;

const BUDGET: usize = 3;
const WINDOW: Duration = Duration::from_secs(300);
const RESTART_DELAY: Duration = Duration::from_millis(500);

pub struct ExtRuntime {
    pub status: String,
    pub pid: Option<u32>,
    pub spec_version: u64,
    pub generation: u64,
    pub crashes: VecDeque<Instant>,
    pub last_exit: Option<String>,
    pub running: bool,
    stop: Option<oneshot::Sender<()>>,
}

impl ExtRuntime {
    pub fn to_json(&self) -> Value {
        json!({"status": self.status, "pid": self.pid, "crashes": self.crashes.len(), "last_exit": self.last_exit, "spec_version": self.spec_version})
    }
}

/// Bring running extensions in line with the workspace entries and safe mode.
pub fn reconcile(core: &Shared) {
    let mut c = core.lock().unwrap();
    let specs: Vec<(String, u64, Value)> = c
        .store
        .entries()
        .iter()
        .filter_map(|(k, e)| Some((k.strip_prefix("extension/")?.to_string(), e.version, e.value.clone())))
        .collect();

    // Stop extensions whose entry is gone.
    let names: Vec<String> = c.exts.keys().cloned().collect();
    for name in names {
        if !specs.iter().any(|(n, _, _)| *n == name) {
            if let Some(mut rt) = c.exts.remove(&name) {
                if let Some(stop) = rt.stop.take() {
                    let _ = stop.send(());
                }
            }
            c.emit("extension", json!({"name": name, "status": "removed"}));
        }
    }

    for (name, version, spec) in specs {
        let enabled = spec.get("enabled").and_then(|e| e.as_bool()).unwrap_or(true);
        let want = enabled && !c.safe_mode;
        let safe_mode = c.safe_mode;
        let rt = c.exts.entry(name.clone()).or_insert_with(|| ExtRuntime {
            status: "stopped".into(),
            pid: None,
            spec_version: version,
            generation: 0,
            crashes: VecDeque::new(),
            last_exit: None,
            running: false,
            stop: None,
        });
        let replaced = rt.spec_version != version;
        if replaced {
            // A new spec is a fresh start: live replacement, crash budget reset.
            rt.spec_version = version;
            rt.crashes.clear();
            if rt.status == "failed" {
                rt.status = "stopped".into();
            }
        }
        if rt.running && (!want || replaced) {
            if let Some(stop) = rt.stop.take() {
                let _ = stop.send(());
            }
            rt.running = false;
            rt.pid = None;
            rt.generation += 1;
        }
        if !want {
            let st = if safe_mode { "safe_mode" } else { "disabled" };
            if rt.status != st {
                rt.status = st.into();
                let j = rt.to_json();
                c.emit("extension", json!({"name": name, "state": j}));
            }
            continue;
        }
        if rt.running || rt.status == "failed" {
            continue;
        }
        let argv: Vec<String> = spec["argv"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
        let cwd = spec.get("cwd").and_then(|c| c.as_str()).map(String::from);
        let socket = c.socket.clone();
        let log = c.state_dir.as_ref().map(|d| d.join(format!("ext-{name}.log")));
        let rt = c.exts.get_mut(&name).unwrap();
        rt.generation += 1;
        let generation = rt.generation;
        let mut cmd = Command::new(&argv[0]);
        cmd.args(&argv[1..])
            .env("PRIM_SOCKET", &socket)
            .env("PRIM_EXTENSION", &name)
            .env("PRIM_EXTENSION_LIFETIME", "stdin")
            .stdin(std::process::Stdio::piped())
            .kill_on_drop(true);
        if let Some(cwd) = cwd {
            cmd.current_dir(cwd);
        }
        match log.and_then(|p| std::fs::OpenOptions::new().create(true).append(true).open(p).ok()) {
            Some(f) => {
                let f2 = f.try_clone().ok();
                cmd.stdout(f);
                cmd.stderr(f2.map(std::process::Stdio::from).unwrap_or_else(std::process::Stdio::null));
            }
            None => {
                cmd.stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
            }
        }
        match cmd.spawn() {
            Err(e) => {
                rt.status = "failed".into();
                rt.last_exit = Some(format!("could not start: {e}"));
                let j = rt.to_json();
                c.emit("extension", json!({"name": name, "state": j}));
            }
            Ok(mut child) => {
                // The core alone holds the writer. Kernel pipe closure on core
                // death reaches extensions even when no Rust cleanup can run.
                let lifetime = child.stdin.take();
                let (stop_tx, stop_rx) = oneshot::channel();
                rt.stop = Some(stop_tx);
                rt.running = true;
                rt.pid = child.id();
                rt.status = "running".into();
                let j = rt.to_json();
                c.emit("extension", json!({"name": name, "state": j}));
                let core = core.clone();
                let name = name.clone();
                tokio::spawn(async move {
                    tokio::select! {
                        _ = stop_rx => {
                            let _ = child.kill().await;
                        }
                        status = child.wait() => {
                            let why = match status {
                                Ok(s) => format!("exited: {s}"),
                                Err(e) => format!("wait failed: {e}"),
                            };
                            let restart = on_exit(&core, &name, generation, why);
                            if restart {
                                tokio::time::sleep(RESTART_DELAY).await;
                                reconcile(&core);
                            }
                        }
                    }
                    drop(lifetime); // retain the writer across the whole child lifetime
                });
            }
        }
    }
}

/// An extension exited on its own. Returns whether to restart it.
fn on_exit(core: &Shared, name: &str, generation: u64, why: String) -> bool {
    let mut c = core.lock().unwrap();
    let Some(rt) = c.exts.get_mut(name) else { return false };
    if rt.generation != generation {
        return false; // already replaced or stopped
    }
    rt.running = false;
    rt.pid = None;
    rt.stop = None;
    rt.last_exit = Some(why);
    let now = Instant::now();
    rt.crashes.push_back(now);
    while rt.crashes.front().map(|t| now.duration_since(*t) > WINDOW).unwrap_or(false) {
        rt.crashes.pop_front();
    }
    let restart = rt.crashes.len() < BUDGET;
    rt.status = if restart { "restarting".into() } else { "failed".into() };
    let j = rt.to_json();
    c.emit("extension", json!({"name": name, "state": j}));
    restart
}

/// Explicit restart: clears the crash budget.
pub fn restart(core: &Shared, name: &str) -> Result<(), crate::rpc::RpcError> {
    {
        let mut c = core.lock().unwrap();
        let rt = c.exts.get_mut(name).ok_or_else(|| crate::rpc::RpcError::new("not_found", format!("no extension {name}")))?;
        rt.crashes.clear();
        if let Some(stop) = rt.stop.take() {
            let _ = stop.send(());
        }
        rt.running = false;
        rt.generation += 1;
        rt.status = "stopped".into();
    }
    reconcile(core);
    Ok(())
}

pub fn stop_all(core: &Shared) {
    let mut c = core.lock().unwrap();
    for rt in c.exts.values_mut() {
        if let Some(stop) = rt.stop.take() {
            let _ = stop.send(());
        }
        rt.running = false;
        rt.generation += 1;
    }
}
