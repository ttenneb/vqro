//! Core namespace casing cannot bypass caller-assertion reservations or owner configuration.
use serde_json::json;
use vqro::{client,server::{self,Backend}};
#[tokio::test]
async fn reserved_names_are_ascii_case_insensitive_but_ordinary_identity_is_not_normalized() {
    let root=tempfile::Builder::new().prefix("case-p").tempdir().unwrap();let socket=root.path().join("s");
    let core=server::start(socket.clone(),root.path().join("st"),false,Backend::Fake,None).unwrap();
    let(owner,_,_)=client::connect_as(&socket,"owner","owner").await.unwrap();
    for principal in ["SYSTEM","System:future","ExTeNsIoN:writer","HOST:rid","UnDeClArEd","OwNeR-UnSeT"] {
        let(raw,_,_)=client::connect(&socket,"case-negative").await.unwrap();
        assert_eq!(raw.call("hello",json!({"identity":{"kind":"principal","principal":principal}})).await.unwrap_err().kind(),"invalid","{principal}");
        assert_eq!(owner.call("workspace.apply",json!({"ops":[{"op":"set","key":"doc/principals/owner","value":principal}]})).await.unwrap_err().kind(),"invalid","{principal}");
    }
    for principal in ["Alice","alice","Systematic","hosted:p","EXTENSIONLESS","p:HOST"] {
        let(_,_,hello)=client::connect_as(&socket,"case-positive",principal).await.unwrap();assert_eq!(hello["actor"],principal);
    }
    server::shutdown(&core).await;
}
