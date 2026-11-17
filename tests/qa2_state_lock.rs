use std::time::Duration;

#[tokio::test]
async fn tui_ensure_core_reports_held_state_lock_without_a_spawn_retry_loop() {
    let root=tempfile::Builder::new().prefix("psf").tempdir_in("/tmp").unwrap();
    let state=root.path().join("state");std::fs::create_dir(&state).unwrap();
    let guard=std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(state.join(".workspace.lock")).unwrap();
    guard.try_lock().unwrap();
    let mut cmd=tokio::process::Command::new(env!("CARGO_BIN_EXE_prim"));
    cmd.arg("tui").env_clear().env("HOME",root.path()).env("PATH","/usr/bin:/bin").env("PRIM_SOCKET",root.path().join("s")).env("PRIM_STATE_DIR",&state).kill_on_drop(true);
    let output=tokio::time::timeout(Duration::from_secs(2),cmd.output()).await.expect("held lock must fail immediately, not loop spawning/waiting").unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("workspace state still in use"),"{}",String::from_utf8_lossy(&output.stderr));
    assert!(!state.join("serve.log").exists(),"held-state notice must precede spawning another core");
}
