//! Retired managed connections must not write during their process cleanup grace.
use std::{process::{Child,Command,Stdio},time::{Duration,Instant}};
use serde_json::json;
use vqro::{client,rpc::{Peer,IncomingReceiver}};
#[path="support/managed.rs"] mod managed;
struct Fixture { child:Child, root:tempfile::TempDir }
impl Drop for Fixture { fn drop(&mut self){let _=self.child.kill();let _=self.child.wait();} }
impl Fixture {
    async fn new()->Self {
        let root=tempfile::Builder::new().prefix("extlife").tempdir().unwrap();
        for dir in ["state","extensions","run"] { std::fs::create_dir(root.path().join(dir)).unwrap(); }
        let mut store=vqro::store::Store::open(root.path().join("state/workspace.json")).unwrap();
        store.apply("fixture",None,["ordinary","provider"].into_iter().map(|name|vqro::store::Op::Set {
            key:format!("extension/{name}"),value:managed::spec(&root.path().join(format!("{name}.token"))),expect:None }).collect()).unwrap();
        drop(store);
        let child=Command::new(env!("CARGO_BIN_EXE_vqro")).env_clear().args(["serve","--fake"])
            .env("HOME",root.path()).env("PATH","/usr/bin:/bin")
            .env("VQRO_STATE_DIR",root.path().join("state")).env("VQRO_SOCKET",root.path().join("s"))
            .env("VQRO_EXTENSIONS_DIR",root.path().join("extensions"))
            .env("XDG_RUNTIME_DIR",root.path().join("run")).env("HC_SOCKET",root.path().join("hc"))
            .env("HC_SESSION","extension-lifetime").env("VQRO_HERDR",r#"["/bin/false"]"#)
            .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
        Self {child,root}
    }
    async fn peer(&self)->(Peer,IncomingReceiver) {
        let until=Instant::now()+Duration::from_secs(5);
        loop {if let Ok((peer,rx,_))=client::connect(&self.root.path().join("s"),"fixture").await {return(peer,rx)}
            assert!(Instant::now()<until);tokio::time::sleep(Duration::from_millis(10)).await;}
    }
    async fn extension(&self,name:&str)->(Peer,IncomingReceiver) {
        let (peer,rx)=self.peer().await;
        let token=managed::token(&self.root.path().join(format!("{name}.token"))).await;
        peer.call("hello",json!({"extension":name,"extension_token":token})).await.unwrap();(peer,rx)
    }
}
#[tokio::test]
async fn retired_connections_cannot_mutate_even_when_author_name_is_reused() {
    let fixture=Fixture::new().await;
    let (control,_)=fixture.peer().await;
    let mut accepted=Vec::new();
    for name in ["ordinary","provider"] {
        let (old,_old_rx)=fixture.extension(name).await;
        let generation=if name=="provider" {
            let r=old.call("provider.register",json!({"name":"managed-provider","role":"backend","instance":"fixture",
                "software":{"name":"fixture","version":"0"},"provider_version":"0","capabilities":{},
                "limits":{"max_streams":1,"stream_window_bytes":1024}})).await.unwrap();
            Some(r["generation"].clone())
        } else {None};
        let mut initial=json!({"name":format!("{name}.handler")});
        if let Some(generation)=&generation {initial["generation"]=generation.clone();}
        old.call("command.register",initial).await.unwrap();
        control.call("command.invoke",json!({"name":"vqro.extension.disable","args":{"name":name}})).await.unwrap();
        let calls=[("workspace.apply",json!({"ops":[{"op":"set","key":format!("doc/stale-{name}"),"value":1}]})),
            ("command.register",json!({"name":format!("{name}.stale")})),
            ("view.publish",json!({"view":format!("stale-{name}"),"content":{"items":[]}})),
            ("ui.notify",json!({"text":"stale notification"})),
            ("provider.sync",json!({"resources":[],"final":true}))];
        for (method,mut params) in calls {
            if method=="provider.sync" && generation.is_none(){continue;}
            if let Some(generation)=&generation {params["generation"]=generation.clone();}
            if old.call(method,params).await.is_ok(){accepted.push(format!("{name}:{method}"));}
        }
        let result=control.call("command.invoke",json!({"name":format!("{name}.handler"),"deadline_ms":50})).await;
        assert!(result.is_err());
        if result.unwrap_err().kind()=="timeout" {accepted.push(format!("{name}:stale handler dispatched"));}
        // New lifetime has the same author string, but must not revive the old connection.
        std::fs::remove_file(fixture.root.path().join(format!("{name}.token"))).unwrap();
        control.call("command.invoke",json!({"name":"vqro.extension.enable","args":{"name":name}})).await.unwrap();
        let (new,_new_rx)=fixture.extension(name).await;
        new.call("command.register",json!({"name":format!("{name}.handler")})).await.unwrap();
        if old.call("workspace.apply",json!({"ops":[{"op":"set","key":"doc/revived","value":1}]})).await.is_ok(){accepted.push(format!("{name}:revived"));}
    }
    control.call("core.shutdown",json!({})).await.unwrap();
    assert!(accepted.is_empty(),"retired operations accepted: {accepted:?}");
}
