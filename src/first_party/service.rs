//! Bundled first-party providers executed through the public stdio protocol.

use std::io::{BufRead, Write};

use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
struct Request {
    #[serde(rename = "type")]
    kind: String,
    id: u64,
    method: String,
    params: serde_json::Value,
}

#[derive(Serialize)]
struct Response<T: Serialize> {
    #[serde(rename = "type")]
    kind: &'static str,
    id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

pub(crate) fn run(provider: &str) -> std::io::Result<i32> {
    let (namespace, capabilities) = match provider {
        "collections" => (
            crate::first_party::collections::NAMESPACE,
            serde_json::json!(["host_document", "focus_terminal"]),
        ),
        _ => return Ok(2),
    };
    let service_id =
        std::env::var("VQRO_EXTENSION_SERVICE_ID").unwrap_or_else(|_| namespace.to_string());
    let hello = serde_json::json!({
        "type": "hello",
        "protocol": crate::extension_service::EXTENSION_SERVICE_PROTOCOL,
        "service_id": service_id,
        "requested_capabilities": capabilities,
        "methods": ["host.document.render"],
    });
    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer(&mut stdout, &hello)?;
    stdout.write_all(b"\n")?;
    stdout.flush()?;

    let stdin = std::io::stdin();
    let mut lines = stdin.lock().lines();
    let Some(welcome) = lines.next() else {
        return Ok(1);
    };
    let welcome: serde_json::Value = serde_json::from_str(&welcome?)?;
    if welcome.get("type").and_then(serde_json::Value::as_str) != Some("welcome") {
        return Ok(1);
    }

    for line in lines {
        let response = match serde_json::from_str::<Request>(&line?) {
            Ok(request)
                if request.kind == "request" && request.method == "host.document.render" =>
            {
                match serde_json::from_value::<crate::extension_contract::HostTerminalGroupsV1>(
                    request.params,
                )
                .map_err(|error| error.to_string())
                .and_then(|snapshot| {
                    crate::first_party::collections::project(&snapshot)
                        .map_err(|error| error.to_string())
                }) {
                    Ok(document) => Response {
                        kind: "response",
                        id: request.id,
                        result: Some(document),
                        error: None,
                    },
                    Err(error) => Response {
                        kind: "response",
                        id: request.id,
                        result: None,
                        error: Some(error),
                    },
                }
            }
            Ok(request) => Response {
                kind: "response",
                id: request.id,
                result: None,
                error: Some("unsupported method".into()),
            },
            Err(error) => Response::<crate::extension_contract::HostDocumentV1> {
                kind: "response",
                id: 0,
                result: None,
                error: Some(error.to_string()),
            },
        };
        serde_json::to_writer(&mut stdout, &response)?;
        stdout.write_all(b"\n")?;
        stdout.flush()?;
    }
    Ok(0)
}
