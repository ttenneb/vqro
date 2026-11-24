use super::*;
fn started(id: &str, run: &str) -> Value {
    json!({"subtype":"task_started","task_id":id,"run_id":run,"task_type":"local_agent","subagent_type":"worker","spawn_depth":1,"description":"observed","tool_use_id":format!("spawn-{id}")})
}
#[test]
fn native_runs_are_ordered_and_terminal_never_regresses_to_running() {
    let mut w = Work::default();
    w.event(&started("a", "2"));
    w.event(
        &json!({"subtype":"task_notification","task_id":"a","run_id":"2","status":"completed"}),
    );
    w.event(&started("a", "1"));
    w.event(&started("a", "2"));
    assert_eq!(w.tasks["a"].value["status"], "completed");
    w.event(&started("a", "3"));
    assert_eq!(w.tasks["a"].value["status"], "running");
    assert_eq!(w.tasks["a"].value["generation"], "3");
    w.event(&json!({"subtype":"task_notification","task_id":"a","run_id":"3","status":"failed"}));
    w.event(
        &json!({"subtype":"task_notification","task_id":"a","run_id":"3","status":"completed"}),
    );
    assert_eq!(w.tasks["a"].value["status"], "unknown");
    assert_eq!(w.tasks["a"].value["incomplete"], true);
}
#[test]
fn full_level_is_independent_membership_not_terminal_evidence() {
    let mut w = Work::default();
    let(_,bg)=w.level(&json!({"tasks":[{"task_id":"a","run_id":"1","task_type":"local_agent","description":"live"}]}));
    assert_eq!(bg.unwrap().len(), 1);
    assert_eq!(w.tasks["a"].value["status"], "unknown");
    assert_eq!(w.tasks["a"].value["native"]["in_background_level"], true);
    let (changes, bg) = w.level(&json!({"tasks":[]}));
    assert_eq!(bg.unwrap().len(), 0);
    assert!(
        changes
            .iter()
            .any(|c| c.kind == "task" && c.value["native"]["in_background_level"] == false)
    );
    assert_eq!(w.tasks["a"].value["status"], "unknown");
    let (_, bg) = w.level(&json!({"tasks":[{"task_id":"a"},{"task_id":"a"}]}));
    assert!(bg.is_none());
}
#[test]
fn scopes_use_positive_producer_or_unambiguous_native_tool_correlation() {
    let mut w = Work::default();
    w.event(&started("a", "1"));
    let child = w.tasks["a"].child.clone().unwrap();
    assert_eq!(
        w.scope(&json!({"parent_tool_use_id":"spawn-a"})).0,
        Some(child.clone())
    );
    assert_eq!(
        w.scope(&json!({"request":{"agent_id":"a"}})).0,
        Some(child.clone())
    );
    assert_eq!(w.scope(&json!({"parent_tool_use_id":"missing"})).2, true);
    assert_eq!(
        w.scope(&json!({"parent_tool_use_id":"x".repeat(129)})).2,
        true
    );
    let mut b = started("b", "1");
    b["tool_use_id"] = json!("spawn-a");
    w.event(&b);
    assert!(
        w.scope(&json!({"parent_tool_use_id":"spawn-a"}))
            .0
            .is_none()
    );
    let id = w.identity(&child, "tool", "same", true).unwrap();
    assert_eq!(w.identity(&child, "tool", "same", false), Some(id.clone()));
    assert_ne!(w.identity("another", "tool", "same", true).unwrap(), id);
    assert_eq!(w.cancel_target("child", &child), Some("a"));
}
#[test]
fn main_session_task_is_not_fabricated_as_child_and_oversize_is_omitted() {
    let mut w = Work::default();
    let mut main = started("root", "1");
    main["subagent_type"] = json!("main-session");
    w.event(&main);
    assert!(w.tasks["root"].child.is_none());
    let mut a = started("a", "1");
    a["description"] = json!("x".repeat(2049));
    w.event(&a);
    assert!(w.tasks["a"].value.get("title").is_none());
    assert_eq!(w.tasks["a"].value["incomplete"], true);
    let (_, bg) = w.level(&json!({"tasks":[{"task_id":"a","run_id":"r".repeat(129)}]}));
    assert!(bg.unwrap()[0]["native"].get("run_id").is_none());
}
#[test]
fn unknown_parent_is_not_main_and_cycles_are_not_projected() {
    let mut w = Work::default();
    let mut a = started("a", "1");
    a["parent_task_id"] = json!("missing");
    w.event(&a);
    assert!(w.tasks["a"].child_value.get("parent").is_none());
    assert_eq!(w.tasks["a"].child_value["incomplete"], true);
    let mut b = started("b", "1");
    b["parent_task_id"] = json!("a");
    w.event(&b);
    a["parent_task_id"] = json!("b");
    w.event(&a);
    assert!(w.tasks["a"].value.get("parent_task").is_none());
}
