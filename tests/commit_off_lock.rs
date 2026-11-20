//! Private slow-writer regressions. Core has its own two-worker runtime so a blocked core cannot
//! also stop the test's deadlines. No live backend/vqro, fault environment or disk sleeps required.
use std::sync::{Arc, Condvar, Mutex, atomic::{AtomicBool, Ordering}};
use std::time::{Duration, Instant};
use base64::Engine;
use vqro::{client, core::Shared, rpc::{Incoming, Peer}, server::{self, Backend}, store};
use serde_json::{json, Value};
#[path="support/managed.rs"] mod managed;

struct Gate {
    entered: tokio::sync::Notify,
    released: Mutex<bool>,
    wake: Condvar,
    armed: AtomicBool,
}
impl Gate {
    fn new() -> Arc<Self> { Arc::new(Self { entered:tokio::sync::Notify::new(), released:Mutex::new(false), wake:Condvar::new(), armed:AtomicBool::new(true) }) }
    fn writer(self: &Arc<Self>) -> store::DurableWriter {
        let gate = self.clone();
        Arc::new(move |path, bytes| {
            if gate.armed.swap(false, Ordering::SeqCst) {
                gate.entered.notify_one();
                let mut released = gate.released.lock().unwrap();
                while !*released { released = gate.wake.wait(released).unwrap(); }
            }
            store::write_atomic(path, bytes)
        })
    }
    fn release(&self) { *self.released.lock().unwrap() = true; self.wake.notify_all(); }
}

struct Fixture {
    core: Shared,
    base:u64,
    socket: std::path::PathBuf,
    gate: Arc<Gate>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
    _dir: tempfile::TempDir,
}
impl Fixture {
    async fn new() -> Self {
        let dir = tempfile::Builder::new().prefix("cw").tempdir_in("/tmp").unwrap();
        let socket = dir.path().join("s"); let state = dir.path().join("state");
        let endpoint = socket.clone();
        let (ready, got) = std::sync::mpsc::channel();
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let thread = std::thread::spawn(move || {
            tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap().block_on(async {
                let core = server::start(endpoint, state, false, Backend::Fake, None).unwrap();
                ready.send(core).unwrap();
                let _ = stopped.await;
            });
        });
        let core = got.recv_timeout(Duration::from_secs(2)).unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            while !core.lock().unwrap().providers.get("fake").is_some_and(|p| p.synced) { tokio::time::sleep(Duration::from_millis(2)).await; }
        }).await.unwrap();
        let gate = Gate::new();
        core.lock().unwrap().configure_workspace_writer(gate.writer()).unwrap();
        let base=core.lock().unwrap().snapshot()["entries"]["doc/principals/owner"]["version"].as_u64().unwrap();
        Self { core, base, socket, gate, stop:Some(stop), thread:Some(thread), _dir:dir }
    }
    async fn peer(&self) -> (Peer, vqro::rpc::IncomingReceiver) {
        let (peer, incoming, _) = client::connect_as(&self.socket, "commit-test", "owner").await.unwrap();
        (peer, incoming)
    }
    async fn pending(&self, count:usize) {
        tokio::time::timeout(Duration::from_secs(2), async {
            while self.core.lock().unwrap().workspace_pending_commits() != count { tokio::time::sleep(Duration::from_millis(2)).await; }
        }).await.unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.gate.release();
        if let Some(stop) = self.stop.take() { let _ = stop.send(()); }
        if let Some(thread) = self.thread.take() { thread.join().unwrap(); }
    }
}
fn set(value: i64, expect: u64) -> Value { json!({"ops":[{"op":"set","key":"doc/value","value":value,"expect":expect}]}) }

