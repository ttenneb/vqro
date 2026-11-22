//! Native hook metadata only; no installed Codex, filesystem discovery or profile reads.
use serde_json::{Value, json};
use vqro::codex::{Change, Endpoint};

fn endpoint() -> Endpoint {
    let mut e = Endpoint::new("e".into(), "inc".into(), "host".into(), "old".into());
    e.set_session_response(
        &json!({"thread":{"id":"s","status":{"type":"idle"}},"cwd":"/native"}),
        "epoch".into(),
    )
    .unwrap();
    e
}
fn hook(key: &str, event: &str) -> Value {
    json!({"key":key,"eventName":event,"source":"user","sourcePath":"/native/hooks.json","enabled":true,"trustStatus":"trusted","currentHash":"opaque-hash","isManaged":false,"displayOrder":0,"timeoutSec":5,"handlerType":"command","command":"private command must not be published"})
}
fn hooks(rows: Vec<Value>) -> Value {
    json!({"data":[{"cwd":"/native","hooks":rows,"warnings":[],"errors":[]}]})
}
fn value(changes: &[Change]) -> &Value {
    &changes
        .iter()
        .find(|c| c.kind == "startup" && c.id == "current")
        .unwrap()
        .value
}
fn transform(e: &Endpoint) -> Value {
    e.facts()["data"]["agent"]["input_transform"].clone()
}

#[test]
fn run_notifications_are_not_a_loaded_inventory_or_a_native_hook_name() {
    let mut e = endpoint();
    e.record_receipt("d", "dispatched", None, &json!({}));
    let changes=e.notification("hook/started",&json!({"threadId":"s","run":{"id":"a-run-id-not-a-hook-name","eventName":"userPromptSubmit","handlerType":"command","status":"running"}}));
    assert!(changes.is_empty());
    assert!(transform(&e).is_null());
    assert!(
        e.record_receipt("d", "admitted", None, &json!({}))
            .value
            .get("may_transform")
            .is_none()
    );
    e.lifecycle = "ended".into();
    assert!(e.notification("hook/started",&json!({"threadId":"s","run":{"eventName":"userPromptSubmit","handlerType":"command","status":"running"}})).is_empty());
}

#[test]
fn errors_partial_warnings_unknown_types_and_scopes_are_not_native_safety() {
    let mut e = endpoint();
    let mut data = hooks(vec![hook("input", "userPromptSubmit")]);
    data["data"][0]["errors"] = json!([{"message":"do not publish this private error"}]);
    let changes = e.finish_startup_inventory("s", "epoch", "hooks", Some(&data));
    assert!(value(&changes).get("extensions").is_none());
    assert!(transform(&e).is_null());
    let mut row = hook("future", "unknownNativeEvent");
    row["source"] = json!("futureScope");
    data = hooks(vec![row]);
    data["data"][0]["warnings"] = json!(["private warning"]);
    let changes = e.finish_startup_inventory("s", "epoch", "hooks", Some(&data));
    let s = value(&changes);
    assert!(s["extensions"][0].get("transforms").is_none());
    assert!(s["extensions"][0].get("scope").is_none());
    assert!(
        s["incomplete"]
            .as_array()
            .unwrap()
            .contains(&json!("extensions"))
    );
    assert!(!s.to_string().contains("private"));
    assert!(transform(&e).is_null());
}

#[test]
fn hooks_share_whole_entity_budget_and_caps_declare_list_only_after_native_evidence() {
    let mut e = endpoint();
    assert!(
        e.facts()["data"]["agent"]["caps"]
            .get("agent.customize")
            .is_none()
    );
    let rows = (0..300)
        .map(|i| {
            let mut h = hook(&format!("hook-{i}"), "preToolUse");
            h["sourcePath"] = json!(format!("/native/{}", "x".repeat(3500)));
            h
        })
        .collect();
    let changes = e.finish_startup_inventory("s", "epoch", "hooks", Some(&hooks(rows)));
    let s = value(&changes);
    assert!(s["extensions"].as_array().unwrap().len() < 256);
    assert!(serde_json::to_vec(s).unwrap().len() < vqro::codex::ENTITY_BYTES);
    assert_eq!(
        e.facts()["data"]["agent"]["caps"]["agent.customize"],
        json!({"extension":{"ops":["list"],"effective":"now"}})
    );
    assert!(
        transform(&e).is_null(),
        "tool transformers alone do not prove input transform"
    );
}

#[test]
fn stale_or_foreign_hook_inventory_cannot_change_current_facts() {
    let mut e = endpoint();
    let data = hooks(vec![hook("input", "userPromptSubmit")]);
    assert!(
        e.finish_startup_inventory("s", "old", "hooks", Some(&data))
            .is_empty()
    );
    assert!(
        e.finish_startup_inventory("other", "epoch", "hooks", Some(&data))
            .is_empty()
    );
    assert!(transform(&e).is_null());
    e.lifecycle = "ended".into();
    assert!(
        e.finish_startup_inventory("s", "epoch", "hooks", Some(&data))
            .is_empty()
    );
}

#[test]
fn managed_mcp_hook_is_native_positive_and_missing_executor_details_are_unknown() {
    let mut e = endpoint();
    let mut row = hook("managed-key", "userPromptSubmit");
    row["handlerType"] = json!("mcpTool");
    row["server"] = json!("private-server");
    row["tool"] = json!("private-tool");
    row["isManaged"] = json!(true);
    row["trustStatus"] = json!("managed");
    row["source"] = json!("sessionFlags");
    let changes = e.finish_startup_inventory("s", "epoch", "hooks", Some(&hooks(vec![row])));
    assert_eq!(transform(&e)["state"], "may");
    assert_eq!(value(&changes)["extensions"][0]["scope"], "session");
    assert!(!value(&changes).to_string().contains("private-server"));
    let mut fresh = endpoint();
    let mut row = hook("incomplete", "userPromptSubmit");
    row.as_object_mut().unwrap().remove("command");
    let changes = fresh.finish_startup_inventory("s", "epoch", "hooks", Some(&hooks(vec![row])));
    assert!(transform(&fresh).is_null());
    assert!(value(&changes)["extensions"][0].get("transforms").is_none());
}

