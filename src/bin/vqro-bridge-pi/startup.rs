//! The agent-v0 `startup` entity (id `current`) for Pi: only what Pi natively reports, each field with its source.
//! Pi 1.5.2 RPC reports the model (get_state) and skills / prompt templates / extension commands with their
//! sourceInfo paths (get_commands, rpc-mode.js:562-591); its version only via the launched binary's own
//! `--version` (config.js VERSION; cli/args.js). Context files, themes and the full extension list are TUI-only in
//! Pi and stay absent (they would need a Pi RPC addition). The launch folder is vqro's own record.
use serde_json::{json,Map,Value};

const LIST:usize=256;
const NAME:usize=128;
const DESCRIPTION:usize=1024;
const PATH:usize=4096;

#[derive(Default,Clone)]
pub struct Startup {fields:Map<String,Value>,sources:Map<String,Value>,incomplete:Vec<String>}

/// A string within `max` bytes, or None (with the field marked incomplete) when it is longer.
fn bounded(value:&Value,max:usize)->Result<Option<String>,()> {
    match value.as_str() {None=>Ok(None),Some(text) if text.len()<=max=>Ok(Some(text.into())),Some(_)=>Err(())}
}

impl Startup {
    fn mark(&mut self,field:&str){if !self.incomplete.iter().any(|f|f==field){self.incomplete.push(field.into());}}
    fn set(&mut self,field:&str,value:Value,source:&str){self.fields.insert(field.into(),value);self.sources.insert(field.into(),json!(source));}
    /// The launch folder vqro itself recorded.
    pub fn cwd(&mut self,cwd:&str){if cwd.len()<=PATH{self.set("cwd",json!(cwd),"vqro:launch");}else{self.mark("cwd");}}
    /// The first line of the launched binary's own `--version` output.
    pub fn version(&mut self,output:&str){
        match output.lines().map(str::trim).find(|line|!line.is_empty()) {
            Some(line) if line.len()<=NAME=>{self.fields.insert("runtime".into(),json!({"name":"Pi","version":line,"version_source":"cli:--version"}));}
            _=>self.mark("runtime"),
        }
    }
    /// get_state: the model's own id.
    pub fn state(&mut self,data:&Value){
        match bounded(&data["model"]["id"],NAME) {Ok(Some(id))=>self.set("model",json!(id),"rpc:get_state"),Ok(None)=>{},Err(())=>self.mark("model")}
    }
    /// get_commands: skills (loaded), prompt templates and extension commands, with their sourceInfo paths.
    pub fn commands(&mut self,rows:&[Value]){
        let (mut skills,mut prompts,mut commands)=(vec![],vec![],vec![]);
        let mut cut=vec![];
        for row in rows {
            let source=row["source"].as_str().unwrap_or("");
            let (list,field,name)=match source {
                "skill"=>(&mut skills,"skills",row["name"].as_str().map(|n|n.strip_prefix("skill:").unwrap_or(n))),
                "prompt"=>(&mut prompts,"prompts",row["name"].as_str()),
                "extension"=>(&mut commands,"commands",row["name"].as_str()),
                _=>continue,
            };
            let Some(name)=name.filter(|n|!n.is_empty() && n.len()<=NAME) else{cut.push(field);continue;};
            if list.len()>=LIST {cut.push(field);continue;}
            let mut entry=json!({"name":name});
            match bounded(&row["description"],DESCRIPTION){Ok(Some(d))=>{entry["description"]=json!(d);},Ok(None)=>{},Err(())=>cut.push(field)}
            match bounded(&row["sourceInfo"]["path"],PATH){Ok(Some(p))=>{entry["path"]=json!(p);},Ok(None)=>{},Err(())=>cut.push(field)}
            if source=="extension"{entry["kind"]=json!("extension");}
            list.push(entry);
        }
        self.set("skills",json!(skills),"rpc:get_commands");self.fields.insert("skills_scope".into(),json!("loaded"));
        self.set("prompts",json!(prompts),"rpc:get_commands");
        self.set("commands",json!(commands),"rpc:get_commands");
        for field in cut {self.mark(field);}
    }
    /// An inventory that errored: its field is absent and named incomplete.
    pub fn failed(&mut self,fields:&[&str]){for field in fields {self.fields.remove(*field);self.sources.remove(*field);self.mark(field);}}
    /// Keep the entity within `bound` bytes by cutting the longest list from its end (named incomplete).
    pub fn fit(&mut self,bound:usize){
        while vqro::limits::encoded_bytes(&self.value())>bound {
            let Some(field)=["skills","prompts","commands"].into_iter().filter(|f|self.fields.get(*f).and_then(Value::as_array).is_some_and(|l|!l.is_empty()))
                .max_by_key(|f|vqro::limits::encoded_bytes(&self.fields[*f])) else {break;};
            self.fields.get_mut(field).and_then(Value::as_array_mut).unwrap().pop();self.mark(field);
        }
    }
    pub fn value(&self)->Value {
        let mut value=Value::Object(self.fields.clone());
        if value.get("runtime").is_none(){value["runtime"]=json!({"name":"Pi"});}
        if !self.sources.is_empty(){value["sources"]=Value::Object(self.sources.clone());}
        if !self.incomplete.is_empty(){value["incomplete"]=json!(self.incomplete);}
        value
    }
}
