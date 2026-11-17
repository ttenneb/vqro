//! D24 attribution over public RPC. Every core/state/process belongs to this private fixture.
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use prim::{client, rpc::{Incoming, IncomingReceiver, Peer}};
use serde_json::{json, Value};

struct Fixture { child:Option<Child>, root:tempfile::TempDir }
impl Fixture {
    async fn new() -> Self {
        let root = tempfile::Builder::new().prefix("pundo").tempdir_in("/tmp").unwrap();
        std::fs::create_dir(root.path().join("extensions")).unwrap();
        let mut fixture = Self { child:None, root };
        fixture.start();
        fixture.peer("ready").await;
        fixture
    }
    fn start(&mut self) {
        let binary = std::env::var_os("UNDO_CORE_BIN").map(PathBuf::from).unwrap_or_else(|| env!("CARGO_BIN_EXE_prim").into());
        self.child = Some(Command::new(binary).env_clear().args(["serve","--fake"])
            .env("HOME",self.root.path()).env("PATH","/usr/bin:/bin")
            .env("PRIM_STATE_DIR",self.root.path().join("st"))
            .env("PRIM_SOCKET",self.root.path().join("s"))
            .env("PRIM_EXTENSIONS_DIR",self.root.path().join("extensions"))
            .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap());
    }
    async fn peer(&self, name:&str) -> (Peer,IncomingReceiver,Value) {
        let deadline = Instant::now()+Duration::from_secs(5);
        loop {
            if let Ok(result) = client::connect(&self.root.path().join("s"),name).await { return result; }
            assert!(Instant::now()<deadline,"private core startup");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
    async fn extension(&self, name:&str) -> (Peer,IncomingReceiver) {
        let (peer,rx,_) = self.peer("extension").await;
        peer.call("hello",json!({"extension":name})).await.unwrap();
        (peer,rx)
    }
    async fn stop(&mut self, peer:&Peer) {
        peer.call("core.shutdown",json!({})).await.unwrap();
        let mut child = self.child.take().unwrap();
        let deadline = Instant::now()+Duration::from_secs(6);
        while child.try_wait().unwrap().is_none() {
            if Instant::now()>deadline { let _=child.kill(); let _=child.wait(); panic!("core shutdown"); }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) { if let Some(child)=&mut self.child { let _=child.kill(); let _=child.wait(); } }
}
async fn request(rx:&mut IncomingReceiver) -> (Value,Value) {
    tokio::time::timeout(Duration::from_secs(2),async {
        loop {
            if let Some(Incoming::Request {id,method,params})=rx.recv().await {
                assert_eq!(method,"command.invoke"); return (id,params);
            }
        }
    }).await.unwrap()
}
async fn set(peer:&Peer,key:&str,value:Value,proof:Option<&str>) -> Value {
    let mut params=json!({"ops":[{"op":"set","key":key,"value":value}]});
    if let Some(proof)=proof { params["on_behalf_of"]=json!(proof); }
    peer.call("workspace.apply",params).await.unwrap()
}
async fn register(peer:&Peer,name:&str) { peer.call("command.register",json!({"name":name})).await.unwrap(); }

#[tokio::test]
async fn client_started_theme_is_undone_globally_but_autonomous_writes_are_not() {
    let mut fixture=Fixture::new().await;
    let (user,_,hello)=fixture.peer("tui").await;
    let (extension,mut commands)=fixture.extension("themes").await;
    set(&extension,"doc/settings/theme",json!({"name":"old"}),None).await;
    register(&extension,"themes.apply").await;
    let caller=user.clone();
    let invocation=tokio::spawn(async move { caller.call("command.invoke",json!({"name":"themes.apply"})).await });
    let (id,params)=request(&mut commands).await;
    let changed=set(&extension,"doc/settings/theme",json!({"name":"new"}),params["invocation"].as_str()).await;
    extension.respond(id,Ok(json!({"ok":true})));
    invocation.await.unwrap().unwrap();
    set(&extension,"doc/autonomous",json!(1),None).await;
    let (other_window,_,_)=fixture.peer("other-window").await;
    // Same command as TUI u; preserve existing global behavior across clients/reconnects.
    let undone=other_window.call("command.invoke",json!({"name":"prim.revert"})).await.unwrap();
    let snapshot=user.call("workspace.snapshot",json!({})).await.unwrap();
    assert_eq!(snapshot["entries"]["doc/settings/theme"]["value"]["name"],"old");
    assert_eq!(snapshot["entries"]["doc/autonomous"]["value"],1);
    let history=user.call("workspace.history",json!({"limit":100})).await.unwrap();
    assert_eq!(history["txns"].as_array().unwrap().iter().find(|txn|txn["id"]==undone["txn"]).unwrap()["reverts"],changed["txn"]);
    let transaction=history["txns"].as_array().unwrap().iter().find(|txn|txn["id"]==changed["txn"]).unwrap();
    assert_eq!(transaction["author"],"ext:themes");
    assert_eq!(transaction["on_behalf_of"],hello["you"]);
    fixture.stop(&user).await;
    fixture.start();
    let (user,_,_)=fixture.peer("restarted").await;
    let history=user.call("workspace.history",json!({"limit":100})).await.unwrap();
    assert_eq!(history["txns"].as_array().unwrap().iter().find(|txn|txn["id"]==changed["txn"]).unwrap()["on_behalf_of"],hello["you"]);
}

#[tokio::test]
async fn forged_other_connection_and_completed_invocation_proofs_are_rejected() {
    let fixture=Fixture::new().await;
    let (user,_,_)=fixture.peer("user").await;
    let (extension,mut commands)=fixture.extension("writer").await;
    let (other,_) =fixture.extension("other").await;
    register(&extension,"writer.run").await;
    let caller=user.clone();
    let invocation=tokio::spawn(async move { caller.call("command.invoke",json!({"name":"writer.run"})).await });
    let (id,params)=request(&mut commands).await;
    let proof=params["invocation"].as_str().unwrap();
    for (peer,proof) in [(&other,proof),(&extension,"user:1"),(&extension,"00000000000000000000000000000000")] {
        let error=peer.call("workspace.apply",json!({"on_behalf_of":proof,"ops":[{"op":"set","key":"doc/forged","value":1}]})).await.unwrap_err();
        assert_eq!(error.kind(),"forbidden");
    }
    extension.respond(id,Ok(json!({})));
    invocation.await.unwrap().unwrap();
    assert_eq!(extension.call("workspace.apply",json!({"on_behalf_of":proof,"ops":[{"op":"set","key":"doc/late","value":1}]})).await.unwrap_err().kind(),"forbidden");
    let snapshot=user.call("workspace.snapshot",json!({})).await.unwrap();
    assert!(snapshot["entries"].get("doc/forged").is_none());
    assert!(snapshot["entries"].get("doc/late").is_none());
}

#[tokio::test]
async fn nested_invocation_retains_origin_and_is_revoked_when_parent_replies() {
    let fixture=Fixture::new().await;
    let (user,_,hello)=fixture.peer("agent").await;
    let (outer,mut outer_commands)=fixture.extension("outer").await;
    let (inner,mut inner_commands)=fixture.extension("inner").await;
    register(&outer,"outer.run").await; register(&inner,"inner.run").await;
    let caller=user.clone();
    let invocation=tokio::spawn(async move { caller.call("command.invoke",json!({"name":"outer.run"})).await });
    let (outer_id,outer_params)=request(&mut outer_commands).await;
    let caller=outer.clone(); let proof=outer_params["invocation"].clone();
    let nested=tokio::spawn(async move { caller.call("command.invoke",json!({"name":"inner.run","on_behalf_of":proof})).await });
    let (inner_id,inner_params)=request(&mut inner_commands).await;
    let changed=set(&inner,"doc/nested",json!(1),inner_params["invocation"].as_str()).await;
    assert_eq!(inner_params["on_behalf_of"],hello["you"]);
    outer.respond(outer_id,Ok(json!({})));
    invocation.await.unwrap().unwrap();
    assert_eq!(inner.call("workspace.apply",json!({"on_behalf_of":inner_params["invocation"],"ops":[{"op":"set","key":"doc/late-child","value":1}]})).await.unwrap_err().kind(),"forbidden");
    inner.respond(inner_id,Ok(json!({})));
    nested.await.unwrap().unwrap();
    let history=user.call("workspace.history",json!({})).await.unwrap();
    assert_eq!(history["txns"].as_array().unwrap().iter().find(|txn|txn["id"]==changed["txn"]).unwrap()["on_behalf_of"],hello["you"]);
}
