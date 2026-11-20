//! Native end/absence/replacement must invalidate a previously cached-forget reservation.
use super::*;
use serde_json::json;
fn ident()->Identity {Identity {provider:"backend".into(),instance:"host".into(),reference:"endpoint".into(),incarnation:"lineage".into()}}
#[test]
fn explicit_native_retirement_invalidates_existing_reservation_across_restart() {
 let root=tempfile::tempdir().unwrap();let path=root.path().join("resources.json");let i=ident();
 let old={let mut map=IdentityMap::open(Some(path.clone())).unwrap();let old=map.identify(i.clone()).unwrap().id;map.forget_cached(&i).unwrap();assert_eq!(map.retire(|candidate|candidate==&i).unwrap().0,vec![old.clone()]);old};
 assert_ne!(IdentityMap::open(Some(path)).unwrap().identify(i).unwrap().id,old);
}
#[test]
fn core_native_end_after_cached_forget_cannot_revive_the_old_id() {
 use crate::{core::Core,adapter::{AdapterHandle,AdapterEvent},store::Store};
 let mut core=Core::new(Store::in_memory(),None,std::path::PathBuf::from("unused"),false).unwrap();let(tx,_rx)=tokio::sync::mpsc::unbounded_channel();
 core.install_provider(AdapterHandle{name:"backend".into(),session:None,context:None,capabilities:json!({}),cmds:tx},false,false).unwrap();core.on_adapter_event("backend",AdapterEvent::Registered(ProviderInfo{connection:1,instance:"host".into(),role:"runtime".into(),software:json!({"name":"fixture","version":"1"})}));
 let fact=||serde_json::from_value(json!({"backend_ref":"endpoint","incarnation":"lineage","kind":"agent","state":"live"})).unwrap();let old=core.upsert_resource("backend",fact()).unwrap();
 core.upsert_resource("backend",serde_json::from_value(json!({"backend_ref":"sibling","incarnation":"lineage","kind":"agent","state":"live"})).unwrap()).unwrap();
 core.on_adapter_event("backend",AdapterEvent::Connected(false,None));core.forget_cached_resource(&old).unwrap();
 core.on_adapter_event("backend",AdapterEvent::Removed{reference:"endpoint".into(),incarnation:"lineage".into(),reason:serde_json::from_value(json!({"reason":"exited"})).unwrap()});
 core.on_adapter_event("backend",AdapterEvent::Connected(true,None));assert_ne!(core.upsert_resource("backend",fact()).unwrap(),old);
}
#[test]
fn authoritative_absence_cannot_preserve_a_retired_reservation() {
 let mut map=IdentityMap::open(None).unwrap();let i=ident();let old=map.identify(i.clone()).unwrap().id;map.forget_cached(&i).unwrap();map.sync_provider("backend",&[]).unwrap();assert_ne!(map.identify(i).unwrap().id,old);
}
#[test]
fn replacement_lineage_cannot_preserve_the_old_reservation() {
 let mut map=IdentityMap::open(None).unwrap();let i=ident();let old=map.identify(i.clone()).unwrap().id;map.forget_cached(&i).unwrap();let mut replacement=i.clone();replacement.incarnation="new-lineage".into();
 assert_eq!(map.identify(replacement).unwrap().retired,vec![old.clone()]);assert_ne!(map.identify(i).unwrap().id,old);
}
#[test]
fn hint_only_native_retirement_obeys_clean_and_uncertain_adoption() {
 for renamed in [false,true] {
  let root=tempfile::tempdir().unwrap();let path=root.path().join("resources.json");let mut map=IdentityMap::open(Some(path.clone())).unwrap();let i=ident();let old=map.identify(i.clone()).unwrap().id;map.forget_cached(&i).unwrap();
  map.set_writer(std::sync::Arc::new(move|path,bytes|{if renamed{crate::store::write_atomic(path,bytes)?;}let mut error:crate::store::AtomicWriteError=std::io::Error::other("native retirement fault").into();error.renamed=renamed;Err(error)}));
  let retired=map.retire(|candidate|candidate==&i);if renamed{assert!(retired.unwrap().1.is_some());}else{assert!(retired.is_err());}
  let disk=IdentityMap::open(Some(path)).unwrap();assert_eq!(disk.state.reservations.get(&i).is_some(),!renamed);
  map.set_writer(std::sync::Arc::new(crate::store::write_atomic));let returned=map.identify(i).unwrap().id;if renamed{assert_ne!(returned,old);}else{assert_eq!(returned,old);}
 }
}
#[test]
fn provider_instance_replacement_retires_hints_even_with_no_active_ids() {
 let mut map=IdentityMap::open(None).unwrap();let i=ident();let old=map.identify(i.clone()).unwrap().id;map.forget_cached(&i).unwrap();
 assert_eq!(map.retire_provider_except("backend","new-host").unwrap().0,vec![old.clone()]);assert_ne!(map.identify(i).unwrap().id,old);
}
