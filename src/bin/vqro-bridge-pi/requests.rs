//! Native dialog routing only. The full request entity lives solely in ItemJournal.
use std::collections::BTreeMap;
use serde_json::{json,Value};
use super::admission::Refusal;
pub const MAX_PENDING:usize=8;
pub const MAX_REQUEST_BYTES:usize=16*1024;
struct Route {native:String,method:String,claimed:bool}
#[derive(Default)]
pub struct Requests {serial:u64,routes:BTreeMap<String,Route>,native_ids:BTreeMap<String,String>}
pub enum Incoming {
    Dialog {id:String,value:Value,replaced:Option<String>},
    Cancel {id:String,native:Value,reason:String,replaced:Option<String>},
    Notice {id:String,value:Value},
}
pub struct Answer {pub request:String,pub wire:Value,pub cancelled:bool}
fn refusal(kind:&'static str,message:&str)->Refusal {Refusal {kind,message:message.into()}}
fn text<'a>(value:&'a Value,max:usize)->Option<&'a str>{value.as_str().filter(|s|s.len()<=max)}
impl Requests {
    pub fn ids(&self)->Vec<String>{self.routes.keys().cloned().collect()}
    pub fn has_unconfirmed(&self)->bool{self.routes.values().any(|route|route.claimed)}
    pub fn would_cancel_another(&self,id:&str,native:&Value)->bool{native.as_str().and_then(|native|self.native_ids.get(native)).is_some_and(|current|current!=id)}
    pub fn observe(&mut self,row:&Value)->Incoming {
        self.serial+=1;let id=format!("q{}",self.serial);
        let native=row.get("id").cloned().unwrap_or(Value::Null);
        let method=row["method"].as_str().unwrap_or("");
        if matches!(method,"notify"|"setStatus"|"setWidget"|"setTitle"|"set_editor_text") {
            return Incoming::Notice {id,value:row.clone()};
        }
        let build=||->Option<(String,Value)> {
            let native=text(&native,128)?.to_string();if native.is_empty(){return None;}
            // Relative native timeouts have no observable birth timestamp/expiry signal.
            if row.get("timeout").is_some_and(|v|!v.is_null() && v.as_f64()!=Some(0.0)){return None;}
            let title=text(&row["title"],256)?;
            let (standard,params,choices)=match method {
                "select"=>{
                    let options=row["options"].as_array()?;if options.len()>16{return None;}
                    let mut choices=Vec::new();
                    for (i,option) in options.iter().enumerate(){choices.push(json!({"id":format!("c{i}"),"label":text(option,128)?}));}
                    ("choice",json!({}),Some(json!(choices)))
                }
                "confirm"=>("confirm",json!({"message":text(&row["message"],4096)?}),None),
                "input"=>{
                    let mut params=json!({"multiline":false});
                    if let Some(placeholder)=row.get("placeholder"){params["placeholder"]=json!(text(placeholder,1024)?);}
                    ("text",params,None)
                }
                "editor"=>{
                    let mut params=json!({"multiline":true});
                    if let Some(prefill)=row.get("prefill"){params["prefill"]=json!(text(prefill,12*1024)?);}
                    ("text",params,None)
                }
                _=>return None,
            };
            let allowed=["type","id","method","title","options","message","placeholder","prefill","timeout"];
            if row.as_object()?.keys().any(|key|!allowed.contains(&key.as_str())){return None;}
            let used:&[&str]=match method {"select"=>&["options"],"confirm"=>&["message"],"input"=>&["placeholder"],_=>&["prefill"]};
            let extras:serde_json::Map<String,Value>=row.as_object()?.iter().filter(|(key,_)|!["type","id","method","title"].contains(&key.as_str()) && !used.contains(&key.as_str())).map(|(key,value)|(key.clone(),value.clone())).collect();
            let mut value=json!({"request":id,"revision":1,"method":standard,"title":title,"params":params,
                "scope":{"operation":format!("ui-{id}")},"status":"pending","incomplete":false,"native":{"id":native,"method":method,"extras":extras}});
            if let Some(choices)=choices{value["choices"]=choices;}
            if vqro::limits::encoded_bytes(&value)>MAX_REQUEST_BYTES{return None;}
            Some((native,value))
        };
        let built=build();
        let replaced=native.as_str().and_then(|native|self.native_ids.remove(native));
        if let Some(old)=&replaced{self.routes.remove(old);}
        let Some((native_id,value))=built else{return Incoming::Cancel {id,native,reason:"unknown, timed or unrepresentable native dialog".into(),replaced};};
        if self.routes.len()>=MAX_PENDING {return Incoming::Cancel {id,native,reason:"native dialog count bound".into(),replaced};}
        self.native_ids.insert(native_id.clone(),id.clone());self.routes.insert(id.clone(),Route {native:native_id,method:method.into(),claimed:false});
        Incoming::Dialog {id,value,replaced}
    }
    pub fn claim(&mut self,id:&str,revision:u64,response:&Value,entity:&Value)->Result<Answer,Refusal> {
        let route=self.routes.get_mut(id).ok_or_else(||refusal("stale","request is not pending"))?;
        if route.claimed || revision!=1 || entity["revision"]!=revision || entity["status"]!="pending" {return Err(refusal("stale","request revision already claimed or ended"));}
        if entity["scope"]["operation"]!=format!("ui-{id}"){return Err(refusal("stale","dialog operation is not live"));}
        let object=response.as_object().ok_or_else(||refusal("invalid","response must be an object"))?;
        let cancelled=response["action"]=="cancel";
        let mut wire=json!({"id":route.native});
        if cancelled {
            if object.len()!=1{return Err(refusal("invalid","cancel cannot also carry an answer"));}
            wire["cancelled"]=json!(true);
        }else{
            if entity["incomplete"]!=false{return Err(refusal("invalid","incomplete requests cannot be approved"));}
            let (field,value)=match route.method.as_str(){
                "select"=>{
                    if object.len()!=1{return Err(refusal("invalid","choice response has undeclared fields"));}
                    let choice=response["choice"].as_str().ok_or_else(||refusal("invalid","choice ID required"))?;
                    let value=entity["choices"].as_array().and_then(|choices|choices.iter().find(|value|value["id"]==choice)).and_then(|value|value["label"].as_str()).ok_or_else(||refusal("invalid","choice is not offered"))?;
                    ("value",json!(value))
                }
                "confirm"=>{
                    if object.len()!=1 || !response["value"].is_boolean(){return Err(refusal("invalid","confirm requires a boolean value"));}
                    ("confirmed",response["value"].clone())
                }
                _=>{
                    if object.len()!=1{return Err(refusal("invalid","text response has undeclared fields"));}
                    let value=text(&response["value"],16*1024).ok_or_else(||refusal("invalid","bounded text value required"))?;
                    ("value",json!(value))
                }
            };
            wire[field]=value;
        }
        route.claimed=true;
        Ok(Answer {request:id.into(),wire,cancelled})
    }
    pub fn rollback(&mut self,id:&str){if let Some(route)=self.routes.get_mut(id){route.claimed=false;}}
    pub fn cancel_one(&mut self,id:&str)->Option<Value>{let route=self.routes.remove(id)?;if self.native_ids.get(&route.native).is_some_and(|current|current==id){self.native_ids.remove(&route.native);}Some(json!(route.native))}
    pub fn finish(&mut self,id:&str){let _=self.cancel_one(id);}
    pub fn cancel_all(&mut self)->Vec<(String,Value)> {
        let rows=self.routes.iter().map(|(id,route)|(id.clone(),json!(route.native))).collect();self.routes.clear();self.native_ids.clear();rows
    }
}
