//! The REAL core item relay + viewer, not a fake core. Native records are synthetic; no child/model/auth.
use base64::Engine;
use serde_json::json;
use std::time::Duration;
use vqro::claude_code::runtime::Shared;
use vqro::provider_client::{Config, Provider};
use vqro::{
    client,
    rpc::Incoming,
    server::{self, Backend},
};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actual_core_viewer_keeps_final_tail_after_acknowledged_provider_disconnect() {
    let dir = tempfile::Builder::new()
        .prefix("cc-tail-")
        .tempdir()
        .unwrap();
    let socket = dir.path().join("s");
    let core = server::start(
        socket.clone(),
        dir.path().join("st"),
        false, // private EMPTY store + defaults:None: no extension or real backend starts
        Backend::Fake,
        None,
    )
    .unwrap();
    struct Stop(vqro::core::Shared);
    impl Drop for Stop {
        fn drop(&mut self) {
            vqro::ext::stop_all(&self.0);
        }
    }
    let _stop = Stop(core.clone());
    let shared = Shared::new(
        "core-tail-boot".into(),
        "claude-core-tail".into(),
        "core-tail-instance".into(),
    );
    shared
        .state
        .lock()
        .unwrap()
        .initialized(&json!({"claude_code_version":"2.1.329"}));
    let mut config = Config::new(
        "claude-core-tail",
        "core-tail-instance",
        shared.state.lock().unwrap().provider_caps(),
    );
    // A tiny native output window forces the real credit/relay path, not one giant enqueue.
    config.stream_window_bytes = 16;
    config.call_timeout = Duration::from_secs(5);
    let io = vqro::transport::connect(&socket).await.unwrap();
    let (provider, mut requests) = Provider::connect(io, config).await.unwrap();
    provider.full_sync(vec![shared.facts()]).await.unwrap();
    let route_shared = shared.clone();
    let route_provider = provider.clone();
    let dispatch = tokio::spawn(async move {
        while let Some(request) = requests.recv().await {
            route_shared.serve_request(&route_provider, request).await;
        }
    });
    let (viewer, mut incoming, _) = client::connect(&socket, "claude-tail-viewer")
        .await
        .unwrap();
    let listed = viewer
        .call("resource.list", json!({"provider":"claude-core-tail"}))
        .await
        .unwrap();
    let resource = listed["resources"][0]["id"].as_str().unwrap();
    let attached = viewer
        .call("resource.attach", json!({"resource":resource,"items":{}}))
        .await
        .unwrap();
    let stream = attached["stream"].as_u64().unwrap();
    let publisher = tokio::spawn(shared.clone().publish(provider.clone()));
    {
        let mut state = shared.state.lock().unwrap();
        state.observe(&vqro::native_process::Event::Exited(
            vqro::native_process::Exit {
                code: Some(0),
                success: true,
            },
        ));
    }
    shared.wake();
    publisher.abort();
    let _ = publisher.await;
    {
        let mut state = shared.state.lock().unwrap();
        for index in 0..8 {
            state.observe(&vqro::native_process::Event::Record(json!({"type":"assistant","uuid":format!("tail-{index}"),"message":{"content":[{"type":"text","text":format!("native-tail-{index}:{}", "x".repeat(128))}]}})));
        }
        state.observe(&vqro::native_process::Event::Record(json!({"type":"assistant","uuid":"final-tail","message":{"content":[{"type":"text","text":"FINAL_NATIVE_TAIL_SENTINEL"}]}})));
        state.observe(&vqro::native_process::Event::Record(json!({"type":"result","uuid":"final-result","terminal_reason":"completed","is_error":false})));
    }
    shared.wake();
    // Intentionally do NOT consume the viewer yet. Run the actual bridge shutdown order and
    // disconnect immediately after its publication/retirement acknowledgements, then read it.
    tokio::time::timeout(Duration::from_secs(8), shared.finish(&provider))
        .await
        .unwrap()
        .unwrap();
    provider.close();
    let (data, reason) = tokio::time::timeout(Duration::from_secs(5), async {
        let mut data = Vec::new();
        loop {
            match incoming
                .recv()
                .await
                .expect("real core viewer closed without terminal")
            {
                Incoming::Notification { method, params } if params["stream"] == stream => {
                    if method == "stream.data" {
                        data.extend(
                            base64::engine::general_purpose::STANDARD
                                .decode(params["data"].as_str().unwrap())
                                .unwrap(),
                        );
                    } else if method == "stream.closed" {
                        break (data, params["reason"].clone());
                    } else if method == "stream.gap" {
                        panic!("unexpected real core gap: {params}");
                    }
                }
                _ => {}
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(
        reason["reason"], "native_reader_closed",
        "core must not replace a normal tail with provider_disconnected/purge"
    );
    let text = String::from_utf8(data).unwrap();
    assert!(
        text.contains("FINAL_NATIVE_TAIL_SENTINEL"),
        "real core purged buffered final native data"
    );
    assert!(text.contains("final-result"));
    viewer.close();
    dispatch.abort();
    server::shutdown(&core).await;
}
