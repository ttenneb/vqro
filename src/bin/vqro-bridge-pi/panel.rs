//! Bounded, derived compatibility panel. The ItemJournal remains the only retained document.
use serde_json::{json,Value};
pub const ROWS:usize=128;
pub const BYTES:usize=16*1024;

#[derive(Default)]
struct Output {pending:Option<String>,last:String,stopped:bool}
#[derive(Default)]
pub struct Fallback {output:Option<std::sync::Arc<(std::sync::Mutex<Output>,std::sync::Condvar)>>}
impl Fallback {
    fn writer(mut write:impl FnMut(&[u8])->bool+Send+'static)->Self{
        let output=std::sync::Arc::new((std::sync::Mutex::new(Output::default()),std::sync::Condvar::new()));let worker=output.clone();
        std::thread::Builder::new().name("pi-fallback".into()).spawn(move||loop{
            let (lock,wake)=&*worker;let mut state=lock.lock().unwrap();while state.pending.is_none() && !state.stopped{state=wake.wait(state).unwrap();}if state.stopped{break;}let frame=state.pending.take().unwrap();drop(state);if !write(frame.as_bytes()){break;}
        }).expect("fallback writer thread");Self {output:Some(output)}
    }
    pub fn tty()->Self{
        if unsafe{libc::isatty(1)}!=1{return Self::default();}
        // Raw writes never hold Rust's global stdout lock. A stalled terminal cannot block the native
        // reader or process shutdown; this detached thread is deliberately never joined on exit.
        Self::writer(|bytes|{let mut at=0;while at<bytes.len(){let n=unsafe{libc::write(1,bytes[at..].as_ptr().cast(),bytes.len()-at)};if n>0{at+=n as usize;}else if std::io::Error::last_os_error().kind()!=std::io::ErrorKind::Interrupted{return false;}}true})
    }
    #[cfg(test)]
    #[allow(dead_code)] // Used by the separately compiled pi_panel integration test.
    pub fn test_writer(write:impl FnMut(&[u8])->bool+Send+'static)->Self{Self::writer(write)}
    pub fn enabled(&self)->bool{self.output.is_some()}
    pub fn show(&self,facts:&Value,content:&Value){
        let Some(output)=&self.output else{return;};let agent=&facts["data"]["agent"];let state=agent["state"].as_str().unwrap_or("unknown");
        let queued=agent["queue"].as_array().map(|rows|rows.len().to_string()).unwrap_or_else(||"unknown".into());let pending=agent["requests"].as_array().map_or(0,Vec::len);
        let lifecycle=agent["lifecycle"].as_str().unwrap_or("unknown");let core=match facts["_core_connected"].as_bool(){Some(true)=>"connected",Some(false)=>"unavailable",None=>"unknown"};
        let mut frame=format!("\x1b[2J\x1b[HPi RPC bridge — {lifecycle}, {state}; core {core}\nRead-only monitor; use agent controls in vqro.\nQueue: {queued}; pending dialogs: {pending}\n");
        if let Some(requests)=agent["requests"].as_array(){for request in requests.iter().take(8){
            let mut text=format!("Request {}: {}",request["request"].as_str().unwrap_or("unknown"),request["title"].as_str().unwrap_or(""));
            if let Some(message)=request["params"]["message"].as_str(){text+=&format!(" — {message}");}if let Some(choices)=request["choices"].as_array(){for choice in choices{text+=&format!(" | {}",choice["label"].as_str().unwrap_or(""));}}
            let text:String=text.chars().filter(|c|!c.is_control()).collect();let (text,_)=tail(&text,1024);frame.push_str(text);frame.push('\n');
        }}frame.push('\n');
        if let Some(rows)=content["items"].as_array(){for row in rows{if let Some(text)=row["text"].as_str(){frame.push_str(text);frame.push('\n');}}}
        let (lock,wake)=&**output;let mut output=lock.lock().unwrap();if output.last!=frame{output.last=frame.clone();output.pending=Some(frame);wake.notify_one();}
    }
}
impl Drop for Fallback {fn drop(&mut self){if let Some(output)=&self.output{let (lock,wake)=&**output;lock.lock().unwrap().stopped=true;wake.notify_all();}}}

fn tail(text:&str,bytes:usize)->(&str,bool){let mut start=text.len().saturating_sub(bytes);while !text.is_char_boundary(start){start+=1;}(&text[start..],start>0)}
fn push(rows:&mut Vec<Value>,remaining:&mut usize,clipped:&mut bool,prefix:&str,text:&str,style:Option<&str>)->bool{
    if rows.len()>=ROWS || *remaining<=prefix.len(){*clipped=true;return false;}
    let (text,cut)=tail(text,*remaining-prefix.len());*clipped|=cut;
    let text=format!("{prefix}{}",text.chars().filter(|c|!c.is_control() || *c=='\t').collect::<String>());
    *remaining=remaining.saturating_sub(text.len());let mut row=json!({"type":"text","text":text});if let Some(style)=style{row["style"]=json!(style);}rows.push(row);true
}
pub fn transcript(mut entities:Vec<(&str,&Value)>,incomplete:bool)->Value{
    entities.sort_by_key(|(_,value)|value["position"].as_u64().unwrap_or(0));
    let (mut rows,mut remaining,mut clipped)=(Vec::new(),BYTES,false);
    'entities:for (kind,value) in entities.into_iter().rev(){
        match kind{
            "message"=>{
                if let Some(error)=value["native"]["errorMessage"].as_str(){for line in error.lines().rev(){if !push(&mut rows,&mut remaining,&mut clipped,"assistant error> ",line,Some("error")){break 'entities;}}}
                let role=match value["role"].as_str(){Some(role @ ("user"|"assistant"|"system"|"coordination"))=>role,_=>"native"};let prefix=format!("{role}> ");
                if let Some(blocks)=value["content"].as_array(){for block in blocks.iter().rev(){
                    if block["type"]=="text"{if let Some(text)=block["text"].as_str(){for line in text.lines().rev(){if !push(&mut rows,&mut remaining,&mut clipped,&prefix,line,None){break 'entities;}}}}
                    else if block["type"]=="image"{if !push(&mut rows,&mut remaining,&mut clipped,&prefix,"[image: inspect the item stream]",Some("dim")){break 'entities;}}
                }}
                if value.get("incomplete").is_some(){clipped=true;}
            }
            "tool"=>{let line=format!("{}: {}",value["title"].as_str().unwrap_or("tool"),value["status"].as_str().unwrap_or("unknown"));if !push(&mut rows,&mut remaining,&mut clipped,"tool> ",&line,Some("dim")){break;}},
            "_ui" if value["method"]=="notify"=>{if let Some(text)=value["native"]["message"].as_str(){for line in text.lines().rev(){if !push(&mut rows,&mut remaining,&mut clipped,"system> ",line,Some("dim")){break 'entities;}}}},
            _=>{}
        }
    }
    rows.reverse();rows.insert(0,json!({"type":"text","style":"muted","text":"Transcript tail; native history is not loaded. Full retained entities are available in the item stream."}));
    if clipped || incomplete{rows.insert(1,json!({"type":"text","style":"warn","text":"Bounded transcript: some output is omitted or unavailable."}));}
    json!({"title":"Pi transcript (tail)","items":rows,"truncated":clipped,"output_gap":incomplete})
}
