use serde_json::{Value, json};
use vqro::codex::{Endpoint, configuration};

fn catalog() -> Vec<Value> {
    let mut choices = Vec::new();
    configuration::page(
        &json!({"data":[{"id":"ui-one","model":"native-one","displayName":"One"},
        {"id":"ui-two","model":"native-two","displayName":"Two"}],"nextCursor":null}),
        &mut choices,
    )
    .unwrap();
    choices
}
fn endpoint() -> Endpoint {
    let mut endpoint = Endpoint::new("slot".into(), "inc".into(), "host".into(), "e1".into());
    endpoint.install_models(catalog());
    endpoint
        .set_session_response(
            &json!({"thread":{"id":"s1","status":{"type":"idle"}},"model":"native-one"}),
            "e2".into(),
        )
        .unwrap();
    endpoint
}
fn send() -> Value {
    json!({"v":0,"mode":"admit","delivery":"d","origin":{"principal":"p","actor":"actor","kind":"agent"},"content":[{"type":"text","text":"literal"}]})
}

#[test]
fn catalog_uses_native_model_slug_and_refuses_unbounded_or_malformed_data() {
    assert_eq!(catalog()[0], json!({"value":"native-one","label":"One"}));
    for value in [
        json!({}),
        json!({"data":[] ,"nextCursor":true}),
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
    let too_many: Vec<_> = (0..128)
        .map(|i| json!({"value":format!("old{i}"),"label":"Existing"}))
        .collect();
    assert!(
        configuration::page(
            &json!({"data":[{"model":"new","displayName":"New"}]}),
            &mut too_many.clone()
        )
        .is_err()
    );
    let large: Vec<_> = (0..64)
        .map(|i| json!({"model":format!("m{i}{}","x".repeat(230)),"displayName":"x".repeat(240)}))
        .collect();
    assert!(configuration::page(&json!({"data":large}), &mut Vec::new()).is_err());
    let mut choices = catalog();
    assert!(
        configuration::page(
            &json!({"data":[{"model":"native-one","displayName":"One"}]}),
            &mut choices
        )
        .is_err()
    );
}

#[test]
fn configuration_requires_native_catalog_thread_revision_and_choice_before_write() {
    let mut empty = Endpoint::new("slot".into(), "inc".into(), "host".into(), "e".into());
    assert!(
        empty.facts()["data"]["agent"]["caps"]
            .get("agent.configure")
            .is_none()
    );
    empty.install_models(catalog());
    assert!(
        !empty.model_config_available(),
        "a native thread is required; no local future-setting facade"
    );
    let endpoint = endpoint();
    let before = endpoint.facts();
    let option = endpoint.model_option();
    assert_eq!(option["category"], "model");
    assert!(
        before["data"]["agent"]["caps"]
            .get("agent.configure")
            .is_some()
    );
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
        assert_eq!(endpoint.prepare_model(&args).unwrap_err().kind(), kind);
        assert_eq!(endpoint.facts(), before);
    }
    let (value, params) = endpoint
        .prepare_model(&json!({"v":0,"id":"model","value":"native-two","rev":option["rev"]}))
        .unwrap();
    assert_eq!(value, "native-two");
    assert_eq!(
        params,
        json!({"threadId":"s1","model":"native-two","excludeTurns":true})
    );
}

#[test]
fn only_native_matching_readback_confirms_value_and_future_turn_override() {
    let mut endpoint = endpoint();
    let rev = endpoint.model_option()["rev"].clone();
    endpoint.begin_model();
    assert!(endpoint.session_info().get("model").is_none());
    assert!(endpoint.model_option()["value"].is_null());
    assert_eq!(endpoint.prepare_send(&send()).unwrap_err().kind(), "busy");
    let (matched, _) = endpoint
        .finish_model(
            &json!({"thread":{"id":"s1"},"model":"native-two"}),
            "s1",
            "native-two",
        )
        .unwrap();
    assert!(matched);
    assert_eq!(endpoint.session_info()["model"]["id"], "native-two");
    assert_eq!(endpoint.model_option()["value"], "native-two");
    assert_ne!(endpoint.model_option()["rev"], rev);
    let call = endpoint.prepare_send(&send()).unwrap();
    assert_eq!(call.params["model"], "native-two");
    assert_eq!(call.params["input"][1]["text"], "literal");
    endpoint.begin_model();
    let (matched, _) = endpoint
        .finish_model(
            &json!({"thread":{"id":"s1"},"model":"native-one"}),
            "s1",
            "native-two",
        )
        .unwrap();
    assert!(!matched);
    assert_eq!(endpoint.model_option()["value"], "native-one");
    assert!(endpoint.model_override().is_none());
}

#[test]
fn missing_or_wrong_native_readback_stays_unknown_without_rebinding() {
    for response in [
        json!({"thread":{"id":"s1"}}),
        json!({"thread":{"id":"other"},"model":"native-two"}),
    ] {
        let mut endpoint = endpoint();
        endpoint.begin_model();
        assert!(
            endpoint
                .finish_model(&response, "s1", "native-two")
                .is_err()
        );
        endpoint.model_unknown();
        assert_eq!(endpoint.session_info()["id"], "s1");
        assert!(endpoint.session_info().get("model").is_none());
        assert_eq!(endpoint.prepare_send(&send()).unwrap_err().kind(), "busy");
    }
    let mut endpoint = endpoint();
    endpoint.begin_model();
    endpoint.lifecycle = "ended".into();
    assert!(
        endpoint
            .finish_model(
                &json!({"thread":{"id":"s1"},"model":"native-two"}),
                "s1",
                "native-two"
            )
            .is_err()
    );
    assert_eq!(endpoint.lifecycle, "ended");
}
