use std::io::Read;
use std::time::Duration;

use crate::api::client::ApiClient;
use crate::api::schema::{ExtensionActionInvokeParams, Method, Request};
use serde_json::{json, Value};

pub(super) fn run_task_command(args: &[String]) -> std::io::Result<i32> {
    if args.first().is_some_and(|arg| arg == "tree") {
        return run_tree(&args[1..]);
    }
    let (mut payload, mutation) = match parse_payload(args) {
        Ok(parsed) => parsed,
        Err(message) => {
            eprintln!("{message}");
            return Ok(2);
        }
    };
    let action = payload["action"].as_str().unwrap_or("").to_string();
    if mutation && payload.get("operation_id").is_none() {
        let mut bytes = [0_u8; 16];
        crate::platform::fill_secure_random(&mut bytes)?;
        payload["operation_id"] = bytes
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
            .into();
    }
    let op = payload["operation_id"].as_str().unwrap_or("").to_string();
    let client = super::target::api_client()?;
    let original_caller = if mutation {
        // A read-only preflight captures the original scope for lost-reply lookup.
        // It grants no authority: the extension compares this precondition with
        // the immutable caller supplied afresh by the host on the write.
        match invoke(&client, json!({"action": "whoami"})) {
            Ok(response) => match decode_reply(&response) {
                TaskReply::Success(value)
                    if matches!(
                        value["caller"]["class"].as_str(),
                        Some("agent" | "human_unverified")
                    ) =>
                {
                    Some(value["caller"].clone())
                }
                _ => {
                    eprintln!("task caller unavailable; no write attempted");
                    return Ok(1);
                }
            },
            Err(_) => {
                eprintln!("task caller unavailable; no write attempted");
                return Ok(1);
            }
        }
    } else {
        None
    };
    if let Some(caller) = &original_caller {
        if payload
            .get("expected_caller")
            .is_some_and(|original| original != caller)
        {
            eprintln!(
                "original caller changed; no write attempted; inspect the original operation scope"
            );
            return Ok(1);
        }
        payload["expected_caller"] = caller.clone();
    }
    let reply = match invoke(&client, payload) {
        Ok(response) => decode_reply(&response),
        Err(error) => TaskReply::Unknown(error.to_string()),
    };
    match reply {
        TaskReply::Success(value) => {
            println!("{value}");
            if action == "op" && value["authoritative"] != true {
                eprintln!("not an authoritative result for the original caller; use --caller with its binding");
                return Ok(2);
            }
            Ok(0)
        }
        TaskReply::Rejected(message) => {
            eprintln!("{message}");
            Ok(1)
        }
        TaskReply::Unknown(message) => {
            if mutation {
                eprintln!("outcome unknown; op {op}");
                if let Some(caller) = original_caller {
                    eprintln!("original caller: {caller}");
                    eprintln!("lookup first: vqro task op {op} --caller '{caller}'");
                }
            } else {
                eprintln!("task read unavailable: {message}");
            }
            Ok(1)
        }
    }
}

fn invoke(client: &ApiClient, payload: Value) -> Result<Value, crate::api::client::ApiClientError> {
    client.request_value_with_timeout(
        &Request {
            id: "cli:task".into(),
            method: Method::ExtensionActionInvoke(ExtensionActionInvokeParams {
                service_id: "vqro.tasks.core".into(),
                payload,
            }),
        },
        Duration::from_secs(6),
    )
}

enum TaskReply {
    Success(Value),
    Rejected(String),
    Unknown(String),
}

fn decode_reply(response: &Value) -> TaskReply {
    if let Some(error) = response.get("error") {
        let message = error["message"]
            .as_str()
            .unwrap_or("invalid action error")
            .to_string();
        return if matches!(
            error["code"].as_str(),
            Some("action_rejected" | "invalid_request" | "caller_unknown")
        ) {
            TaskReply::Rejected(message)
        } else {
            TaskReply::Unknown(message)
        };
    }
    if response["result"]["type"] != "extension_action_invoked" {
        return TaskReply::Unknown("unexpected action response".into());
    }
    let result = &response["result"]["value"];
    match result["ok"].as_bool() {
        Some(true) if result.get("value").is_some() => TaskReply::Success(result["value"].clone()),
        Some(false) if result["error"].is_string() => {
            TaskReply::Rejected(result["error"].as_str().unwrap_or("").into())
        }
        _ => TaskReply::Unknown("invalid task outcome".into()),
    }
}

