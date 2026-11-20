//! Static, bounded extension metadata. Never imports or executes an extension.
use std::{collections::BTreeMap,io::Read,path::PathBuf,time::SystemTime};
use serde_json::{json,Value};
const FILE_BYTES:u64=32*1024;
const CATALOG_BYTES:usize=1024*1024;
#[derive(Clone)]
struct Record { path:PathBuf, stamp:Option<(SystemTime,u64)>, value:Value, error:Option<String> }
pub struct Catalog { pub root:PathBuf, records:BTreeMap<String,Record>, overflow:bool }
impl Catalog {
    pub fn new(root:PathBuf)->Self {Self{root,records:BTreeMap::new(),overflow:false}}
    /// Reconciliation and extension.list refresh cached file stamps; snapshots perform no file I/O.
    pub fn refresh(&mut self,specs:&[(String,u64,Value)]) {
        let mut records=BTreeMap::new();let mut bytes=0;self.overflow=false;
        for (name,_,spec) in specs {
            let explicit=spec["manifest"].as_str();
            let path=explicit.map(PathBuf::from).map(|p|if p.is_absolute(){p}else{self.root.join(p)}).unwrap_or_else(||self.root.join("manifests").join(format!("{name}.json")));
            let meta=std::fs::metadata(&path);
            if explicit.is_none() && meta.as_ref().is_err_and(|e|e.kind()==std::io::ErrorKind::NotFound){continue;}
            let stamp=meta.as_ref().ok().and_then(|m|m.modified().ok().map(|t|(t,m.len())));
            let record=if let Some(old)=self.records.get(name).filter(|r|r.path==path && r.stamp==stamp && stamp.is_some()){old.clone()}else{
                let loaded=(||->Result<Value,String>{
                    let m=meta.map_err(|e|format!("Cannot read manifest: {e}"))?;
                    if !m.is_file(){return Err("Manifest is not a regular file".into());}
                    if m.len()>FILE_BYTES{return Err("Manifest exceeds 32 KiB".into());}
                    let mut data=Vec::new();std::fs::File::open(&path).map_err(|e|e.to_string())?.take(FILE_BYTES+1).read_to_end(&mut data).map_err(|e|e.to_string())?;
                    if data.len() as u64>FILE_BYTES{return Err("Manifest exceeds 32 KiB".into());}
                    let value:Value=serde_json::from_slice(&data).map_err(|e|format!("Invalid manifest JSON: {e}"))?;
                    if !value.is_object() || value["name"].as_str()!=Some(name){return Err("Manifest name must match the extension declaration".into());}
                    Ok(value)
                })();
                let(value,error)=match loaded{Ok(v)=>(v,None),Err(e)=>(Value::Null,Some(e))};
                Record{path,stamp,value,error}
            };
            let size=serde_json::to_vec(&json!({"name":name,"source":record.path.to_string_lossy(),"manifest":record.value,"error":record.error})).map(|v|v.len()+128).unwrap_or(CATALOG_BYTES+1);
            if bytes+size>CATALOG_BYTES{self.overflow=true;continue;}
            bytes+=size;records.insert(name.clone(),record);
        }
        self.records=records;
    }
    /// Resolve every declaration's `needs`/`works_with` against all manifests' `provides`, the core's built-ins and
    /// the connected clients' UI features. `status` maps declaration name to display status. Reports only: nothing
    /// here enables, disables or gates anything (docs/research/declarations-and-ui-clients.md).
    pub fn dependencies(&self,status:&BTreeMap<String,String>,clients:&[Value])->BTreeMap<String,Value> {
        let mut providers:BTreeMap<String,Vec<(String,u64)>>=BTreeMap::new();
        for (name,r) in &self.records {
            for cap in r.value["provides"].as_array().into_iter().flatten().filter_map(Value::as_str).filter_map(parse_cap) {
                providers.entry(cap.0).or_default().push((name.clone(),cap.1));
            }
        }
        let running=|name:&str|status.get(name).map(String::as_str)==Some("running");
        let mut out:BTreeMap<String,Value>=BTreeMap::new();
        let mut used_by:BTreeMap<String,Vec<Value>>=BTreeMap::new();
        for (name,r) in &self.records {
            let mut worst=0;
            let mut lists=serde_json::Map::new();
            for (field,hard) in [("needs",true),("works_with",false)] {
                let mut rows=Vec::new();
                for raw in declared(&r.value,field) {
                    let cap_text=raw.get("cap").and_then(Value::as_str).unwrap_or("");
                    let mut row=json!({"cap":cap_text});
                    for k in ["suggest","why","without"] {if let Some(v)=raw.get(k).and_then(Value::as_str){row[k]=json!(v);}}
                    let Some((cap,major))=parse_cap(cap_text) else {row["status"]=json!("invalid");rows.push(row);continue};
                    if cap.starts_with("ui/") {
                        ui_status(&mut row,&(cap,major),clients);rows.push(row);continue;
                    }
                    let state=if core_provides(&cap,major) {
                        row["providers"]=json!([{"name":"vqro","display_status":"core"}]);"satisfied"
                    } else {
                        let offered=providers.get(&cap).map(Vec::as_slice).unwrap_or(&[]);
                        let same:Vec<&(String,u64)>=offered.iter().filter(|(p,m)|*m==major && p!=name).collect();
                        for (p,_) in offered.iter().filter(|(p,_)|p!=name) {
                            used_by.entry(p.clone()).or_default().push(json!({"name":name,"cap":cap_text,"hard":hard,"display_status":status.get(name)}));
                        }
                        row["providers"]=Value::Array(same.iter().map(|(p,_)|json!({"name":p,"display_status":status.get(p)})).collect());
                        if offered.iter().any(|(p,_)|p==name) {"satisfied"}
                        else if same.iter().any(|(p,_)|running(p)) {"satisfied"}
                        else if !same.is_empty() {"off"}
                        else if !offered.is_empty() {"version"}
                        else {"missing"}
                    };
                    if state!="satisfied" {worst=worst.max(if hard{2}else{1});}
                    row["status"]=json!(state);rows.push(row);
                }
                lists.insert(field.into(),Value::Array(rows));
            }
            lists.insert("status".into(),json!(["ok","degraded","missing"][worst]));
            out.insert(name.clone(),Value::Object(lists));
        }
        for (name,deps) in out.iter_mut() {deps["used_by"]=Value::Array(used_by.remove(name).unwrap_or_default());}
        out
    }
    pub fn facts(&self,name:&str)->Value {
        match self.records.get(name) {
            Some(r)=>json!({"manifest":r.value,"manifest_source":r.path.to_string_lossy(),"manifest_error":r.error}),
            None=>json!({"manifest":null,"manifest_source":null,"manifest_error":if self.overflow{Some("Manifest catalog exceeds 1 MiB; metadata omitted")}else{None}}),
        }
    }
}
/// `kind/name[@major]` -> (`kind/name`, major). Majors only; a missing major is 1.
pub fn parse_cap(text:&str)->Option<(String,u64)> {
    let (body,major)=match text.rsplit_once('@') {Some((b,m))=>(b,m.parse::<u64>().ok().filter(|m|*m>0)?),None=>(text,1)};
    let (kind,name)=body.split_once('/')?;
    let ok=|s:&str|!s.is_empty() && s.len()<=256 && s.chars().all(|c|c.is_ascii_alphanumeric() || "._-/*".contains(c));
    (ok(kind) && !kind.contains('/') && ok(name)).then(||(body.to_string(),major))
}
/// Built-in capabilities: core commands and the spec-defined shared settings, whose readers always have a fallback.
fn core_provides(cap:&str,major:u64)->bool {
    if cap=="ability/vqro.operational-domains" {return major==1;}
    cap.starts_with("command/vqro.") || matches!(cap,"doc/settings/tui"|"doc/settings/theme"|"doc/settings/icons")
}
/// Declared entries of `needs`/`works_with` as objects; `requires.client_features` are UI needs (`ui/vqro.<feature>`).
fn declared(manifest:&Value,field:&str)->Vec<Value> {
    let mut rows:Vec<Value>=manifest[field].as_array().into_iter().flatten().map(|v|match v {Value::String(s)=>json!({"cap":s}),Value::Object(_)=>v.clone(),_=>json!({"cap":""})}).collect();
    if field=="needs" {
        rows.extend(manifest["requires"]["client_features"].as_array().into_iter().flatten().filter_map(Value::as_str).map(|f|json!({"cap":ui_cap(f)})));
    }
    rows
}
/// Bare feature names (today's `requires` vocabulary) are `vqro.*` features.
pub fn ui_cap(feature:&str)->String {if feature.contains('.'){format!("ui/{feature}")}else{format!("ui/vqro.{feature}")}}
/// UI features are content concerns with fallbacks: report which connected clients lack one, never `missing`.
fn ui_status(row:&mut Value,want:&(String,u64),clients:&[Value]) {
    // Rendering clients: a declared `base` profile, or a legacy client without profiles (not the one-shot CLI).
    let interactive:Vec<&Value>=clients.iter().filter(|c|c["client"].is_string() && c["client"]!="cli" && c["profiles"].as_array().is_none_or(|p|p.iter().any(|x|x=="base"))).collect();
    let label=|c:&Value|c["client"].as_str().unwrap_or("client").to_string();
    let claims=|c:&Value|c["features"].as_array().is_some_and(|a|a.iter().filter_map(Value::as_str).any(|x|parse_cap(&ui_cap(x)).as_ref()==Some(want)));
    let lacking:Vec<String>=interactive.iter().filter(|c|c["features"].is_array() && !claims(c)).map(|c|label(c)).collect();
    let unknown:Vec<String>=interactive.iter().filter(|c|!c["features"].is_array()).map(|c|label(c)).collect();
    row["status"]=json!(if !lacking.is_empty(){"lacking"}else if !unknown.is_empty() || interactive.is_empty(){"unknown"}else{"satisfied"});
    row["lacking_in"]=json!(lacking);row["unknown_in"]=json!(unknown);
}
#[cfg(test)] mod tests {
    #[test]
    fn operational_core_ability_does_not_claim_unknown_major_versions() {
        assert!(super::core_provides("ability/vqro.operational-domains",1));
        assert!(!super::core_provides("ability/vqro.operational-domains",2));
    }
    use super::*;
    #[test]fn dependencies_report_satisfied_off_missing_version_and_reverse_users(){
        let root=tempfile::tempdir().unwrap();
        let write=|n:&str,v:Value|std::fs::write(root.path().join(format!("{n}.json")),v.to_string()).unwrap();
        write("tabs",json!({"name":"tabs","provides":["command/tabs.show"]}));
        write("herdr",json!({"name":"herdr","provides":["ability/vqro.terminals"]}));
        write("themes",json!({"name":"themes","provides":["data/themes@2"]}));
        write("agents",json!({"name":"agents","needs":[{"cap":"command/tabs.show","suggest":"tabs","why":"open panels"},"ability/vqro.terminals","command/vqro.revert"],
            "works_with":[{"cap":"data/themes","without":"no theme page"},"data/nobody","bad cap"],"requires":{"client_features":["items","x.acme.table"]}}));
        let specs:Vec<(String,u64,Value)>=["tabs","herdr","themes","agents"].iter().map(|n|(n.to_string(),0,json!({"manifest":format!("{n}.json")}))).collect();
        let mut catalog=Catalog::new(root.path().into());catalog.refresh(&specs);
        let status:BTreeMap<String,String>=[("tabs","off"),("herdr","running"),("themes","running"),("agents","running")].iter().map(|(a,b)|(a.to_string(),b.to_string())).collect();
        let clients=vec![json!({"client":"tui","features":["items"]}),json!({"client":"old"}),json!({"client":null})];
        let deps=catalog.dependencies(&status,&clients);let a=&deps["agents"];
        assert_eq!(a["status"],"missing");
        let need=|i:usize|&a["needs"][i];
        assert_eq!(need(0)["status"],"off");assert_eq!(need(0)["suggest"],"tabs");assert_eq!(need(0)["providers"][0]["name"],"tabs");
        assert_eq!(need(1)["status"],"satisfied");assert_eq!(need(2)["status"],"satisfied");assert_eq!(need(2)["providers"][0]["name"],"vqro");
        assert_eq!(need(3)["cap"],"ui/vqro.items");assert_eq!(need(3)["status"],"unknown");assert_eq!(need(3)["unknown_in"],json!(["old"]));
        assert_eq!(need(4)["status"],"lacking");assert_eq!(need(4)["lacking_in"],json!(["tui"]));
        let w=&a["works_with"];assert_eq!(w[0]["status"],"version");assert_eq!(w[1]["status"],"missing");assert_eq!(w[2]["status"],"invalid");
        assert_eq!(deps["tabs"]["used_by"],json!([{"name":"agents","cap":"command/tabs.show","hard":true,"display_status":"running"}]));
        assert_eq!(deps["themes"]["used_by"][0]["hard"],false);
        assert_eq!(deps["tabs"]["status"],"ok");
        // A running alternative satisfies; UI needs never decide the overall status.
        let mut status=status;status.insert("tabs".into(),"running".into());
        assert_eq!(catalog.dependencies(&status,&clients)["agents"]["status"],"degraded");
    }
    #[test]fn capability_names_parse_with_majors(){
        assert_eq!(parse_cap("command/tabs.show"),Some(("command/tabs.show".into(),1)));
        assert_eq!(parse_cap("view/agent-*@2"),Some(("view/agent-*".into(),2)));
        assert_eq!(parse_cap("doc/settings/tui"),Some(("doc/settings/tui".into(),1)));
        for bad in ["tabs.show","command/","/x","command/x@0","command/x@v1","command/a b"] {assert_eq!(parse_cap(bad),None,"{bad}");}
        assert_eq!(ui_cap("items"),"ui/vqro.items");assert_eq!(ui_cap("x.acme.table@1"),"ui/x.acme.table@1");
    }
    #[test]fn catalog_budget_and_non_regular_files_fail_as_metadata_not_execution(){
        let root=tempfile::tempdir().unwrap();let mut catalog=Catalog::new(root.path().into());
        catalog.refresh(&[("folder".into(),0,json!({"manifest":"."}))]);assert!(catalog.facts("folder")["manifest_error"].as_str().unwrap().contains("regular"));
        let mut specs=Vec::new();
        for i in 0..40 {let name=format!("ext{i:02}");let file=format!("{name}.json");std::fs::write(root.path().join(&file),json!({"name":name,"future_padding":"x".repeat(30*1024)}).to_string()).unwrap();specs.push((name,0,json!({"manifest":file})));}
        catalog.refresh(&specs);assert!(catalog.facts("ext39")["manifest"].is_null());assert!(catalog.facts("ext39")["manifest_error"].as_str().unwrap().contains("1 MiB"));
        let size=specs.iter().map(|(name,_,_)|serde_json::to_vec(&catalog.facts(name)).unwrap().len()).sum::<usize>();assert!(size<CATALOG_BYTES+16*1024);
        catalog.refresh(&[]);assert!(!catalog.overflow);
    }
    #[test]fn static_manifest_is_available_without_execution_and_refreshes(){
        let root=tempfile::tempdir().unwrap();let file=root.path().join("info.json");
        std::fs::write(&file,r#"{"name":"sleeping","title":"Off but readable","future":42}"#).unwrap();
        let mut catalog=Catalog::new(root.path().into());let specs=vec![("sleeping".into(),1,json!({"enabled":false,"argv":["never-run"],"manifest":"info.json"}))];
        catalog.refresh(&specs);assert_eq!(catalog.facts("sleeping")["manifest"]["title"],"Off but readable");
        std::fs::write(&file,r#"{"name":"sleeping","title":"Updated metadata"}"#).unwrap();catalog.refresh(&specs);assert_eq!(catalog.facts("sleeping")["manifest"]["title"],"Updated metadata");
        std::fs::write(&file,r#"{"name":"wrong"}"#).unwrap();catalog.refresh(&specs);assert!(catalog.facts("sleeping")["manifest_error"].as_str().unwrap().contains("name"));
        std::fs::write(&file,vec![b' ';FILE_BYTES as usize+1]).unwrap();catalog.refresh(&specs);assert!(catalog.facts("sleeping")["manifest_error"].as_str().unwrap().contains("32 KiB"));
    }
}
