use std::time::Duration;

use crate::api::client::ApiClient;
use crate::api::schema::{AgentTarget, ExtensionActionInvokeParams, Method, Request};
use serde_json::{json, Value};

const SERVICE: &str = "vqro.tasks.core";
const USAGE: &str = "usage: vqro message <send TARGET TEXT [--reply-to ID] [--op ID]|list|claim [--idle] [--op ID]|edit ID TEXT --rev N [--op ID]|activation on|off [--op ID]>";

pub(super) fn run_message_command(args: &[String]) -> std::io::Result<i32> {
    match args.first().map(String::as_str) {
        Some("send") => message_send(&args[1..]),
        Some("list") => {
            let client = super::target::api_client()?;
            invoke_and_print(&client, json!({"action": "message_list"}))
        }
        Some("claim") => message_claim(&args[1..]),
        Some("edit") => message_edit(&args[1..]),
        Some("activation") => message_activation(&args[1..]),
        Some("help" | "--help" | "-h") => {
            eprintln!("{USAGE}");
            Ok(0)
        }
        _ => {
            eprintln!("{USAGE}");
            Ok(2)
        }
    }
}

/// A caller-supplied operation id makes an uncertain retry idempotent; otherwise a
/// fresh id is generated and printed in the outcome. Entropy failure fails closed
/// before any effect.
fn operation_id(supplied: Option<String>) -> std::io::Result<String> {
    match supplied {
        Some(id) if !id.is_empty() => Ok(id),
        Some(_) => Err(std::io::Error::other("--op id must not be empty")),
        None => super::random_operation_id("msg-op-"),
    }
}

fn flag_value(args: &[String], index: usize, name: &str) -> Result<String, i32> {
    args.get(index + 1).cloned().ok_or_else(|| {
        eprintln!("{name} requires a value");
        2
    })
}

/// Resolve an exact agent-process recipient binding for a durable message.
pub(super) fn recipient_binding(client: &ApiClient, target: &str) -> Result<Value, String> {
    let response = client
        .request_value_with_timeout(
            &Request {
                id: "cli:message:resolve".into(),
                method: Method::AgentGet(AgentTarget {
                    target: target.to_string(),
                }),
            },
            Duration::from_secs(5),
        )
        .map_err(|error| error.to_string())?;
    if let Some(error) = response.get("error") {
        return Err(error["message"]
            .as_str()
            .unwrap_or("agent not found")
            .to_string());
    }
    let agent = &response["result"]["agent"];
    let (terminal_id, kind, execution) = (
        agent["terminal_id"].as_str(),
        agent["agent"].as_str(),
        agent["execution"].as_str(),
    );
    // A durable message addresses an exact execution; a name or pane alone is not enough.
    let (Some(terminal_id), Some(kind), Some(execution)) = (terminal_id, kind, execution) else {
        return Err("recipient is not a detected agent process with a provable execution".into());
    };
    Ok(json!({
        "class": "agent",
        "terminal_id": terminal_id,
        "agent": kind,
        "execution": execution,
    }))
}

fn message_send(args: &[String]) -> std::io::Result<i32> {
    let (Some(target), Some(text)) = (args.first(), args.get(1)) else {
        eprintln!("{USAGE}");
        return Ok(2);
    };
    let (mut reply_to, mut op) = (None, None);
    let mut index = 2;
    while index < args.len() {
        match args[index].as_str() {
            "--reply-to" => {
                let Ok(value) = flag_value(args, index, "--reply-to") else {
                    return Ok(2);
                };
                reply_to = Some(json!(value));
                index += 2;
            }
            "--op" => {
                let Ok(value) = flag_value(args, index, "--op") else {
                    return Ok(2);
                };
                op = Some(value);
                index += 2;
            }
            other => {
                eprintln!("unknown option: {other}");
                return Ok(2);
            }
        }
    }
    let client = super::target::api_client()?;
    let recipient = match recipient_binding(&client, target) {
        Ok(binding) => binding,
        Err(message) => {
            eprintln!("{message}");
            return Ok(1);
        }
    };
    let mut payload = json!({
        "action": "message_send",
        "operation_id": operation_id(op)?,
        "recipient": recipient,
        "body": text,
    });
    if let Some(reply_to) = reply_to {
        payload["reply_to"] = reply_to;
    }
    invoke_and_print(&client, payload)
}

