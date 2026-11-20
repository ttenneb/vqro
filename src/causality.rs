//! Explicit caller links only. Shape bounds, never evidence inference or authority.
use serde::{Deserialize,Serialize};
use serde_json::Value;
use crate::rpc::RpcError;
const STRING_BYTES:usize=256;
const CAUSE_BYTES:usize=4096;
const CAUSE_DEPTH:usize=64;
#[derive(Clone,Debug,Default,Serialize,Deserialize)]
pub struct Causality {
    #[serde(default,skip_serializing_if="Option::is_none")]
    pub cause:Option<Value>,
    #[serde(default,skip_serializing_if="Option::is_none")]
    pub correlation:Option<String>,
}
fn string<'a>(value:&'a Value,field:&str)->Result<&'a str,RpcError> {
    match value.as_str() {
        Some(text) if !text.is_empty() && text.len()<=STRING_BYTES && !crate::limits::text_controls(text)=>Ok(text),
        _=>Err(RpcError::invalid(format!("{field} must be a string of 1..=256 bytes without control/format characters"))),
    }
}
fn validate_cause(cause:&Value)->Result<(),RpcError> {
    let object=cause.as_object().ok_or_else(||RpcError::invalid("cause must be an object"))?;
    crate::limits::json_depth(cause,CAUSE_DEPTH).map_err(|_|RpcError::invalid("cause exceeds 64 containers"))?;
    if crate::limits::encoded_bytes(cause)>CAUSE_BYTES{return Err(RpcError::invalid("cause exceeds 4096 encoded bytes"));}
    for field in ["kind","id","provenance"] {string(&cause[field],field)?;}
    for field in ["endpoint","epoch","boot"] {if let Some(value)=object.get(field){string(value,field)?;}}
    // Qualification is structural only: prior boots/epochs and unknown enums remain data.
    match cause["kind"].as_str().unwrap() {
        "entity"|"request"=>{string(&cause["endpoint"],"cause.endpoint")?;string(&cause["epoch"],"cause.epoch")?;}
        "effect"=>{string(&cause["boot"],"cause.boot")?;}
        _=>{}
    }
    Ok(())
}
impl Causality {
    /// Explicit metadata must have a recording sink; accepting then stripping it would lose caller intent.
    pub(crate) fn check_method(method:&str,params:&Value)->Result<(),RpcError> {
        if (params.get("cause").is_some() || params.get("correlation").is_some()) && !matches!(method,
            "workspace.apply"|"workspace.revert"|"workspace.owner.bootstrap"|"operational.register"|"operational.apply"|
            "resource.spawn"|"resource.kill"|"resource.attach"|"resource.invoke"|"resource.forget"|"resource.forget_cached") {
            return Err(RpcError::invalid("cause/correlation are not recorded by this method"));
        }
        Ok(())
    }
    pub fn parse(params:&Value)->Result<Self,RpcError> {
        let cause=if let Some(cause)=params.get("cause") {
            validate_cause(cause)?;
            Some(cause.clone())
        } else {None};
        let correlation=params.get("correlation").map(|value|string(value,"correlation").map(String::from)).transpose()?;
        Ok(Self{cause,correlation})
    }
    pub(crate) fn take(params:&mut Value)->Result<Self,RpcError> {
        let value=Self::parse(params)?;
        if let Some(object)=params.as_object_mut(){object.remove("cause");object.remove("correlation");}
        Ok(value)
    }
    /// Common resource-operation args may carry explicit links. Never rewrite the op's args.
    pub(crate) fn lift_args(params:&mut Value)->Result<(),RpcError> {
        for field in ["cause","correlation"] {
            let Some(nested)=params.get("args").and_then(|args|args.get(field)) else{continue};
            if field=="cause"{validate_cause(nested)?;}else{string(nested,field)?;}
            if let Some(explicit)=params.get(field) {
                if explicit!=nested{return Err(RpcError::invalid(format!("conflicting {field} in request and args")));}
            } else {
                // Clone only after bounds/shape validation, never an unbounded/deep nested payload.
                let nested=nested.clone();
                if let Some(object)=params.as_object_mut(){object.insert(field.into(),nested);}
            }
        }
        Ok(())
    }
    /// Absence stays absent; no current boot, delivery id or provenance is invented.
    pub fn annotate(&self,value:&mut Value) {
        if let Some(object)=value.as_object_mut() {
            if let Some(cause)=&self.cause {object.insert("cause".into(),cause.clone());}
            if let Some(correlation)=&self.correlation {object.insert("correlation".into(),Value::String(correlation.clone()));}
        }
    }
}

#[cfg(test)]
#[test]
fn nested_links_are_refused_before_lifting_into_the_root() {
    for field in ["cause","correlation"] {
        let value=if field=="cause"{serde_json::json!({"kind":"future","id":"id","provenance":"asserted","opaque":"x".repeat(900_000)})}else{Value::String("x".repeat(900_000))};
        let mut params=serde_json::json!({"args":{}});params["args"][field]=value;
        let error=Causality::lift_args(&mut params).expect_err("oversized nested field must fail at the lift stage, not after cloning/inserting it");
        assert_eq!(error.kind(),"invalid");assert!(params.get(field).is_none());assert!(params["args"].get(field).is_some(),"operation args remain untouched");
    }
    let mut nested=Value::Null;for _ in 0..64{nested=serde_json::json!([nested]);}
    let mut params=serde_json::json!({"args":{"cause":{"kind":"future","id":"id","provenance":"asserted","opaque":nested}}});
    assert_eq!(Causality::lift_args(&mut params).unwrap_err().kind(),"invalid");assert!(params.get("cause").is_none());
}
