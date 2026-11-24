//! Synthetic wait-result provenance, not a claim that an OS actually returned wait error.
use super::*;
struct CodecStub(&'static str);
impl Codec for CodecStub {
    fn name(&self) -> &'static str {
        self.0
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
fn fixture(name: &'static str) -> (Process, Arc<Mutex<Vec<Event>>>) {
    let (writes, _) = mpsc::channel(1);
    let (stop, _) = watch::channel(false);
    let (exit, _) = watch::channel(None);
    let (drained, _) = watch::channel(None);
    let events = Arc::new(Mutex::new(Vec::new()));
    let captured = events.clone();
    (
        Process {
            inner: Arc::new(Inner {
                pending: Mutex::new(HashMap::new()),
                writes,
                budget: Arc::new(Semaphore::new(64)),
                serial: AtomicU64::new(1),
                dead: AtomicBool::new(false),
                broken: AtomicBool::new(false),
                stopping: AtomicBool::new(false),
                stop,
                exit,
                drained,
                stderr: Mutex::new(VecDeque::new()),
                stderr_total: AtomicU64::new(0),
                observe: Arc::new(move |event| captured.lock().unwrap().push(event)),
                codec: Arc::new(CodecStub(name)),
            }),
        },
        events,
    )
}
#[tokio::test]
async fn pi_and_claude_watch_and_observation_preserve_wait_error_provenance() {
    for name in ["Pi", "Claude"] {
        let (process, events) = fixture(name);
        publish_exit(
            &process.inner,
            Err(std::io::Error::other("synthetic wait error")),
        );
        let exit = process.exited().await;
        assert!(
            !exit.observed,
            "{name}: a resolved watch is not exit evidence"
        );
        assert_eq!(exit.code, None);
        assert!(!exit.success);
        assert!(
            !process.alive(),
            "legacy dead behaviour preserved but is NOT permission to replace"
        );
        let events = events.lock().unwrap();
        assert!(matches!(&events[0],Event::Exited(exit) if !exit.observed));
    }
}
#[cfg(unix)]
#[tokio::test]
async fn successful_wait_codes_and_signals_are_observed_for_both_callers() {
    use std::os::unix::process::ExitStatusExt;
    for name in ["Pi", "Claude"] {
        for raw in [0, 9] {
            let (process, events) = fixture(name);
            publish_exit(&process.inner, Ok(std::process::ExitStatus::from_raw(raw)));
            let exit = process.exited().await;
            assert!(exit.observed);
            assert_eq!(exit.success, raw == 0);
            assert_eq!(exit.code, if raw == 0 { Some(0) } else { None });
            assert!(matches!(&events.lock().unwrap()[0],Event::Exited(exit) if exit.observed));
        }
    }
}
#[cfg(unix)]
#[test]
fn wait_error_cannot_equal_an_observed_signal_exit() {
    use std::os::unix::process::ExitStatusExt;
    let signal = exit_from_wait(Ok(std::process::ExitStatus::from_raw(9)));
    let unknown = exit_from_wait(Err(std::io::Error::other("synthetic wait error")));
    assert_eq!(signal.code, None);
    assert!(!signal.success);
    assert!(signal.observed);
    assert_eq!(unknown.code, None);
    assert!(!unknown.success);
    assert!(!unknown.observed);
    assert_ne!(
        format!("{signal:?}"),
        format!("{unknown:?}"),
        "WAIT ERROR and successful signal wait must carry distinct exit-observation provenance"
    );
}
