//! Validate agent-v0 input before any native write; never turn data into a native command.
use super::state::State;
use crate::{causality::Causality, principal::Attribution, provider_client::agent};
use serde_json::{Value, json};
#[derive(Debug)]
pub struct Refusal {
    pub kind: &'static str,
    pub message: &'static str,
}
fn fail(kind: &'static str, message: &'static str) -> Refusal {
    Refusal { kind, message }
}
pub fn id(value: &Value) -> Result<&str, Refusal> {
    value
        .as_str()
        .filter(|s| !s.is_empty() && s.len() <= 128)
        .ok_or_else(|| {
            fail(
                "invalid",
                "identifier must be nonempty text of at most128 bytes",
            )
        })
}
pub fn common(args: &Value, state: &State) -> Result<(), Refusal> {
    if !args.is_object() || args["v"] != 0 {
        return Err(fail("invalid", "v:0 object arguments are required"));
    }
    if crate::limits::json_depth(args, 64).is_err() {
        return Err(fail("invalid", "arguments exceed64 containers"));
    }
    if let Some(rev) = args.get("caps_rev") {
        if rev.as_u64() != Some(state.caps_rev()) {
            return Err(fail("stale", "capabilities changed"));
        }
    }
    if let Some(epoch) = args.get("epoch") {
        if id(epoch)? != state.epoch() {
            return Err(fail("stale", "observation epoch changed"));
        }
    }
    if let Some(guard) = args.get("guard") {
        let guard = guard
            .as_object()
            .ok_or_else(|| fail("invalid", "guard must be an object"))?;
        for (key, value) in guard {
            if key != "session" {
                return Err(fail("unsupported", "this guard cannot be enforced"));
            }
            let Some(session) = &state.session else {
                return Err(fail(
                    "unsupported",
                    "native session guard cannot be checked",
                ));
            };
            if id(value)? != session {
                return Err(fail("stale", "native session changed"));
            }
        }
    }
    Causality::parse(args).map_err(|_| fail("invalid", "invalid caller cause/correlation"))?;
    Ok(())
}
pub struct Send {
    pub delivery: String,
    pub uuid: String,
    pub epoch: String,
    pub native: Value,
    pub receipt: Value,
}
pub fn send(
    args: &Value,
    state: &State,
    attribution: &Attribution,
    owner: &Value,
    uuid: String,
) -> Result<Send, Refusal> {
    common(args, state)?;
    if !state.ready || !state.literal_supported() {
        return Err(fail(
            "unsupported",
            "this native CLI has no qualified literal-send path",
        ));
    }
    if args.as_object().unwrap().keys().any(|key| {
        ![
            "v",
            "caps_rev",
            "epoch",
            "guard",
            "cause",
            "correlation",
            "mode",
            "content",
            "delivery",
            "origin",
            "quoted",
            "render",
            "key",
        ]
        .contains(&key.as_str())
    }) {
        return Err(fail("unsupported", "undeclared send argument"));
    }
    if crate::limits::encoded_bytes(args) > 65536 {
        return Err(fail("too_large", "input exceeds declared byte limit"));
    }
    if args.get("key").is_some() {
        return Err(fail(
            "unsupported",
            "native idempotent insertion keys are not declared",
        ));
    }
    if args["mode"] != "admit" {
        return Err(fail(
            "unsupported",
            "only native admit delivery is declared",
        ));
    }
    let delivery = id(&args["delivery"])?.to_owned();
    if state.receipts.contains_key(&delivery) {
        return Err(fail(
            "conflict",
            "delivery already retained; look it up, never resend",
        ));
    }
    if state.receipts.len() >= 256 && state.receipts.keys().all(|id| state.delivery_active(id)) {
        return Err(fail("overloaded", "all retained deliveries remain active"));
    }
    let frame = agent::frame(args, attribution, owner).map_err(|e| fail(e.kind, e.message))?;
    // C2 already lifts these into recorded attribution. Refuse conflicts even on a fake/misbehaving peer.
    for (field, recorded) in [
        ("cause", attribution.causality.cause.clone()),
        (
            "correlation",
            attribution.causality.correlation.as_ref().map(|s| json!(s)),
        ),
    ] {
        if let Some(value) = args.get(field) {
            if Some(value) != recorded.as_ref() {
                return Err(fail(
                    "forbidden",
                    "links do not match core-recorded attribution",
                ));
            }
        }
    }
    let blocks = args["content"]
        .as_array()
        .filter(|a| !a.is_empty() && a.len() <= 32)
        .ok_or_else(|| fail("invalid", "content requires1..32 typed blocks"))?;
    let mut native = Vec::new();
    if let Some(header) = &frame.header {
        native.push(json!({"type":"text","text":header}));
    }
    for block in blocks {
        let object = block
            .as_object()
            .ok_or_else(|| fail("invalid", "content block must be an object"))?;
        if block["type"] != "text" {
            return Err(fail("unsupported", "only declared text input is supported"));
        }
        if object
            .keys()
            .any(|key| !matches!(key.as_str(), "type" | "text" | "format"))
        {
            return Err(fail("unsupported", "undeclared text block field"));
        }
        if block.get("format").is_some_and(|v| !v.is_string()) {
            return Err(fail("invalid", "text format must be a string"));
        }
        let text = block["text"]
            .as_str()
            .ok_or_else(|| fail("invalid", "text block requires text"))?;
        // Presentation formats do not change the bytes or dispatch slash/file-mention expansions.
        native.push(json!({"type":"text","text":text}));
    }
    let mut record = json!({"type":"user","message":{"role":"user","content":native},"parent_tool_use_id":null,
        "uuid":uuid,"client_composed":true,"priority":"later"});
    if let Some(session) = &state.session {
        record["session_id"] = json!(session);
    }
    let mut receipt = json!({"delivery":delivery,"stage":"unknown","mode":"admit","origin":frame.origin,
        "rendering":frame.rendering,"may_transform":"unknown","native":{"client_uuid":uuid}});
    if let Some(quoted) = frame.quoted {
        receipt["quoted"] = quoted;
    }
    attribution.causality.annotate(&mut receipt);
    Ok(Send {
        delivery,
        uuid,
        epoch: state.epoch().into(),
        native: record,
        receipt,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (State, Attribution, Value) {
        let mut state = State::new("boot".into());
        state.initialized(&json!({"claude_code_version":"2.1.329"}));
        let attribution: Attribution =
            serde_json::from_value(json!({"principal":"owner","actor":"client:test"})).unwrap();
        (state, attribution, json!({"value":"owner"}))
    }
    #[test]
    fn literal_blocks_are_not_commands_or_merged_and_labels_cannot_forge_receipts() {
        let (state, attribution, owner) = fixture();
        let args = json!({"v":0,"mode":"admit","delivery":"d1","origin":{"kind":"human"},"content":[{"type":"text","text":"/clear @private"},{"type":"text","text":"second"}]});
        let prepared = send(&args, &state, &attribution, &owner, "uuid1".into()).unwrap();
        assert_eq!(prepared.native["client_composed"], true);
        assert_eq!(prepared.native["message"]["content"], args["content"]);
        assert_eq!(prepared.receipt["may_transform"], "unknown");
        let mut forged = args;
        forged["may_transform"] = json!([]);
        assert_eq!(
            send(&forged, &state, &attribution, &owner, "uuid2".into())
                .err()
                .unwrap()
                .kind,
            "unsupported"
        );
    }
    #[test]
    fn unknown_native_version_does_not_get_a_guessed_literal_guarantee() {
        let (mut state, attribution, owner) = fixture();
        state.version = None;
        let args = json!({"v":0,"mode":"admit","delivery":"d1","origin":{"kind":"human"},"content":[{"type":"text","text":"data"}]});
        assert_eq!(
            send(&args, &state, &attribution, &owner, "uuid1".into())
                .err()
                .unwrap()
                .kind,
            "unsupported"
        );
    }
}
