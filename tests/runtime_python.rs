//! A private core must skip an old python3 without inheriting a live backend endpoint.
#![cfg(unix)]
use serde_json::{json, Value};
use std::{os::unix::fs::symlink, process::{Child, Command, Stdio}, time::Duration};
use vqro::client;
struct Owned(Child);
impl Drop for Owned { fn drop(&mut self) { let _ = self.0.kill(); let _ = self.0.wait(); } }

fn write_executable(path: &std::path::Path, script: &str) {
    // Only a short-lived child opens the executable for writing, so parallel test
    // forks from this process cannot inherit that fd and cause ETXTBSY on exec.
    let mut writer = Command::new("/bin/sh")
        .args(["-c", "cat > \"$1\" && chmod 700 \"$1\"", "sh"]).arg(path)
        .stdin(Stdio::piped()).spawn().unwrap();
    {
        use std::io::Write;
        writer.stdin.take().unwrap().write_all(script.as_bytes()).unwrap();
    }
    assert!(writer.wait().unwrap().success());
}

#[tokio::test]
async fn private_core_skips_old_python_and_starts_extension() {
    let root = tempfile::tempdir().unwrap();
    let old = root.path().join("old");
    let good = root.path().join("good");
    std::fs::create_dir_all(&old).unwrap();
    std::fs::create_dir_all(&good).unwrap();
    std::fs::create_dir_all(root.path().join("extensions")).unwrap();
    let real = vqro::program::python().unwrap().0;
    // Resolve a literal override (if any) before constructing the private PATH.
    let real = if real.components().count() == 1 {
        std::env::split_paths(&std::env::var_os("PATH").unwrap()).map(|d| d.join(&real)).find(|p| p.is_file()).unwrap()
    } else { real };
    write_executable(&old.join("python3"), "#!/bin/sh\necho '(3, 9)'\n");
    symlink(real, good.join("python3")).unwrap();
    let path = std::env::join_paths([old, good]).unwrap();
    let socket = root.path().join("p");
    let mut core = Owned(Command::new(env!("CARGO_BIN_EXE_vqro")).args(["serve", "--fake"])
        .env_clear().env("HOME", root.path()).env("PATH", path)
        .env("VQRO_SOCKET", &socket).env("VQRO_STATE_DIR", root.path().join("st"))
        .env("VQRO_EXTENSIONS_DIR", root.path().join("extensions"))
        .env("VQRO_HERDR", "[\"/bin/false\"]")
        .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap());
    let (peer, _, _) = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(connection) = client::connect(&socket, "runtime-python-test").await { break connection; }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }).await.unwrap();
    let marker = root.path().join("started");
    peer.call("workspace.apply", json!({"ops":[{"op":"set","key":"extension/python-control","value":{
        "argv":["python3","-c","import pathlib,sys,time; pathlib.Path(sys.argv[1]).write_text('ready'); time.sleep(30)",marker]}}]})).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !marker.exists() { tokio::time::sleep(Duration::from_millis(20)).await; }
    }).await.unwrap();
    assert_eq!(peer.call("extension.list", Value::Null).await.unwrap()["extensions"]["python-control"]["status"], "running");
    peer.call("core.shutdown", Value::Null).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while core.0.try_wait().unwrap().is_none() { tokio::time::sleep(Duration::from_millis(20)).await; }
    }).await.unwrap();
}

#[tokio::test]
async fn slow_discovery_keeps_core_responsive_and_launches_only_current_generation() {
    let root = tempfile::tempdir().unwrap();
    let slow = root.path().join("slow");
    let good = root.path().join("good");
    let other = root.path().join("other-cwd");
    for dir in [&slow, &good, &other, &root.path().join("extensions")] { std::fs::create_dir_all(dir).unwrap(); }
    let real = vqro::program::python().unwrap().0;
    let probe_started = root.path().join("probing");
    write_executable(&slow.join("python3"), &format!("#!/bin/sh\necho started > '{}'\n/bin/sleep 1.5\necho '(3, 9)'\n", probe_started.display()));
    symlink(real, good.join("python3")).unwrap();
    let socket = root.path().join("p");
    // Both PATH entries are relative to the core cwd; the extension has a different cwd.
    let mut core = Owned(Command::new(env!("CARGO_BIN_EXE_vqro")).args(["serve", "--fake"])
        .current_dir(root.path()).env_clear().env("HOME", root.path()).env("PATH", "slow:good")
        .env("VQRO_SOCKET", &socket).env("VQRO_STATE_DIR", root.path().join("st"))
        .env("VQRO_EXTENSIONS_DIR", root.path().join("extensions")).env("VQRO_HERDR", "[\"/bin/false\"]")
        .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap());
    let (peer, _, _) = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(connection) = client::connect(&socket, "runtime-python-test").await { break connection; }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.unwrap();
    let stale = root.path().join("stale-launch");
    let current = root.path().join("current-launch");
    let declaration = |marker: &std::path::Path| json!({"ops":[{"op":"set","key":"extension/python-control","value":{
        "cwd":other,"argv":["python3","-c","import pathlib,sys,time; pathlib.Path(sys.argv[1]).write_text('ready'); time.sleep(30)",marker]}}]});
    peer.call("workspace.apply", declaration(&stale)).await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while !probe_started.exists() { tokio::time::sleep(Duration::from_millis(10)).await; }
    }).await.unwrap();
    let snapshot = tokio::time::timeout(Duration::from_millis(300), peer.call("workspace.snapshot", Value::Null)).await
        .expect("a slow interpreter probe must not hold the core mutex").unwrap();
    assert_eq!(snapshot["extensions"]["python-control"]["status"], "starting");
    let generation = snapshot["extensions"]["python-control"]["generation"].as_u64().unwrap();
    tokio::time::timeout(Duration::from_millis(300), peer.call("workspace.apply", declaration(&current))).await
        .expect("a probe must not delay current-declaration commits").unwrap();
    peer.call("command.invoke", json!({"name":"vqro.extension.restart","args":{"name":"python-control"}})).await.unwrap();
    let state = peer.call("extension.list", Value::Null).await.unwrap();
    assert!(state["extensions"]["python-control"]["generation"].as_u64().unwrap() > generation);
    tokio::time::timeout(Duration::from_secs(5), async {
        while !current.exists() { tokio::time::sleep(Duration::from_millis(20)).await; }
    }).await.unwrap();
    assert!(!stale.exists(), "no stale declaration may launch after discovery");
    assert_eq!(peer.call("extension.list", Value::Null).await.unwrap()["extensions"]["python-control"]["status"], "running");
    peer.call("core.shutdown", Value::Null).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while core.0.try_wait().unwrap().is_none() { tokio::time::sleep(Duration::from_millis(20)).await; }
    }).await.unwrap();
}
