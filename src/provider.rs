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

pub const MAX_PROVIDERS:usize=1024;
pub const MAX_REGISTRY_METADATA_BYTES:usize=4*1024*1024;
pub const REGISTRATION_FLOOR:usize=1024;
pub fn registration_charge(name:&str,capabilities:&Value,info:Option<&ProviderInfo>,metadata:&Value)->usize {
    crate::resource_admission::encoded_bytes(&json!({"name":name,"capabilities":capabilities,"info":info,"metadata":metadata})).saturating_add(REGISTRATION_FLOOR)
}

pub struct Provider {
    pub registration_bytes:usize,
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
        Self { registration_bytes:0,handle, info: None, source, generation, since: crate::store::now_ms(), error: None, synced: false, metadata: json!({}), health }
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

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
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
    writer: crate::store::DurableWriter,
    by_identity: std::collections::HashMap<Identity,String>,
    by_reference: std::collections::HashMap<(String,String),String>,
    by_provider:std::collections::HashMap<String,std::collections::BTreeSet<String>>,
    #[cfg(test)] writes:usize,
}

pub struct IdentityBatch {
    pub ids:Vec<String>, pub replaced:Vec<String>, pub removed:Vec<String>, pub uncertain:Option<RpcError>,
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
                        // The high-water mark was advanced above even for discarded legacy/tombstone ids.
                        if !record.retired {state.ids.insert(id, record);}
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        // Confirm loaded identities (and any legacy migration) before returning durable ids,
        // including a restart following an earlier unconfirmed directory sync.
        let needs_sync = path.is_some();
        let mut map=Self { state, path, needs_sync, writer:std::sync::Arc::new(crate::store::write_atomic), by_identity:Default::default(),by_reference:Default::default(), by_provider:Default::default(), #[cfg(test)] writes:0 };
        for (id,record) in &map.state.ids {
            map.by_provider.entry(record.identity.provider.clone()).or_default().insert(id.clone());
            if map.by_identity.insert(record.identity.clone(),id.clone()).is_some() || map.by_reference.insert((record.identity.provider.clone(),record.identity.reference.clone()),id.clone()).is_some() {anyhow::bail!("duplicate active resource identity/reference");}
        }
        Ok(map)
    }

    fn commit(&mut self, candidate: Persisted) -> Result<Option<RpcError>, RpcError> {
        let mut uncertain = None;
        if let Some(path) = &self.path {
            #[cfg(test)] {self.writes+=1;}
            let bytes = serde_json::to_vec(&candidate).map_err(|e| RpcError::new("io", e.to_string()))?;
            if let Err(error) = (self.writer)(path, &bytes) {
                if !error.renamed {
                    self.needs_sync = true;
                    return Err(RpcError::new("io", format!("could not persist resource identities: {error}")));
                }
                uncertain = Some(RpcError::new("durability_unconfirmed", format!("resource identities adopted after rename; directory sync failed: {error}")));
            }
        }
        self.by_identity=candidate.ids.iter().map(|(id,r)|(r.identity.clone(),id.clone())).collect();
        self.by_reference=candidate.ids.iter().map(|(id,r)|((r.identity.provider.clone(),r.identity.reference.clone()),id.clone())).collect();
        self.by_provider.clear();for (id,record) in &candidate.ids{self.by_provider.entry(record.identity.provider.clone()).or_default().insert(id.clone());}
        self.state = candidate;
        self.needs_sync = uncertain.is_some();
        Ok(uncertain)
    }

