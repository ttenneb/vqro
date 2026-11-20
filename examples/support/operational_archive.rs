//! Test-only protected archive workflow selectors, not destination-file fsync cuts.
use super::predecessor;
use serde_json::Value;
use std::path::Path;

pub fn mode(fault: &str) -> Option<(&str, &str)> {
    let (stage, action) = fault.strip_prefix("archive-")?.rsplit_once('-')?;
    (["freeze", "export-intent", "export-confirm", "free"].contains(&stage)
        && ["before", "after", "error", "unconfirmed"].contains(&action))
    .then_some((stage, action))
}

// Select the actual protected transition. Cases independently check source CAS,
// real destination bytes/readback and old-or-new atomic state after freeing.
pub fn transition(path: &Path, candidate: &Value, stage: &str) -> bool {
    let previous = predecessor(path);
    let changed = candidate["last"]["changes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|change| change["key"].as_str())
        .collect::<Vec<_>>();
    changed.iter().any(|key| {
        let old = &previous["entries"][*key]["value"];
        let new = &candidate["entries"][*key]["value"];
        if stage == "export-intent" {
            if !key.starts_with("operational/coord/external/")
                || new["command"] != "coord.inbox.archive.export"
                || new["state"] != "claimed"
                || old["state"] == "claimed"
            {
                return false;
            }
            let Some(id) = new["intent"]["archive"].as_str() else {
                return false;
            };
            let plan_key = format!("operational/coord/archive/{id}");
            let before = &previous["entries"][&plan_key]["value"];
            let after = &candidate["entries"][&plan_key]["value"];
            return before["state"] == "frozen"
                && after["state"] == "frozen"
                && before["frozen"] == after["frozen"];
        }
        if !key.starts_with("operational/coord/archive/") {
            return false;
        }
        match stage {
            "freeze" => {
                old.is_null()
                    && new["state"] == "frozen"
                    && new["frozen"].is_object()
                    && new["destination"].is_string()
            }
            "export-confirm" => {
                matches!(old["state"].as_str(), Some("frozen" | "exporting"))
                    && new["state"] == "exported"
                    && new["locator"].is_string()
                    && new["manifest_hash"].is_string()
                    && old["frozen"] == new["frozen"]
                    && old["destination"] == new["destination"]
            }
            "free" => {
                old["state"] == "exported"
                    && matches!(new["state"].as_str(), Some("completed" | "partial"))
                    && old["locator"].is_string()
                    && old["manifest_hash"].is_string()
                    && old["locator"] == new["locator"]
                    && old["manifest_hash"] == new["manifest_hash"]
                    && changed.contains(&"operational/coord/archive-index")
            }
            _ => false,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn modes_are_bounded_and_workflow_specific() {
        assert_eq!(
            mode("archive-export-confirm-unconfirmed"),
            Some(("export-confirm", "unconfirmed"))
        );
        assert!(mode("archive-destination-fsync-error").is_none());
        assert!(mode("archive-free-unknown").is_none());
    }
    #[test]
    fn plan_transitions_match_once_and_preserve_selection() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("domain.json");
        let key = "operational/coord/archive/A";
        let frozen =
            json!({"state":"frozen","frozen":{"message/D":3},"destination":"private.json"});
        let mut candidate =
            json!({"last":{"changes":[{"key":key}]},"entries":{key:{"value":frozen}}});
        assert!(transition(&path, &candidate, "freeze"));
        std::fs::write(&path, serde_json::to_vec(&candidate).unwrap()).unwrap();
        assert!(!transition(&path, &candidate, "freeze"));
        candidate["entries"][key]["value"]["state"] = json!("exported");
        candidate["entries"][key]["value"]["locator"] = json!("private.json");
        candidate["entries"][key]["value"]["manifest_hash"] = json!("verified");
        assert!(transition(&path, &candidate, "export-confirm"));
        candidate["entries"][key]["value"]["frozen"]["message/D"] = json!(4);
        assert!(!transition(&path, &candidate, "export-confirm"));
        candidate["entries"][key]["value"]["frozen"]["message/D"] = json!(3);
        std::fs::write(&path, serde_json::to_vec(&candidate).unwrap()).unwrap();
        assert!(!transition(&path, &candidate, "export-confirm"));
        candidate["entries"][key]["value"]["state"] = json!("completed");
        assert!(!transition(&path, &candidate, "free"));
        candidate["last"]["changes"]
            .as_array_mut()
            .unwrap()
            .push(json!({"key":"operational/coord/archive-index"}));
        assert!(transition(&path, &candidate, "free"));
    }
    #[test]
    fn export_intent_requires_exact_existing_frozen_plan() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("domain.json");
        let plan = "operational/coord/archive/A";
        let ext = "operational/coord/external/E";
        let before =
            json!({"entries":{plan:{"value":{"state":"frozen","frozen":{"message/D":3}}}}});
        std::fs::write(&path, serde_json::to_vec(&before).unwrap()).unwrap();
        let mut candidate = before.clone();
        candidate["last"] = json!({"changes":[{"key":ext}]});
        candidate["entries"][ext] = json!({"value":{"state":"claimed","command":"coord.inbox.archive.export","intent":{"archive":"A"}}});
        assert!(transition(&path, &candidate, "export-intent"));
        candidate["entries"][ext]["value"]["intent"]["archive"] = json!("B");
        assert!(!transition(&path, &candidate, "export-intent"));
    }
}
