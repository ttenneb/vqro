//! Ephemeral command attribution, not workspace authority or an effect replay mechanism.
//! Proofs name one live callback on one connection; nested proofs retain/revalidate ancestors.
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use crate::rpc::RpcError;

const MAX_ACTIVE: usize = 1024;
const MAX_BYTES: usize = 8 * 1024 * 1024;
const MAX_DEPTH: usize = 32;
pub(crate) type Shared = Arc<Mutex<Invocations>>;

#[derive(Clone, Default)]
pub(crate) struct Context {
    pub proof: Option<String>,
    pub principal: Option<Arc<str>>,
    pub initiator: Option<Arc<str>>,
    pub attribution: crate::principal::Attribution,
}
struct Entry {
    connection: u64,
    principal: Option<Arc<str>>,
    initiator: Option<Arc<str>>,
    parent: Option<String>,
    expires: Instant,
    bytes: usize,
}
#[derive(Default)]
pub(crate) struct Invocations {
    entries: BTreeMap<String, Entry>,
    bytes: usize,
}
pub(crate) struct Scope {
    shared: Shared,
    token: String,
}
impl Scope {
    pub fn token(&self) -> &str { &self.token }
}
impl Drop for Scope {
    fn drop(&mut self) {
        let mut state = self.shared.lock().unwrap_or_else(|error| error.into_inner());
        if let Some(entry) = state.entries.remove(&self.token) { state.bytes -= entry.bytes; }
    }
}
fn denied() -> RpcError { RpcError::new("forbidden", "attribution requires a live command invocation on this connection") }
impl Invocations {
    fn chain(&self, proof: &str) -> Result<(Instant, usize), RpcError> {
        let first = self.entries.get(proof).ok_or_else(denied)?;
        let mut expires = first.expires;
        let mut depth = 0;
        let mut current = Some(proof);
        while let Some(token) = current {
            depth += 1;
            if depth > MAX_DEPTH { return Err(denied()); }
            let entry = self.entries.get(token).ok_or_else(denied)?;
            expires = expires.min(entry.expires);
            if expires <= Instant::now() { return Err(denied()); }
            current = entry.parent.as_deref();
        }
        Ok((expires, depth))
    }
}

pub(crate) fn authorize(shared: &Shared, connection: u64, proof: Option<String>) -> Result<Context, RpcError> {
    let Some(proof) = proof else { return Ok(Context::default()); };
    if proof.len() != 32 || !proof.bytes().all(|byte| byte.is_ascii_hexdigit()) { return Err(denied()); }
    let state = shared.lock().unwrap();
    if state.entries.get(&proof).is_none_or(|entry| entry.connection != connection) { return Err(denied()); }
    state.chain(&proof)?;
    let principal=state.entries.get(&proof).and_then(|entry|entry.principal.clone());
    let initiator=state.entries.get(&proof).and_then(|entry|entry.initiator.clone());
    Ok(Context { proof:Some(proof), principal, initiator, attribution:crate::principal::Attribution::default() })
}

#[cfg(test)]
pub(crate) fn enter(shared: &Shared, connection: u64, author: &str, parent: &Context, timeout: Duration) -> Result<Scope, RpcError> {
    enter_attributed(shared,connection,Some(parent.principal.as_deref().unwrap_or(author)),parent.initiator.as_deref(),parent,timeout)
}
pub(crate) fn enter_attributed(shared:&Shared, connection:u64, principal:Option<&str>, initiator:Option<&str>, parent:&Context, timeout:Duration)->Result<Scope,RpcError> {
    let mut state = shared.lock().unwrap();
    let now = Instant::now();
    state.entries.retain(|_, entry| entry.expires > now);
    state.bytes = state.entries.values().map(|entry| entry.bytes).sum();
    let mut expires = now + timeout;
    if let Some(proof) = &parent.proof {
        let (parent_expires, depth) = state.chain(proof)?;
        if depth >= MAX_DEPTH { return Err(RpcError::new("overloaded", "command attribution nesting limit; not dispatched")); }
        expires = expires.min(parent_expires);
    }
    let principal=principal.map(Arc::<str>::from).or_else(||parent.proof.as_ref().and_then(|proof|state.entries.get(proof).and_then(|entry|entry.principal.clone())));
    let initiator=initiator.map(Arc::<str>::from).or_else(||parent.proof.as_ref().and_then(|proof|state.entries.get(proof).and_then(|entry|entry.initiator.clone()))).or_else(||principal.clone());
    let bytes = crate::limits::encoded_bytes(&(principal.as_deref(),initiator.as_deref())).saturating_add(128);
    if state.entries.len() >= MAX_ACTIVE || bytes > MAX_BYTES.saturating_sub(state.bytes) {
        return Err(RpcError::new("overloaded", "command attribution capacity; not dispatched"));
    }
    let token = loop {
        let mut random = [0u8; 16];
        getrandom::fill(&mut random).map_err(|error| RpcError::new("io", format!("invocation nonce: {error}")))?;
        let token: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
        if !state.entries.contains_key(&token) { break token; }
    };
    state.entries.insert(token.clone(), Entry { connection, principal, initiator, parent:parent.proof.clone(), expires, bytes });
    state.bytes += bytes;
    Ok(Scope { shared:shared.clone(), token })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn proofs_are_connection_bound_and_revoked_with_their_parent() {
        let state = Shared::default();
        let parent = enter(&state, 2, "user:1", &Context::default(), Duration::from_secs(5)).unwrap();
        assert!(authorize(&state, 3, Some(parent.token().into())).is_err());
        let context = authorize(&state, 2, Some(parent.token().into())).unwrap();
        let child = enter(&state, 3, "ext:outer", &context, Duration::from_secs(60)).unwrap();
        assert_eq!(authorize(&state, 3, Some(child.token().into())).unwrap().principal.as_deref(), Some("user:1"));
        { let state = state.lock().unwrap(); assert!(state.entries[child.token()].expires <= state.entries[parent.token()].expires); }
        drop(parent);
        assert!(authorize(&state, 3, Some(child.token().into())).is_err());
        drop(child);
        assert_eq!(state.lock().unwrap().bytes, 0);
    }
    #[test]
    fn active_count_and_nesting_are_bounded() {
        let state = Shared::default();
        let mut scopes = Vec::new();
        for _ in 0..MAX_ACTIVE { scopes.push(enter(&state, 2, "user", &Context::default(), Duration::from_secs(60)).unwrap()); }
        assert!(enter(&state, 2, "user", &Context::default(), Duration::from_secs(60)).is_err());
        drop(scopes);
        let mut scopes = Vec::new();
        let mut context = Context::default();
        for _ in 0..MAX_DEPTH {
            let scope = enter(&state, 2, "user", &context, Duration::from_secs(60)).unwrap();
            context = authorize(&state, 2, Some(scope.token().into())).unwrap();
            scopes.push(scope);
        }
        assert!(enter(&state, 2, "user", &context, Duration::from_secs(60)).is_err());
    }
    #[test]
    fn expired_or_oversized_contexts_fail_closed() {
        let state = Shared::default();
        let scope = enter(&state, 2, "user:1", &Context::default(), Duration::from_secs(1)).unwrap();
        state.lock().unwrap().entries.get_mut(scope.token()).unwrap().expires = Instant::now()-Duration::from_secs(1);
        assert!(authorize(&state, 2, Some(scope.token().into())).is_err());
        assert!(enter(&state, 3, &"\0".repeat(MAX_BYTES/6+1), &Context::default(), Duration::from_secs(1)).is_err());
    }
}
