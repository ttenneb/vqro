//! Synthetic native metadata only; no installed runtime, profiles or model calls.
use serde_json::{Value, json};
use vqro::codex::{Change, Endpoint};

fn endpoint() -> Endpoint {
    Endpoint::new("e".into(), "inc".into(), "host".into(), "epoch".into())
}
fn startup(changes: &[Change]) -> &Value {
    &changes
        .iter()
        .find(|c| c.kind == "startup" && c.id == "current")
        .expect("native session response must publish its retained startup snapshot")
        .value
}

fn started() -> Endpoint {
    let mut e = endpoint();
    e.set_session_response(
        &json!({"thread":{"id":"s","status":{"type":"idle"}},"cwd":"/native","model":"initial"}),
        "next".into(),
    )
    .unwrap();
    e
}

#[test]
fn raw_native_identity_has_no_invented_semver_and_survives_new_epoch() {
    let mut e = endpoint();
    let initial = e.initialize_startup(
        &json!({"userAgent":"native identity (opaque)","version":"not a native field"}),
    );
    assert_eq!(
        startup(&initial)["runtime"],
        json!({"name":"native identity (opaque)"})
    );
    let changes = e
        .set_session_response_from(
            &json!({"thread":{"id":"s","status":{"type":"idle"}},"model":"resumed"}),
            "next".into(),
            "rpc:thread/resume",
        )
        .unwrap();
    assert_eq!(
        startup(&changes)["runtime"]["name"],
        "native identity (opaque)"
    );
    assert_eq!(startup(&changes)["sources"]["model"], "rpc:thread/resume");
    assert!(startup(&changes).get("instructions").is_none());
}

#[test]
fn catalogs_are_available_skills_installed_plugins_and_native_mcp_status_only() {
    let mut e = started();
    let changes = e.finish_startup_inventory("s", "next", "skills", Some(&json!({"data":[{"cwd":"/native","errors":[],"skills":[{"name":"enabled","enabled":true,"description":"native description","path":"/native/SKILL.md","secret":"never expose"},{"name":"disabled","enabled":false}]}]})));
    assert_eq!(
        startup(&changes)["skills"],
        json!([{"name":"enabled","description":"native description","path":"/native/SKILL.md"}])
    );
    assert_eq!(startup(&changes)["skills_scope"], "available");
    let changes = e.finish_startup_inventory("s", "next", "plugins", Some(&json!({"marketplaces":[{"plugins":[{"name":"installed-disabled","installed":true,"enabled":false,"localVersion":"1.2","version":"99","source":{"type":"local","path":"/native/plugin"}},{"name":"suggestion","installed":false,"enabled":true},{"name":"remote-installed","installed":true,"enabled":true,"version":"99","source":{"type":"remote"}}]}]})));
    assert_eq!(
        startup(&changes)["plugins"],
        json!([{"name":"installed-disabled","enabled":false,"version":"1.2","path":"/native/plugin"},{"name":"remote-installed","enabled":true}])
    );
    let changes = e.finish_startup_inventory("s", "next", "mcp", Some(&json!({"data":[{"name":"native","runtimeStatus":"authenticationRequired","serverInfo":{"version":"not-runtime-version"}},{"name":"unavailable","runtimeStatus":null}]})));
    assert_eq!(
        startup(&changes)["mcp"],
        json!([{"name":"native","status":"authenticationRequired"}])
    );
    assert!(
        startup(&changes)["incomplete"]
            .as_array()
            .unwrap()
            .contains(&json!("mcp"))
    );
    assert!(!startup(&changes).to_string().contains("never expose"));
    assert!(
        !startup(&changes)
            .to_string()
            .contains("not-runtime-version")
    );
}

#[test]
fn failed_inventories_are_absent_not_zero_and_do_not_leak_diagnostics() {
    let mut e = started();
    for field in ["skills", "plugins", "mcp"] {
        let changes = e.finish_startup_inventory("s", "next", field, None);
        let value = startup(&changes);
        assert!(value.get(field).is_none());
        assert!(
            value["incomplete"]
                .as_array()
                .unwrap()
                .contains(&json!(field))
        );
    }
    let changes = e.finish_startup_inventory(
        "s",
        "next",
        "plugins",
        Some(&json!({"marketplaces":[],"marketplaceLoadErrors":[{"message":"secret"}]})),
    );
    assert!(startup(&changes).get("plugins").is_none());
    assert!(!startup(&changes).to_string().contains("secret"));
    let changes = e.finish_startup_inventory(
        "s",
        "next",
        "skills",
        Some(&json!({"data":[{"cwd":"/native","skills":[],"errors":[{"message":"secret"}]}]})),
    );
    assert!(startup(&changes).get("skills").is_none());
}

#[test]
fn late_old_epoch_other_thread_or_ended_results_cannot_overwrite_snapshot() {
    let mut e = started();
    e.set_session_response(
        &json!({"thread":{"id":"s","status":{"type":"idle"}},"model":"second","cwd":"/native"}),
        "newer".into(),
    )
    .unwrap();
    let data = json!({"data":[{"name":"m","runtimeStatus":"connected"}]});
    assert!(
        e.finish_startup_inventory("s", "next", "mcp", Some(&data))
            .is_empty()
    );
    assert!(
        e.finish_startup_inventory("other", "newer", "mcp", Some(&data))
            .is_empty()
    );
    let changes = e.finish_startup_inventory("s", "newer", "mcp", Some(&data));
    assert_eq!(startup(&changes)["model"], "second");
    assert!(startup(&changes).get("skills").is_none());
    e.lifecycle = "ended".into();
    assert!(
        e.finish_startup_inventory("s", "newer", "mcp", Some(&data))
            .is_empty()
    );
}

