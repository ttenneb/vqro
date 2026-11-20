//! Actual item-channel tests: production bridge/SDK, credited provider wire, shared viewer reducer.
use super::*;
use vqro::provider_client::items::ItemView;

impl Core {
    async fn open_items(&self, id: u64, after: Value, window: usize) -> Value {
        let incarnation = self.facts.lock().unwrap()["endpoint-1"]["incarnation"].clone();
        match self
            .peer
            .call_with_timeout(
                "stream.open",
                json!({"generation":self.generation,
            "stream":id,"ref":"endpoint-1","incarnation":incarnation,"channel":"items",
            "after":after,"window_bytes":window}),
                Duration::from_secs(3),
            )
            .await
        {
            Ok(value) => value,
            Err(error) => json!({"_error":format!("{error:?}")}),
        }
    }
    fn credit(&self, id: u64) {
        self.auto_credit.lock().unwrap().insert(id);
        let outstanding: usize = self
            .events
            .lock()
            .unwrap()
            .iter()
            .filter(|e| e["method"] == "stream.data" && e["params"]["stream"] == id)
            .map(|e| {
                B64.decode(e["params"]["data"].as_str().unwrap())
                    .unwrap()
                    .len()
            })
            .sum();
        if outstanding > 0 {
            self.peer.notify(
                "stream.credit",
                json!({"generation":self.generation,"stream":id,"bytes":outstanding}),
            );
        }
    }
    fn detach(&self, id: u64) {
        self.auto_credit.lock().unwrap().remove(&id);
        self.peer.notify(
            "stream.close",
            json!({"generation":self.generation,"stream":id}),
        );
    }
    fn bytes(&self, id: u64) -> Vec<u8> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter(|e| e["method"] == "stream.data" && e["params"]["stream"] == id)
            .flat_map(|e| B64.decode(e["params"]["data"].as_str().unwrap()).unwrap())
            .collect()
    }
    fn view(&self, id: u64) -> ItemView {
        self.seeded_view(id, &[])
    }
    fn seeded_view(&self, id: u64, seed: &[u8]) -> ItemView {
        let mut view = ItemView::new();
        view.feed(seed);
        view.stream_ended(); // discard any uncommitted final line before continuing on another stream
        let mut offset = 0;
        for event in self
            .events
            .lock()
            .unwrap()
            .iter()
            .filter(|e| e["params"]["stream"] == id)
        {
            if event["method"] == "stream.data" {
                assert_eq!(event["params"]["offset"].as_u64(), Some(offset));
                let bytes = B64
                    .decode(event["params"]["data"].as_str().unwrap())
                    .unwrap();
                offset += bytes.len() as u64;
                view.feed(&bytes);
            } else if event["method"] == "stream.closed" || event["method"] == "stream.gap" {
                view.stream_ended();
            }
        }
        assert_eq!(
            view.errors.total(),
            0,
            "invalid canonical item records: {:?}",
            view.errors
        );
        view
    }
    fn has_event(&self, id: u64, method: &str) -> bool {
        self.events
            .lock()
            .unwrap()
            .iter()
            .any(|e| e["params"]["stream"] == id && e["method"] == method)
    }
    async fn wait_items(&mut self, predicate: impl Fn(&Self) -> bool) {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if predicate(self) {
                    return;
                }
                self.changed.changed().await.unwrap();
            }
        })
        .await
        .expect("item stream condition");
    }
    async fn new_session(&self, effect: &str) {
        let result = self
            .effect(
                effect,
                "endpoint-1",
                "agent.session",
                json!({"v":0,"action":"new"}),
                1000,
            )
            .await;
        assert_eq!(result["outcome"], "observed");
    }
}

