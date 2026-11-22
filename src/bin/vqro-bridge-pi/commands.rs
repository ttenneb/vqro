//! Explicit Pi commands; plain sends never dispatch through this registry.
use serde_json::{json,Value};
use super::admission::Refusal;
pub const MAX_COMMANDS:usize=64;
#[derive(Default)]
pub struct Commands {native:Vec<Value>,pub incomplete:bool,observed:bool}
pub enum Prepared {NewSession,Rpc {name:String,command:&'static str,args:Value},Prompt {message:String}}
pub fn observed_result(command:&str,data:&Value)->bool{match command{
    "cycle_model"=>data.is_null() || (data["model"]["provider"].is_string() && data["model"]["id"].is_string()),
    "compact"=>data["summary"].is_string() && data["firstKeptEntryId"].is_string() && data["tokensBefore"].as_u64().is_some(),
    _=>false,
}}
pub fn safe_result(command:&str,data:&Value)->Value{
    if command!="cycle_model" || data.is_null(){return data.clone();}
    // Native models may include configured headers: never publish the raw credential-bearing model object.
    let mut result=json!({"incomplete":["native_model_fields"]});
    for key in ["provider","id","name"]{if let Some(value)=data["model"][key].as_str().filter(|value|value.len()<=512){result["model"][key]=json!(value);}}
    if let Some(level)=data["thinkingLevel"].as_str().filter(|value|value.len()<=64){result["thinkingLevel"]=json!(level);}
    result
}
fn error(kind:&'static str,message:&str)->Refusal{Refusal {kind,message:message.into()}}
impl Commands {
    pub fn observe(&mut self,data:&Value){
        self.native.clear();self.incomplete=false;self.observed=true;
        let Some(rows)=super::contract::rows(data,"commands")else{self.incomplete=true;return;};
        if rows.len()>MAX_COMMANDS{self.incomplete=true;}
        for row in rows.iter().take(MAX_COMMANDS){
            let Some(name)=row["name"].as_str().filter(|name|!name.is_empty() && name.len()<=128 && !name.chars().any(|c|c.is_whitespace() || c.is_control()))else{self.incomplete=true;continue;};
            if ["new_session","cycle_model","compact"].contains(&name) || self.native.iter().any(|row|row["name"]==name){continue;}
            let mut value=json!({"name":name,"input":{"hint":"Arguments (optional)"}});
            if let Some(description)=row["description"].as_str(){let end=description.char_indices().map(|(at,_)|at).take_while(|at|*at<=300).last().unwrap_or(0);let end=if description.len()<=300{description.len()}else{end};value["description"]=json!(&description[..end]);if end<description.len(){self.incomplete=true;}}
            self.native.push(value);
        }
    }
    /// Every native slash name Pi would expand or run, or None while the inventory is unknown or incomplete.
    pub fn slash_names(&self)->Option<Vec<String>>{
        (self.observed && !self.incomplete).then(||self.native.iter().filter_map(|row|row["name"].as_str().map(str::to_string)).collect())
    }
    pub fn facts(&self)->Vec<Value>{
        let mut rows=vec![json!({"name":"new_session","description":"Start a fresh native Pi conversation."}),json!({"name":"cycle_model","description":"Switch to the next available native model."}),json!({"name":"compact","description":"Summarise the conversation (one native model operation).","input":{"hint":"Custom instructions (optional)"}})];rows.extend(self.native.iter().cloned());rows
    }
    pub fn prepare(&self,name:&str,args:Option<&Value>)->Result<Prepared,Refusal>{
        let text=match args{None=>None,Some(Value::String(text)) if text.len()<=16*1024=>Some(text.as_str()),_=>return Err(error("invalid","command args must be a string of at most16KiB"))};
        match name{
            "new_session"|"cycle_model"=>{
                if text.is_some_and(|text|!text.is_empty()){return Err(error("unsupported","this native command takes no arguments"));}
                if name=="new_session"{Ok(Prepared::NewSession)}else{Ok(Prepared::Rpc{name:name.into(),command:"cycle_model",args:json!({})})}
            }
            "compact"=>Ok(Prepared::Rpc{name:name.into(),command:"compact",args:text.map(|text|json!({"customInstructions":text})).unwrap_or_else(||json!({}))}),
            _ if self.native.iter().any(|row|row["name"]==name)=>Ok(Prepared::Prompt {message:format!("/{name}{}",text.map(|text|format!(" {text}")).unwrap_or_default())}),
            _=>Err(error("unsupported","command is not advertised by this native runtime")),
        }
    }
}
