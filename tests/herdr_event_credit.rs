//! Finite-window NDJSON split/coalescing, snapshot fencing and consumed-credit regression.
//! Real hc under-load convergence is covered by e2e_herdr_drift.py.
use std::time::Duration;
use prim::server::{self,Backend};
#[tokio::test(flavor="multi_thread",worker_threads=4)]
async fn event_credit_waits_for_core_consumption_and_advances_fixture() {
    let dir=tempfile::Builder::new().prefix("pe").tempdir_in("/tmp").unwrap();
    let script=std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/event_credit_backend.py");
    let core=server::start(dir.path().join("s"),dir.path().join("st"),false,Backend::Herdr(vec!["python3".into(),script.to_string_lossy().into(),dir.path().to_string_lossy().into()]),None).unwrap();
    let guard=core.lock().unwrap();
    tokio::time::timeout(Duration::from_secs(2),async { while !dir.path().join("first-sent").exists() { tokio::time::sleep(Duration::from_millis(5)).await; } }).await.unwrap();
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert!(!dir.path().join("credits").exists(),"enqueue alone must not return metadata credit");
    drop(guard);
    tokio::time::timeout(Duration::from_secs(2),async {
        while !dir.path().join("all-consumed").exists() { tokio::time::sleep(Duration::from_millis(5)).await; }
    }).await.unwrap();
    assert!(std::fs::read_to_string(dir.path().join("credits")).unwrap().lines().count()>=2);
    let snapshot=core.lock().unwrap().snapshot();
    assert_eq!(snapshot["resources"][0]["state"],"finished");
    assert_eq!(snapshot["resources"][0]["name"],"snapshot","queued pre-snapshot created must not resurrect the terminal");
    assert_eq!(snapshot["resources"][0]["exit"]["exit_code"],0);
    server::shutdown(&core).await;
}
