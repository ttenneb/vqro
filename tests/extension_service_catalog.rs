use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

#[test]
fn bundled_collections_provider_uses_public_stdio_contract() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_vqro"))
        .args(["_extension-service", "collections"])
        .env("VQRO_EXTENSION_SERVICE_ID", "vqro.collections")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    stdout.read_line(&mut line).unwrap();
    let hello: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(hello["type"], "hello");
    assert_eq!(hello["service_id"], "vqro.collections");
    assert!(hello["methods"]
        .as_array()
        .unwrap()
        .iter()
        .any(|method| method == "host.document.render"));

    writeln!(
        stdin,
        "{}",
        serde_json::json!({
            "type": "welcome",
            "protocol": 2,
            "granted_capabilities": ["host_document", "focus_terminal"],
            "methods": ["host.document.render"],
            "trust": "trusted_same_user_not_sandboxed"
        })
    )
    .unwrap();
    writeln!(
        stdin,
        "{}",
        serde_json::json!({
            "type": "request",
            "id": 7,
            "method": "host.document.render",
            "params": {
                "contract": "host.terminal-groups.v1",
                "generation": 3,
                "revision": 9,
                "scope_id": "w1:t1",
                "groups": [],
                "ungrouped": [{
                    "terminal": {
                        "terminal_id": "term_a",
                        "placement": {"pane_id": "w1:p1", "workspace_id": "w1", "tab_id": "w1:t1"}
                    },
                    "label": "Shell",
                    "lifecycle": "live"
                }]
            }
        })
    )
    .unwrap();
    stdin.flush().unwrap();
    line.clear();
    stdout.read_line(&mut line).unwrap();
    let response: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(response["type"], "response");
    assert_eq!(response["id"], 7);
    assert_eq!(response["result"]["contract"], "host.document.v1");
    assert_eq!(response["result"]["namespace"], "vqro.collections");
    assert_eq!(
        response["result"]["items"][0]["terminal"]["terminal_id"],
        "term_a"
    );

    drop(stdin);
    assert!(child.wait().unwrap().success());
}
