//! Extension supervisor (D15). vqro runs its own extensions as plain processes declared by
//! `extension/<name>` entries: started with `VQRO_SOCKET` and `VQRO_EXTENSION`, restarted after a
//! crash within a budget (3 crashes in 5 minutes), then marked `failed` until changed or restarted.
//! Changing the entry replaces the extension live. Terminals and agents are never touched: they
//! belong to backends.

use std::collections::VecDeque;
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tokio::process::Command;
use tokio::sync::oneshot;

use crate::core::Shared;

const BUDGET: usize = 3;
pub(crate) const CRASH_WINDOW: Duration = Duration::from_secs(300);
const RESTART_DELAY: Duration = Duration::from_millis(500);
// Cooperative providers (vqro-tmux) allow up to1s for their private backend to exit.
// Leave a cleanup margin inside the supervisor's2s overall shutdown bound.
const STOP_GRACE: Duration = Duration::from_millis(1500);

pub struct ExtRuntime {
    pub status: String,
    pub pid: Option<u32>,
    pub spec_version: u64,
    pub generation: u64,
    pub crashes: VecDeque<Instant>,
    crashes_total: u64, // This declaration's supervised exits during this core lifetime.
    pub last_exit: Option<String>,
    failure: Option<Value>,
    last_exits: VecDeque<Value>, // Last three exits survive budget aging/reset, bounded independently.
    pub running: bool,
    stop: Option<oneshot::Sender<()>>,
    token: String,
    live: Arc<AtomicBool>,
}

impl ExtRuntime {
    fn fail(&mut self, kind: &str, reason: String) {
        self.status="failed".into();
        let now=Instant::now();
        let crashes=self.crashes.iter().filter(|at|now.saturating_duration_since(**at)<=CRASH_WINDOW).count();
        self.failure=Some(json!({"kind":kind,"reason":reason,"at_ms":crate::store::now_ms(),
            "crashes":crashes,"window_seconds":CRASH_WINDOW.as_secs()}));
    }

    pub fn to_json(&self) -> Value {
        self.to_json_at(Instant::now())
    }

    fn to_json_at(&self, now: Instant) -> Value {
        // Serialization must age the displayed count even during a healthy run
        // with no new exit. Budget resets and the failed latch are unchanged.
        let recent = self.crashes.iter().filter(|at| now.saturating_duration_since(**at) <= CRASH_WINDOW).count();
        json!({"status": self.status, "running":self.running, "generation":self.generation,
            "crashes": recent, "crashes_total": self.crashes_total, "crash_window_seconds": CRASH_WINDOW.as_secs(),
            "last_exit": self.last_exit, "last_exits":self.last_exits, "failure":self.failure, "spec_version": self.spec_version})
    }
}

#[derive(Clone, Debug)]
pub struct Owner { pub name:String, pub generation:u64, live:Arc<AtomicBool> }
impl Owner {
    pub fn is_live(&self)->bool { self.live.load(Ordering::Acquire) }
}
pub fn authenticate(core:&crate::core::Core, name:&str, token:&str)->Option<Owner> {
    core.exts.get(name).filter(|r|r.running && !r.token.is_empty() && r.token==token).map(|r|Owner{name:name.into(),generation:r.generation,live:r.live.clone()})
}
pub fn owner_valid(core:&crate::core::Core, owner:&Owner)->bool { owner.is_live() && core.exts.get(&owner.name).is_some_and(|r|r.running && r.generation==owner.generation) }
pub(crate) fn check_owner(core:&crate::core::Core, owner:Option<&Owner>)->Result<(),crate::rpc::RpcError> {
    if owner.is_some_and(|o|!owner_valid(core,o)) { return Err(crate::provider::rejected("stale_generation","supervised extension lifetime ended; not dispatched")); }
    Ok(())
}

