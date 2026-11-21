//! Explicit single-endpoint launcher identity is distinct from shared-host/tool attribution.
use serde_json::Value;
use vqro::codex::runtime::Runtime;
use vqro::rpc::Peer;

fn runtime(count: usize, principal: Option<&str>) -> std::sync::Arc<Runtime> {
    let (io, _other) = tokio::io::duplex(1024);
    let (read, write) = tokio::io::split(io);
    let (peer, _) = Peer::spawn(read, write);
    Runtime::with_host_principal(
        peer,
        count,
        "inc".into(),
        "codex-fixture".into(),
        "instance".into(),
        None,
        principal.map(str::to_owned),
    )
}

fn no_tool_env(fact: &Value) {
    assert!(fact["data"]["agent"]["caps"]["agent"]
        .get("tool_principal")
        .is_none());
}

#[tokio::test]
async fn explicit_single_endpoint_principal_is_published_on_every_fact_path() {
    let runtime = runtime(1, Some("p:launcher-one"));
    let all = runtime.facts();
    let one = runtime.fact("endpoint-1").unwrap();
    let dirty = runtime.take_dirty_facts();
    assert_eq!(all[0]["data"]["agent"]["principal"], "p:launcher-one");
    assert_eq!(all[0], one);
    assert_eq!(all[0], dirty[0]);
    no_tool_env(&all[0]);
    runtime.native.close();
}

#[tokio::test]
async fn shared_host_never_turns_configured_principal_into_endpoint_claims() {
    let runtime = runtime(2, Some("p:shared-runtime"));
    for fact in runtime.facts() {
        assert!(fact["data"]["agent"].get("principal").is_none());
        no_tool_env(&fact);
    }
    runtime.native.close();
}

#[tokio::test]
async fn absent_or_reserved_launcher_identity_never_creates_an_endpoint_claim() {
    for configured in [
        None,
        Some("undeclared"),
        Some("host:r1"),
        Some("extension:wrong"),
        Some(""),
    ] {
        let runtime = runtime(1, configured);
        assert!(runtime.facts()[0]["data"]["agent"]
            .get("principal")
            .is_none());
        runtime.native.close();
    }
}

#[test]
fn executable_rejects_multi_endpoint_or_mismatched_opt_in_before_native_spawn() {
    for (slots, requested, expected) in [
        ("2", "p:one", "requires exactly one endpoint"),
        ("1", "p:two", "must match an explicit assertable"),
    ] {
        let result = std::process::Command::new(env!("CARGO_BIN_EXE_vqro-bridge-codex"))
            .env_clear()
            .env("VQRO_SOCKET", "/nonexistent/private.sock")
            .env("VQRO_BRIDGE_TOKEN", "fixture-only")
            .env("VQRO_PRINCIPAL", "p:one")
            .args([
                "--slots",
                slots,
                "--endpoint-principal",
                requested,
                "--",
                "/never-run-native",
            ])
            .output()
            .unwrap();
        assert!(!result.status.success());
        assert!(String::from_utf8_lossy(&result.stderr).contains(expected));
    }
}
