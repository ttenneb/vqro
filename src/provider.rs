//! In-process provider mechanism. Backends own execution; core owns identity, availability and
//! generation-fenced routing. This is not the proposed external provider wire protocol.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::watch;
use crate::adapter::{AdapterHandle, ResourceFacts};
use crate::rpc::RpcError;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProviderInfo {
    /// Adapter-local connection token; never a public wire generation or persisted identity.
    #[serde(skip)]
    pub connection: u64,
    pub instance: String,
    pub role: String,
    pub software: Value,
}

#[derive(Clone, Debug)]
pub struct Health {
    pub connected: bool,
    pub generation: u64,
    pub instance: String,
}

pub struct Provider {
    pub handle: AdapterHandle,
    pub info: Option<ProviderInfo>,
    pub source: u64,
    pub generation: u64,
    pub since: u64,
    pub error: Option<String>,
    pub synced: bool,
    /// Additional validated wire registration metadata (version, effective limits, runtime host).
    pub metadata: Value,
    pub health: watch::Sender<Health>,
}

impl Provider {
    pub fn new(handle: AdapterHandle, source: u64, generation: u64) -> Self {
        let (health, _) = watch::channel(Health { connected: false, generation, instance: String::new() });
        Self { handle, info: None, source, generation, since: crate::store::now_ms(), error: None, synced: false, metadata: json!({}), health }
    }

    pub fn register(&mut self, info: ProviderInfo) {
        self.generation += 1;
        self.since = crate::store::now_ms();
        self.error = None;
        self.synced = false;
        self.health.send_replace(Health { connected: true, generation: self.generation, instance: info.instance.clone() });
        self.handle.session = Some(info.connection);
        self.info = Some(info);
    }

    pub fn disconnect(&mut self, error: Option<String>) {
        let mut health = self.health.borrow().clone();
        health.connected = false;
        self.health.send_replace(health);
        self.error = error;
        self.synced = false;
        self.since = crate::store::now_ms();
    }

    pub fn to_json(&self) -> Value {
        let health = self.health.borrow();
        let mut value = json!({"name": self.handle.name, "role": self.info.as_ref().map(|i| &i.role),
            "instance": health.instance, "generation": self.generation,
            "software": self.info.as_ref().map(|i| &i.software), "capabilities": self.handle.capabilities,
            "state": if health.connected { "connected" } else { "disconnected" },
            "connected": health.connected, "synced": self.synced, "since": self.since, "error": self.error});
        for key in ["provider_version", "limits", "host"] {
            if let Some(extra) = self.metadata.get(key) { value[key] = extra.clone(); }
        }
        value
    }

    pub fn route(&self, capability: &str) -> Result<Route, RpcError> {
        if !self.health.borrow().connected {
            return Err(rejected("unavailable", format!("provider {} is disconnected", self.handle.name)));
        }
        if !self.handle.capabilities.get(capability).is_some_and(Value::is_object) {
            return Err(rejected("unsupported", format!("provider {} does not declare {capability}", self.handle.name)));
        }
        Ok(Route { handle: self.handle.clone(), health: self.health.subscribe(), generation: self.generation, instance: self.health.borrow().instance.clone(), resource: None, target: None })
    }
}

#[derive(Clone)]
pub struct Route {
    pub handle: AdapterHandle,
    pub generation: u64,
    pub instance: String,
    pub health: watch::Receiver<Health>,
    pub resource: Option<watch::Receiver<bool>>,
    pub target: Option<(String, String)>,
}

impl Route {
    pub fn valid(&self) -> bool {
        let health = self.health.borrow();
        health.connected && health.generation == self.generation && health.instance == self.instance
            && self.resource.as_ref().is_none_or(|r| *r.borrow())
    }

    pub async fn ended(&self) {
        let mut health = self.health.clone();
        let mut resource = self.resource.clone();
        // Do not erase an unseen generation transition. The validity check also catches a
        // disconnect/reconnect that happened before this future was first polled.
        if !self.valid() { return; }
        tokio::select! {
            _ = health.changed() => {},
            _ = async {
                match resource.as_mut() {
                    Some(r) => { let _ = r.changed().await; }
                    None => std::future::pending::<()>().await,
                }
            } => {},
        }
    }

    pub async fn guard<T>(&self, work: impl std::future::Future<Output = Result<T, RpcError>>) -> Result<T, RpcError> {
        if !self.valid() { return Err(rejected("unavailable", "provider changed before dispatch")); }
        let result = tokio::select! {
            biased;
            _ = self.ended() => Err(indeterminate("provider generation or resource ended while effect was in flight")),
            result = work => result,
        };
        if !self.valid() { return Err(indeterminate("provider changed while effect was in flight")); }
        result
    }
}

pub fn rejected(kind: &str, message: impl Into<String>) -> RpcError {
    let mut error = RpcError::new(kind, message);
    error.data.as_mut().unwrap()["outcome"] = json!("rejected");
    error
}

pub fn indeterminate(message: impl Into<String>) -> RpcError {
    let mut error = RpcError::new("unavailable", message);
    error.data.as_mut().unwrap()["outcome"] = json!("indeterminate");
    error
}

