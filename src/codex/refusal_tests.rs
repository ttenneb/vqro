//! Refusals pass through the existing projection's incomplete witness, not a silent drop.
use super::*;

fn endpoint() -> Endpoint {
    let mut endpoint = Endpoint::new("ep".into(), "inc".into(), "host".into(), "epoch".into());
    endpoint
        .set_session(
            &json!({"id":"main","status":{"type":"idle"}}),
            "epoch2".into(),
        )
        .unwrap();
    endpoint.notification(
        "thread/started",
        &json!({"thread":{"id":"child","parentThreadId":"main","status":{"type":"active"}}}),
    );
    endpoint
}

#[test]
fn oversized_main_and_child_deltas_retain_prefix_and_mark_entity_and_stream_incomplete() {
    for thread in ["main", "child"] {
        for (method, kind) in [
            ("item/agentMessage/delta", "message"),
            ("item/commandExecution/outputDelta", "tool"),
        ] {
            let mut endpoint = endpoint();
            let mut projection = Projection::new("epoch2".into());
            projection.apply(vec![endpoint.state_entity()]);
            let first = endpoint.notification(
                method,
                &json!({"threadId":thread,"itemId":"item","delta":"retained prefix"}),
            );
            let id = first[0].id.clone();
            projection.apply(first);
            projection.apply(endpoint.notification(
                method,
                &json!({"threadId":thread,"itemId":"item","delta":"x".repeat(ENTITY_BYTES+1)}),
            ));
            let value = &projection.journal.entity(kind, &id).unwrap().1;
            assert_eq!(value["content"][0]["text"], "retained prefix");
            assert_eq!(value["incomplete"], true);
            assert_eq!(
                projection.journal.entity("state", "current").unwrap().1["items_incomplete"],
                true
            );
            if thread == "child" {
                assert!(value["child"].is_string());
            }
            let mut view = ItemView::new();
            let mut cursor = projection.journal.open(None);
            drain(&mut cursor, &mut projection, &mut view);
            assert_eq!(view.entity(kind, &id).unwrap().1["incomplete"], true);
        }
    }
}

#[test]
fn over_bound_whole_plans_retain_old_list_but_mark_it_incomplete() {
    for thread in ["main", "child"] {
        for oversized in [
            json!(vec![json!({"step":"new","status":"pending"}); 257]),
            json!([{"step":"x".repeat(8193),"status":"pending"}]),
            json!(vec![
                json!({"step":"x".repeat(8192),"status":"pending"});
                10
            ]),
        ] {
            let mut endpoint = endpoint();
            let mut projection = Projection::new("epoch2".into());
            projection.apply(vec![endpoint.state_entity()]);
            let first = endpoint.notification("turn/plan/updated", &json!({"threadId":thread,"turnId":"turn","plan":[{"step":"old list","status":"pending"}]}));
            let id = first[0].id.clone();
            projection.apply(first);
            projection.apply(endpoint.notification(
                "turn/plan/updated",
                &json!({"threadId":thread,"turnId":"turn","plan":oversized}),
            ));
            let value = &projection.journal.entity("plan", &id).unwrap().1;
            assert_eq!(value["items"][0]["text"], "old list");
            assert_eq!(value["incomplete"], true);
            assert_eq!(
                projection.journal.entity("state", "current").unwrap().1["items_incomplete"],
                true
            );
            if thread == "child" {
                assert!(value["owner"].is_string());
            }
            let mut view = ItemView::new();
            let mut cursor = projection.journal.open(None);
            drain(&mut cursor, &mut projection, &mut view);
            assert_eq!(view.entity("plan", &id).unwrap().1["incomplete"], true);
        }
    }
}
