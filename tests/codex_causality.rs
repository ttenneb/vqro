//! A2 must accept exactly the generic causal wire contract, without upgrading caller labels.
use serde_json::{Value, json};
use vqro::causality::Causality;
use vqro::codex::Endpoint;

fn endpoint() -> Endpoint {
    let mut endpoint = Endpoint::new("slot".into(), "inc".into(), "host".into(), "e".into());
    endpoint
        .set_session(&json!({"id":"s","status":{"type":"idle"}}), "e2".into())
        .unwrap();
    endpoint
}

fn send() -> Value {
    json!({"v":0,"mode":"admit","delivery":"d","origin":{"principal":"p:test","actor":"actor:test","kind":"tool"},
        "content":[{"type":"text","text":"literal input"}]})
}

fn sized_cause(bytes: usize) -> Value {
    let mut cause = json!({"kind":"future","id":"id","provenance":"future","extra":""});
    let overhead = vqro::limits::encoded_bytes(&cause);
    cause["extra"] = json!("x".repeat(bytes - overhead));
    assert_eq!(vqro::limits::encoded_bytes(&cause), bytes);
    cause
}

fn nested_cause(arrays: usize) -> Value {
    let mut extra = Value::Null;
    for _ in 0..arrays {
        extra = json!([extra]);
    }
    json!({"kind":"future","id":"id","provenance":"future","extra":extra})
}

#[test]
fn generic_open_provenance_and_exact_bounds_survive_receipts_unchanged() {
    let causes = [
        json!({"kind":"effect","id":"prior-effect","boot":"prior-boot","provenance":"native"}),
        json!({"kind":"entity","id":"entity","endpoint":"other-endpoint","epoch":"prior-epoch","provenance":"adapter"}),
        json!({"kind":"request","id":"request","endpoint":"endpoint","epoch":"epoch","provenance":"future"}),
        json!({"kind":"future-kind","id":"é".repeat(128),"provenance":"x".repeat(256),"extra":{"opaque":[true, 1, null]}}),
        sized_cause(1025),
        sized_cause(4096),
        nested_cause(63),
    ];
    for cause in causes {
        let mut endpoint = endpoint();
        let mut args = send();
        args["cause"] = cause.clone();
        args["correlation"] = json!("c".repeat(256));
        Causality::parse(&args).expect("within the generic contract");
        let before = args.clone();
        endpoint
            .prepare_send(&args)
            .expect("A2 must not impose a narrower undeclared bound");
        assert_eq!(args, before);
        let receipt = endpoint.record_receipt("d", "admitted", None, &args);
        assert_eq!(receipt.value["cause"], cause);
        assert_eq!(receipt.value["correlation"], args["correlation"]);
    }
}

#[test]
fn generic_invalid_causality_rejects_without_mutating_endpoint() {
    let mut invalid = vec![
        Value::Null,
        json!([]),
        json!({}),
        sized_cause(4097),
        nested_cause(64),
    ];
    for (field, bad) in [
        ("kind", json!("")),
        ("kind", json!("x".repeat(257))),
        ("id", json!(false)),
        ("id", json!("é".repeat(129))),
        ("provenance", json!(null)),
        ("provenance", json!("x".repeat(257))),
        ("provenance", json!("native\n")),
        ("endpoint", json!("x".repeat(257))),
        ("epoch", json!([])),
        ("boot", json!("")),
    ] {
        let mut cause = json!({"kind":"future","id":"id","provenance":"future"});
        cause[field] = bad;
        invalid.push(cause);
    }
    for cause in [
        json!({"kind":"entity","id":"id","provenance":"native"}),
        json!({"kind":"request","id":"id","endpoint":"endpoint","provenance":"adapter"}),
        json!({"kind":"effect","id":"id","provenance":"future"}),
    ] {
        invalid.push(cause);
    }
    let endpoint = endpoint();
    let before = endpoint.facts();
    for cause in invalid {
        let mut args = send();
        args["cause"] = cause;
        assert_eq!(Causality::parse(&args).unwrap_err().kind(), "invalid");
        assert_eq!(endpoint.prepare_send(&args).unwrap_err().kind(), "invalid");
        assert_eq!(endpoint.facts(), before);
    }
    for correlation in [
        Value::Null,
        json!(false),
        json!(""),
        json!("x".repeat(257)),
        json!("c\n"),
    ] {
        let mut args = send();
        args["correlation"] = correlation;
        assert_eq!(Causality::parse(&args).unwrap_err().kind(), "invalid");
        assert_eq!(endpoint.prepare_send(&args).unwrap_err().kind(), "invalid");
        assert_eq!(endpoint.facts(), before);
    }
}
