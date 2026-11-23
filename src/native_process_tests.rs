//! Memory-only controls for the shared writer's actual commit boundary and stdout drain outcome.
#[path = "native_process_automatic_tests.rs"]
mod automatic;
use super::*;
struct CodecStub;
impl Codec for CodecStub {
    fn name(&self) -> &'static str {
        "synthetic"
    }
    fn request(&self, _: &str, _: &str, args: Value) -> Result<Value, String> {
        Ok(args)
    }
    fn reply_id(&self, _: &Value) -> Result<Option<String>, String> {
        Ok(None)
    }
    fn reply(&self, _: &Value, _: &str) -> Result<Reply, String> {
        unreachable!()
    }
}
fn inner() -> (Arc<Inner>, mpsc::Receiver<Write>) {
    let (writes, receiver) = mpsc::channel(4);
    let (stop, _) = watch::channel(false);
    let (exit, _) = watch::channel(Some(Exit {
        code: Some(0),
        success: true,
    }));
    let (drained, _) = watch::channel(None);
    (
        Arc::new(Inner {
            pending: Mutex::new(HashMap::new()),
            writes,
            budget: Arc::new(Semaphore::new(64)),
            serial: AtomicU64::new(1),
            dead: AtomicBool::new(true),
            broken: AtomicBool::new(false),
            stopping: AtomicBool::new(false),
            stop,
            exit,
            drained,
            stderr: Mutex::new(VecDeque::new()),
            stderr_total: AtomicU64::new(0),
            observe: Arc::new(|_| {}),
            codec: Arc::new(CodecStub),
        }),
        receiver,
    )
}
#[tokio::test]
async fn queued_revoked_record_never_reaches_actual_writer_and_committed_record_cannot_rearm() {
    for revoke in [true, false] {
        let (state, receiver) = inner();
        state.exit.send_replace(None);
        state.dead.store(false, Ordering::SeqCst);
        let (a, mut b) = tokio::io::duplex(1024);
        let token = WriteToken::new();
        let permit = state.budget.clone().acquire_many_owned(64).await.unwrap();
        state
            .writes
            .try_send(Write {
                id: None,
                line: b"consent\n".to_vec(),
                deadline: Instant::now() + Duration::from_secs(3),
                phase: token.phase.clone(),
                _permit: permit,
            })
            .unwrap_or_else(|_| panic!("fixture queue full"));
        // It is already QUEUED, but the real writer has not committed it. Withdrawal wins atomically.
        if revoke {
            assert!(token.cancel_before_write());
        }
        let writer = tokio::spawn(write_native(a, receiver, state.clone()));
        let _released = tokio::time::timeout(
            Duration::from_secs(2),
            state.budget.clone().acquire_many_owned(64),
        )
        .await
        .unwrap()
        .unwrap();
        if !revoke {
            assert!(token.write_begun());
            assert!(
                !token.cancel_before_write(),
                "a committed write cannot become no-effect"
            );
        }
        writer.abort();
        let _ = writer.await;
        let mut bytes = Vec::new();
        b.read_to_end(&mut bytes).await.unwrap();
        assert_eq!(
            bytes,
            if revoke {
                Vec::new()
            } else {
                b"consent\n".to_vec()
            }
        );
    }
}
#[tokio::test]
async fn post_exit_cutoff_is_not_eof_even_with_partial_record_held_open() {
    let (state, _) = inner();
    let (a, mut b) = tokio::io::duplex(64);
    b.write_all(b"{\"type\":").await.unwrap();
    let process = Process {
        inner: state.clone(),
    };
    let reader = tokio::spawn(read_native(a, state));
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(3), process.drained())
            .await
            .unwrap(),
        Drain::Cutoff
    );
    reader.await.unwrap();
    drop(b);
}
#[tokio::test]
async fn real_eof_and_protocol_failure_are_distinct_drain_outcomes() {
    for (valid, expected) in [(true, Drain::Eof), (false, Drain::Protocol)] {
        let (state, _) = inner();
        let (a, mut b) = tokio::io::duplex(64);
        b.write_all(if valid {
            b"{\"type\":\"notice\"}\n"
        } else {
            b"not-json\n"
        })
        .await
        .unwrap();
        drop(b);
        let process = Process {
            inner: state.clone(),
        };
        let reader = tokio::spawn(read_native(a, state));
        assert_eq!(process.drained().await, expected);
        reader.await.unwrap();
    }
}