    pub fn has_provider(&self,name:&str)->bool{self.by_provider.contains_key(name)}
    pub fn provider_ids(&self,name:&str)->Vec<String>{self.by_provider.get(name).map(|ids|ids.iter().cloned().collect()).unwrap_or_default()}
    pub fn reference_id(&self,name:&str,reference:&str)->Option<&str>{self.by_reference.get(&(name.into(),reference.into())).map(String::as_str)}
    pub fn retire_provider_except(&mut self,name:&str,instance:&str)->Result<(Vec<String>,Option<RpcError>),RpcError>{
        if !self.needs_sync && self.provider_ids(name).iter().all(|id|self.state.ids[id].identity.instance==instance){return Ok((Vec::new(),None));}
        self.retire(|i|i.provider==name&&i.instance!=instance)
    }
    pub fn sync_provider(&mut self,name:&str,identities:&[Identity])->Result<IdentityBatch,RpcError>{
        if !self.needs_sync && self.by_provider.get(name).map(|ids|ids.len()).unwrap_or(0)==identities.len() {
            if let Some(ids)=identities.iter().map(|i|self.lookup(i).map(String::from)).collect::<Option<Vec<_>>>() {return Ok(IdentityBatch{ids,replaced:Vec::new(),removed:Vec::new(),uncertain:None});}
        }
        let seen=identities.iter().collect::<std::collections::HashSet<_>>();
        self.batch(identities,|i|i.provider==name&&!seen.contains(i))
    }
    pub fn active(&self, id: &str) -> bool { self.state.ids.contains_key(id) }
    pub fn lookup(&self,identity:&Identity)->Option<&str>{self.by_identity.get(identity).map(String::as_str)}

    /// One candidate, one durable replace, then adoption. No candidate id escapes a failed write.
    pub fn batch(&mut self,identities:&[Identity],mut remove:impl FnMut(&Identity)->bool)->Result<IdentityBatch,RpcError>{
        let mut candidate=self.state.clone();let mut exact=self.by_identity.clone();let mut references=self.by_reference.clone();
        let mut result=IdentityBatch{ids:Vec::with_capacity(identities.len()),replaced:Vec::new(),removed:Vec::new(),uncertain:None};
        let mut changed=false;
        for identity in identities {
            if let Some(id)=exact.get(identity){result.ids.push(id.clone());continue;}
            let reference=(identity.provider.clone(),identity.reference.clone());
            if let Some(old)=references.remove(&reference) {if let Some(record)=candidate.ids.remove(&old){exact.remove(&record.identity);}result.replaced.push(old);}
            let id=format!("r{}",candidate.next);candidate.next=candidate.next.checked_add(1).ok_or_else(||RpcError::new("io","resource ids exhausted"))?;
            candidate.ids.insert(id.clone(),IdentityRecord{identity:identity.clone(),retired:false});exact.insert(identity.clone(),id.clone());references.insert(reference,id.clone());result.ids.push(id);changed=true;
        }
        candidate.ids.retain(|id,record|{if remove(&record.identity){result.removed.push(id.clone());changed=true;false}else{true}});
        if changed || self.needs_sync {result.uncertain=self.commit(candidate)?;}
        Ok(result)
    }
    pub fn identify_many(&mut self,identities:&[Identity])->Result<IdentityBatch,RpcError>{
        if !self.needs_sync {if let Some(ids)=identities.iter().map(|i|self.lookup(i).map(String::from)).collect::<Option<Vec<_>>>() {return Ok(IdentityBatch{ids,replaced:Vec::new(),removed:Vec::new(),uncertain:None});}}
        self.batch(identities,|_|false)
    }
    pub fn identify(&mut self, identity: Identity) -> Result<IdentityUpdate, RpcError> {
        if !self.needs_sync {if let Some(id)=self.lookup(&identity){return Ok(IdentityUpdate{id:id.into(),retired:Vec::new(),uncertain:None});}}
        let mut batch=self.batch(&[identity],|_|false)?;
        Ok(IdentityUpdate{id:batch.ids.remove(0),retired:batch.replaced,uncertain:batch.uncertain})
    }
    pub fn retire(&mut self, predicate: impl FnMut(&Identity) -> bool) -> Result<(Vec<String>, Option<RpcError>), RpcError> {
        let batch=self.batch(&[],predicate)?;Ok((batch.removed,batch.uncertain))
    }
    #[cfg(test)] pub(crate) fn writes(&self)->usize{self.writes}
    #[cfg(test)] pub(crate) fn set_writer(&mut self,writer:crate::store::DurableWriter){self.writer=writer;}
}

pub fn identity(provider: &str, instance: &str, facts: &ResourceFacts) -> Identity {
    Identity { provider: provider.into(), instance: instance.into(), reference: facts.backend_ref.clone(), incarnation: facts.incarnation.clone() }
}
