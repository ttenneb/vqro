//! Test/measurement core with explicit durability gates. Not a production entrypoint.
#[path = "support/operational_archive.rs"]
mod archive_cuts;

use serde_json::{json, Value};
use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

fn predecessor(path: &Path) -> Value {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or(Value::Null)
}

// Match the first real K2 claim, not a later step write to an already-claimed row.
fn claim_transition(path: &Path, candidate: &Value) -> bool {
    // Preserve the original standalone operational-oracle fixture's explicit marker.
    if candidate["last"]["note"] == "claim" {
        return true;
    }
    let keys = candidate["last"]["changes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|change| change["key"].as_str())
        .filter(|key| {
            key.starts_with("operational/coord/message/")
                && candidate["entries"][*key]["value"]["state"] == "claimed"
        })
        .collect::<Vec<_>>();
    if keys.is_empty() {
        return false;
    }
    let previous = predecessor(path);
    keys.into_iter().any(|key| {
        matches!(
            previous["entries"][key]["value"]["state"].as_str(),
            Some("held" | "recorded")
        )
    })
}

fn known_steps(record: &Value) -> usize {
    record["attempt"]["steps"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|step| {
            step["outcome"]
                .as_str()
                .is_some_and(|outcome| outcome != "unknown")
        })
        .count()
}

// A native result becomes known on the same durable attempt. This excludes the
// pre-invoke unknown step and also covers recording a prerequisite before a recipe tail.
fn outcome_transition(path: &Path, candidate: &Value) -> bool {
    let previous = predecessor(path);
    candidate["last"]["changes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|change| change["key"].as_str())
        .filter(|key| key.starts_with("operational/coord/message/"))
        .any(|key| {
            let old = &previous["entries"][key]["value"];
            let new = &candidate["entries"][key]["value"];
            old["state"] == "claimed"
                && old["attempt"]["id"].as_str().is_some()
                && old["attempt"]["id"] == new["attempt"]["id"]
                && known_steps(new) > known_steps(old)
        })
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    anyhow::ensure!(
        args.len() == 3 || (args.len() == 4 && args[3] == "--no-bootstrap-owner"),
        "usage: operational-test-core STATE SOCKET FAULT [--no-bootstrap-owner]"
    );
    let state = PathBuf::from(&args[0]);
    let socket = PathBuf::from(&args[1]);
    let fault = args[2].clone();
    anyhow::ensure!(
        [
            "none",
            "claim-before",
            "claim-after",
            "claim-unconfirmed",
            "claim-error",
            "register-before",
            "register-error",
            "outcome-before",
            "outcome-error",
            "outcome-unconfirmed"
        ]
        .contains(&fault.as_str())
            || archive_cuts::mode(&fault).is_some(),
        "unknown fault"
    );
    if args.len() == 3 && !state.join("workspace.json").exists() {
        let mut store = vqro::store::Store::open(state.join("workspace.json"))?;
        store.apply("fixture", None, vec![vqro::store::Op::Set {
            key: "extension/owner".into(),
            value: json!({"enabled":true,"argv":["/usr/bin/python3",PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/managed_identity.py"),state.join("owner.token")]}), expect: None,
        }])?;
    }
    let core = vqro::server::start(
        socket,
        state.clone(),
        false,
        vqro::server::Backend::Fake,
        None,
    )?;
    let ready = state.join("ready");
    let once = Arc::new(AtomicBool::new(false));
    core.lock()
        .unwrap()
        .configure_operational_writer(Arc::new(move |path, bytes| {
            if fault == "none" {
                return vqro::store::write_atomic(path, bytes);
            }
            let parsed: Value = serde_json::from_slice(bytes).unwrap();
            let matched = if let Some((stage, _)) = archive_cuts::mode(&fault) {
                archive_cuts::transition(path, &parsed, stage)
            } else if fault.starts_with("register-") {
                parsed["last"]["note"]
                    .as_str()
                    .is_some_and(|note| note.starts_with("operational.register"))
            } else if fault.starts_with("outcome-") {
                outcome_transition(path, &parsed)
            } else {
                claim_transition(path, &parsed)
            };
            if !matched || once.swap(true, Ordering::AcqRel) {
                return vqro::store::write_atomic(path, bytes);
            }
            let phase = if let Some((stage, _)) = archive_cuts::mode(&fault) {
                format!("before-archive-{stage}-write")
            } else if fault.starts_with("register-") {
                "before-register-write".to_owned()
            } else if fault.starts_with("outcome-") {
                "before-outcome-write".to_owned()
            } else {
                "before-claim-write".to_owned()
            };
            std::fs::write(state.join("fault-boundary"), phase)?;
            if fault.ends_with("before") {
                while !state.join("release-fault").exists() {
                    std::thread::sleep(Duration::from_millis(2));
                }
            }
            if fault.ends_with("error") {
                return Err(std::io::Error::other("injected before rename").into());
            }
            vqro::store::write_atomic(path, bytes)?;
            if fault == "claim-after"
                || archive_cuts::mode(&fault).is_some_and(|(_, action)| action == "after")
            {
                let phase = archive_cuts::mode(&fault)
                    .map(|(stage, _)| format!("after-durable-archive-{stage}-before-receipt"))
                    .unwrap_or_else(|| "after-durable-before-receipt".to_owned());
                std::fs::write(state.join("fault-boundary"), phase)?;
                while !state.join("release-fault").exists() {
                    std::thread::sleep(Duration::from_millis(2));
                }
            }
            if fault.ends_with("unconfirmed") {
                if let Some((stage, _)) = archive_cuts::mode(&fault) {
                    std::fs::write(
                        state.join("fault-boundary"),
                        format!("after-rename-archive-{stage}-unconfirmed"),
                    )?;
                }
                let mut error = vqro::store::AtomicWriteError::from(std::io::Error::other(
                    "injected missing directory confirmation",
                ));
                error.renamed = true;
                return Err(error);
            }
            Ok(())
        }))
        .unwrap();
    std::fs::write(ready, "ready")?;
    let shutdown = core.lock().unwrap().shutdown.clone();
    tokio::select! { _ = shutdown.stopped() => {}, _ = tokio::signal::ctrl_c() => vqro::server::shutdown(&core).await }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn write(path: &Path, value: &Value) {
        std::fs::write(path, serde_json::to_vec(value).unwrap()).unwrap();
    }
    #[test]
    fn real_claim_is_not_a_later_step_write() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("domain.json");
        let key = "operational/coord/message/42";
        let candidate = json!({"last":{"note":"coordination","changes":[{"key":key}]},"entries":{key:{"value":{"state":"claimed"}}}});
        assert!(!claim_transition(&path, &candidate));
        write(&path, &json!({"entries":{key:{"value":{"state":"held"}}}}));
        assert!(claim_transition(&path, &candidate));
        write(&path, &candidate);
        assert!(!claim_transition(&path, &candidate));
    }
    #[test]
    fn outcome_requires_same_claim_and_new_known_native_step() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("domain.json");
        let key = "operational/coord/message/42";
        let old = json!({"state":"claimed","attempt":{"id":"A","steps":[{"op":"agent.send","outcome":"unknown"}]}});
        write(&path, &json!({"entries":{key:{"value":old}}}));
        let mut candidate = json!({"last":{"changes":[{"key":key}]},"entries":{key:{"value":old}}});
        assert!(!outcome_transition(&path, &candidate));
        candidate["entries"][key]["value"]["attempt"]["steps"][0]["outcome"] = json!("accepted");
        assert!(outcome_transition(&path, &candidate));
        assert!(!claim_transition(&path, &candidate));
        candidate["entries"][key]["value"]["attempt"]["id"] = json!("B");
        assert!(!outcome_transition(&path, &candidate));
    }
}