#[test]
fn skills_count_and_utf8_description_bounds_are_marked() {
    let mut e = started();
    let rows: Vec<_> = (0..300)
        .map(|n| json!({"name":format!("skill{n}"),"enabled":true,"description":"é".repeat(700)}))
        .collect();
    let changes = e.finish_startup_inventory(
        "s",
        "next",
        "skills",
        Some(&json!({"data":[{"cwd":"/native","skills":rows,"errors":[]}]})),
    );
    let value = startup(&changes);
    assert!(value["skills"].as_array().unwrap().len() <= 256);
    assert!(
        value["skills"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r.get("description").is_none())
    );
    assert!(
        value["incomplete"]
            .as_array()
            .unwrap()
            .contains(&json!("skills"))
    );
    assert!(serde_json::to_vec(value).unwrap().len() < vqro::codex::ENTITY_BYTES);
}

#[test]
fn over_bound_paths_are_omitted_never_cut_into_an_actionable_prefix() {
    let mut e = endpoint();
    let long = format!("/native/{}", "é".repeat(2200));
    let changes = e.set_session_response(&json!({"thread":{"id":"s","status":{"type":"idle"}},"cwd":long,"instructionSources":[long,"/native/AGENTS.md"]}), "next".into()).unwrap();
    let s = startup(&changes);
    assert!(s.get("cwd").is_none());
    assert_eq!(s["instructions"], json!([{"path":"/native/AGENTS.md"}]));
    assert!(s["incomplete"].as_array().unwrap().contains(&json!("cwd")));
    let changes = e.finish_startup_inventory("s", "next", "plugins", Some(&json!({"marketplaces":[{"plugins":[{"name":"p","installed":true,"source":{"type":"local","path":long}}]}]})));
    assert!(startup(&changes)["plugins"][0].get("path").is_none());
    assert!(!startup(&changes).to_string().contains("/native/é"));
}

#[test]
fn over_bound_names_descriptions_identity_and_entity_budget_never_publish_prefixes() {
    let mut e = endpoint();
    let changes = e.initialize_startup(&json!({"userAgent":"n".repeat(129)}));
    assert!(startup(&changes).get("runtime").is_none());
    let path = format!("/native/{}", "x".repeat(2000));
    let changes = e.set_session_response(&json!({"thread":{"id":"s","status":{"type":"idle"}},"cwd":"/native","instructionSources":vec![path.clone();256]}), "next".into()).unwrap();
    let s = startup(&changes);
    let paths = s["instructions"].as_array().unwrap();
    assert!(!paths.is_empty() && paths.len() < 256);
    assert!(paths.iter().all(|p| p["path"] == path));
    assert!(serde_json::to_vec(s).unwrap().len() < vqro::codex::ENTITY_BYTES);
    let changes = e.finish_startup_inventory("s", "next", "skills", Some(&json!({"data":[{"cwd":"/native","errors":[],"skills":[{"name":"n".repeat(129),"enabled":true},{"name":"kept","enabled":true,"description":"d".repeat(1025)}]}]})));
    assert_eq!(startup(&changes)["skills"], json!([{"name":"kept"}]));
    assert!(
        startup(&changes)["incomplete"]
            .as_array()
            .unwrap()
            .contains(&json!("skills"))
    );
}

#[test]
fn native_startup_captures_loaded_paths_effective_settings_and_provenance() {
    let mut e = endpoint();
    let changes = e.set_session_response(&json!({"thread":{"id":"s","status":{"type":"idle"}},"model":"native-model","cwd":"C:\\native\\project","instructionSources":["C:\\native\\AGENTS.md","/remote/rules.md"]}), "next".into()).unwrap();
    let s = startup(&changes);
    assert_eq!(s["model"], "native-model");
    assert_eq!(s["cwd"], "C:\\native\\project");
    assert_eq!(
        s["instructions"],
        json!([{"path":"C:\\native\\AGENTS.md"},{"path":"/remote/rules.md"}])
    );
    assert_eq!(s["sources"]["instructions"], "rpc:thread/start");
    for field in [
        "skills", "plugins", "mcp", "commands", "prompts", "themes", "runtime",
    ] {
        assert!(s.get(field).is_none(), "{field} is unreported, not empty");
    }
}

#[test]
fn absent_and_invalid_native_fields_never_become_zero_or_guessed_values() {
    let mut e = endpoint();
    let changes = e.set_session_response(&json!({"thread":{"id":"s","status":{"type":"idle"}},"model":null,"cwd":3,"instructionSources":null}), "next".into()).unwrap();
    let s = startup(&changes);
    assert!(s.get("model").is_none());
    assert!(s.get("cwd").is_none());
    assert!(s.get("instructions").is_none());
    assert!(s.get("runtime").is_none());
}

#[test]
fn native_paths_are_byte_bounded_and_large_lists_are_explicitly_partial() {
    let mut e = endpoint();
    let paths: Vec<_> = (0..300).map(|_| json!("é".repeat(3000))).collect();
    let changes = e
        .set_session_response(
            &json!({"thread":{"id":"s","status":{"type":"idle"}},"instructionSources":paths}),
            "next".into(),
        )
        .unwrap();
    let s = startup(&changes);
    assert!(s["instructions"].as_array().unwrap().len() <= 256);
    assert!(
        s["instructions"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["path"].as_str().unwrap().len() <= 4096)
    );
    assert!(
        s["incomplete"]
            .as_array()
            .unwrap()
            .contains(&json!("instructions"))
    );
    assert!(serde_json::to_vec(s).unwrap().len() < vqro::codex::ENTITY_BYTES);
}
