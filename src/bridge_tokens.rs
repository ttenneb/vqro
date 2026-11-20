//! Opt-in runtime credentials. Spawn tokens are ephemeral; rotated resume credentials are durable
//! so a backend-owned bridge can reconnect after a core restart without resending any work.
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::watch;
use crate::core::{Core, Shared};
use crate::rpc::{Peer, RpcError};
use crate::provider;

#[derive(Clone)] enum State { Pending, Ready(String), Invalid }
#[derive(Clone, Serialize, Deserialize)] struct Resume { host: String, provider: String, instance: String }
#[derive(Default)]
pub struct Tokens {
    pending: BTreeMap<String, watch::Sender<State>>,
    resumes: BTreeMap<String, Resume>,
    path: Option<PathBuf>,
}
pub struct Claim { pub host: String, pub resuming: bool }
/// Shared credential entropy only; callers choose their own lifetime/storage.
pub(crate) fn random_token() -> Result<String, RpcError> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|e| provider::rejected("io", format!("credential randomness: {e}")))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}
impl Tokens {
    pub fn open(path: Option<PathBuf>) -> anyhow::Result<Self> {
        let resumes = match &path {
            Some(path) => match std::fs::read(path) {
                Ok(bytes) => serde_json::from_slice(&bytes)?,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
                Err(e) => return Err(e.into()),
            },
            None => BTreeMap::new(),
        };
        Ok(Self { pending: BTreeMap::new(), resumes, path })
    }
    fn save(&mut self, candidate: BTreeMap<String, Resume>) -> Result<(), RpcError> {
        if let Some(path) = &self.path {
            let bytes = serde_json::to_vec(&candidate).map_err(|e| RpcError::new("io", e.to_string()))?;
            if let Err(error) = crate::store::write_atomic(path, &bytes) {
                if error.renamed { self.resumes = candidate; return Err(RpcError::new("durability_unconfirmed", "runtime credential rotation adopted but directory durability is unconfirmed")); }
                return Err(RpcError::new("io", format!("could not persist runtime credentials: {error}")));
            }
        }
        self.resumes = candidate;
        Ok(())
    }
    pub fn valid_claim(&self, token: &str, claim: &Claim, provider: &str, instance: &str) -> bool {
        if claim.resuming {
            self.resumes.get(token).is_some_and(|r| r.host == claim.host && r.provider == provider && r.instance == instance)
        } else { self.pending.get(token).is_some_and(|t| matches!(&*t.borrow(), State::Ready(id) if id == &claim.host)) }
    }
    pub fn rotate(&mut self, token: &str, claim: &Claim, provider: &str, instance: &str) -> Result<String, RpcError> {
        if !self.valid_claim(token, claim, provider, instance) { return Err(RpcError::new("forbidden", "runtime token used or identity differs from its registration")); }
        let next = random_token()?;
        let mut candidate = self.resumes.clone();
        // Keep exactly the presented credential and the newly issued one for this host.
        // Presenting current acknowledges receipt and retires the older previous credential;
        // presenting previous after a lost reply preserves it while replacing unseen current.
        candidate.retain(|old, resume| resume.host != claim.host || old == token);
        candidate.insert(token.into(), Resume { host: claim.host.clone(), provider: provider.into(), instance: instance.into() });
        candidate.insert(next.clone(), Resume { host: claim.host.clone(), provider: provider.into(), instance: instance.into() });
        let result = self.save(candidate);
        if result.is_ok() || result.as_ref().is_err_and(|e| e.kind() == "durability_unconfirmed") { self.pending.remove(token); }
        result.map_err(|mut error| {
            if error.kind() == "durability_unconfirmed" { error.data.as_mut().unwrap()["resume_token"] = json!(next); }
            error
        })?;
        Ok(next)
    }
    pub fn expire_host(&mut self,host:&str)->Result<(),RpcError>{self.expire_matching(|id|id==host)}
    pub fn expire_hosts(&mut self,hosts:&std::collections::HashSet<&str>)->Result<(),RpcError>{self.expire_matching(|id|hosts.contains(id))}
    fn expire_matching(&mut self,expired:impl Fn(&str)->bool)->Result<(),RpcError>{
        self.pending.retain(|_, token| {
            let remove = matches!(&*token.borrow(), State::Ready(id) if expired(id));
            if remove { token.send_replace(State::Invalid); }
            !remove
        });
        if !self.resumes.values().any(|resume|expired(&resume.host)){return Ok(());}
        let mut candidate = self.resumes.clone();
        candidate.retain(|_, resume| !expired(&resume.host));
        // Identity tombstones independently reject this host even if persisting revocation fails.
        self.resumes = candidate.clone();
        self.save(candidate)
    }
}

