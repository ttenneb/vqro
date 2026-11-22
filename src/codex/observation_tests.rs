use super::*;
use crate::codex::Endpoint;
use crate::provider_client::items::ItemView;

fn drain(cursor: &mut ItemCursor, projection: &mut Projection, view: &mut ItemView) {
    loop {
        match cursor.next(&mut projection.journal, ENTITY_BYTES) {
            Next::Data(bytes) => {
                for chunk in bytes.chunks(7) {
                    view.feed(chunk);
                }
            }
            Next::Idle => break,
            Next::Gap(reason) => panic!("unexpected gap {reason}"),
        }
    }
    assert_eq!(view.errors.total(), 0);
}

#[test]
fn native_first_delta_creates_a_typed_base_and_final_put_keeps_position() {
    let mut endpoint = Endpoint::new("s".into(), "i".into(), "h".into(), "e".into());
    endpoint
        .set_session(
            &json!({"id":"native","status":{"type":"idle"}}),
            "e2".into(),
        )
        .unwrap();
    let mut p = Projection::new("e2".into());
    p.apply(endpoint.notification(
        "item/agentMessage/delta",
        &json!({"threadId":"native","itemId":"m","delta":"héllo"}),
    ));
    let (rev, value) = p.journal.entity("message", "m").unwrap();
    assert_eq!(rev, 2);
    assert_eq!(value["content"][0]["text"], "héllo");
    let position = value["position"].clone();
    p.apply(endpoint.notification("item/completed",&json!({"threadId":"native","item":{"type":"agentMessage","id":"m","text":"authoritative"}})));
    let (_, value) = p.journal.entity("message", "m").unwrap();
    assert_eq!(value["position"], position);
    assert_eq!(value["content"][0]["text"], "authoritative");
    assert_eq!(value["final"], true);
    let mut view = ItemView::new();
    let mut cursor = p.journal.open(None);
    drain(&mut cursor, &mut p, &mut view);
    assert_eq!(view.entity("message", "m").unwrap().1["position"], position);
}

#[test]
fn startup_survives_tail_pressure_updates_in_place_and_resets_per_epoch() {
    let mut p = Projection::new("e".into());
    assert!(p.put("startup", "current", json!({"model":"start"}), true));
    for n in 0..1000 {
        assert!(p.put("message", &format!("m{n}"), json!({"role":"assistant","content":[{"type":"text","text":"x".repeat(1000)}],"final":true}), true));
    }
    assert_eq!(
        p.journal
            .entity("startup", "current")
            .expect("startup is retained, not a tail message")
            .1["model"],
        "start"
    );
    assert!(p.put(
        "startup",
        "current",
        json!({"model":"start","skills":[]}),
        true
    ));
    assert_eq!(p.journal.entity("startup", "current").unwrap().0, 2);
    let mut view = ItemView::new();
    let mut cursor = p.journal.open(None);
    drain(&mut cursor, &mut p, &mut view);
    assert_eq!(
        view.entity("startup", "current").unwrap().1["model"],
        "start"
    );
    p.reset("next");
    assert!(p.journal.entity("startup", "current").is_none());
}

#[test]
fn tail_eviction_preserves_state_pending_requests_and_active_item_positions() {
    let mut p = Projection::new("e".into());
    p.put("state", "current", json!({"state":"requires_action"}), true);
    p.put(
        "request",
        "q",
        json!({"status":"pending","params":{"body":"x".repeat(16000)}}),
        true,
    );
    p.put(
        "tool",
        "active",
        json!({"title":"active","status":"running","content":[]}),
        true,
    );
    let position = p.journal.entity("tool", "active").unwrap().1["position"].clone();
    for n in 0..1000 {
        assert!(p.put("message",&format!("m{n}"),json!({"role":"assistant","content":[{"type":"text","text":"x".repeat(1000)}],"final":true}),true));
    }
    assert!(p.journal.retained_bytes() <= RETAIN_BYTES);
    assert!(p.journal.entity_bytes() <= ENTITY_TOTAL);
    assert!(p.journal.len() <= ENTITIES);
    assert!(p.normal_bytes <= ENTITY_TOTAL - CONTROL_RESERVE);
    assert_eq!(p.entries.len(), p.journal.len());
    assert_eq!(p.order.len(), p.journal.len());
    assert_eq!(
        p.journal.entity("state", "current").unwrap().1["state"],
        "requires_action"
    );
    assert_eq!(
        p.journal.entity("request", "q").unwrap().1["status"],
        "pending"
    );
    assert_eq!(
        p.journal.entity("tool", "active").unwrap().1["position"],
        position
    );
    assert!(p.journal.entity("message", "m0").is_none());
}

