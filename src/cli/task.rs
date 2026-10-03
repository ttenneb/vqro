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
        payload["operation_id"] = super::random_operation_id("")?.into();
    }
    let op = payload["operation_id"].as_str().unwrap_or("").to_string();
    let client = super::target::api_client()?;
    if let Err(message) = resolve_owner_names(&mut payload, |name| {
        super::message::recipient_binding(&client, name)
    }) {
        eprintln!("{message}; no write attempted");
        return Ok(1);
    }
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
            if action == "decision_digest" {
                println!("{}", render_decisions(&value));
            } else {
                println!("{value}");
            }
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

fn render_decisions(value: &Value) -> String {
    fn visible(value: &Value) -> String {
        value
            .as_str()
            .unwrap_or("unknown")
            .escape_debug()
            .to_string()
    }
    let pending = value["needs_you_now"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let automatic = value["auto_since_look"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let more = |key: &str| if value["more"][key] == true { "+" } else { "" };
    let mut lines = vec![format!(
        "Decisions: {}{} pending; {}{} auto since last look. Total auto {}, reverted {}.",
        pending.len(),
        more("needs_you_now"),
        automatic.len(),
        more("auto_since_look"),
        value["counts"]["auto"],
        value["counts"]["revert"]
    )];
    if let Some(oldest) = pending.first() {
        lines.push(format!(
            "Oldest waiting: {} {} ({})",
            visible(&oldest["created_at"]),
            visible(&oldest["title"]),
            visible(&oldest["id"])
        ));
    }
    for task in pending {
        lines.push(format!(
            "{}: {}",
            visible(&task["id"]),
            visible(&task["title"])
        ));
        let decision = &task["decision"];
        if decision.is_null() {
            lines.push("  Legacy: human free-text answer; no automatic default.".into());
            continue;
        }
        let reversible = if decision["reversible"] == true {
            "reversible"
        } else {
            "irreversible"
        };
        let timer = if decision["auto_at"].is_null() {
            "manual".into()
        } else {
            format!(
                "auto at {} (no on-time guarantee)",
                visible(&decision["auto_at"])
            )
        };
        lines.push(format!(
            "  Default {} | {reversible} | {timer}",
            visible(&decision["default"])
        ));
        if let Some(options) = decision["options"].as_array() {
            lines.push(format!(
                "  Options: {}",
                options
                    .iter()
                    .map(|option| format!(
                        "{}={}",
                        visible(&option["key"]),
                        visible(&option["label"])
                    ))
                    .collect::<Vec<_>>()
                    .join("; ")
            ));
        }
        lines.push(format!(
            "  Waiting cost: {}",
            visible(&decision["waiting_cost"])
        ));
    }
    for event in automatic {
        lines.push(format!(
            "Auto {}: {} at {} (applied late by {} ms)",
            visible(&event["id"]),
            visible(&event["result"]["option"]),
            visible(&event["result"]["applied_at"]),
            event["result"]["late_ms"]
        ));
    }
    if value["last_look_known"] == true {
        lines.push(format!(
            "Explicit last look: vqro task decisions-seen --through {}",
            value["ack_through"]
        ));
    } else {
        lines.push(
            "Last-look UID unknown here; run the digest/acknowledgement from a human shell.".into(),
        );
    }
    lines.join("\n")
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
    let command = args.first().map(String::as_str).ok_or_else(usage)?;
    let action = match command {
        "decisions" => "decision_digest",
        "decisions-seen" => "decision_seen",
        "decision-log" => "decision_log",
        "decision-timer" => "decision_timer",
        "revert" => "decision_revert",
        other => other,
    };
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
            | "decision_seen"
            | "decision_timer"
            | "decision_revert"
            | "plan"
            | "depends"
            | "update"
    );
    let remaining = match (action, args.get(1)) {
        ("list" | "ready" | "whoami" | "decision_digest" | "decision_seen" | "decision_log", _) => {
            &args[1..]
        }
        ("decision_timer" | "decision_revert", Some(id)) => {
            payload["id"] = id.clone().into();
            &args[2..]
        }
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
            ("decision", "--options") => {
                payload["options"] = serde_json::from_str(value).map_err(|_| {
                    "--options must be a JSON list of key/label objects".to_string()
                })?;
            }
            ("decision", "--default" | "--waiting-cost") => {
                payload[&flag.trim_start_matches("--").replace('-', "_")] = value.clone().into();
            }
            ("decision", "--reversible") => {
                payload["reversible"] = value
                    .parse::<bool>()
                    .map_err(|_| "--reversible must be true or false".to_string())?
                    .into();
            }
            ("decision" | "decision_timer", "--auto-at") => {
                payload["auto_at"] = if value.is_empty() {
                    Value::Null
                } else {
                    value.clone().into()
                };
            }
            ("answer", "--note") => payload["note"] = value.clone().into(),
            ("decision_seen", "--through") | ("decision_log", "--after") => {
                let key = if action == "decision_seen" {
                    "through"
                } else {
                    "after"
                };
                payload[key] = value
                    .parse::<u64>()
                    .map_err(|_| {
                        "decision sequence/revision must be an unsigned integer".to_string()
                    })?
                    .into();
            }
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
            ("delegate", "--to") => {
                if payload.get("owner").is_some() {
                    return Err("use only one of --to NAME or --owner JSON".into());
                }
                payload["owner"] = value.clone().into();
            }
            ("delegate", "--owner") => {
                if payload.get("owner").is_some() {
                    return Err("use only one of --to NAME or --owner JSON".into());
                }
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
        && !matches!(action, "create" | "decision" | "decision_seen")
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
    if action == "decision"
        && payload.get("operation_id").is_none()
        && ["options", "default", "reversible", "waiting_cost"]
            .iter()
            .any(|key| payload.get(*key).is_none())
    {
        return Err("decision requires --options, --default, --reversible and --waiting-cost; --auto-at is optional".into());
    }
    for (action_name, field, message) in [
        (
            "decision_seen",
            "through",
            "--through is required: explicitly acknowledge the observed sequence",
        ),
        (
            "decision_timer",
            "auto_at",
            "--auto-at is required: UTC deadline or empty to cancel",
        ),
    ] {
        if action == action_name && payload.get(field).is_none() {
            return Err(message.into());
        }
    }
    if action == "depends" && payload.get("depends_on").is_none() {
        return Err("--on is required: JSON list of prerequisite task IDs".into());
    }
    if action == "plan" && payload.get("revision").is_some() && payload.get("parent").is_none() {
        return Err("plan --rev selects an existing --parent".into());
    }
    if action == "delegate" && payload.get("owner").is_none() {
        return Err("--to NAME or --owner JSON is required".into());
    }
    Ok((payload, mutation))
}

// Names are CLI conveniences only. The provider still receives exact bindings,
// and enforces the same caller, ownership, revision and acceptance rules.
fn resolve_owner_names(
    payload: &mut Value,
    mut resolve: impl FnMut(&str) -> Result<Value, String>,
) -> Result<(), String> {
    let mut resolve_owner = |owner: &mut Value| -> Result<(), String> {
        if let Some(name) = owner.as_str() {
            *owner =
                resolve(name).map_err(|error| format!("cannot resolve owner {name}: {error}"))?;
        }
        Ok(())
    };
    if payload["action"] == "delegate" {
        resolve_owner(&mut payload["owner"])?;
    } else if payload["action"] == "plan" {
        if let Some(nodes) = payload["nodes"].as_array_mut() {
            for node in nodes {
                if let Some(owner) = node.get_mut("owner") {
                    resolve_owner(owner)?;
                }
            }
        }
    }
    Ok(())
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
    "usage: vqro task <create/decision TITLE --done-when TEXT [--parent ID]|plan FILE [--parent ID --rev N]|depends ID --on JSON --rev N|update ID --rev N [metadata options]|delegate ID --to NAME (or --owner JSON) --rev N|start/accept/done ID --rev N [done --evidence TEXT]|block/report/answer ID TEXT --rev N (answer uses an option KEY for structured decisions, with optional --note TEXT)|revert ID --rev N|decision-timer ID --rev N --auto-at UTC|decisions|decisions-seen --through SEQ|decision-log [--after SEQ]|list/ready [--after ID]|tree [--json]|whoami|get ID|op ID [--caller JSON]>; new decision also requires --options JSON --default KEY --reversible BOOL --waiting-cost TEXT [--auto-at UTC]; explicit --op ID requires --caller JSON from the original attempt".into()
}

#[cfg(test)]
mod tests {
    #[test]
    fn named_owners_resolve_only_to_exact_bindings_before_writing() {
        let binding = serde_json::json!({"class":"agent", "terminal_id":"term_a", "agent":"pi", "execution":"exec-a"});
        let mut plan = serde_json::json!({"action":"plan", "nodes":[{"key":"root"}, {"key":"a", "owner":"task-a"}, {"key":"b", "owner":binding}]});
        let mut lookups = Vec::new();
        super::resolve_owner_names(&mut plan, |name| {
            lookups.push(name.to_string());
            Ok(binding.clone())
        })
        .unwrap();
        assert_eq!(lookups, ["task-a"]);
        assert_eq!(plan["nodes"][1]["owner"], binding);
        assert_eq!(plan["nodes"][2]["owner"], binding);
        assert!(plan["nodes"][0].get("owner").is_none());
        let mut delegate = serde_json::json!({"action":"delegate", "owner":"missing"});
        assert!(
            super::resolve_owner_names(&mut delegate, |_| Err("agent not found".into()))
                .unwrap_err()
                .contains("cannot resolve owner missing")
        );
    }

    #[test]
    fn delegate_to_name_requires_revision_and_conflicts_with_binding() {
        let args = |items: &[&str]| {
            items
                .iter()
                .map(|item| item.to_string())
                .collect::<Vec<_>>()
        };
        let (payload, mutation) = super::parse_payload(&args(&[
            "delegate", "task_1", "--to", "task-b", "--rev", "1",
        ]))
        .unwrap();
        assert!(mutation);
        assert_eq!(payload["owner"], "task-b");
        assert!(super::parse_payload(&args(&["delegate", "task_1", "--to", "task-b"])).is_err());
        for flags in [
            vec!["--to", "task-b", "--owner", "{}"],
            vec!["--owner", "{}", "--to", "task-b"],
        ] {
            let mut command = vec!["delegate", "task_1", "--rev", "1"];
            command.extend(flags);
            assert!(super::parse_payload(&args(&command))
                .unwrap_err()
                .contains("only one"));
        }
    }

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
    fn structured_decision_filing_and_option_answer_stay_extension_payloads() {
        assert!(parse_payload(
            &["decision", "Question?", "--done-when", "Choice recorded."].map(String::from)
        )
        .is_err());
        let (payload, mutation) = parse_payload(
            &[
                "decision",
                "Question?",
                "--done-when",
                "Choice recorded.",
                "--options",
                r#"[{"key":"yes","label":"Proceed"},{"key":"no","label":"Wait"}]"#,
                "--default",
                "yes",
                "--reversible",
                "true",
                "--waiting-cost",
                "Work waits.",
                "--auto-at",
                "2026-10-02T20:00:00+00:00",
            ]
            .map(String::from),
        )
        .unwrap();
        assert!(mutation);
        assert_eq!(payload["options"].as_array().unwrap().len(), 2);
        assert_eq!(payload["reversible"], true);
        let (answer, mutation) = parse_payload(
            &[
                "answer",
                "task_a",
                "yes",
                "--rev",
                "1",
                "--note",
                "Verified.",
            ]
            .map(String::from),
        )
        .unwrap();
        assert!(mutation);
        assert_eq!(answer["action"], "answer");
        assert_eq!(answer["text"], "yes");
        assert_eq!(answer["note"], "Verified.");
    }

    #[test]
    fn digest_reads_never_ack_and_seen_requires_explicit_sequence() {
        let (digest, mutation) = parse_payload(&["decisions"].map(String::from)).unwrap();
        assert!(!mutation);
        assert_eq!(digest["action"], "decision_digest");
        assert!(parse_payload(&["decisions-seen"].map(String::from)).is_err());
        let (seen, mutation) =
            parse_payload(&["decisions-seen", "--through", "17"].map(String::from)).unwrap();
        assert!(mutation);
        assert_eq!(seen["through"], 17);
    }

    #[test]
    fn decision_digest_is_glanceable_cached_text_and_escapes_controls() {
        let digest = json!({"needs_you_now":[{"id":"task_choice", "title":"Keep?\u{1b}[31m", "decision":{"default":"keep", "reversible":true, "auto_at":null, "waiting_cost":"Work waits.", "options":[{"key":"keep","label":"Keep"},{"key":"change","label":"Change"}]}}], "auto_since_look":[{"id":"task_prior", "result":{"option":"keep","applied_at":"2026-10-02T16:00:00+00:00","late_ms":30000}}], "counts":{"auto":2,"revert":1}, "more":{}, "oldest_waiting":null, "last_look_known":true, "ack_through":17});
        let before = digest.clone();
        let text = render_decisions(&digest);
        assert!(text.contains("1 pending; 1 auto since last look"));
        assert!(text.contains("Total auto 2, reverted 1"));
        assert!(text.contains("Default keep | reversible | manual"));
        assert!(text.contains("keep=Keep; change=Change"));
        assert!(text.contains("applied late by 30000 ms"));
        assert!(text.contains("decisions-seen --through 17"));
        assert!(!text.contains('\u{1b}'));
        assert_eq!(before, digest);
    }

    #[test]
    fn timer_and_revert_require_current_revision_without_implicit_external_effects() {
        assert!(parse_payload(&["revert", "task_a"].map(String::from)).is_err());
        let (revert, mutation) =
            parse_payload(&["revert", "task_a", "--rev", "2"].map(String::from)).unwrap();
        assert!(mutation);
        assert_eq!(revert["action"], "decision_revert");
        let (timer, mutation) = parse_payload(
            &["decision-timer", "task_a", "--rev", "3", "--auto-at", ""].map(String::from),
        )
        .unwrap();
        assert!(mutation);
        assert_eq!(timer["auto_at"], Value::Null);
        assert!(parse_payload(
            &["decisions-seen", "--through", "17", "--op", "ack-1"].map(String::from)
        )
        .is_err());
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
    fn shipped_skill_task_examples_parse_with_current_cli() {
        let skill = include_str!("../../skills/vqro/SKILL.md");
        let mut nonce = [0u8; 8];
        crate::platform::fill_secure_random(&mut nonce).unwrap();
        let root = std::env::temp_dir().join(format!(
            "vqro-skill-test-{:016x}",
            u64::from_ne_bytes(nonce)
        ));
        std::fs::create_dir(&root).unwrap();
        let plan_file = root.join("plan.json");
        std::fs::write(&plan_file, json!({"nodes": [{"key": "proof", "title": "proof", "done_when": "Proof file contains PASS."}]}).to_string()).unwrap();
        // Only the documented single-line literal/variable quoting subset is accepted.
        // No shell execution, expansion or new production parser is involved.
        let words = regex::Regex::new(r#"'([^']*)'|"([^"]*)"|([^\s'"]+)"#).unwrap();
        let binding =
            r#"{"class":"agent","terminal_id":"term_fixture","agent":"pi","execution":"boot:1:2"}"#;
        let mut actions = std::collections::BTreeSet::new();
        let mut attached_plan = false;
        let mut root_plan = false;
        let mut own_evidence = false;
        let mut original_retry = false;
        for line in skill
            .lines()
            .filter_map(|line| line.strip_prefix("vqro task "))
        {
            let mut previous = 0;
            let mut args = Vec::new();
            for capture in words.captures_iter(line) {
                let span = capture.get(0).unwrap();
                assert!(
                    line[previous..span.start()].trim().is_empty(),
                    "unsupported example quoting: {line}"
                );
                previous = span.end();
                let word = (1..=3)
                    .find_map(|index| capture.get(index))
                    .unwrap()
                    .as_str();
                args.push(match word {
                    "$PARENT_ID" => "task_parent".into(),
                    "$TASK_ID" => "task_child".into(),
                    "$REV" => "3".into(),
                    "$PARENT_REV" => "2".into(),
                    "$OWNER" | "$CALLER" => binding.into(),
                    "$CRITERIA" => "Proof file contains PASS.".into(),
                    "$EVIDENCE" => {
                        "> Proof file contains PASS.\nEvidence: result.txt checked".into()
                    }
                    "$CURSOR" => "task_cursor".into(),
                    "$OP_ID" => "original-op".into(),
                    "$PLAN_FILE" => plan_file.to_str().unwrap().into(),
                    _ => word.replace("PREREQUISITE_ID", "task_proof"),
                });
            }
            assert!(
                line[previous..].trim().is_empty(),
                "unsupported example quoting: {line}"
            );
            let (payload, _) = parse_payload(&args)
                .unwrap_or_else(|error| panic!("skill example {line:?}: {error}"));
            let action = payload["action"].as_str().unwrap();
            actions.insert(action.to_string());
            if matches!(action, "create" | "decision") {
                assert!(
                    payload.get("done_when").is_some(),
                    "missing criteria: {line}"
                );
            }
            if action == "plan" {
                attached_plan |= payload.get("parent").is_some();
                root_plan |= payload.get("parent").is_none();
            }
            own_evidence |= action == "done" && payload.get("text").is_some();
            original_retry |= action == "report"
                && payload.get("operation_id").is_some()
                && payload.get("expected_caller").is_some();
        }
        for action in [
            "whoami", "get", "create", "decision", "plan", "ready", "list", "start", "depends",
            "update", "block", "delegate", "report", "accept", "done", "answer", "op",
        ] {
            assert!(
                actions.contains(action),
                "missing shipped example: {action}"
            );
        }
        assert!(attached_plan && root_plan && own_evidence && original_retry);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn shipped_skill_keeps_task_authority_and_delivery_boundaries_explicit() {
        let skill = include_str!("../../skills/vqro/SKILL.md");
        for rule in [
            "Pi todo stays **private scratch**",
            "Agents never create/import ordinary roots",
            "**not a reservation**",
            "Start only your own task",
            "`next_cursor`",
            "prerequisite ownership alone grants no edit right",
            "**Delegate/report record, do not deliver.**",
            "an accept/done receipt can authoritatively record its historical committed outcome",
            "by its assigned owner or cooperative human",
            "**agents never self-accept**",
            "task ID only",
            "even a root decision is allowed",
            "the reply-routing adapter sends that final text back to that agent automatically",
            "anything not labelled as an agent message is treated as human and never routed",
            "Never manufacture human classification",
            "**not proof a queued invocation cannot still commit**",
        ] {
            assert!(skill.contains(rule), "missing skill boundary: {rule}");
        }
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
