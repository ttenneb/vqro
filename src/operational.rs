//! Generic protected, non-undo operational state. No agent/inbox schema or native effects.
//! The existing committer owns dispatch/adoption; this module owns bounded immutable records
//! and uses the workspace's durable replacement and process-held lifetime fence.
use std::{collections::{BTreeMap,BTreeSet},path::{Path,PathBuf},sync::Arc,io::Read};
use serde::{Serialize,Deserialize};
use serde_json::{Value,json};
use crate::{store::{Entry,Entries,Op,DurableWriter},rpc::RpcError};

pub const DOMAINS:usize=16;
pub const BYTES:usize=2*1024*1024;
pub const RECORDS:usize=1024;
pub const MAX_BYTES:usize=8*1024*1024;
pub const AGGREGATE_BYTES:usize=32*1024*1024;
pub const MIN_BYTES:usize=128*1024;
pub const PAGE_ITEMS:usize=64;
pub const PAGE_BYTES:usize=256*1024;
pub const METADATA_BYTES:usize=64*1024;
pub const RECORD_BYTES:usize=PAGE_BYTES-4096;
const MAX_FILE:usize=MAX_BYTES+METADATA_BYTES;
fn default_bytes()->usize {BYTES}
fn valid_budget(bytes:usize)->bool {(MIN_BYTES..=MAX_BYTES).contains(&bytes)}
fn record_limit(bytes:usize)->usize {((bytes as u64*RECORDS as u64)/BYTES as u64)as usize}
type EncodedEntries=BTreeMap<String,Arc<Box<serde_json::value::RawValue>>>;

pub(crate) fn protected_key(key:&str)->bool {key.starts_with("operational/")}
pub(crate) fn prefix(name:&str)->String {format!("operational/{name}/")}
pub(crate) fn valid_name(name:&str)->bool {
    !name.is_empty()&&name.len()<=64&&name.bytes().next().is_some_and(|b|b.is_ascii_alphanumeric())&&name.bytes().all(|b|b.is_ascii_alphanumeric()||b==b'_'||b==b'-'||b==b'.')
}
fn rejected(kind:&str,message:impl Into<String>)->RpcError {let mut e=RpcError::new(kind,message);e.data.as_mut().unwrap()["outcome"]=json!("rejected");e}

