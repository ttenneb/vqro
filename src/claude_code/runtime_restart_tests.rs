//! Generation reducer controls, not a native-process or account qualification.
use super::*;
use crate::native_process::{Event, Exit};
#[test]
fn old_process_observations_cannot_mutate_new_epoch() {
    let shared = Shared::new("boot".into(), "fixture".into(), "instance".into());
    {
        let mut s = shared.state.lock().unwrap();
        shared.generation.store(2, Ordering::SeqCst);
        s.replace_owned(json!({"id":"B"}), 2);
    }
    let epoch = shared.state.lock().unwrap().epoch().to_owned();
    shared.observe_native(
        1,
        &Event::Record(json!({"type":"system","subtype":"init","session_id":"OLD"})),
    );
    shared.observe_native(
        1,
        &Event::Exited(Exit {
            code: Some(0),
            success: true,
            observed: true,
        }),
    );
    shared.observe_native(1, &Event::ProtocolError("old pipe".into()));
    {
        let s = shared.state.lock().unwrap();
        assert_eq!(s.epoch(), epoch);
        assert!(s.session.is_none());
        assert!(s.exit.is_none());
        assert!(!s.incomplete.contains("native_protocol"));
    }
    shared.observe_native(
        2,
        &Event::Record(json!({"type":"system","subtype":"init","session_id":"NEW"})),
    );
    assert_eq!(shared.state.lock().unwrap().session.as_deref(), Some("NEW"));
}

#[test]
fn restart_wait_error_record_is_not_replacement_permission() {
    assert!(
        !observed_exit(Some(Exit {
            code: None,
            success: false,
            observed: false
        })),
        "resolved WAIT ERROR must refuse BEFORE generation reset or replacement exec"
    );
}
#[test]
fn restart_observed_signal_and_nonzero_status_are_valid_exit_evidence() {
    assert!(observed_exit(Some(Exit {
        code: None,
        success: false,
        observed: true
    })));
    assert!(observed_exit(Some(Exit {
        code: Some(1),
        success: false,
        observed: true
    })));
    assert!(!observed_exit(None));
}