#[tokio::test]
async fn retirement_rejects_queued_writes_but_adopts_an_already_dispatched_write() {
    let f=Fixture::new().await;
    f.gate.armed.store(false,Ordering::SeqCst);
    let (control,_)=f.peer().await;
    let token_file=f._dir.path().join("managed.token");
    let seed=control.call("workspace.apply",json!({"ops":[{"op":"set","key":"extension/managed","value":managed::spec(&token_file)}]})).await.unwrap();
    let (old,_old_rx)=f.peer().await;
    old.call("hello",json!({"extension":"managed","extension_token":managed::token(&token_file).await})).await.unwrap();
    f.gate.armed.store(true,Ordering::SeqCst);
    let active=tokio::spawn({let old=old.clone();async move {old.call("workspace.apply",set(1,0)).await}});
    tokio::time::timeout(Duration::from_secs(2),f.gate.entered.notified()).await.unwrap();
    let queued=tokio::spawn({let old=old.clone();async move {old.call("workspace.apply",set(2,0)).await}});
    f.pending(2).await;
    let revert=tokio::spawn({let old=old.clone();async move {old.call("workspace.revert",json!({"txn":seed["txn"]})).await}});
    f.pending(3).await;
    // Restart is not a workspace commit and must fence work already waiting in its queue.
    vqro::ext::restart(&f.core,"managed").unwrap();
    f.gate.release();
    let applied=active.await.unwrap().expect("dispatched durable work is not retrospectively rejected");
    assert!(applied["txn"].as_u64().is_some());
    for result in [queued.await.unwrap(),revert.await.unwrap()] {
        let error=result.unwrap_err();
        assert_eq!(error.kind(),"stale_generation");
        assert_eq!(error.data.unwrap()["outcome"],"rejected");
    }
    assert_eq!(control.call("workspace.get",json!({"key":"doc/value"})).await.unwrap()["value"],1);
    let disk=store::Store::open(f._dir.path().join("state/workspace.json")).unwrap();
    assert_eq!(disk.entries()["doc/value"].value,json!(1));
    control.call("core.shutdown",json!({})).await.unwrap();
}

#[tokio::test]
async fn slow_durable_write_does_not_block_snapshots_or_echo_and_stays_invisible() {
    let f = Fixture::new().await;
    let (writer, _) = f.peer().await;
    let (reader, _) = f.peer().await;
    let (terminal, mut output) = f.peer().await;
    let resource = terminal.call("resource.spawn", json!({"argv":["sh"]})).await.unwrap()["resource"]["id"].clone();
    let stream = terminal.call("resource.attach", json!({"resource":resource})).await.unwrap()["stream"].clone();
    let apply = tokio::spawn(async move { writer.call("workspace.apply", set(1, 0)).await });
    tokio::time::timeout(Duration::from_secs(2), f.gate.entered.notified()).await.unwrap();
    let start = Instant::now();
    let snapshot = reader.call_with_timeout("workspace.snapshot", Value::Null, Duration::from_millis(200));
    let echo = async {
        terminal.notify("stream.input", json!({"stream":stream,"text":"independent-echo"}));
        tokio::time::timeout(Duration::from_millis(200), async {
            loop {
                if let Some(Incoming::Notification { method, params }) = output.recv().await {
                    if method == "stream.data" {
                        let data = base64::engine::general_purpose::STANDARD.decode(params["data"].as_str().unwrap()).unwrap();
                        if String::from_utf8_lossy(&data).contains("independent-echo") { break; }
                    }
                }
            }
        }).await
    };
    let (snapshot, echo) = tokio::join!(snapshot, echo);
    let elapsed = start.elapsed();
    f.gate.release();
    let txn = apply.await.unwrap().unwrap();
    println!("unrelated snapshot={:?} echo={echo:?} elapsed_ms={:.2} txn={}", snapshot.as_ref().map(|_| "ok").map_err(|e| e.kind()), elapsed.as_secs_f64()*1000.0, txn["txn"]);
    let snapshot = snapshot.expect("snapshot must not wait for a durable writer holding no core state lock");
    assert!(snapshot["entries"].get("doc/value").is_none(), "pending candidate is not visible before durable adoption");
    assert!(echo.is_ok(), "stream echo must not wait for the state filesystem");
    assert_eq!(f.core.lock().unwrap().snapshot()["entries"]["doc/value"]["value"], 1);
}

