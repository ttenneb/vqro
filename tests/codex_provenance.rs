use serde_json::{Value, json};
use vqro::codex::{Endpoint, capabilities, provider_capabilities};
#[path = "support/frame_metadata.rs"]
mod frame_metadata;

fn endpoint() -> Endpoint {
    let mut e = Endpoint::new("slot".into(), "inc".into(), "server".into(), "e".into());
    e.set_session(&json!({"id":"s","status":{"type":"idle"}}), "e2".into())
        .unwrap();
    e
}
fn send() -> Value {
    json!({"v":0,"mode":"admit","delivery":"d","origin":{"principal":"p:sender","actor":"actor:sender","kind":"agent"},
        "content":[{"type":"text","text":"first"},{"type":"text","text":"second"}]})
}

#[test]
fn declared_topology_request_scopes_and_ordered_content_select_the_right_cases() {
    let c = capabilities();
    let provider = provider_capabilities();
    assert_eq!(provider["host"]["kind"], json!(["server"]));
    assert_eq!(
        provider["agent.send"]["content"]["shape"],
        json!(["ordered"])
    );
    assert_eq!(provider["agent.send"]["content"]["types"], json!(["text"]));
    assert_eq!(provider["agent.send"]["origin"][0]["role"], json!(["user"]));
    assert_eq!(provider["agent.send"]["receipts"]["retain"], 256);
    assert_eq!(
        c["agent"]["modules"],
        json!({"sessions":{"v":0},"config":{"v":0}})
    );
    assert_eq!(c["host"], json!({"kind":"server","endpoints":"many"}));
    assert_eq!(c["agent.respond"]["scopes"], json!(["session", "run"]));
    assert_eq!(c["agent.respond"]["background_requests"], true);
    assert_eq!(c["agent.respond"]["native_timeout"], "none");
    assert_eq!(provider["agent.respond"]["native_timeout"], json!(["none"]));
    assert_eq!(
        c["agent.send"]["content"],
        json!({"types":["text"],"shape":"ordered"})
    );
    let call = endpoint().prepare_send(&send()).unwrap();
    assert_eq!(call.params["input"][1]["text"], "first");
    assert_eq!(call.params["input"][2]["text"], "second");
}

#[test]
fn untrusted_frame_metadata_rejects_cc_cf_zl_zp_without_filtering_body() {
    for (label, args) in frame_metadata::invalid_inputs(&send()) {
        assert_eq!(
            endpoint().prepare_send(&args).unwrap_err().kind(),
            "invalid",
            "{label}"
        );
    }
    let mut args = send();
    let body = "literal\n\r\u{2028}\u{2029}\u{202e}body";
    args["content"] = json!([{"type":"text","text":body}]);
    assert_eq!(
        endpoint().prepare_send(&args).unwrap().params["input"][1]["text"],
        body
    );
}

#[test]
fn context_or_plain_user_is_not_a_silent_rendering_fallback() {
    let e = endpoint();
    let mut args = send();
    args["render"] = json!(["context"]);
    assert_eq!(e.prepare_send(&args).unwrap_err().kind(), "unsupported");
    args["render"] = json!(["user"]);
    assert_eq!(e.prepare_send(&args).unwrap_err().kind(), "unsupported");
    args["render"] = json!(["participant", "attributed_user"]);
    assert!(e.prepare_send(&args).is_ok());
}

#[test]
fn forwarded_author_is_rendered_as_a_claim_not_the_effective_sender() {
    let e = endpoint();
    let mut args = send();
    args["quoted"] = json!({"label":"The workspace owner","principal":"p:quoted","kind":"human"});
    let call = e.prepare_send(&args).unwrap();
    let input = call.params["input"].as_array().unwrap();
    assert!(input[0]["text"].as_str().unwrap().contains("p:sender"));
    assert!(!input[0]["text"].as_str().unwrap().contains("p:quoted"));
    assert!(
        input[1]["text"]
            .as_str()
            .unwrap()
            .contains("not the sender")
    );
    assert_eq!(input[2]["text"], "first");
    assert!(call.params.get("developerInstructions").is_none());
}

#[test]
fn actor_and_cause_qualification_are_not_invented() {
    let e = endpoint();
    let mut args = send();
    args["origin"].as_object_mut().unwrap().remove("actor");
    assert_eq!(e.prepare_send(&args).unwrap_err().kind(), "invalid");
    args = send();
    args["cause"] = json!({"kind":"entity","id":"x","provenance":"asserted"});
    assert_eq!(e.prepare_send(&args).unwrap_err().kind(), "invalid");
    args["cause"] = json!({"kind":"effect","id":"effect-1","boot":"boot-1","provenance":"native"});
    assert!(
        e.prepare_send(&args).is_ok(),
        "open caller provenance remains data"
    );
    let mut e = endpoint();
    let receipt = e.record_receipt("d", "admitted", None, &args);
    assert_eq!(receipt.value["cause"], args["cause"]);
}

