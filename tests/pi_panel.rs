#[allow(dead_code)] #[path="../src/bin/vqro-bridge-pi/panel.rs"] mod panel;
use serde_json::json;
#[test]
fn transcript_is_derived_bounded_utf8_and_never_renders_terminal_controls(){
    let mut docs=Vec::new();for n in 0..300{docs.push(json!({"position":n,"role":"assistant","content":[{"type":"text","text":format!("{n}: {}","é".repeat(100))}]}));}
    docs.last_mut().unwrap()["native"]=json!({"errorMessage":"\u{1b}[31mNative failure\u{7}"});
    let rendered=panel::transcript(docs.iter().map(|value|("message",value)).collect(),false);let rows=rendered["items"].as_array().unwrap();assert!(rows.len()<=panel::ROWS+2);
    let body:usize=rows.iter().skip(2).map(|row|row["text"].as_str().unwrap().len()).sum();assert!(body<=panel::BYTES);assert!(serde_json::to_vec(&rendered).unwrap().len()<32768);
    let text=rows.iter().map(|row|row["text"].as_str().unwrap()).collect::<Vec<_>>().join("\n");assert!(!text.contains('\u{1b}'));assert!(!text.contains('\u{7}'));assert!(text.contains("Native failure"));assert!(text.contains("299:"));assert_eq!(rendered["truncated"],true);
}
#[test]
fn tool_images_and_runtime_notices_are_visible_without_a_second_document(){
    let message=json!({"position":1,"role":"user","content":[{"type":"image","data":"must-not-render"}]});let tool=json!({"position":2,"title":"bash","status":"completed"});let notice=json!({"position":3,"role":"system","content":[{"type":"text","text":"Native retry scheduled (attempt 1)"}]});
    let panel=panel::transcript(vec![("message",&message),("tool",&tool),("message",&notice)],true);let text=panel.to_string();assert!(text.contains("image: inspect"));assert!(!text.contains("must-not-render"));assert!(text.contains("bash: completed"));assert!(text.contains("system> Native retry"));assert_eq!(panel["output_gap"],true);
}
#[test]
fn blocked_fallback_writer_keeps_only_latest_frame_without_blocking_producer(){
    use std::sync::{Arc,Mutex,Condvar,mpsc};let gate=Arc::new((Mutex::new(false),Condvar::new()));let worker=gate.clone();let (entered,ready)=mpsc::channel();let (sent,frames)=mpsc::channel();let mut first=true;
    let output=panel::Fallback::test_writer(move|frame|{if first{first=false;entered.send(()).unwrap();let(lock,wake)=&*worker;let mut release=lock.lock().unwrap();while !*release{release=wake.wait(release).unwrap();}}sent.send(String::from_utf8(frame.to_vec()).unwrap()).is_ok()});
    assert!(output.enabled());let facts=json!({"data":{"agent":{"state":"idle","lifecycle":"live","queue":[],"requests":[]}},"_core_connected":false});
    output.show(&facts,&json!({"items":[{"text":"first"}]}));ready.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
    output.show(&facts,&json!({"items":[{"text":"superseded"}]}));output.show(&facts,&json!({"items":[{"text":"latest"}]}));
    {let(lock,wake)=&*gate;*lock.lock().unwrap()=true;wake.notify_all();}
    assert!(frames.recv_timeout(std::time::Duration::from_secs(2)).unwrap().contains("first"));let next=frames.recv_timeout(std::time::Duration::from_secs(2)).unwrap();assert!(next.contains("latest"));assert!(!next.contains("superseded"));assert!(next.contains("core unavailable"));
}