#[test]
fn large_delta_is_explicitly_incomplete_and_every_snapshot_record_stays_bounded() {
    let mut p = Projection::new("e".into());
    p.put("state", "current", json!({"state":"running"}), true);
    p.apply(vec![Change::append(
        "message",
        "m".into(),
        json!({"role":"assistant","content":[{"type":"text","text":""}],"final":false}),
        "/content/0/text",
        "漢字🙂".repeat(20000),
    )]);
    let (_, value) = p.journal.entity("message", "m").unwrap();
    assert_eq!(value["incomplete"], true);
    assert_eq!(
        p.journal.entity("state", "current").unwrap().1["items_incomplete"],
        true
    );
    for line in p
        .journal
        .snapshot()
        .split(|b| *b == b'\n')
        .filter(|b| !b.is_empty())
    {
        assert!(line.len() + 1 <= ENTITY_BYTES);
    }
}

#[test]
fn two_snapshot_cuts_and_epoch_switch_reconstruct_without_duplicate_or_missing_updates() {
    let mut p = Projection::new("e1".into());
    p.put(
        "message",
        "m",
        json!({"role":"assistant","content":[{"type":"text","text":"a"}],"final":false}),
        true,
    );
    let mut first = p.journal.open(None);
    let mut a = ItemView::new();
    drain(&mut first, &mut p, &mut a);
    let mut second = p.journal.open(None);
    let mut b = ItemView::new();
    p.append(
        Change::new("message", "m", json!({})),
        "/content/0/text".into(),
        "b".into(),
    );
    drain(&mut second, &mut p, &mut b);
    drain(&mut first, &mut p, &mut a);
    assert_eq!(a.digest(), b.digest());
    p.reset("e2");
    p.put("state", "current", json!({"state":"unknown"}), true);
    drain(&mut first, &mut p, &mut a);
    drain(&mut second, &mut p, &mut b);
    assert_eq!(a.epoch(), Some("e2"));
    assert_eq!(a.digest(), b.digest());
    assert_eq!(a.snapshots, 2);
    assert!(a.entity("message", "m").is_none());
}

#[test]
fn snapshot_coverage_is_truthfully_tail_not_complete_history() {
    let p = Projection::new("e".into());
    let bytes = p.journal.snapshot();
    let first = bytes.split(|b| *b == b'\n').next().unwrap();
    let first: Value = serde_json::from_slice(first).unwrap();
    assert_eq!(
        first["coverage"], "tail",
        "C3 must expose truthful coverage for an adapter that prunes native history"
    );
}

#[test]
fn post_cut_overflow_gaps_before_emitting_more_snapshot_pieces() {
    let mut journal = ItemJournal::new(
        "e",
        JournalConfig {
            retain_bytes: 4096,
            snapshot_buffer_bytes: 4096,
            ..Default::default()
        },
    );
    for n in 0..8 {
        journal
            .put(
                "message",
                &format!("m{n}"),
                json!({"text":"x".repeat(2048)}),
            )
            .unwrap();
    }
    let mut cursor = journal.open(None);
    assert!(matches!(cursor.next(&mut journal, 1024), Next::Data(_)));
    for n in 0..20 {
        journal
            .put("state", "current", json!({"n":n,"text":"y".repeat(2048)}))
            .unwrap();
    }
    let next = cursor.next(&mut journal, 1024);
    assert!(
        matches!(next, Next::Gap("retention")),
        "overflowed cut must stop before snapshot end, not send more pieces"
    );
}

