//! Native Pi configuration, porting pi_bridge/control.py's bounded declarations.
use std::collections::BTreeMap;
use serde_json::{json,Value};
use super::admission::Refusal;
pub const MAX_CHOICES:usize=64;
#[derive(Clone)]
struct Model {key:String,provider:String,id:String,label:String}
#[derive(Default)]
pub struct Configuration {
    models:Option<Vec<Model>>,levels:Option<Vec<String>>,options:Vec<Value>,revisions:BTreeMap<String,u64>,
    pub pending:bool,pub incomplete:bool,
}
#[derive(Clone)]
pub struct Change {pub id:String,pub value:Value,pub command:&'static str,pub args:Value}
fn error(kind:&'static str,message:&str)->Refusal{Refusal {kind,message:message.into()}}
fn model(row:&Value)->Option<Model>{
    let provider=row["provider"].as_str().filter(|s|!s.is_empty() && s.len()<=128)?;
    let id=row["id"].as_str().filter(|s|!s.is_empty() && s.len()<=256)?;
    let label=row.get("name").and_then(Value::as_str).unwrap_or(id).chars().take(80).collect::<String>();
    // Stable, collision-free opaque choice value. Never split a provider/id concatenation.
    Some(Model {key:json!([provider,id]).to_string(),provider:provider.into(),id:id.into(),label})
}
impl Configuration {
    pub fn options(&self)->&[Value]{&self.options}
    pub fn observe_models(&mut self,data:&Value){
        self.models=None;
        if let Some(rows)=super::contract::rows(data,"models"){
            let mut models=Vec::new();let mut omitted=rows.len()>MAX_CHOICES;
            for row in rows.iter().take(MAX_CHOICES){if let Some(model)=model(row){if !models.iter().any(|old:&Model|old.key==model.key){models.push(model);}}else{omitted=true;}}
            self.models=Some(models);self.incomplete=omitted;
        }else{self.incomplete=true;}
    }
    pub fn observe_levels(&mut self,data:&Value){
        self.levels=super::contract::rows(data,"levels").and_then(|rows|{
            if rows.len()>MAX_CHOICES{return None;}
            rows.iter().map(|row|row.as_str().filter(|s|!s.is_empty() && s.len()<=64).map(str::to_string)).collect()
        });
        if self.levels.is_none(){self.incomplete=true;}
    }
    pub fn rebuild(&mut self,metadata:&Value){
        let mut options=Vec::new();
        if let Some(models)=&self.models{
            let mut models=models.clone();let current=model(&metadata["model"]);
            if let Some(current)=&current{if !models.iter().any(|row|row.key==current.key){models.insert(0,current.clone());}}
            if !models.is_empty(){options.push(json!({"id":"model","label":"Model","category":"model","type":"select","value":current.map(|model|model.key),"choices":models.iter().map(|row|json!({"value":row.key,"label":row.label,"description":row.provider})).collect::<Vec<_>>()}));}
        }
        if let (Some(levels),Some(current))=(&self.levels,metadata["thinkingLevel"].as_str()){
            options.push(json!({"id":"thinking_level","label":"Thinking level","category":"thought_level","type":"select","value":current,"choices":levels.iter().map(|level|json!({"value":level,"label":level})).collect::<Vec<_>>()}));
        }
        for (id,label,field) in [("steering_mode","Steering delivery","steeringMode"),("follow_up_mode","Follow-up delivery","followUpMode")]{
            if matches!(metadata[field].as_str(),Some("one-at-a-time"|"all")){
                options.push(json!({"id":id,"label":label,"category":"_queue","type":"select","value":metadata[field],"description":"Applies to queued input when consumed, not when admitted.","choices":[{"value":"one-at-a-time","label":"One at a time"},{"value":"all","label":"All at once"}]}));
            }
        }
        if metadata["autoCompactionEnabled"].is_boolean(){options.push(json!({"id":"auto_compaction","label":"Auto-compaction","category":"_compaction","type":"boolean","value":metadata["autoCompactionEnabled"]}));}
        for option in &mut options{
            if self.pending {option["value"]=Value::Null;option["incomplete"]=json!(["native_change_unconfirmed"]);}
            let id=option["id"].as_str().unwrap();let mut old=self.options.iter().find(|old|old["id"]==id).cloned().unwrap_or(Value::Null);
            if let Some(old)=old.as_object_mut(){old.remove("rev");}
            let revision=self.revisions.entry(id.into()).or_default();if old!=*option{*revision+=1;}option["rev"]=json!(*revision);
        }
        self.options=options;
    }
    pub fn prepare(&self,id:&str,value:&Value,revision:Option<&Value>,metadata:&Value)->Result<Change,Refusal>{
        if self.pending{return Err(error("busy","a native configuration change is unresolved"));}
        let option=self.options.iter().find(|option|option["id"]==id).ok_or_else(||error("unsupported","configuration option is not declared"))?;
        if revision.is_some_and(|revision|revision!=&option["rev"]){return Err(error("stale","configuration option revision changed"));}
        if option["type"]=="boolean" {if !value.is_boolean(){return Err(error("invalid","configuration value must be boolean"));}}
        else if !option["choices"].as_array().is_some_and(|choices|choices.iter().any(|choice|choice["value"]==*value)){return Err(error("invalid","value is outside declared choices"));}
        let (command,args)=match id{
            "model"=>{
                let current=model(&metadata["model"]);
                let model=self.models.as_ref().and_then(|models|models.iter().find(|row|json!(row.key)==*value)).or_else(||current.as_ref().filter(|row|json!(row.key)==*value)).ok_or_else(||error("stale","model choice is no longer available"))?;
                ("set_model",json!({"provider":model.provider,"modelId":model.id}))
            }
            "thinking_level"=>("set_thinking_level",json!({"level":value})),
            "steering_mode"=>("set_steering_mode",json!({"mode":value})),
            "follow_up_mode"=>("set_follow_up_mode",json!({"mode":value})),
            "auto_compaction"=>("set_auto_compaction",json!({"enabled":value})),
            _=>return Err(error("unsupported","unknown configuration option")),
        };
        Ok(Change{id:id.into(),value:value.clone(),command,args})
    }
    pub fn matches(&self,change:&Change)->bool{self.options.iter().any(|option|option["id"]==change.id && option["value"]==change.value)}
}
