#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

#[test]
fn remote_attach_is_quarantined_before_ssh_discovery() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock after Unix epoch")
        .as_nanos();
    let temp_dir = std::env::temp_dir().join(format!(
        "vqro-remote-quarantine-test-{}-{nonce}",
        std::process::id()
    ));
    fs::create_dir_all(&temp_dir).expect("create test directory");

    let invoked_path = temp_dir.join("ssh-invoked");
    let ssh_path = temp_dir.join("ssh");
    fs::write(
        &ssh_path,
        format!("#!/bin/sh\n: > '{}'\n", invoked_path.display()),
    )
    .expect("write fake ssh");
    fs::set_permissions(&ssh_path, fs::Permissions::from_mode(0o755))
        .expect("make fake ssh executable");

    let inherited_path = std::env::var("PATH").unwrap_or_default();
    let output = Command::new(env!("CARGO_BIN_EXE_vqro"))
        .args(["--remote", "check-host"])
        .env("PATH", format!("{}:{inherited_path}", temp_dir.display()))
        .env("VQRO_CONFIG_PATH", temp_dir.join("config.toml"))
        .env_remove("VQRO_ENV")
        .env_remove("VQRO_SESSION")
        .env_remove("VQRO_SOCKET_PATH")
        .env_remove("VQRO_CLIENT_SOCKET_PATH")
        .env_remove("VQRO_REMOTE_BINARY")
        .output()
        .expect("run remote attach");

    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("vqro remote mode is disabled"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !invoked_path.exists(),
        "remote quarantine must run before ssh"
    );

    fs::remove_dir_all(temp_dir).expect("remove test directory");
}
