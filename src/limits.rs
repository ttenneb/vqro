//! Mechanism limits shared by admission and transport recovery. No workspace policy.
pub const MIB: usize = 1024 * 1024;
pub const ENTRY_BYTES: usize = MIB;
pub const WORKSPACE_BYTES: usize = 16 * MIB;
pub const LIVE_VALUE_BYTES: usize = MIB;
pub const LIVE_BYTES: usize = 8 * MIB;
/// Ephemeral command rows, including encoded names/titles/owners and array separators.
pub const COMMAND_BYTES: usize = MIB;
/// Enforced for canonical and unpublished IO-recovery resource JSON by provider admission.
pub const RESOURCE_ALLOWANCE: usize = crate::resource_admission::MAX_RESOURCE_BYTES;
pub const SNAPSHOT_BYTES: usize = WORKSPACE_BYTES + LIVE_BYTES + RESOURCE_ALLOWANCE + 8 * MIB;

/// Leave room for history/snapshot/RPC wrappers below serde's128-container framing limit.
pub const JSON_DEPTH: usize = 64;
pub const MESSAGE_DEPTH: usize = 96;

/// Iterative validation: rejection never recursively walks the untrusted tree.
pub fn json_depth(value:&serde_json::Value,limit:usize)->Result<(),crate::rpc::RpcError> {
    enum Children<'a> {Array(std::slice::Iter<'a,serde_json::Value>),Object(serde_json::map::Values<'a>)}
    impl<'a> Iterator for Children<'a> {
        type Item=&'a serde_json::Value;
        fn next(&mut self)->Option<Self::Item> {match self {Self::Array(values)=>values.next(),Self::Object(values)=>values.next()}}
    }
    // One iterator per ancestor, not one pending pointer per leaf of a wide document.
    let mut pending=vec![Children::Array(std::slice::from_ref(value).iter())];
    while let Some(parent)=pending.last_mut() {
        let Some(value)=parent.next() else {pending.pop();continue;};
        let children=match value {serde_json::Value::Array(values)=>Children::Array(values.iter()),serde_json::Value::Object(values)=>Children::Object(values.values()),_=>continue};
        if pending.len()>limit {
            let mut error=crate::rpc::RpcError::new("too_large",format!("JSON nesting exceeds {limit} containers; value not admitted"));
            let data=error.data.as_mut().unwrap();data["outcome"]=serde_json::json!("rejected");data["limit"]=serde_json::json!(limit);data["unit"]=serde_json::json!("container_depth");
            return Err(error);
        }
        pending.push(children);
    }
    Ok(())
}

/// Only for syntax-validated RawValue input. Strings/escaped quotes do not count as containers.
/// RawValue skips nested JSON iteratively, so unsafe legacy trees are never constructed.
pub(crate) fn raw_depth_within(raw:&str,limit:usize)->bool {
    let(mut depth,mut quoted,mut escaped)=(0usize,false,false);
    for byte in raw.bytes() {
        if quoted {if escaped {escaped=false;} else if byte==b'\\' {escaped=true;} else if byte==b'"' {quoted=false;}}
        else {match byte {b'"'=>quoted=true,b'['|b'{'=>{depth+=1;if depth>limit{return false;}},b']'|b'}'=>depth-=1,_=>{}}}
    }
    true
}

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