fn parse_payload(args: &[String]) -> Result<(Value, bool), String> {
    let action = args.first().map(String::as_str).ok_or_else(usage)?;
    let mut payload = json!({"action": action});
    let mutation = matches!(
        action,
        "create"
            | "decision"
            | "delegate"
            | "start"
            | "block"
            | "report"
            | "accept"
            | "done"
            | "answer"
            | "plan"
            | "depends"
            | "update"
    );
    let remaining = match (action, args.get(1)) {
        ("list" | "ready" | "whoami", _) => &args[1..],
        ("plan", Some(path)) => {
            payload["nodes"] = read_plan(path)?;
            &args[2..]
        }
        ("get", Some(id)) if args.len() == 2 => {
            payload["id"] = id.clone().into();
            &args[2..]
        }
        ("op", Some(id)) => {
            payload["operation_id"] = id.clone().into();
            &args[2..]
        }
        ("create" | "decision", Some(title)) => {
            payload["title"] = title.clone().into();
            &args[2..]
        }
        ("delegate" | "start" | "accept" | "done" | "depends" | "update", Some(id)) => {
            payload["id"] = id.clone().into();
            &args[2..]
        }
        ("block" | "report" | "answer", Some(id)) if args.len() >= 3 => {
            payload["id"] = id.clone().into();
            payload["text"] = args[2].clone().into();
            &args[3..]
        }
        _ => return Err(usage()),
    };
    let mut seen = std::collections::HashSet::new();
    for pair in remaining.chunks(2) {
        let [flag, value] = pair else {
            return Err(usage());
        };
        if !seen.insert(flag.as_str()) {
            return Err("duplicate option".into());
        }
        match (action, flag.as_str()) {
            ("create" | "decision" | "plan", "--parent") => {
                payload["parent"] = value.clone().into()
            }
            ("done", "--evidence") => payload["text"] = value.clone().into(),
            ("depends", "--on") | ("create" | "decision", "--depends-on") => {
                payload["depends_on"] = serde_json::from_str(value)
                    .map_err(|_| "dependencies must be a JSON list of task IDs".to_string())?;
                if !payload["depends_on"].is_array() {
                    return Err("dependencies must be a JSON list".into());
                }
            }
            ("create" | "decision" | "update", "--done-when" | "--brief-path" | "--due-at") => {
                let key = flag.trim_start_matches("--").replace('-', "_");
                payload[&key] = if value.is_empty() && flag != "--done-when" {
                    Value::Null
                } else {
                    value.clone().into()
                };
            }
            (
                "create" | "decision" | "update",
                "--estimate-work-ms" | "--estimate-wait-ms" | "--actual-work-ms"
                | "--actual-wait-ms",
            ) => {
                let key = flag.trim_start_matches("--").replace('-', "_");
                payload[&key] = value
                    .parse::<u64>()
                    .map_err(|_| {
                        "task timing must be nonnegative integer milliseconds".to_string()
                    })?
                    .into();
            }
            (_, "--op") if mutation => payload["operation_id"] = value.clone().into(),
            (_, "--caller") if mutation => {
                payload["expected_caller"] = serde_json::from_str(value)
                    .map_err(|_| "--caller must be the original binding JSON".to_string())?;
            }
            (_, "--rev") if mutation && !matches!(action, "create" | "decision") => {
                let revision = value
                    .parse::<u64>()
                    .map_err(|_| "--rev must be a positive integer".to_string())?;
                if revision == 0 {
                    return Err("--rev must be positive".into());
                }
                payload["revision"] = revision.into();
            }
            ("delegate", "--owner") => {
                payload["owner"] = serde_json::from_str(value).map_err(|_| {
                    "--owner must be the assignee's binding JSON from task whoami".to_string()
                })?
            }
            ("list" | "ready", "--after") => payload["after"] = value.clone().into(),
            ("op", "--caller") => {
                payload["lookup_caller"] = serde_json::from_str(value)
                    .map_err(|_| "--caller must be the original binding JSON".to_string())?
            }
            _ => return Err(usage()),
        }
    }
    if mutation && payload.get("operation_id").is_some() && payload.get("expected_caller").is_none()
    {
        return Err("an explicit --op requires --caller with the original attempt's binding (use task whoami for a fresh explicit ID)".into());
    }
    if mutation
        && !matches!(action, "create" | "decision")
        && (action != "plan" || payload.get("parent").is_some())
        && payload.get("revision").is_none()
    {
        return Err("--rev is required; inspect the task before writing".into());
    }
    // An exact historical explicit-op replay may predate required criteria. The provider
    // looks up that receipt first; a fresh missing-criteria write still rejects there.
    if matches!(action, "create" | "decision")
        && payload.get("done_when").is_none()
        && payload.get("operation_id").is_none()
    {
        return Err("--done-when is required: 1-3 observable lines".into());
    }
    if action == "depends" && payload.get("depends_on").is_none() {
        return Err("--on is required: JSON list of prerequisite task IDs".into());
    }
    if action == "plan" && payload.get("revision").is_some() && payload.get("parent").is_none() {
        return Err("plan --rev selects an existing --parent".into());
    }
    if action == "delegate" && payload.get("owner").is_none() {
        return Err("--owner is required; use the assignee's task whoami binding".into());
    }
    Ok((payload, mutation))
}

