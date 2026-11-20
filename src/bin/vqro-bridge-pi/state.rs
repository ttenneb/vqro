//! Pi event normalization. ItemJournal is the sole retained entity document, not a second mirror.
use std::collections::{BTreeMap,BTreeSet,HashMap,VecDeque};
use std::sync::Arc;
use serde_json::{json,Map,Value};
use vqro::provider_client::items::{Coverage,ItemJournal,JournalConfig};
use super::admission::Signals;
use super::native::{Event,Exit};

pub const ENTITY_BYTES:usize=60*1024; // Room for the whole snapshot/put envelope within64KiB.
pub const ENTITY_COUNT:usize=512;
pub const ENTITY_TOTAL:usize=512*1024;
pub struct Agent {
    pub journal:ItemJournal,
    pub signals:Arc<Signals>,
    pub incomplete:BTreeSet<String>,
    pub receipts:BTreeMap<String,Value>,
    pub queue:Option<Value>,pub queue_version:u64,
    pub configuration:super::configuration::Configuration,
    pub requests:super::requests::Requests,ui_cancels:Vec<(String,Value)>,ui_unconfirmed:BTreeSet<String>,pub ui_overflow:bool,
    receipt_order:VecDeque<String>,
    history:VecDeque<(String,String)>,
    tools:HashMap<String,String>,
    current_message:Option<String>,
    position:u64,serial:u64,epoch_number:u64,instance:String,boot:String,
    pub session:Option<Value>,pub run:Option<String>,pub completion:Option<Value>,
    reason:String,error:Option<String>,exit:Option<Exit>,caps:Value,
}
impl Agent {
    pub fn new(instance:String,signals:Arc<Signals>)->Self {
        let mut bytes=[0u8;16];getrandom::fill(&mut bytes).expect("fresh observation epoch");
        let boot=bytes.iter().map(|byte|format!("{byte:02x}")).collect::<String>();
        let mut journal=ItemJournal::new(format!("{boot}.e1"),JournalConfig {max_entities:ENTITY_COUNT,max_entity_bytes:ENTITY_TOTAL,snapshot_buffer_bytes:1024*1024,..Default::default()});
        journal.set_coverage(Coverage::Tail);
        let mut agent=Self {journal,signals,incomplete:BTreeSet::new(),receipts:BTreeMap::new(),queue:None,queue_version:0,configuration:Default::default(),requests:Default::default(),ui_cancels:Vec::new(),ui_unconfirmed:BTreeSet::new(),ui_overflow:false,receipt_order:VecDeque::new(),history:VecDeque::new(),tools:HashMap::new(),current_message:None,
            position:0,serial:0,epoch_number:1,instance,boot,session:None,run:None,completion:None,reason:"unknown".into(),error:None,exit:None,caps:Value::Null};
        agent.refresh();agent
    }
    fn next(&mut self,prefix:&str)->String {self.serial+=1;format!("{prefix}{}",self.serial)}
    pub fn epoch(&self)->&str {self.journal.epoch()}
    pub fn caps(&self)->&Value {&self.caps}
    pub fn state(&self)->&'static str {
        let current=self.signals.current();
        if self.exit.is_some() {"unknown"}
        else if self.requests.ids().iter().any(|id|self.journal.entity("request",id).is_some_and(|(_,value)|value["status"]=="pending")){"requires_action"}
        else if current.run_active || current.streaming==Some(true) || current.compacting==Some(true) {"running"}
        else if self.requests.has_unconfirmed() || !self.ui_unconfirmed.is_empty() || self.configuration.pending{"unknown"}
        else if current.idle() {"idle"} else {"unknown"}
    }
    fn declarations(&self)->Value {
        let current=self.signals.current();
        let images=!self.configuration.pending && current.metadata["model"]["input"].as_array().is_some_and(|types|types.iter().any(|v|v=="image"));
        let mut modes=json!({"start":{"busy":"reject","preflight":"serialized"},"admit":{}});
        for (mode,field,point) in [("boundary","steeringMode","after_tool_calls"),("settle","followUpMode","would_stop")] {
            if self.configuration.pending{continue;}
            if let Some(batch)=match current.metadata[field].as_str() {Some("one-at-a-time")=>Some("one"),Some("all")=>Some("all"),_=>None} {
                modes[mode]=json!({"point":point,"batch":batch,"batch_at":"consume","idle":"park"});
            }
        }
        json!({"agent":{"v":0,"modules":{"queue":{"v":0},"stop":{"v":0},"config":{"v":0}}},"host":{"kind":"process","endpoints":"one"},
            "agent.send":{"modes":modes,"content":{"types":if images {vec!["text","image"]} else {vec!["text"]},"shape":"text_then_media","max_bytes":super::contract::input_bytes()},
                "literal":"controlled_launch","origin":[{"as":"attributed_user","role":"user","frame":"sender_header"}],
                "correlation":"none","handled":"none","keys":null,"guards":{"session":"adapter","run":"none"},"receipts":{"retain":256}},
            "agent.interrupt":{"pending":"unknown","scope":"foreground","waits":"none"},"agent.receipt":{},
            "agent.respond":{"ack":"dispatched","methods":["choice","confirm","text"],"scopes":["operation"],"background_requests":false,"native_timeout":"unrepresentable_positive","max_pending":super::requests::MAX_PENDING,"request_bytes":super::requests::MAX_REQUEST_BYTES},
            "agent.configure":{},
            "agent.stop":{"covers":["foreground","queued"],"waits":"exit"},
            "agent.queue":{"actions":["list","clear"],"ids":"none","removed":{"content":["text"],"media":"unknown"}},
            "items.stream":{"retain_bytes":131072,"entities":ENTITY_COUNT,"entity_bytes":ENTITY_TOTAL,"snapshot_buffer_bytes":1048576,"max_viewers":4}})
    }
    pub fn provider_capabilities(&self)->Map<String,Value> {
        fn bound(value:Value)->Value {match value {
            Value::String(s)=>json!([s]),Value::Object(object)=>Value::Object(object.into_iter().filter(|(key,_)|key!="rev").map(|(key,value)|(key,bound(value))).collect()),
            Value::Array(values)=>Value::Array(values.into_iter().map(|v|if v.is_object(){bound(v)}else{v}).collect()),other=>other,
        }}
        let mut caps=self.declarations();
        // Registration covers either consume-time batch setting and either native model input class.
        for (mode,point) in [("boundary","after_tool_calls"),("settle","would_stop")] {
            caps["agent.send"]["modes"][mode]=json!({"point":point,"batch":["one","all"],"batch_at":"consume","idle":"park"});
        }
        caps["agent.send"]["content"]["types"]=json!(["text","image"]);
        bound(caps).as_object().unwrap().clone()
    }
    pub fn facts(&self,provider:&str,cwd:&str)->Value {
        let phase=match self.state() {"running"=>"working","requires_action"=>"blocked","idle" if self.completion.as_ref().is_some_and(|c|c["reason"]=="error")=>"error","idle"=>"idle",_=>"working"};
        let mut facts=json!({"ref":"agent","incarnation":"","kind":"agent","state":if self.exit.is_some(){"finished"}else{"live"},"name":"Pi","cwd":cwd,"exit":self.exit.as_ref().map(|exit|json!({"code":exit.code,"success":exit.success})),
            "status":{"state":phase},
            "data":{"agent":{"v":0,"state":self.state(),"lifecycle":if self.exit.is_some(){"ended"}else{"live"},
                "host":{"id":format!("{}.child",self.boot),"provider":provider,"instance":self.instance,"incarnation":"","kind":"process","owns_lifecycle":true},
                "session":self.session,"epoch":self.epoch(),"caps":self.caps,"completion":self.completion,"queue":self.queue,"requests":self.request_summaries(),"background":null,
                "config":self.configuration.options(),"commands":[],"usage":null,"incomplete":self.incomplete,
                "items":{"retain_bytes":131072,"entities":ENTITY_COUNT,"entity_bytes":ENTITY_TOTAL,"snapshot_buffer_bytes":1048576,"coverage":"tail"}}}});
        if self.configuration.incomplete {facts["data"]["agent"]["incomplete"].as_array_mut().unwrap().push(json!("config.models_or_levels"));}
        // Optional UI catalogs never displace requests/current state or exceed the64KiB facts contract.
        for field in ["commands","config"]{if vqro::limits::encoded_bytes(&facts["data"])>60*1024{facts["data"]["agent"][field]=json!([]);facts["data"]["agent"]["incomplete"].as_array_mut().unwrap().push(json!(field));}}
        facts
    }
    pub fn configuration_pending(&mut self,pending:bool){self.configuration.pending=pending;self.configuration.rebuild(&self.signals.current().metadata);self.refresh();}
    pub fn refresh(&mut self) {
        let mut caps=self.declarations();
        let rev=self.caps["rev"].as_u64().unwrap_or(0);
        let mut prior=self.caps.clone();if let Some(object)=prior.as_object_mut(){object.remove("rev");}
        caps["rev"]=json!(if prior==caps {rev}else{rev+1});self.caps=caps;
        let value=json!({"state":self.state(),"run":self.run,"completion":self.completion,"background":null});
        self.store("state","current",value,false);
    }
    pub fn receipt(&mut self,delivery:&str,mut value:Value) {
        // A late ACK may win the race with the provider-deadline dispatched reply; never downgrade it.
        if self.receipts.get(delivery).is_some_and(|old|old["stage"]=="admitted" && value["stage"]=="dispatched") {return;}
        value["delivery"]=json!(delivery);
        // Native has at most32 unresolved calls. Keep those in addition to256 completed receipts.
        if value["stage"]!="dispatched" && !self.receipt_order.iter().any(|id|id==delivery) {self.receipt_order.push_back(delivery.into());}
        self.receipts.insert(delivery.into(),value.clone());
        while self.receipt_order.len()>256 {if let Some(old)=self.receipt_order.pop_front(){self.receipts.remove(&old);}}
        self.store("receipt",delivery,value,true);
    }
    pub fn request_summaries(&self)->Vec<Value>{
        self.requests.ids().iter().filter_map(|id|self.journal.entity("request",id).map(|(_,value)|value)).filter(|value|value["status"]=="pending").map(|value|{
            let mut summary=json!({"request":value["request"],"revision":value["revision"],"method":value["method"],"title":value["title"],"status":value["status"],"incomplete":value["incomplete"]});
            if let Some(choices)=value.get("choices"){summary["choices"]=choices.clone();}
            if vqro::limits::encoded_bytes(&value["params"])<=2048{summary["params"]=value["params"].clone();}else{summary["incomplete"]=json!(["params"]);}
            summary
        }).collect()
    }
    fn queue_ui_cancel(&mut self,id:String,native:Value){if self.ui_unconfirmed.len()>=16{self.ui_overflow=true;self.incomplete.insert("native_ui".into());return;}self.ui_unconfirmed.insert(id.clone());self.ui_cancels.push((id,native));}
    pub fn take_ui_cancels(&mut self)->Vec<(String,Value)>{std::mem::take(&mut self.ui_cancels)}
    fn make_prunable(&mut self,kind:&str,id:&str){if !self.history.iter().any(|(k,i)|k==kind && i==id){self.history.push_back((kind.into(),id.into()));}}
    pub fn answer_written(&mut self,id:&str,cancelled:bool){
        if let Some((_,value))=self.journal.entity("request",id){let mut value=value.clone();value["status"]=json!(if cancelled{"cancelled"}else{"answered"});value["native"]["response"]=json!("dispatched");value["native"]["waiting"]=json!("unknown");self.store("request",id,value,false);}
        self.refresh();
    }
    pub fn answer_fenced(&mut self,id:&str){
        self.requests.finish(id);self.ui_unconfirmed.remove(id);
        if let Some((_,value))=self.journal.entity("request",id){let mut value=value.clone();value["native"]["waiting"]=json!("not_waiting");self.store("request",id,value,false);self.make_prunable("request",id);}
        self.refresh();
    }
    pub fn cancel_requests(&mut self,reason:&str){
        for (id,native) in self.requests.cancel_all(){
            if let Some((_,value))=self.journal.entity("request",&id){let mut value=value.clone();value["status"]=json!("cancelled");value["native"]["reason"]=json!(reason);value["native"]["waiting"]=json!("unknown");self.store("request",&id,value,false);self.make_prunable("request",&id);}
            self.queue_ui_cancel(id,native);
        }
    }
    fn ui(&mut self,record:&Value){
        match self.requests.observe(record){
            super::requests::Incoming::Dialog {id,value,replaced}=>{
                if let Some(old)=replaced{if let Some((_,value))=self.journal.entity("request",&old){let mut value=value.clone();value["status"]=json!("expired");self.store("request",&old,value,false);self.make_prunable("request",&old);}}
                self.store("request",&id,value,false);
                if self.journal.entity("request",&id).is_none(){
                    if let Some(native)=self.requests.cancel_one(&id){self.queue_ui_cancel(id,native);}self.incomplete.insert("requests".into());
                }
            }
            super::requests::Incoming::Cancel {id,native,reason,replaced}=>{
                if let Some(old)=replaced{if let Some((_,value))=self.journal.entity("request",&old){let mut value=value.clone();value["status"]=json!("expired");self.store("request",&old,value,false);self.make_prunable("request",&old);}}
                self.store("request",&id,json!({"request":id,"revision":1,"method":"_unrepresentable","title":reason,"scope":{"operation":format!("ui-{id}")},"status":"cancelled","incomplete":true}),true);
                self.queue_ui_cancel(id,native);self.incomplete.insert("native_ui".into());
            }
            super::requests::Incoming::Notice {id,value}=>{
                if vqro::limits::encoded_bytes(&value)<=ENTITY_BYTES{self.store("_ui",&id,value,true);}else{self.incomplete.insert("native_ui".into());}
            }
        }
    }
    pub fn invalidate_queue(&mut self) {self.queue=None;self.queue_version=self.queue_version.wrapping_add(1);}
    pub fn control_result(&mut self,id:&str,value:Value) {self.store("_control",id,value,true);}
    fn prune(&mut self,except:(&str,&str))->bool {
        let count=self.history.len();
        for _ in 0..count {
            let Some((kind,id))=self.history.pop_front() else {break;};
            let active=self.current_message.as_deref()==Some(&id) || (except.0!="request" && self.tools.values().any(|tool|tool==&id))
                || (kind=="receipt" && self.receipts.get(&id).is_some_and(|receipt|receipt["stage"]=="dispatched"));
            if (kind.as_str(),id.as_str())==except || active {self.history.push_back((kind,id));continue;}
            if self.journal.entity(&kind,&id).is_some() {let _=self.journal.remove(&kind,&id);return true;}
        }
        false
    }
    fn store(&mut self,kind:&str,id:&str,value:Value,prunable:bool) {
        if self.journal.entity(kind,id).is_some_and(|(_,old)|old==&value) {return;}
        let existed=self.journal.entity(kind,id).is_some();
        while !existed && self.journal.len()>=ENTITY_COUNT {if !self.prune((kind,id)){self.incomplete.insert("items".into());return;}}
        loop {
            match self.journal.put(kind,id,value.clone()) {
                Ok(_)=>{if !existed && prunable {self.history.push_back((kind.into(),id.into()));}break;},
                Err(error) if error.kind=="overloaded" && self.prune((kind,id))=>{},
                Err(_)=>{self.incomplete.insert("items".into());break;},
            }
        }
    }
    fn message_value(&mut self,message:&Value,final_value:bool,position:u64)->Value {
        let content=if let Some(text)=message["content"].as_str(){json!([{"type":"text","text":text}])}else if let Some(blocks)=message["content"].as_array(){
            Value::Array(blocks.iter().map(|block|{
                let mut block=block.clone();
                if block["type"]=="thinking" {let text=block["thinking"].clone();block.as_object_mut().unwrap().remove("thinking");block["type"]=json!("thought");block["text"]=text;}
                if block["type"]=="image" {if let Some(mime)=block.get("mimeType").cloned(){block.as_object_mut().unwrap().remove("mimeType");block["mime"]=mime;}}
                block
            }).collect())
        }else{Value::Null};
        let mut native=message.as_object().cloned().unwrap_or_default();native.remove("role");native.remove("content");
        let mut value=json!({"position":position,"role":message["role"],"content":content,"final":final_value,"native":native});
        if vqro::limits::encoded_bytes(&value)>ENTITY_BYTES {
            self.incomplete.insert("items".into());
            value=json!({"position":position,"role":message["role"],"content":[{"type":"text","text":"[Native message exceeds the declared entity bound; content omitted]"}],"final":final_value,"incomplete":["content"]});
        }
        value
    }
    fn message(&mut self,record:&Value,final_value:bool) {
        let message=&record["message"];
        if message["role"]=="toolResult" {return;}
        let id=if let Some(id)=&self.current_message {id.clone()}else{self.position+=1;let id=self.next("m");self.current_message=Some(id.clone());id};
        let position=self.journal.entity("message",&id).and_then(|(_,value)|value["position"].as_u64()).unwrap_or(self.position);
        let value=self.message_value(message,final_value,position);
        let event=&record["assistantMessageEvent"];
        let field=event["contentIndex"].as_u64().map(|index|format!("/content/{index}/text"));
        let appended=if let (Some(field),Some(delta))=(field,event["delta"].as_str()) {
            let same=self.journal.entity("message",&id).and_then(|(_,old)|old.pointer(&field)).and_then(Value::as_str)
                .zip(value.pointer(&field).and_then(Value::as_str)).is_some_and(|(old,new)|new.len()==old.len()+delta.len() && new.starts_with(old) && new.ends_with(delta));
            same && !final_value && self.journal.append_text("message",&id,&field,delta).is_ok()
        }else{false};
        if !appended {self.store("message",&id,value,true);}
        if final_value {
            if message["role"]=="assistant" {
                self.reason=match message["stopReason"].as_str().unwrap_or("unknown") {"stop"=>"end_turn","length"=>"max_tokens","aborted"=>"cancelled",other=>other}.into();
                self.error=message["errorMessage"].as_str().map(|s|s.chars().take(1024).collect());
            }
            self.current_message=None;
        }
    }
    fn tool(&mut self,record:&Value,status:&str) {
        let Some(native)=record["toolCallId"].as_str().filter(|id|id.len()<=128) else{self.incomplete.insert("items".into());return;};
        let id=if let Some(id)=self.tools.get(native){id.clone()}else{
            if self.tools.len()>=32 {self.incomplete.insert("items".into());return;}
            self.position+=1;let id=self.next("t");self.tools.insert(native.into(),id.clone());id
        };
        let mut value=self.journal.entity("tool",&id).map(|(_,value)|value.clone()).unwrap_or_else(||json!({"position":self.position,"title":record["toolName"],"input":record["args"]}));
        value["status"]=json!(status);
        if let Some(result)=record.get("result").or_else(||record.get("partialResult")){
            value["content"]=result["content"].clone();
            let mut extras=result.as_object().cloned().unwrap_or_default();extras.remove("content");value["native"]=json!(extras);
        }
        if vqro::limits::encoded_bytes(&value)>ENTITY_BYTES {self.incomplete.insert("items".into());value=json!({"position":value["position"],"title":record["toolName"],"status":status,"incomplete":["input","content"]});}
        self.store("tool",&id,value,true);
        if matches!(status,"completed"|"failed"|"cancelled"){self.tools.remove(native);}
    }
    pub fn observe(&mut self,event:&Event) {
        match event {
            Event::Exited(exit)=>{self.exit=Some(exit.clone());self.run=None;self.ui_unconfirmed.clear();
                for id in self.requests.ids(){self.requests.finish(&id);if let Some((_,value))=self.journal.entity("request",&id){let mut value=value.clone();value["status"]=json!("expired");self.store("request",&id,value,false);self.make_prunable("request",&id);}}
            },
            Event::ProtocolError(_)=>{self.incomplete.insert("native".into());},
            Event::Record(record)=>match record["type"].as_str() {
                Some("extension_ui_request")=>self.ui(record),
                Some("queue_update")=>{
                    self.queue=super::queue::summary(record);self.queue_version=self.queue_version.wrapping_add(1);
                    if self.queue.is_none(){self.incomplete.insert("queue".into());}else{self.incomplete.remove("queue");}
                }
                Some("response") if record["command"]=="get_available_models" && record["success"]==true=>{self.configuration.observe_models(&record["data"]);self.configuration.rebuild(&self.signals.current().metadata);},
                Some("response") if record["command"]=="get_available_thinking_levels" && record["success"]==true=>{self.configuration.observe_levels(&record["data"]);self.configuration.rebuild(&self.signals.current().metadata);},
                Some("response") if record["command"]=="get_state" && record["success"]==true=>{
                    self.configuration.rebuild(&self.signals.current().metadata);
                    if let Some(count)=record["data"]["pendingMessageCount"].as_u64(){
                        if count==0 {self.queue=Some(json!([]));self.queue_version=self.queue_version.wrapping_add(1);}
                        else if self.queue.as_ref().and_then(Value::as_array).is_some_and(|rows|rows.len() as u64!=count){self.invalidate_queue();}
                    }
                    if let Some(id)=record["data"]["sessionId"].as_str().filter(|id|!id.is_empty() && id.len()<=128) {
                        if self.session.as_ref().is_some_and(|session|session["id"]!=id) {
                            self.cancel_requests("epoch_changed");self.epoch_number+=1;self.journal.new_epoch(format!("{}.e{}",self.boot,self.epoch_number));self.journal.set_coverage(Coverage::Tail);self.history.clear();self.tools.clear();self.current_message=None;self.run=None;self.completion=None;
                        }
                        let mut session=json!({"id":id});
                        if let Some(title)=record["data"]["sessionName"].as_str().filter(|s|s.len()<=512){session["title"]=json!(title);}
                        else if !record["data"]["sessionName"].is_null(){self.incomplete.insert("session.title".into());}
                        if let Some(model)=record["data"]["model"]["id"].as_str().filter(|s|s.len()<=256){
                            session["model"]=json!({"id":model});
                            if let Some(label)=record["data"]["model"]["name"].as_str().filter(|s|s.len()<=512){session["model"]["label"]=json!(label);}
                            if let Some(effort)=record["data"]["thinkingLevel"].as_str().filter(|s|s.len()<=64){session["model"]["effort"]=json!(effort);}
                        }
                        if let Some(cwd)=record["data"]["cwd"].as_str().filter(|s|s.len()<=4096){session["cwd"]=json!(cwd);}
                        self.session=Some(session);
                        self.store("session","current",self.session.clone().unwrap(),false);
                    }else{self.incomplete.insert("session".into());}
                }
                Some("agent_start") if self.exit.is_none()=>{if self.run.is_none(){self.run=Some(self.next("run"));self.reason="unknown".into();self.error=None;}},
                Some("agent_settled") if self.exit.is_none()=>{
                    if let Some(run)=self.run.take(){let reason=if record["aborted"]==true{"cancelled"}else{&self.reason};self.completion=Some(json!({"id":run,"reason":reason,"error":self.error.as_ref().map(|message|json!({"message":message}))}));}
                }
                Some("message_start")=>{self.current_message=None;self.message(record,false);},
                Some("message_update")=>self.message(record,false),Some("message_end")=>self.message(record,true),
                Some("tool_execution_start")=>self.tool(record,"running"),Some("tool_execution_update")=>self.tool(record,"running"),
                Some("tool_execution_end")=>self.tool(record,if record["isError"]==true{"failed"}else{"completed"}),
                _=>{}
            },
        }
        self.refresh();
    }
}
