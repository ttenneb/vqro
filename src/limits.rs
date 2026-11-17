//! Mechanism limits shared by admission and transport recovery. No workspace policy.
pub const MIB: usize = 1024 * 1024;
pub const ENTRY_BYTES: usize = MIB;
pub const WORKSPACE_BYTES: usize = 16 * MIB;
pub const LIVE_VALUE_BYTES: usize = MIB;
pub const LIVE_BYTES: usize = 8 * MIB;
/// Enforced for canonical and unpublished IO-recovery resource JSON by provider admission.
pub const RESOURCE_ALLOWANCE: usize = crate::resource_admission::MAX_RESOURCE_BYTES;
pub const SNAPSHOT_BYTES: usize = WORKSPACE_BYTES + LIVE_BYTES + RESOURCE_ALLOWANCE + 8 * MIB;

/// Count encoded JSON without allocating another copy of a large value.
pub fn encoded_bytes(value: &impl serde::Serialize) -> usize {
    #[derive(Default)]
    struct Counter(usize);
    impl std::io::Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> { self.0 = self.0.saturating_add(bytes.len()); Ok(bytes.len()) }
        fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
    }
    let mut counter = Counter::default();
    if serde_json::to_writer(&mut counter, value).is_err() { usize::MAX } else { counter.0 }
}

pub fn too_large(scope: &str, bytes: usize, limit: usize) -> crate::rpc::RpcError {
    let mut error = crate::rpc::RpcError::new("too_large", format!("{scope} uses {bytes} encoded bytes; limit is {limit}; write not applied"));
    error.data.as_mut().unwrap()["outcome"] = serde_json::json!("rejected");
    error.data.as_mut().unwrap()["bytes"] = serde_json::json!(bytes);
    error.data.as_mut().unwrap()["limit"] = serde_json::json!(limit);
    error
}
