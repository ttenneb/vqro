use std::os::fd::{FromRawFd, OwnedFd};

use portable_pty::{native_pty_system, Child, CommandBuilder, PtySize};

use crate::pty::fd;

pub(crate) struct SpawnedPty {
    pub master_fd: OwnedFd,
    pub child: Box<dyn Child + Send + Sync>,
    pub ownership: Option<std::sync::Arc<crate::pty_ownership::publisher::OwnershipLease>>,
}

pub(crate) fn spawn_with_portable_pty(
    rows: u16,
    cols: u16,
    cmd: CommandBuilder,
) -> std::io::Result<SpawnedPty> {
    // Dirty before allocation/child exposure; registry failure never gates PTYs.
    let ownership_change = crate::pty_ownership::publisher::begin_change();
    let (master_fd, child, ownership) = spawn_with_guard(rows, cols, cmd, |identity| {
        ownership_change.attach(identity)
    })?;
    Ok(SpawnedPty {
        master_fd,
        child,
        ownership,
    })
}

// The factory is a narrow test seam for deterministic rollback release order;
// production supplies the real ownership lease, never a fake authority source.
fn spawn_with_guard<G>(
    rows: u16,
    cols: u16,
    cmd: CommandBuilder,
    attach: impl FnOnce(Option<crate::pty_ownership::TtyIdentity>) -> G,
) -> std::io::Result<(OwnedFd, Box<dyn Child + Send + Sync>, G)> {
    // Declare the guard BEFORE every PTY handle. On spawn_command failure,
    // reverse local destruction closes both masters/slave before removing the
    // ownership observation. Successful transfer keeps the same RAII ordering.
    let ownership;
    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|err| std::io::Error::other(err.to_string()))?;
    let master_fd = pair
        .master
        .as_raw_fd()
        .ok_or_else(|| std::io::Error::other("pty master fd is unavailable"))?;
    let actor_fd = fd::duplicate_cloexec_fd(master_fd)?;
    let actor_fd = unsafe { OwnedFd::from_raw_fd(actor_fd) };
    ownership = attach(crate::platform::pty_slave_identity_from_master(master_fd));
    let child = pair
        .slave
        .spawn_command(cmd)
        .map_err(|err| std::io::Error::other(err.to_string()))?;
    drop(pair);

    Ok((actor_fd, child, ownership))
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    fn pty_fd_test_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn parent_pty_fd_targets() -> Vec<String> {
        let Ok(entries) = std::fs::read_dir("/proc/self/fd") else {
            return Vec::new();
        };
        let mut targets: Vec<String> = entries
            .filter_map(Result::ok)
            .filter_map(|entry| std::fs::read_link(entry.path()).ok())
            .map(|target| target.to_string_lossy().into_owned())
            .filter(|target| target.starts_with("/dev/pts/") || target == "/dev/ptmx")
            .collect();
        targets.sort();
        targets
    }

    fn parent_pty_fd_count() -> usize {
        parent_pty_fd_targets().len()
    }

    #[test]
    fn spawn_failure_releases_all_host_pty_handles_before_ownership_guard() {
        use std::sync::{
            atomic::{AtomicBool, AtomicUsize, Ordering},
            Arc,
        };
        struct Witness {
            path: std::path::PathBuf,
            still_open: Arc<AtomicBool>,
            released: Arc<AtomicUsize>,
        }
        impl Drop for Witness {
            fn drop(&mut self) {
                self.still_open.store(self.path.exists(), Ordering::SeqCst);
                self.released.fetch_add(1, Ordering::SeqCst);
            }
        }
        let _guard = pty_fd_test_lock().lock().expect("private PTY fixture lock");
        let still_open = Arc::new(AtomicBool::new(false));
        let released = Arc::new(AtomicUsize::new(0));
        let mut command = CommandBuilder::new("/vqro-private-nonexistent-command/spawn-failure");
        command.env_clear();
        let result = spawn_with_guard(24, 80, command, |identity| {
            let identity = identity.expect("actual Linux slave identity");
            Witness {
                path: std::path::PathBuf::from(format!("/dev/pts/{}", libc::minor(identity.rdev))),
                still_open: Arc::clone(&still_open),
                released: Arc::clone(&released),
            }
        });
        assert!(
            result.is_err(),
            "must exercise the real spawn_command error unwind"
        );
        assert_eq!(released.load(Ordering::SeqCst), 1);
        assert!(
            !still_open.load(Ordering::SeqCst),
            "lease removal must follow closing every host master/slave handle"
        );
    }

    #[test]
    fn portable_pty_setup_leaves_one_parent_pty_fd() {
        let _guard = pty_fd_test_lock().lock().expect("pty fd test lock");
        let before = parent_pty_fd_count();
        let mut cmd = CommandBuilder::new("/bin/cat");
        cmd.env(crate::HERDR_ENV_VAR, crate::HERDR_ENV_VALUE);

        let mut spawned =
            spawn_with_portable_pty(24, 80, cmd).expect("portable pty setup succeeds");
        let after_spawn = parent_pty_fd_count();

        assert_eq!(
            after_spawn,
            before + 1,
            "portable-pty setup should leave only the Herdr-owned master fd in the parent: {:?}",
            parent_pty_fd_targets()
        );

        let _ = spawned.child.kill();
        let _ = spawned.child.wait();
        drop(spawned.master_fd);
    }
}
