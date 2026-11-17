use super::*;

fn fixture(capacity: usize) -> (Panes, PaneId, broadcast::Receiver<Notification>, broadcast::Sender<Notification>, PaneStream) {
    let pane = PaneId::parse("%1").unwrap();
    let (tx, rx) = broadcast::channel(capacity);
    let panes = Arc::new(Mutex::new(BTreeMap::from([(pane.clone(), Pane { tx, offset: 0, osc: Osc7501::default(), recovering: false })])));
    let (notes, nrx) = broadcast::channel(8);
    (panes, pane, nrx, notes, PaneStream { rx })
}
fn data(panes: &Panes, pane: &PaneId, notes: &broadcast::Sender<Notification>, bytes: &[u8]) {
    dispatch(Record::Output { pane: pane.clone(), bytes: bytes.to_vec(), age_ms: Some(0) }, panes, notes);
}

#[tokio::test]
async fn slow_viewer_has_explicit_gap_without_stalling_fast_viewer() {
    let (panes, pane, _, notes, mut slow) = fixture(2);
    let mut fast = PaneStream { rx: panes.lock().unwrap()[&pane].tx.subscribe() };
    for offset in 0..5 {
        data(&panes, &pane, &notes, b"x");
        assert_eq!(fast.recv().await, PaneEvent::Data { offset, bytes: Arc::from(&b"x"[..]) });
    }
    assert!(matches!(slow.recv().await, PaneEvent::Gap(GapReason::SubscriberLagged(_))));
    data(&panes, &pane, &notes, b"fresh");
    assert_eq!(slow.recv().await, PaneEvent::Data { offset: 5, bytes: Arc::from(&b"fresh"[..]) });
}

#[tokio::test]
async fn pause_and_recovery_discard_partial_osc_and_stale_raw_tail() {
    let (panes, pane, mut nrx, notes, mut stream) = fixture(16);
    data(&panes, &pane, &notes, b"\x1b]7501;partial");
    assert!(matches!(stream.recv().await, PaneEvent::Data { .. }));
    dispatch(Record::Notification(Notification::Pause(pane.clone())), &panes, &notes);
    assert_eq!(stream.recv().await, PaneEvent::Gap(GapReason::Paused));
    assert_eq!(nrx.recv().await.unwrap(), Notification::Pause(pane.clone()));
    panes.lock().unwrap().get_mut(&pane).unwrap().recovering = true;
    data(&panes, &pane, &notes, b"stale-tail\x07");
    dispatch(Record::Notification(Notification::Continue(pane.clone())), &panes, &notes);
    assert_eq!(stream.recv().await, PaneEvent::Continued);
    data(&panes, &pane, &notes, b"tail\x07");
    assert!(matches!(stream.recv().await, PaneEvent::Data { .. }));
    assert!(stream.rx.try_recv().is_err(), "gap cannot stitch an old status onto fresh output");
    data(&panes, &pane, &notes, b"\x1b]7501;new\x07");
    assert!(matches!(stream.recv().await, PaneEvent::Data { .. }));
    assert_eq!(stream.recv().await, PaneEvent::Osc7501(Arc::from(&b"new"[..])));
}

#[test]
fn invalid_arguments_and_ids_cannot_inject_control_lines() {
    assert!(quote("a\nb").is_err());
    assert!(quote("\0").is_err());
    assert!(PaneId::parse("%1; kill-server").is_err());
    assert!(PaneId::parse("%x").is_err());
    assert!(dimensions(0, 20).is_err());
    assert_eq!(quote("a'b\\c; $d").unwrap(), "'a'\\''b\\c; $d'");
}