#[tokio::test]
async fn credited_views_continue_independently_and_session_switch_fences_a_new_epoch() {
    let root = tempfile::tempdir().unwrap();
    let listener = UnixListener::bind(root.path().join("core.sock")).unwrap();
    let child = bridge(&root, 1, 0, &[]);
    let mut core = Core::accept(&listener, 1, "bootstrap").await;
    core.wait(|f| f.len() == 1).await;
    let initial_epoch = core.facts.lock().unwrap()["endpoint-1"]["data"]["agent"]["epoch"].clone();
    core.credit(1);
    let open = core.open_items(1, Value::Null, 262144).await;
    assert_eq!(open["initial"], "reset");
    assert_eq!(
        open["epoch"], initial_epoch,
        "sessionless facts and snapshot use one epoch"
    );
    core.wait_items(|c| c.view(1).snapshots == 1).await;
    assert_eq!(
        core.view(1).entity("session", "current").unwrap().1["id"],
        Value::Null
    );
    core.new_session("first").await;
    core.wait_items(|c| c.view(1).snapshots == 2).await;
    let epoch = core.view(1).epoch().unwrap().to_string();
    assert_ne!(Value::String(epoch.clone()), initial_epoch);
    assert_eq!(
        core.open_items(2, Value::Null, 64).await["initial"],
        "reset"
    );
    core.wait_items(|c| c.has_event(2, "stream.data")).await;
    assert_eq!(
        core.view(2).snapshots,
        0,
        "slow viewer has not completed its snapshot"
    );
    assert_eq!(
        core.effect("input", "endpoint-1", "agent.send", send("visible"), 1000)
            .await["outcome"],
        "accepted"
    );
    core.wait(|f| f["endpoint-1"]["data"]["agent"]["state"] == "idle")
        .await;
    core.wait_items(|c| c.view(1).records > 0).await;
    // An independent query completes while the second viewer is stalled in SDK send().
    assert_eq!(
        core.effect(
            "receipt",
            "endpoint-1",
            "agent.receipt",
            json!({"v":0,"delivery":"visible"}),
            1000
        )
        .await["data"]["stage"],
        "admitted"
    );
    core.credit(2);
    core.wait_items(|c| c.view(2).snapshots == 1 && c.view(1).digest() == c.view(2).digest())
        .await;
    let seed = core.bytes(1);
    let before = core.seeded_view(99, &seed);
    let after = before.after();
    let before_seq = before.seq();
    core.detach(2);
    core.wait_items(|c| c.has_event(2, "stream.closed")).await;
    core.credit(3);
    let continued = core.open_items(3, after, 262144).await;
    assert_eq!(continued["initial"], "continue");
    assert_eq!(continued["epoch"], epoch);
    let completion =
        core.facts.lock().unwrap()["endpoint-1"]["data"]["agent"]["completion"].clone();
    assert_eq!(
        core.effect(
            "continued-input",
            "endpoint-1",
            "agent.send",
            send("continued"),
            1000
        )
        .await["outcome"],
        "accepted"
    );
    core.wait(|f| {
        f["endpoint-1"]["data"]["agent"]["state"] == "idle"
            && f["endpoint-1"]["data"]["agent"]["completion"] != completion
    })
    .await;
    core.wait_items(|c| {
        c.seeded_view(3, &seed).seq() > before_seq
            && c.view(1).digest() == c.seeded_view(3, &seed).digest()
    })
    .await;
    assert_eq!(
        core.seeded_view(3, &seed).snapshots,
        2,
        "continue adds no replacement snapshot"
    );
    core.new_session("second").await;
    core.wait_items(|c| c.view(1).snapshots == 3 && c.seeded_view(3, &seed).snapshots == 3)
        .await;
    assert_eq!(core.view(1).digest(), core.seeded_view(3, &seed).digest());
    assert_ne!(core.view(1).epoch(), Some(epoch.as_str()));
    finish(child).await;
    core.close().await;
}

#[tokio::test]
async fn actual_item_budget_reclaims_all_credit_blocked_snapshot_cursors_on_detach() {
    let root = tempfile::tempdir().unwrap();
    let listener = UnixListener::bind(root.path().join("core.sock")).unwrap();
    let child = bridge(&root, 1, 0, &[]);
    let mut core = Core::accept(&listener, 1, "bootstrap").await;
    core.wait(|f| f.len() == 1).await;
    for id in 1..=4 {
        assert_eq!(
            core.open_items(id, Value::Null, 1).await["initial"],
            "reset"
        );
    }
    core.wait_items(|c| (1..=4).all(|id| c.has_event(id, "stream.data")))
        .await;
    assert!(
        core.open_items(5, Value::Null, 1).await["_error"].is_string(),
        "four viewers fill the item budget"
    );
    for id in 1..=4 {
        core.detach(id);
    }
    core.wait_items(|c| (1..=4).all(|id| c.has_event(id, "stream.closed")))
        .await;
    for id in 6..=9 {
        assert_eq!(
            core.open_items(id, Value::Null, 1).await["initial"],
            "reset",
            "viewer/cursor/snapshot permits were not reclaimed"
        );
    }
    finish(child).await;
    core.close().await;
}

#[tokio::test]
async fn post_cut_retention_gap_is_explicit_and_reattach_resets_to_current_tail() {
    let root = tempfile::tempdir().unwrap();
    let listener = UnixListener::bind(root.path().join("core.sock")).unwrap();
    let child = bridge(&root, 1, 0, &[]);
    let mut core = Core::accept(&listener, 1, "bootstrap").await;
    core.wait(|f| f.len() == 1).await;
    core.new_session("new").await;
    core.credit(1);
    assert_eq!(
        core.open_items(1, Value::Null, 262144).await["initial"],
        "reset"
    );
    core.wait_items(|c| c.view(1).snapshots == 1).await;
    let old = core.view(1).after();
    assert_eq!(
        core.open_items(2, Value::Null, 64).await["initial"],
        "reset"
    );
    core.wait_items(|c| c.has_event(2, "stream.data")).await;
    for n in 0..32 {
        let mut input = send(&format!("large-{n}"));
        input["content"] = json!([{"type":"text","text":"z".repeat(20000)}]);
        assert_eq!(
            core.effect(
                &format!("large-{n}"),
                "endpoint-1",
                "agent.send",
                input,
                1000
            )
            .await["outcome"],
            "accepted"
        );
        core.wait(|f| f["endpoint-1"]["data"]["agent"]["state"] == "idle")
            .await;
    }
    core.credit(2);
    core.wait_items(|c| c.has_event(2, "stream.gap")).await;
    let gap = core
        .events
        .lock()
        .unwrap()
        .iter()
        .find(|e| e["method"] == "stream.gap" && e["params"]["stream"] == 2)
        .unwrap()
        .clone();
    assert_eq!(gap["params"]["reason"]["gap"], "retention");
    core.detach(2);
    core.credit(3);
    assert_eq!(core.open_items(3, old, 262144).await["initial"], "reset");
    core.wait_items(|c| c.view(3).snapshots == 1 && c.view(1).digest() == c.view(3).digest())
        .await;
    finish(child).await;
    core.close().await;
}
