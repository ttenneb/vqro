//! Pi's native envelopes over the shared backend-owned bounded JSONL process mechanism.
use std::collections::HashMap;
use std::ops::Deref;
use std::sync::Arc;
use serde_json::{Value, json};
use vqro::native_process::{Codec, Process};
// Compatibility exports are used by the binary and independent path-based integration tests.
#[allow(unused_imports)]
pub use vqro::native_process::{Error, Event, Exit, MAX_RECORD, Observe, Reply, Ticket};

struct PiCodec;
impl Codec for PiCodec {
    fn name(&self) -> &'static str { "Pi" }
    fn request(&self, command: &str, id: &str, args: Value) -> Result<Value, String> {
        let Some(mut params) = args.as_object().cloned() else { return Err("native arguments must be an object".into()); };
        params.insert("type".into(), json!(command));
        params.insert("id".into(), json!(id));
        Ok(Value::Object(params))
    }
    fn reply_id(&self, record: &Value) -> Result<Option<String>, String> {
        if record["type"] != "response" { return Ok(None); }
        record["id"].as_str().map(|id| Some(id.into()))
            .ok_or_else(|| "native response has no id".into())
    }
    fn reply(&self, record: &Value, command: &str) -> Result<Reply, String> {
        if record["command"] != command || !record["success"].is_boolean() {
            return Err("native response command/type mismatch".into());
        }
        Ok(Reply { success:record["success"] == true,
            data:record.get("data").cloned().unwrap_or(Value::Null),
            error:record.get("error").cloned().unwrap_or(Value::Null) })
    }
}

#[derive(Clone)]
pub struct Native(Process);
/// Pi alone admits the two dedicated selectors; the shared/Codex default remains unchanged.
pub fn native_env(command:&mut tokio::process::Command,env:&HashMap<String,String>) {
    vqro::native_process::native_env(command,env);
    for key in ["VQRO_TESTING_SOCKET", "VQRO_TESTING_STATE_DIR"] {
        if let Some(value)=env.get(key) { command.env(key,value); }
    }
}
impl Native {
    pub async fn spawn(argv: &[String], cwd: &std::path::Path, env: &HashMap<String,String>, observe: Observe) -> Result<Self, Error> {
        Process::spawn_with_env(argv, cwd, env, Arc::new(PiCodec), observe, native_env).await.map(Self)
    }
}
impl Deref for Native {
    type Target = Process;
    fn deref(&self) -> &Self::Target { &self.0 }
}
