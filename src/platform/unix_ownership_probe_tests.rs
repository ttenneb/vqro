use super::unix_ownership_probe::*;
use crate::pty_ownership::{OwnershipStatus, ServerKey, ServerLiveness};
use std::io::{Read, Write};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::time::{Duration, Instant};

struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "op-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .subsec_nanos()
        ));
        super::create_private_package_directory(&root).expect("private probe root");
        Self(root)
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn serve(
    root: &Root,
    response: Vec<u8>,
    delay: Duration,
) -> (PathBuf, std::thread::JoinHandle<()>) {
    let path = root.0.join("p.sock");
    let listener = UnixListener::bind(&path).expect("private endpoint");
    listener.set_nonblocking(true).expect("bounded accept");
    let worker = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if let Ok((mut stream, _)) = listener.accept() {
                stream
                    .set_read_timeout(Some(Duration::from_millis(200)))
                    .expect("timeout");
                let mut request = [0_u8; 1024];
                let _ = stream.read(&mut request);
                std::thread::sleep(delay);
                let _ = stream.write_all(&response);
                return;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    });
    (path, worker)
}

fn key() -> ServerKey {
    super::ownership_server_key().expect("native process incarnation")
}

#[test]
fn live_status_is_bound_to_kernel_server_incarnation_not_response_json() {
    let root = Root::new();
    let key = key();
    let status = OwnershipStatus {
        server: key.clone(),
        epoch: 7,
        complete: true,
    };
    let response = format!("{}\n", serde_json::json!({"id":"ownership-proof","result":{"type":"pty_ownership_status","status":status}})).into_bytes();
    let (path, worker) = serve(&root, response, Duration::ZERO);
    let observed = ownership_status_at(&path, &key, Instant::now() + Duration::from_secs(1));
    worker.join().expect("private endpoint worker");
    assert_eq!(observed.expect("kernel-bound status"), status);
}

#[test]
fn wrong_birth_unsupported_method_oversize_and_timeout_are_read_only_errors() {
    for (n, response, delay) in [
        (0, b"{}\n".to_vec(), Duration::ZERO),
        (1, vec![b'x'; 9000], Duration::ZERO),
        (2, b"{}\n".to_vec(), Duration::from_millis(150)),
    ] {
        let root = Root::new();
        let (path, worker) = serve(&root, response, delay);
        let duration = if n == 2 {
            Duration::from_millis(30)
        } else {
            Duration::from_secs(1)
        };
        assert!(ownership_status_at(&path, &key(), Instant::now() + duration).is_err());
        worker.join().expect("private endpoint worker");
    }
    let root = Root::new();
    let (path, worker) = serve(&root, b"{}\n".to_vec(), Duration::ZERO);
    let mut wrong = key();
    wrong.birth += 1;
    assert!(ownership_status_at(&path, &wrong, Instant::now() + Duration::from_secs(1)).is_err());
    worker.join().expect("private endpoint worker");
}

#[test]
fn native_liveness_ignores_only_positive_death_or_incarnation_change() {
    let key = key();
    assert!(matches!(
        ownership_server_liveness(&key, &key.boot_id),
        ServerLiveness::Live(_)
    ));
    let mut changed = key.clone();
    changed.birth += 1;
    assert!(matches!(
        ownership_server_liveness(&changed, &key.boot_id),
        ServerLiveness::Reused
    ));
    changed.pid = i32::MAX as u32;
    assert!(matches!(
        ownership_server_liveness(&changed, &key.boot_id),
        ServerLiveness::Dead
    ));
}
