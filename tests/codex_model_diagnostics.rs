use serde_json::json;
use vqro::{
    codex::configuration::{FailureStage, failure},
    rpc::RpcError,
};

#[test]
fn model_failure_keeps_exact_code_and_bounded_message_but_never_error_data() {
    let native = RpcError {
        code: -32602,
        message: "native model rejection".into(),
        data: Some(json!({"request":"DO_NOT_COPY_PAYLOAD","credential":"DO_NOT_COPY_SECRET"})),
    };
    let public = failure(FailureStage::NativeRpc, &native);
    assert_eq!(public.kind(), "backend");
    assert_eq!(
        public.message,
        "native model native_rpc failure (code -32602): native model rejection; do not replay"
    );
    let data = public.data.unwrap();
    assert_eq!(data["stage"], "native_rpc");
    assert_eq!(
        data["native_error"],
        json!({"code":-32602,"message":"native model rejection"})
    );
    assert!(!data.to_string().contains("DO_NOT_COPY"));
    let readback = failure(
        FailureStage::Readback,
        &RpcError::new("stale", "changed native thread"),
    );
    let data = readback.data.unwrap();
    assert_eq!(data["stage"], "readback");
    assert!(data.get("native_error").is_none());
    assert_eq!(data["readback_error"]["message"], "changed native thread");
}

#[test]
fn model_failure_omits_oversize_or_control_messages_instead_of_truncating() {
    for message in [
        "x".repeat(257),
        "x\nforged".into(),
        "x\rforged".into(),
        "x\u{0085}forged".into(),
        "x\u{202e}forged".into(),
        "x\u{2066}forged".into(),
        "x\u{2028}forged".into(),
        "x\u{2029}forged".into(),
    ] {
        let public = failure(
            FailureStage::NativeRpc,
            &RpcError {
                code: -32001,
                message,
                data: None,
            },
        );
        let data = public.data.unwrap();
        assert_eq!(data["native_error"]["code"], -32001);
        assert!(data["native_error"].get("message").is_none());
        assert_eq!(
            data["native_error"]["message_omitted"],
            "bounds_or_controls"
        );
    }
    let boundary = "\u{1f642}".repeat(64);
    assert_eq!(boundary.len(), 256);
    let data = failure(
        FailureStage::NativeRpc,
        &RpcError {
            code: -32602,
            message: boundary.clone(),
            data: None,
        },
    )
    .data
    .unwrap();
    assert_eq!(data["native_error"]["message"], boundary);
}
