#[path="../src/bin/vqro-bridge-pi/queue.rs"] mod queue;
use serde_json::{json,Value};
#[test]
fn native_queue_previews_are_bounded_and_have_no_invented_ids() {
    let result=queue::summary(&json!({"steering":["雪".repeat(200)],"followUp":["next"]})).unwrap();
    assert!(result[0]["preview"].as_str().unwrap().len()<=256);
    assert!(result[0]["id"].is_null());assert_eq!(result[0]["mode"],"boundary");assert_eq!(result[1]["mode"],"settle");
    assert!(queue::summary(&json!({"steering":vec!["x";33],"followUp":[]})).is_none());
    assert!(queue::summary(&json!({"steering":[{"unknown":"native shape"}],"followUp":[]})).is_none());
}
#[test]
fn clear_preserves_returned_text_without_inventing_media_or_origin() {
    let result=queue::removed(&json!({"steering":["/literal\nexact"],"followUp":["second"]}));
    assert_eq!(result["removed"][0]["content"],json!([{"type":"text","text":"/literal\nexact"}]));
    assert_eq!(result["removed"][1]["mode"],"settle");
    assert_eq!(result["incomplete"],json!(["removed.media"]));
    assert!(result["removed"][0].get("delivery").is_none());
}
#[test]
fn oversized_or_unknown_clear_results_are_unknown_not_empty() {
    for value in [json!({"steering":vec!["x";65],"followUp":[]}),json!({"steering":["x".repeat(50000)],"followUp":[]}),Value::Null] {
        let result=queue::removed(&value);assert!(result["removed"].is_null());assert!(result["incomplete"].as_array().is_some_and(|fields|!fields.is_empty()));
    }
}