fn read_plan(path: &str) -> Result<Value, String> {
    let file = std::fs::File::open(path).map_err(|error| format!("cannot read plan: {error}"))?;
    let mut bytes = Vec::new();
    file.take(64 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("cannot read plan: {error}"))?;
    if bytes.len() > 64 * 1024 {
        return Err("plan exceeds 64 KiB request size".into());
    }
    let plan: Value =
        serde_json::from_slice(&bytes).map_err(|error| format!("invalid plan JSON: {error}"))?;
    let Some(object) = plan.as_object() else {
        return Err("plan file must contain an object with only nodes".into());
    };
    if object.len() != 1 || !object.get("nodes").is_some_and(Value::is_array) {
        return Err("plan file must contain an object with only nodes".into());
    }
    Ok(plan["nodes"].clone())
}

fn run_tree(args: &[String]) -> std::io::Result<i32> {
    let json_output = match args {
        [] => false,
        [flag] if flag == "--json" => true,
        _ => {
            eprintln!("usage: vqro task tree [--json]");
            return Ok(2);
        }
    };
    let client = super::target::api_client()?;
    let mut tasks = Vec::new();
    let mut cursor = None::<String>;
    loop {
        let mut payload = json!({"action": "list"});
        if let Some(after) = &cursor {
            payload["after"] = after.clone().into();
        }
        let response = match invoke(&client, payload) {
            Ok(response) => response,
            Err(error) => {
                eprintln!("task tree unavailable: {error}");
                return Ok(1);
            }
        };
        let page = match decode_reply(&response) {
            TaskReply::Success(value) => value,
            TaskReply::Rejected(message) | TaskReply::Unknown(message) => {
                eprintln!("task tree unavailable: {message}");
                return Ok(1);
            }
        };
        let Some(rows) = page["tasks"].as_array() else {
            eprintln!("invalid task page");
            return Ok(1);
        };
        tasks.extend(rows.iter().cloned());
        if tasks.len() > 10000 {
            eprintln!("task tree exceeds extension quota");
            return Ok(1);
        }
        let next = page["next_cursor"].as_str().map(str::to_string);
        if let Some(next) = next {
            if cursor.as_ref().is_some_and(|old| old >= &next) || rows.is_empty() {
                eprintln!("non-progressing task cursor");
                return Ok(1);
            }
            cursor = Some(next);
        } else {
            break;
        }
    }
    let text = match render_tree(&tasks) {
        Ok(text) => text,
        Err(error) => {
            eprintln!("task tree unavailable: {error}");
            return Ok(1);
        }
    };
    if json_output {
        println!("{}", json!({"tasks": tasks}));
    } else {
        print!("{text}");
    }
    Ok(0)
}