pub struct BridgeReservation { core: Weak<Mutex<Core>>, pub token: String, committed: bool }
impl BridgeReservation {
    pub fn new(core: &Shared) -> Result<Self, RpcError> {
        let token = random_token()?;
        let mut c = core.lock().unwrap();
        if c.bridge_tokens.pending.len() >= 1024 { return Err(provider::rejected("overloaded", "pending bridge token limit reached")); }
        c.bridge_tokens.pending.insert(token.clone(), watch::channel(State::Pending).0);
        Ok(Self { core: Arc::downgrade(core), token, committed: false })
    }
    pub fn bind(mut self, host: &str) -> Result<(), RpcError> {
        let core = self.core.upgrade().ok_or_else(|| provider::indeterminate("core closed before bridge host was established"))?;
        let c = core.lock().unwrap();
        let token = c.bridge_tokens.pending.get(&self.token).ok_or_else(|| provider::indeterminate("bridge token expired before host was established"))?;
        token.send_replace(State::Ready(host.into()));
        self.committed = true;
        Ok(())
    }
}
impl Drop for BridgeReservation {
    fn drop(&mut self) {
        if !self.committed {
            if let Some(core) = self.core.upgrade() {
                if let Some(token) = core.lock().unwrap().bridge_tokens.pending.remove(&self.token) { token.send_replace(State::Invalid); }
            }
        }
    }
}

pub async fn claim(core: &Shared, peer: &Peer, token: &str) -> Result<Claim, RpcError> {
    let mut state = {
        let c = core.lock().unwrap();
        if let Some(resume) = c.bridge_tokens.resumes.get(token) {
            if !c.has_resource_identity(&resume.host) { return Err(RpcError::new("forbidden", "runtime host was removed")); }
            return Ok(Claim { host: resume.host.clone(), resuming: true });
        }
        c.bridge_tokens.pending.get(token).ok_or_else(|| RpcError::new("forbidden", "unknown, used or expired bridge token"))?.subscribe()
    };
    let wait = async {
        loop {
            let current = state.borrow_and_update().clone();
            match current {
                State::Ready(host) => return Ok(host),
                State::Invalid => return Err(RpcError::new("forbidden", "bridge spawn failed or expired")),
                State::Pending => { if state.changed().await.is_err() { return Err(RpcError::new("forbidden", "bridge token expired")); } }
            }
        }
    };
    let host = tokio::select! {
        _ = peer.closed() => return Err(provider::indeterminate("runtime connection closed while waiting for host")),
        result = tokio::time::timeout(Duration::from_secs(5), wait) => result.map_err(|_| RpcError::new("forbidden", "host not established within pending-token wait"))??,
    };
    if !core.lock().unwrap().has_resource_identity(&host) { return Err(RpcError::new("forbidden", "runtime host was removed")); }
    Ok(Claim { host, resuming: false })
}

#[cfg(test)] mod contention_profile {
    use super::*;
    #[test] #[ignore="private credential rotation profile"]
    fn rotation_400_hosts() {
        let root=tempfile::tempdir().unwrap();let mut tokens=Tokens::open(Some(root.path().join("tokens.json"))).unwrap();
        for i in 0..400 {for j in 0..2 {tokens.resumes.insert(format!("{i}-{j}"),Resume {host:format!("r{i}"),provider:format!("p{i}"),instance:"instance".into()});}}
        let mut token="0-1".to_string();let mut times=Vec::new();
        for _ in 0..100 {let t=std::time::Instant::now();token=tokens.rotate(&token,&Claim {host:"r0".into(),resuming:true},"p0","instance").unwrap();times.push(t.elapsed().as_secs_f64()*1000.);}
        times.sort_by(f64::total_cmp);eprintln!("credential400 p50_ms={} p99_ms={} max_ms={}",times[50],times[98],times[99]);
    }
}
