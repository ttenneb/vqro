//! Agent-v0 sender binding and framing shared by native adapters. No delivery or approval policy.
use crate::principal::Attribution;
use serde_json::{Value, json};

#[derive(Debug)]
pub struct Invalid {
    pub kind: &'static str,
    pub message: &'static str,
}
fn invalid(message: &'static str) -> Invalid {
    Invalid {
        kind: "invalid",
        message,
    }
}
fn forbidden(message: &'static str) -> Invalid {
    Invalid {
        kind: "forbidden",
        message,
    }
}
pub struct Frame {
    pub origin: Value,
    pub quoted: Option<Value>,
    pub rendering: &'static str,
    pub header: Option<String>,
}

fn text(value: &Value, max: usize) -> Result<&str, Invalid> {
    let value = value
        .as_str()
        .filter(|s| !s.is_empty() && s.len() <= max)
        .ok_or_else(|| invalid("frame metadata must be bounded nonempty text"))?;
    if crate::limits::text_controls(value) || value.contains(['\u{2028}', '\u{2029}']) {
        return Err(invalid(
            "frame metadata must be single-line text without control or format characters",
        ));
    }
    Ok(value)
}

/// `owner` is the current public doc/principals/owner entry, not a caller's owner claim.
pub fn frame(args: &Value, attribution: &Attribution, owner: &Value) -> Result<Frame, Invalid> {
    let mut origin = args["origin"]
        .as_object()
        .cloned()
        .ok_or_else(|| invalid("effective origin is required"))?;
    for (field, recorded) in [
        ("principal", attribution.principal.as_deref()),
        ("actor", attribution.actor.as_deref()),
    ] {
        let actual = recorded.unwrap_or(crate::principal::UNDECLARED);
        if let Some(supplied) = origin.get(field) {
            if text(supplied, 256)? != actual {
                return Err(forbidden("origin does not match core-recorded caller"));
            }
        }
        origin.insert(field.into(), json!(actual));
        text(&origin[field], 256)?;
    }
    if let Some(supplied) = origin.get("initiator") {
        if Some(text(supplied, 256)?) != attribution.initiator.as_deref() {
            return Err(forbidden(
                "origin initiator does not match core attribution",
            ));
        }
    }
    if let Some(initiator) = &attribution.initiator {
        origin.insert("initiator".into(), json!(initiator));
        text(&origin["initiator"], 256)?;
    }
    text(&origin.get("kind").cloned().unwrap_or(Value::Null), 128)?;
    if let Some(agent) = origin.get("agent") {
        text(agent, 128)?;
    }
    let sender = if let Some(sender) = origin.get("sender") {
        Some(
            text(
                if sender.is_string() {
                    sender
                } else {
                    &sender["principal"]
                },
                256,
            )?
            .to_owned(),
        )
    } else {
        None
    };
    let claims: Vec<_> = ["kind", "agent", "sender"]
        .into_iter()
        .filter(|field| origin.contains_key(*field))
        .collect();
    origin.insert("claimed".into(), json!(claims));
    if crate::limits::encoded_bytes(&Value::Object(origin.clone())) > 2048 {
        return Err(invalid("origin exceeds2048 bytes"));
    }
    let quoted = args.get("quoted").cloned();
    if let Some(quoted) = &quoted {
        if !quoted.is_object() || crate::limits::encoded_bytes(quoted) > 2048 {
            return Err(invalid("quoted provenance must be a bounded object"));
        }
        text(&quoted["label"], 1024)?;
        for field in ["principal", "kind"] {
            if let Some(value) = quoted.get(field) {
                text(value, 128)?;
            }
        }
    }
    let proven_owner = owner["value"]
        .as_str()
        .filter(|owner| crate::principal::validate_assertable(owner).is_ok())
        .is_some_and(|owner| attribution.principal.as_deref() == Some(owner));
    let human_owner = proven_owner && origin["kind"] == "human";
    let default = json!(["participant", "attributed_user"]);
    let render = args
        .get("render")
        .unwrap_or(&default)
        .as_array()
        .filter(|r| r.len() <= 16 && r.iter().all(Value::is_string))
        .ok_or_else(|| invalid("render must be a bounded array of names"))?;
    let rendering = render
        .iter()
        .find_map(|mode| match mode.as_str() {
            Some("user") if human_owner => Some("user"),
            Some("attributed_user") => Some("attributed_user"),
            _ => None,
        })
        .ok_or(Invalid {
            kind: "unsupported",
            message: "none of the requested renderings is supported",
        })?;
    let header = if human_owner {
        None
    } else {
        let from = sender
            .as_deref()
            .unwrap_or(origin["principal"].as_str().unwrap());
        let mut header = format!("from {from} via {}", origin["actor"].as_str().unwrap());
        if sender.is_some() {
            header.push_str(" (sender asserted by extension)");
        }
        for field in ["kind", "agent"] {
            if let Some(value) = origin.get(field).and_then(Value::as_str) {
                header.push_str(&format!("\nasserted {field}: {value}"));
            }
        }
        if let Some(quoted) = &quoted {
            header.push_str(&format!(
                "\nquoted provenance (asserted): {}",
                quoted["label"].as_str().unwrap()
            ));
        }
        header.push_str("\n\n");
        Some(header)
    };
    Ok(Frame {
        origin: Value::Object(origin),
        quoted,
        rendering,
        header,
    })
}
