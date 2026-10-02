use std::time::Duration;

use crate::api::schema::{ExtensionActionInvokeParams, Method, Request};

pub(super) fn run_task_command(args: &[String]) -> std::io::Result<i32> {
    let Some(action) = args.first().map(String::as_str) else {
        return usage();
    };
    let mut payload = serde_json::json!({"action": action});
    let mutation = action == "create";
    match (action, args.get(1)) {
        ("list", None) => {}
        ("get", Some(id)) if args.len() == 2 => payload["id"] = id.clone().into(),
        ("op", Some(id)) if args.len() == 2 => payload["operation_id"] = id.clone().into(),
        ("create", Some(title)) => {
            payload["title"] = title.clone().into();
            let mut remaining = &args[2..];
            while !remaining.is_empty() {
                match remaining {
                    [flag, value, rest @ ..] if flag == "--parent" => {
                        payload["parent"] = value.clone().into();
                        remaining = rest;
                    }
                    [flag, value, rest @ ..] if flag == "--op" => {
                        payload["operation_id"] = value.clone().into();
                        remaining = rest;
                    }
                    _ => return usage(),
                }
            }
            if payload.get("operation_id").is_none() {
                let mut bytes = [0_u8; 16];
                crate::platform::fill_secure_random(&mut bytes)?;
                payload["operation_id"] = bytes
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>()
                    .into();
            }
        }
        _ => return usage(),
    }
    let op = payload
        .get("operation_id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .to_string();
    let request = Request {
        id: format!("cli:task:{action}:{op}"),
        method: Method::ExtensionActionInvoke(ExtensionActionInvokeParams {
            service_id: "vqro.tasks.core".into(),
            payload,
        }),
    };
    let client = super::target::api_client()?;
    let response = match client.request_value_with_timeout(&request, Duration::from_secs(6)) {
        Ok(response) => response,
        Err(error) => {
            if mutation {
                eprintln!("outcome unknown; op {op}");
            } else {
                eprintln!("task read unavailable: {error}");
            }
            return Ok(1);
        }
    };
    if let Some(error) = response.get("error") {
        if mutation && error["code"] == "outcome_unknown" {
            eprintln!("outcome unknown; op {op}");
        } else {
            eprintln!("{error}");
        }
        return Ok(1);
    }
    let result = &response["result"]["value"];
    if result["ok"] != true {
        eprintln!("{}", result["error"]);
        return Ok(1);
    }
    println!("{}", result["value"]);
    Ok(0)
}

fn usage() -> std::io::Result<i32> {
    eprintln!("usage: vqro task <create TITLE [--parent ID] [--op ID]|list|get ID|op ID>");
    Ok(2)
}