pub fn deadline(ms: Option<u64>) -> Result<Duration, RpcError> {
    let ms = ms.unwrap_or(30_000);
    if !(1..=300_000).contains(&ms) { return Err(rejected("invalid", "deadline_ms must be between 1 and 300000")); }
    Ok(Duration::from_millis(ms))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identity {
    pub provider: String,
    pub instance: String,
    pub reference: String,
    pub incarnation: String,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct IdentityRecord {
    pub identity: Identity,
    pub retired: bool,
}

#[derive(Clone, Serialize)]
struct Persisted {
    next: u64,
    ids: BTreeMap<String, IdentityRecord>,
}

pub struct IdentityMap {
    state: Persisted,
    path: Option<PathBuf>,
    needs_sync: bool,
}

pub struct IdentityUpdate {
    pub id: String,
    pub retired: Vec<String>,
    pub uncertain: Option<RpcError>,
}

impl IdentityMap {
    pub fn open(path: Option<PathBuf>) -> anyhow::Result<Self> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Record { Current(IdentityRecord), Legacy((String, String)) }
        #[derive(Deserialize)]
        struct File { next: u64, ids: BTreeMap<String, Record> }
        let mut state = Persisted { next: 1, ids: BTreeMap::new() };
        if let Some(path) = &path {
            match std::fs::read(path) {
                Ok(bytes) => {
                    let saved: File = serde_json::from_slice(&bytes)?;
                    state.next = saved.next.max(1);
                    for (id, record) in saved.ids {
                        let record = match record {
                            Record::Current(record) => record,
                            // Old two-part identities cannot prove which backend instance they
                            // named. Preserve their ids as tombstones, never silently rebind them.
                            Record::Legacy((provider, reference)) => IdentityRecord {
                                identity: Identity { provider, reference, instance: String::new(), incarnation: String::new() }, retired: true,
                            },
                        };
                        let number: u64 = id.strip_prefix('r').ok_or_else(|| anyhow::anyhow!("invalid resource id {id}"))?.parse()?;
                        state.next = state.next.max(number.checked_add(1).ok_or_else(|| anyhow::anyhow!("resource id exhausted"))?);
                        state.ids.insert(id, record);
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        // Confirm loaded identities (and any legacy migration) before returning durable ids,
        // including a restart following an earlier unconfirmed directory sync.
        let needs_sync = path.is_some();
        Ok(Self { state, path, needs_sync })
    }

    fn commit(&mut self, candidate: Persisted) -> Result<Option<RpcError>, RpcError> {
        let mut uncertain = None;
        if let Some(path) = &self.path {
            let bytes = serde_json::to_vec_pretty(&candidate).map_err(|e| RpcError::new("io", e.to_string()))?;
            if let Err(error) = crate::store::write_atomic(path, &bytes) {
                if !error.renamed {
                    self.needs_sync = true;
                    return Err(RpcError::new("io", format!("could not persist resource identities: {error}")));
                }
                uncertain = Some(RpcError::new("durability_unconfirmed", format!("resource identities adopted after rename; directory sync failed: {error}")));
            }
        }
        self.state = candidate;
        self.needs_sync = uncertain.is_some();
        Ok(uncertain)
    }

    pub fn active(&self, id: &str) -> bool { self.state.ids.get(id).is_some_and(|record| !record.retired) }

    pub fn identify(&mut self, identity: Identity) -> Result<IdentityUpdate, RpcError> {
        if let Some(id) = self.state.ids.iter().find(|(_, r)| !r.retired && r.identity == identity).map(|(id, _)| id.clone()) {
            // A previous rename may have become visible without confirmed directory durability.
            // A repeated observation must not accidentally turn that uncertainty into success.
            let uncertain = if self.needs_sync { self.commit(self.state.clone())? } else { None };
            return Ok(IdentityUpdate { id, retired: Vec::new(), uncertain });
        }
        let mut candidate = self.state.clone();
        let mut retired = Vec::new();
        for (id, record) in &mut candidate.ids {
            if !record.retired && record.identity.provider == identity.provider && record.identity.reference == identity.reference {
                record.retired = true;
                retired.push(id.clone());
            }
        }
        let id = format!("r{}", candidate.next);
        candidate.next = candidate.next.checked_add(1).ok_or_else(|| RpcError::new("io", "resource ids exhausted"))?;
        candidate.ids.insert(id.clone(), IdentityRecord { identity, retired: false });
        let uncertain = self.commit(candidate)?;
        Ok(IdentityUpdate { id, retired, uncertain })
    }

    pub fn retire(&mut self, mut predicate: impl FnMut(&Identity) -> bool) -> Result<(Vec<String>, Option<RpcError>), RpcError> {
        let mut candidate = self.state.clone();
        let mut retired = Vec::new();
        for (id, record) in &mut candidate.ids {
            if !record.retired && predicate(&record.identity) { record.retired = true; retired.push(id.clone()); }
        }
        if retired.is_empty() && !self.needs_sync { return Ok((retired, None)); }
        let uncertain = self.commit(candidate)?;
        Ok((retired, uncertain))
    }
}

pub fn identity(provider: &str, instance: &str, facts: &ResourceFacts) -> Identity {
    Identity { provider: provider.into(), instance: instance.into(), reference: facts.backend_ref.clone(), incarnation: facts.incarnation.clone() }
}
