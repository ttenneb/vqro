//! Store-only history regressions. Large fixtures live in the agent's cargo scratch target.
use std::{path::PathBuf,sync::Arc,time::Instant};
use vqro::store::{Store,Op,HISTORY_BYTES,NOTE_BYTES};
use serde_json::{json,Value};
fn set(key:impl Into<String>,value:Value)->Op {Op::Set {key:key.into(),value,expect:None}}
fn root()->tempfile::TempDir {
    let base=PathBuf::from(env!("CARGO_TARGET_TMPDIR"));std::fs::create_dir_all(&base).unwrap();
    tempfile::Builder::new().prefix("history-").tempdir_in(base).unwrap()
}
#[test]
fn history_has_compact_undo_records_and_shares_unchanged_transactions_and_entries() {
    let mut store=Store::in_memory();
    let first=store.apply("user",None,vec![set("doc/a",json!({"body":"old"}))]).unwrap();
    let old=store.entries()["doc/a"].clone();
    let second=store.apply_attributed("ext:theme",Some("user"),None,vec![set("doc/a",json!({"body":"new"}))]).unwrap();
    assert!(Arc::ptr_eq(&store.history(2)[1],&first));
    assert!(Arc::ptr_eq(second.changes[0].before.as_ref().unwrap(),&old));
    let json=serde_json::to_value(&second).unwrap();
    assert!(json["changes"][0].get("after").is_none());
    assert_eq!(json["changes"][0]["after_version"],2);
    assert_eq!(json["on_behalf_of"],"user");
    store.revert("user",second.id).unwrap();
    assert_eq!(store.get("doc/a").unwrap().value["body"],"old");
    assert_eq!(store.history_bytes(),serde_json::to_vec(&store.history(500)).unwrap().len());
}
#[test]
fn near_limit_entries_evict_oldest_and_latest_undo_survives_restart() {
    let dir=root();let path=dir.path().join("workspace.json");let mut store=Store::open(path.clone()).unwrap();
    let mut first=0;let mut last=0;
    for i in 0..12 {let txn=store.apply("user",None,vec![set("doc/a",json!(format!("{i:03}{}","x".repeat(1024*1024-256))))]).unwrap();if i==0 {first=txn.id;}last=txn.id;}
    assert!(store.history_bytes()<=HISTORY_BYTES);
    assert!(std::fs::metadata(&path).unwrap().len()< (HISTORY_BYTES+1024*1024+1024)as u64);
    assert_eq!(store.revert("user",first).unwrap_err().kind(),"not_found");
    drop(store);let mut store=Store::open(path.clone()).unwrap();
    store.revert("user",last).unwrap();
    assert!(store.get("doc/a").unwrap().value.as_str().unwrap().starts_with("010"));
    assert!(store.history_bytes()<=HISTORY_BYTES);
    assert!(!std::fs::read_to_string(&path).unwrap().starts_with("{\n"));
}
#[test]
fn oversized_undo_is_an_explicit_boundary_not_an_amplifying_revert_loop() {
    let mut store=Store::in_memory();
    store.apply("user",None,(0..10).map(|i|set(format!("doc/k{i}"),json!("x".repeat(900*1024)))).collect()).unwrap();
    let deleted=store.apply("user",None,(0..10).map(|i|Op::Delete {key:format!("doc/k{i}"),expect:None}).collect()).unwrap();
    assert!(!deleted.undoable);
    assert!(!store.history(1)[0].undoable);
    assert!(store.history(1)[0].changes.iter().all(|change|change.before.is_none()));
    let last=store.history(1)[0].id;let bytes=store.history_bytes();
    for _ in 0..30 {assert_eq!(store.revert("user",deleted.id).unwrap_err().kind(),"unavailable");}
    assert_eq!(store.history(1)[0].id,last);assert!(store.entries().is_empty());assert_eq!(store.history_bytes(),bytes);assert!(bytes<=HISTORY_BYTES);
    // A subsequent small reversible change remains undoable on the new side of that boundary.
    let small=store.apply("user",None,vec![set("doc/small",json!(1))]).unwrap();
    store.revert("user",small.id).unwrap();assert!(store.get("doc/small").is_none());
}
#[test]
fn notes_reject_growth_atomically_and_legacy_pretty_full_after_history_migrates() {
    let dir=root();let path=dir.path().join("workspace.json");
    let legacy=json!({"rev":2,"next_txn":3,"entries":{"doc/a":{"version":2,"value":"new"}},"history":[
        {"id":1,"author":"user","at_ms":1,"changes":[{"key":"doc/a","before":null,"after":{"version":1,"value":"old"}}]},
        {"id":2,"author":"ext:theme","on_behalf_of":"user","at_ms":2,"note":"z".repeat(NOTE_BYTES+200),"changes":[{"key":"doc/a","before":{"version":1,"value":"old"},"after":{"version":2,"value":"new"}}]}
    ]});
    std::fs::write(&path,serde_json::to_vec_pretty(&legacy).unwrap()).unwrap();
    let mut store=Store::open(path.clone()).unwrap();
    assert!(store.history(1)[0].note.as_ref().unwrap().ends_with("... [truncated]"));
    let disk=std::fs::read(&path).unwrap();
    assert_eq!(store.apply("user",Some("x".repeat(NOTE_BYTES+1)),vec![set("doc/no",json!(1))]).unwrap_err().kind(),"too_large");
    assert_eq!(std::fs::read(&path).unwrap(),disk);assert!(store.get("doc/no").is_none());
    store.revert("user",2).unwrap();assert_eq!(store.get("doc/a").unwrap().value,"old");
    drop(store);let loaded=Store::open(path.clone()).unwrap();
    assert_eq!(loaded.history(2)[1].on_behalf_of.as_deref(),Some("user"));
    let encoded=std::fs::read_to_string(path).unwrap();assert!(!encoded.contains("\n  "));assert!(!encoded.contains("\"after\":"));
}
#[test]
fn count_horizon_applies_even_when_records_are_tiny() {
    let mut store=Store::in_memory();
    for i in 0..520 {store.apply("user",None,vec![set("doc/n",json!(i))]).unwrap();}
    assert_eq!(store.history(1000).len(),500);
    assert_eq!(store.history(1000).last().unwrap().id,21);
    assert!(store.history_bytes()<HISTORY_BYTES);
}

