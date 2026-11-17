//! prim-herdr against an in-test fake core (provider-v0 section 9) and a PRIVATE herdr-core.
//! Opt-in: cargo test --test herdr_provider -- --ignored --test-threads=1
//! The core runs from ~/.local/bin/herdr-core with env -i: private HOME/XDG dirs, its own HC_SESSION
//! and an empty PATH (it cannot reach loginctl/systemctl). Only that private core is ever stopped.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use base64::Engine;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::UnixListener;

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

fn herdr_core() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap()).join(".local/bin/herdr-core")
}

/// A private herdr-core session: every hc call gets exactly these variables.
struct Hc {
    root: tempfile::TempDir,
}

impl Hc {
    fn new() -> Hc {
        assert!(herdr_core().exists(), "needs ~/.local/bin/herdr-core");
        // Short: herdr-core's socket path must stay under 107 bytes.
        let root = tempfile::Builder::new().prefix("phc").tempdir_in("/tmp").unwrap();
        for d in ["h", "s", "r", "c"] {
            std::fs::create_dir(root.path().join(d)).unwrap();
        }
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(root.path().join("r"), std::fs::Permissions::from_mode(0o700)).unwrap();
        Hc { root }
    }
    fn env(&self, cmd: &mut Command) {
        let p = self.root.path();
        cmd.env_clear()
            .env("HOME", p.join("h"))
            .env("XDG_STATE_HOME", p.join("s"))
            .env("XDG_RUNTIME_DIR", p.join("r"))
            .env("XDG_CONFIG_HOME", p.join("c"))
            .env("HC_SESSION", "p")
            .env("PATH", "");
    }
    fn hc(&self, args: &[&str]) -> Value {
        let mut cmd = Command::new(herdr_core());
        self.env(&mut cmd);
        let out = cmd.args(args).arg("--json").output().unwrap();
        serde_json::from_slice(&out.stdout).unwrap_or(Value::Null)
    }
    fn core_id(&self) -> Option<String> {
        let v: Value = serde_json::from_slice(&std::fs::read(self.root.path().join("s/herdr-core/p/core.json")).ok()?).ok()?;
        v["id"].as_str().map(String::from)
    }
    fn core_pid(&self) -> Option<u64> {
        self.hc(&["server", "status"])["pid"].as_u64()
    }
    /// Stop only this private core (and make sure it is gone).
    fn stop(&self) {
        let pid = std::fs::read_to_string(self.root.path().join("s/herdr-core/p/core.json")).ok().and_then(|_| {
            // `server status` would auto-start a stopped core; only ask while its socket exists.
            self.root.path().join("r/herdr-core/p.sock").exists().then(|| self.core_pid()).flatten()
        });
        if self.root.path().join("r/herdr-core/p.sock").exists() {
            self.hc(&["server", "stop"]);
        }
        if let Some(pid) = pid {
            for _ in 0..100 {
                if !Path::new(&format!("/proc/{pid}")).exists() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            let _ = Command::new("/bin/kill").args(["-KILL", &pid.to_string()]).status();
        }
    }
}

impl Drop for Hc {
    fn drop(&mut self) {
        self.stop();
    }
}

struct Core {
    r: BufReader<OwnedReadHalf>,
    w: OwnedWriteHalf,
    backlog: Vec<Value>,
}

impl Core {
    async fn read(&mut self) -> Value {
        let mut line = String::new();
        let n = tokio::time::timeout(Duration::from_secs(15), self.r.read_line(&mut line)).await.expect("provider message in time").unwrap();
        assert!(n > 0, "provider disconnected");
        serde_json::from_str(&line).unwrap()
    }
    async fn send(&mut self, v: Value) {
        self.w.write_all(format!("{v}\n").as_bytes()).await.unwrap();
    }
    async fn reply(&mut self, req: &Value, result: Value) {
        self.send(json!({"jsonrpc": "2.0", "id": req["id"], "result": result})).await;
    }
    async fn until(&mut self, mut pred: impl FnMut(&Value) -> bool) -> Value {
        if let Some(i) = self.backlog.iter().position(&mut pred) {
            return self.backlog.remove(i);
        }
        loop {
            let m = self.read().await;
            if pred(&m) {
                return m;
            }
            self.backlog.push(m);
        }
    }
}

struct Provider(std::process::Child);
impl Drop for Provider {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Start prim-herdr against a fresh fake core; answer hello and register. Returns the register
/// params and the sync's resources.
async fn start(hc: &Hc, tag: &str) -> (Core, Provider, Value, Vec<Value>) {
    let endpoint = hc.root.path().join(format!("core-{tag}.sock"));
    let listener = UnixListener::bind(&endpoint).unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_prim-herdr"));
    hc.env(&mut cmd);
    cmd.args(["--hc", herdr_core().to_str().unwrap(), "--name", "herdr@test"]).env("PRIM_SOCKET", &endpoint);
    let provider = Provider(cmd.spawn().unwrap());
    let (sock, _) = tokio::time::timeout(Duration::from_secs(15), listener.accept()).await.unwrap().unwrap();
    let (r, w) = sock.into_split();
    let mut core = Core { r: BufReader::new(r), w, backlog: Vec::new() };
    let hello = core.read().await;
    assert_eq!(hello["method"], "hello");
    core.reply(&hello, json!({})).await;
    let reg = core.read().await;
    assert_eq!(reg["method"], "provider.register");
    core.reply(&reg, json!({"provider": "herdr@test", "generation": 5, "limits": {"stream_window_bytes": 65536, "max_record_bytes": 1048576}})).await;
    let sync = core.read().await;
    assert_eq!((sync["method"].as_str(), sync["params"]["final"].as_bool()), (Some("provider.sync"), Some(true)));
    let resources = sync["params"]["resources"].as_array().unwrap().clone();
    core.reply(&sync, json!({})).await;
    (core, provider, reg["params"].clone(), resources)
}

fn data_of(m: &Value) -> Vec<u8> {
    B64.decode(m["params"]["data"].as_str().unwrap()).unwrap()
}

#[tokio::test]
#[ignore = "requires ~/.local/bin/herdr-core; private core"]
async fn spawn_attach_input_status_and_kill_on_a_private_herdr_core() {
    let hc = Hc::new();
    let (mut core, _provider, reg, resources) = start(&hc, "a").await;
    assert_eq!(reg["software"]["name"], "herdr-core");
    assert_eq!(reg["instance"].as_str(), hc.core_id().as_deref(), "instance is herdr-core's core.id");
    let caps = &reg["capabilities"];
    assert_eq!(caps["terminal.exit"]["exit_code"], true);
    assert_eq!(caps["terminal.spawn"]["env"], true);
    assert!(caps["remote.machines"].is_object() && caps["events.resync"].is_object());
    assert!(caps.as_object().unwrap().values().all(Value::is_object));
    assert!(resources.is_empty());

    core.send(json!({"jsonrpc": "2.0", "id": 1, "method": "effect.invoke", "params": {"generation": 5, "effect": "e1",
        "op": "terminal.spawn", "args": {"argv": ["/bin/sh"], "name": "spawned"}, "deadline_ms": 10000}})).await;
    let r = core.until(|m| m["id"] == 1).await;
    assert_eq!(r["result"]["outcome"], "observed", "{r}");
    let f = r["result"]["data"]["facts"].clone();
    let (term, inc) = (f["ref"].as_str().unwrap().to_string(), f["incarnation"].as_str().unwrap().to_string());
    assert!(!inc.is_empty(), "incarnation: the terminal's creation time");
    assert_eq!(f["name"], "spawned");
    core.until(|m| m["method"] == "resource.upsert" && m["params"]["facts"]["ref"] == term.as_str()).await;
    // env reaches the program.
    core.send(json!({"jsonrpc": "2.0", "id": 2, "method": "effect.invoke", "params": {"generation": 5, "effect": "e2",
        "op": "terminal.spawn", "args": {"argv": ["/bin/sh", "-c", "echo env=$PRIM_TEST > ../envcheck"], "cwd": hc.root.path().join("h").to_str().unwrap(),
        "env": {"PRIM_TEST": "passed"}}, "deadline_ms": 10000}})).await;
    assert_eq!(core.until(|m| m["id"] == 2).await["result"]["outcome"], "observed");
    let envcheck = hc.root.path().join("envcheck");
    for _ in 0..100 {
        if envcheck.exists() { break; }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(std::fs::read_to_string(&envcheck).unwrap().trim(), "env=passed");

    core.send(json!({"jsonrpc": "2.0", "id": 3, "method": "stream.open", "params": {"generation": 5, "stream": 1, "ref": term,
        "incarnation": inc, "cols": 100, "rows": 30, "history_lines": 0, "window_bytes": 65536}})).await;
    assert_eq!(core.until(|m| m["id"] == 3).await["result"]["initial"], "none");
    let input = "printf '\\033]7501;state=working:app=prim\\007'; echo hi-$((40+2))\r";
    core.send(json!({"jsonrpc": "2.0", "method": "stream.input", "params": {"generation": 5, "stream": 1, "data": B64.encode(input)}})).await;
    let mut seen = Vec::new();
    let mut offset = 0u64;
    while !seen.windows(5).any(|w| w == b"hi-42") {
        let d = core.until(|m| m["method"] == "stream.data").await;
        assert_eq!(d["params"]["offset"].as_u64(), Some(offset));
        offset += data_of(&d).len() as u64;
        seen.extend(data_of(&d));
        core.send(json!({"jsonrpc": "2.0", "method": "stream.credit", "params": {"generation": 5, "stream": 1, "bytes": data_of(&d).len()}})).await;
    }
    let s = core.until(|m| m["method"] == "resource.upsert" && m["params"]["facts"]["ref"] == term.as_str() && m["params"]["facts"]["status"].is_object()).await;
    eprintln!("status as published: {}", s["params"]["facts"]["status"]);

    core.send(json!({"jsonrpc": "2.0", "id": 4, "method": "effect.invoke", "params": {"generation": 5, "effect": "e3",
        "op": "terminal.kill", "ref": term, "incarnation": inc, "args": {}, "deadline_ms": 10000}})).await;
    let k = core.until(|m| m["id"] == 4).await;
    assert_eq!(k["result"]["outcome"], "observed", "{k}");
    assert_eq!((k["result"]["data"]["facts"]["state"].as_str(), k["result"]["data"]["facts"]["exit"]["reason"].as_str()), (Some("finished"), Some("killed")));
    core.until(|m| m["method"] == "resource.upsert" && m["params"]["facts"]["state"] == "finished").await;
    let end = core.until(|m| (m["method"] == "stream.closed" || m["method"] == "stream.gap") && m["params"]["stream"] == 1).await;
    assert_eq!((end["method"].as_str(), end["params"]["reason"]["reason"].as_str()), (Some("stream.closed"), Some("killed")), "{end}");
    // Wrong incarnation: not_found, no effect.
    core.send(json!({"jsonrpc": "2.0", "id": 5, "method": "effect.invoke", "params": {"generation": 5, "effect": "e4",
        "op": "terminal.kill", "ref": term, "incarnation": "0", "args": {}, "deadline_ms": 10000}})).await;
    assert_eq!(core.until(|m| m["id"] == 5).await["error"]["data"]["kind"], "not_found");
}

#[tokio::test]
#[ignore = "requires ~/.local/bin/herdr-core; private core"]
async fn provider_restart_keeps_instance_and_ids_and_a_wiped_core_is_a_new_instance() {
    let hc = Hc::new();
    let (mut core, provider, reg, _) = start(&hc, "a").await;
    let instance = reg["instance"].as_str().unwrap().to_string();
    core.send(json!({"jsonrpc": "2.0", "id": 1, "method": "effect.invoke", "params": {"generation": 5, "effect": "e1",
        "op": "terminal.spawn", "args": {"argv": ["/usr/bin/sleep", "600"]}, "deadline_ms": 10000}})).await;
    let f = core.until(|m| m["id"] == 1).await["result"]["data"]["facts"].clone();
    let (term, inc) = (f["ref"].as_str().unwrap().to_string(), f["incarnation"].as_str().unwrap().to_string());
    drop(provider);
    drop(core);

    // Restart: same herdr-core, same instance, same ref and incarnation.
    let (core, provider, reg, resources) = start(&hc, "b").await;
    assert_eq!(reg["instance"].as_str(), Some(instance.as_str()));
    let again = resources.iter().find(|r| r["ref"] == term.as_str()).expect("the terminal is listed again");
    assert_eq!(again["incarnation"].as_str(), Some(inc.as_str()));
    drop(provider);
    drop(core);

    // Wipe: stop the private core, remove its state; the next core is a new instance without it.
    hc.stop();
    std::fs::remove_dir_all(hc.root.path().join("s/herdr-core/p")).unwrap();
    let (_core, _provider, reg, resources) = start(&hc, "c").await;
    assert_ne!(reg["instance"].as_str(), Some(instance.as_str()), "a wiped core is a new instance");
    assert!(resources.iter().all(|r| r["ref"] != term.as_str() || r["incarnation"] != inc.as_str()));
}

#[tokio::test]
#[ignore = "requires ~/.local/bin/herdr-core; private core"]
async fn status_events_keep_flowing_past_a_kilobyte_of_events() {
    // herdr-core's events channel is credited ("JSON lines, with credit", SubscribeResult.channel) and the
    // adapter returns no events credit; regression guard that more than a kilobyte of events still arrives.
    let hc = Hc::new();
    let (mut core, _provider, _, _) = start(&hc, "a").await;
    core.send(json!({"jsonrpc": "2.0", "id": 1, "method": "effect.invoke", "params": {"generation": 5, "effect": "e1",
        "op": "terminal.spawn", "args": {"argv": ["/bin/sh"]}, "deadline_ms": 10000}})).await;
    let f = core.until(|m| m["id"] == 1).await["result"]["data"]["facts"].clone();
    let (term, inc) = (f["ref"].as_str().unwrap().to_string(), f["incarnation"].as_str().unwrap().to_string());
    core.send(json!({"jsonrpc": "2.0", "id": 2, "method": "stream.open", "params": {"generation": 5, "stream": 1, "ref": term,
        "incarnation": inc, "cols": 100, "rows": 30, "history_lines": 0, "window_bytes": 262144}})).await;
    core.until(|m| m["id"] == 2).await;
    for i in 0..12 {
        let report = format!("printf '\\033]7501;state=working:app=n{i}\\007'; echo step-{i}\r");
        core.send(json!({"jsonrpc": "2.0", "method": "stream.input", "params": {"generation": 5, "stream": 1, "data": B64.encode(&report)}})).await;
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
    core.until(|m| m["method"] == "resource.upsert" && m["params"]["facts"]["status"]["app"] == "n11").await;
}

#[tokio::test(flavor="multi_thread", worker_threads=2)]
#[ignore = "approved QA3_STATE_COPY input only; private herdr-core, source is never read here"]
async fn copied_realistic_state_upgrades_through_actual_defaults_and_provider_lookup() {
    let Ok(input)=std::env::var("QA3_STATE_COPY") else { return; };
    let hc=Hc::new(); let root=hc.root.path(); let state=root.join("copied-state"); std::fs::create_dir(&state).unwrap();
    let mut workspace:Value=serde_json::from_slice(&std::fs::read(Path::new(&input).join("workspace.json")).unwrap()).unwrap();
    let mut identities:Value=serde_json::from_slice(&std::fs::read(Path::new(&input).join("resources.json")).unwrap()).unwrap();
    // Preserve real layout shape/ids/history but never run copied declarations or user tools.
    for (key,entry) in workspace["entries"].as_object_mut().unwrap() { if key.starts_with("extension/") {entry["value"]["enabled"]=json!(false);} }
    let layouts:Vec<_>=workspace["entries"].as_object().unwrap().iter().filter(|(key,_)|key.starts_with("layout/")).map(|(key,value)|(key.clone(),value.clone())).collect();
    let mut argv=private_env(&hc);argv.extend([herdr_core().to_string_lossy().into_owned(),"connect".into()]);
    let native=prim::server::start(root.join("native.sock"),root.join("native-state"),false,prim::server::Backend::Herdr(argv),None).unwrap();
    let (old,_,_)=prim::client::connect(&root.join("native.sock"),"copy-upgrade").await.unwrap();
    for _ in 0..200 {if native.lock().unwrap().providers["herdr"].synced {break;}tokio::time::sleep(Duration::from_millis(20)).await;}
    let mut expected=Vec::new();
    for (id,record) in identities["ids"].as_object_mut().unwrap() {
        if record["retired"]==true {continue;}
        let resource=old.call("resource.spawn",json!({"argv":["/usr/bin/sleep","600"]})).await.unwrap()["resource"].clone();
        record["identity"]=json!({"provider":"herdr","instance":resource["instance"],"reference":resource["backend_ref"],"incarnation":resource["incarnation"]});expected.push(id.clone());
    }
    prim::server::shutdown(&native).await;
    std::fs::write(state.join("workspace.json"),workspace.to_string()).unwrap();std::fs::write(state.join("resources.json"),identities.to_string()).unwrap();
    let ext=root.join("extensions");std::fs::create_dir(&ext).unwrap();
    let mut defaults:Value=serde_json::from_slice(&std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("extensions/defaults.json")).unwrap()).unwrap();
    for item in defaults["extensions"].as_array_mut().unwrap() {if item["name"]!="herdr" {item["enabled"]=json!(false);}}
    std::fs::write(ext.join("defaults.json"),defaults.to_string()).unwrap();
    let bin=root.join("bin");std::fs::create_dir(&bin).unwrap();std::os::unix::fs::symlink(herdr_core(),bin.join("hc")).unwrap();
    let socket=root.join("new.sock");let mut cmd=Command::new(env!("CARGO_BIN_EXE_prim"));hc.env(&mut cmd);
    cmd.arg("serve").env("PATH",&bin).env("PRIM_STATE_DIR",&state).env("PRIM_SOCKET",&socket).env_remove("PRIM_SOCKET_SOURCE").env("PRIM_EXTENSIONS_DIR",&ext).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    let mut process=Provider(cmd.spawn().unwrap());
    let peer=tokio::time::timeout(Duration::from_secs(10),async{loop{if let Ok((p,_,_))=prim::client::connect(&socket,"copy-upgrade").await {break p;}tokio::time::sleep(Duration::from_millis(20)).await;}}).await.unwrap();
    let snapshot=tokio::time::timeout(Duration::from_secs(10),async{loop{let snap=peer.call("workspace.snapshot",Value::Null).await.unwrap();if expected.iter().all(|id|snap["resources"].as_array().unwrap().iter().any(|r|r["id"]==id.as_str()&&r["available"]==true)){break snap;}tokio::time::sleep(Duration::from_millis(20)).await;}}).await.unwrap();
    for (key,value) in layouts {assert_eq!(snapshot["entries"][&key],value,"copied layout/id structure changed");}
    assert_eq!(peer.call("resource.spawn",json!({"argv":["/usr/bin/sleep","600"]})).await.unwrap()["resource"]["provider"],"herdr");
    peer.call("core.shutdown",Value::Null).await.unwrap();
    for _ in 0..100 {if process.0.try_wait().unwrap().is_some(){break;}tokio::time::sleep(Duration::from_millis(20)).await;}
    println!("private realistic-copy upgrade passed: {} remapped identities; no source values logged",expected.len());
}

/// argv prefix that runs a program in this Hc's private environment (keeping PRIM_SOCKET and friends).
fn private_env(hc: &Hc) -> Vec<String> {
    let p = hc.root.path();
    ["/usr/bin/env".to_string(), format!("HOME={}", p.join("h").display()), format!("XDG_STATE_HOME={}", p.join("s").display()),
        format!("XDG_RUNTIME_DIR={}", p.join("r").display()), format!("XDG_CONFIG_HOME={}", p.join("c").display()),
        "HC_SESSION=p".into(), "PATH=".into()].to_vec()
}

#[tokio::test]
#[ignore = "requires ~/.local/bin/herdr-core; private core"]
async fn switching_a_state_dir_from_in_process_herdr_to_prim_herdr_keeps_resource_ids() {
    use prim::server::{start, Backend};
    let hc = Hc::new();
    let state = hc.root.path().join("prim-state");
    // 1. The old way: the in-process herdr adapter.
    let mut argv = private_env(&hc);
    argv.extend([herdr_core().to_string_lossy().into_owned(), "connect".into()]);
    let sock_a = hc.root.path().join("a.sock");
    let core_a = start(sock_a.clone(), state.clone(), false, Backend::Herdr(argv), None).unwrap();
    let (a, _, _) = prim::client::connect(&sock_a, "t").await.unwrap();
    let synced = |c: &prim::core::Shared| c.lock().unwrap().snapshot()["providers"]["herdr"]["synced"] == true;
    for _ in 0..200 { if synced(&core_a) { break; } tokio::time::sleep(Duration::from_millis(50)).await; }
    let r = a.call("resource.spawn", json!({"argv": ["/usr/bin/sleep", "600"]})).await.unwrap();
    let rid = r["resource"]["id"].as_str().unwrap().to_string();
    let (reference, instance) = (r["resource"]["backend_ref"].clone(), r["resource"]["instance"].clone());
    a.call("command.invoke", json!({"name": "prim.layout.reset"})).await.unwrap();
    prim::server::shutdown(&core_a).await;

    // 2. The new way: same state dir, no in-process backend, prim-herdr as the `herdr` extension.
    let sock_b = hc.root.path().join("b.sock");
    let core_b = start(sock_b.clone(), state, false, Backend::Extensions, None).unwrap();
    let (b, _, _) = prim::client::connect(&sock_b, "t").await.unwrap();
    let mut ext = private_env(&hc);
    ext.extend([env!("CARGO_BIN_EXE_prim-herdr").to_string(), "--hc".into(), herdr_core().to_string_lossy().into_owned(), "--name".into(), "herdr".into()]);
    b.call("workspace.apply", json!({"ops": [{"op": "set", "key": "extension/herdr", "value": {"argv": ext, "kind": "provider", "default": true}}]})).await.unwrap();
    let mut back = Value::Null;
    for _ in 0..200 {
        let snap = b.call("workspace.snapshot", Value::Null).await.unwrap();
        if let Some(res) = snap["resources"].as_array().unwrap().iter().find(|x| x["id"] == rid.as_str() && x["available"] == true) {
            back = res.clone();
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(back["id"], rid.as_str(), "the same prim id comes back through prim-herdr");
    assert_eq!((&back["backend_ref"], &back["instance"], back["provider"].as_str()), (&reference, &instance, Some("herdr")));
    // The tab made by layout reset still points at it.
    let layout = b.call("workspace.snapshot", Value::Null).await.unwrap()["entries"]["view/term-".to_string() + &rid].clone();
    assert_eq!(layout["value"]["resource"], rid.as_str());
    // New spawns without a provider go to prim-herdr (the default provider extension).
    let fresh = b.call("resource.spawn", json!({"argv": ["/usr/bin/sleep", "600"]})).await.unwrap();
    assert_eq!(fresh["resource"]["provider"], "herdr");
    prim::server::shutdown(&core_b).await;
}
