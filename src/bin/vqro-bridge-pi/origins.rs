//! Core origin for user messages this bridge itself sent (agent-v0 §3.2: "the inserted message carries `origin`").
//!
//! Pi echoes no id: its user message is `{role, content, timestamp}` (pi-coding-agent agent-session.js `_prepareRun`,
//! `_queueSteer`/`_queueFollowUp`). So the link is an evidence-gated correlation, and everything else stays without
//! origin (agent-v0 §4: "uncorrelated native messages still have no inferred core origin/delivery"):
//! - **started**: Pi answered this send's `prompt` with `disposition: "started"` (sent before the run's events,
//!   `_startRun` calls `onStarted` before `agent.prompt`). The next user message is the run's starting message; it
//!   gets the origin only if its text and images are exactly what the bridge wrote.
//! - **queued**: Pi answered `disposition: "queued"`. Pi removes a delivered queue entry and emits `queue_update`
//!   just before that user message's `message_start` (agent-session.js, the `message_start` handler). The message
//!   gets the origin only if it is the oldest bridge entry of that queue, with exact text, and Pi's queue held no
//!   other identical text the bridge did not send.
//! Pi rewriting the text (image hints, input handlers, templates), `handled`, a cleared queue, a new session or a
//! bridge restart leaves the message without origin. Receipts never change here (text is not stage evidence, §2.2).
use std::collections::VecDeque;
use std::hash::{Hash,Hasher};
use serde_json::{json,Value};

const PENDING:usize=32; // Pi's unresolved-call bound

