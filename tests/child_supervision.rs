//! A generic CLI child (for example a git hook) must not inherit extension lifetime authority implicitly.
use serde_json::json;
use vqro::{server::{self,Backend},store::{Store,Op}};
#[path="support/managed.rs"] mod managed;
#[tokio::test(flavor="multi_thread",worker_threads=2)]
async fn generic_cli_child_is_raw_but_explicit_sdk_opt_in_is_verified() {
    let root=tempfile::Builder::new().prefix("ctx-ch").tempdir().unwrap();let state=root.path().join("st");std::fs::create_dir(&state).unwrap();
    let credential=root.path().join("token");let mut store=Store::open(state.join("workspace.json")).unwrap();
    store.apply("fixture",None,vec![
        Op::Set{key:"extension/writer".into(),value:managed::spec(&credential),expect:None},
        Op::Set{key:"doc/principals/owner".into(),value:json!("owner"),expect:None}]).unwrap();drop(store);
    let socket=root.path().join("s");let core=server::start(socket.clone(),state,false,Backend::Fake,None).unwrap();let token=managed::token(&credential).await;
    let home=root.path().join("home");std::fs::create_dir(&home).unwrap();
    let command=|binary:&std::path::Path|{let mut cmd=std::process::Command::new(binary);cmd.env_clear().env("HOME",&home).env("XDG_CONFIG_HOME",&home).env("XDG_DATA_HOME",&home).env("XDG_STATE_HOME",&home).env("PATH","/usr/bin:/bin").env("VQRO_SOCKET",&socket).env("VQRO_HERDR","[\"/bin/false\"]").env("VQRO_PRINCIPAL","undeclared").env("VQRO_EXTENSION","writer").env("VQRO_EXTENSION_TOKEN",&token);cmd};
    // The override only selects an immutable old CLI for a reproducible red run; normal gates use this build.
    let binary=std::env::var_os("VQRO_TEST_BASELINE_CLI").map(std::path::PathBuf::from).unwrap_or_else(||env!("CARGO_BIN_EXE_vqro").into());
    let output=tokio::task::block_in_place(||command(&binary).args(["call","workspace.apply","{\"ops\":[{\"op\":\"set\",\"key\":\"doc/child\",\"value\":1}]}"]).output()).unwrap();
    let wrote=output.status.success();
    let(inspector,_,_)=vqro::client::connect_as(&socket,"inspector","owner").await.unwrap();
    let raw_actor=inspector.call("workspace.history",json!({"limit":1})).await.unwrap()["txns"][0]["actor"].clone();
    let output=tokio::task::block_in_place(||command(&binary).args(["call","operational.register","{\"name\":\"child\"}"]).output()).unwrap();let child_registered=output.status.success();
    let script="import json,os; from vqro_ext import Vqro, supervision_hello; c=Vqro(os.environ['VQRO_SOCKET'],None); print(json.dumps(c.call('hello', supervision_hello({'client':'intentional-child'})))); c.call('operational.register',{'name':'intentional'})";
    let output=tokio::task::block_in_place(||command(std::path::Path::new("/usr/bin/python3")).env("PYTHONPATH",std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("extensions")).args(["-c",script]).output()).unwrap();
    server::shutdown(&core).await;
    assert!(wrote,"child raw workspace recovery remains available");
    assert_eq!(raw_actor,"undeclared","a generic child must never be silently promoted by inherited supervisor credentials");
    assert!(!child_registered,"raw children have no extension namespace ownership");
    assert!(output.status.success(),"{}",String::from_utf8_lossy(&output.stderr));let hello:serde_json::Value=serde_json::from_slice(&output.stdout).unwrap();assert_eq!(hello["actor"],"extension:writer","an opted-in public SDK still presents credentials for core verification");
}
