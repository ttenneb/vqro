//! Genuine native control routing to a synthetic peer; acceptance is never terminal proof.
use super::*;
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn full_protected_budget_cannot_silence_public_foreground_restart_refusal() {
    let (_root, shared, _guard, provider, mut requests, mut core, _log, helpers) =
        fixture("same", true).await;
    {
        let mut s = shared.state.lock().unwrap();
        let mut remaining: usize = 192 * 1024;
        for n in 0..4 {
            let mut tool = json!({"status":"running","padding":""});
            let overhead = vqro::limits::encoded_bytes(&tool);
            let size = remaining.min(60000);
            tool["padding"] = json!("x".repeat(size - overhead));
            assert!(s.put("tool", &format!("protected-{n}"), tool));
            remaining -= size;
        }
        assert_eq!(remaining, 0);
        for n in 0..128 {
            s.observe(&vqro::native_process::Event::Record(json!({"type":"system","subtype":"task_notification","task_id":format!("old-{n}"),"run_id":"1","status":"completed"})));
        }
        s.observe(&vqro::native_process::Event::Record(json!({"type":"system","subtype":"task_started","task_id":"lost-129","run_id":"1","task_type":"local_agent","subagent_type":"worker","spawn_depth":1,"is_backgrounded":false})));
        assert!(
            s.journal.entity("notice", "foreground-work-loss").is_none(),
            "existing budget permits only best-effort pane notice"
        );
    }
    for loss in [false, true] {
        let mut a = args(&shared);
        a["allow_background_loss"] = json!(loss);
        let reply = core
            .invoke(&shared, &provider, &mut requests, "claude.restart", a)
            .await;
        assert_eq!(reply["result"]["outcome"], "rejected");
        let message = reply["result"]["error"]["message"].as_str().unwrap();
        assert!(
            message.contains("task list full")
                && message.contains(
                    "automatic restart refused until the process is replaced another way"
                ),
            "public refusal must retain lost-foreground reason/recovery even without pane budget: {reply}"
        );
    }
    assert!(
        lines(&helpers).is_empty(),
        "guard must refuse before helper/preflight/new exec"
    );
}
fn seed(shared: &Arc<Shared>) -> (String, String, String) {
    let mut s = shared.state.lock().unwrap();
    s.observe(&vqro::native_process::Event::Record(json!({"type":"system","subtype":"task_started","task_id":"native-task-a","run_id":"1","task_type":"local_agent","subagent_type":"worker","spawn_depth":1,"description":"actual task"})));
    s.observe(&vqro::native_process::Event::Record(json!({"type":"system","subtype":"background_tasks_changed","tasks":[{"task_id":"native-task-a","run_id":"1","task_type":"local_agent","subagent_type":"worker","description":"actual task"}]})));
    let row = &s.background.as_ref().unwrap()[0];
    (
        s.epoch().into(),
        row["id"].as_str().unwrap().into(),
        row["child"].as_str().unwrap().into(),
    )
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_task_and_child_cancel_ack_is_accepted_not_an_observed_end() {
    let (_root, shared, _guard, provider, mut requests, mut core, log, _helpers) =
        fixture("same", false).await;
    let (epoch, task, child) = seed(&shared);
    for (kind, id) in [("task", task.clone()), ("child", child.clone())] {
        let reply = core
            .invoke(
                &shared,
                &provider,
                &mut requests,
                "agent.object",
                json!({"v":0,"epoch":epoch,"action":"cancel","kind":kind,"id":id}),
            )
            .await;
        assert_eq!(reply["result"]["outcome"], "accepted", "{reply}");
        assert!(reply["result"]["data"].get("stopped").is_none());
    }
    {
        let s = shared.state.lock().unwrap();
        assert_eq!(
            s.journal.entity("task", &task).unwrap().1["status"],
            "running"
        );
        assert_eq!(
            s.journal.entity("child", &child).unwrap().1["status"],
            "running"
        );
    }
    let calls: Vec<_> = lines(&log)
        .into_iter()
        .filter(|r| r["in"]["request"]["subtype"] == "stop_task")
        .collect();
    assert_eq!(calls.len(), 2);
    assert!(
        calls
            .iter()
            .all(|r| r["in"]["request"]["task_id"] == "native-task-a")
    );
    for row in lines(&log)
        .iter()
        .filter(|r| r["in"]["request"]["subtype"] == "initialize")
    {
        assert!(row["in"]["request"].get("perTaskStopAffordance").is_none());
        assert!(row["in"]["request"].get("forwardSubagentText").is_none());
    }
    {
        let mut s = shared.state.lock().unwrap();
        s.observe(&vqro::native_process::Event::Record(json!({"type":"system","subtype":"task_notification","task_id":"native-task-a","run_id":"1","status":"stopped"})));
        assert_eq!(
            s.journal.entity("task", &task).unwrap().1["status"],
            "stopped"
        );
    }
    stop(&shared).await;
    provider.close();
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stale_unknown_and_unqualified_object_controls_never_dispatch() {
    let (_root, shared, _guard, provider, mut requests, mut core, log, _helpers) =
        fixture("same", false).await;
    let (epoch, task, _) = seed(&shared);
    for args in [
        json!({"v":0,"epoch":"old","action":"cancel","kind":"task","id":task}),
        json!({"v":0,"epoch":epoch,"action":"cancel","kind":"task","id":"not-observed"}),
        json!({"v":0,"epoch":epoch,"action":"close","kind":"child","id":task}),
        json!({"v":0,"action":"cancel","kind":"task","id":task}),
        json!({"v":0,"epoch":epoch,"action":"cancel","kind":"task","id":task,"task_id":"override"}),
    ] {
        let reply = core
            .invoke(&shared, &provider, &mut requests, "agent.object", args)
            .await;
        assert_eq!(reply["result"]["outcome"], "rejected", "{reply}");
    }
    shared.state.lock().unwrap().version = Some("unknown-future".into());
    let reply = core
        .invoke(
            &shared,
            &provider,
            &mut requests,
            "agent.object",
            json!({"v":0,"epoch":epoch,"action":"cancel","kind":"task","id":task}),
        )
        .await;
    assert_eq!(reply["result"]["outcome"], "rejected");
    assert!(
        lines(&log)
            .iter()
            .all(|r| r["in"]["request"]["subtype"] != "stop_task")
    );
    let caps =
        shared.state.lock().unwrap().facts("fixture", "fixture")["data"]["agent"]["caps"].clone();
    assert!(caps.get("agent.object").is_none());
    assert!(caps.get("agent.child").is_none());
    assert_eq!(caps["agent.interrupt"]["scope"], "all_work");
    stop(&shared).await;
    provider.close();
}
