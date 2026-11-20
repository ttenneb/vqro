//! A legacy absent-version delete must not recreate protected authority, even in a mixed revert.
use serde_json::json;
use vqro::store::{Store,Op};
#[test]
fn legacy_absent_delete_revert_and_mixed_batches_cannot_resurrect_operational_keys() {
 let root=tempfile::tempdir().unwrap();let path=root.path().join("workspace.json");
 let original=json!({"rev":2,"next_txn":10,"entries":{"doc/sentinel":{"version":2,"value":"current"}},"history":[{"id":9,"author":"legacy","at_ms":0,"changes":[{"key":"doc/sentinel","before":{"version":1,"value":"old"},"after_version":2},{"key":"operational/state/message","before":{"version":1,"value":{"phase":"pending"}},"after_version":null}]}]});
 std::fs::write(&path,serde_json::to_vec(&original).unwrap()).unwrap();
 let bytes=std::fs::read(&path).unwrap();let mut store=Store::open(path.clone()).unwrap();let before=json!(store.entries());let history=json!(store.history(500));
 assert_eq!(store.revert("regression",9).unwrap_err().kind(),"forbidden");
 assert_eq!(json!(store.entries()),before);assert_eq!(json!(store.history(500)),history);
 for op in [Op::Set {key:"operational/state/message".into(),value:json!({"phase":"pending"}),expect:Some(0)},Op::Delete {key:"operational/state/message".into(),expect:Some(0)}] {
  assert_eq!(store.apply("regression",None,vec![Op::Set {key:"doc/new".into(),value:json!(1),expect:Some(0)},op]).unwrap_err().kind(),"forbidden");
  assert_eq!(json!(store.entries()),before);assert_eq!(json!(store.history(500)),history);
 }
 assert_eq!(std::fs::read(&path).unwrap(),bytes);
}
