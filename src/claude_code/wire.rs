//! Claude Code's native stream-json control envelopes, not JSON-RPC.
//! Reference: @anthropic-ai/claude-agent-sdk 0.3.329 sdk.d.ts, SDKControlRequest/Response.
use serde_json::{Value, json};
use crate::native_process::{Codec, Reply};

pub struct ClaudeCodec;

impl Codec for ClaudeCodec {
    fn name(&self) -> &'static str { "Claude Code" }

    fn request(&self, command: &str, id: &str, args: Value) -> Result<Value, String> {
        let Some(mut request) = args.as_object().cloned() else {
            return Err("native arguments must be an object".into());
        };
        // The adapter chooses the subtype; argument objects cannot replace it or the outer id.
        request.insert("subtype".into(), json!(command));
        Ok(json!({"type":"control_request","request_id":id,"request":request}))
    }

    fn reply_id(&self, record: &Value) -> Result<Option<String>, String> {
        if record["type"] != "control_response" { return Ok(None); }
        let Some(response) = record["response"].as_object() else {
            return Err("native control response has no response object".into());
        };
        let Some(subtype) = response.get("subtype").and_then(Value::as_str) else {
            return Err("native control response has no subtype".into());
        };
        // Unknown future subtypes are observations, not completion of a pending request.
        if !matches!(subtype, "success" | "error") { return Ok(None); }
        response.get("request_id").and_then(Value::as_str).map(|id| Some(id.into()))
            .ok_or_else(|| "native control response has no request_id".into())
    }

    fn reply(&self, record: &Value, _command: &str) -> Result<Reply, String> {
        let response = &record["response"];
        match response["subtype"].as_str() {
            Some("success") => Ok(Reply {
                success: true,
                data: response.get("response").cloned().unwrap_or(Value::Null),
                error: Value::Null,
            }),
            Some("error") if response["error"].is_string() => Ok(Reply {
                success: false, data: Value::Null, error: response["error"].clone(),
            }),
            _ => Err("native control response payload/type mismatch".into()),
        }
        // The process observes the complete original frame before waking its caller. In particular,
        // initialize's outer pending_*_requests must not disappear with this convenience payload.
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_control_request_preserves_arguments_without_accepting_forged_subtype() {
        assert_eq!(ClaudeCodec.request("initialize", "n7", json!({"subtype":"interrupt","hooks":{}})).unwrap(),
            json!({"type":"control_request","request_id":"n7","request":{"subtype":"initialize","hooks":{}}}));
        assert!(ClaudeCodec.request("initialize", "n1", json!([])).is_err());
    }

    #[test]
    fn success_and_empty_ack_are_correlated_by_native_request_id() {
        let frame = json!({"type":"control_response","response":{"subtype":"success","request_id":"n9","response":{"commands":[]}}});
        assert_eq!(ClaudeCodec.reply_id(&frame).unwrap().as_deref(), Some("n9"));
        let reply = ClaudeCodec.reply(&frame, "initialize").unwrap();
        assert!(reply.success);
        assert_eq!(reply.data, json!({"commands":[]}));
        let empty = json!({"type":"control_response","response":{"subtype":"success","request_id":"n10"}});
        assert!(ClaudeCodec.reply(&empty, "interrupt").unwrap().data.is_null());
    }

    #[test]
    fn native_error_is_not_a_transport_failure_or_success() {
        let frame = json!({"type":"control_response","response":{"subtype":"error","request_id":"n1","error":"unsupported subtype"}});
        assert_eq!(ClaudeCodec.reply_id(&frame).unwrap().as_deref(), Some("n1"));
        let reply = ClaudeCodec.reply(&frame, "future_operation").unwrap();
        assert!(!reply.success);
        assert_eq!(reply.error, "unsupported subtype");
        assert!(reply.data.is_null());
    }

    #[test]
    fn incoming_questions_and_conversation_are_not_outbound_control_replies() {
        for frame in [
            json!({"type":"control_request","request_id":"p1","request":{"subtype":"can_use_tool"}}),
            json!({"type":"assistant","message":{"content":[]}}),
            json!({"type":"system","subtype":"control_request_progress","request_id":"n1"}),
        ] { assert!(ClaudeCodec.reply_id(&frame).unwrap().is_none()); }
    }

    #[test]
    fn future_subtypes_do_not_claim_completion() {
        let future = json!({"type":"control_response","response":{"subtype":"future_progress","request_id":"n1"}});
        assert!(ClaudeCodec.reply_id(&future).unwrap().is_none());
    }

    #[test]
    fn malformed_known_control_replies_fail_closed() {
        for frame in [
            json!({"type":"control_response"}),
            json!({"type":"control_response","response":[]}),
            json!({"type":"control_response","response":{"request_id":"n1"}}),
            json!({"type":"control_response","response":{"subtype":"success","request_id":7}}),
        ] { assert!(ClaudeCodec.reply_id(&frame).is_err()); }
        let invalid = json!({"type":"control_response","response":{"subtype":"error","request_id":"n1","error":false}});
        assert!(ClaudeCodec.reply(&invalid, "initialize").is_err());
    }
}