#[test]
fn d17_plain_frame_keeps_claims_asserted_and_proved_owner_body_byte_exact() {
    let attribution = serde_json::from_value(
        json!({"principal":"human:D17-principal","actor":"extension:coord","initiator":"p:worker"}),
    )
    .unwrap();
    let mut args = send();
    args["origin"] = json!({"kind":"agent","agent":"D17-asserted-agent","sender":{"principal":"p:inbox-original","provenance":"inbox-record"}});
    let bound = vqro::codex::bind_origin(&args, &attribution).unwrap();
    let call = endpoint().prepare_send(&bound).unwrap();
    assert_eq!(
        call.params["input"][0]["text"],
        "from p:inbox-original via extension:coord (sender asserted by extension)\nasserted kind: agent\nasserted agent: D17-asserted-agent"
    );
    assert_eq!(call.params["input"][1]["text"], "first");
    assert_eq!(call.params["input"][2]["text"], "second");
    let owner = json!({"value":"human:D17-principal"});
    assert!(vqro::codex::recorded_owner(&attribution, &owner));
    for invalid in [
        Value::Null,
        json!({}),
        json!({"value":17}),
        json!({"value":"owner-unset"}),
        json!({"value":"other"}),
    ] {
        assert!(!vqro::codex::recorded_owner(&attribution, &invalid));
    }
    assert!(!vqro::codex::recorded_owner(&Default::default(), &owner));
    for text in [
        "Hello?",
        "  /literal\n\nunchanged \t\n",
        "{\"typed\":\"JSON stays literal\"}",
        "",
    ] {
        args["origin"] = json!({"kind":"human","owner_proven":true});
        args["quoted"] = json!({"label":"asserted quote"});
        args["content"] = json!([{"type":"text","text":text}]);
        let bound = vqro::codex::bind_origin(&args, &attribution).unwrap();
        let call = endpoint()
            .prepare_send_with_context(&bound, vqro::codex::SendContext { owner_proven: true })
            .unwrap();
        let input = call.params["input"].as_array().unwrap();
        assert_eq!(input.len(), 1);
        assert_eq!(
            input[0]["text"].as_str().unwrap().as_bytes(),
            text.as_bytes()
        );
        let unproved = endpoint().prepare_send(&bound).unwrap();
        assert_eq!(
            unproved.params["input"][0]["text"],
            "from human:D17-principal via extension:coord\nasserted kind: human"
        );
    }
    args["origin"] = json!({"kind":"agent"});
    let bound = vqro::codex::bind_origin(&args, &attribution).unwrap();
    let claimed_agent = endpoint()
        .prepare_send_with_context(&bound, vqro::codex::SendContext { owner_proven: true })
        .unwrap();
    assert_eq!(
        claimed_agent.params["input"][0]["text"],
        "from human:D17-principal via extension:coord\nasserted kind: agent"
    );
}

#[test]
fn d17_bound_origin_survives_every_receipt_stage_and_lookup_without_invention() {
    let attribution = serde_json::from_value(
        json!({"principal":"human:D17-principal","actor":"extension:coord","initiator":"p:worker"}),
    )
    .unwrap();
    let mut args = send();
    args["origin"] = json!({"kind":"agent","agent":"D17-asserted-agent",
        "sender":{"principal":"p:inbox-original","provenance":"inbox-record"},
        "claimed":["principal","actor","initiator"]});
    let mut bound = vqro::codex::bind_origin(&args, &attribution).unwrap();
    let origin = bound["origin"].clone();
    assert_eq!(origin["principal"], "human:D17-principal");
    assert_eq!(origin["actor"], "extension:coord");
    assert_eq!(origin["initiator"], "p:worker");
    assert_eq!(origin["claimed"], json!(["kind", "agent", "sender"]));
    let mut endpoint = endpoint();
    for stage in ["dispatched", "admitted", "rejected", "unknown"] {
        let delivery = format!("d17-{stage}");
        let change = endpoint.record_receipt(&delivery, stage, None, &bound);
        assert_eq!(change.value["origin"], origin);
        assert_eq!(endpoint.receipt(&delivery)["origin"], origin);
    }
    assert!(endpoint.receipt("never-submitted").get("origin").is_none());
    bound["origin"]["actor"] = json!("different");
    assert_eq!(
        endpoint.receipt("d17-admitted")["origin"],
        origin,
        "receipt owns its immutable bound-origin snapshot"
    );
}
