//! Connection-known principals and exact, cooperative delegation. Not a security boundary.
//! Coordination rights belong to extensions; raw workspace/resource/recovery stays ungated.
use serde::{Deserialize,Serialize};
use serde_json::{json,Value};
use crate::{rpc::RpcError,store::{Store,Op}};

pub const OWNER:&str="owner";
pub const UNDECLARED:&str="undeclared";
pub const OWNER_KEY:&str="doc/principals/owner";
pub const DELEGATIONS:&str="doc/principals/delegations/";
const ID_BYTES:usize=256;
const TABLE_BYTES:usize=128*1024;
const TABLE_ROWS:usize=1024;

#[derive(Clone,Debug,Default,Serialize,Deserialize)]
pub struct Attribution {
    #[serde(default,skip_serializing_if="Option::is_none")]
    pub actor:Option<String>,
    #[serde(default,skip_serializing_if="Option::is_none")]
    pub principal:Option<String>,
    #[serde(default,skip_serializing_if="Option::is_none")]
    pub initiator:Option<String>,
    #[serde(default,skip_serializing_if="Option::is_none")]
    pub label:Option<String>,
    #[serde(default,flatten)]
    pub causality:crate::causality::Causality,
    #[serde(skip)]
    pub(crate) authority:Option<Authority>,
}
#[derive(Clone)]
pub(crate) struct Authority {pub connection:u64,pub invocation:Option<String>,pub target:Option<String>}
impl std::fmt::Debug for Authority {
    fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result {f.debug_struct("Authority").field("connection",&self.connection).field("has_invocation",&self.invocation.is_some()).field("target",&self.target).finish()}
}
impl Attribution {
    pub fn for_actor(actor:&str,params:&mut Value)->Result<Self,RpcError> {
        let label=match params.get("label") {
            None=>None,
            Some(Value::String(label)) if label.len()<=ID_BYTES && !label.chars().any(char::is_control)=>Some(label.clone()),
            Some(_)=>return Err(RpcError::invalid("label must be a bounded string without control characters")),
        };
        if let Some(object)=params.as_object_mut(){object.remove("label");}
        Ok(Self {actor:Some(actor.into()),principal:Some(actor.into()),initiator:None,label,causality:crate::causality::Causality::take(params)?,authority:None})
    }
    pub(crate) fn recorded(mut self)->Self {self.authority=None;self}
    pub fn effective(&self)->Option<&str> {self.actor.as_ref()?;self.principal.as_deref().filter(|id|*id!=UNDECLARED)}
    pub fn actor(&self)->Option<&str> {self.actor.as_deref().filter(|id|*id!=UNDECLARED)}
    pub fn forwarded(mut self,principal:&str,initiator:Option<&str>)->Self {
        self.principal=Some(principal.into());self.initiator=Some(initiator.unwrap_or(principal).into());self
    }
}
#[derive(Clone,Debug,Default,Serialize,Deserialize)]
pub struct CallAuthor {
    #[serde(default)] pub author:String,
    #[serde(flatten)] pub attribution:Attribution,
}
impl CallAuthor {pub fn effective(&self)->Option<&str>{self.attribution.effective()}}
/// Same-user callers can deliberately configure this. A per-call display label cannot bind it.
#[derive(Clone,Debug,Serialize,Deserialize)]
#[serde(tag="kind",rename_all="snake_case",deny_unknown_fields)]
pub enum Identity {Owner,Principal {principal:String}}

