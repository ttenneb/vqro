//! The real adapter protocol over a disposable stdio fixture, never hc or a live server.
use std::time::Duration;
use vqro::adapter::{herdr, StreamInput};

#[tokio::test]
async fn herdr_credit_is_withheld_until_downstream_handoff() {
    let dir = tempfile::tempdir().unwrap();
    let receipt = dir.path().join("credit.bin");
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/credit_backend.py");
    let (events, _rx) = tokio::sync::mpsc::unbounded_channel();
    let adapter = herdr::start("credit-test".into(), vec!["python3".into(), fixture.to_string_lossy().into(), receipt.to_string_lossy().into()], events);
    let mut stream = tokio::time::timeout(Duration::from_secs(2), adapter.attach("t1".into(), 80, 24, 0)).await.unwrap().unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(!receipt.exists(), "enqueueing backend bytes must not return credit");
    let item = tokio::time::timeout(Duration::from_secs(2), stream.output.recv()).await.unwrap().unwrap();
    assert!(!receipt.exists(), "dequeue alone is not downstream acceptance");
    StreamInput::Consumed(3).send(&stream.input).unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if std::fs::read(&receipt).ok().is_some_and(|b| b == 3u32.to_le_bytes()) { break; }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }).await.unwrap();
    drop(item);
    StreamInput::Close.send(&stream.input).unwrap();
    drop(stream);
    drop(adapter);
}
