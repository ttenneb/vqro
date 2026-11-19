use serde_json::{Value, json};
use vqro::codex::{Endpoint, capabilities};

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
    assert_eq!(c["host"], json!({"kind":"server","endpoints":"many"}));
    assert_eq!(c["agent.respond"]["scopes"], json!(["session", "run"]));
    assert_eq!(c["agent.respond"]["background_requests"], true);
    assert_eq!(
        c["agent.send"]["content"],
        json!({"types":["text"],"shape":"ordered"})
    );
    let call = endpoint().prepare_send(&send()).unwrap();
    assert_eq!(call.params["input"][1]["text"], "first");
    assert_eq!(call.params["input"][2]["text"], "second");
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
    assert_eq!(e.prepare_send(&args).unwrap_err().kind(), "unsupported");
    args["cause"]["provenance"] = json!("asserted");
    assert!(e.prepare_send(&args).is_ok());
    let mut e = endpoint();
    let receipt = e.record_receipt("d", "admitted", None, &args);
    assert_eq!(receipt.value["cause"], args["cause"]);
}
