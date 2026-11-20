//! The Python and Rust reducers consume the same contract examples, without normalizing the oracle.
use serde_json::{json, Map, Value};
use vqro::provider_client::items::ItemView;

#[test]
fn shared_viewer_vectors_match_the_python_twin() {
    let mut cases = 0;
    for line in include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/items-vectors.jsonl"
    ))
    .lines()
    {
        let case: Value = serde_json::from_str(line).expect("valid shared vector");
        if case.get("comment").is_some() {
            continue;
        }
        let name = case["name"].as_str().expect("named vector");
        let mut view = ItemView::new();
        for phase in ["snapshot", "records"] {
            for record in case[phase].as_array().expect("record array") {
                let mut bytes = serde_json::to_vec(record).unwrap();
                bytes.push(b'\n');
                view.feed(&bytes);
            }
            if phase == "snapshot" {
                assert_eq!(view.snapshots, 1, "invalid setup snapshot: {name}");
                assert_eq!(view.errors.total(), 0, "invalid setup snapshot: {name}");
            }
        }
        let errors: Map<String, Value> = serde_json::to_value(&view.errors)
            .unwrap()
            .as_object()
            .unwrap()
            .iter()
            .filter(|(_, count)| count.as_u64() != Some(0))
            .map(|(key, count)| (key.clone(), count.clone()))
            .collect();
        assert_eq!(
            json!({"seq":view.seq(), "records":view.records, "broken":view.broken(), "errors":errors}),
            case["expect"],
            "shared vector: {name}"
        );
        cases += 1;
    }
    assert!(cases > 0, "the shared vector fixture must not be empty");
}
