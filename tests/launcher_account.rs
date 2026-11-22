//! Configured account metadata only: no credential/profile content or inferred auth.
#[path = "../src/bin/launcher_account/mod.rs"]
mod launcher_account;
use serde_json::json;

#[test]
fn account_metadata_is_bounded_sourced_and_absent_without_opt_in() {
    assert_eq!(launcher_account::parse(None).unwrap(), None);
    let row = json!({"id":"work","label":"Work","provider":"openai-codex","kind":"subscription",
        "sources":{"id":"vqro:launcher","label":"vqro:launcher","provider":"vqro:launcher","kind":"vqro:launcher"}});
    assert_eq!(launcher_account::parse(Some(&row.to_string())).unwrap(), Some(row.clone()));
    for (key,value) in [("profile",json!("/never/read")),("token",json!("opaque-secret")),("kind",json!("unknown")),
                        ("label",json!("x".repeat(129))),("id",json!("bad id")),("sources",json!({}))] {
        let mut invalid=row.clone(); invalid[key]=value;
        let error=launcher_account::parse(Some(&invalid.to_string())).unwrap_err();
        assert!(!error.contains("opaque-secret"));
    }
    assert!(launcher_account::parse(Some(&" ".repeat(2049))).is_err());
    assert!(launcher_account::parse(Some("not-json")).is_err());
}

#[test]
fn malformed_account_is_rejected_by_both_executables_before_native_spawn() {
    for binary in [env!("CARGO_BIN_EXE_vqro-bridge-pi"), env!("CARGO_BIN_EXE_vqro-bridge-codex")] {
        let root = tempfile::tempdir().unwrap();
        let marker = root.path().join("native-started");
        let result = std::process::Command::new(binary).env_clear()
            .env("PATH", "/usr/bin:/bin").env("HOME", root.path())
            .env("VQRO_SOCKET", root.path().join("private.sock"))
            .env("VQRO_BRIDGE_TOKEN", "fake-bootstrap")
            .env("VQRO_ACCOUNT", r#"{"token":"never-echo-this"}"#)
            .current_dir(root.path()).args(["--", "/usr/bin/touch"]).arg(&marker).output().unwrap();
        assert!(!result.status.success());
        assert!(!marker.exists(), "malformed metadata must not fall back to native defaults");
        assert!(!String::from_utf8_lossy(&result.stderr).contains("never-echo-this"));
    }
}

#[tokio::test]
async fn codex_configured_account_is_consistent_on_every_public_fact_path() {
    use vqro::codex::runtime::Runtime;
    use vqro::rpc::Peer;
    let (io, _other) = tokio::io::duplex(1024);
    let (read, write) = tokio::io::split(io);
    let (peer, _) = Peer::spawn(read, write);
    let account = json!({"id":"work","label":"Work","kind":"subscription",
        "sources":{"id":"vqro:launcher","label":"vqro:launcher","kind":"vqro:launcher"}});
    let runtime = Runtime::with_host_metadata(peer, 1, "inc".into(), "fixture".into(), "instance".into(), None, None, Some(account.clone()));
    assert_eq!(runtime.facts()[0]["data"]["agent"]["account"], account);
    assert_eq!(runtime.fact("endpoint-1").unwrap()["data"]["agent"]["account"], account);
    assert_eq!(runtime.take_dirty_facts()[0]["data"]["agent"]["account"], account);
    runtime.native.close();
}

#[tokio::test]
async fn codex_without_account_opt_in_omits_the_fact() {
    use vqro::codex::runtime::Runtime;
    use vqro::rpc::Peer;
    let (io, _other) = tokio::io::duplex(1024);
    let (read, write) = tokio::io::split(io);
    let (peer, _) = Peer::spawn(read, write);
    let runtime = Runtime::new(peer, 1, "inc".into(), None);
    assert!(runtime.facts()[0]["data"]["agent"].get("account").is_none());
    runtime.native.close();
}