#[derive(Debug,Clone,Serialize,Deserialize)]
pub(crate) struct Attribution {pub author:String,pub on_behalf_of:Option<String>,pub epoch:String,pub generation:u64}
#[derive(Debug,Clone,Serialize,Deserialize)]
pub(crate) struct Transaction {
    pub id:u64,pub attribution:Attribution,pub at_ms:u64,pub note:Option<String>,
    pub changes:Vec<Change>,
}
#[derive(Debug,Clone,Serialize,Deserialize)]
pub(crate) struct Change {pub key:String,pub version:Option<u64>}
#[derive(Serialize,Deserialize)]
struct Persisted {
    format:u32,domain:String,owner:String,#[serde(default="default_bytes")] max_bytes:usize,revision:u64,entries:Arc<Entries>,last:Option<Arc<Transaction>>,
}
#[derive(Clone)]
pub(crate) struct Domain {
    pub name:String,pub owner:String,pub revision:u64,pub entries:Arc<Entries>,pub last:Option<Arc<Transaction>>,
    pub max_bytes:usize,pub max_records:usize,encoded:Arc<EncodedEntries>,capacity_request:Option<Value>,
    path:PathBuf,writer:DurableWriter,lifetime:Option<Arc<std::fs::File>>,pub registered:bool,charged:usize,
}
impl Domain {
    pub fn new(name:String,owner:String,max_bytes:usize,path:PathBuf,writer:DurableWriter,lifetime:Option<Arc<std::fs::File>>)->Self {
        Self {name,owner,revision:0,entries:Arc::new(BTreeMap::new()),last:None,max_bytes,max_records:record_limit(max_bytes),encoded:Arc::new(BTreeMap::new()),capacity_request:None,path,writer,lifetime,registered:false,charged:2+METADATA_BYTES}
    }
    fn document(&self)->Persisted {Persisted {format:1,domain:self.name.clone(),owner:self.owner.clone(),max_bytes:self.max_bytes,revision:self.revision,entries:self.entries.clone(),last:self.last.clone()}}
    pub fn load(path:PathBuf,writer:DurableWriter,lifetime:Option<Arc<std::fs::File>>)->anyhow::Result<Self> {
        let mut bytes=Vec::new();std::fs::File::open(&path)?.take((MAX_FILE+1)as u64).read_to_end(&mut bytes)?;
        anyhow::ensure!(bytes.len()<=MAX_FILE,"operational file exceeds bounded recovery read");
        let p:Persisted=serde_json::from_slice(&bytes)?;
        anyhow::ensure!(p.format==1&&valid_name(&p.domain)&&!p.owner.is_empty()&&p.owner.len()<=128&&valid_budget(p.max_bytes),"invalid operational header");
        anyhow::ensure!(path.file_name().and_then(|n|n.to_str())==Some(format!("{}.json",p.domain).as_str()),"domain/file mismatch");
        let start=prefix(&p.domain);
        for (key,entry) in p.entries.iter() {anyhow::ensure!(key.starts_with(&start)&&key.len()>start.len()&&key.len()<=512&&!entry.quarantined()&&entry.version>0&&entry.version<=p.revision&&crate::limits::encoded_bytes(&(key,entry))<=RECORD_BYTES,"invalid/unsafe operational record; original retained");}
        if let Some(last)=&p.last {anyhow::ensure!(last.id==p.revision&&crate::limits::encoded_bytes(last)<METADATA_BYTES-4096,"invalid operational audit metadata");}
        let charged=crate::limits::encoded_bytes(p.entries.as_ref()).saturating_add(METADATA_BYTES);
        anyhow::ensure!(charged<=p.max_bytes&&p.entries.len()<=record_limit(p.max_bytes),"operational file exceeds its declared allocation; original retained");
        let mut encoded=BTreeMap::new();for(key,entry)in p.entries.iter(){encoded.insert(key.clone(),Arc::new(serde_json::value::to_raw_value(entry)?));}
        Ok(Self {name:p.domain,owner:p.owner,revision:p.revision,max_bytes:p.max_bytes,max_records:record_limit(p.max_bytes),encoded:Arc::new(encoded),capacity_request:None,entries:p.entries,last:p.last,path,writer,lifetime,registered:true,charged})
    }
    pub fn release_state(&mut self) {self.lifetime=None;}
    pub fn set_writer(&mut self,writer:DurableWriter) {self.writer=writer;}
    pub fn used(&self)->usize {self.charged}
    pub fn metadata(&self)->Value {json!({"domain":self.name,"owner":self.owner,"revision":self.revision,"prefix":prefix(&self.name),"records":self.entries.len(),"bytes_used":self.used(),"limits":{"bytes":self.max_bytes,"records":self.max_records,"normal_bytes":self.max_bytes-self.max_bytes.div_ceil(10),"normal_records":self.max_records-self.max_records.div_ceil(10),"metadata_reserve":METADATA_BYTES,"record_bytes":RECORD_BYTES,"batch_bytes":PAGE_BYTES,"batch_items":PAGE_ITEMS,"max_domain_bytes":MAX_BYTES,"min_domain_bytes":MIN_BYTES,"page_bytes":PAGE_BYTES,"page_items":PAGE_ITEMS},"available":true,"capacity_request":self.capacity_request})}
    pub fn register(&mut self,attribution:Attribution)->Result<Arc<Transaction>,RpcError> {
        if !self.registered&&self.path.try_exists().map_err(|e|rejected("io",e.to_string()))? {return Err(rejected("unavailable","untracked operational file exists; inspect, do not overwrite"));}
        // Explicit re-registration confirms the complete current file again; an earlier
        // uncertain acknowledgement must not be upgraded to durable merely by a lookup.
        let id=if self.registered {self.revision.checked_add(1).ok_or_else(||rejected("unavailable","operational revision exhausted"))?}else{0};
        let transaction=Arc::new(Transaction {id,attribution,at_ms:crate::store::now_ms(),note:Some(match &self.capacity_request {Some(request)=>format!("operational.register capacity {request}"),None=>"operational.register".into()}),changes:Vec::new()});
        let mut candidate=self.document();candidate.revision=id;candidate.last=Some(transaction.clone());self.persist(candidate,transaction,self.charged,self.encoded.clone())
    }
    pub fn apply(&mut self,attribution:Attribution,note:Option<String>,ops:Vec<Op>,control:bool)->Result<Arc<Transaction>,RpcError> {
        if !self.registered {return Err(rejected("unavailable","operational domain must be registered before writes"));}
        crate::store::validate_note(note.as_deref())?;
        if ops.is_empty()||ops.len()>PAGE_ITEMS {return Err(rejected("too_large","operational batch requires1..64 operations"));}
        if crate::limits::encoded_bytes(&ops)>PAGE_BYTES {return Err(crate::limits::too_large("operational batch",crate::limits::encoded_bytes(&ops),PAGE_BYTES));}
        let revision=self.revision.checked_add(1).ok_or_else(||rejected("unavailable","operational revision exhausted"))?;
        let start=prefix(&self.name);let mut keys=BTreeSet::new();let mut entries=self.entries.as_ref().clone();let mut encoded=self.encoded.as_ref().clone();let mut changes=Vec::new();
        // Exact compact-map accounting, touching only changed immutable records. Each pair
        // contributes encoded-key + colon + raw entry + comma, minus the final comma.
        let mut sum=self.charged-METADATA_BYTES-2+usize::from(!entries.is_empty());
        for op in ops {
            let (key,expect)=match &op {Op::Set{key,expect,..}|Op::Delete{key,expect}=>(key.clone(),*expect)};
            if !key.starts_with(&start)||key.len()==start.len()||key.len()>512 {return Err(rejected("forbidden","mixed/wrong-domain transaction; operational keys must use their registered prefix"));}
            if !keys.insert(key.clone()) {return Err(rejected("invalid","duplicate operational key in atomic batch"));}
            let version=entries.get(&key).map(|e|e.version).unwrap_or(0);
            if expect.is_some_and(|want|want!=version) {return Err(rejected("conflict",format!("{key} is at version {version}, expected {}",expect.unwrap())));}
            let key_bytes=crate::limits::encoded_bytes(&key);
            if let Some(old)=encoded.remove(&key) {sum-=key_bytes+2+old.get().len();}
            match op {
                Op::Set{value,..}=>{
                    crate::limits::json_depth(&value,crate::limits::JSON_DEPTH)?;
                    let entry=Arc::new(Entry::new(revision,value));let raw=Arc::new(serde_json::value::to_raw_value(&entry).map_err(|e|rejected("invalid",e.to_string()))?);let bytes=key_bytes+raw.get().len()+3;
                    if bytes>RECORD_BYTES {return Err(crate::limits::too_large("operational record",bytes,RECORD_BYTES));}
                    sum+=key_bytes+2+raw.get().len();encoded.insert(key.clone(),raw);
                    entries.insert(key.clone(),entry);changes.push(Change {key,version:Some(revision)});
                },
                Op::Delete{..}=>{entries.remove(&key);changes.push(Change {key,version:None});},
            }
        }
        let bytes=METADATA_BYTES+2+sum-usize::from(!entries.is_empty());
        let count_limit=if control {self.max_records}else{self.max_records-self.max_records.div_ceil(10)};let byte_limit=if control {self.max_bytes}else{self.max_bytes-self.max_bytes.div_ceil(10)};
        if (entries.len()>count_limit&&entries.len()>self.entries.len())||(bytes>byte_limit&&bytes>self.used()) {return Err(rejected("quota",format!("operational allocation full ({} records, {bytes} charged bytes); control/recovery reserve retained",entries.len())));}
        let transaction=Arc::new(Transaction {id:revision,attribution,at_ms:crate::store::now_ms(),note,changes});
        if crate::limits::encoded_bytes(&transaction)>METADATA_BYTES-4096 {return Err(rejected("too_large","operational audit exceeds fixed metadata reservation"));}
        let candidate=Persisted {format:1,domain:self.name.clone(),owner:self.owner.clone(),max_bytes:self.max_bytes,revision,entries:Arc::new(entries),last:Some(transaction.clone())};
        self.persist(candidate,transaction,bytes,Arc::new(encoded))
    }
    fn persist(&mut self,candidate:Persisted,transaction:Arc<Transaction>,charged:usize,encoded:Arc<EncodedEntries>)->Result<Arc<Transaction>,RpcError> {
        #[derive(Serialize)] struct Encoded<'a> {format:u32,domain:&'a str,owner:&'a str,max_bytes:usize,revision:u64,entries:&'a EncodedEntries,last:&'a Option<Arc<Transaction>>}
        let document=Encoded {format:1,domain:&candidate.domain,owner:&candidate.owner,max_bytes:candidate.max_bytes,revision:candidate.revision,entries:&encoded,last:&candidate.last};
        let bytes=serde_json::to_vec(&document).map_err(|e|rejected("io",e.to_string()))?;
        // Fixed audit reservation ensures deletion/recovery does not unexpectedly need new space.
        if bytes.len()>self.max_bytes {return Err(rejected("quota","operational file exceeds its complete allocation"));}
        let error=match (self.writer)(&self.path,&bytes) {
            Ok(())=>None,
            Err(error) if !error.renamed=>return Err(rejected("io",format!("operational write not applied: {error}"))),
            Err(error)=>{let mut e=RpcError::new("durability_unconfirmed",format!("operational transaction {} adopted after rename; {error}; do not replay",transaction.id));e.data.as_mut().unwrap()["txn"]=json!(transaction.id);e.data.as_mut().unwrap()["domain"]=json!(self.name);Some(e)},
        };
        self.revision=candidate.revision;self.entries=candidate.entries;self.last=candidate.last;self.registered=true;self.charged=charged;self.encoded=encoded;
        match error {Some(e)=>Err(e),None=>Ok(transaction)}
    }
}

