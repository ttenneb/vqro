use super::*;
use std::os::fd::AsRawFd;

#[test]
fn actual_pty_slave_identity_is_not_the_shared_ptmx_master_device() {
    use portable_pty::{native_pty_system, PtySize};
    use std::os::unix::fs::MetadataExt;
    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("private PTY");
    let master = pair.master.as_raw_fd().expect("master descriptor");
    let identity = pty_slave_identity_from_master(master).expect("corroborated actual slave");
    let named = super::super::tty_identity_from_path(&pair.master.tty_name().expect("slave path"))
        .expect("named slave");
    assert_eq!(identity, named);
    let master_metadata =
        std::fs::metadata(format!("/proc/self/fd/{master}")).expect("master metadata");
    assert_ne!(
        identity.rdev,
        master_metadata.rdev(),
        "do not compare all PTYs using ptmx rdev"
    );
}

#[test]
fn an_owned_child_has_real_controlling_tty_and_a_detached_child_does_not() {
    use portable_pty::CommandBuilder;
    let mut command = CommandBuilder::new("/bin/sleep");
    command.arg("5");
    command.env_clear();
    let mut spawned =
        crate::pty::backend::spawn_with_portable_pty(24, 80, command).expect("private child");
    let pid = spawned.child.process_id().expect("child PID");
    let observed = client_controlling_tty(pid);
    let expected = pty_slave_identity_from_master(spawned.master_fd.as_raw_fd());
    let _ = spawned.child.kill();
    let _ = spawned.child.wait();
    assert_eq!(observed.as_ref().map(|(tty, _)| tty), expected.as_ref());
    assert!(observed.is_some());

    use std::os::unix::process::CommandExt;
    let mut command = std::process::Command::new("/bin/sleep");
    command.arg("5").env_clear();
    // SAFETY: setsid is async-signal-safe; no allocation/locks in pre_exec.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }
    let mut detached = command.spawn().expect("detached private process");
    let observed = client_controlling_tty(detached.id());
    let _ = detached.kill();
    let _ = detached.wait();
    assert!(observed.is_none());
}
