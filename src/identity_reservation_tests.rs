use super::*;
fn identity(n:usize)->Identity {Identity {provider:"runtime".into(),instance:"host".into(),reference:format!("endpoint{n}"),incarnation:"session".into()}}
#[test]
fn cached_retirement_reuses_only_the_exact_identity_and_native_retirement_does_not() {
 let original=identity(0);
 for field in 0..4 {let mut map=IdentityMap::open(None).unwrap();let id=map.identify(original.clone()).unwrap().id;map.forget_cached(&original).unwrap();assert!(!map.active(&id));assert!(map.lookup(&original).is_none());
  let mut other=original.clone();match field {0=>other.provider.push('x'),1=>other.instance.push('x'),2=>other.reference.push('x'),_=>other.incarnation.push('x')};assert_ne!(map.identify(other).unwrap().id,id);
  let returned=map.identify(original.clone()).unwrap().id;if field==1||field==3 {assert_ne!(returned,id,"same reference with changed host/lineage permanently replaces the hint");}else{assert_eq!(returned,id);}
 }
 let mut map=IdentityMap::open(None).unwrap();let id=map.identify(original.clone()).unwrap().id;map.forget_cached(&original).unwrap();assert_eq!(map.identify(original.clone()).unwrap().id,id);
 map.retire(|i|i==&original).unwrap();assert_ne!(map.identify(original).unwrap().id,id);
}
#[test]
fn oldest_hint_evicts_at_the_count_bound_and_order_survives_restart() {
 let mut map=IdentityMap::open(None).unwrap();let mut ids=Vec::new();
 for n in 0..IDENTITY_RESERVATIONS+4 {let i=identity(n);ids.push(map.identify(i.clone()).unwrap().id);map.forget_cached(&i).unwrap();}
 assert_eq!(map.state.reservations.identities.len(),IDENTITY_RESERVATIONS);assert_eq!(map.state.reservations.oldest.len(),IDENTITY_RESERVATIONS);assert!(map.state.reservations.bytes<=IDENTITY_RESERVATIONS*RESERVATION_BYTES);
 let root=tempfile::tempdir().unwrap();let path=root.path().join("resources.json");map.path=Some(path.clone());map.commit(map.state.clone()).unwrap();drop(map);
 let mut map=IdentityMap::open(Some(path)).unwrap();assert_eq!(map.state.reservations.oldest.values().next().unwrap().as_ref(),&identity(4));
 assert_ne!(map.identify(identity(0)).unwrap().id,ids[0]);assert_eq!(map.identify(identity(IDENTITY_RESERVATIONS+3)).unwrap().id,*ids.last().unwrap());
}
#[test]
fn hints_survive_restart_and_oversized_hints_never_refuse_registration() {
 let root=tempfile::tempdir().unwrap();let path=root.path().join("resources.json");let i=identity(0);
 let old={let mut map=IdentityMap::open(Some(path.clone())).unwrap();let id=map.identify(i.clone()).unwrap().id;map.forget_cached(&i).unwrap();id};
 let file:Value=serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();assert_eq!(file["reservations"].as_array().unwrap().len(),1);
 let mut map=IdentityMap::open(Some(path.clone())).unwrap();assert_eq!(map.state.reservations.identities.len(),1);assert_eq!(map.identify(i).unwrap().id,old);
 let mut huge=identity(1);huge.reference="x".repeat(4096);let id=map.identify(huge.clone()).unwrap().id;map.forget_cached(&huge).unwrap();assert_eq!(map.state.reservations.identities.len(),0);assert_ne!(map.identify(huge).unwrap().id,id);
}
#[test]
fn clean_and_uncertain_writes_keep_durable_reservations_aligned_with_actual_adoption() {
 for renamed in [false,true] {
  let root=tempfile::tempdir().unwrap();let path=root.path().join("resources.json");let mut map=IdentityMap::open(Some(path.clone())).unwrap();let i=identity(0);let id=map.identify(i.clone()).unwrap().id;
  let fault:crate::store::DurableWriter=std::sync::Arc::new(move|path,bytes|{if renamed {crate::store::write_atomic(path,bytes)?;}let mut error:crate::store::AtomicWriteError=std::io::Error::other("injected identity write").into();error.renamed=renamed;Err(error)});
  map.set_writer(fault.clone());let retired=map.forget_cached(&i);
  if renamed {assert_eq!(retired.unwrap().1.unwrap().kind(),"durability_unconfirmed");assert!(!map.active(&id));assert_eq!(IdentityMap::open(Some(path.clone())).unwrap().state.reservations.identities.len(),1);}else{assert!(retired.is_err());assert!(map.active(&id));assert_eq!(IdentityMap::open(Some(path.clone())).unwrap().state.reservations.identities.len(),0);map.set_writer(std::sync::Arc::new(crate::store::write_atomic));map.forget_cached(&i).unwrap();}
  map.set_writer(fault);let returned=map.identify(i.clone());
  if renamed {let returned=returned.unwrap();assert_eq!(returned.id,id);assert!(returned.uncertain.is_some());assert!(map.active(&id));assert_eq!(IdentityMap::open(Some(path.clone())).unwrap().state.reservations.identities.len(),0);}else{assert!(returned.is_err());assert!(!map.active(&id));assert_eq!(map.state.reservations.identities.len(),1);assert_eq!(IdentityMap::open(Some(path.clone())).unwrap().state.reservations.identities.len(),1);}
  map.set_writer(std::sync::Arc::new(crate::store::write_atomic));assert_eq!(map.identify(i).unwrap().id,id);assert_eq!(map.state.reservations.identities.len(),0);
 }
}
#[test]
fn legacy_maps_load_without_reservations_and_unsafe_cache_rows_fail_closed() {
 let root=tempfile::tempdir().unwrap();let path=root.path().join("resources.json");let i=identity(0);
 std::fs::write(&path,serde_json::json!({"next":2,"ids":{"r1":{"identity":i,"retired":false}}}).to_string()).unwrap();let map=IdentityMap::open(Some(path.clone())).unwrap();assert!(map.active("r1"));assert_eq!(map.state.reservations.identities.len(),0);
 for rows in [serde_json::json!([{"identity":i,"id":"r1"}]),serde_json::json!([{"identity":i,"id":"r3"},{"identity":i,"id":"r4"}]),serde_json::json!([{"identity":i,"id":"not-an-id"}])] {
  std::fs::write(&path,serde_json::json!({"next":2,"ids":{"r1":{"identity":i,"retired":false}},"reservations":rows}).to_string()).unwrap();assert!(IdentityMap::open(Some(path.clone())).is_err());
 }
}
