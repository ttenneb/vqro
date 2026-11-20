use super::*;
fn identity()->Identity {Identity{provider:"runtime".into(),instance:"host".into(),reference:"endpoint".into(),incarnation:"session".into()}}
#[test]
fn invalid_cache_keeps_all_readable_ids_and_durable_recovery_notice() {
 let root=tempfile::tempdir().unwrap();let path=root.path().join("resources.json");
 for cache in [serde_json::json!([{"id":"r90000"},{"id":"r7000","identity":null}]),serde_json::json!({"id":"r90000"}),serde_json::json!(["r90000"])] {
  let original=serde_json::json!({"next":17,"ids":{},"reservations":cache}).to_string();std::fs::write(&path,&original).unwrap();
  let mut map=IdentityMap::open(Some(path.clone())).unwrap();assert_eq!(std::fs::read_to_string(&path).unwrap(),original,"startup must not rewrite source");assert_eq!(map.state.reservations.identities.len(),0);assert!(map.reservation_stats()["recovery_notice"].is_string());
  assert_eq!(map.identify(identity()).unwrap().id,"r90001");let restored=IdentityMap::open(Some(path.clone())).unwrap();assert!(restored.reservation_stats()["recovery_notice"].is_string());assert_eq!(restored.lookup(&identity()),Some("r90001"));
 }
}
#[test]
fn malformed_cache_does_not_block_safe_mode_core_recovery() {
 let root=tempfile::tempdir().unwrap();std::fs::write(root.path().join("resources.json"),serde_json::json!({"next":42,"ids":{},"reservations":[{"identity":identity(),"id":"invalid"}]}).to_string()).unwrap();
 let store=crate::store::Store::open_locked(root.path().join("workspace.json")).unwrap();let core=crate::core::Core::new(store,Some(root.path().into()),root.path().join("p"),true).unwrap();let snapshot=core.capture_snapshot().into_value();assert_eq!(snapshot["safe_mode"],true);assert!(snapshot["identity_reservations"]["recovery_notice"].is_string());assert_eq!(snapshot["identity_reservations"]["persistent"],true);
}
#[test]
fn memory_and_configured_persistence_are_reported_honestly() {
 let mut memory=IdentityMap::open(None).unwrap();let i=identity();memory.identify(i.clone()).unwrap();memory.forget_cached(&i).unwrap();assert_eq!(memory.reservation_stats()["persistent"],false);assert_eq!(memory.reservation_stats()["durability_confirmed"],false);
 let root=tempfile::tempdir().unwrap();let path=root.path().join("resources.json");let mut disk=IdentityMap::open(Some(path.clone())).unwrap();assert_eq!(disk.reservation_stats()["persistent"],true);assert_eq!(disk.reservation_stats()["durability_confirmed"],false);disk.identify(i).unwrap();assert_eq!(disk.reservation_stats()["durability_confirmed"],true);
 std::fs::write(&path,b"{ corrupt whole file").unwrap();assert!(IdentityMap::open(Some(path)).is_err(),"whole identity-file syntax failure remains strict");
}
#[test]
fn unreadable_row_identity_and_overflow_cannot_reuse_a_readable_id() {
 let root=tempfile::tempdir().unwrap();let path=root.path().join("resources.json");
 let huge="[".repeat(150)+"0"+&"]".repeat(150);let bytes=format!("{{\"next\":1,\"ids\":{{}},\"reservations\":[{{\"identity\":{huge},\"id\":\"r999\"}}]}}");std::fs::write(&path,bytes).unwrap();let mut map=IdentityMap::open(Some(path.clone())).unwrap();assert_eq!(map.identify(identity()).unwrap().id,"r1000");
 std::fs::write(&path,serde_json::json!({"next":1,"ids":{},"reservations":[{"id":format!("r{}",u64::MAX)}]}).to_string()).unwrap();let mut map=IdentityMap::open(Some(path)).unwrap();assert!(map.reservation_stats()["recovery_notice"].is_string());assert!(map.identify(identity()).is_err(),"unreadable high-water must never wrap into reusable ids");
}
