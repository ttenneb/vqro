//! Secondary's synthetic native-source counterexample: a Git selector is not a host path.
use serde_json::json;
use vqro::codex::Endpoint;

#[test]
fn git_source_selector_must_not_be_promoted_to_a_runtime_host_path() {
    let mut e = Endpoint::new("e".into(), "inc".into(), "host".into(), "old".into());
    e.set_session_response(
        &json!({"thread":{"id":"s","status":{"type":"idle"}},"cwd":"/native/project","model":"m"}),
        "epoch".into(),
    )
    .unwrap();
    let changes = e.finish_startup_inventory("s", "epoch", "plugins", Some(&json!({"marketplaces":[{"name":"synthetic-marketplace","plugins":[{"name":"git-plugin","id":"synthetic-git","authPolicy":"ON_USE","installPolicy":"AVAILABLE","installed":true,"enabled":true,"localVersion":"1.2.3","source":{"type":"git","url":"https://example.invalid/powerless.git","path":"plugins/example"}}]}]})));
    let value = &changes.iter().find(|c| c.kind == "startup").unwrap().value;
    assert_eq!(value["plugins"][0]["name"], "git-plugin");
    assert_eq!(value["plugins"][0]["enabled"], true);
    assert_eq!(value["plugins"][0]["version"], "1.2.3");
    assert!(
        value["plugins"][0].get("path").is_none(),
        "unproven Git selector became host path: {value}"
    );
}

#[test]
fn source_variant_not_spelling_decides_host_path_meaning() {
    let mut e = Endpoint::new("e".into(), "inc".into(), "host".into(), "old".into());
    e.set_session_response(
        &json!({"thread":{"id":"s","status":{"type":"idle"}}}),
        "epoch".into(),
    )
    .unwrap();
    let changes = e.finish_startup_inventory("s", "epoch", "plugins", Some(&json!({"marketplaces":[{"name":"synthetic-marketplace","plugins":[
        {"name":"local","id":"local-id","authPolicy":"ON_USE","installPolicy":"AVAILABLE","enabled":true,"installed":true,"source":{"type":"local","path":"C:\\native\\plugin"}},
        {"name":"git","id":"git-id","authPolicy":"ON_USE","installPolicy":"AVAILABLE","enabled":true,"installed":true,"source":{"type":"git","url":"https://example.invalid/repo.git","path":"/looks/absolute"}},
        {"name":"remote","id":"remote-id","authPolicy":"ON_USE","installPolicy":"AVAILABLE","enabled":true,"installed":true,"source":{"type":"remote","path":"/not/a/local/source"}}
    ]}]})));
    let value = &changes.iter().find(|c| c.kind == "startup").unwrap().value;
    assert_eq!(value["plugins"][0]["path"], "C:\\native\\plugin");
    assert!(value["plugins"][1].get("path").is_none());
    assert!(value["plugins"][2].get("path").is_none());
    assert!(!value.to_string().contains("https://example.invalid"));
}
