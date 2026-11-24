use super::*;
#[test]
fn todo_uses_whole_successful_output_not_input_and_never_fabricates_item_ids() {
    let mut p = Plans::default();
    assert!(
        p.observe(
            "TodoWrite",
            "tool",
            None,
            &json!({"todos":[{"content":"intent"}]}),
            false
        )
        .is_empty()
    );
    let c = p.observe(
        "TodoWrite",
        "tool",
        None,
        &json!({"newTodos":[{"content":"actual","status":"pending","activeForm":"doing"}]}),
        false,
    );
    let v = &c[0].value;
    assert_eq!(v["items"][0]["text"], "actual");
    assert!(v["items"][0].get("id").is_none());
    assert_eq!(v["update"], "whole");
    assert_eq!(v["incomplete"], false);
    let c = p.observe("TodoWrite", "tool", None, &json!({"newTodos":[]}), false);
    assert!(c[0].value["items"].as_array().unwrap().is_empty());
    assert_eq!(c[0].value["incomplete"], false);
}
#[test]
fn native_tracking_lists_are_plans_and_partial_mutations_are_not_input_echo() {
    let mut p = Plans::default();
    let list = json!({"tasks":[{"id":"1","subject":"actual saved","status":"pending","owner":"native-agent-name","blockedBy":[]}]});
    let first = p.observe("TaskList", "tool1", None, &list, false);
    let id = first[0].id.clone();
    assert!(first[0].value.get("owner").is_none());
    assert_eq!(
        first[0].value["items"][0]["native"]["owner"],
        "native-agent-name"
    );
    let partial = p.observe(
        "TaskUpdate",
        "tool2",
        None,
        &json!({"success":true,"taskId":"1","updatedFields":["status","subject"]}),
        false,
    );
    assert_eq!(partial[0].id, id);
    assert_eq!(partial[0].value["items"][0]["status"], "pending");
    assert_eq!(partial[0].value["items"][0]["text"], "actual saved");
    assert_eq!(partial[0].value["incomplete"], true);
    let actual = p.observe(
        "TaskUpdate",
        "tool3",
        None,
        &json!({"success":true,"taskId":"1","statusChange":{"from":"pending","to":"completed"}}),
        false,
    );
    assert_eq!(actual[0].value["items"][0]["status"], "completed");
    assert_eq!(actual[0].value["incomplete"], true);
    let full = p.observe("TaskList", "tool4", None, &list, false);
    assert_eq!(full[0].value["incomplete"], false);
}
#[test]
fn partial_reads_and_unknown_item_changes_never_prove_inventory_or_guess_a_delete() {
    let mut p = Plans::default();
    let c = p.observe(
        "TaskCreate",
        "tool",
        None,
        &json!({"task":{"id":"1","subject":"saved not input","description":"native description"}}),
        false,
    );
    assert_eq!(c[0].value["incomplete"], true);
    assert_eq!(c[0].value["items"][0]["status"], "unknown");
    let c = p.observe("TaskGet", "tool", None, &json!({"task":null}), false);
    assert_eq!(c[0].value["items"].as_array().unwrap().len(), 1);
    let c = p.observe(
        "TaskUpdate",
        "tool",
        None,
        &json!({"success":true,"taskId":"missing","statusChange":{"to":"completed"}}),
        false,
    );
    assert_eq!(c[0].value["items"].as_array().unwrap().len(), 1);
    assert_eq!(c[0].value["incomplete"], true);
}
#[test]
fn producer_context_is_not_invented_list_ownership() {
    let mut p = Plans::default();
    let v = json!({"tasks":[]});
    let a = p.observe("TaskList", "root-tool", None, &v, false);
    let b = p.observe("TaskList", "child-tool", Some("child1"), &v, false);
    assert_ne!(a[0].id, b[0].id);
    assert!(b[0].value.get("owner").is_none());
    assert_eq!(b[0].value["native"]["observer_child"], "child1");
}
#[test]
fn oversized_values_are_omitted_not_shortened_and_goal_clear_is_not_completion() {
    let mut p = Plans::default();
    let c = p.observe(
        "TodoWrite",
        "tool",
        None,
        &json!({"newTodos":[{"content":"x".repeat(2049),"status":"pending"}]}),
        false,
    );
    assert!(c[0].value["items"].as_array().unwrap().is_empty());
    assert_eq!(c[0].value["incomplete"], true);
    let c=p.goal(&json!({"value":{"condition":"native objective","iterations":2,"set_at":100,"tokens_at_start":50}}));
    assert_eq!(c[0].value["status"], "active");
    assert!(c[0].value.get("budget").is_none());
    assert!(c[0].value.get("used").is_none());
    let c = p.goal(&json!({"value":null}));
    assert_eq!(c[0].value["status"], "unknown");
    assert!(c[0].value.get("objective").is_none());
    assert_eq!(c[0].value["native"]["cleared"], true);
}