#[tokio::test]
async fn concurrent_cas_applies_once_and_publication_order_matches_transaction_order() {
    let f = Fixture::new().await;
    let (a, _) = f.peer().await; let (b, _) = f.peer().await;
    let (observer, mut events) = f.peer().await;
    observer.call("events.subscribe", json!({"types":["entries"]})).await.unwrap();
    let first = tokio::spawn(async move { a.call("workspace.apply", set(1, 0)).await });
    tokio::time::timeout(Duration::from_secs(2), f.gate.entered.notified()).await.unwrap();
    let second = tokio::spawn(async move { b.call("workspace.apply", set(2, 0)).await });
    f.pending(2).await;
    f.gate.release();
    let first = first.await.unwrap().unwrap();
    assert_eq!(second.await.unwrap().unwrap_err().kind(), "conflict");
    let next = observer.call("workspace.apply", set(3, first["txn"].as_u64().unwrap())).await.unwrap();
    assert_eq!(next["txn"].as_u64().unwrap(), first["txn"].as_u64().unwrap()+1, "conflicts consume no transaction id");
    let mut observed = Vec::new();
    tokio::time::timeout(Duration::from_secs(2), async {
        while observed.len() < 2 {
            if let Some(Incoming::Notification { method, params }) = events.recv().await {
                if method == "event" && params["type"] == "entries" { observed.push(params["data"]["txn"].clone()); }
            }
        }
    }).await.unwrap();
    assert_eq!(observed, vec![first["txn"].clone(), next["txn"].clone()]);
}

#[tokio::test]
async fn queued_cas_and_default_revert_use_the_preceding_adopted_state() {
    let f = Fixture::new().await;
    let (a, _) = f.peer().await; let (b, _) = f.peer().await;
    let first = tokio::spawn(async move { a.call("workspace.apply", set(1, 0)).await });
    tokio::time::timeout(Duration::from_secs(2), f.gate.entered.notified()).await.unwrap();
    // This CAS is valid only AFTER the first queued transaction is adopted.
    let version=f.base+1;
    let second = tokio::spawn(async move { b.call("workspace.apply", set(2, version)).await });
    f.pending(2).await;
    let core = f.core.clone();
    let undo = tokio::spawn(async move { server::invoke_as(&core, "test", "owner", "vqro.revert", Value::Null).await });
    f.pending(3).await;
    f.gate.release();
    assert_eq!(first.await.unwrap().unwrap()["txn"], f.base+1);
    assert_eq!(second.await.unwrap().unwrap()["txn"], f.base+2);
    assert_eq!(undo.await.unwrap().unwrap()["txn"], f.base+3);
    assert_eq!(f.core.lock().unwrap().snapshot()["entries"]["doc/value"]["value"], 1, "default undo must select transaction2, not the pre-queue history");
}

#[tokio::test]
async fn cancelled_caller_cannot_suppress_adoption_or_extension_reconciliation() {
    let f = Fixture::new().await;
    f.gate.armed.store(false, Ordering::SeqCst);
    let (peer, _) = f.peer().await;
    peer.call("workspace.apply", json!({"ops":[{"op":"set","key":"extension/gated","value":{"argv":["true"],"enabled":false}}]})).await.unwrap();
    assert_eq!(f.core.lock().unwrap().exts["gated"].spec_version, f.base+1);
    f.gate.armed.store(true, Ordering::SeqCst);
    let core = f.core.clone();
    let invoke = tokio::spawn(async move { server::invoke(&core, "test", "vqro.extension.disable", json!({"name":"gated"})).await });
    tokio::time::timeout(Duration::from_secs(2), f.gate.entered.notified()).await.unwrap();
    invoke.abort();
    assert!(invoke.await.unwrap_err().is_cancelled());
    f.gate.release();
    f.pending(0).await;
    let mut core = f.core.lock().unwrap();
    assert_eq!(core.snapshot()["entries"]["extension/gated"]["version"], f.base+2);
    assert_eq!(core.exts["gated"].spec_version, f.base+2, "adoption hook belongs to the committer, not the cancelled receipt");
    assert_eq!(core.configure_workspace_writer(f.gate.writer()).unwrap_err().kind(), "conflict");
}

