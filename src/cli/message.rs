use std::time::Duration;

use crate::api::client::ApiClient;
use crate::api::schema::{AgentTarget, ExtensionActionInvokeParams, Method, Request};
use serde_json::{json, Value};

const SERVICE: &str = "vqro.tasks.core";
const USAGE: &str = "usage: vqro message <send TARGET TEXT [--reply-to ID]|list|claim|edit ID TEXT --rev N|activation on|off>";

pub(super) fn run_message_command(args: &[String]) -> std::io::Result<i32> {
    match args.first().map(String::as_str) {
        Some("send") => message_send(&args[1..]),
        Some("list") => message_read(json!({"action": "message_list"})),
        Some("claim") => message_claim(),
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

fn operation_id() -> String {
    let mut bytes = [0_u8; 16];
    // Random failure only means no id; the command then fails closed before sending.
    let _ = crate::platform::fill_secure_random(&mut bytes);
    format!("msg-op-{}", bytes.iter().map(|byte| format!("{byte:02x}")).collect::<String>())
}

/// Resolve an exact agent-process recipient binding for a durable message.
fn recipient_binding(client: &ApiClient, target: &str) -> Result<Value, String> {
    let response = client
        .request_value_with_timeout(
            &Request {
                id: "cli:message:resolve".into(),
                method: Method::AgentGet(AgentTarget { target: target.to_string() }),
            },
            Duration::from_secs(5),
        )
        .map_err(|error| error.to_string())?;
    if let Some(error) = response.get("error") {
        return Err(error["message"].as_str().unwrap_or("agent not found").to_string());
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

/// The sender's own reply address (agent name, else pane id), best effort.
fn sender_address(client: &ApiClient) -> Option<String> {
    let pane = std::env::var(crate::product::PANE_ID_ENV).ok()?;
    let response = client
        .request_value_with_timeout(
            &Request {
                id: "cli:message:self".into(),
                method: Method::AgentGet(AgentTarget { target: pane.clone() }),
            },
            Duration::from_secs(5),
        )
        .ok()?;
    let name = response["result"]["agent"]["name"].as_str();
    Some(name.map(str::to_string).unwrap_or(pane))
}

fn message_send(args: &[String]) -> std::io::Result<i32> {
    let (Some(target), Some(text)) = (args.first(), args.get(1)) else {
        eprintln!("{USAGE}");
        return Ok(2);
    };
    let mut reply_to = None;
    let mut index = 2;
    while index < args.len() {
        match args[index].as_str() {
            "--reply-to" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("--reply-to requires a message id");
                    return Ok(2);
                };
                reply_to = Some(json!(value));
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
        "operation_id": operation_id(),
        "recipient": recipient,
        "body": text,
    });
    if let Some(reply_to) = reply_to {
        payload["reply_to"] = reply_to;
    }
    if let Some(address) = sender_address(&client) {
        payload["sender_address"] = json!(address);
    }
    invoke_and_print(&client, payload)
}

fn message_read(payload: Value) -> std::io::Result<i32> {
    let client = super::target::api_client()?;
    invoke_and_print(&client, payload)
}

fn message_claim() -> std::io::Result<i32> {
    let client = super::target::api_client()?;
    invoke_and_print(&client, json!({"action": "message_claim", "operation_id": operation_id()}))
}

fn message_edit(args: &[String]) -> std::io::Result<i32> {
    let (Some(id), Some(text)) = (args.first(), args.get(1)) else {
        eprintln!("{USAGE}");
        return Ok(2);
    };
    let mut revision = None;
    let mut index = 2;
    while index < args.len() {
        match args[index].as_str() {
            "--rev" => {
                let Some(value) = args.get(index + 1).and_then(|value| value.parse::<u64>().ok()) else {
                    eprintln!("--rev requires a message revision number");
                    return Ok(2);
                };
                revision = Some(value);
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
        json!({"action": "message_edit", "operation_id": operation_id(), "id": id, "revision": revision, "body": text}),
    )
}

fn message_activation(args: &[String]) -> std::io::Result<i32> {
    let activation = match args.first().map(String::as_str) {
        Some("on") => true,
        Some("off") => false,
        _ => {
            eprintln!("usage: vqro message activation on|off");
            return Ok(2);
        }
    };
    let client = super::target::api_client()?;
    invoke_and_print(&client, json!({"action": "message_activation", "operation_id": operation_id(), "activation": activation}))
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
        eprintln!("{}", serde_json::to_string(&response).map_err(std::io::Error::other)?);
        return Ok(1);
    }
    let value = &response["result"]["value"];
    if value["ok"] == true {
        println!("{}", serde_json::to_string(&value["value"]).map_err(std::io::Error::other)?);
        Ok(0)
    } else {
        eprintln!("{}", serde_json::to_string(&value["error"]).map_err(std::io::Error::other)?);
        Ok(1)
    }
}