#[test]
fn loaded_hooks_report_documented_potential_and_not_guessed_names_or_commands() {
    let mut e = endpoint();
    let changes = e.finish_startup_inventory(
        "s",
        "epoch",
        "hooks",
        Some(&hooks(vec![
            hook("native-input-key", "userPromptSubmit"),
            hook("native-tool-key", "preToolUse"),
        ])),
    );
    let s = value(&changes);
    assert_eq!(
        s["extensions"],
        json!([
            {"name":"native-input-key","kind":"hook","path":"/native/hooks.json","scope":"user","state":"enabled","transforms":["input"]},
            {"name":"native-tool-key","kind":"hook","path":"/native/hooks.json","scope":"user","state":"enabled","transforms":["tool_calls"]}
        ])
    );
    assert_eq!(s["sources"]["extensions"], "rpc:hooks/list");
    assert_eq!(
        transform(&e),
        json!({"state":"may","names":["native-input-key"],"names_complete":false})
    );
    assert!(!s.to_string().contains("private command"));
}

#[test]
fn missing_disabled_untrusted_modified_and_unknown_hooks_never_claim_none() {
    let mut e = endpoint();
    for mut row in [
        hook("disabled", "userPromptSubmit"),
        hook("untrusted", "userPromptSubmit"),
        hook("modified", "userPromptSubmit"),
        hook("future", "futureEvent"),
        hook("reserved", "userPromptSubmit"),
    ] {
        match row["key"].as_str().unwrap() {
            "disabled" => row["enabled"] = json!(false),
            "untrusted" => row["trustStatus"] = json!("untrusted"),
            "modified" => row["trustStatus"] = json!("modified"),
            "reserved" => row["handlerType"] = json!("prompt"),
            _ => {}
        }
        e.finish_startup_inventory("s", "epoch", "hooks", Some(&hooks(vec![row])));
        assert!(transform(&e).is_null());
    }
    e.finish_startup_inventory("s", "epoch", "hooks", Some(&hooks(vec![])));
    assert!(
        transform(&e).is_null(),
        "an empty snapshot is not a lasting no-transform guarantee"
    );
    let changes = e.finish_startup_inventory("s", "epoch", "hooks", None);
    assert!(value(&changes).get("extensions").is_none());
    assert!(
        value(&changes)["incomplete"]
            .as_array()
            .unwrap()
            .contains(&json!("extensions"))
    );
    assert!(transform(&e).is_null());
}

#[test]
fn positive_native_evidence_survives_stale_or_failed_snapshots_until_new_epoch() {
    let mut e = endpoint();
    e.finish_startup_inventory(
        "s",
        "epoch",
        "hooks",
        Some(&hooks(vec![hook("input", "userPromptSubmit")])),
    );
    let before = transform(&e);
    e.finish_startup_inventory("s", "epoch", "hooks", Some(&hooks(vec![])));
    e.finish_startup_inventory("s", "epoch", "hooks", None);
    assert_eq!(transform(&e), before);
    assert_eq!(before["state"], "may");
    e.set_session_response(
        &json!({"thread":{"id":"s","status":{"type":"idle"}}}),
        "new".into(),
    )
    .unwrap();
    assert!(transform(&e).is_null());
}

#[test]
fn over_bound_identity_is_omitted_but_never_becomes_safety_or_truncated_path() {
    let mut e = endpoint();
    let mut row = hook(&"n".repeat(129), "userPromptSubmit");
    row["sourcePath"] = json!("x".repeat(4097));
    let changes = e.finish_startup_inventory("s", "epoch", "hooks", Some(&hooks(vec![row])));
    let s = value(&changes);
    assert_eq!(s["extensions"], json!([]));
    assert!(
        s["incomplete"]
            .as_array()
            .unwrap()
            .contains(&json!("extensions"))
    );
    assert_eq!(transform(&e)["state"], "may");
    assert_eq!(transform(&e)["names_complete"], false);
    assert!(transform(&e).get("names").is_none());
}

#[test]
fn delivery_labels_are_internal_possibilities_frozen_at_first_dispatch() {
    let mut e = endpoint();
    let args = json!({"origin":{"principal":"p"},"may_transform":["forged"]});
    let first = e.record_receipt("before", "dispatched", None, &args).value;
    assert!(first.get("may_transform").is_none());
    e.finish_startup_inventory(
        "s",
        "epoch",
        "hooks",
        Some(&hooks(vec![hook("input", "userPromptSubmit")])),
    );
    let late = e.record_receipt("before", "admitted", None, &args).value;
    assert!(
        late.get("may_transform").is_none(),
        "late metadata must not backdate dispatch evidence"
    );
    let after = e.record_receipt("after", "dispatched", None, &args).value;
    assert_eq!(after["may_transform"], "unknown");
    e.set_session_response(
        &json!({"thread":{"id":"s2","status":{"type":"idle"}}}),
        "new".into(),
    )
    .unwrap();
    let late = e.record_receipt("after", "unknown", None, &args).value;
    assert_eq!(late["may_transform"], "unknown");
    assert_eq!(e.receipt("after")["may_transform"], "unknown");
    assert!(!late.to_string().contains("forged"));
}