pub fn validate(id:&str)->Result<(),RpcError> {
    if id.is_empty() || id.len()>ID_BYTES || id.chars().any(|c|c.is_control()||c.is_whitespace()) || id==UNDECLARED {
        return Err(RpcError::invalid("principal must be 1..=256 bytes without whitespace/control characters; undeclared is reserved"));
    }
    Ok(())
}
pub fn owner(store:&Store)->Result<String,RpcError> {
    let principal=match store.get(OWNER_KEY) {None=>OWNER,Some(entry)=>entry.value.as_str().ok_or_else(||RpcError::invalid("workspace owner must be a principal string"))?};
    validate(principal)?;Ok(principal.into())
}
pub fn identity(store:&Store,identity:Option<&Identity>,extension:Option<&str>)->Result<String,RpcError> {
    let resolved=if let Some(extension)=extension {format!("extension:{extension}")} else {
        match identity {
            None=>return Ok(UNDECLARED.into()),Some(Identity::Owner)=>owner(store)?,
            Some(Identity::Principal {principal})=>{
                validate(principal)?;
                if principal=="system" || ["extension:","host:","system:"].iter().any(|prefix|principal.starts_with(*prefix)) {
                    return Err(RpcError::invalid("extension:, host: and system principals are core-derived, not caller-assertable"));
                }
                principal.clone()
            }
        }
    };
    validate(&resolved)?;Ok(resolved)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Delegation {
    from:String,to:String,
    #[serde(default)] until_ms:Option<u64>,
    #[serde(default)] note:Option<String>,
}
fn row(value:&Value)->Result<Delegation,RpcError> {
    let row:Delegation=serde_json::from_value(value.clone()).map_err(|error|RpcError::invalid(format!("invalid delegation row: {error}")))?;
    validate(&row.from)?;validate(&row.to)?;crate::store::validate_note(row.note.as_deref())?;Ok(row)
}
pub fn represent(store:&Store,mut attribution:Attribution,target:Option<&str>)->Result<Attribution,RpcError> {
    let Some(target)=target else{return Ok(attribution)};validate(target)?;
    let actor=attribution.actor().ok_or_else(||crate::provider::rejected("forbidden","delegation requires a connection-known principal"))?;
    let mut allowed=target==actor;let mut count=0usize;let mut bytes=0usize;
    // Always match the connection actor, never a previously represented principal: no chains.
    for(key,entry)in store.entries().range(DELEGATIONS.to_string()..).take_while(|(key,_)|key.starts_with(DELEGATIONS)) {
        count+=1;bytes=bytes.saturating_add(crate::limits::encoded_bytes(&(key,&entry.value)));
        if count>TABLE_ROWS{return Err(crate::provider::rejected("overloaded","delegation row limit"));}
        if bytes>TABLE_BYTES{return Err(crate::limits::too_large("delegation table",bytes,TABLE_BYTES));}
        let grant=row(&entry.value)?;
        allowed|=grant.from==target && grant.to==actor && grant.until_ms.is_none_or(|until|crate::store::now_ms()<until);
    }
    if !allowed{return Err(crate::provider::rejected("forbidden","no current exact delegation from the represented principal to this actor"));}
    if attribution.initiator.is_none(){attribution.initiator=attribution.principal.clone();}
    attribution.principal=Some(target.into());Ok(attribution)
}
pub(crate) fn resolve(core:&crate::core::Core,connection:u64,base:Attribution,invocation:Option<String>,target:Option<String>)->Result<crate::invocation::Context,RpcError> {
    let mut context=crate::invocation::authorize(&core.invocations,connection,invocation.clone())?;
    let base=if let Some(principal)=context.principal.as_deref(){base.forwarded(principal,context.initiator.as_deref())}else{base};
    context.attribution=represent(&core.store,base,target.as_deref())?;
    context.attribution.authority=Some(Authority{connection,invocation,target});
    Ok(context)
}
pub(crate) fn revalidate(core:&crate::core::Core,attribution:&Attribution)->Result<Attribution,RpcError> {
    let Some(authority)=&attribution.authority else{return Ok(attribution.clone())};
    let base=Attribution{actor:attribution.actor.clone(),principal:attribution.actor.clone(),initiator:None,label:attribution.label.clone(),causality:attribution.causality.clone(),authority:None};
    Ok(resolve(core,authority.connection,base,authority.invocation.clone(),authority.target.clone())?.attribution)
}
pub fn protected(key:&str)->bool {key==OWNER_KEY || key.starts_with(DELEGATIONS)}
/// Caller owns the serialized prepare boundary: authorization sees preceding adopted commits.
pub fn validate_writes(store:&Store,attribution:&Attribution,ops:&[Op])->Result<(),RpcError> {
    let keys=ops.iter().filter(|op|protected(match op{Op::Set{key,..}|Op::Delete{key,..}=>key})).collect::<Vec<_>>();
    if keys.is_empty(){return Ok(())}
    // Standing authority cannot be minted, extended or transferred through representation.
    let actor=attribution.actor().ok_or_else(||crate::provider::rejected("forbidden","principal configuration needs a connection-known actor"))?;
    let owner=owner(store)?;let is_owner=actor==owner;
    let mut staged=std::collections::BTreeMap::<String,Option<Value>>::new();
    for op in keys {
        let(key,next)=match op{Op::Set{key,value,..}=>(key,Some(value)),Op::Delete{key,..}=>(key,None)};
        let previous=staged.get(key).map(|value|value.as_ref()).unwrap_or_else(||store.get(key).map(|entry|&entry.value));
        if key==OWNER_KEY {
            if !is_owner{return Err(crate::provider::rejected("forbidden","only the workspace owner may change its principal"));}
            if let Some(value)=next {validate(value.as_str().ok_or_else(||RpcError::invalid("workspace owner must be a principal string"))?)?;}
        } else {
            if key.len()==DELEGATIONS.len(){return Err(RpcError::invalid("delegation key needs an id"));}
            if !is_owner {
                for value in previous.into_iter().chain(next) {
                    if row(value)?.from!=actor{return Err(crate::provider::rejected("forbidden","delegation rows are writable only by from or the workspace owner"));}
                }
            }
            if let Some(value)=next {row(value)?;}
        }
        staged.insert(key.clone(),next.cloned());
    }
    // Validate the final table's aggregate bounds before any IO/adoption.
    let mut count=0usize;let mut bytes=0usize;
    for(key,entry)in store.entries().range(DELEGATIONS.to_string()..).take_while(|(key,_)|key.starts_with(DELEGATIONS)) {
        if staged.contains_key(key){continue;}count+=1;bytes=bytes.saturating_add(crate::limits::encoded_bytes(&(key,&entry.value)));
    }
    for(key,value)in staged.iter().filter(|(key,_)|key.starts_with(DELEGATIONS)) {
        if let Some(value)=value {count+=1;bytes=bytes.saturating_add(crate::limits::encoded_bytes(&(key,value)));}
    }
    if count>TABLE_ROWS{return Err(crate::provider::rejected("overloaded","delegation row limit"));}
    if bytes>TABLE_BYTES{return Err(crate::limits::too_large("delegation table",bytes,TABLE_BYTES));}
    Ok(())
}
pub fn validate_revert(store:&Store,attribution:&Attribution,txn:u64)->Result<(),RpcError> {
    let Some(txn)=store.transaction(txn)else{return Ok(())};
    let ops=txn.changes.iter().filter(|change|protected(&change.key)).map(|change|match &change.before {
        Some(entry)=>Op::Set{key:change.key.clone(),value:entry.value.clone(),expect:None},
        None=>Op::Delete{key:change.key.clone(),expect:None},
    }).collect::<Vec<_>>();
    validate_writes(store,attribution,&ops)
}
/// Called with the startup state lock held; system work is visible but not an owner-u target.
pub fn bootstrap(store:&mut Store)->Result<(),RpcError> {
    if store.get(OWNER_KEY).is_some() && owner(store).is_ok(){return Ok(())}
    let mut nonce=[0u8;16];getrandom::fill(&mut nonce).map_err(|error|RpcError::new("io",format!("owner bootstrap nonce: {error}")))?;
    let author=format!("system:{}",nonce.iter().map(|b|format!("{b:02x}")).collect::<String>());
    let attribution=Attribution{actor:Some("system".into()),principal:Some("system".into()),initiator:None,label:None,causality:Default::default(),authority:None};
    match store.apply_context(&author,None,attribution,Some("initialize workspace owner (prior value retained in history)".into()),vec![Op::Set{key:OWNER_KEY.into(),value:json!(OWNER),expect:None}]) {
        Ok(_)=>Ok(()),
        Err(error)if error.kind()=="durability_unconfirmed"=>{eprintln!("vqro: owner bootstrap adopted, durability unconfirmed; do not replay: {error}");Ok(())},
        Err(error)if error.kind()=="too_large"=>{eprintln!("vqro: owner bootstrap could not grow legacy state; default owner retained for raw recovery: {error}");Ok(())},
        Err(error)=>Err(error),
    }
}
