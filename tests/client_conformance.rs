//! Base-profile conformance fixtures (D29): the renderer-independent model (`vqro::ui_model`) must produce the
//! expected output of every case in `tests/fixtures/client/*.json`. Other clients (the Python text client in
//! `clients/`) check themselves against the same files. `VQRO_FIXTURE_UPDATE=1 cargo test --test
//! client_conformance` rewrites the `expect` fields from the model; review the diff before committing.
use serde_json::{json, Value};
use vqro::ui_model::{panel, recovery, workspace::{resolve, Workspace}};

fn fixture(name: &str) -> (std::path::PathBuf, Value) {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/client").join(name);
    let value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    (path, value)
}

/// Compare (or, when updating, record) every case's `expect`.
fn check(name: &str, run: impl Fn(&Value) -> Value) {
    let (path, mut doc) = fixture(name);
    let update = std::env::var_os("VQRO_FIXTURE_UPDATE").is_some();
    for case in doc["cases"].as_array_mut().unwrap() {
        let got = run(case);
        if update { case["expect"] = got; } else { assert_eq!(got, case["expect"], "{name}: {}", case["name"]); }
    }
    if update {
        if name == "panels.json" { doc["base_features"] = json!(panel::BASE_FEATURES); }
        std::fs::write(&path, serde_json::to_string_pretty(&doc).unwrap() + "\n").unwrap();
    } else if name == "panels.json" {
        assert_eq!(doc["base_features"], json!(panel::BASE_FEATURES), "base profile features changed: update the spec and fixtures");
    }
}

fn workspace(snapshot: &Value) -> Workspace {
    let mut ws = Workspace::default();
    ws.load(snapshot);
    ws
}

#[test]
fn panels_flatten_like_the_model() {
    check("panels.json", |case| {
        let s = &case["state"];
        let mut state = panel::State::default();
        state.page = s["page"].as_u64().unwrap_or(0) as usize;
        state.filter = s["filter"].as_str().unwrap_or("").to_string();
        state.open = s["open"].as_object().map(|o| o.iter().map(|(k, v)| (k.clone(), v == true)).collect()).unwrap_or_default();
        let p = &case["panel"];
        let rows: Vec<Value> = panel::rows(p, &state).iter().map(|r| json!({"id": r.id, "depth": r.depth, "text": r.item["text"],
            "type": panel::kind(&r.item), "has_children": r.has_children, "expanded": r.expanded, "pinned": r.pinned,
            "selectable": panel::selectable(&r.item)})).collect();
        let enhancement = p["list_detail"].is_object().then(|| panel::missing_for(&p["list_detail"], panel::BASE_FEATURES).is_empty());
        json!({"rows": rows, "missing": panel::missing_for(p, panel::BASE_FEATURES), "list_detail": enhancement})
    });
}

#[test]
fn contributions_resolve_like_the_model() {
    check("contributions.json", |case| {
        let ws = workspace(&case["snapshot"]);
        let menus: serde_json::Map<String, Value> = case["places"].as_object().unwrap().iter().map(|(place, ctx)| {
            (place.clone(), json!(ws.menu_entries(place, ctx).iter().map(|e| e["label"].clone()).collect::<Vec<_>>()))
        }).collect();
        let cands = ws.key_candidates(&case["ctx"]);
        let keys: serde_json::Map<String, Value> = resolve(&cands).into_iter().map(|(key, i)| {
            let (id, b) = &cands[i];
            (key, if b["command"] == "client.none" { Value::Null } else { json!(id) })
        }).collect();
        let gestures: serde_json::Map<String, Value> = case["gesture_queries"].as_array().unwrap().iter().map(|q| {
            let (key, place) = (q[0].as_str().unwrap(), q[1].as_str().unwrap());
            (format!("{key} {place}"), ws.gesture(key, place).map(|b| b["command"].clone()).unwrap_or(Value::Null))
        }).collect();
        json!({"menus": menus, "keys": keys, "gestures": gestures})
    });
}

#[test]
fn recovery_content_is_the_shared_definition() {
    check("recovery.json", |case| {
        let ws = workspace(&case["snapshot"]);
        let rows: Vec<Value> = recovery::prefix_rows(&ws, &case["ctx"], &|k| k.to_string()).into_iter()
            .map(|r| json!({"key": r.key, "label": r.label, "command": r.binding["command"], "args": r.binding["args"]})).collect();
        let selected = case["selected"].as_array().into_iter().flatten().filter_map(Value::as_str).map(String::from).collect();
        let chooser = ws.safe_mode.then(|| recovery::safe_chooser(&ws, &selected));
        json!({"prefix": rows, "safe_mode_chooser": chooser})
    });
}
