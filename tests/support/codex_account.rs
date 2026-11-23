//! Read-only native account verification double. No runtime/auth files or model calls.
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use vqro::codex::runtime::Runtime;
use vqro::rpc::Peer;

pub async fn runtime(account: Option<Value>) -> Arc<Runtime> {
    let (client, server) = tokio::io::duplex(4096);
    let (read, write) = tokio::io::split(client);
    let (peer, incoming) = Peer::spawn(read, write);
    tokio::spawn(async move {
        let (read, mut write) = tokio::io::split(server);
        let mut lines = BufReader::new(read).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let request: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(request["method"], "accountSession/list");
            let result = json!({"activeSessionId":"saved-native","sessions":[{"sessionId":"saved-native","isActive":true,"selectedWorkspaceAccountId":"native-workspace","workspaces":[{"accountId":"native-workspace"}]}]});
            write
                .write_all(format!("{}\n", json!({"id":request["id"],"result":result})).as_bytes())
                .await
                .unwrap();
        }
    });
    let configured = account.clone();
    let runtime = Runtime::with_host_metadata(
        peer,
        1,
        "inc".into(),
        "fixture".into(),
        "instance".into(),
        None,
        None,
        account,
    );
    let drain = runtime.clone();
    tokio::spawn(async move {
        drain.drain_native(incoming).await;
    });
    if let Some(account) = configured {
        runtime
            .verify_launcher_account(
                json!({"sessionId":"saved-native","accountId":"native-workspace"}),
                account,
                tokio::time::Instant::now() + std::time::Duration::from_secs(2),
            )
            .await
            .unwrap();
    }
    runtime
}
