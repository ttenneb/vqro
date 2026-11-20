//! Malformed objects must not erase readable resource IDs through duplicate-field rejection.
use super::*;
fn old_identity()->Identity {Identity{provider:"old-runtime".into(),instance:"old-host".into(),reference:"old-endpoint".into(),incarnation:"old-lineage".into()}}
#[test]
fn duplicate_id_field_cannot_erase_a_readable_old_id() {
 let root=tempfile::tempdir().unwrap();let path=root.path().join("resources.json");let old=old_identity();let bytes=format!("{{\"next\":17,\"ids\":{{}},\"reservations\":[{{\"identity\":{},\"id\":\"r17\",\"id\":\"garbage\"}}]}}",serde_json::to_string(&old).unwrap());
 std::fs::write(&path,&bytes).unwrap();let mut map=IdentityMap::open(Some(path.clone())).unwrap();assert_eq!(std::fs::read_to_string(&path).unwrap(),bytes);assert!(map.reservation_stats()["recovery_notice"].is_string());let mut new=old;new.reference="different-endpoint".into();assert_ne!(map.identify(new).unwrap().id,"r17");
}
#[test]
fn every_duplicate_numeric_value_raises_the_recovery_high_water() {
 let root=tempfile::tempdir().unwrap();let path=root.path().join("resources.json");
 for(first,second)in[("r17","r90000"),("r90000","r17")] {
  let bytes=format!("{{\"next\":17,\"ids\":{{}},\"reservations\":[{{\"identity\":{},\"id\":\"{first}\",\"id\":\"{second}\"}}]}}",serde_json::to_string(&old_identity()).unwrap());std::fs::write(&path,bytes).unwrap();let mut map=IdentityMap::open(Some(path.clone())).unwrap();let mut new=old_identity();new.provider="new-runtime".into();assert!(map.identify(new).unwrap().id[1..].parse::<u64>().unwrap()>90000);
 }
}
#[test]
fn healthy_cache_does_not_treat_a_native_reference_as_an_allocation_id() {
 let root=tempfile::tempdir().unwrap();let path=root.path().join("resources.json");let mut map=IdentityMap::open(Some(path.clone())).unwrap();let mut old=old_identity();old.reference=format!("r{}",u64::MAX);let id=map.identify(old.clone()).unwrap().id;map.forget_cached(&old).unwrap();drop(map);
 let mut map=IdentityMap::open(Some(path)).unwrap();assert_eq!(map.reservation_stats()["fresh_allocation"],true);assert_eq!(map.identify(old).unwrap().id,id);assert!(map.identify(old_identity()).is_ok());
}
