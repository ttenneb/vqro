use serde_json::{Value, json};
use vqro::codex::{Endpoint, configuration};
fn catalog() -> Vec<Value> {
    let mut choices = Vec::new();
    configuration::page(&json!({"data":[{"id":"ui-one","model":"native-one","displayName":"One"},{"id":"ui-two","model":"native-two","displayName":"Two"}],"nextCursor":null}),&mut choices).unwrap();
    choices
}
fn endpoint() -> Endpoint {
    let mut e = Endpoint::new("slot".into(), "inc".into(), "host".into(), "e1".into());
    e.install_models(catalog());
    e.set_session_response(
        &json!({"thread":{"id":"s1","status":{"type":"idle"}},"model":"native-one"}),
        "e2".into(),
    )
    .unwrap();
    e
}
fn send() -> Value {
    json!({"v":0,"mode":"admit","delivery":"d","origin":{"principal":"p","actor":"actor","kind":"agent"},"content":[{"type":"text","text":"literal"}]})
}
#[test]
fn catalog_uses_native_slug_and_refuses_unbounded_or_malformed_data() {
    assert_eq!(catalog()[0], json!({"value":"native-one","label":"One"}));
    for value in [
        json!({}),
        json!({"data":[],"nextCursor":true}),
        json!({"data":[{"id":"ui","displayName":"One"}]}),
        json!({"data":[{"model":"native\nforged","displayName":"One"}]}),
        json!({"data":[{"model":"native","displayName":"x".repeat(257)}]}),
    ] {
        assert!(configuration::page(&value, &mut Vec::new()).is_err());
    }
    let row = json!({"model":"same","displayName":"Same"});
    assert!(configuration::page(&json!({"data":[row.clone(),row]}), &mut Vec::new()).is_err());
    let rows: Vec<_> = (0..65)
        .map(|i| json!({"model":format!("m{i}"),"displayName":"Model"}))
        .collect();
    assert!(configuration::page(&json!({"data":rows}), &mut Vec::new()).is_err());
    let mut many: Vec<_> = (0..128)
        .map(|i| json!({"value":format!("old{i}"),"label":"Existing"}))
        .collect();
    assert!(
        configuration::page(
            &json!({"data":[{"model":"new","displayName":"New"}]}),
            &mut many
        )
        .is_err()
    );
    let large: Vec<_> = (0..64)
        .map(|i| json!({"model":format!("m{i}{}","x".repeat(230)),"displayName":"x".repeat(240)}))
        .collect();
    assert!(configuration::page(&json!({"data":large}), &mut Vec::new()).is_err());
    assert!(
        configuration::page(
            &json!({"data":[{"model":"native-one","displayName":"One"}]}),
            &mut catalog()
        )
        .is_err()
    );
}
#[test]
fn configuration_guards_precede_recording_future_intent() {
    let mut empty = Endpoint::new("slot".into(), "inc".into(), "host".into(), "e".into());
    assert!(
        empty.facts()["data"]["agent"]["caps"]
            .get("agent.configure")
            .is_none()
    );
    empty.install_models(catalog());
    assert!(!empty.model_config_available());
    let e = endpoint();
    let before = e.facts();
    let option = e.model_option();
    assert_eq!(option["category"], "model");
    for (args, kind) in [
        (
            json!({"v":0,"id":"model","value":"native-two","rev":0}),
            "stale",
        ),
        (
            json!({"v":0,"id":"model","value":"native-two","rev":true}),
            "stale",
        ),
        (json!({"v":0,"id":"model","value":"invented"}), "invalid"),
        (json!({"v":0,"id":"model","value":true}), "invalid"),
        (
            json!({"v":0,"id":"other","value":"native-two"}),
            "unsupported",
        ),
    ] {
        assert_eq!(e.prepare_model(&args).unwrap_err().kind(), kind);
        assert_eq!(e.facts(), before);
    }
    assert_eq!(
        e.prepare_model(&json!({"v":0,"id":"model","value":"native-two","rev":option["rev"]}))
            .unwrap(),
        "native-two"
    );
}
#[test]
fn intent_is_pending_until_actual_native_thread_readback_not_request_or_ack() {
    let mut e = endpoint();
    let old = e.session_info();
    e.select_model("native-two".into());
    assert_eq!(e.session_info(), old);
    assert_eq!(e.model_option()["value"], "native-two");
    assert_eq!(e.model_option()["pending"], true);
    assert_eq!(e.model_option()["applies_from"], "next_turn");
    let call = e.prepare_send(&send()).unwrap();
    assert_eq!(call.params["model"], "native-two");
    assert_eq!(call.params["input"][1]["text"], "literal");
    e.begin_model_turn();
    assert_eq!(e.session_info()["model"]["id"], "native-one");
    assert_eq!(e.model_option()["pending"], true);
    e.finish_model_readback(
        &json!({"thread":{"id":"s1","model":"native-two"}}),
        "s1",
        "e2",
        "native-two",
    )
    .unwrap();
    assert_eq!(e.session_info()["model"]["id"], "native-two");
    assert_eq!(e.model_option()["pending"], false);
    assert_eq!(e.model_option()["status"], "confirmed");
    assert_eq!(
        e.model_option()["readback_scope"],
        "current_configured_after_turn_dispatch"
    );
}
#[test]
fn mismatch_reports_actual_native_model_without_clearing_intent_or_pending() {
    let mut e = endpoint();
    e.select_model("native-two".into());
    e.begin_model_turn();
    e.finish_model_readback(
        &json!({"thread":{"id":"s1","model":"native-one"}}),
        "s1",
        "e2",
        "native-two",
    )
    .unwrap();
    assert_eq!(e.session_info()["model"]["id"], "native-one");
    assert_eq!(e.model_option()["value"], "native-two");
    assert_eq!(e.model_option()["pending"], true);
    assert_eq!(e.model_option()["status"], "mismatch");
    assert_eq!(e.model_option()["diagnostic"]["reported"], "native-one");
}
#[test]
fn unknown_and_stale_readback_never_rebind_revive_or_fabricate_applied_model() {
    for response in [
        json!({"thread":{"id":"s1"}}),
        json!({"thread":{"id":"other","model":"native-two"}}),
        json!({"thread":{"id":"s1","model":"native-two","status":{"type":"notLoaded"}}}),
    ] {
        let mut e = endpoint();
        e.select_model("native-two".into());
        e.begin_model_turn();
        let before = e.facts();
        assert!(
            e.finish_model_readback(&response, "s1", "e2", "native-two")
                .is_err()
        );
        assert_eq!(e.facts(), before);
        e.model_unknown(json!({"kind":"unknown"}));
        assert_eq!(e.session_info()["id"], "s1");
        assert!(e.session_info().get("model").is_none());
        assert_eq!(e.model_option()["pending"], true);
        assert_eq!(e.model_option()["status"], "unknown");
        assert_eq!(
            e.prepare_send(&send()).unwrap().params["model"],
            "native-two"
        );
    }
    let mut e = endpoint();
    e.select_model("native-two".into());
    e.lifecycle = "ended".into();
    let before = e.facts();
    assert!(
        e.finish_model_readback(
            &json!({"thread":{"id":"s1","model":"native-two"}}),
            "s1",
            "e2",
            "native-two"
        )
        .is_err()
    );
    assert_eq!(e.facts(), before);
}
#[test]
fn new_session_keeps_future_intent_but_reports_its_actual_default_model() {
    let mut e = endpoint();
    e.select_model("native-two".into());
    e.set_session_response(
        &json!({"thread":{"id":"s2","status":{"type":"idle"}},"model":"native-one"}),
        "e3".into(),
    )
    .unwrap();
    assert_eq!(e.session_info()["model"]["id"], "native-one");
    assert_eq!(e.model_option()["value"], "native-two");
    assert_eq!(e.model_option()["pending"], true);
    assert_eq!(e.prepare_send(&send()).unwrap().params["threadId"], "s2");
    assert_eq!(
        e.prepare_send(&send()).unwrap().params["model"],
        "native-two"
    );
}