#[tokio::test]
async fn clean_failure_is_invisible_but_post_rename_failure_is_adopted_once() {
    for renamed in [false, true] {
        let f = Fixture::new().await;
        let first = AtomicBool::new(true);
        f.core.lock().unwrap().configure_workspace_writer(Arc::new(move |path, bytes| {
            if first.swap(false, Ordering::SeqCst) {
                if renamed { store::write_atomic(path, bytes)?; }
                let mut err = store::AtomicWriteError::from(std::io::Error::other("injected durable writer failure"));
                err.renamed = renamed;
                Err(err)
            } else { store::write_atomic(path, bytes) }
        })).unwrap();
        let (peer, _) = f.peer().await;
        let (observer, mut events) = f.peer().await;
        observer.call("events.subscribe", json!({"types":["entries"]})).await.unwrap();
        let err = peer.call("workspace.apply", set(1, 0)).await.unwrap_err();
        assert_eq!(err.kind(), if renamed { "durability_unconfirmed" } else { "io" });
        let snapshot = peer.call("workspace.snapshot", Value::Null).await.unwrap();
        let history = peer.call("workspace.history", Value::Null).await.unwrap();
        assert_eq!(snapshot["entries"].get("doc/value").is_some(), renamed);
        assert_eq!(history["txns"].as_array().unwrap().len(), f.base as usize+usize::from(renamed));
        if renamed {
            assert_eq!(err.data.unwrap()["txn"], f.base+1);
            assert_eq!(store::Store::open(f._dir.path().join("state/workspace.json")).unwrap().get("doc/value").unwrap().value, 1);
        }
        let next = peer.call("workspace.apply", set(2, if renamed{f.base+1}else{0})).await.unwrap();
        assert_eq!(next["txn"], f.base+if renamed { 2 } else { 1 });
        let expected = if renamed { vec![f.base+1, f.base+2] } else { vec![f.base+1] };
        let mut seen = Vec::new();
        tokio::time::timeout(Duration::from_secs(2), async {
            while seen.len() < expected.len() {
                if let Some(Incoming::Notification { method, params }) = events.recv().await {
                    if method == "event" && params["type"] == "entries" { seen.push(params["data"]["txn"].as_u64().unwrap()); }
                }
            }
        }).await.unwrap();
        assert_eq!(seen, expected);
        assert!(tokio::time::timeout(Duration::from_millis(20), events.recv()).await.is_err(), "no phantom or duplicate adopted event");
    }
}

#[tokio::test]
async fn commit_admission_counts_escaped_note_bytes_not_only_raw_text() {
    let f = Fixture::new().await;
    let (a, _) = f.peer().await; let (b, _) = f.peer().await; let (c, _) = f.peer().await;
    let mut authors=Vec::new();
    for peer in [&a,&b,&c] {authors.push(peer.call("hello",json!({"client":"commit-budget"})).await.unwrap()["actual_author"].as_str().unwrap().to_owned());}
    // Use real retained metadata, within the note cap. Three operation payloads leave room for
    // raw notes, but not their six-byte JSON escapes. Descriptive client labels are not authors.
    let budget=8*1024*1024;
    let note="\0".repeat(store::NOTE_BYTES);
    let input=||json!({"note":note,"ops":(0..3).map(|i|json!({"op":"set","key":format!("doc/escaped-{i}"),"value":"x".repeat(budget/9-store::NOTE_BYTES),"expect":0})).collect::<Vec<_>>()});
    let ops:Vec<store::Op>=serde_json::from_value(input()["ops"].clone()).unwrap();
    let payload=vqro::limits::encoded_bytes(&ops);
    let raw=3*payload+authors.iter().map(|author|author.len()+note.len()).sum::<usize>();
    let encoded=3*payload+authors.iter().map(|author|vqro::limits::encoded_bytes(&(author,None::<&str>,Some(&note)))).sum::<usize>();
    assert!(raw<budget && encoded>budget,"raw {raw}, encoded {encoded}, budget {budget}");
    let first_input = input(); let second_input = input();
    let first = tokio::spawn(async move { a.call("workspace.apply", first_input).await });
    tokio::time::timeout(Duration::from_secs(2), f.gate.entered.notified()).await.unwrap();
    let second = tokio::spawn(async move { b.call("workspace.apply", second_input).await });
    f.pending(2).await;
    let rejected = c.call_with_timeout("workspace.apply", input(), Duration::from_secs(2)).await;
    f.gate.release();
    let err = rejected.unwrap_err();
    assert_eq!(err.kind(), "overloaded");
    assert_eq!(err.data.unwrap()["outcome"], "rejected");
    assert_eq!(first.await.unwrap().unwrap()["txn"], f.base+1);
    assert_eq!(second.await.unwrap().unwrap_err().kind(), "conflict");
    f.pending(0).await;
}

