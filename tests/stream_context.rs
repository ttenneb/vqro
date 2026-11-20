//! Full caller context and explicit links reach native stream.open without inferring callback causes.
use serde_json::json;
use tokio::sync::oneshot;
use vqro::{client,provider_client::{Config,Provider,Request,Initial,StreamCommand},server::{self,Backend}};
#[tokio::test]
async fn attach_forwards_actual_actor_effective_initiator_label_and_explicit_links() {context(false).await;}
#[tokio::test]
async fn item_attach_preserves_the_same_full_context_without_inference() {context(true).await;}
async fn context(items:bool) {
    let root=tempfile::Builder::new().prefix("stream-c").tempdir().unwrap();let socket=root.path().join("s");
    let core=server::start(socket.clone(),root.path().join("st"),false,Backend::Fake,None).unwrap();
    let config=Config::new("stream-probe","fixed",json!({"terminal.output":{"raw":true},"items.stream":{}}).as_object().unwrap().clone());
    let(provider,mut requests)=Provider::connect(tokio::net::UnixStream::connect(&socket).await.unwrap(),config).await.unwrap();
    provider.sync(vec![json!({"ref":"terminal","incarnation":"one","kind":"terminal","state":"live","argv":[]})],true).await.unwrap();
    let(owner,_,_)=client::connect_as(&socket,"owner","owner").await.unwrap();
    owner.call("workspace.apply",json!({"ops":[{"op":"set","key":"doc/principals/delegations/b-a","value":{"from":"bob","to":"alice"}}]})).await.unwrap();
    let(alice,_,hello)=client::connect_as(&socket,"initiator","alice").await.unwrap();
    let resource=alice.call("resource.list",json!({"provider":"stream-probe"})).await.unwrap()["resources"][0]["id"].clone();
    let(tx,rx)=oneshot::channel();
    let worker=tokio::spawn(async move {
        let Some(Request::Open(open))=requests.recv().await else{panic!("stream open")};
        let captured=json!({"author":open.author,"actor":open.attribution.actor,"principal":open.attribution.principal,"initiator":open.attribution.initiator,"label":open.attribution.label,"cause":open.cause(),"correlation":open.correlation()});
        let mut stream=if items{open.respond_items(true,"item-epoch").unwrap()}else{open.respond(Ok(Initial::None)).unwrap()};tx.send(captured).unwrap();
        while let Some(command)=stream.next().await {if matches!(command,StreamCommand::Close){break}}
    });
    let before=owner.call("workspace.history",json!({"limit":100})).await.unwrap();
    let cause=json!({"kind":"request","id":"old-local","endpoint":"previous-endpoint","epoch":"previous-epoch","provenance":"asserted"});
    let mut params=json!({"resource":resource,"on_behalf_of":"bob","label":"display","cause":cause,"correlation":"stream-thread"});
    if items{params["items"]=json!({});}else{params["cols"]=json!(80);params["rows"]=json!(24);}
    let attached=alice.call("resource.attach",params).await.unwrap();
    let captured=rx.await.unwrap();
    alice.call("stream.close",json!({"stream":attached["stream"]})).await.unwrap();
    worker.await.unwrap();
    let after=owner.call("workspace.history",json!({"limit":100})).await.unwrap();
    server::shutdown(&core).await;drop(provider);
    assert_eq!(captured,json!({"author":hello["actual_author"],"actor":"alice","principal":"bob","initiator":"alice","label":"display","cause":cause,"correlation":"stream-thread"}));
    assert_eq!(after,before,"stream context is not a workspace transaction");
}
