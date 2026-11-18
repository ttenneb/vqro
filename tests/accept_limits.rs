//! Real per-process descriptor pressure; every lowered limit applies only to an owned child.
#![cfg(target_os="linux")]
use std::{path::PathBuf,process::{Child,Command,Stdio},time::{Duration,Instant}};
use std::os::unix::process::CommandExt;
use vqro::{client,rpc::{Peer,IncomingReceiver}};
use serde_json::json;
struct Running(Child);
impl Drop for Running {fn drop(&mut self){let _=self.0.kill();let _=self.0.wait();}}
struct Fixture {root:tempfile::TempDir,core:Running,control:Peer,_incoming:IncomingReceiver}
impl Fixture {
 async fn new(soft:u64,hard:u64)->Self {
  let root=tempfile::Builder::new().prefix("pacc").tempdir_in("/tmp").unwrap();
  for name in ["home","state","ext","run"] {std::fs::create_dir(root.path().join(name)).unwrap();}
  let log=std::fs::File::create(root.path().join("core.log")).unwrap();
  let binary=std::env::var_os("ACCEPT_CORE_BIN").unwrap_or_else(||env!("CARGO_BIN_EXE_vqro").into());
  let mut cmd=Command::new(binary);cmd.args(["serve","--fake"]).env_clear().env("HOME",root.path().join("home")).env("PATH","/usr/bin:/bin")
   .env("VQRO_STATE_DIR",root.path().join("state")).env("VQRO_SOCKET",root.path().join("p")).env("VQRO_EXTENSIONS_DIR",root.path().join("ext"))
   .env("VQRO_HERDR",r#"["/bin/false"]"#).env("XDG_RUNTIME_DIR",root.path().join("run")).env("HC_SOCKET",root.path().join("h")).env("HC_SESSION","accept-limit")
   .stdin(Stdio::null()).stdout(Stdio::null()).stderr(log);
  // SAFETY: the child-only pre-exec hook uses only async-signal-safe setrlimit and errno.
  unsafe {cmd.pre_exec(move || {let limit=libc::rlimit{rlim_cur:soft,rlim_max:hard};if libc::setrlimit(libc::RLIMIT_NOFILE,&limit)!=0 {Err(std::io::Error::last_os_error())}else{Ok(())}});}
  let core=Running(cmd.spawn().unwrap());let until=Instant::now()+Duration::from_secs(5);
  let(control,incoming,_)=loop {match client::connect(&root.path().join("p"),"accept-control").await {Ok(p)=>break p,Err(_) if Instant::now()<until=>tokio::time::sleep(Duration::from_millis(10)).await,Err(e)=>panic!("private core startup: {e}")}};
  Self {root,core,control,_incoming:incoming}
 }
 fn socket(&self)->PathBuf {self.root.path().join("p")}
 fn log(&self)->String {std::fs::read_to_string(self.root.path().join("core.log")).unwrap()}
}
impl Drop for Fixture {fn drop(&mut self){self.control.close();let _=self.core.0.kill();let _=self.core.0.wait();}}
#[tokio::test]
#[ignore="private real EMFILE pressure; full gate"]
async fn emfile_accept_backoff_keeps_existing_clients_and_recovers_new_admission() {
 let mut f=Fixture::new(64,64).await;let mut sockets=Vec::new();
 for _ in 0..100 {match tokio::time::timeout(Duration::from_millis(100),tokio::net::UnixStream::connect(f.socket())).await {Ok(Ok(socket))=>sockets.push(socket),_=>break}}
 let until=Instant::now()+Duration::from_secs(3);
 while !f.log().contains("listener resource pressure") {
  assert!(f.core.0.try_wait().unwrap().is_none(),"core exited on accept pressure: {}",f.log());
  assert!(Instant::now()<until,"did not hit real descriptor pressure: {}",f.log());tokio::time::sleep(Duration::from_millis(20)).await;
 }
 f.control.call_with_timeout("hello",json!({"client":"still-healthy"}),Duration::from_secs(1)).await.unwrap();
 drop(sockets);
 let until=Instant::now()+Duration::from_secs(5);
 loop {match client::connect(&f.socket(),"after-pressure").await {Ok((peer,_rx,_))=>{peer.close();break;},Err(_) if Instant::now()<until=>tokio::time::sleep(Duration::from_millis(20)).await,Err(error)=>panic!("accept did not recover: {error}; {}",f.log())}}
 assert!(f.core.0.try_wait().unwrap().is_none());
}
#[tokio::test]
#[ignore="private child nofile startup policy; full gate"]
async fn soft_nofile_is_raised_only_to_inherited_hard_limit() {
 let f=Fixture::new(64,128).await;
 let limits=std::fs::read_to_string(format!("/proc/{}/limits",f.core.0.id())).unwrap();
 let row=limits.lines().find(|line|line.starts_with("Max open files")).unwrap().split_whitespace().collect::<Vec<_>>();
 assert_eq!((row[3],row[4]),("128","128"));
}
#[tokio::test(flavor="multi_thread",worker_threads=4)]
#[ignore="private full connection-cap admission; full gate"]
async fn connection_cap_refuses_clearly_and_releases_slots_on_eof() {
 // This throwaway test process also needs room to own the other ends of2048 sockets.
 let mut limit=libc::rlimit{rlim_cur:0,rlim_max:0};
 unsafe {assert_eq!(libc::getrlimit(libc::RLIMIT_NOFILE,&mut limit),0);if limit.rlim_cur<4096 {limit.rlim_cur=4096;assert!(limit.rlim_max>=4096);assert_eq!(libc::setrlimit(libc::RLIMIT_NOFILE,&limit),0);}}
 let f=Fixture::new(4096,4096).await;let mut sockets=Vec::new();
 for _ in 1..vqro::server::MAX_CONNECTIONS {sockets.push(tokio::net::UnixStream::connect(f.socket()).await.unwrap());}
 // Accept callbacks may still be catching up; their count is independent of hello registration.
 tokio::time::sleep(Duration::from_millis(100)).await;
 let error=match client::connect(&f.socket(),"over-cap").await {Ok((peer,_,_))=>{peer.close();panic!("connection above cap admitted")},Err(error)=>error};
 let rpc=error.downcast_ref::<vqro::rpc::RpcError>().unwrap_or_else(||panic!("missing structured refusal: {error}"));
 assert_eq!(rpc.kind(),"overloaded");assert_eq!(rpc.data.as_ref().unwrap()["reason"],"connection_limit");
 f.control.call("hello",json!({"client":"healthy-at-cap"})).await.unwrap();drop(sockets);
 let until=Instant::now()+Duration::from_secs(5);
 loop {match client::connect(&f.socket(),"released-capacity").await {Ok((peer,_rx,_))=>{peer.close();break;},Err(_) if Instant::now()<until=>tokio::time::sleep(Duration::from_millis(20)).await,Err(error)=>panic!("connection slots not released: {error}")}}
}