fn render_tree(tasks: &[Value]) -> Result<String, String> {
    use std::collections::{BTreeMap, HashSet};
    let mut records = BTreeMap::new();
    let mut children = BTreeMap::<Option<String>, Vec<String>>::new();
    for task in tasks {
        let id = task["id"].as_str().ok_or("missing task id")?.to_string();
        if records.insert(id.clone(), task).is_some() {
            return Err("duplicate task id".into());
        }
        let parent = match &task["parent"] {
            Value::Null => None,
            Value::String(parent) => Some(parent.clone()),
            _ => return Err("invalid parent".into()),
        };
        children.entry(parent).or_default().push(id);
    }
    for parent in children.keys().flatten() {
        if !records.contains_key(parent) {
            return Err("missing parent; tree may have changed during paginated read".into());
        }
    }
    for ids in children.values_mut() {
        ids.sort();
    }
    let mut stack = children
        .get(&None)
        .into_iter()
        .flatten()
        .rev()
        .map(|id| (id.clone(), 0_usize))
        .collect::<Vec<_>>();
    let mut visited = HashSet::new();
    let mut text = String::new();
    while let Some((id, depth)) = stack.pop() {
        if !visited.insert(id.clone()) {
            return Err("task cycle".into());
        }
        let task = records.get(&id).ok_or("missing task")?;
        let indent = "  ".repeat(depth.min(16));
        let depth_note = if depth > 16 {
            format!(" (depth {depth})")
        } else {
            String::new()
        };
        text.push_str(&format!(
            "{indent}{} [{} / {} r{}]{depth_note} {}\n",
            id.escape_default(),
            task["kind"],
            task["state"],
            task["revision"],
            task["title"]
        ));
        text.push_str(&format!(
            "{indent}  owner {} | owes {} {} since {}\n",
            task["owner"], task["owed_to"], task["owed_action"], task["owed_since"]
        ));
        if let Some(ids) = children.get(&Some(id)) {
            stack.extend(ids.iter().rev().map(|id| (id.clone(), depth + 1)));
        }
    }
    if visited.len() != records.len() {
        return Err("task cycle or disconnected parent graph".into());
    }
    if tasks.is_empty() {
        text.push_str("(no tasks)\n");
    }
    Ok(text)
}