/// Bring running extensions in line with the workspace entries and safe mode.
pub fn reconcile(core: &Shared) {
    let mut c = core.lock().unwrap();
    if c.shutdown.is_requested() { return; }
    c.extension_tasks.retain(|task| !task.is_finished());
    let specs: Vec<(String, u64, Value)> = c
        .store
        .entries()
        .iter()
        .filter_map(|(k, e)| Some((k.strip_prefix("extension/")?.to_string(), e.version, e.value.clone())))
        .collect();

    c.manifest_catalog.refresh(&specs);
    // Stop extensions whose entry is gone.
    let names: Vec<String> = c.exts.keys().cloned().collect();
    for name in names {
        if !specs.iter().any(|(n, _, _)| *n == name) {
            if let Some(mut rt) = c.exts.remove(&name) {
                rt.live.store(false,Ordering::Release);
                if let Some(stop) = rt.stop.take() {
                    let _ = stop.send(());
                }
            }
            c.emit("extension", json!({"name": name, "status": "removed"}));
        }
    }

    for (name, version, spec) in specs {
        let enabled = spec.get("enabled").and_then(|e| e.as_bool()).unwrap_or(true);
        let want = if c.safe_mode { c.safe_mode_allow.contains(&name) } else { enabled };
        let safe_mode = c.safe_mode;
        let rt = c.exts.entry(name.clone()).or_insert_with(|| ExtRuntime {
            status: "stopped".into(),
            pid: None,
            spec_version: version,
            generation: 0,
            crashes: VecDeque::new(),
            crashes_total: 0,
            last_exit: None,
            failure: None,
            last_exits: VecDeque::new(),
            running: false,
            stop: None,
            token: String::new(),
            live:Arc::new(AtomicBool::new(false)),
        });
        let replaced = rt.spec_version != version;
        if replaced {
            // A new spec is a fresh start: live replacement, crash budget reset.
            rt.spec_version = version;
            rt.crashes.clear();
            rt.failure=None;
            if rt.status == "failed" {
                rt.status = "stopped".into();
            }
        }
        if rt.running && (!want || replaced) {
            rt.live.store(false,Ordering::Release);
            if let Some(stop) = rt.stop.take() {
                let _ = stop.send(());
            }
            rt.running = false;
            rt.pid = None;
            rt.generation += 1;
        }
        if !want {
            let st = if safe_mode { "safe_mode" } else { "disabled" };
            if rt.status != "failed" && rt.status != st {
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
        rt.live=Arc::new(AtomicBool::new(true));
        let mut secret=[0u8;32];
        if let Err(error)=getrandom::fill(&mut secret) {
            let why=format!("supervision credential: {error}");rt.last_exit=Some(why.clone());rt.fail("startup",why);
            let state=rt.to_json();c.emit("extension",json!({"name":name,"state":state}));continue;
        }
        rt.token=secret.iter().map(|b|format!("{b:02x}")).collect();
        let program=match crate::program::resolve_provider(&argv[0]) {
            Ok(program)=>program,
            Err(error)=>{ rt.last_exit=Some(error.clone());rt.fail("startup",error);let state=rt.to_json();c.emit("extension",json!({"name":name,"state":state}));continue; }
        };
        let mut cmd = Command::new(program);
        cmd.args(&argv[1..])
            .env("VQRO_SOCKET", &socket)
            .env_remove("VQRO_SOCKET_SOURCE")
            .env("VQRO_EXTENSION", &name)
            .env("VQRO_EXTENSION_TOKEN", &rt.token)
            .env("VQRO_EXTENSION_LIFETIME", "stdin")
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
                let why=format!("could not start: {e}");
                rt.last_exit = Some(why.clone());
                rt.fail("startup",why);
                let j = rt.to_json();
                c.emit("extension", json!({"name": name, "state": j}));
            }
            Ok(mut child) => {
                // The core alone holds the writer. Kernel pipe closure on core
                // death reaches extensions even when no Rust cleanup can run.
                let mut lifetime = child.stdin.take();
                let (stop_tx, stop_rx) = oneshot::channel();
                rt.stop = Some(stop_tx);
                rt.running = true;
                rt.pid = child.id();
                rt.status = "running".into();
                let j = rt.to_json();
                c.emit("extension", json!({"name": name, "state": j}));
                let core = core.clone();
                let name = name.clone();
                c.extension_tasks.push(tokio::spawn(async move {
                    tokio::select! {
                        _ = stop_rx => {
                            // Cooperative extensions see EOF and can close owned private backends.
                            // An uncooperative extension is still bounded and reaped.
                            drop(lifetime.take());
                            if tokio::time::timeout(STOP_GRACE, child.wait()).await.is_err() { let _ = child.kill().await; }
                            let mut c = core.lock().unwrap();
                            if let Some(rt) = c.exts.get_mut(&name) {
                                if rt.generation == generation + 1 && !rt.running { rt.pid = None; rt.status = "stopped".into(); }
                            }
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
                }));
            }
        }
    }
    c.fence_retired_extensions();
}

/// An extension exited on its own. Returns whether to restart it.
fn on_exit(core: &Shared, name: &str, generation: u64, why: String) -> bool {
    let mut c = core.lock().unwrap();
    let shutting_down = c.shutdown.is_requested();
    let Some(rt) = c.exts.get_mut(name) else { return false };
    if rt.generation != generation {
        return false; // already replaced or stopped
    }
    rt.live.store(false,Ordering::Release);
    rt.running = false;
    rt.pid = None;
    rt.stop = None;
    rt.last_exit = Some(why.clone());
    rt.last_exits.push_back(json!({"at_ms":crate::store::now_ms(),"reason":why}));
    while rt.last_exits.len()>BUDGET {rt.last_exits.pop_front();}
    let now = Instant::now();
    rt.crashes_total = rt.crashes_total.saturating_add(1);
    rt.crashes.push_back(now);
    while rt.crashes.front().map(|t| now.duration_since(*t) > CRASH_WINDOW).unwrap_or(false) {
        rt.crashes.pop_front();
    }
    let restart = !shutting_down && rt.crashes.len() < BUDGET;
    if restart {rt.status="restarting".into();}
    else if rt.crashes.len()>=BUDGET {rt.fail("crash_budget",format!("{} exits in {} seconds; stopped until explicit restart",rt.crashes.len(),CRASH_WINDOW.as_secs()));}
    else {rt.fail("shutdown","core stopping; exit was not retried".into());}
    let j = rt.to_json();
    c.emit("extension", json!({"name": name, "state": j}));
    c.fence_retired_extensions();
    restart
}

/// Explicit restart: clears the crash budget.
pub fn restart(core: &Shared, name: &str) -> Result<(), crate::rpc::RpcError> { restart_owned(core,name,None) }
pub(crate) fn restart_owned(core:&Shared,name:&str,owner:Option<&Owner>)->Result<(),crate::rpc::RpcError> {
    {
        let mut c = core.lock().unwrap();
        check_owner(&c,owner)?;
        let rt = c.exts.get_mut(name).ok_or_else(|| crate::rpc::RpcError::new("not_found", format!("no extension {name}")))?;
        rt.crashes.clear();
        rt.failure=None;
        if let Some(stop) = rt.stop.take() {
            let _ = stop.send(());
        }
        rt.live.store(false,Ordering::Release);
        rt.running = false;
        rt.generation += 1;
        rt.status = "stopped".into();
        c.fence_retired_extensions();
    }
    reconcile(core);
    Ok(())
}

/// Close lifetimes, await child cleanup/reaping, and never restart while the core is stopping.
pub async fn shutdown(core: &Shared) {
    stop_all(core);
    let mut tasks = std::mem::take(&mut core.lock().unwrap().extension_tasks);
    let joined = tokio::time::timeout(Duration::from_secs(2), async {
        for task in &mut tasks { let _ = task.await; }
    }).await;
    if joined.is_ok() {
        for rt in core.lock().unwrap().exts.values_mut() { rt.pid = None; if rt.status != "failed" { rt.status = "stopped".into(); } }
    }
    if joined.is_err() {
        eprintln!("vqro: extension shutdown deadline; cleanup unconfirmed");
        for task in &tasks { task.abort(); }
        for task in tasks { let _ = task.await; }
    }
}

pub fn stop_all(core: &Shared) {
    stop_all_locked(&mut core.lock().unwrap());
}

pub(crate) fn stop_all_locked(c: &mut crate::core::Core) {
    for rt in c.exts.values_mut() {
        rt.live.store(false,Ordering::Release);
        if let Some(stop) = rt.stop.take() {
            let _ = stop.send(());
        }
        rt.running = false;
        rt.generation += 1;
    }
    c.fence_retired_extensions();
}

#[cfg(test)]
mod qa2_tests {
    use super::*;

    fn runtime_with_crashes(crashes: VecDeque<Instant>) -> ExtRuntime {
        ExtRuntime { status: "failed".into(), pid: None, spec_version: 1, generation: 7, crashes_total: crashes.len() as u64, crashes,
            last_exit: Some("historical failure".into()), failure:None, last_exits:VecDeque::new(), running: false, stop: None, token: String::new(), live:Arc::new(AtomicBool::new(false)) }
    }

    #[test]
    fn crash_facts_exclude_expired_budget_entries_without_losing_the_total() {
        let start = Instant::now();
        let now = start+CRASH_WINDOW+Duration::from_secs(1);
        let mut rt = runtime_with_crashes(VecDeque::from([start, start+Duration::from_secs(1)]));
        let state = rt.to_json_at(now);
        assert_eq!(state["crashes"], 1, "healthy elapsed time must age old crashes out of displayed budget counts");
        assert_eq!(state["crashes_total"], 2);
        assert_eq!(state["crash_window_seconds"], 300);
        let aged = rt.to_json_at(now+Duration::from_secs(1));
        assert_eq!(aged["crashes"], 0);
        assert_eq!(aged["crashes_total"], 2);
        assert_eq!(aged["status"], "failed", "aging must not clear the crash-budget failed latch");
        rt.crashes.clear(); // Explicit restart/spec replacement resets only the budget.
        let reset = rt.to_json_at(now);
        assert_eq!(reset["crashes"], 0);
        assert_eq!(reset["crashes_total"], 2);
        assert_eq!(reset["last_exit"], "historical failure");
    }

    #[test]
    fn unexpected_exits_increment_total_but_only_recent_exits_exhaust_the_budget() {
        let mut c = crate::core::Core::new(crate::store::Store::in_memory(), None, "unused".into(), false).unwrap();
        let mut rt = runtime_with_crashes(VecDeque::new());
        rt.crashes_total = 1; // An older, already expired crash still belongs in the total.
        c.exts.insert("fixture".into(), rt);
        let core = std::sync::Arc::new(std::sync::Mutex::new(c));
        assert!(on_exit(&core, "fixture", 7, "first recent exit".into()));
        assert!(on_exit(&core, "fixture", 7, "second recent exit".into()));
        assert!(!on_exit(&core, "fixture", 7, "third recent exit".into()));
        let c = core.lock().unwrap();
        let state = c.exts["fixture"].to_json();
        assert_eq!(state["crashes"], 3);
        assert_eq!(state["crashes_total"], 4);
        assert_eq!(state["status"], "failed");
        assert_eq!(state["failure"]["kind"],"crash_budget");
        assert_eq!(state["failure"]["crashes"],3);
        assert!(state["failure"]["at_ms"].as_u64().unwrap()>0);
        assert_eq!(state["last_exits"].as_array().unwrap().len(),3);
        let aged=c.exts["fixture"].to_json_at(Instant::now()+Duration::from_secs(3600));
        assert_eq!(aged["crashes"],0);
        assert_eq!(aged["crashes_total"],4);
        assert_eq!(aged["failure"],state["failure"],"aging does not erase why/when the failed latch was set");
        assert_eq!(aged["last_exits"],state["last_exits"]);
    }
    #[tokio::test]
    async fn aged_failure_stays_latched_and_explicit_restart_preserves_history_and_safe_mode() {
        let mut store=crate::store::Store::in_memory();
        store.apply("test",None,vec![crate::store::Op::Set {key:"extension/fixture".into(),value:json!({"argv":["/bin/cat"]}),expect:None}]).unwrap();
        let mut c=crate::core::Core::new(store,None,"unused".into(),false).unwrap();
        let old=Instant::now()-CRASH_WINDOW-Duration::from_secs(1);
        let mut rt=runtime_with_crashes(VecDeque::from([old,old,old]));
        rt.last_exits.push_back(json!({"at_ms":1234,"reason":"third exit"}));
        rt.fail("crash_budget","3 exits in 300 seconds".into());
        c.exts.insert("fixture".into(),rt);
        let core=std::sync::Arc::new(std::sync::Mutex::new(c));
        reconcile(&core);
        {
            let mut c=core.lock().unwrap();
            let state=c.exts["fixture"].to_json();
            assert_eq!(state["crashes"],0);
            assert_eq!(state["status"],"failed");
            assert_eq!(state["running"],false,"cool-down does not silently restart a failed extension");
            c.safe_mode=true;
        }
        restart(&core,"fixture").unwrap();
        let c=core.lock().unwrap();let state=c.exts["fixture"].to_json();
        assert!(c.safe_mode);
        assert_eq!(state["running"],false,"restart cannot bypass safe-mode admission");
        assert_eq!(state["crashes_total"],3);
        assert_eq!(state["last_exits"][0]["reason"],"third exit");
        assert!(state["failure"].is_null());
    }

    #[tokio::test]
    async fn cooperative_provider_gets_time_for_its_one_second_cleanup() {
        let root=tempfile::Builder::new().prefix("pxq").tempdir_in("/tmp").unwrap();
        let ready=root.path().join("ready"); let done=root.path().join("done");
        let script="import pathlib,sys,time; pathlib.Path(sys.argv[1]).write_text('ready'); sys.stdin.buffer.read(); time.sleep(0.8); pathlib.Path(sys.argv[2]).write_text('done')";
        let mut store=crate::store::Store::in_memory();
        store.apply("test",None,vec![crate::store::Op::Set { key:"extension/cleanup".into(),value:json!({"argv":["/usr/bin/python3","-c",script,ready,done]}),expect:None }]).unwrap();
        let core=std::sync::Arc::new(std::sync::Mutex::new(crate::core::Core::new(store,None,root.path().join("s"),false).unwrap()));
        reconcile(&core);
        let started=tokio::time::timeout(Duration::from_secs(2),async { while !ready.exists() { tokio::time::sleep(Duration::from_millis(5)).await; } }).await;
        // Always reap the private child before any assertion, including the red assertion.
        shutdown(&core).await;
        assert!(started.is_ok(),"private helper did not start");
        assert!(done.exists(),"500ms grace killed a cooperative provider during its bounded cleanup");
        assert!(core.lock().unwrap().exts.values().all(|rt|rt.pid.is_none()));
    }
    #[tokio::test]
    async fn repeated_reconciliation_prunes_finished_supervisor_handles() {
        let core=std::sync::Arc::new(std::sync::Mutex::new(crate::core::Core::new(crate::store::Store::in_memory(),None,"unused".into(),false).unwrap()));
        for _ in 0..64 {
            let task=tokio::spawn(async {});
            tokio::time::timeout(Duration::from_secs(1),async { while !task.is_finished() { tokio::task::yield_now().await; } }).await.unwrap();
            core.lock().unwrap().extension_tasks.push(task);
            reconcile(&core);
            assert!(core.lock().unwrap().extension_tasks.is_empty());
        }
    }
}