fn message_claim(args: &[String]) -> std::io::Result<i32> {
    let (mut idle, mut op) = (false, None);
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--idle" => {
                idle = true;
                index += 1;
            }
            "--op" => {
                let Ok(value) = flag_value(args, index, "--op") else {
                    return Ok(2);
                };
                op = Some(value);
                index += 2;
            }
            other => {
                eprintln!("unknown option: {other}");
                return Ok(2);
            }
        }
    }
    let client = super::target::api_client()?;
    let mut payload = json!({"action": "message_claim", "operation_id": operation_id(op)?});
    if idle {
        payload["idle"] = json!(true);
    }
    invoke_and_print(&client, payload)
}

fn message_edit(args: &[String]) -> std::io::Result<i32> {
    let (Some(id), Some(text)) = (args.first(), args.get(1)) else {
        eprintln!("{USAGE}");
        return Ok(2);
    };
    let (mut revision, mut op) = (None, None);
    let mut index = 2;
    while index < args.len() {
        match args[index].as_str() {
            "--rev" => {
                let Ok(value) = flag_value(args, index, "--rev") else {
                    return Ok(2);
                };
                let Ok(parsed) = value.parse::<u64>() else {
                    eprintln!("--rev requires a message revision number");
                    return Ok(2);
                };
                revision = Some(parsed);
                index += 2;
            }
            "--op" => {
                let Ok(value) = flag_value(args, index, "--op") else {
                    return Ok(2);
                };
                op = Some(value);
                index += 2;
            }
            other => {
                eprintln!("unknown option: {other}");
                return Ok(2);
            }
        }
    }
    let Some(revision) = revision else {
        eprintln!("edit requires the inspected --rev revision");
        return Ok(2);
    };
    let client = super::target::api_client()?;
    invoke_and_print(
        &client,
        json!({"action": "message_edit", "operation_id": operation_id(op)?, "id": id, "revision": revision, "body": text}),
    )
}

fn message_activation(args: &[String]) -> std::io::Result<i32> {
    let activation = match args.first().map(String::as_str) {
        Some("on") => true,
        Some("off") => false,
        _ => {
            eprintln!("usage: vqro message activation on|off [--op ID]");
            return Ok(2);
        }
    };
    let mut op = None;
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--op" => {
                let Ok(value) = flag_value(args, index, "--op") else {
                    return Ok(2);
                };
                op = Some(value);
                index += 2;
            }
            other => {
                eprintln!("unknown option: {other}");
                return Ok(2);
            }
        }
    }
    let client = super::target::api_client()?;
    invoke_and_print(
        &client,
        json!({"action": "message_activation", "operation_id": operation_id(op)?, "activation": activation}),
    )
}

fn invoke_and_print(client: &ApiClient, payload: Value) -> std::io::Result<i32> {
    let response = client
        .request_value_with_timeout(
            &Request {
                id: "cli:message".into(),
                method: Method::ExtensionActionInvoke(ExtensionActionInvokeParams {
                    service_id: SERVICE.into(),
                    payload,
                }),
            },
            Duration::from_secs(6),
        )
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    if response.get("error").is_some() {
        eprintln!(
            "{}",
            serde_json::to_string(&response).map_err(std::io::Error::other)?
        );
        return Ok(1);
    }
    let value = &response["result"]["value"];
    if value["ok"] == true {
        println!(
            "{}",
            serde_json::to_string(&value["value"]).map_err(std::io::Error::other)?
        );
        Ok(0)
    } else {
        eprintln!(
            "{}",
            serde_json::to_string(&value["error"]).map_err(std::io::Error::other)?
        );
        Ok(1)
    }
}
