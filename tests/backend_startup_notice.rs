//! Private fake-core controls: connecting is a bounded notice, never readiness.
use serde_json::{json, Value};
use std::time::Duration;
use vqro::{client, server::{self, Backend}};

#[tokio::test]
async fn slow_provider_connects_then_clears_notice() {
    let root = tempfile::tempdir().unwrap();
    let socket = root.path().join("p");
    let core = server::start(socket.clone(), root.path().join("st"), false, Backend::Fake, None).unwrap();
    let (peer, _, _) = client::connect(&socket, "startup-test").await.unwrap();
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake_provider.py");
    let python = vqro::program::resolve_argv0("python3").unwrap();
    let mut argv = vec![python.0.to_string_lossy().into_owned()];
    argv.extend(python.1);
    argv.extend(["-c".into(), "import runpy,sys,time; time.sleep(0.5); p=sys.argv.pop(1); runpy.run_path(p,run_name='__main__')".into(), fixture.to_string_lossy().into_owned(), "--name".into(), "herdr".into()]);
    peer.call("workspace.apply", json!({"ops":[{"op":"set","key":"extension/herdr","value":{"argv":argv,"kind":"provider","default":true}}]})).await.unwrap();
    let snap = peer.call("workspace.snapshot", Value::Null).await.unwrap();
    assert_eq!(snap["backend_notice"], "connecting to herdr…");
    assert!(core.lock().unwrap().provider_route(None, "terminal.spawn").is_err());
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let snap = peer.call("workspace.snapshot", Value::Null).await.unwrap();
            if snap["providers"]["herdr"]["synced"] == true {
                assert!(snap["backend_notice"].is_null());
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }).await.unwrap();
    {
        let mut c = core.lock().unwrap();
        c.on_adapter_event("herdr", vqro::adapter::AdapterEvent::Connected(false, Some("control: current registration disconnected".into())));
        assert!(!c.backend_notice().unwrap().starts_with("connecting to"), "a failed current-attempt registration must not be hidden by grace");
    }
    vqro::ext::restart(&core, "herdr").unwrap();
    {
        let c = core.lock().unwrap();
        assert!(c.providers.contains_key("herdr"), "the previous backend record is deliberately retained");
        assert!(!c.providers["herdr"].owner.as_ref().unwrap().is_live());
        assert_eq!(c.backend_notice().as_deref(), Some("connecting to herdr…"), "the retired registration must not suppress the fresh start window");
        assert!(c.provider_route(None, "terminal.spawn").is_err(), "the fresh notice is not readiness");
    }
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let c = core.lock().unwrap();
            let ready = c.providers.get("herdr").is_some_and(|p| p.synced && p.owner.as_ref().is_some_and(|o| o.is_live()));
            if ready {
                assert!(c.backend_notice().is_none());
                break;
            }
            drop(c);
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }).await.unwrap();
    server::shutdown(&core).await;
}

#[cfg(unix)] // These hold/exit controls use /bin/cat and /bin/false.
#[tokio::test]
async fn expiry_and_real_failure_publish_unavailable_without_polling() {
    let root = tempfile::tempdir().unwrap();
    let socket = root.path().join("p");
    let core = server::start(socket.clone(), root.path().join("st"), false, Backend::Fake, None).unwrap();
    let (peer, _, _) = client::connect(&socket, "startup-test").await.unwrap();
    peer.call("workspace.apply", json!({"ops":[{"op":"set","key":"extension/herdr","value":{"argv":["/bin/cat"],"kind":"provider","default":true}}]})).await.unwrap();
    assert_eq!(core.lock().unwrap().backend_notice().as_deref(), Some("connecting to herdr…"));
    tokio::time::sleep(Duration::from_millis(3200)).await;
    let unavailable = |v: &Value| v.as_str().is_some_and(|s| s.contains("not connected"));
    assert!(core.lock().unwrap().events_since(0).unwrap().iter().any(|e| e["type"] == "extension" && unavailable(&e["data"]["backend_notice"])));
    peer.call("workspace.apply", json!({"ops":[{"op":"set","key":"extension/herdr","value":{"argv":["/bin/false"],"kind":"provider","default":true}}]})).await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let c = core.lock().unwrap();
            let failed = c.events_since(0).unwrap().iter().any(|e| e["type"] == "extension" && e["data"]["state"]["last_exit"].is_string() && unavailable(&e["data"]["backend_notice"]));
            drop(c);
            if failed { break; }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.unwrap();
    server::shutdown(&core).await;
}