pub(crate) struct Registry {
    pub domains:BTreeMap<String,Domain>,failures:BTreeMap<String,String>,owners:BTreeMap<String,String>,
    dir:Option<PathBuf>,closed:Option<String>,writer:DurableWriter,lifetime:Option<std::sync::Weak<std::fs::File>>,
}
impl Registry {
    pub fn open(state:Option<&Path>,parent:&crate::store::Store)->Self {
        let (writer,lease)=parent.durable_context();let lifetime=lease.as_ref().map(Arc::downgrade);
        let mut result=Self {domains:BTreeMap::new(),failures:BTreeMap::new(),owners:BTreeMap::new(),dir:state.map(|s|s.join("operational")),closed:None,writer,lifetime};
        let Some(dir)=result.dir.as_ref() else{return result;};
        let files=match std::fs::read_dir(dir) {Ok(files)=>files,Err(e) if e.kind()==std::io::ErrorKind::NotFound=>return result,Err(e)=>{result.closed=Some(format!("operational registry unreadable: {e}"));return result;}};
        for (n,file) in files.enumerate() {
            if n>=64 {result.closed=Some("operational directory scan bound exceeded; inspect files".into());break;}
            let file=match file {Ok(f)=>f,Err(e)=>{result.closed=Some(e.to_string());break;}};
            let path=file.path();if path.extension().and_then(|e|e.to_str())!=Some("json") {continue;}
            if result.domains.len()+result.failures.len()>=DOMAINS {result.closed=Some("operational domain capacity exceeded on disk; no implicit eviction".into());break;}
            let name=path.file_stem().unwrap().to_string_lossy().into_owned();
            match Domain::load(path,result.writer.clone(),None) {
                Ok(domain)=>{
                    if result.domains.values().map(|domain|domain.max_bytes).sum::<usize>().saturating_add(domain.max_bytes)>AGGREGATE_BYTES {result.closed=Some("operational aggregate capacity exceeds32MiB on disk; read-only recovery required".into());result.failures.insert(name,"declared capacity exceeds aggregate; original retained".into());break;}
                    if result.owners.insert(domain.owner.clone(),name.clone()).is_some() {result.closed=Some("multiple operational domains for one owner; recovery required".into());}
                    result.domains.insert(name,domain);
                },
                Err(e)=>{result.closed=Some("operational ownership/authority file needs recovery; registrations and writes fail closed".into());result.failures.insert(name,format!("{e}; original retained, domain unavailable"));},
            }
        }
        result
    }
    pub fn get(&self,name:&str)->Result<&Domain,RpcError> {
        if let Some(error)=self.failures.get(name) {return Err(rejected("unavailable",error));}
        self.domains.get(name).ok_or_else(||rejected("not_found","operational domain not registered"))
    }
    pub fn prepare(&self,name:&str,owner:&str,bytes:Option<usize>)->Result<Domain,RpcError> {
        if !valid_name(name)||owner.is_empty()||owner.len()>128 {return Err(rejected("invalid","invalid domain/owner name"));}
        if let Some(error)=&self.closed {return Err(rejected("unavailable",error));}
        let lifetime=Some(self.lifetime.as_ref().and_then(std::sync::Weak::upgrade).ok_or_else(||rejected("unavailable","operational state needs the workspace process lifetime fence"))?);
        if let Some(domain)=self.domains.get(name) {if domain.owner!=owner {return Err(rejected("forbidden","operational domain belongs to another extension"));}let mut domain=domain.clone();domain.lifetime=lifetime;
            if let Some(bytes)=bytes {
                if !valid_budget(bytes) {return Err(rejected("invalid","operational bytes must be128KiB..8MiB"));}
                if bytes>domain.max_bytes {
                    let capacity=self.domains.values().map(|domain|domain.max_bytes).sum::<usize>()-domain.max_bytes+bytes;
                    if capacity>AGGREGATE_BYTES {return Err(rejected("quota","operational growth exceeds32MiB aggregate"));}
                    domain.max_bytes=bytes;domain.max_records=record_limit(bytes);
                    domain.capacity_request=Some(json!({"requested":bytes,"applied":bytes,"outcome":"grown"}));
                }else{domain.capacity_request=Some(json!({"requested":bytes,"applied":domain.max_bytes,"outcome":if bytes<domain.max_bytes {"shrink_refused"}else{"unchanged"}}));}
            }
            return Ok(domain);}
        if self.failures.contains_key(name) {return Err(rejected("unavailable","operational file requires recovery; never recreate empty authority"));}
        if self.owners.contains_key(owner) {return Err(rejected("conflict","owning extension already has an operational domain"));}
        if self.domains.len()+self.failures.len()>=DOMAINS {return Err(rejected("quota","operational domain aggregate bound16/32MiB reached"));}
        let bytes=bytes.unwrap_or(BYTES);if !valid_budget(bytes) {return Err(rejected("invalid","operational bytes must be128KiB..8MiB"));}
        if self.domains.values().map(|domain|domain.max_bytes).sum::<usize>().saturating_add(bytes)>AGGREGATE_BYTES {return Err(rejected("quota","operational declared capacity exceeds32MiB aggregate"));}
        let dir=self.dir.as_ref().ok_or_else(||rejected("unavailable","persistent operational state unavailable"))?;
        Ok(Domain::new(name.to_owned(),owner.to_owned(),bytes,dir.join(format!("{name}.json")),self.writer.clone(),lifetime))
    }
    pub fn adopt(&mut self,mut domain:Domain)->Option<Domain> {domain.release_state();domain.capacity_request=None;self.owners.insert(domain.owner.clone(),domain.name.clone());self.domains.insert(domain.name.clone(),domain)}
    pub fn summaries(&self)->Value {
        let mut domains=self.domains.values().map(Domain::metadata).collect::<Vec<_>>();
        domains.extend(self.failures.iter().map(|(name,error)|json!({"domain":name,"available":false,"error":error})));
        json!({"domains":domains,"limits":{"domains":DOMAINS,"bytes":AGGREGATE_BYTES},"capacity_used":self.domains.values().map(|domain|domain.max_bytes).sum::<usize>(),"read_only":self.closed.is_some(),"error":self.closed})
    }
    pub fn release_state(&mut self) {self.lifetime=None;for domain in self.domains.values_mut(){domain.release_state();}}
    pub fn set_writer(&mut self,writer:DurableWriter) {self.writer=writer.clone();for domain in self.domains.values_mut(){domain.set_writer(writer.clone());}}
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::AtomicWriteError;
    fn attribution()->Attribution {Attribution {author:"ext:test".into(),on_behalf_of:Some("client:invoker".into()),epoch:"boot-test".into(),generation:7}}
    fn set(key:&str,value:Value,expect:Option<u64>)->Op {Op::Set {key:key.into(),value,expect}}
    fn fixture()->(tempfile::TempDir,crate::store::Store,Registry) {
        let root=tempfile::tempdir().unwrap();let parent=crate::store::Store::open_locked(root.path().join("workspace.json")).unwrap();
        let registry=Registry::open(Some(root.path()),&parent);(root,parent,registry)
    }
    #[test]
    fn cached_encoding_and_delta_budget_are_exact_through_cas_replacement_delete_restart() {
        let(root,_parent,registry)=fixture();let mut domain=registry.prepare("test","owner",None).unwrap();domain.register(attribution()).unwrap();
        for n in 0..100 {
            let key=format!("operational/test/κ\\\"-{0}",n%7);
            let expect=domain.entries.get(&key).map(|e|e.version).unwrap_or(0);
            let before=domain.document().revision;
            domain.apply(attribution(),None,vec![set(&key,json!({"n":n,"text":"quoted \" and \\ [ }\n"}),Some(expect))],true).unwrap();
            assert_eq!(domain.revision,before+1);
            assert_eq!(domain.used(),METADATA_BYTES+crate::limits::encoded_bytes(domain.entries.as_ref()));
            let disk:Value=serde_json::from_slice(&std::fs::read(&domain.path).unwrap()).unwrap();assert_eq!(disk["entries"],json!(domain.entries));
            if n%3==0 {domain.apply(attribution(),None,vec![Op::Delete {key,expect:Some(domain.revision)}],true).unwrap();}
        }
        let loaded=Domain::load(domain.path.clone(),Arc::new(crate::store::write_atomic),None).unwrap();assert_eq!(json!(loaded.entries),json!(domain.entries));assert_eq!(loaded.used(),domain.used());
        assert!(root.path().join("operational/test.json").is_file());
        assert_eq!(domain.last.as_ref().unwrap().attribution.generation,7);
        assert_eq!(domain.apply(attribution(),None,vec![set("doc/ordinary",json!(1),None)],true).unwrap_err().kind(),"forbidden");
    }
    #[test]
    fn normal_allocation_retains_control_reserve_and_hard_limit() {
        let(_root,_parent,registry)=fixture();let mut domain=registry.prepare("test","owner",None).unwrap();domain.register(attribution()).unwrap();
        for n in 0..11 {domain.apply(attribution(),None,vec![set(&format!("operational/test/{n}"),json!("x".repeat(160_000)),Some(0))],false).unwrap();}
        let before=domain.revision;
        assert_eq!(domain.apply(attribution(),None,vec![set("operational/test/control",json!("x".repeat(160_000)),Some(0))],false).unwrap_err().kind(),"quota");assert_eq!(domain.revision,before);
        domain.apply(attribution(),None,vec![set("operational/test/control",json!("x".repeat(160_000)),Some(0))],true).unwrap();
        assert_eq!(domain.apply(attribution(),None,vec![set("operational/test/overflow",json!("x".repeat(160_000)),Some(0))],true).unwrap_err().kind(),"quota");
        domain.apply(attribution(),None,vec![Op::Delete {key:"operational/test/control".into(),expect:None}],true).unwrap();
        assert!(domain.used()<=domain.max_bytes);
    }
    #[test]
    fn record_count_reserve_and_declared_capacity_bounds_are_independent() {
        let(_root,_parent,mut registry)=fixture();
        for n in 0..4 {let mut domain=registry.prepare(&format!("d{n}"),&format!("owner{n}"),Some(MAX_BYTES)).unwrap();domain.register(attribution()).unwrap();registry.adopt(domain);}
        assert_eq!(registry.prepare("overflow","fifth",None).err().unwrap().kind(),"quota");
        assert_eq!(registry.prepare("d0","stranger",None).err().unwrap().kind(),"forbidden");
        assert_eq!(registry.prepare("other","owner0",None).err().unwrap().kind(),"conflict");
        let shrink=registry.prepare("d0","owner0",Some(BYTES)).unwrap();assert_eq!(shrink.max_bytes,MAX_BYTES);assert_eq!(shrink.metadata()["capacity_request"]["outcome"],"shrink_refused");
        let mut domain=registry.prepare("d0","owner0",None).unwrap();assert_eq!(domain.max_records,4096);
        assert_eq!(domain.apply(attribution(),None,vec![set("operational/d0/a",json!(1),None),set("operational/d1/b",json!(2),None)],true).unwrap_err().kind(),"forbidden");assert!(domain.entries.is_empty());
    }
    #[test]
    fn growth_is_durable_shrink_is_explicitly_refused_and_count_reserve_is_real() {
        let(root,parent,mut registry)=fixture();let mut domain=registry.prepare("test","owner",None).unwrap();domain.register(attribution()).unwrap();registry.adopt(domain);
        let mut grown=registry.prepare("test","owner",Some(MAX_BYTES)).unwrap();assert_eq!(grown.metadata()["capacity_request"]["outcome"],"grown");grown.register(attribution()).unwrap();registry.adopt(grown);
        let recovered=Registry::open(Some(root.path()),&parent);assert_eq!(recovered.get("test").unwrap().max_bytes,MAX_BYTES);
        let shrink=recovered.prepare("test","owner",Some(BYTES)).unwrap();assert_eq!(shrink.max_records,4096);assert_eq!(shrink.metadata()["capacity_request"]["outcome"],"shrink_refused");
        let mut small=registry.prepare("count","other",Some(MIN_BYTES)).unwrap();small.register(attribution()).unwrap();assert_eq!(small.max_records,64);
        let normal=small.max_records-small.max_records.div_ceil(10);
        small.apply(attribution(),None,(0..normal).map(|n|set(&format!("operational/count/{n}"),json!(null),None)).collect(),false).unwrap();
        assert_eq!(small.apply(attribution(),None,vec![set("operational/count/reserve",json!(null),None)],false).unwrap_err().kind(),"quota");
        small.apply(attribution(),None,(normal..64).map(|n|set(&format!("operational/count/{n}"),json!(null),None)).collect(),true).unwrap();
        assert_eq!(small.apply(attribution(),None,vec![set("operational/count/full",json!(null),None)],true).unwrap_err().kind(),"quota");
    }
    #[test]
    fn uncertain_and_clean_failures_preserve_exact_adoption_and_audit() {
        let(_root,_parent,registry)=fixture();let mut domain=registry.prepare("test","owner",None).unwrap();domain.register(attribution()).unwrap();
        domain.set_writer(Arc::new(|_,_|Err(std::io::Error::other("pre-rename").into())));
        assert_eq!(domain.apply(attribution(),None,vec![set("operational/test/a",json!(1),None)],true).unwrap_err().kind(),"io");assert!(domain.entries.is_empty());assert_eq!(domain.revision,0);
        domain.set_writer(Arc::new(|path,bytes| {crate::store::write_atomic(path,bytes)?;let mut e=AtomicWriteError::from(std::io::Error::other("missing directory confirmation"));e.renamed=true;Err(e)}));
        let error=domain.apply(attribution(),Some("claim".into()),vec![set("operational/test/a",json!({"phase":"claimed"}),None)],true).unwrap_err();
        assert_eq!(error.kind(),"durability_unconfirmed");assert_eq!(error.data.unwrap()["txn"],domain.revision);assert_eq!(domain.last.as_ref().unwrap().note.as_deref(),Some("claim"));assert_eq!(domain.entries["operational/test/a"].value["phase"],"claimed");
        domain.set_writer(Arc::new(crate::store::write_atomic));let old=domain.revision;domain.register(attribution()).unwrap();assert_eq!(domain.revision,old+1);assert!(domain.entries.contains_key("operational/test/a"));
    }
    #[test]
    fn metadata_does_not_pin_shutdown_but_a_dispatched_writer_does() {
        let(root,mut parent,mut registry)=fixture();let mut domain=registry.prepare("test","owner",None).unwrap();domain.register(attribution()).unwrap();registry.adopt(domain);
        let writer=registry.prepare("test","owner",None).unwrap();parent.release_state();
        assert!(crate::store::Store::open_locked(root.path().join("workspace.json")).is_err());drop(writer);
        let replacement=crate::store::Store::open_locked(root.path().join("workspace.json")).unwrap();drop(replacement);
        assert!(registry.get("test").is_ok());assert_eq!(registry.prepare("test","owner",None).err().unwrap().kind(),"unavailable");
    }
    #[test]
    fn corrupt_authority_is_retained_and_never_recreated_empty() {
        let(root,parent,mut registry)=fixture();let mut domain=registry.prepare("test","owner",None).unwrap();domain.register(attribution()).unwrap();registry.adopt(domain);
        let path=root.path().join("operational/test.json");std::fs::write(&path,b"{ broken authority").unwrap();
        let recovered=Registry::open(Some(root.path()),&parent);assert_eq!(recovered.get("test").err().unwrap().kind(),"unavailable");assert!(recovered.prepare("test","owner",None).is_err());assert_eq!(std::fs::read(&path).unwrap(),b"{ broken authority");
    }
}
