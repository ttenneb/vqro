//! Bounded diagnostic logs, never file I/O under Core's mutex. No unbounded output queue.
use std::{io::{self,Write},path::{Path,PathBuf},sync::Mutex};
use tokio::io::AsyncReadExt;

pub(crate) const LOG_BYTES:u64=1024*1024;
// Old/new generations may overlap during cleanup. Opening on every append under this lock
// keeps them on the current file, and makes rotation/size admission atomic across generations.
// This lock is taken ONLY on a blocking worker, never under the core lock or on an async worker.
static APPEND:Mutex<()>=Mutex::new(());
fn append(path:&Path,bytes:&[u8])->io::Result<()> {
    let _guard=APPEND.lock().unwrap_or_else(|e|e.into_inner());
    let mut file=std::fs::OpenOptions::new().create(true).append(true).open(path)?;
    let mut len=file.metadata()?.len();
    if len>LOG_BYTES { file.set_len(LOG_BYTES)?; len=LOG_BYTES; }
    if len.saturating_add(bytes.len() as u64)>LOG_BYTES {
        drop(file);
        let backup=path.with_extension("log.1");
        match std::fs::remove_file(&backup) {Ok(())=>{},Err(e) if e.kind()==io::ErrorKind::NotFound=>{},Err(e)=>return Err(e)}
        std::fs::rename(path,backup)?;
        file=std::fs::OpenOptions::new().create(true).append(true).open(path)?;
    }
    file.write_all(bytes)
}

pub(crate) async fn drain(mut stdout:tokio::process::ChildStdout,mut stderr:tokio::process::ChildStderr,mut path:Option<PathBuf>) {
    let (mut out,mut err)=(true,true);
    let (mut a,mut b)=([0u8;16*1024],[0u8;16*1024]);
    while out || err {
        let bytes=tokio::select! {
            n=stdout.read(&mut a), if out => match n {Ok(n) if n>0=>a[..n].to_vec(),_=>{out=false;continue;}},
            n=stderr.read(&mut b), if err => match n {Ok(n) if n>0=>b[..n].to_vec(),_=>{err=false;continue;}},
        };
        if let Some(file)=path.clone() {
            // One bounded chunk/job per extension. Cancellation never releases APPEND before
            // an already-dispatched filesystem operation has actually finished.
            if !matches!(tokio::task::spawn_blocking(move ||append(&file,&bytes)).await,Ok(Ok(()))) {
                eprintln!("vqro: extension log unavailable; discarding diagnostic output");
                path=None; // Keep draining: a full/broken log must not deadlock its child.
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn one_generation_and_overlapping_writers_stay_within_two_fixed_files() {
        let dir=tempfile::tempdir().unwrap();let path=dir.path().join("ext-noisy.log");
        std::fs::write(&path,vec![b'x';2*LOG_BYTES as usize]).unwrap(); // Legacy oversize is bounded too.
        std::thread::scope(|scope| {
            for byte in [b'a',b'b'] {let path=&path;scope.spawn(move|| {for _ in 0..200 {append(path,&[byte;16*1024]).unwrap();}});}
        });
        let files=std::fs::read_dir(dir.path()).unwrap().map(|e|e.unwrap().path()).collect::<Vec<_>>();
        assert_eq!(files.len(),2);
        for file in files {assert!(std::fs::metadata(file).unwrap().len()<=LOG_BYTES);}
        let active=std::fs::read(path).unwrap();
        assert!(!active.is_empty());assert!(active.iter().all(|b|*b==b'a'||*b==b'b'));
    }
}
