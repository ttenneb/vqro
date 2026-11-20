//! Base-profile conformance fixtures (D29): the renderer-independent model (`vqro::ui_model`) must produce the
//! expected output of every case in `tests/fixtures/client/*.json`. Other clients (the Python text client in
//! `clients/`) check themselves against the same files. `VQRO_FIXTURE_UPDATE=1 cargo test --test
//! client_conformance` rewrites the `expect` fields from the model; review the diff before committing.
use serde_json::{json, Value};
use vqro::ui_model::{composer, links, markdown, panel, recovery, tabbar, view, window::{self, Settled, Window}, workspace::{resolve, Workspace}};

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
        state.facets = s["facets"].as_object().map(|o| o.iter().filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string()))).collect()).unwrap_or_default();
        state.sort = s["sort"].as_u64().map(|n| n as usize);
        let p = &case["panel"];
        let custom: Option<Vec<&str>> = case["features"].as_array().map(|a| a.iter().filter_map(Value::as_str).collect());
        let features = custom.as_deref().unwrap_or(panel::BASE_FEATURES);
        let rows: Vec<Value> = panel::rows_for(p, &state, features).iter().map(|r| json!({"id": r.id, "depth": r.depth, "text": r.item["text"],
            "type": panel::kind(&r.item), "has_children": r.has_children, "expanded": r.expanded, "pinned": r.pinned,
            "selectable": panel::selectable(&r.item), "bottom": r.bottom, "chips": r.item.get("chips").map(|c| json!(c.as_array().into_iter().flatten().map(|c| c["text"].clone()).collect::<Vec<_>>()))})).collect();
        let enhancement = p["list_detail"].is_object().then(|| panel::missing_for(&p["list_detail"], panel::BASE_FEATURES).is_empty());
        let mut out = json!({"rows": rows, "missing": panel::missing_for(p, panel::BASE_FEATURES), "list_detail": enhancement});
        if let Some(q) = s["search"].as_str() {
            out["search"] = json!(panel::search(p, &state, q, features).into_iter().map(|(id, open)| json!({"id": id, "open": open})).collect::<Vec<_>>());
        }
        // Facet line counts and the sort label, where the panel declares them (features `facets`, `sort`).
        let counts = panel::facet_counts(p, &state, features);
        if !counts.is_empty() {
            out["facets"] = json!(counts.iter().map(|c| json!({"key": c.key, "label": c.label, "chosen": c.chosen, "all": c.all,
                "values": c.values.iter().map(|(v, l, n)| json!({"value": v, "label": l, "count": n})).collect::<Vec<_>>()})).collect::<Vec<_>>());
        }
        if features.contains(&"sort") { if let Some(label) = panel::sort_label(p, &state) { out["sort"] = json!(label); } }
        // Column widths for a given width, where the panel declares `columns` (feature `columns`).
        if let (Some(spec), Some(width)) = (panel::declared(p, &state, "columns"), case["width"].as_u64()) {
            if features.contains(&"columns") {
                out["column_widths"] = json!(panel::column_widths(spec, &panel::rows_for(p, &state, features), width as usize));
            }
        }
        out
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
        let scoped: Vec<Value> = case["scoped"].as_array().into_iter().flatten().map(|q| {
            ws.scoped_binding(q[0].as_str().unwrap(), q[1].as_str().unwrap(), &q[2]).map(|b| b["command"].clone()).unwrap_or(Value::Null)
        }).collect();
        if case.get("scoped").is_some() { json!({"menus": menus, "keys": keys, "gestures": gestures, "scoped": scoped}) }
        else { json!({"menus": menus, "keys": keys, "gestures": gestures}) }
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