#[tokio::test]
async fn descriptive_client_labels_are_not_duplicated_into_commit_metadata() {
    let f=Fixture::new().await;
    let (a,_)=f.peer().await;let (b,_)=f.peer().await;let (c,_)=f.peer().await;
    let label="\0".repeat(500*1024);
    let mut authors=Vec::new();
    for peer in [&a,&b,&c] {authors.push(peer.call("hello",json!({"client":label})).await.unwrap()["actual_author"].as_str().unwrap().to_owned());}
    assert!(authors.iter().all(|author|author.starts_with("client:")&&author.len()<64));
    let first=tokio::spawn(async move{a.call("workspace.apply",set(1,0)).await});
    tokio::time::timeout(Duration::from_secs(2),f.gate.entered.notified()).await.unwrap();
    let second=tokio::spawn(async move{b.call("workspace.apply",set(2,0)).await});f.pending(2).await;
    let reader=c.clone();let third=tokio::spawn(async move{c.call("workspace.apply",set(3,0)).await});f.pending(3).await;
    f.gate.release();assert_eq!(first.await.unwrap().unwrap()["txn"],f.base+1);
    assert_eq!(second.await.unwrap().unwrap_err().kind(),"conflict");assert_eq!(third.await.unwrap().unwrap_err().kind(),"conflict");f.pending(0).await;
    let history=reader.call("workspace.history",json!({"limit":10})).await.unwrap();
    assert_eq!(history["txns"][0]["author"],authors[0]);
    assert!(vqro::limits::encoded_bytes(&history)<4096,"client labels must not be retained per transaction");
}

#[tokio::test]
async fn revert_admission_charges_undo_payload_not_the_tiny_request() {
    let f=Fixture::new().await;f.gate.armed.store(false,Ordering::SeqCst);
    let (seed,_)=f.peer().await;
    let input=|value:&str|json!({"ops":(0..3).map(|i|json!({"op":"set","key":format!("doc/big{i}"),"value":value.repeat(1000*1024)})).collect::<Vec<_>>()});
    seed.call("workspace.apply",input("a")).await.unwrap();
    let changed=seed.call("workspace.apply",input("b")).await.unwrap()["txn"].clone();
    f.gate.armed.store(true,Ordering::SeqCst);
    let (a,_)=f.peer().await;let (b,_)=f.peer().await;let (c,_)=f.peer().await;
    let txn=changed.clone();let first=tokio::spawn(async move {a.call("workspace.revert",json!({"txn":txn})).await});
    tokio::time::timeout(Duration::from_secs(2),f.gate.entered.notified()).await.unwrap();
    let txn=changed.clone();let second=tokio::spawn(async move {b.call("workspace.revert",json!({"txn":txn})).await});
    f.pending(2).await;
    let rejected=c.call("workspace.revert",json!({"txn":changed})).await.unwrap_err();
    f.gate.release();
    assert_eq!(rejected.kind(),"overloaded");assert_eq!(rejected.data.unwrap()["outcome"],"rejected");
    first.await.unwrap().unwrap();assert_eq!(second.await.unwrap().unwrap_err().kind(),"conflict");
    f.pending(0).await;
}

