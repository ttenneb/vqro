//! Native end evidence is ordered before publisher/provider teardown, even under socket backpressure.
use serde_json::{json,Value};
use vqro::provider_client::{Provider,ProviderError};

pub async fn retire(provider:&Provider,facts:Value,name:&str)->Result<(),ProviderError> {
    // Try to preserve the complete final facts, but a refusal must not suppress the native end report.
    let final_facts=provider.full_sync(vec![facts]).await;
    provider.gone("agent","",json!({"reason":"exited"})).await?;
    // The core handles provider notifications inline/in order before dispatching this scoped read
    // (server::connection). Its reply proves the preceding gone was processed, not just enqueued.
    provider.call("resource.list",json!({"provider":name,"backend_ref":"agent"})).await?;
    final_facts
}
