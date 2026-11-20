//! Public provider reads must never consume the construction slots reserved for ordinary recovery.
use super::*;

fn connection(id:u64,peer:Peer,wire:crate::provider_wire::Connection)->Arc<Conn> {
 Arc::new(Conn {id,peer,author:Mutex::new(format!("client:{id}")),actual_author:format!("client:test:{id}"),principal:Mutex::new(None),extension:Mutex::new(None),supervised:Mutex::new(None),host_context:Mutex::new(None),streams:Mutex::new(HashMap::new()),next_stream:AtomicU64::new(1),subscribed:Mutex::new(false),requests:Arc::new(tokio::sync::Semaphore::new(crate::rpc::MAX_PENDING_CALLS)),control_requests:Arc::new(tokio::sync::Semaphore::new(crate::commit::CONTROL_REQUESTS)),mutations:Arc::new(tokio::sync::Semaphore::new(MUTATION_SLOTS)),wire})
}
#[tokio::test]
async fn provider_operational_list_and_snapshot_preserve_ordinary_recovery_pool() {
 let root=tempfile::tempdir().unwrap();let store=crate::store::Store::open_locked(root.path().join("workspace.json")).unwrap();
 let core=Arc::new(Mutex::new(Core::new(store,Some(root.path().into()),root.path().join("p"),false).unwrap()));
 {let mut c=core.lock().unwrap();let mut domain=c.operational.prepare("state","owner",None).unwrap();domain.register(crate::operational::Attribution {author:"ext:owner".into(),on_behalf_of:None,context:crate::principal::Attribution::default(),epoch:c.epoch.clone(),generation:1}).unwrap();c.operational.adopt(domain);}
 let(socket,_remote)=tokio::io::duplex(65536);let(reader,writer)=tokio::io::split(socket);let(peer,_incoming)=Peer::spawn(reader,writer);
 let wire=crate::provider_wire::Connection::new(17,peer.clone());wire.hello().unwrap();
 let registration=wire.register(&core,json!({"name":"pool-provider","role":"backend","instance":"pool-test","software":{"name":"pool-test","version":"0"},"provider_version":"0","capabilities":{},"limits":{"max_streams":1,"stream_window_bytes":65536}}),None).await.unwrap();
 let provider=connection(17,peer.clone(),wire);
 let ordinary=connection(18,peer.clone(),crate::provider_wire::Connection::new(18,peer.clone()));
 let(ordinary_slots,provider_slots)={let c=core.lock().unwrap();(c.snapshot_slots.clone(),c.provider_snapshot_slots.clone())};
 let held=ordinary_slots.clone().acquire_many_owned(2).await.unwrap();
 assert!(dispatch(&core,&provider,"workspace.snapshot",json!({"generation":registration["generation"]})).await.is_ok());
 for method in ["operational.list","operational.snapshot"] {
  let result=dispatch(&core,&provider,method,json!({"domain":"state","generation":registration["generation"]})).await;
  assert!(result.is_ok(),"{method} must route provider reads away from occupied ordinary slots: {result:?}");
 }
 drop(held);
 let held=provider_slots.acquire_many_owned(2).await.unwrap();
 for method in ["operational.list","operational.snapshot"] {
  assert_eq!(dispatch(&core,&provider,method,json!({"domain":"state","generation":registration["generation"]})).await.unwrap_err().kind(),"overloaded");
  assert_eq!(ordinary_slots.available_permits(),2,"provider wait must not hold an ordinary recovery slot");
  assert!(dispatch(&core,&ordinary,method,json!({"domain":"state"})).await.is_ok(),"ordinary recovery must remain available");
 }
 assert!(dispatch(&core,&ordinary,"workspace.snapshot",json!({})).await.is_ok());
 drop(held);peer.close();
}