#[tokio::test]
async fn default_undo_reaccounts_a_changed_target_before_copying_its_payload() {
    let f=Fixture::new().await;f.gate.armed.store(false,Ordering::SeqCst);
    let (seed,_)=f.peer().await;
    let big=||json!({"ops":(0..3).map(|i|json!({"op":"set","key":format!("doc/big{i}"),"value":"x".repeat(1000*1024)})).collect::<Vec<_>>()});
    seed.call("workspace.apply",big()).await.unwrap();
    f.gate.armed.store(true,Ordering::SeqCst);
    let (a,_)=f.peer().await;let (undo,_)=f.peer().await;let (b,_)=f.peer().await;let (c,_)=f.peer().await;
    let deleting=tokio::spawn(async move {a.call("workspace.apply",json!({"ops":(0..3).map(|i|json!({"op":"delete","key":format!("doc/big{i}")})).collect::<Vec<_>>()})).await});
    tokio::time::timeout(Duration::from_secs(2),f.gate.entered.notified()).await.unwrap();
    // Admission sees the old create (tiny undo); queue head sees deletion (3MiB undo).
    let undo=tokio::spawn(async move {undo.call("command.invoke",json!({"name":"vqro.revert"})).await});
    f.pending(2).await;
    let input=big();let next=tokio::spawn(async move {b.call("workspace.apply",input).await});
    let input=big();let last=tokio::spawn(async move {c.call("workspace.apply",input).await});
    f.pending(4).await;f.gate.release();deleting.await.unwrap().unwrap();
    let error=undo.await.unwrap().unwrap_err();
    assert_eq!(error.kind(),"overloaded");assert_eq!(error.data.unwrap()["outcome"],"rejected");
    next.await.unwrap().unwrap();last.await.unwrap().unwrap();f.pending(0).await;
}

#[tokio::test]
async fn default_undo_stops_at_an_oversized_record_instead_of_skipping_to_older_work() {
    let f=Fixture::new().await;f.gate.armed.store(false,Ordering::SeqCst);
    let (peer,_)=f.peer().await;
    for i in 0..10 {peer.call("workspace.apply",json!({"ops":[{"op":"set","key":format!("doc/large{i}"),"value":"x".repeat(900*1024)}]})).await.unwrap();}
    let deleted=peer.call("workspace.apply",json!({"ops":(0..10).map(|i|json!({"op":"delete","key":format!("doc/large{i}")})).collect::<Vec<_>>()})).await.unwrap();
    assert_eq!(deleted["undoable"],false);
    let error=peer.call("command.invoke",json!({"name":"vqro.revert"})).await.unwrap_err();
    assert_eq!(error.kind(),"unavailable");assert!(error.message.contains("undo history byte horizon"));
    assert_eq!(peer.call("workspace.history",json!({"limit":1})).await.unwrap()["txns"][0]["id"],deleted["txn"]);
}

#[tokio::test]
async fn shutdown_drains_admitted_write_and_its_reply_but_rejects_new_commits() {
    let f=Fixture::new().await;
    let (writer,_)=f.peer().await; let (other,_)=f.peer().await;
    let apply=tokio::spawn(async move { writer.call("workspace.apply",set(1,0)).await });
    tokio::time::timeout(Duration::from_secs(2),f.gate.entered.notified()).await.unwrap();
    let mut stop=tokio::spawn({let core=f.core.clone();async move { server::shutdown(&core).await }});
    let early=tokio::time::timeout(Duration::from_millis(50),&mut stop).await;
    let rejected=other.call_with_timeout("workspace.apply",set(2,0),Duration::from_secs(1)).await;
    f.gate.release();
    let applied=apply.await.unwrap();
    if early.is_err() { stop.await.unwrap(); }
    assert!(early.is_err(),"shutdown completed before its admitted durable write");
    assert_eq!(applied.unwrap()["txn"],f.base+1,"admitted commit reply must flush before peer closes");
    let error=rejected.unwrap_err();
    assert_eq!(error.kind(),"unavailable");
    assert_eq!(error.data.unwrap()["outcome"],"rejected");
}

