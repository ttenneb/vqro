//! Linux process-death regression; the contract itself is only inherited stdin EOF.
#![cfg(target_os = "linux")]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use prim::client;
use serde_json::json;

fn process(pid: u32) -> Option<(String, bool)> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let fields: Vec<_> = stat.rsplit_once(')')?.1.split_whitespace().collect();
    Some((fields.get(19)?.to_string(), *fields.first()? != "Z"))
}

struct Core {
    child: Child,
    owned: Vec<(u32, String)>,
    root: tempfile::TempDir,
}

impl Core {
    fn track(&mut self, pid: u32) {
        if let Some((birth, _)) = process(pid) {
            self.owned.push((pid, birth));
        }
    }

    fn live(&self) -> Vec<u32> {
        self.owned.iter().filter_map(|(pid, birth)| {
            process(*pid).filter(|(now, live)| now == birth && *live).map(|_| *pid)
        }).collect()
    }

    fn signal(&self, signal: &str) {
        assert!(Command::new("/bin/kill").env_clear().args([signal, &self.child.id().to_string()]).status().unwrap().success());
    }
}

impl Drop for Core {
    fn drop(&mut self) {
        // Even a red assertion owns cleanup: never scan/kill unrelated processes.
        let _ = self.child.kill();
        let _ = self.child.wait();
        for pid in self.live() {
            let _ = Command::new("/bin/kill").env_clear().args(["-KILL", &pid.to_string()]).status();
        }
    }
}

async fn until(mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while !predicate() {
        assert!(Instant::now() < deadline, "extension lifetime deadline exceeded");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn exercise(signal: &str, include_reader: bool) {
    let base = Path::new(env!("CARGO_TARGET_TMPDIR"));
    std::fs::create_dir_all(base).unwrap();
    let root = tempfile::Builder::new().prefix("life-").tempdir_in(base).unwrap();
    let socket = root.path().join("p.sock");
    assert!(socket.as_os_str().len() < 100);
    for dir in ["home", "config", "state", "run", "extensions"] {
        std::fs::create_dir(root.path().join(dir)).unwrap();
    }
    let python = root.path().join("helper.py");
    std::fs::write(&python, r#"
import pathlib, sys, threading
from prim_ext import Extension
if sys.argv[1] == 'connected':
    Extension()
# blocked deliberately never connects to the socket: lifetime must not depend
# on a responsive command thread, or on receiving socket EOF.
pathlib.Path(sys.argv[2]).write_text(str(__import__('os').getpid()))
threading.Event().wait()
"#).unwrap();
    let child = Command::new(env!("CARGO_BIN_EXE_prim"))
        .args(["serve", "--fake"]).env_clear()
        .env("HOME", root.path().join("home")).env("PATH", "/usr/bin:/bin")
        .env("XDG_CONFIG_HOME", root.path().join("config"))
        .env("XDG_STATE_HOME", root.path().join("state"))
        .env("XDG_RUNTIME_DIR", root.path().join("run"))
        .env("PRIM_STATE_DIR", root.path().join("state"))
        .env("PRIM_SOCKET", &socket)
        .env("PRIM_EXTENSIONS_DIR", root.path().join("extensions"))
        .env("PYTHONPATH", PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("extensions"))
        .current_dir(root.path())
        .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())
        .spawn().unwrap();
    let mut core = Core { child, owned: Vec::new(), root };
    let deadline = Instant::now() + Duration::from_secs(3);
    let peer = loop {
        match client::connect(&socket, "lifetime-test").await {
            Ok((peer, _incoming, _hello)) => break peer,
            Err(_) => {
                assert!(Instant::now() < deadline, "private core did not start");
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }
    };
    let names: &[&str] = if include_reader { &["connected", "blocked", "reader"] } else { &["connected", "blocked"] };
    for &name in names {
        let ready = core.root.path().join(format!("{name}.ready"));
        let argv = if name == "reader" {
            // A non-Python extension with no prim/socket awareness. EOF is its
            // only lifetime signal. No Linux parent-death API is involved.
            json!(["/bin/sh", "-c", "printf '%s' \"$$\" > \"$1\"; while IFS= read -r line; do :; done", "lifetime-reader", ready])
        } else {
            json!(["/usr/bin/python3", python, name, ready])
        };
        peer.call("workspace.apply", json!({"ops": [{"op": "set", "key": format!("extension/{name}"), "value": {"argv": argv}}]})).await.unwrap();
        until(|| std::fs::read_to_string(&ready).ok().and_then(|s| s.parse::<u32>().ok()).is_some()).await;
        let pid = std::fs::read_to_string(ready).unwrap().parse().unwrap();
        core.track(pid);
    }
    // Nonzero observation window asserts the lifetime pipe is kept open; this
    // is a property being tested, not a startup/completion sleep.
    let stable_until = Instant::now() + Duration::from_millis(100);
    while Instant::now() < stable_until {
        assert_eq!(core.live().len(), names.len(), "extension exited while core remained alive");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    core.signal(signal);
    until(|| core.child.try_wait().unwrap().is_some()).await;
    // A retained dead socket connection must not keep any extension alive.
    until(|| core.live().is_empty()).await;
}

#[tokio::test]
async fn killed_core_closes_extension_lifetime_pipes() {
    exercise("-KILL", true).await;
}

#[tokio::test]
async fn clean_core_shutdown_still_stops_extensions() {
    exercise("-INT", true).await;
}

#[tokio::test]
async fn term_core_stops_extensions_via_lifetime_eof() {
    exercise("-TERM", true).await;
}

#[tokio::test]
async fn killed_core_stops_helper_even_before_socket_connection() {
    exercise("-KILL", false).await;
}
