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

#[test]
fn bundled_spaces_provider_parses_the_public_session_snapshot() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_vqro"))
        .args(["_extension-service", "spaces"])
        .env("VQRO_EXTENSION_SERVICE_ID", "vqro.spaces")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    stdout.read_line(&mut line).unwrap();
    let hello: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(hello["service_id"], "vqro.spaces");
    assert_eq!(
        hello["requested_capabilities"],
        serde_json::json!(["host_document"])
    );

    writeln!(
        stdin,
        "{}",
        serde_json::json!({
            "type": "welcome",
            "protocol": 2,
            "granted_capabilities": ["host_document"],
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
            "id": 8,
            "method": "host.document.render",
            "params": {
                "contract": "host.spaces.v1",
                "generation": 4,
                "revision": 6,
                "scope_id": "session_alpha",
                "repositories": [{
                    "repository_id": "repo_vqro",
                    "label": "vqro",
                    "workspaces": [{"workspace_id": "workspace_main", "label": "main"}]
                }],
                "standalone": [{"workspace_id": "workspace_notes", "label": "Notes"}],
                "order": [
                    {"kind": "standalone_workspace", "workspace_id": "workspace_notes"},
                    {"kind": "repository", "repository_id": "repo_vqro"}
                ]
            }
        })
    )
    .unwrap();
    stdin.flush().unwrap();

    line.clear();
    stdout.read_line(&mut line).unwrap();
    let response: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(response["id"], 8);
    assert_eq!(response["result"]["namespace"], "vqro.spaces");
    assert_eq!(response["result"]["target"]["kind"], "session");
    assert_eq!(
        response["result"]["items"][0]["id"],
        "workspace:workspace_notes"
    );
    assert_eq!(response["result"]["items"][1]["id"], "repository:repo_vqro");
    assert_eq!(
        response["result"]["items"][2]["parent_id"],
        "repository:repo_vqro"
    );

    drop(stdin);
    assert!(child.wait().unwrap().success());
}