#[tokio::test]
async fn writer_panic_is_indeterminate_and_fails_closed_without_replay() {
    let f = Fixture::new().await;
    let writer = f.gate.writer();
    f.core.lock().unwrap().configure_workspace_writer(Arc::new(move |path, bytes| {
        writer(path, bytes)?;
        panic!("injected writer crash after durable replace");
    })).unwrap();
    let (a, _) = f.peer().await; let (b, _) = f.peer().await; let (reader, _) = f.peer().await;
    let first = tokio::spawn(async move { a.call("workspace.apply", set(1, 0)).await });
    tokio::time::timeout(Duration::from_secs(2), f.gate.entered.notified()).await.unwrap();
    let queued = tokio::spawn(async move { b.call("workspace.apply", set(2, 0)).await });
    f.pending(2).await;
    f.gate.release();
    assert_eq!(first.await.unwrap().unwrap_err().kind(), "indeterminate");
    assert_eq!(queued.await.unwrap().unwrap_err().kind(), "indeterminate");
    f.pending(0).await;
    assert_eq!(reader.call("workspace.apply", set(3, 0)).await.unwrap_err().kind(), "unavailable");
    assert!(reader.call("workspace.snapshot", Value::Null).await.unwrap()["entries"].get("doc/value").is_none());
    let persisted = store::Store::open(f._dir.path().join("state/workspace.json")).unwrap();
    assert_eq!(persisted.get("doc/value").unwrap().value, 1, "no later queued write or replay may overwrite an uncertain rename");
    assert_eq!(persisted.history(10).len(), f.base as usize+1);
}

#[tokio::test]
async fn principal_config_ownership_observes_the_preceding_queued_owner_change() {
    let f=Fixture::new().await;let(owner,_)=f.peer().await;let(second,_)=f.peer().await;
    let change=tokio::spawn(async move{owner.call("workspace.apply",json!({"ops":[{"op":"set","key":"doc/principals/owner","value":"bob"}]})).await});
    tokio::time::timeout(Duration::from_secs(2),f.gate.entered.notified()).await.unwrap();
    let queued=tokio::spawn(async move{second.call("workspace.apply",json!({"ops":[{"op":"set","key":"doc/ordinary","value":1},{"op":"set","key":"doc/principals/delegations/c-d","value":{"from":"carol","to":"dave"}}]})).await});
    f.pending(2).await;f.gate.release();change.await.unwrap().unwrap();assert_eq!(queued.await.unwrap().unwrap_err().kind(),"forbidden");
    let snapshot=f.core.lock().unwrap().snapshot();assert!(snapshot["entries"].get("doc/ordinary").is_none());assert!(snapshot["entries"].get("doc/principals/delegations/c-d").is_none());
}

#[tokio::test]
async fn queued_delegation_is_rechecked_after_an_earlier_revocation_is_adopted() {
    let f=Fixture::new().await;f.gate.armed.store(false,Ordering::SeqCst);let(owner,_)=f.peer().await;
    owner.call("workspace.apply",json!({"ops":[{"op":"set","key":"doc/principals/delegations/b-a","value":{"from":"bob","to":"alice"}}]})).await.unwrap();
    let(alice,_,_)=client::connect_as(&f.socket,"delegate","alice").await.unwrap();f.gate.armed.store(true,Ordering::SeqCst);
    let revoke=tokio::spawn(async move{owner.call("workspace.apply",json!({"ops":[{"op":"delete","key":"doc/principals/delegations/b-a"}]})).await});
    tokio::time::timeout(Duration::from_secs(2),f.gate.entered.notified()).await.unwrap();
    let queued=tokio::spawn(async move{alice.call("workspace.apply",json!({"on_behalf_of":"bob","ops":[{"op":"set","key":"doc/after-revoke","value":1}]})).await});
    f.pending(2).await;f.gate.release();revoke.await.unwrap().unwrap();assert_eq!(queued.await.unwrap().unwrap_err().kind(),"forbidden");
    assert!(f.core.lock().unwrap().snapshot()["entries"].get("doc/after-revoke").is_none());
}
