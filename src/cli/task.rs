use std::time::Duration;

use crate::api::client::ApiClient;
use crate::api::schema::{ExtensionActionInvokeParams, Method, Request};
use serde_json::{json, Value};

pub(super) fn run_task_command(args: &[String]) -> std::io::Result<i32> {
    let (mut payload, mutation) = match parse_payload(args) {
        Ok(parsed) => parsed,
        Err(message) => {
            eprintln!("{message}");
            return Ok(2);
        }
    };
    let action = payload["action"].as_str().unwrap_or("").to_string();
    if mutation && payload.get("operation_id").is_none() {
        let mut bytes = [0_u8; 16];
        crate::platform::fill_secure_random(&mut bytes)?;
        payload["operation_id"] = bytes
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
            .into();
    }
    let op = payload["operation_id"].as_str().unwrap_or("").to_string();
    let client = super::target::api_client()?;
    let original_caller = if mutation {
        // A read-only preflight captures the original scope for lost-reply lookup.
        // It grants no authority: the extension compares this precondition with
        // the immutable caller supplied afresh by the host on the write.
        match invoke(&client, json!({"action": "whoami"})) {
            Ok(response) => match decode_reply(&response) {
                TaskReply::Success(value)
                    if matches!(
                        value["caller"]["class"].as_str(),
                        Some("agent" | "human_unverified")
                    ) =>
                {
                    Some(value["caller"].clone())
                }
                _ => {
                    eprintln!("task caller unavailable; no write attempted");
                    return Ok(1);
                }
            },
            Err(_) => {
                eprintln!("task caller unavailable; no write attempted");
                return Ok(1);
            }
        }
    } else {
        None
    };
    if let Some(caller) = &original_caller {
        payload["expected_caller"] = caller.clone();
    }
    let reply = match invoke(&client, payload) {
        Ok(response) => decode_reply(&response),
        Err(error) => TaskReply::Unknown(error.to_string()),
    };
    match reply {
        TaskReply::Success(value) => {
            println!("{value}");
            if action == "op" && value["authoritative"] != true {
                eprintln!("not an authoritative result for the original caller; use --caller with its binding");
                return Ok(2);
            }
            Ok(0)
        }
        TaskReply::Rejected(message) => {
            eprintln!("{message}");
            Ok(1)
        }
        TaskReply::Unknown(message) => {
            if mutation {
                eprintln!("outcome unknown; op {op}");
                if let Some(caller) = original_caller {
                    eprintln!("original caller: {caller}");
                    eprintln!("lookup first: vqro task op {op} --caller '{caller}'");
                }
            } else {
                eprintln!("task read unavailable: {message}");
            }
            Ok(1)
        }
    }
}

fn invoke(client: &ApiClient, payload: Value) -> Result<Value, crate::api::client::ApiClientError> {
    client.request_value_with_timeout(
        &Request {
            id: "cli:task".into(),
            method: Method::ExtensionActionInvoke(ExtensionActionInvokeParams {
                service_id: "vqro.tasks.core".into(),
                payload,
            }),
        },
        Duration::from_secs(6),
    )
}

enum TaskReply {
    Success(Value),
    Rejected(String),
    Unknown(String),
}

fn decode_reply(response: &Value) -> TaskReply {
    if let Some(error) = response.get("error") {
        let message = error["message"]
            .as_str()
            .unwrap_or("invalid action error")
            .to_string();
        return if matches!(
            error["code"].as_str(),
            Some("action_rejected" | "invalid_request" | "caller_unknown")
        ) {
            TaskReply::Rejected(message)
        } else {
            TaskReply::Unknown(message)
        };
    }
    if response["result"]["type"] != "extension_action_invoked" {
        return TaskReply::Unknown("unexpected action response".into());
    }
    let result = &response["result"]["value"];
    match result["ok"].as_bool() {
        Some(true) if result.get("value").is_some() => TaskReply::Success(result["value"].clone()),
        Some(false) if result["error"].is_string() => {
            TaskReply::Rejected(result["error"].as_str().unwrap_or("").into())
        }
        _ => TaskReply::Unknown("invalid task outcome".into()),
    }
}

fn parse_payload(args: &[String]) -> Result<(Value, bool), String> {
    let action = args.first().map(String::as_str).ok_or_else(usage)?;
    let mut payload = json!({"action": action});
    let mutation = action == "create";
    let remaining = match (action, args.get(1)) {
        ("list" | "whoami", _) => &args[1..],
        ("get", Some(id)) if args.len() == 2 => {
            payload["id"] = id.clone().into();
            &args[2..]
        }
        ("op", Some(id)) => {
            payload["operation_id"] = id.clone().into();
            &args[2..]
        }
        ("create", Some(title)) => {
            payload["title"] = title.clone().into();
            &args[2..]
        }
        _ => return Err(usage()),
    };
    let mut seen = std::collections::HashSet::new();
    for pair in remaining.chunks(2) {
        let [flag, value] = pair else {
            return Err(usage());
        };
        if !seen.insert(flag.as_str()) {
            return Err("duplicate option".into());
        }
        match (action, flag.as_str()) {
            ("create", "--parent") => payload["parent"] = value.clone().into(),
            ("create", "--op") => payload["operation_id"] = value.clone().into(),
            ("list", "--after") => payload["after"] = value.clone().into(),
            ("op", "--caller") => {
                payload["lookup_caller"] = serde_json::from_str(value)
                    .map_err(|_| "--caller must be the original binding JSON".to_string())?
            }
            _ => return Err(usage()),
        }
    }
    Ok((payload, mutation))
}

fn usage() -> String {
    "usage: vqro task <create TITLE [--parent ID] [--op ID]|list [--after ID]|whoami|get ID|op ID [--caller JSON]>".into()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn receipt_lookup_selector_is_not_a_caller_authority_flag() {
        let args = [
            "op",
            "op-1",
            "--caller",
            r#"{"class":"agent","terminal_id":"term_a","agent":"pi","execution":"boot:1:2"}"#,
        ]
        .map(String::from);
        let (payload, mutation) = parse_payload(&args).unwrap();
        assert!(!mutation);
        assert!(payload.get("lookup_caller").is_some());
        assert!(payload.get("caller").is_none());
    }
    #[test]
    fn lost_or_malformed_reply_never_becomes_definite_rejection() {
        for response in [
            json!({"error":{"code":"outcome_unknown","message":"lost"}}),
            json!({}),
            json!({"result":{"type":"extension_action_invoked","value":{"ok":true}}}),
        ] {
            assert!(matches!(decode_reply(&response), TaskReply::Unknown(_)));
        }
        assert!(matches!(
            decode_reply(&json!({"error":{"code":"action_rejected","message":"not admitted"}})),
            TaskReply::Rejected(_)
        ));
        assert!(matches!(
            decode_reply(
                &json!({"result":{"type":"extension_action_invoked","value":{"ok":false,"error":"stale revision"}}})
            ),
            TaskReply::Rejected(_)
        ));
    }
}
