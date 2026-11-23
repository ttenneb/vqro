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
    if len>LOG_BYTES {
        #[cfg(unix)]
        file.set_len(LOG_BYTES)?;
        #[cfg(windows)]
        trim_with_write_handle(path)?;
        len=LOG_BYTES;
    }
    if len.saturating_add(bytes.len() as u64)>LOG_BYTES {
        drop(file);
        let backup=path.with_extension("log.1");
        match std::fs::remove_file(&backup) {Ok(())=>{},Err(e) if e.kind()==io::ErrorKind::NotFound=>{},Err(e)=>return Err(e)}
        std::fs::rename(path,backup)?;
        file=std::fs::OpenOptions::new().create(true).append(true).open(path)?;
    }
    file.write_all(bytes)
}

#[cfg(any(windows,test))]
fn trim_with_write_handle(path:&Path)->io::Result<()> {
    // Rust append handles omit FILE_WRITE_DATA, which FileEndOfFileInfo requires.
    // Use a short-lived write-capable handle; keep normal writes append-only.
    std::fs::OpenOptions::new().write(true).open(path)?.set_len(LOG_BYTES)
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
    fn write_handle_caps_legacy_file_without_replacing_it() {
        let dir=tempfile::tempdir().unwrap();let path=dir.path().join("ext-legacy.log");
        std::fs::write(&path,vec![b'x';2*LOG_BYTES as usize]).unwrap();
        let reader=std::fs::File::open(&path).unwrap();
        trim_with_write_handle(&path).unwrap();
        assert_eq!(reader.metadata().unwrap().len(),LOG_BYTES);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(),1);
        assert_eq!(std::fs::read(&path).unwrap(),vec![b'x';LOG_BYTES as usize]);
    }

    #[cfg(windows)]
    #[test]
    fn oversized_log_is_trimmed_with_write_data_access_before_rotation() {
        let dir=tempfile::tempdir().unwrap();let path=dir.path().join("ext-legacy.log");
        std::fs::write(&path,vec![b'x';2*LOG_BYTES as usize]).unwrap();
        let append_only=std::fs::OpenOptions::new().append(true).open(&path).unwrap();
        assert_eq!(append_only.set_len(LOG_BYTES).unwrap_err().kind(),io::ErrorKind::PermissionDenied,
            "control: Windows append-only rights cannot resize");
        drop(append_only);
        append(&path,b"new output").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(),b"new output");
        assert_eq!(std::fs::metadata(path.with_extension("log.1")).unwrap().len(),LOG_BYTES);
    }

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