#[derive(Clone,Debug)]
pub struct Entry {pub delivery:String,pub origin:Value,queue:&'static str,text:String,images:Vec<(String,u64,usize)>}

#[derive(Default,Debug)]
pub struct Origins {
    awaiting:Option<Entry>,     // the one gated prompt whose ACK has not been observed yet
    starting:Option<Entry>,     // ACKed `started`: the next user message is its run's starting message
    queued:VecDeque<Entry>,     // ACKed `queued`, oldest first
    queues:(Vec<String>,Vec<String>), // Pi's last reported steering / followUp texts
    delivered:Option<(&'static str,String)>, // the queue entry Pi removed for the next user message
}

fn image_key(block:&Value)->Option<(String,u64,usize)>{
    let data=block["data"].as_str()?;let mut hasher=std::collections::hash_map::DefaultHasher::new();data.hash(&mut hasher);
    Some((block["mimeType"].as_str()?.to_string(),hasher.finish(),data.len()))
}

impl Entry {
    /// From the exact native `prompt` arguments the bridge writes (contract::send).
    pub fn new(delivery:&str,origin:&Value,native:&Value)->Option<Self>{
        let queue=if native["streamingBehavior"]=="steer"{"steering"}else{"followUp"};
        let images=match native.get("images"){None=>vec![],Some(images)=>images.as_array()?.iter().map(image_key).collect::<Option<Vec<_>>>()?};
        Some(Self {delivery:delivery.into(),origin:origin.clone(),queue,text:native["message"].as_str()?.to_string(),images})
    }
    /// Exactly the bridge's text and images, in Pi's user content shape (one text block, then images).
    fn matches(&self,message:&Value)->bool{
        let Some(blocks)=message["content"].as_array() else {return message["content"].as_str()==Some(self.text.as_str()) && self.images.is_empty();};
        let Some((first,rest))=blocks.split_first() else {return false;};
        first["type"]=="text" && first["text"].as_str()==Some(self.text.as_str()) && rest.len()==self.images.len()
            && rest.iter().zip(&self.images).all(|(block,key)|block["type"]=="image" && image_key(block).as_ref()==Some(key))
    }
}

fn text(message:&Value)->Option<String>{
    if let Some(text)=message["content"].as_str(){return Some(text.into());}
    Some(message["content"].as_array()?.iter().filter(|block|block["type"]=="text").filter_map(|block|block["text"].as_str()).collect())
}
fn strings(value:&Value)->Option<Vec<String>>{value.as_array()?.iter().map(|v|v.as_str().map(str::to_string)).collect()}

impl Origins {
    /// Inside the start gate, before the write: the gate admits one prompt at a time, so one ACK is awaited.
    pub fn arm(&mut self,entry:Option<Entry>){self.awaiting=entry;}
    /// Nothing was written (refused): forget it.
    pub fn disarm(&mut self,delivery:&str){if self.awaiting.as_ref().is_some_and(|entry|entry.delivery==delivery){self.awaiting=None;}}
    /// A new session, epoch or reconnect: nothing pending can be correlated any more.
    pub fn reset(&mut self){*self=Self::default();}
    /// Pi's `prompt` response, observed in order with the events.
    pub fn prompt_response(&mut self,record:&Value){
        let Some(entry)=self.awaiting.take() else {return;};
        match (record["success"]==true,record["data"]["disposition"].as_str()) {
            (true,Some("started"))=>self.starting=Some(entry),
            (true,Some("queued"))=>{self.queued.push_back(entry);while self.queued.len()>PENDING{self.queued.pop_front();}},
            _=>{} // handled, failed or no disposition: no placement evidence
        }
    }
    /// Any native record other than a user `message_start`: a removal just noted was not a delivery after all (Pi emits
    /// the delivering `queue_update` immediately before that message's start), e.g. a one-entry `clear_queue`. That
    /// bridge entry has left Pi's queue undelivered; drop it so a later identical text is never attributed to it.
    pub fn other_record(&mut self){
        if let Some((queue,text))=self.delivered.take(){
            if let Some(index)=self.queued.iter().position(|entry|entry.queue==queue && entry.text==text){self.queued.remove(index);}
        }
    }
    /// Pi's `queue_update`: note a single removed entry (delivery), and drop bridge entries Pi no longer holds.
    pub fn queue_update(&mut self,record:&Value){
        let (Some(steering),Some(follow))=(strings(&record["steering"]),strings(&record["followUp"])) else {self.queued.clear();self.delivered=None;return;};
        let removed=|old:&Vec<String>,new:&Vec<String>|->Option<String>{
            if old.len()!=new.len()+1 {return None;}
            let mut rest=new.clone();for text in old {if let Some(i)=rest.iter().position(|t|t==text){rest.remove(i);}else{return Some(text.clone());}}None
        };
        let (old_steering,old_follow)=std::mem::replace(&mut self.queues,(steering,follow));
        self.delivered=match (removed(&old_steering,&self.queues.0),removed(&old_follow,&self.queues.1)) {
            (Some(text),None) if old_follow==self.queues.1=>Some(("steering",text)),
            (None,Some(text)) if old_steering==self.queues.0=>Some(("followUp",text)),
            _=>None,
        };
        // Bridge entries can never outnumber Pi's identical texts in that queue (clear_queue, lost entries).
        let (queues,delivered)=(&self.queues,&self.delivered);
        let mut kept=VecDeque::new();
        for entry in self.queued.drain(..) {
            let held=if entry.queue=="steering"{&queues.0}else{&queues.1};
            let pending=delivered.as_ref().is_some_and(|(queue,text)|*queue==entry.queue && *text==entry.text) as usize;
            let same=kept.iter().filter(|e:&&Entry|e.queue==entry.queue && e.text==entry.text).count();
            if same < held.iter().filter(|t|**t==entry.text).count()+pending {kept.push_back(entry);}
        }
        self.queued=kept;
    }
    /// A user `message_start`: the origin to publish on it, if the evidence holds; otherwise None.
    pub fn user_message(&mut self,message:&Value)->Option<Entry>{
        if let Some(entry)=self.starting.take() {return entry.matches(message).then_some(entry);}
        let (queue,delivered)=self.delivered.take()?;
        if text(message).as_deref()!=Some(delivered.as_str()) {return None;}
        let index=self.queued.iter().position(|entry|entry.queue==queue)?;
        if self.queued[index].text!=delivered {return None;} // a message the bridge did not send was delivered first
        let entry=self.queued.remove(index)?;
        // Ambiguous if Pi held more identical texts in this queue (before removing this one) than the bridge sent.
        let held=if queue=="steering"{&self.queues.0}else{&self.queues.1};
        let identical_in_pi=held.iter().filter(|t|**t==delivered).count()+1;
        let identical_from_bridge=self.queued.iter().filter(|e|e.queue==queue && e.text==delivered).count()+1;
        (identical_in_pi==identical_from_bridge && entry.matches(message)).then_some(entry)
    }
}

/// The fields a correlated user message carries (agent-v0 §4 `message`: `delivery?`, `origin?`).
pub fn fields(entry:&Entry)->Value{json!({"delivery":entry.delivery,"origin":entry.origin})}
