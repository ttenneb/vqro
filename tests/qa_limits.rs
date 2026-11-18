//! Admission limits leave old state recoverable and reject new growth atomically.
use vqro::{client, limits, server::{self, Backend}, store::{Store, Op}};
use serde_json::{json, Value};

fn set(key: &str, value: Value) -> Op { Op::Set { key:key.into(), value, expect:None } }

#[test]
fn workspace_entry_and_total_limits_reject_without_adopting() {
    let mut store = Store::in_memory();
    let first = store.apply("qa", None, vec![set("doc/small", json!(1))]).unwrap();
    let error = store.apply("qa", None, vec![set("doc/big", json!("x".repeat(1024*1024)))]).unwrap_err();
    assert_eq!(error.kind(), "too_large");
    assert!(store.get("doc/big").is_none());
    assert_eq!(store.history(10).len(), 1);
    let ops = (0..18).map(|i| set(&format!("doc/total/{i}"), json!("x".repeat(950*1024)))).collect();
    assert_eq!(store.apply("qa", None, ops).unwrap_err().kind(), "too_large");
    assert_eq!(store.entries().len(), 1);
    assert_eq!(store.apply("qa", None, vec![set("doc/small", json!(2))]).unwrap().id, first.id+1);
}

#[test]
fn oversized_legacy_state_still_loads_and_can_be_shrunk_or_deleted() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("workspace.json");
    let saved = json!({"rev":1,"next_txn":2,"entries":{"doc/legacy":{"version":1,"value":"x".repeat(limits::ENTRY_BYTES+100)}},"history":[]});
    std::fs::write(&path, saved.to_string()).unwrap();
    let mut store = Store::open(path).unwrap();
    assert!(store.admission_notice().is_some());
    store.apply("qa", None, vec![set("doc/legacy", json!("x".repeat(limits::ENTRY_BYTES+10)))]).unwrap();
    store.apply("qa", None, vec![Op::Delete { key:"doc/legacy".into(), expect:None }]).unwrap();
    assert!(store.entries().is_empty());
    assert!(store.admission_notice().is_none());
}

#[tokio::test]
async fn bounded_python_helper_still_consumes_real_rpc_notifications() {
    let dir = tempfile::Builder::new().prefix("ph").tempdir_in("/tmp").unwrap();
    let socket = dir.path().join("s");
    let _core = server::start(socket.clone(), dir.path().join("state"), false, Backend::Fake, None).unwrap();
    let script = r#"from vqro_ext import Extension
import threading
ext = Extension('qa-smoke')
seen = threading.Event()
ext.on_event(lambda kind, data: seen.set() if kind == 'entries' else None)
ext.vqro.call('events.subscribe', {'epoch':ext.epoch,'from':ext.seq})
ext.apply([{'op':'set','key':'doc/qa-helper','value':1}], 'private helper smoke')
assert seen.wait(2)
assert ext.value('doc/qa-helper') == 1
print('ok')
"#;
    let mut cmd = tokio::process::Command::new("/usr/bin/python3");
    cmd.args(["-B", "-c", script]).env_clear().env("HOME", dir.path()).env("PATH", "/usr/bin:/bin")
        .env("VQRO_SOCKET", &socket).env("PYTHONPATH", std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("extensions")).kill_on_drop(true);
    let output = tokio::time::timeout(std::time::Duration::from_secs(5), cmd.output()).await.unwrap().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(output.stdout, b"ok\n");
}

#[tokio::test]
async fn live_content_limits_reject_growth_and_allow_clearing() {
    let dir = tempfile::Builder::new().prefix("pl").tempdir_in("/tmp").unwrap();
    let socket = dir.path().join("s");
    let core = server::start(socket.clone(), dir.path().join("state"), false, Backend::Fake, None).unwrap();
    let (peer, _, _) = client::connect(&socket, "qa").await.unwrap();
    let error = peer.call("view.publish", json!({"view":"big","content":{"text":"x".repeat(limits::LIVE_VALUE_BYTES)}})).await.unwrap_err();
    assert_eq!(error.kind(), "too_large");
    assert!(core.lock().unwrap().live.is_empty());
    for i in 0..9 {
        peer.call("view.publish", json!({"view":format!("v{i}"),"content":{"text":"x".repeat(900*1024)}})).await.unwrap();
    }
    let before = core.lock().unwrap().seq;
    assert_eq!(peer.call("view.publish", json!({"view":"excess","content":{"text":"x".repeat(900*1024)}})).await.unwrap_err().kind(), "too_large");
    assert_eq!(core.lock().unwrap().seq, before);
    for i in 0..9 { peer.call("view.publish", json!({"view":format!("v{i}"),"content":null})).await.unwrap(); }
    assert!(core.lock().unwrap().live.is_empty());
    assert!(!peer.is_closed());
}