#[test]
fn snapshot_lease_survives_the_final_piece_until_send_completes() {
    let observation = Observation::new(1, "host".into());
    let (mut cursor, mut lease, charge) = {
        let mut p = observation.slots[0].projection.lock().unwrap();
        let charge = p.snapshot_charge();
        let lease = observation.snapshot_permit(&p).unwrap();
        (p.journal.open(None), Some(lease), charge)
    };
    let (next, pending) = observation.advance(0, &mut cursor, &mut lease).unwrap();
    assert!(matches!(next, Next::Data(_)));
    assert!(!pending, "empty snapshot begin/end is the final piece");
    assert!(
        lease.is_some(),
        "advance must not release a piece before send"
    );
    assert_eq!(
        observation.snapshots.available_permits(),
        SNAPSHOT_BYTES - charge
    );
    drop(lease.take()); // the caller's successful final-piece send
    assert_eq!(observation.snapshots.available_permits(), SNAPSHOT_BYTES);
}

#[test]
fn epoch_restart_funds_a_larger_cut_even_while_the_old_snapshot_is_pending() {
    let observation = Observation::new(1, "host".into());
    let (mut cursor, mut lease, old_charge) = {
        let mut p = observation.slots[0].projection.lock().unwrap();
        for n in 0..40 {
            p.journal
                .put(
                    "message",
                    &format!("old{n}"),
                    json!({"text":"x".repeat(2048)}),
                )
                .unwrap();
        }
        let charge = p.snapshot_charge();
        let lease = observation.snapshot_permit(&p).unwrap();
        (p.journal.open(None), Some(lease), charge)
    };
    assert!(observation.advance(0, &mut cursor, &mut lease).unwrap().1);
    let new_charge = {
        let mut p = observation.slots[0].projection.lock().unwrap();
        p.reset("new");
        for n in 0..200 {
            p.journal
                .put(
                    "message",
                    &format!("new{n}"),
                    json!({"text":"y".repeat(2048)}),
                )
                .unwrap();
        }
        p.snapshot_charge()
    };
    assert!(new_charge > old_charge);
    let (next, pending) = observation.advance(0, &mut cursor, &mut lease).unwrap();
    let Next::Data(data) = next else {
        panic!("replacement snapshot")
    };
    let begin: Value = serde_json::from_slice(data.split(|b| *b == b'\n').next().unwrap()).unwrap();
    assert_eq!(begin["epoch"], "new");
    assert!(pending);
    assert_eq!(
        observation.snapshots.available_permits(),
        SNAPSHOT_BYTES - new_charge
    );
    drop(cursor);
    drop(lease);
    assert_eq!(observation.snapshots.available_permits(), SNAPSHOT_BYTES);
}

#[test]
fn an_unfunded_epoch_restart_preserves_the_old_cut_until_the_viewer_closes() {
    let observation = Observation::new(1, "host".into());
    let (mut cursor, mut lease) = {
        let mut p = observation.slots[0].projection.lock().unwrap();
        let lease = observation.snapshot_permit(&p).unwrap();
        (p.journal.open(None), Some(lease))
    };
    let old_epoch = cursor.epoch().to_string();
    let blocker = observation
        .snapshots
        .clone()
        .try_acquire_many_owned(observation.snapshots.available_permits() as u32)
        .unwrap();
    observation.slots[0].projection.lock().unwrap().reset("new");
    assert_eq!(
        observation
            .advance(0, &mut cursor, &mut lease)
            .unwrap_err()
            .kind,
        "overloaded"
    );
    assert_eq!(cursor.epoch(), old_epoch);
    assert!(cursor.snapshot_pending() && lease.is_some());
    drop(cursor);
    drop(lease);
    drop(blocker);
    assert_eq!(observation.snapshots.available_permits(), SNAPSHOT_BYTES);
}
