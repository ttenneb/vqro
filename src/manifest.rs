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
    pub fn facts(&self,name:&str)->Value {
        match self.records.get(name) {
            Some(r)=>json!({"manifest":r.value,"manifest_source":r.path.to_string_lossy(),"manifest_error":r.error}),
            None=>json!({"manifest":null,"manifest_source":null,"manifest_error":if self.overflow{Some("Manifest catalog exceeds 1 MiB; metadata omitted")}else{None}}),
        }
    }
}
#[cfg(test)] mod tests {
    use super::*;
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