fn usage() -> String {
    "usage: vqro task <create/decision TITLE --done-when TEXT [--parent ID]|plan FILE [--parent ID --rev N]|depends ID --on JSON --rev N|update ID --rev N [metadata options]|delegate ID --owner JSON --rev N|start/accept/done ID --rev N [done --evidence TEXT]|block/report/answer ID TEXT --rev N|list/ready [--after ID]|tree [--json]|whoami|get ID|op ID [--caller JSON]>; explicit --op ID requires --caller JSON from the original attempt".into()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn task_writes_require_explicit_revision_and_delegation_binding() {
        for args in [
            vec!["start", "task_a"],
            vec!["accept", "task_a", "--rev", "0"],
            vec!["delegate", "task_a", "--rev", "1"],
        ] {
            assert!(
                parse_payload(&args.into_iter().map(String::from).collect::<Vec<_>>()).is_err()
            );
        }
        for action in ["start", "accept", "done"] {
            let (payload, mutation) =
                parse_payload(&[action, "task_a", "--rev", "2"].map(String::from)).unwrap();
            assert!(mutation);
            assert_eq!(payload["revision"], 2);
        }
        let (payload, mutation) =
            parse_payload(&["answer", "task_a", "smaller path", "--rev", "1"].map(String::from))
                .unwrap();
        assert!(mutation);
        assert_eq!(payload["text"], "smaller path");
        assert!(parse_payload(
            &["start", "task_a", "--rev", "1", "--actor", "human"].map(String::from)
        )
        .is_err());
    }

    #[test]
    fn explicit_operation_requires_original_binding_not_current_preflight() {
        assert!(parse_payload(&["create", "retry", "--op", "old"].map(String::from)).is_err());
        let original =
            json!({"class":"agent","terminal_id":"term_a","agent":"pi","execution":"boot:10:100"});
        let (payload, mutation) = parse_payload(&[
            "create".into(),
            "retry".into(),
            "--op".into(),
            "old".into(),
            "--caller".into(),
            original.to_string(),
            "--done-when".into(),
            "Observable result".into(),
        ])
        .unwrap();
        assert!(mutation);
        assert_eq!(payload["expected_caller"], original);
    }

    fn tree_task(id: &str, parent: Option<&str>, kind: &str, state: &str, owes: Value) -> Value {
        json!({"id": id, "parent": parent, "kind": kind, "state": state, "revision": 1, "title": "evidence", "owner": {"class":"agent","terminal_id":"term_a"}, "owed_to": owes, "owed_action": "accept", "owed_since": "2026-10-02T00:00:00+00:00"})
    }

    #[test]
    fn task_tree_projects_extension_supplied_debts_and_rejects_bad_graphs() {
        let root = tree_task(
            "root",
            None,
            "task",
            "in_progress",
            json!({"class":"agent"}),
        );
        let child = tree_task(
            "child",
            Some("root"),
            "task",
            "reported",
            json!({"role":"human_unverified"}),
        );
        let text = render_tree(&[child.clone(), root.clone()]).unwrap();
        assert!(text.find("root [").unwrap() < text.find("  child [").unwrap());
        assert!(text.contains("human_unverified"));
        assert!(text.contains("2026-10-02T00:00:00+00:00"));
        assert!(render_tree(std::slice::from_ref(&child)).is_err());
        assert!(render_tree(&[root.clone(), root]).is_err());
        let cycle = [
            tree_task("a", Some("b"), "task", "open", Value::Null),
            tree_task("b", Some("a"), "task", "open", Value::Null),
        ];
        assert!(render_tree(&cycle).is_err());
    }

    #[test]
    fn deep_tree_indent_is_bounded_and_control_text_is_escaped() {
        let mut tasks = Vec::new();
        for depth in 0..20 {
            let parent = (depth > 0).then(|| format!("task_{}", depth - 1));
            let mut task = tree_task(
                &format!("task_{depth}"),
                parent.as_deref(),
                "task",
                "open",
                Value::Null,
            );
            task["title"] = "unsafe\u{1b}[31m\ntext".into();
            tasks.push(task);
        }
        let text = render_tree(&tasks).unwrap();
        assert!(text.contains("(depth 19)"));
        assert!(!text.contains('\u{1b}'));
        assert!(!text.lines().any(|line| line.starts_with(&" ".repeat(35))));
    }

    #[test]
    fn receipt_lookup_selector_is_not_a_caller_authority_flag() {
        let args = [
            "op",
            "op-1",
            "--caller",
            r#"{"class":"agent","terminal_id":"term_a","agent":"pi","execution":"boot:1:2"}"#,
        ]
        .map(String::from);
        let (payload, mutation) = parse_payload(&args).unwrap();
        assert!(!mutation);
        assert!(payload.get("lookup_caller").is_some());
        assert!(payload.get("caller").is_none());
    }
    #[test]
    fn graph_metadata_and_ready_commands_remain_extension_payloads() {
        let (payload, mutation) = parse_payload(
            &[
                "create",
                "child",
                "--parent",
                "task_root",
                "--done-when",
                "File is verified.",
                "--brief-path",
                "brief.md",
                "--estimate-work-ms",
                "100",
                "--actual-wait-ms",
                "0",
                "--depends-on",
                "[\"task_proof\"]",
            ]
            .map(String::from),
        )
        .unwrap();
        assert!(mutation);
        assert_eq!(payload["done_when"], "File is verified.");
        assert_eq!(payload["estimate_work_ms"], 100);
        assert_eq!(payload["actual_wait_ms"], 0);
        assert_eq!(payload["depends_on"], json!(["task_proof"]));
        let (payload, mutation) =
            parse_payload(&["ready", "--after", "task_a"].map(String::from)).unwrap();
        assert!(!mutation);
        assert_eq!(payload["action"], "ready");
        assert_eq!(payload["after"], "task_a");
    }

    #[test]
    fn criteria_required_but_original_legacy_intent_may_replay() {
        assert!(parse_payload(&["create", "new"].map(String::from)).is_err());
        let caller =
            r#"{"class":"agent","terminal_id":"term_a","agent":"pi","execution":"boot:1:2"}"#;
        let (payload, _) = parse_payload(
            &["create", "old", "--op", "old-create", "--caller", caller].map(String::from),
        )
        .unwrap();
        assert!(payload.get("done_when").is_none());
        assert_eq!(payload["operation_id"], "old-create");
    }

    #[test]
    fn dependency_evidence_and_timing_flags_require_original_revision() {
        assert!(parse_payload(&["depends", "task_a", "--on", "[]"].map(String::from)).is_err());
        let (payload, mutation) =
            parse_payload(&["depends", "task_a", "--on", "[]", "--rev", "3"].map(String::from))
                .unwrap();
        assert!(mutation);
        assert_eq!(payload["depends_on"], json!([]));
        let (payload, _) = parse_payload(
            &[
                "done",
                "task_a",
                "--rev",
                "3",
                "--evidence",
                "> File exists.\nEvidence: checked",
            ]
            .map(String::from),
        )
        .unwrap();
        assert!(payload["text"].as_str().unwrap().contains("Evidence:"));
        for value in ["-1", "true", "1.5"] {
            assert!(parse_payload(
                &["update", "task_a", "--rev", "3", "--actual-work-ms", value].map(String::from)
            )
            .is_err());
        }
    }

    #[test]
    fn plan_file_is_bounded_and_attachment_requires_revision() {
        let mut nonce = [0u8; 8];
        crate::platform::fill_secure_random(&mut nonce).unwrap();
        let root =
            std::env::temp_dir().join(format!("vqro-plan-test-{:016x}", u64::from_ne_bytes(nonce)));
        std::fs::create_dir(&root).unwrap();
        let file = root.join("plan.json");
        std::fs::write(
            &file,
            r#"{"nodes":[{"key":"proof","title":"proof","done_when":"File exists."}]}"#,
        )
        .unwrap();
        let path = file.to_str().unwrap();
        let (payload, mutation) = parse_payload(&["plan", path].map(String::from)).unwrap();
        assert!(mutation);
        assert_eq!(payload["nodes"][0]["key"], "proof");
        assert!(
            parse_payload(&["plan", path, "--parent", "task_parent"].map(String::from)).is_err()
        );
        assert!(parse_payload(&["plan", path, "--rev", "1"].map(String::from)).is_err());
        std::fs::write(&file, r#"{"nodes":[],"controller":true}"#).unwrap();
        assert!(read_plan(path).is_err());
        std::fs::write(&file, vec![b' '; 64 * 1024 + 1]).unwrap();
        assert!(read_plan(path).unwrap_err().contains("64 KiB"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn lost_or_malformed_reply_never_becomes_definite_rejection() {
        for response in [
            json!({"error":{"code":"outcome_unknown","message":"lost"}}),
            json!({}),
            json!({"result":{"type":"extension_action_invoked","value":{"ok":true}}}),
        ] {
            assert!(matches!(decode_reply(&response), TaskReply::Unknown(_)));
        }
        assert!(matches!(
            decode_reply(&json!({"error":{"code":"action_rejected","message":"not admitted"}})),
            TaskReply::Rejected(_)
        ));
        assert!(matches!(
            decode_reply(
                &json!({"result":{"type":"extension_action_invoked","value":{"ok":false,"error":"stale revision"}}})
            ),
            TaskReply::Rejected(_)
        ));
    }
}
