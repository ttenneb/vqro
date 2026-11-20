//! Only native queue observations; no delivery ledger is used to reconstruct membership.
use serde_json::{json,Value};
fn arrays(value:&Value)->Option<(&Vec<Value>,&Vec<Value>)> {Some((value["steering"].as_array()?,value["followUp"].as_array()?))}
fn clip(text:&str,bytes:usize)->String {text.chars().scan(0,|used,ch|{*used+=ch.len_utf8();(*used<=bytes).then_some(ch)}).collect()}

pub fn summary(value:&Value)->Option<Value> {
    let (steering,follow)=arrays(value)?;
    if steering.len()+follow.len()>32 {return None;}
    let mut rows=Vec::new();
    for (mode,items) in [("boundary",steering),("settle",follow)] {
        for item in items {let text=item.as_str()?;rows.push(json!({"id":null,"mode":mode,"preview":clip(text,256)}));}
    }
    Some(json!(rows))
}
pub fn removed(value:&Value)->Value {
    let Some((steering,follow))=arrays(value) else {return json!({"removed":null,"incomplete":["removed"],"native":{"reason":"unrecognized_clear_result"}});};
    let counts=json!({"boundary":steering.len(),"settle":follow.len()});
    if steering.len()+follow.len()>64 {return json!({"removed":null,"counts":counts,"incomplete":["removed"],"native":{"reason":"result_count_bound"}});}
    let mut rows=Vec::new();
    for (mode,items) in [("boundary",steering),("settle",follow)] {
        for item in items {
            let Some(text)=item.as_str() else {return json!({"removed":null,"counts":counts,"incomplete":["removed"]});};
            rows.push(json!({"mode":mode,"content":[{"type":"text","text":text}]}));
        }
    }
    let result=json!({"removed":rows,"incomplete":["removed.media"],"native":{"media":"not_reported"}});
    if vqro::limits::encoded_bytes(&result)>48*1024 {json!({"removed":null,"counts":counts,"incomplete":["removed"],"native":{"reason":"result_byte_bound"}})}else{result}
}
