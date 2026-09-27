use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

#[test]
fn compiled_collections_service_is_absent() {
    let output = Command::new(env!("CARGO_BIN_EXE_vqro"))
        .args(["_extension-service", "collections"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
}

#[test]
fn compiled_spaces_service_is_absent() {
    let output = Command::new(env!("CARGO_BIN_EXE_vqro"))
        .args(["_extension-service", "spaces"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
}

fn render_with_bundled_provider(provider: &str, params: serde_json::Value) -> serde_json::Value {
    let mut child = Command::new(env!("CARGO_BIN_EXE_vqro"))
        .args(["_extension-service", provider])
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
            "id": 12,
            "method": "host.document.render",
            "params": params
        })
    )
    .unwrap();
    stdin.flush().unwrap();
    line.clear();
    stdout.read_line(&mut line).unwrap();
    let response = serde_json::from_str(&line).unwrap();
    drop(stdin);
    assert!(child.wait().unwrap().success());
    response
}

#[test]
fn bundled_delegation_provider_parses_public_relationships() {
    let response = render_with_bundled_provider(
        "delegation",
        serde_json::json!({
            "contract": "host.relationships.v1",
            "generation": 2,
            "revision": 4,
            "scope_id": "session_alpha",
            "relationships": [{
                "relationship_id": "delegation_1",
                "kind": "vqro.delegation",
                "terminal": {"terminal_id": "term_1"},
                "sibling_rank": 0,
                "lifecycle": "active",
                "metadata": {"vqro.delegation.purpose": "Review"}
            }]
        }),
    );
    assert_eq!(response["result"]["namespace"], "vqro.delegation");
    assert_eq!(response["result"]["items"][0]["label"], "Review");
}

#[test]
fn bundled_agent_status_provider_parses_public_activity() {
    let response = render_with_bundled_provider(
        "agent-status",
        serde_json::json!({
            "contract": "host.agent-activity.v1",
            "generation": 2,
            "revision": 4,
            "scope_id": "session_alpha",
            "activities": [{
                "terminal": {"terminal_id": "term_1"},
                "activity": "idle",
                "source": "adapter-v2",
                "evidence_kind": "screen/state",
                "state_change_seq": 7,
                "labels": {"display": "Reviewer"}
            }]
        }),
    );
    assert_eq!(response["result"]["namespace"], "vqro.agent-status");
    assert_eq!(response["result"]["items"][0]["label"], "Idle");
    assert_ne!(response["result"]["items"][0]["label"], "Done");
}