#[test]
fn windows_show_their_own_layout_like_the_model() {
    check("window.json", |case| {
        let mut ws = workspace(&case["snapshot"]);
        let mut win = Window::default();
        let settled = |s: Settled| match s {
            Settled::Same => json!("Same"),
            Settled::Took(l) => json!({"Took": l}),
            Settled::Closed { was, now } => json!({"Closed": {"was": was, "now": now}}),
        };
        let result = |r: Result<bool, String>| match r { Ok(changed) => json!({"ok": changed}), Err(e) => json!({"error": e}) };
        let steps: Vec<Value> = case["steps"].as_array().unwrap().iter().map(|step| {
            let out = match step["do"].as_str().unwrap() {
                "start" => json!({"settled": settled(win.settle(&ws))}),
                "event" => { ws.apply(&step["event"]); json!({"settled": settled(win.settle(&ws))}) }
                "show" => json!(result(win.show(&ws, step["layout"].as_str().unwrap()))),
                "client.layout" => json!(result(win.client_layout(&ws, &step["args"]))),
                "reply" => {
                    let hinted = window::reply_layout(step["command"].as_str().unwrap(), &step["reply"]);
                    if let Some(l) = &hinted { let _ = win.show(&ws, l); }
                    json!({"reply_layout": hinted})
                }
                "invoke" => json!({"ctx": win.invocation_context(&ws, step["ctx"].clone())}),
                "context" => json!({"context": win.context(&ws)}),
                "mark" => { let mut p = step["panel"].clone(); window::mark_current(&mut p, win.shown(&ws).as_deref()); json!({"panel": p}) }
                other => panic!("unknown step {other}"),
            };
            let mut out = out;
            out["shown"] = json!(win.shown(&ws));
            out
        }).collect();
        json!(steps)
    });
}

#[test]
fn view_fallbacks_resolve_like_the_model() {
    let (_, doc) = fixture("views.json");
    let entries: std::collections::BTreeMap<String, Value> = doc["entries"].as_object().unwrap().iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    let live: std::collections::HashMap<String, Value> = doc["live"].as_object().unwrap().iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    let kinds: Vec<&str> = doc["kinds"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
    check("views.json", |case| {
        let (shown, chain) = view::shown(case["view"].as_str().unwrap(), &|id| view::merged(id, &entries, &live),
            &|v| view::renders(v, &kinds, panel::BASE_FEATURES));
        json!({"shown": shown, "chain": chain})
    });
}

#[test]
fn markdown_lays_out_like_the_model() {
    check("markdown.json", |case| {
        let lines = markdown::lines(case["text"].as_str().unwrap(), case["width"].as_u64().unwrap() as usize);
        json!(lines.iter().map(|l| l.iter().map(|(t, s)| json!([t, s])).collect::<Vec<_>>()).collect::<Vec<_>>())
    });
}

#[test]
fn composer_edits_like_the_model() {
    check("composer.json", |case| {
        let row = &case["row"];
        let mut c = composer::Composer::new(row);
        let mut outcomes = Vec::new();
        let mut args = Value::Null;
        for key in case["keys"].as_array().unwrap().iter().filter_map(Value::as_str) {
            let outcome = if let Some(text) = key.strip_prefix("paste:") { c.editor.insert(text) }
                else if let Some(set) = key.strip_prefix("set:") { let (k, v) = set.split_once('=').unwrap(); c.values.insert(k.into(), json!(v)); composer::Outcome::Edited }
                else { c.editor.key(key) };
            if outcome == composer::Outcome::Submit { args = c.submit_args(row); }
            outcomes.push(outcome.name());
        }
        json!({"outcomes": outcomes, "text": c.editor.string(), "cursor": c.editor.cursor, "args": args})
    });
}

#[test]
fn links_like_the_model() {
    check("links.json", |case| json!(links::find_all(case["text"].as_str().unwrap()).iter().map(links::Link::describe).collect::<Vec<_>>()));
}

#[test]
fn tab_marks_like_the_model() {
    check("tabmark.json", |case| {
        let now: Vec<&Value> = case["now"].as_array().unwrap().iter().collect();
        let seen: Vec<&Value> = case["seen"].as_array().unwrap().iter().collect();
        json!(tabbar::tab_mark(case.get("setting"), &now, &seen))
    });
}

#[test]
fn tab_states_like_the_model() {
    check("tabstate.json", |case| json!(tabbar::tab_state(&case["tab"])));
}

#[test]
fn tab_titles_like_the_model() {
    check("tabtitle.json", |case| {
        let resource = Some(&case["resource"]).filter(|r| !r.is_null());
        json!(tabbar::tab_title(case.get("setting"), case["title"].as_str().unwrap(), resource))
    });
}

#[test]
fn tab_bar_labels_like_the_model() {
    check("tabbar.json", |case| {
        let titles: Vec<&str> = case["titles"].as_array().unwrap().iter().map(|t| t.as_str().unwrap()).collect();
        tabbar::describe(case.get("setting"), &titles, case["active"].as_u64().unwrap() as usize)
    });
}