#[test]
#[ignore="large legacy pretty-file migration; release full gate"]
fn full_legacy_pretty_history_migrates_and_undo_works_after_restart() {
    #[derive(serde::Serialize)] #[serde(untagged)]
    enum OldValue<'a> {View {kind:&'a str},Layout {tabs:&'a Value,active:usize}}
    #[derive(serde::Serialize)] struct OldEntry<'a> {version:u64,value:OldValue<'a>}
    #[derive(serde::Serialize)] struct OldChange<'a> {key:&'a str,before:Option<OldEntry<'a>>,after:Option<OldEntry<'a>>}
    #[derive(serde::Serialize)] struct OldTxn<'a> {id:u64,author:&'a str,on_behalf_of:&'a str,at_ms:u64,changes:Vec<OldChange<'a>>}
    #[derive(serde::Serialize)] struct OldFile<'a> {rev:u64,next_txn:u64,entries:std::collections::BTreeMap<&'a str,OldEntry<'a>>,history:Vec<OldTxn<'a>>}
    let dir=root();let path=dir.path().join("workspace.json");
    let mut tabs=json!((0..600).map(|i|json!({"title":format!("Tab {i}"),"node":{"view":"p"}})).collect::<Vec<_>>());
    let missing=37*1024-serde_json::to_vec(&json!({"tabs":tabs,"active":0})).unwrap().len();
    tabs[0]["title"]=json!(format!("Tab 0{}","x".repeat(missing)));
    let entry=|version:u64,active:usize|OldEntry {version,value:OldValue::Layout {tabs:&tabs,active}};
    let mut entries=std::collections::BTreeMap::new();
    entries.insert("view/p",OldEntry {version:1,value:OldValue::View {kind:"panel"}});
    entries.insert("layout/main",entry(501,499));
    let history=(1..=500).map(|id|OldTxn {id,author:"ext:theme",on_behalf_of:"user:legacy",at_ms:id,
        changes:vec![OldChange {key:"layout/main",before:if id==1 {None}else{Some(entry(id,(id-2)as usize))},after:Some(entry(id+1,(id-1)as usize))}]}).collect();
    serde_json::to_writer_pretty(std::io::BufWriter::new(std::fs::File::create(&path).unwrap()),&OldFile {rev:501,next_txn:501,entries,history}).unwrap();
    let legacy_bytes=std::fs::metadata(&path).unwrap().len();assert!(legacy_bytes>50*1024*1024);
    let mut store=Store::open(path.clone()).unwrap();
    assert_eq!(std::fs::metadata(&path).unwrap().len(),legacy_bytes,"load never rewrites the legacy file");
    assert!(store.history_bytes()<=HISTORY_BYTES);assert!(store.history(500).len()<500);
    assert_eq!(store.history(1)[0].id,500);
    let undone=store.revert("user",500).unwrap();assert_eq!(undone.id,501);
    assert_eq!(store.get("layout/main").unwrap().value["active"],498);
    assert_eq!(store.get("layout/main").unwrap().version,502);
    assert!(std::fs::metadata(&path).unwrap().len()<(HISTORY_BYTES+40*1024)as u64);
    drop(store);let mut store=Store::open(path).unwrap();
    assert_eq!(store.history(2)[1].on_behalf_of.as_deref(),Some("user:legacy"));
    store.revert("user",501).unwrap();assert_eq!(store.get("layout/main").unwrap().value["active"],499);
}

#[test]
#[ignore="release durable 500x37KiB latency/size regression; scripts/check.sh full"]
fn history_500_layouts_have_bounded_file_and_commit_p99() {
    let dir=root();let path=dir.path().join("workspace.json");let mut store=Store::open(path.clone()).unwrap();
    store.apply("seed",None,vec![set("view/p",json!({"kind":"panel"}))]).unwrap();
    let mut layout=json!({"tabs":(0..600).map(|i|json!({"title":format!("Tab {i}"),"node":{"view":"p"}})).collect::<Vec<_>>(),"active":0});
    let missing=37*1024-serde_json::to_vec(&layout).unwrap().len();
    layout["tabs"][0]["title"]=json!(format!("Tab 0{}","x".repeat(missing)));
    let mut samples=Vec::new();
    for i in 0..500 {layout["active"]=json!(i%600);let start=Instant::now();store.apply("user",None,vec![set("layout/main",layout.clone())]).unwrap();samples.push(start.elapsed().as_secs_f64()*1000.);}
    samples.sort_by(f64::total_cmp);let p99=samples[494];
    assert!(std::fs::metadata(&path).unwrap().len()<(HISTORY_BYTES+40*1024)as u64);
    assert!(p99<200.,"release p99 {p99}ms exceeds200ms for bounded8MiB history");
    eprintln!("500x37KiB file={} history={} p99_ms={p99}",std::fs::metadata(&path).unwrap().len(),store.history_bytes());
}
