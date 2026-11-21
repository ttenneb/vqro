//! Core-recorded C2 attribution binds identity; body classification/sender remain claims.
use serde_json::{json, Value};
use vqro::codex::bind_origin;
use vqro::principal::Attribution;

fn attribution() -> Attribution {
    serde_json::from_value(
        json!({"principal":"p:effective","actor":"extension:agents","initiator":"p:original"}),
    )
    .unwrap()
}

#[test]
fn omitted_identity_is_bound_from_typed_attribution_and_claims_stay_claims() {
    let args = json!({"origin":{"kind":"human","agent":"r7","sender":{"principal":"claimed:sender"},"claimed":["actor"]}});
    let original = args.clone();
    let bound = bind_origin(&args, &attribution()).unwrap();
    assert_eq!(args, original, "binding does not mutate caller input");
    assert_eq!(bound["origin"]["principal"], "p:effective");
    assert_eq!(bound["origin"]["actor"], "extension:agents");
    assert_eq!(bound["origin"]["initiator"], "p:original");
    assert_eq!(bound["origin"]["sender"]["principal"], "claimed:sender");
    assert_eq!(
        bound["origin"]["claimed"],
        json!(["kind", "agent", "sender"])
    );
}

#[test]
fn supplied_identity_must_match_every_recorded_field() {
    let args = json!({"origin":{"kind":"human","principal":"p:effective","actor":"extension:agents","initiator":"p:original"}});
    assert!(bind_origin(&args, &attribution()).is_ok());
    for field in ["principal", "actor", "initiator"] {
        let mut forged = args.clone();
        forged["origin"][field] = json!("p:forged");
        assert_eq!(
            bind_origin(&forged, &attribution()).unwrap_err().kind(),
            "forbidden"
        );
        for malformed in [Value::Null, json!(true), json!(9), json!({})] {
            forged["origin"][field] = malformed;
            assert_eq!(
                bind_origin(&forged, &attribution()).unwrap_err().kind(),
                "invalid"
            );
        }
    }
}

#[test]
fn absent_attribution_stays_undeclared_not_a_claimed_owner() {
    let empty: Attribution = serde_json::from_value(json!({})).unwrap();
    let bound = bind_origin(&json!({"origin":{"kind":"human"}}), &empty).unwrap();
    assert_eq!(bound["origin"]["principal"], "undeclared");
    assert_eq!(bound["origin"]["actor"], "undeclared");
    assert!(bound["origin"].get("initiator").is_none());
    assert_eq!(
        bind_origin(
            &json!({"origin":{"kind":"human","principal":"owner"}}),
            &empty
        )
        .unwrap_err()
        .kind(),
        "forbidden"
    );
    assert_eq!(
        bind_origin(
            &json!({"origin":{"kind":"human","initiator":"p:forged"}}),
            &empty
        )
        .unwrap_err()
        .kind(),
        "forbidden"
    );
}

#[test]
fn missing_or_non_object_origin_never_produces_bound_args() {
    for args in [
        json!({}),
        json!({"origin":null}),
        json!({"origin":[]}),
        json!({"origin":"human"}),
    ] {
        assert_eq!(
            bind_origin(&args, &attribution()).unwrap_err().kind(),
            "invalid"
        );
    }
}
