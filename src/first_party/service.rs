//! Bundled first-party providers executed through the public stdio protocol.

use std::io::{BufRead, Write};

pub(crate) fn run(provider: &str) -> std::io::Result<i32> {
    let (namespace, capabilities) = match provider {
        "delegation" => (
            crate::first_party::delegation::NAMESPACE,
            serde_json::json!(["host_document"]),
        ),
        "agent-status" => (
            crate::first_party::agent_status::NAMESPACE,
            serde_json::json!(["host_document"]),
        ),
        _ => return Ok(2),
    };
    let service_id =
        std::env::var("VQRO_EXTENSION_SERVICE_ID").unwrap_or_else(|_| namespace.to_string());
    let hello = crate::extension_service::ServiceHello {
        kind: "hello".into(),
        protocol: crate::extension_service::EXTENSION_SERVICE_PROTOCOL,
        service_id,
        requested_capabilities: serde_json::from_value(capabilities)?,
        methods: vec!["host.document.render".into()],
    };
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
        let response = match serde_json::from_str::<
            crate::extension_service::ExtensionServiceRequestV1,
        >(&line?)
        {
            Ok(request)
                if request.kind == "request" && request.method == "host.document.render" =>
            {
                let projection = match provider {
                    "delegation" => serde_json::from_value::<
                        crate::extension_contract::HostRelationshipsV1,
                    >(request.params)
                    .map_err(|error| error.to_string())
                    .and_then(|snapshot| {
                        crate::first_party::delegation::project(&snapshot)
                            .map_err(|error| error.to_string())
                    }),
                    "agent-status" => serde_json::from_value::<
                        crate::extension_contract::HostAgentActivityV1,
                    >(request.params)
                    .map_err(|error| error.to_string())
                    .and_then(|snapshot| {
                        crate::first_party::agent_status::project(&snapshot)
                            .map_err(|error| error.to_string())
                    }),
                    _ => unreachable!("provider was validated before the service loop"),
                };
                match projection {
                    Ok(document) => crate::extension_service::ExtensionServiceResponseV1 {
                        kind: "response".into(),
                        id: request.id,
                        result: Some(serde_json::to_value(document)?),
                        error: None,
                    },
                    Err(error) => crate::extension_service::ExtensionServiceResponseV1 {
                        kind: "response".into(),
                        id: request.id,
                        result: None,
                        error: Some(error),
                    },
                }
            }
            Ok(request) => crate::extension_service::ExtensionServiceResponseV1 {
                kind: "response".into(),
                id: request.id,
                result: None,
                error: Some("unsupported method".into()),
            },
            Err(error) => crate::extension_service::ExtensionServiceResponseV1 {
                kind: "response".into(),
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
