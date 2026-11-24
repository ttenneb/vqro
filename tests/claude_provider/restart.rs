//! Real owned Python peers and fake helper, in-memory core. No Claude/model/auth/keychain access.
use super::*;
use std::path::PathBuf;
use vqro::claude_code::restart::Config as RestartConfig;
#[path = "restart_guards.rs"]
mod guards;
#[path = "work.rs"]
mod work;
struct NativeGuard(Arc<Shared>);
impl Drop for NativeGuard {
    fn drop(&mut self) {
        if let Some(native) = self.0.process() {
            native.stop();
        }
    }
}
async fn fixture(
    mode: &str,
    helper: bool,
) -> (
    tempfile::TempDir,
    Arc<Shared>,
    NativeGuard,
    Provider,
    UnboundedReceiver<Request>,
    Core,
    PathBuf,
    PathBuf,
) {
    let root = tempfile::tempdir().unwrap();
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let log = root.path().join("native.jsonl");
    let helpers = root.path().join("helper.jsonl");
    let native = vec![
        "/usr/bin/python3".into(),
        base.join("tests/fixtures/fake_claude_stream.py")
            .to_string_lossy()
            .into_owned(),
    ];
    let mut env = HashMap::from([
        ("HOME".into(), root.path().to_string_lossy().into_owned()),
        ("PATH".into(), "/usr/bin:/bin".into()),
        ("FAKE_CLAUDE_LOG".into(), log.to_string_lossy().into_owned()),
        (
            "FAKE_RESTART_LOG".into(),
            helpers.to_string_lossy().into_owned(),
        ),
        ("FAKE_RESTART_SESSION".into(), mode.into()),
    ]);
    if mode == "refuse" {
        env.insert("FAKE_RESTART_LAUNCH".into(), "refuse".into());
    }
    if mode == "deny" {
        env.insert("FAKE_RESTART_PREFLIGHT".into(), "deny".into());
    }
    let shared = Shared::new(
        "restart-boot".into(),
        "claude-fixture".into(),
        "fixture-instance".into(),
    );
    shared.state.lock().unwrap().account =
        Some(json!({"id":"A","kind":"subscription","provider":"anthropic"}));
    let guard = NativeGuard(shared.clone());
    if helper {
        let path = if mode == "missing-helper" {
            root.path().join("ABSENT.py")
        } else {
            base.join("tests/fixtures/fake_claude_restart_helper.py")
        };
        shared
            .configure_restart(RestartConfig {
                helper: vec![
                    "/usr/bin/python3".into(),
                    path.to_string_lossy().into_owned(),
                ],
                native: native.clone(),
                cwd: root.path().to_path_buf(),
                environment: env.clone(),
            })
            .unwrap();
    }
    let process = shared
        .spawn_native(&native, root.path(), &env)
        .await
        .unwrap();
    shared.clone().initialize().await;
    // One synthetic SOURCE turn establishes native session + terminal subscription episode.
    // It is not a model invocation and no user input is sent to the replacement process.
    process.notify("user",json!({"uuid":"source-turn","client_composed":true,"priority":"later","message":{"role":"user","content":[{"type":"text","text":"[[limit]]"}]}}),tokio::time::Instant::now()+Duration::from_secs(3)).await.unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let changed = shared.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let ready = {
                let s = shared.state.lock().unwrap();
                s.foreground == "idle"
                    && s.availability.limit_episode.is_some()
                    && s.background_known_empty()
            };
            if ready {
                break;
            }
            changed.await;
        }
    })
    .await
    .unwrap();
    let (provider, requests, core) = registered(&shared).await;
    (root, shared, guard, provider, requests, core, log, helpers)
}
fn args(shared: &Shared) -> Value {
    let s = shared.state.lock().unwrap();
    json!({"v":0,"epoch":s.epoch(),"session":s.session,"account":"A","episode":s.availability.limit_episode.as_ref().unwrap()["episode"],"target":{"account":"B","ref":"0123456789abcdef0123456789abcdef","host":"fixture-backend","revision":3}})
}
fn lines(path: &std::path::Path) -> Vec<Value> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect()
}
async fn stop(shared: &Shared) {
    if let Some(n) = shared.process() {
        n.stop();
        tokio::time::timeout(Duration::from_secs(3), n.exited())
            .await
            .unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn legacy_per_agent_credential_host_never_dispatches_helper_or_stop() {
    let (_root, shared, _guard, provider, mut requests, mut core, _log, helpers) = fixture("same", true).await;
    let old = shared.process().unwrap();
    let mut before = args(&shared);
    before["target"]["host"] = json!("r/fixture");
    let reply = core.invoke(&shared, &provider, &mut requests, "claude.restart", before.clone()).await;
    assert_ne!(reply["result"]["data"]["started"], true);
    assert!(old.alive());
    assert_eq!(shared.state.lock().unwrap().epoch(), before["epoch"].as_str().unwrap());
    assert!(lines(&helpers).is_empty());
    stop(&shared).await;
    provider.close();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn restart_confirms_native_session_without_replaying_input() {
    let (_root, shared, _guard, provider, mut requests, mut core, log, helpers) =
        fixture("same", true).await;
    let old = shared.process().unwrap();
    let before = args(&shared);
    let reply = core
        .invoke(
            &shared,
            &provider,
            &mut requests,
            "claude.restart",
            before.clone(),
        )
        .await;
    assert_eq!(reply["result"]["data"]["started"], true, "{reply}");
    assert_eq!(reply["result"]["data"]["resume_confirmed"], true, "{reply}");
    assert_eq!(reply["result"]["data"]["session"], before["session"]);
    assert_ne!(reply["result"]["data"]["epoch"], before["epoch"]);
    assert_eq!(reply["result"]["data"]["account"], "B");
    assert_eq!(shared.facts()["data"]["agent"]["_claude"]["credential_binding"],
        json!({"account":"B","host":"fixture-backend","ref":"0123456789abcdef0123456789abcdef","epoch":reply["result"]["data"]["epoch"]}));
    assert!(!old.alive());
    let h = lines(&helpers);
    assert_eq!(h.len(), 2);
    assert_eq!(h[0]["action"], "check");
    assert_eq!(h[1]["action"], "launch");
    let argv = h[1]["runtime"].as_array().unwrap();
    assert_eq!(argv[argv.len() - 2], "--resume");
    assert_eq!(argv.last().unwrap(), &before["session"]);
    assert_eq!(
        lines(&log)
            .iter()
            .filter(|v| v["in"]["type"] == "user")
            .count(),
        1,
        "replacement must receive metadata ONLY, no user replay/continuation"
    );
    let stale = core
        .invoke(&shared, &provider, &mut requests, "claude.restart", before)
        .await;
    assert_ne!(stale["result"]["outcome"], "observed");
    assert_eq!(
        lines(&helpers).len(),
        2,
        "stale request cannot launch again"
    );
    stop(&shared).await;
    provider.close();
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn restart_healthy_without_native_session_is_explicitly_unconfirmed() {
    let (_root, shared, _guard, provider, mut requests, mut core, _log, _helpers) =
        fixture("missing", true).await;
    let reply = core
        .invoke(
            &shared,
            &provider,
            &mut requests,
            "claude.restart",
            args(&shared),
        )
        .await;
    assert_eq!(reply["result"]["data"]["started"], true, "{reply}");
    assert_eq!(
        reply["result"]["data"]["resume_confirmed"], false,
        "{reply}"
    );
    assert!(reply["result"]["data"].get("session").is_none());
    assert!(
        shared.state.lock().unwrap().session.is_none(),
        "requested --resume is NOT native readback"
    );
    stop(&shared).await;
    provider.close();
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn restart_wrong_session_or_refused_start_is_not_success_and_never_retries() {
    for mode in ["wrong", "refuse"] {
        let (_root, shared, _guard, provider, mut requests, mut core, _log, helpers) =
            fixture(mode, true).await;
        let reply = core
            .invoke(
                &shared,
                &provider,
                &mut requests,
                "claude.restart",
                args(&shared),
            )
            .await;
        assert!(reply.get("error").is_some(), "{mode}: {reply}");
        assert_eq!(
            lines(&helpers)
                .iter()
                .filter(|v| v["action"] == "launch")
                .count(),
            1
        );
        stop(&shared).await;
        provider.close();
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn restart_missing_helper_does_not_stop_old_process() {
    for mode in ["omitted", "missing-helper", "deny"] {
        let (_root, shared, _guard, provider, mut requests, mut core, _log, helpers) =
            fixture(mode, mode != "omitted").await;
        let old = shared.process().unwrap();
        let before = args(&shared);
        if mode == "omitted" {
            assert!(
                shared.facts()["data"]["agent"]["caps"]
                    .get("claude.restart")
                    .is_none()
            );
            assert!(old.alive());
            stop(&shared).await;
            provider.close();
            continue;
        }
        let reply = core
            .invoke(
                &shared,
                &provider,
                &mut requests,
                "claude.restart",
                before.clone(),
            )
            .await;
        assert_ne!(reply["result"]["data"]["started"], true, "{mode}: {reply}");
        assert!(old.alive(), "preflight failure must NOT stop A");
        assert_eq!(
            shared.state.lock().unwrap().epoch(),
            before["epoch"].as_str().unwrap()
        );
        assert!(!lines(&helpers).iter().any(|v| v["action"] == "launch"));
        stop(&shared).await;
        provider.close();
    }
}