// Actual private OS spawn at the adapter seam, not a core hook. No timing sleeps.
// Poll the real async signal publisher once while the actual dispatch is held after its last
// ending sample. The stdin producer uses the same admission mutex via blocking_lock.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lifetime_cannot_publish_inside_the_admitted_spawn_window() {
    use std::{
        future::{Future, poll_fn},
        sync::Barrier,
        task::Poll,
    };
    let shared = Shared::new("boot".into(), "fixture".into(), "instance".into());
    let (sender, receiver) = tokio::sync::watch::channel(false);
    shared.set_lifetime(receiver.clone());
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    *shared.spawn_window.lock().unwrap() = Some((entered.clone(), release.clone()));
    let running = shared.clone();
    let spawn = tokio::spawn(async move {
        running
            .spawn_native(
                &[
                    "/usr/bin/python3".into(),
                    "-c".into(),
                    "import sys; sys.stdin.buffer.read()".into(),
                ],
                &std::env::current_dir().unwrap(),
                &HashMap::new(),
            )
            .await
    });
    entered.wait(); // Actual last sample=false; OS spawn has NOT happened yet, admission held.
    let mut input_admission = Box::pin(shared.effects.lock());
    let input_pending =
        poll_fn(|cx| Poll::Ready(input_admission.as_mut().poll(cx).is_pending())).await;
    drop(input_admission);
    let mut publisher = Box::pin(shared.end_lifetime(&sender));
    let pending = poll_fn(|cx| Poll::Ready(publisher.as_mut().poll(cx).is_pending())).await;
    let published_before_dispatch = *receiver.borrow();
    release.wait();
    let native = spawn.await.unwrap().unwrap(); // This proves an actual private OS child was created.
    if pending {
        publisher.await;
    }
    let stopped_by_publication = if pending {
        Some(tokio::time::timeout(Duration::from_secs(3), native.exited()).await)
    } else {
        None
    };
    native.stop();
    let exit = tokio::time::timeout(Duration::from_secs(3), native.exited())
        .await
        .unwrap();
    assert!(exit.observed);
    let _ = tokio::time::timeout(Duration::from_secs(2), native.drained()).await;
    assert!(
        input_pending,
        "no other input admission may enter before the process is up"
    );
    assert!(
        !published_before_dispatch && pending,
        "lifetime PUBLISHED after last sample but BEFORE actual OS dispatch; admission was not fenced"
    );
    assert!(*receiver.borrow());
    assert!(
        matches!(
            stopped_by_publication,
            Some(Ok(Exit { observed: true, .. }))
        ),
        "dispatch winner must later stop its OWNED child on lifetime publication"
    );
}
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_control_bringup_releases_admission_then_lifetime_stops_current_child() {
    use std::{
        future::{Future, poll_fn},
        task::Poll,
    };
    let shared = Shared::new("boot".into(), "fixture".into(), "instance".into());
    let (sender, receiver) = tokio::sync::watch::channel(false);
    shared.set_lifetime(receiver);
    let native = shared
        .spawn_native(
            &[
                "/usr/bin/python3".into(),
                "-c".into(),
                "import sys; sys.stdin.buffer.read()".into(),
            ],
            &std::env::current_dir().unwrap(),
            &HashMap::new(),
        )
        .await
        .unwrap();
    // Exercise the actual initializer's failed-deadline path under the SAME admission fence.
    // This is a private bring-up control, not a full native/helper restart qualification.
    let admission = shared.effects.lock().await;
    let mut publisher = Box::pin(shared.end_lifetime(&sender));
    let pending = poll_fn(|cx| Poll::Ready(publisher.as_mut().poll(cx).is_pending())).await;
    let initialized = tokio::time::timeout(Duration::ZERO, shared.initialize_primary(&native, 1))
        .await
        .unwrap_or(false);
    drop(admission);
    if pending {
        publisher.await;
    }
    let publication_stop = tokio::time::timeout(Duration::from_secs(3), native.exited()).await;
    native.stop(); // unconditional owned cleanup even if the assertion below would fail
    let _ = tokio::time::timeout(Duration::from_secs(3), native.exited()).await;
    let _ = tokio::time::timeout(Duration::from_secs(2), native.drained()).await;
    assert!(!initialized);
    assert!(pending);
    assert!(
        matches!(publication_stop, Ok(Exit { observed: true, .. })),
        "failed bring-up must release the fence and lifetime publication must stop CURRENT owned child"
    );
}
#[tokio::test]
async fn stdin_thread_publication_winner_refuses_before_any_spawn() {
    let shared = Shared::new("boot".into(), "fixture".into(), "instance".into());
    let (sender, receiver) = tokio::sync::watch::channel(false);
    shared.set_lifetime(receiver);
    let publishing = shared.clone();
    std::thread::spawn(move || publishing.end_lifetime_blocking(&sender))
        .join()
        .unwrap();
    let result = shared
        .spawn_native(
            &["must-not-execute".into()],
            &std::env::current_dir().unwrap(),
            &HashMap::new(),
        )
        .await;
    assert!(matches!(result,Err(error) if error.kind=="unavailable"&&!error.may_have_acted));
    assert!(!shared.spawned.load(Ordering::SeqCst));
    assert!(shared.process().is_none());
}
#[tokio::test]
async fn lifetime_publication_winner_refuses_before_any_spawn() {
    let shared = Shared::new("boot".into(), "fixture".into(), "instance".into());
    let (sender, receiver) = tokio::sync::watch::channel(false);
    shared.set_lifetime(receiver);
    shared.end_lifetime(&sender).await;
    let result = shared
        .spawn_native(
            &["must-not-execute".into()],
            &std::env::current_dir().unwrap(),
            &HashMap::new(),
        )
        .await;
    assert!(matches!(result,Err(error) if error.kind=="unavailable"&&!error.may_have_acted));
    assert!(!shared.spawned.load(Ordering::SeqCst));
    assert!(shared.process().is_none());
}
