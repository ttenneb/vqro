//! Unix-only: write an executable fake script so that this test process never holds it open for writing.
//!
//! With tests running in parallel, another test's fork (before its exec) can inherit an open write descriptor
//! of a script this process is writing; executing the script then fails with ETXTBSY ("Text file busy"). A
//! short-lived child writes and marks the file instead (same fix as src/bin/vqro-rex/status_tests.rs).
use std::{io::Write, path::Path, process::{Command, Stdio}};

pub fn write_executable(path: &Path, bytes: &[u8]) {
    let mut writer = Command::new("/bin/sh")
        .args(["-c", "cat > \"$1\" && chmod 700 \"$1\"", "sh"]).arg(path)
        .stdin(Stdio::piped()).spawn().unwrap();
    writer.stdin.take().unwrap().write_all(bytes).unwrap();
    assert!(writer.wait().unwrap().success(), "could not write fake script {}", path.display());
}
