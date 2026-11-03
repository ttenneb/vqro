#![cfg(unix)]

pub mod support;

use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::fd::AsRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use support::{
    cleanup_test_base, client_shell_handshake, register_runtime_dir, register_spawned_herdr_pid,
    send_client_shell_shift_enter, unregister_spawned_herdr_pid, wait_for_client_shell_bootstrap,
    wait_for_message_variant, wait_for_socket, SERVER_MESSAGE_ENDPOINT_CONTROL,
    SERVER_MESSAGE_SERVER_SHUTDOWN,
};

struct SpawnedHerdr {
    _master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
}

struct RequestError {
    retryable: bool,
    message: String,
}

impl Drop for SpawnedHerdr {
    fn drop(&mut self) {
        let pid = self.child.process_id();
        support::stop_spawned_herdr(&mut *self.child);
        unregister_spawned_herdr_pid(pid);
    }
}

fn test_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn unique_test_dir() -> PathBuf {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    PathBuf::from(format!("/tmp/hlh-{}-{n}", std::process::id()))
}

fn spawn_server(config_home: &Path, runtime_dir: &Path, api_socket: &Path) -> SpawnedHerdr {
    spawn_server_with_env(config_home, runtime_dir, api_socket, &[])
}

fn spawn_server_with_env(
    config_home: &Path,
    runtime_dir: &Path,
    api_socket: &Path,
    extra_env: &[(&str, &str)],
) -> SpawnedHerdr {
    spawn_server_binary_with_env(
        Path::new(env!("CARGO_BIN_EXE_vqro")),
        config_home,
        runtime_dir,
        api_socket,
        extra_env,
    )
}

fn spawn_server_binary_with_env(
    binary: &Path,
    config_home: &Path,
    runtime_dir: &Path,
    api_socket: &Path,
    extra_env: &[(&str, &str)],
) -> SpawnedHerdr {
    register_runtime_dir(runtime_dir);
    fs::create_dir_all(config_home.join("vqro-dev")).unwrap();
    fs::create_dir_all(runtime_dir).unwrap();
    fs::write(
        config_home.join("vqro-dev/config.toml"),
        "onboarding = false\n[update]\nversion_check = false\nmanifest_check = false\n",
    )
    .unwrap();

    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();
    let mut cmd = CommandBuilder::new(binary);
    cmd.arg("server");
    cmd.env("XDG_CONFIG_HOME", config_home);
    cmd.env("XDG_RUNTIME_DIR", runtime_dir);
    cmd.env("VQRO_SOCKET_PATH", api_socket);
    cmd.env(
        "VQRO_CLIENT_SOCKET_PATH",
        runtime_dir.join("herdr-client.sock"),
    );
    cmd.env("SHELL", "/bin/sh");
    for (key, value) in extra_env {
        cmd.env(key, value);
    }

    let child = pair.slave.spawn_command(cmd).unwrap();
    register_spawned_herdr_pid(child.process_id());
    SpawnedHerdr {
        _master: pair.master,
        child,
    }
}

fn spawn_named_session_server(
    config_home: &Path,
    runtime_dir: &Path,
    session_name: &str,
) -> SpawnedHerdr {
    register_runtime_dir(runtime_dir);
    fs::create_dir_all(config_home.join("vqro-dev")).unwrap();
    fs::create_dir_all(runtime_dir).unwrap();
    fs::write(
        config_home.join("vqro-dev/config.toml"),
        "onboarding = false\n",
    )
    .unwrap();

    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();
    let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_vqro"));
    cmd.arg("server");
    cmd.env("XDG_CONFIG_HOME", config_home);
    cmd.env("XDG_RUNTIME_DIR", runtime_dir);
    cmd.env("VQRO_SESSION", session_name);
    cmd.env_remove("VQRO_SOCKET_PATH");
    cmd.env_remove("VQRO_CLIENT_SOCKET_PATH");
    cmd.env("SHELL", "/bin/sh");

    let child = pair.slave.spawn_command(cmd).unwrap();
    register_spawned_herdr_pid(child.process_id());
    SpawnedHerdr {
        _master: pair.master,
        child,
    }
}

fn spawn_default_session_server(config_home: &Path, runtime_dir: &Path) -> SpawnedHerdr {
    register_runtime_dir(runtime_dir);
    fs::create_dir_all(config_home.join("vqro-dev")).unwrap();
    fs::create_dir_all(runtime_dir).unwrap();
    fs::write(
        config_home.join("vqro-dev/config.toml"),
        "onboarding = false\n",
    )
    .unwrap();

    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();
    let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_vqro"));
    cmd.arg("server");
    cmd.env("XDG_CONFIG_HOME", config_home);
    cmd.env("XDG_RUNTIME_DIR", runtime_dir);
    cmd.env("XDG_STATE_HOME", runtime_dir.join("state"));
    cmd.env_remove("VQRO_SESSION");
    cmd.env_remove("VQRO_SOCKET_PATH");
    cmd.env_remove("VQRO_CLIENT_SOCKET_PATH");
    cmd.env("SHELL", "/bin/sh");

    let child = pair.slave.spawn_command(cmd).unwrap();
    register_spawned_herdr_pid(child.process_id());
    SpawnedHerdr {
        _master: pair.master,
        child,
    }
}

fn spawn_server_with_args_and_socket_env(
    config_home: &Path,
    runtime_dir: &Path,
    session_name: Option<&str>,
    api_socket_env: Option<&Path>,
    client_socket_env: Option<&Path>,
) -> SpawnedHerdr {
    register_runtime_dir(runtime_dir);
    fs::create_dir_all(config_home.join("vqro-dev")).unwrap();
    fs::create_dir_all(runtime_dir).unwrap();
    fs::write(
        config_home.join("vqro-dev/config.toml"),
        "onboarding = false\n",
    )
    .unwrap();

    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();
    let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_vqro"));
    if let Some(session_name) = session_name {
        cmd.arg("--session");
        cmd.arg(session_name);
    }
    cmd.arg("server");
    cmd.env("XDG_CONFIG_HOME", config_home);
    cmd.env("XDG_RUNTIME_DIR", runtime_dir);
    cmd.env_remove("VQRO_SESSION");
    if let Some(api_socket_env) = api_socket_env {
        cmd.env("VQRO_SOCKET_PATH", api_socket_env);
    } else {
        cmd.env_remove("VQRO_SOCKET_PATH");
    }
    if let Some(client_socket_env) = client_socket_env {
        cmd.env("VQRO_CLIENT_SOCKET_PATH", client_socket_env);
    } else {
        cmd.env_remove("VQRO_CLIENT_SOCKET_PATH");
    }
    cmd.env("SHELL", "/bin/sh");

    let child = pair.slave.spawn_command(cmd).unwrap();
    register_spawned_herdr_pid(child.process_id());
    SpawnedHerdr {
        _master: pair.master,
        child,
    }
}

fn try_request(
    socket_path: &Path,
    request: serde_json::Value,
) -> Result<serde_json::Value, RequestError> {
    let mut stream = UnixStream::connect(socket_path).map_err(|err| RequestError {
        retryable: true,
        message: format!("connect {}: {err}", socket_path.display()),
    })?;
    let request_text = request.to_string();
    stream
        .write_all(request_text.as_bytes())
        .map_err(|err| RequestError {
            retryable: true,
            message: format!("write request to {}: {err}", socket_path.display()),
        })?;
    stream.write_all(b"\n").map_err(|err| RequestError {
        retryable: true,
        message: format!("write newline to {}: {err}", socket_path.display()),
    })?;
    stream.flush().map_err(|err| RequestError {
        retryable: true,
        message: format!("flush request to {}: {err}", socket_path.display()),
    })?;
    let mut line = String::new();
    BufReader::new(stream)
        .read_line(&mut line)
        .map_err(|err| RequestError {
            retryable: true,
            message: format!("read response from {}: {err}", socket_path.display()),
        })?;
    if line.is_empty() {
        return Err(RequestError {
            retryable: true,
            message: format!(
                "empty response from {} for request {request_text}",
                socket_path.display()
            ),
        });
    }
    serde_json::from_str(&line).map_err(|err| RequestError {
        retryable: false,
        message: format!(
            "parse response from {} for request {request_text}: {err}; response was {line:?}",
            socket_path.display()
        ),
    })
}

fn request(socket_path: &Path, request: serde_json::Value) -> serde_json::Value {
    try_request(socket_path, request).unwrap_or_else(|err| panic!("{}", err.message))
}

fn assert_ok(response: serde_json::Value) {
    assert!(
        response.get("result").is_some(),
        "api request failed: {response}"
    );
}

fn terminal_id_for_pane(socket_path: &Path, pane_id: &str) -> String {
    let panes = request(
        socket_path,
        serde_json::json!({"id":"test:terminal-identity","method":"pane.list","params":{}}),
    );
    panes["result"]["panes"]
        .as_array()
        .and_then(|panes| {
            panes
                .iter()
                .find(|pane| pane["pane_id"].as_str() == Some(pane_id))
        })
        .and_then(|pane| pane["terminal_id"].as_str())
        .expect("pane terminal identity")
        .to_owned()
}

fn wait_for_api(socket_path: &Path, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    let mut last_error = String::new();
    while Instant::now() < deadline {
        match try_request(
            socket_path,
            serde_json::json!({"id":"test:ping","method":"ping","params":{}}),
        ) {
            Ok(response) if response.get("result").is_some() => return,
            Ok(response) => panic!("api ping returned non-success response: {response}"),
            Err(err) if !err.retryable => panic!("{}", err.message),
            Err(err) => {
                last_error = err.message;
            }
        }
        thread::sleep(Duration::from_millis(25));
    }
    panic!(
        "api did not become ready at {}; last error: {last_error}",
        socket_path.display()
    );
}

fn write_plugin_manifest(root: &Path, plugin_id: &str) {
    fs::create_dir_all(root).unwrap();
    fs::write(
        root.join("herdr-plugin.toml"),
        format!(
            r#"id = "{plugin_id}"
name = "Live handoff test"
version = "0.1.0"
min_herdr_version = "0.6.10"
platforms = ["linux", "macos", "windows"]
"#
        ),
    )
    .unwrap();
}

fn link_plugin(socket_path: &Path, root: &Path) {
    assert_ok(request(
        socket_path,
        serde_json::json!({
            "id": "test:plugin:link",
            "method": "plugin.link",
            "params": {"path": root, "enabled": true}
        }),
    ));
}

fn listed_plugin_ids(socket_path: &Path) -> Vec<String> {
    let response = request(
        socket_path,
        serde_json::json!({"id":"test:plugin:list","method":"plugin.list","params":{}}),
    );
    assert_ok(response.clone());
    response["result"]["plugins"]
        .as_array()
        .unwrap()
        .iter()
        .map(|plugin| plugin["plugin_id"].as_str().unwrap().to_string())
        .collect()
}

fn saved_plugin_ids(registry_path: &Path) -> Vec<String> {
    let mut ids =
        serde_json::from_str::<Vec<serde_json::Value>>(&fs::read_to_string(registry_path).unwrap())
            .unwrap()
            .into_iter()
            .map(|plugin| plugin["plugin_id"].as_str().unwrap().to_string())
            .collect::<Vec<_>>();
    ids.sort();
    ids
}

fn wait_for_output(socket_path: &Path, pane_id: &str, needle: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut last_text = String::new();
    let mut last_response = serde_json::Value::Null;
    while Instant::now() < deadline {
        let response = request(
            socket_path,
            serde_json::json!({
                "id": "test:pane:read",
                "method": "pane.read",
                "params": {
                    "pane_id": pane_id,
                    "source": "visible",
                    "lines": 20,
                    "format": "text",
                    "strip_ansi": true
                }
            }),
        );
        last_response = response.clone();
        let text = response["result"]["read"]["text"]
            .as_str()
            .unwrap_or_default();
        last_text = text.to_string();
        if text.contains(needle) {
            return;
        }
        thread::sleep(Duration::from_millis(50));
    }
    panic!(
        "pane output did not contain {needle:?}; last text was {last_text:?}; last response was {last_response}"
    );
}

fn wait_for_file_contains(path: &Path, needle: &str, timeout: Duration) -> String {
    let deadline = Instant::now() + timeout;
    let mut last_text = String::new();
    while Instant::now() < deadline {
        if let Ok(text) = fs::read_to_string(path) {
            last_text = text;
            if last_text.contains(needle) {
                return last_text;
            }
        }
        thread::sleep(Duration::from_millis(50));
    }
    panic!(
        "{} did not contain {needle:?}; last text was {last_text:?}",
        path.display()
    );
}

#[cfg(target_os = "linux")]
fn server_ptmx_fd_count(pid: u32) -> usize {
    let Ok(entries) = fs::read_dir(format!("/proc/{pid}/fd")) else {
        return 0;
    };
    entries
        .filter_map(Result::ok)
        .filter_map(|entry| fs::read_link(entry.path()).ok())
        // ptmx master node: /dev/ptmx or /dev/pts/ptmx (devpts); slaves /dev/pts/<N> excluded.
        .filter(|target| target == Path::new("/dev/ptmx") || target == Path::new("/dev/pts/ptmx"))
        .count()
}

#[cfg(target_os = "macos")]
fn server_ptmx_fd_count(pid: u32) -> usize {
    let Ok(output) = std::process::Command::new("lsof")
        .args(["-nP", "-p", &pid.to_string()])
        .output()
    else {
        return 0;
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| line.contains("/dev/ptmx"))
        .count()
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn wait_for_server_ptmx_fd_count(pid: u32, expected: usize, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    let mut last_count = 0;
    while Instant::now() < deadline {
        last_count = server_ptmx_fd_count(pid);
        if last_count == expected {
            return;
        }
        thread::sleep(Duration::from_millis(25));
    }
    panic!("server pid {pid} had {last_count} ptmx master fds; expected {expected}");
}

#[cfg(target_os = "linux")]
fn wait_for_replacement_server_pid(runtime_dir: &Path, old_pid: u32, timeout: Duration) -> u32 {
    let deadline = Instant::now() + timeout;
    let mut last_pids = Vec::new();
    while Instant::now() < deadline {
        last_pids = support::herdr_server_pids_for_runtime_dir(runtime_dir).unwrap_or_default();
        if let Some(pid) = last_pids.iter().copied().find(|pid| *pid != old_pid) {
            return pid;
        }
        thread::sleep(Duration::from_millis(25));
    }
    panic!(
        "replacement server for {} did not appear; last pids: {:?}",
        runtime_dir.display(),
        last_pids
    );
}

#[cfg(target_os = "macos")]
fn wait_for_replacement_server_pid(_runtime_dir: &Path, old_pid: u32, timeout: Duration) -> u32 {
    let handoff_socket_pattern = format!("herdr-handoff-{old_pid}.sock");
    let deadline = Instant::now() + timeout;
    let mut last_stdout = String::new();
    while Instant::now() < deadline {
        if let Ok(output) = std::process::Command::new("pgrep")
            .args(["-af", &handoff_socket_pattern])
            .output()
        {
            last_stdout = String::from_utf8_lossy(&output.stdout).into_owned();
            for line in last_stdout.lines() {
                let Some(pid_text) = line.split_whitespace().next() else {
                    continue;
                };
                let Ok(pid) = pid_text.parse::<u32>() else {
                    continue;
                };
                if pid != old_pid {
                    return pid;
                }
            }
        }
        thread::sleep(Duration::from_millis(25));
    }
    panic!(
        "replacement server for {} did not appear; last pgrep output: {}",
        _runtime_dir.display(),
        last_stdout
    );
}

fn unused_local_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn wait_for_http_contains(port: u16, needle: &str, timeout: Duration) -> String {
    let deadline = Instant::now() + timeout;
    let mut last_response = String::new();
    while Instant::now() < deadline {
        if let Ok(mut stream) = TcpStream::connect(("127.0.0.1", port)) {
            let _ =
                stream.write_all(b"GET / HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n");
            let mut response = String::new();
            let _ = stream.read_to_string(&mut response);
            last_response = response;
            if last_response.contains(needle) {
                return last_response;
            }
        }
        thread::sleep(Duration::from_millis(50));
    }
    panic!(
        "http server on port {port} did not return {needle:?}; last response was {last_response:?}"
    );
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn live_server_holds_one_pty_master_fd_per_pane() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");

    let spawned = spawn_server(&config_home, &runtime_dir, &api_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));
    register_runtime_dir(&runtime_dir);
    let server_pid = spawned
        .child
        .process_id()
        .expect("test server should expose pid");
    wait_for_server_ptmx_fd_count(server_pid, 0, Duration::from_secs(5));

    let created = request(
        &api_socket,
        serde_json::json!({
            "id": "test:workspace:create",
            "method": "workspace.create",
            "params": {"cwd": "/tmp", "focus": true}
        }),
    );
    let pane_id = created["result"]["root_pane"]["pane_id"]
        .as_str()
        .unwrap()
        .to_string();
    wait_for_server_ptmx_fd_count(server_pid, 1, Duration::from_secs(5));

    let second = request(
        &api_socket,
        serde_json::json!({
            "id": "test:pane:split-second",
            "method": "pane.split",
            "params": {
                "target_pane_id": pane_id,
                "direction": "right",
                "focus": true
            }
        }),
    );
    assert_ok(second.clone());
    let second_pane_id = second["result"]["pane"]["pane_id"]
        .as_str()
        .unwrap()
        .to_string();
    wait_for_server_ptmx_fd_count(server_pid, 2, Duration::from_secs(5));

    assert_ok(request(
        &api_socket,
        serde_json::json!({
            "id": "test:pane:split-third",
            "method": "pane.split",
            "params": {
                "target_pane_id": second_pane_id,
                "direction": "down",
                "focus": true
            }
        }),
    ));
    wait_for_server_ptmx_fd_count(server_pid, 3, Duration::from_secs(5));

    assert_ok(request(
        &api_socket,
        serde_json::json!({"id":"test:handoff","method":"server.live_handoff","params":{}}),
    ));
    let replacement_pid =
        wait_for_replacement_server_pid(&runtime_dir, server_pid, Duration::from_secs(10));
    wait_for_api(&api_socket, Duration::from_secs(10));
    wait_for_server_ptmx_fd_count(replacement_pid, 3, Duration::from_secs(5));

    let _ = request(
        &api_socket,
        serde_json::json!({"id":"test:stop","method":"server.stop","params":{}}),
    );
    drop(spawned);
    cleanup_test_base(&base);
}

#[cfg(target_os = "linux")]
#[test]
fn live_handoff_unknown_pane_exit_preserves_session_on_shutdown() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");

    let spawned = spawn_server(&config_home, &runtime_dir, &api_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));
    register_runtime_dir(&runtime_dir);

    let created = request(
        &api_socket,
        serde_json::json!({
            "id": "test:workspace:create",
            "method": "workspace.create",
            "params": {"cwd": "/tmp", "focus": true}
        }),
    );
    let pane_id = created["result"]["root_pane"]["pane_id"]
        .as_str()
        .expect("root pane id")
        .to_string();
    let old_pid = spawned.child.process_id().expect("old server pid");

    assert_ok(request(
        &api_socket,
        serde_json::json!({"id":"test:handoff","method":"server.live_handoff","params":{}}),
    ));
    let replacement_pid =
        wait_for_replacement_server_pid(&runtime_dir, old_pid, Duration::from_secs(10));
    drop(spawned);
    wait_for_api(&api_socket, Duration::from_secs(10));

    let process_info = request(
        &api_socket,
        serde_json::json!({
            "id": "test:process-info",
            "method": "pane.process_info",
            "params": {"pane_id": pane_id}
        }),
    );
    let shell_pid = process_info["result"]["process_info"]["shell_pid"]
        .as_u64()
        .expect("shell pid") as libc::pid_t;
    assert_eq!(unsafe { libc::kill(shell_pid, libc::SIGHUP) }, 0);

    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let panes = request(
            &api_socket,
            serde_json::json!({"id":"test:panes","method":"pane.list","params":{}}),
        );
        if panes["result"]["panes"]
            .as_array()
            .is_some_and(Vec::is_empty)
        {
            break;
        }
        assert!(Instant::now() < deadline, "handoff pane was not removed");
        thread::sleep(Duration::from_millis(20));
    }

    assert_ok(request(
        &api_socket,
        serde_json::json!({"id":"test:stop","method":"server.stop","params":{}}),
    ));
    let deadline = Instant::now() + Duration::from_secs(5);
    while Path::new(&format!("/proc/{replacement_pid}")).exists() {
        assert!(Instant::now() < deadline, "replacement server did not stop");
        thread::sleep(Duration::from_millis(20));
    }

    let session: serde_json::Value = serde_json::from_slice(
        &fs::read(config_home.join("vqro-dev/session.json")).expect("saved session"),
    )
    .expect("valid session json");
    assert_eq!(session["workspaces"].as_array().map(Vec::len), Some(1));
    assert_eq!(
        session["workspaces"][0]["tabs"][0]["panes"]
            .as_object()
            .map(serde_json::Map::len),
        Some(1)
    );

    cleanup_test_base(&base);
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn live_handoff_carries_more_panes_than_one_scm_rights_message() {
    const PANES: usize = 70;

    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");

    let spawned = spawn_server(&config_home, &runtime_dir, &api_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));
    register_runtime_dir(&runtime_dir);
    let server_pid = spawned
        .child
        .process_id()
        .expect("test server should expose pid");

    let created = request(
        &api_socket,
        serde_json::json!({
            "id": "test:workspace:create",
            "method": "workspace.create",
            "params": {"cwd": "/tmp", "focus": true}
        }),
    );
    let workspace_id = created["result"]["workspace"]["workspace_id"]
        .as_str()
        .unwrap()
        .to_string();

    // One pane per tab keeps the layout shallow, so this exercises the fd
    // transfer rather than the depth of a single split tree.
    for index in 1..PANES {
        assert_ok(request(
            &api_socket,
            serde_json::json!({
                "id": format!("test:tab:create-{index}"),
                "method": "tab.create",
                "params": {"workspace_id": workspace_id, "focus": false}
            }),
        ));
    }
    wait_for_server_ptmx_fd_count(server_pid, PANES, Duration::from_secs(60));

    assert_ok(request(
        &api_socket,
        serde_json::json!({"id":"test:handoff","method":"server.live_handoff","params":{}}),
    ));
    let replacement_pid =
        wait_for_replacement_server_pid(&runtime_dir, server_pid, Duration::from_secs(30));
    wait_for_api(&api_socket, Duration::from_secs(30));
    wait_for_server_ptmx_fd_count(replacement_pid, PANES, Duration::from_secs(30));

    let panes = request(
        &api_socket,
        serde_json::json!({"id":"test:pane:list","method":"pane.list","params":{}}),
    );
    assert_eq!(
        panes["result"]["panes"].as_array().map(Vec::len),
        Some(PANES),
        "replacement server should report every pane after handoff"
    );

    let _ = request(
        &api_socket,
        serde_json::json!({"id":"test:stop","method":"server.stop","params":{}}),
    );
    drop(spawned);
    cleanup_test_base(&base);
}

#[test]
fn live_handoff_preserves_named_session_socket_paths() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let session_dir = config_home.join("vqro-dev/sessions/work");
    let api_socket = session_dir.join("vqro.sock");
    let client_socket = session_dir.join("vqro-client.sock");

    let spawned = spawn_named_session_server(&config_home, &runtime_dir, "work");
    wait_for_socket(&api_socket, Duration::from_secs(10));
    register_runtime_dir(&runtime_dir);

    assert_ok(request(
        &api_socket,
        serde_json::json!({"id":"test:handoff","method":"server.live_handoff","params":{}}),
    ));
    drop(spawned);
    wait_for_api(&api_socket, Duration::from_secs(10));
    wait_for_socket(&client_socket, Duration::from_secs(5));
    assert!(
        !config_home.join("vqro-dev/vqro.sock").exists(),
        "named handoff unexpectedly bound the default session API socket"
    );

    let _ = request(
        &api_socket,
        serde_json::json!({"id":"test:stop","method":"server.stop","params":{}}),
    );
    cleanup_test_base(&base);
}

#[test]
fn live_handoff_ignores_leaked_default_socket_env_for_named_session() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let default_session_dir = config_home.join("vqro-dev");
    let default_api_socket = default_session_dir.join("vqro.sock");
    let default_client_socket = default_session_dir.join("vqro-client.sock");
    let work_session_dir = config_home.join("vqro-dev/sessions/work");
    let work_api_socket = work_session_dir.join("vqro.sock");
    let work_client_socket = work_session_dir.join("vqro-client.sock");

    let default_spawned = spawn_default_session_server(&config_home, &runtime_dir);
    wait_for_socket(&default_api_socket, Duration::from_secs(10));
    register_runtime_dir(&runtime_dir);

    let work_spawned = spawn_server_with_args_and_socket_env(
        &config_home,
        &runtime_dir,
        Some("work"),
        Some(&default_api_socket),
        Some(&default_client_socket),
    );
    wait_for_socket(&work_api_socket, Duration::from_secs(10));

    assert_ok(request(
        &work_api_socket,
        serde_json::json!({"id":"test:handoff","method":"server.live_handoff","params":{}}),
    ));
    drop(work_spawned);
    wait_for_api(&default_api_socket, Duration::from_secs(10));
    wait_for_api(&work_api_socket, Duration::from_secs(10));
    wait_for_socket(&work_client_socket, Duration::from_secs(5));

    let _ = request(
        &work_api_socket,
        serde_json::json!({"id":"test:stop-work","method":"server.stop","params":{}}),
    );
    let _ = request(
        &default_api_socket,
        serde_json::json!({"id":"test:stop-default","method":"server.stop","params":{}}),
    );
    drop(default_spawned);
    cleanup_test_base(&base);
}

#[test]
fn live_handoff_preserves_client_socket_env_without_api_socket_env() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = config_home.join("vqro-dev/vqro.sock");
    let client_socket = runtime_dir.join("custom-client.sock");

    let spawned = spawn_server_with_args_and_socket_env(
        &config_home,
        &runtime_dir,
        None,
        None,
        Some(&client_socket),
    );
    wait_for_socket(&api_socket, Duration::from_secs(10));
    wait_for_socket(&client_socket, Duration::from_secs(10));
    register_runtime_dir(&runtime_dir);

    assert_ok(request(
        &api_socket,
        serde_json::json!({"id":"test:handoff","method":"server.live_handoff","params":{}}),
    ));
    drop(spawned);
    wait_for_api(&api_socket, Duration::from_secs(10));
    wait_for_socket(&client_socket, Duration::from_secs(5));

    let _ = request(
        &api_socket,
        serde_json::json!({"id":"test:stop","method":"server.stop","params":{}}),
    );
    cleanup_test_base(&base);
}

#[test]
fn live_handoff_preserves_installed_plugins() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = config_home.join("vqro-dev/vqro.sock");
    let registry_path = config_home.join("vqro-dev/plugins.json");
    let existing_plugin = base.join("plugins/existing");
    let added_plugin = base.join("plugins/added");
    write_plugin_manifest(&existing_plugin, "test.live-handoff-existing");
    write_plugin_manifest(&added_plugin, "test.live-handoff-added");

    let spawned = spawn_default_session_server(&config_home, &runtime_dir);
    wait_for_socket(&api_socket, Duration::from_secs(10));
    register_runtime_dir(&runtime_dir);

    link_plugin(&api_socket, &existing_plugin);
    assert_eq!(
        listed_plugin_ids(&api_socket),
        ["test.live-handoff-existing"]
    );

    assert_ok(request(
        &api_socket,
        serde_json::json!({"id":"test:handoff","method":"server.live_handoff","params":{}}),
    ));
    drop(spawned);
    wait_for_api(&api_socket, Duration::from_secs(10));

    assert_eq!(
        listed_plugin_ids(&api_socket),
        ["test.live-handoff-existing"]
    );
    link_plugin(&api_socket, &added_plugin);
    assert_eq!(
        saved_plugin_ids(&registry_path),
        ["test.live-handoff-added", "test.live-handoff-existing"]
    );

    let _ = request(
        &api_socket,
        serde_json::json!({"id":"test:stop","method":"server.stop","params":{}}),
    );
    cleanup_test_base(&base);
}

#[test]
fn live_handoff_preserves_pane_process_io() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let client_socket = runtime_dir.join("herdr-client.sock");
    let marker = base.join("child.pid");
    let second_marker = base.join("second-child.pid");
    let hup_marker = base.join("hup");
    let second_hup_marker = base.join("second-hup");
    let received_marker = base.join("received");
    let second_received_marker = base.join("second-received");

    let spawned = spawn_server(&config_home, &runtime_dir, &api_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));
    register_runtime_dir(&runtime_dir);

    let created = request(
        &api_socket,
        serde_json::json!({
            "id": "test:workspace:create",
            "method": "workspace.create",
            "params": {"cwd": "/tmp", "focus": true}
        }),
    );
    let pane_id = created["result"]["root_pane"]["pane_id"]
        .as_str()
        .unwrap()
        .to_string();
    let split = request(
        &api_socket,
        serde_json::json!({
            "id": "test:pane:split",
            "method": "pane.split",
            "params": {
                "target_pane_id": pane_id,
                "direction": "right",
                "focus": false
            }
        }),
    );
    assert_ok(split.clone());
    let second_pane_id = split["result"]["pane"]["pane_id"]
        .as_str()
        .unwrap()
        .to_string();
    let terminal_id_before = terminal_id_for_pane(&api_socket, &pane_id);
    let second_terminal_id_before = terminal_id_for_pane(&api_socket, &second_pane_id);
    assert_ne!(terminal_id_before, second_terminal_id_before);

    let command = format!(
        "sh -c 'echo READY $$ > {}; trap \"echo HUP >> {}\" HUP; while read line; do echo got:$line; echo got:$line >> {}; done'",
        marker.display(),
        hup_marker.display(),
        received_marker.display()
    );
    let second_command = format!(
        "sh -c 'echo SECOND_READY $$ > {}; trap \"echo HUP >> {}\" HUP; while read line; do echo second:$line; echo second:$line >> {}; done'",
        second_marker.display(),
        second_hup_marker.display(),
        second_received_marker.display()
    );
    assert_ok(request(
        &api_socket,
        serde_json::json!({
            "id": "test:pane:run",
            "method": "pane.send_input",
            "params": {"pane_id": pane_id, "text": command, "keys": ["Enter"]}
        }),
    ));
    assert_ok(request(
        &api_socket,
        serde_json::json!({
            "id": "test:second-pane:run",
            "method": "pane.send_input",
            "params": {"pane_id": second_pane_id, "text": second_command, "keys": ["Enter"]}
        }),
    ));
    support::wait_for_file(&marker, Duration::from_secs(5));
    support::wait_for_file(&second_marker, Duration::from_secs(5));
    let pid_text = fs::read_to_string(&marker).unwrap();
    let child_pid: u32 = pid_text.split_whitespace().last().unwrap().parse().unwrap();
    let second_pid_text = fs::read_to_string(&second_marker).unwrap();
    let second_child_pid: u32 = second_pid_text
        .split_whitespace()
        .last()
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(unsafe { libc::kill(child_pid as libc::pid_t, 0) }, 0);
    assert_eq!(unsafe { libc::kill(second_child_pid as libc::pid_t, 0) }, 0);

    let endpoint_generation = support::CURRENT_ENDPOINT_PROTOCOL_GENERATION;
    let mut client_stream = UnixStream::connect(&client_socket).unwrap();
    let (server_generation, error) =
        client_shell_handshake(&mut client_stream, endpoint_generation, 54, 23).unwrap();
    assert_eq!(server_generation, endpoint_generation);
    assert!(error.is_none(), "client shell handshake failed: {error:?}");
    assert!(
        wait_for_message_variant(
            &mut client_stream,
            Duration::from_secs(5),
            SERVER_MESSAGE_ENDPOINT_CONTROL,
        )
        .unwrap(),
        "client shell should receive a complete snapshot before handoff"
    );

    assert_ok(request(
        &api_socket,
        serde_json::json!({
            "id": "test:pane:before-log",
            "method": "pane.send_input",
            "params": {"pane_id": pane_id, "text": "before_replay", "keys": ["Enter"]}
        }),
    ));
    wait_for_output(&api_socket, &pane_id, "got:before_replay");

    assert_ok(request(
        &api_socket,
        serde_json::json!({"id":"test:handoff","method":"server.live_handoff","params":{}}),
    ));
    assert!(
        wait_for_message_variant(
            &mut client_stream,
            Duration::from_secs(5),
            SERVER_MESSAGE_SERVER_SHUTDOWN,
        )
        .unwrap(),
        "connected client shell should receive live-handoff shutdown"
    );
    drop(spawned);
    thread::sleep(Duration::from_millis(300));
    wait_for_api(&api_socket, Duration::from_secs(10));
    wait_for_socket(&client_socket, Duration::from_secs(5));
    assert_eq!(
        terminal_id_for_pane(&api_socket, &pane_id),
        terminal_id_before
    );
    assert_eq!(
        terminal_id_for_pane(&api_socket, &second_pane_id),
        second_terminal_id_before
    );
    assert_eq!(unsafe { libc::kill(child_pid as libc::pid_t, 0) }, 0);
    assert_eq!(unsafe { libc::kill(second_child_pid as libc::pid_t, 0) }, 0);
    assert!(
        !hup_marker.exists(),
        "pane process received HUP during handoff"
    );
    assert!(
        !second_hup_marker.exists(),
        "second pane process received HUP during handoff"
    );
    wait_for_output(&api_socket, &pane_id, "got:before_replay");

    assert_ok(request(
        &api_socket,
        serde_json::json!({
            "id": "test:pane:send",
            "method": "pane.send_input",
            "params": {"pane_id": pane_id, "text": "after-handoff", "keys": ["Enter"]}
        }),
    ));
    wait_for_file_contains(
        &received_marker,
        "got:after-handoff",
        Duration::from_secs(5),
    );
    wait_for_output(&api_socket, &pane_id, "got:after-handoff");
    assert_ok(request(
        &api_socket,
        serde_json::json!({
            "id": "test:second-pane:send",
            "method": "pane.send_input",
            "params": {"pane_id": second_pane_id, "text": "after-handoff-second", "keys": ["Enter"]}
        }),
    ));
    wait_for_file_contains(
        &second_received_marker,
        "second:after-handoff-second",
        Duration::from_secs(5),
    );
    wait_for_output(&api_socket, &second_pane_id, "second:after-handoff-sec");

    let mut reattached_shell = UnixStream::connect(&client_socket).unwrap();
    let (server_generation, error) = client_shell_handshake(
        &mut reattached_shell,
        support::CURRENT_ENDPOINT_PROTOCOL_GENERATION,
        54,
        23,
    )
    .unwrap();
    assert_eq!(
        server_generation,
        support::CURRENT_ENDPOINT_PROTOCOL_GENERATION
    );
    assert!(error.is_none(), "reattached client shell failed: {error:?}");
    wait_for_client_shell_bootstrap(&mut reattached_shell, Duration::from_secs(5))
        .expect("fresh client shell should receive restored snapshot before pane content");

    let _ = request(
        &api_socket,
        serde_json::json!({"id":"test:stop","method":"server.stop","params":{}}),
    );
    let _ = client_socket;
    cleanup_test_base(&base);
}

// Legacy persisted Collections recover generically without a package grant.
// collection.list remains unavailable; neutral layout facts prove the member is hidden.
fn assert_hidden_handoff_collection(socket: &Path) {
    let unavailable = request(
        socket,
        serde_json::json!({"id":"test:authority","method":"collection.list","params":{}}),
    );
    assert_eq!(
        unavailable["error"]["code"],
        "collections_authority_unavailable"
    );
    let response = request(
        socket,
        serde_json::json!({"id":"test:layout","method":"session.snapshot","params":{}}),
    );
    assert_ok(response.clone());
    let snapshot = &response["result"]["snapshot"];
    assert_eq!(snapshot["panes"].as_array().unwrap().len(), 3);
    let layouts = snapshot["layouts"].as_array().unwrap();
    assert_eq!(layouts.len(), 1);
    let visible: Vec<_> = layouts[0]["panes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|pane| pane["pane_id"].as_str().unwrap())
        .collect();
    assert_eq!(visible, ["w2:p1", "w2:p3"]);
    assert_eq!(layouts[0]["splits"].as_array().unwrap().len(), 1);
}

fn assert_saved_handoff_collection(session_path: &Path) {
    let saved: serde_json::Value =
        serde_json::from_slice(&fs::read(session_path).unwrap()).unwrap();
    let workspace = &saved["workspaces"][0];
    let tab = &workspace["tabs"][0];
    let stack = &tab["layout"]["Split"]["first"]["Stack"];
    let is_tree = saved["version"].as_u64().unwrap() == current_snapshot_version() as u64;
    if is_tree {
        assert_eq!(stack["id"], "collection_6100001");
    } else {
        assert_eq!(
            tab["layout"]["Split"]["first"]["Collection"],
            "collection_6100001"
        );
    }
    let collections = tab["collections"].as_array().unwrap();
    assert_eq!(collections.len(), 1);
    let collection = &collections[0];
    assert_eq!(collection["id"], "collection_6100001");
    let members: Vec<_> = if is_tree {
        assert!(collection.get("members").is_none());
        assert!(collection.get("selected").is_none());
        assert_eq!(stack["selected"], 0);
        stack["children"]
            .as_array()
            .unwrap()
            .iter()
            .map(|child| child["Pane"].clone())
            .collect()
    } else {
        let members = collection["members"].as_array().unwrap();
        assert_eq!(collection["selected"], members[0]);
        members.clone()
    };
    assert_eq!(members.len(), 2);
    assert_eq!(collection["archived"].as_array().unwrap().len(), 0);
    for (index, raw) in members.iter().enumerate() {
        let raw = raw.as_u64().unwrap().to_string();
        assert_eq!(workspace["public_pane_numbers"][&raw], index + 1);
        assert_eq!(
            tab["panes"][&raw]["terminal_id"],
            format!("term_{}", (index + 1).to_string().repeat(32))
        );
    }
    let outside = tab["layout"]["Split"]["second"]["Pane"]
        .as_u64()
        .unwrap()
        .to_string();
    assert_eq!(workspace["public_pane_numbers"][&outside], 3);
    assert_eq!(
        tab["panes"][&outside]["terminal_id"],
        "term_33333333333333333333333333333333"
    );
}

fn assert_handoff_pane_identity(socket: &Path, target: &str, number: usize) {
    let response = request(
        socket,
        serde_json::json!({"id":"test:identity","method":"pane.get","params":{"pane_id":target}}),
    );
    assert_ok(response.clone());
    let pane = &response["result"]["pane"];
    assert_eq!(pane["pane_id"], format!("w2:p{number}"), "{target}");
    assert_eq!(
        pane["terminal_id"],
        format!("term_{}", number.to_string().repeat(32)),
        "{target} must resolve to its own terminal"
    );
    assert_eq!(pane["workspace_id"], "w2");
    assert_eq!(pane["tab_id"], "w2:t1");
}

fn current_snapshot_version() -> u32 {
    include_str!("../src/persist/snapshot.rs")
        .lines()
        .find_map(|line| line.strip_prefix("pub(crate) const SNAPSHOT_VERSION: u32 = "))
        .expect("snapshot source of truth")
        .trim_end_matches(';')
        .parse()
        .unwrap()
}

#[test]
fn live_handoff_preserves_hidden_collection_member_identity_and_io() {
    hidden_member_handoff(None, None);
}

#[test]
#[ignore = "requires the approved previous binary in a private root"]
fn session_v8_cross_version_hidden_member_conversion_and_downgrade_refusal() {
    let old =
        PathBuf::from(std::env::var("SESSION_V8_OLD_BINARY").expect("pinned previous binary"));
    let target = std::env::var("SESSION_V8_TARGET_BINARY")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(env!("CARGO_BIN_EXE_vqro")));
    hidden_member_handoff(Some(&old), Some(&target));
}

fn hidden_member_handoff(source_binary: Option<&Path>, target_binary: Option<&Path>) {
    let _lock = test_lock();
    let base = unique_test_dir();
    fs::create_dir_all(&base).unwrap();
    let pinned_source = source_binary.map(|binary| {
        use sha2::Digest;
        let bytes = fs::read(binary).unwrap();
        assert_eq!(
            format!("{:x}", sha2::Sha256::digest(&bytes)),
            "12341bd294acdeeb3c39480b8ea9aac279c80b8e3368b5657ac34352e4392469"
        );
        let copy = base.join("previous-vqro");
        fs::copy(binary, &copy).unwrap();
        copy
    });
    let source_binary = pinned_source.as_deref();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let client_socket = runtime_dir.join("herdr-client.sock");
    let session_dir = config_home.join("vqro-dev");
    fs::create_dir_all(&session_dir).unwrap();
    if source_binary.is_some() {
        // The approved deployed binary is release-profile (vqro), while the
        // candidate is debug-profile (vqro-dev). Both must use this one private store.
        std::os::unix::fs::symlink("vqro-dev", config_home.join("vqro")).unwrap();
    }
    let mut fixture: serde_json::Value = serde_json::from_str(include_str!(
        "fixtures/session/hidden-member-handoff-v7.json"
    ))
    .unwrap();
    fixture["workspaces"][0]["identity_cwd"] = serde_json::json!(base);
    for pane in fixture["workspaces"][0]["tabs"][0]["panes"]
        .as_object_mut()
        .unwrap()
        .values_mut()
    {
        pane["cwd"] = serde_json::json!(base);
    }
    // Burn three source IDs with ordinary restored terminals, then close their workspace.
    // The target starts allocation afresh: every retained child's raw ID must need an alias,
    // rather than accidentally passing via a numerically identical live pane ID.
    let warmup_tabs: Vec<_> = (4..=6).map(|number| {
        let raw = 800_000 + number;
        serde_json::json!({
            "layout": {"Pane":raw},
            "panes": {raw.to_string(): {"cwd":base, "terminal_id":format!("term_{}", number.to_string().repeat(32))}},
            "zoomed":false,"focused":raw,"root_pane":raw
        })
    }).collect();
    fixture["workspaces"].as_array_mut().unwrap().insert(
        0,
        serde_json::json!({
            "id":"w1", "identity_cwd":base, "tabs":warmup_tabs, "active_tab":0
        }),
    );
    fixture["active"] = serde_json::json!(1);
    fs::write(session_dir.join("session.json"), fixture.to_string()).unwrap();
    // Legacy is an admitted persisted authority phase, not a fake package grant.
    // This exercises loaded authority validation and ordinary server activation.
    let home = base.join("home");
    let state_home = base.join("state");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&state_home).unwrap();
    let startup_started = std::time::Instant::now();
    let spawned = spawn_server_binary_with_env(
        source_binary.unwrap_or_else(|| Path::new(env!("CARGO_BIN_EXE_vqro"))),
        &config_home,
        &runtime_dir,
        &api_socket,
        &[
            ("HOME", home.to_str().unwrap()),
            ("XDG_STATE_HOME", state_home.to_str().unwrap()),
        ],
    );
    register_runtime_dir(&runtime_dir);
    wait_for_socket(&api_socket, Duration::from_secs(10));
    wait_for_api(&api_socket, Duration::from_secs(10));
    let startup_durable_by_api_ready = startup_started.elapsed();
    let close_started = std::time::Instant::now();
    assert_ok(request(
        &api_socket,
        serde_json::json!({"id":"test:warmup-close","method":"workspace.close","params":{"workspace_id":"w1"}}),
    ));
    assert_hidden_handoff_collection(&api_socket);
    // Closing the warmup triggers a real host checkpoint. Read its actual raw IDs,
    // not the seed's IDs (which were remapped at startup) or a test alias derivation.
    // The existing save debounce is five seconds: a five-second observer deadline
    // races that cadence under load. Keep the exact content predicate with margin.
    let session_path = session_dir.join("session.json");
    let mut source = serde_json::Value::Null;
    assert!(
        support::wait_until(Duration::from_secs(20), Duration::from_millis(25), || {
            source = fs::read(&session_path)
                .ok()
                .and_then(|bytes| serde_json::from_slice(&bytes).ok())
                .unwrap_or(serde_json::Value::Null);
            source["workspaces"]
                .as_array()
                .is_some_and(|workspaces| workspaces.len() == 1)
                && source["workspaces"][0]["tabs"][0]["panes"]
                    .as_object()
                    .is_some_and(|panes| {
                        panes
                            .keys()
                            .all(|raw| raw.parse::<u32>().is_ok_and(|raw| raw <= 6))
                    })
        }),
        "source checkpoint: {source}; server log: {}",
        fs::read_to_string(session_dir.join("vqro-server.log")).unwrap_or_default()
    );
    println!("source checkpoint timing: source={} startup_durable_by_api_ready_ms={} close_to_observed_checkpoint_ms={} total_startup_to_observed_checkpoint_ms={}",
        if source_binary.is_some() {"pinned-master-release"} else {"candidate-debug"},
        startup_durable_by_api_ready.as_millis(), close_started.elapsed().as_millis(),
        startup_started.elapsed().as_millis());
    assert_saved_handoff_collection(&session_path);

    let mut owners = Vec::new();
    for number in 1..=3 {
        let public_id = format!("w2:p{number}");
        let terminal_id = format!("term_{}", number.to_string().repeat(32));
        let raw = source["workspaces"][0]["tabs"][0]["panes"]
            .as_object()
            .unwrap()
            .iter()
            .find(|(_, pane)| pane["terminal_id"] == terminal_id)
            .unwrap()
            .0;
        let raw_id = format!("p_{raw}");
        assert!(
            raw.parse::<u32>().unwrap() > 3,
            "source IDs must need remapping: {raw}"
        );
        assert_handoff_pane_identity(&api_socket, &public_id, number);
        let marker = base.join(format!("owner-{number}"));
        let received = base.join(format!("received-{number}"));
        let hup = base.join(format!("hup-{number}"));
        let script = base.join(format!("receiver-{number}.sh"));
        fs::write(
            &script,
            format!(
                "echo \"$$ $VQRO_PANE_ID\" > {marker}\n\
                 trap 'echo HUP > {hup}' HUP\n\
                 while IFS= read -r line; do\n\
                 echo \"owner{number}:$$:$line\" >> {received}\n\
                 echo \"owner{number}:$$:$line\"\n\
                 done\n",
                marker = marker.display(),
                hup = hup.display(),
                received = received.display(),
            ),
        )
        .unwrap();
        assert_ok(request(
            &api_socket,
            serde_json::json!({"id":"test:receiver","method":"pane.send_input","params":{
                "pane_id":public_id,"text":format!("sh {}", script.display()),"keys":["Enter"]
            }}),
        ));
        let owner = wait_for_file_contains(&marker, &public_id, Duration::from_secs(5));
        let mut fields = owner.split_whitespace();
        let pid: u32 = fields.next().unwrap().parse().unwrap();
        assert_eq!(
            fields.next(),
            Some(public_id.as_str()),
            "child pane identity"
        );
        assert_handoff_pane_identity(&api_socket, &raw_id, number);
        let token = format!("before{number}");
        assert_ok(request(
            &api_socket,
            serde_json::json!({"id":"test:before","method":"pane.send_input","params":{
                "pane_id":raw_id,"text":token,"keys":["Enter"]
            }}),
        ));
        let expected = format!("owner{number}:{pid}:{token}");
        wait_for_file_contains(&received, &expected, Duration::from_secs(5));
        wait_for_output(&api_socket, &raw_id, &expected);
        owners.push((raw_id, pid, received, hup));
    }
    assert_eq!(
        owners
            .iter()
            .map(|owner| &owner.0)
            .collect::<std::collections::HashSet<_>>()
            .len(),
        3
    );
    assert_eq!(
        owners
            .iter()
            .map(|owner| owner.1)
            .collect::<std::collections::HashSet<_>>()
            .len(),
        3
    );

    let mut client_stream = UnixStream::connect(&client_socket).unwrap();
    let generation = support::CURRENT_ENDPOINT_PROTOCOL_GENERATION;
    let (server_generation, error) =
        client_shell_handshake(&mut client_stream, generation, 80, 24).unwrap();
    assert_eq!(server_generation, generation);
    assert!(error.is_none());
    wait_for_client_shell_bootstrap(&mut client_stream, Duration::from_secs(5)).unwrap();
    if source_binary.is_some() {
        assert!(
            source["version"].as_u64().unwrap() < current_snapshot_version() as u64,
            "cross-version fence requires an actual old writer checkpoint"
        );
        // Seed valid v7 legacy-authority metadata at the last durable checkpoint.
        // No package grant is fabricated and running pane state is untouched.
        source = serde_json::from_slice(&fs::read(&session_path).unwrap()).unwrap();
        source["version"] = serde_json::json!(7);
        source["collections_authority"] = serde_json::json!({
            "contract":"collections_authority.v1","phase":"legacy",
            "authority_epoch":1,"minimum_reader_version":7
        });
        source["collections_action_sequence"] = serde_json::json!(9);
        fs::write(&session_path, source.to_string()).unwrap();
        assert_eq!(source["collections_action_sequence"], 9);
    }
    let source_bytes = fs::read(&session_path).unwrap();
    let params = target_binary
        .map(|binary| serde_json::json!({"import_exe":binary}))
        .unwrap_or_else(|| serde_json::json!({}));
    assert_ok(request(
        &api_socket,
        serde_json::json!({"id":"test:handoff","method":"server.live_handoff","params":params}),
    ));
    assert!(wait_for_message_variant(
        &mut client_stream,
        Duration::from_secs(5),
        SERVER_MESSAGE_SERVER_SHUTDOWN,
    )
    .unwrap());
    drop(spawned);
    thread::sleep(Duration::from_millis(300));
    wait_for_api(&api_socket, Duration::from_secs(10));
    assert_hidden_handoff_collection(&api_socket);
    assert_saved_handoff_collection(&session_dir.join("session.json"));
    if source_binary.is_some() {
        let converted: serde_json::Value =
            serde_json::from_slice(&fs::read(&session_path).unwrap()).unwrap();
        assert_eq!(
            converted["version"],
            current_snapshot_version(),
            "handoff must durably publish the new format"
        );
        let prefix = format!(
            "session.json.pre-v{}-v{}.",
            current_snapshot_version(),
            source["version"]
        );
        let backups: Vec<_> = fs::read_dir(&session_dir)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                path.file_name()
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .starts_with(&prefix)
            })
            .collect();
        assert_eq!(
            backups.len(),
            1,
            "handoff conversion must back up the last source checkpoint"
        );
        assert_eq!(fs::read(&backups[0]).unwrap(), source_bytes);
        let log = wait_for_file_contains(
            &session_dir.join("vqro-server.log"),
            "exported_collections_action_sequence=0",
            Duration::from_secs(5),
        );
        assert!(
            log.contains("source_snapshot_version=6"),
            "must exercise the real old live exporter"
        );
        assert!(
            converted["collections_action_sequence"]
                .as_u64()
                .unwrap_or(0)
                >= source["collections_action_sequence"].as_u64().unwrap(),
            "old live export must not lower the durable action high-water: {converted}"
        );
    }

    // Public IDs alone miss the bug: peers can still name pre-handoff raw IDs.
    // Exact pane/terminal pairing also proves each tested alias names a live pane.
    // The in-process restore fence owns the full assert_invariants_for_test check.
    // Check the hidden member first so the old derivation fails on that member itself.
    for index in [1, 0, 2] {
        let (raw_id, pid, received, hup) = &owners[index];
        let number = index + 1;
        assert_handoff_pane_identity(&api_socket, raw_id, number);
        assert_handoff_pane_identity(&api_socket, &format!("w2:p{number}"), number);
        assert_eq!(unsafe { libc::kill(*pid as libc::pid_t, 0) }, 0);
        assert!(!hup.exists(), "owner {number} received HUP");
        wait_for_output(
            &api_socket,
            raw_id,
            &format!("owner{number}:{pid}:before{number}"),
        );
        for (lane, target) in [("raw", raw_id.clone()), ("public", format!("w2:p{number}"))] {
            let token = format!("after{number}-{lane}");
            assert_ok(request(
                &api_socket,
                serde_json::json!({"id":"test:after","method":"pane.send_input","params":{
                    "pane_id":target,"text":token,"keys":["Enter"]
                }}),
            ));
            let expected = format!("owner{number}:{pid}:{token}");
            wait_for_file_contains(received, &expected, Duration::from_secs(5));
            wait_for_output(&api_socket, raw_id, &expected);
        }
    }
    // Delivery is proved by the same child, not by API success or terminal echo.
    // Exact logs catch sibling delivery, duplicates, and loss for hidden and visible panes.
    for (index, (_, pid, received, hup)) in owners.iter().enumerate() {
        let number = index + 1;
        assert_eq!(fs::read_to_string(received).unwrap(), format!(
            "owner{number}:{pid}:before{number}\nowner{number}:{pid}:after{number}-raw\nowner{number}:{pid}:after{number}-public\n"
        ));
        assert!(!hup.exists());
    }
    assert_ok(request(
        &api_socket,
        serde_json::json!({"id":"test:stop","method":"server.stop","params":{}}),
    ));
    if let Some(previous) = source_binary {
        assert!(
            support::wait_until(Duration::from_secs(5), Duration::from_millis(25), || {
                !api_socket.exists()
            }),
            "new server must stop before downgrade test"
        );
        let authoritative = fs::read(&session_path).unwrap();
        let after_ordinary_save: serde_json::Value =
            serde_json::from_slice(&authoritative).unwrap();
        assert!(
            after_ordinary_save["collections_action_sequence"]
                .as_u64()
                .unwrap_or(0)
                >= source["collections_action_sequence"].as_u64().unwrap(),
            "ordinary post-import save must retain the adopted action high-water"
        );
        let rejected = spawn_server_binary_with_env(
            previous,
            &config_home,
            &runtime_dir,
            &api_socket,
            &[
                ("HOME", home.to_str().unwrap()),
                ("XDG_STATE_HOME", state_home.to_str().unwrap()),
            ],
        );
        wait_for_api(&api_socket, Duration::from_secs(10));
        // The previous build refuses restoring future bytes and fences saves;
        // it remains available only for generic recovery of an empty session.
        let log = wait_for_file_contains(
            &session_dir.join("vqro-server.log"),
            &format!(
                "session file version {} is newer than supported",
                current_snapshot_version()
            ),
            Duration::from_secs(5),
        );
        assert!(log.contains("session load rejected"));
        assert_eq!(fs::read(&session_path).unwrap(), authoritative);
        assert_ok(request(
            &api_socket,
            serde_json::json!({"id":"test:downgrade-stop","method":"server.stop","params":{}}),
        ));
        drop(rejected);
        assert_eq!(fs::read(&session_path).unwrap(), authoritative);
    }
    cleanup_test_base(&base);
}

#[test]
fn live_handoff_preserves_keyboard_protocol_for_client_input() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let client_socket = runtime_dir.join("herdr-client.sock");
    let script = base.join("read-raw.py");
    let ready_marker = base.join("keyboard-ready");
    let received_marker = base.join("keyboard-received");

    fs::create_dir_all(&base).unwrap();
    fs::write(
        &script,
        format!(
            r#"import os
import pathlib
import select
import sys
import tty

sys.stdout.buffer.write(b"\x1b[>5u")
sys.stdout.flush()
pathlib.Path({ready:?}).write_text("ready")
tty.setraw(sys.stdin.fileno())
ready_fds, _, _ = select.select([sys.stdin.fileno()], [], [], 5)
data = os.read(sys.stdin.fileno(), 32) if ready_fds else b""
pathlib.Path({received:?}).write_text(data.hex())
"#,
            ready = ready_marker.display().to_string(),
            received = received_marker.display().to_string()
        ),
    )
    .unwrap();

    let spawned = spawn_server(&config_home, &runtime_dir, &api_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));
    register_runtime_dir(&runtime_dir);

    let created = request(
        &api_socket,
        serde_json::json!({
            "id": "test:workspace:create",
            "method": "workspace.create",
            "params": {"cwd": "/tmp", "focus": true}
        }),
    );
    let pane_id = created["result"]["root_pane"]["pane_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ok(request(
        &api_socket,
        serde_json::json!({
            "id": "test:pane:run",
            "method": "pane.send_input",
            "params": {"pane_id": pane_id, "text": format!("python3 {}", script.display()), "keys": ["Enter"]}
        }),
    ));
    support::wait_for_file(&ready_marker, Duration::from_secs(5));

    assert_ok(request(
        &api_socket,
        serde_json::json!({"id":"test:handoff","method":"server.live_handoff","params":{}}),
    ));
    drop(spawned);
    wait_for_api(&api_socket, Duration::from_secs(10));
    wait_for_socket(&client_socket, Duration::from_secs(5));

    let mut client_stream = UnixStream::connect(&client_socket).unwrap();
    let (server_generation, error) = client_shell_handshake(
        &mut client_stream,
        support::CURRENT_ENDPOINT_PROTOCOL_GENERATION,
        54,
        23,
    )
    .unwrap();
    assert_eq!(
        server_generation,
        support::CURRENT_ENDPOINT_PROTOCOL_GENERATION
    );
    assert!(error.is_none(), "client shell handshake failed: {error:?}");
    wait_for_client_shell_bootstrap(&mut client_stream, Duration::from_secs(5))
        .expect("client shell should receive restored state before sending input");
    send_client_shell_shift_enter(&mut client_stream, &pane_id).unwrap();

    wait_for_file_contains(&received_marker, "1b5b31333b3275", Duration::from_secs(5));

    let _ = request(
        &api_socket,
        serde_json::json!({"id":"test:stop","method":"server.stop","params":{}}),
    );
    cleanup_test_base(&base);
}

#[test]
fn live_handoff_preserves_modify_other_keys_for_client_input() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let client_socket = runtime_dir.join("herdr-client.sock");
    let script = base.join("read-raw.py");
    let ready_marker = base.join("modify-ready");
    let received_marker = base.join("modify-received");

    fs::create_dir_all(&base).unwrap();
    fs::write(
        &script,
        format!(
            r#"import os
import pathlib
import select
import sys
import tty

sys.stdout.buffer.write(b"\x1b[>4;2m")
sys.stdout.flush()
pathlib.Path({ready:?}).write_text("ready")
tty.setraw(sys.stdin.fileno())
ready_fds, _, _ = select.select([sys.stdin.fileno()], [], [], 5)
data = os.read(sys.stdin.fileno(), 32) if ready_fds else b""
pathlib.Path({received:?}).write_text(data.hex())
"#,
            ready = ready_marker.display().to_string(),
            received = received_marker.display().to_string()
        ),
    )
    .unwrap();

    let spawned = spawn_server(&config_home, &runtime_dir, &api_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));
    register_runtime_dir(&runtime_dir);

    let created = request(
        &api_socket,
        serde_json::json!({
            "id": "test:workspace:create",
            "method": "workspace.create",
            "params": {"cwd": "/tmp", "focus": true}
        }),
    );
    let pane_id = created["result"]["root_pane"]["pane_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ok(request(
        &api_socket,
        serde_json::json!({
            "id": "test:pane:run",
            "method": "pane.send_input",
            "params": {"pane_id": pane_id, "text": format!("python3 {}", script.display()), "keys": ["Enter"]}
        }),
    ));
    support::wait_for_file(&ready_marker, Duration::from_secs(5));

    assert_ok(request(
        &api_socket,
        serde_json::json!({"id":"test:handoff","method":"server.live_handoff","params":{}}),
    ));
    drop(spawned);
    wait_for_api(&api_socket, Duration::from_secs(10));
    wait_for_socket(&client_socket, Duration::from_secs(5));

    let mut client_stream = UnixStream::connect(&client_socket).unwrap();
    let (server_generation, error) = client_shell_handshake(
        &mut client_stream,
        support::CURRENT_ENDPOINT_PROTOCOL_GENERATION,
        54,
        23,
    )
    .unwrap();
    assert_eq!(
        server_generation,
        support::CURRENT_ENDPOINT_PROTOCOL_GENERATION
    );
    assert!(error.is_none(), "client shell handshake failed: {error:?}");
    wait_for_client_shell_bootstrap(&mut client_stream, Duration::from_secs(5))
        .expect("client shell should receive restored state before sending input");
    send_client_shell_shift_enter(&mut client_stream, &pane_id).unwrap();

    wait_for_file_contains(
        &received_marker,
        "1b5b32373b323b31337e",
        Duration::from_secs(5),
    );

    let _ = request(
        &api_socket,
        serde_json::json!({"id":"test:stop","method":"server.stop","params":{}}),
    );
    cleanup_test_base(&base);
}

#[test]
fn live_handoff_accepts_canonical_pane_id_from_child_env() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let pane_id_marker = base.join("pane-id");

    let spawned = spawn_server(&config_home, &runtime_dir, &api_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));
    register_runtime_dir(&runtime_dir);

    let created = request(
        &api_socket,
        serde_json::json!({
            "id": "test:workspace:create",
            "method": "workspace.create",
            "params": {"cwd": "/tmp", "focus": true}
        }),
    );
    let pane_id = created["result"]["root_pane"]["pane_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ok(request(
        &api_socket,
        serde_json::json!({
            "id": "test:pane:print-id",
            "method": "pane.send_input",
            "params": {"pane_id": pane_id, "text": format!("printf '%s' \"$VQRO_PANE_ID\" > {}", pane_id_marker.display()), "keys": ["Enter"]}
        }),
    ));
    let old_pane_id = wait_for_file_contains(&pane_id_marker, &pane_id, Duration::from_secs(5));
    assert!(
        old_pane_id == pane_id,
        "unexpected pane id from env: {old_pane_id:?}"
    );

    assert_ok(request(
        &api_socket,
        serde_json::json!({"id":"test:handoff","method":"server.live_handoff","params":{}}),
    ));
    drop(spawned);
    wait_for_api(&api_socket, Duration::from_secs(10));

    assert_ok(request(
        &api_socket,
        serde_json::json!({
            "id": "test:old-pane-report",
            "method": "pane.report_agent",
            "params": {
                "pane_id": old_pane_id,
                "source": "handoff-test",
                "agent": "pi",
                "state": "working"
            }
        }),
    ));
    let agents = request(
        &api_socket,
        serde_json::json!({"id":"test:agent-list","method":"agent.list","params":{}}),
    );
    let found = agents["result"]["agents"]
        .as_array()
        .unwrap()
        .iter()
        .any(|agent| {
            agent["agent"].as_str() == Some("pi")
                && agent["agent_status"].as_str() == Some("working")
        });
    assert!(
        found,
        "old pane id report did not update restored pane: {agents}"
    );

    let _ = request(
        &api_socket,
        serde_json::json!({"id":"test:stop","method":"server.stop","params":{}}),
    );
    cleanup_test_base(&base);
}

#[test]
fn live_handoff_keeps_unmanaged_agent_name_bound_to_saved_session() {
    use std::os::unix::fs::PermissionsExt;

    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let old_session = base.join("old-session.jsonl");
    let new_session = base.join("new-session.jsonl");
    let started_marker = base.join("agent-started");
    let fake_pi = base.join("pi");
    fs::create_dir_all(&base).unwrap();
    fs::write(
        &fake_pi,
        format!(
            "#!/bin/sh\nexport HERDR_AGENT=pi\necho started > {}\n/bin/sleep 30\n:\n",
            started_marker.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&fake_pi, fs::Permissions::from_mode(0o755)).unwrap();

    let spawned = spawn_server(&config_home, &runtime_dir, &api_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));
    register_runtime_dir(&runtime_dir);
    let created = request(
        &api_socket,
        serde_json::json!({
            "id": "test:workspace:create",
            "method": "workspace.create",
            "params": {"cwd": "/tmp", "focus": true}
        }),
    );
    let pane_id = created["result"]["root_pane"]["pane_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ok(request(
        &api_socket,
        serde_json::json!({
            "id": "test:pane:start-agent",
            "method": "pane.send_input",
            "params": {"pane_id": pane_id, "text": fake_pi, "keys": ["Enter"]}
        }),
    ));
    support::wait_for_file(&started_marker, Duration::from_secs(5));
    assert_ok(request(
        &api_socket,
        serde_json::json!({
            "id": "test:agent:session",
            "method": "pane.report_agent_session",
            "params": {
                "pane_id": pane_id,
                "source": "vqro:pi",
                "agent": "pi",
                "seq": 1,
                "agent_session_path": old_session,
                "session_start_source": "startup"
            }
        }),
    ));
    assert_ok(request(
        &api_socket,
        serde_json::json!({
            "id": "test:agent:report",
            "method": "pane.report_agent",
            "params": {
                "pane_id": pane_id,
                "source": "vqro:pi",
                "agent": "pi",
                "state": "idle",
                "seq": 2,
                "agent_session_path": old_session
            }
        }),
    ));
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let response = request(
            &api_socket,
            serde_json::json!({
                "id": "test:agent:wait-for-process",
                "method": "agent.get",
                "params": {"target": pane_id}
            }),
        );
        if response.get("result").is_some() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "agent process was not detected: {response}"
        );
        thread::sleep(Duration::from_millis(25));
    }
    assert_ok(request(
        &api_socket,
        serde_json::json!({
            "id": "test:agent:rename",
            "method": "agent.rename",
            "params": {"target": pane_id, "name": "reviewer"}
        }),
    ));

    assert_ok(request(
        &api_socket,
        serde_json::json!({"id":"test:handoff","method":"server.live_handoff","params":{}}),
    ));
    drop(spawned);
    wait_for_api(&api_socket, Duration::from_secs(10));

    assert_ok(request(
        &api_socket,
        serde_json::json!({
            "id": "test:agent:new-session",
            "method": "pane.report_agent_session",
            "params": {
                "pane_id": pane_id,
                "source": "vqro:pi",
                "agent": "pi",
                "seq": 3,
                "agent_session_path": new_session,
                "session_start_source": "new"
            }
        }),
    ));
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let old_name = request(
            &api_socket,
            serde_json::json!({
                "id": "test:agent:get-old-name",
                "method": "agent.get",
                "params": {"target": "reviewer"}
            }),
        );
        if old_name["error"]["code"] == "agent_not_found" {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "old session alias was not cleared: {old_name}"
        );
        thread::sleep(Duration::from_millis(25));
    }

    let _ = request(
        &api_socket,
        serde_json::json!({"id":"test:stop","method":"server.stop","params":{}}),
    );
    cleanup_test_base(&base);
}

#[test]
fn live_handoff_keeps_agent_started_pane_after_agent_exits() {
    use std::os::unix::fs::PermissionsExt;

    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let started_marker = base.join("agent-started");
    let exited_marker = base.join("agent-exited");
    let shell_marker = base.join("shell-after-agent");
    let bin = base.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let fake_pi = bin.join("pi");
    fs::write(
        &fake_pi,
        format!(
            "#!/bin/sh\nexport HERDR_AGENT=pi\necho started > {}\n/bin/sleep 1\necho exited > {}\n",
            started_marker.display(),
            exited_marker.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&fake_pi, fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!("{}:/bin:/usr/bin", bin.display());

    let spawned = spawn_server_with_env(
        &config_home,
        &runtime_dir,
        &api_socket,
        &[("PATH", path.as_str())],
    );
    wait_for_socket(&api_socket, Duration::from_secs(10));
    register_runtime_dir(&runtime_dir);
    let workspace = request(
        &api_socket,
        serde_json::json!({
            "id": "test:workspace-create",
            "method": "workspace.create",
            "params": { "cwd": "/tmp", "focus": false }
        }),
    );
    assert_ok(workspace.clone());
    let pane_id = workspace["result"]["root_pane"]["pane_id"]
        .as_str()
        .unwrap()
        .to_string();

    let started = request(
        &api_socket,
        serde_json::json!({
            "id": "test:agent-start",
            "method": "agent.start",
            "params": {
                "name": "handoff-agent",
                "kind": "pi",
                "pane_id": pane_id,
                "timeout_ms": 5000
            }
        }),
    );
    assert_ok(started);
    support::wait_for_file(&started_marker, Duration::from_secs(5));

    assert_ok(request(
        &api_socket,
        serde_json::json!({"id":"test:handoff","method":"server.live_handoff","params":{}}),
    ));
    drop(spawned);
    wait_for_api(&api_socket, Duration::from_secs(10));
    support::wait_for_file(&exited_marker, Duration::from_secs(5));
    thread::sleep(Duration::from_millis(300));

    assert_ok(request(
        &api_socket,
        serde_json::json!({
            "id": "test:pane:shell-after-agent",
            "method": "pane.send_input",
            "params": {"pane_id": pane_id, "text": format!("echo alive > {}", shell_marker.display()), "keys": ["Enter"]}
        }),
    ));
    support::wait_for_file(&shell_marker, Duration::from_secs(5));

    let _ = request(
        &api_socket,
        serde_json::json!({"id":"test:stop","method":"server.stop","params":{}}),
    );
    cleanup_test_base(&base);
}

#[test]
fn live_handoff_keeps_shell_pane_after_foreground_process_exits() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let started_marker = base.join("foreground-started");
    let exited_marker = base.join("foreground-exited");
    let shell_marker = base.join("shell-after-foreground");

    let spawned = spawn_server(&config_home, &runtime_dir, &api_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));
    register_runtime_dir(&runtime_dir);

    let created = request(
        &api_socket,
        serde_json::json!({
            "id": "test:workspace:create",
            "method": "workspace.create",
            "params": {"cwd": "/tmp", "focus": true}
        }),
    );
    let pane_id = created["result"]["root_pane"]["pane_id"]
        .as_str()
        .unwrap()
        .to_string();
    let command = format!(
        "sh -c 'echo started > {}; sleep 1; echo exited > {}'",
        started_marker.display(),
        exited_marker.display()
    );
    assert_ok(request(
        &api_socket,
        serde_json::json!({
            "id": "test:pane:run-foreground",
            "method": "pane.send_input",
            "params": {"pane_id": pane_id, "text": command, "keys": ["Enter"]}
        }),
    ));
    support::wait_for_file(&started_marker, Duration::from_secs(5));

    assert_ok(request(
        &api_socket,
        serde_json::json!({"id":"test:handoff","method":"server.live_handoff","params":{}}),
    ));
    drop(spawned);
    wait_for_api(&api_socket, Duration::from_secs(10));
    support::wait_for_file(&exited_marker, Duration::from_secs(5));

    assert_ok(request(
        &api_socket,
        serde_json::json!({
            "id": "test:pane:shell-after-foreground",
            "method": "pane.send_input",
            "params": {"pane_id": pane_id, "text": format!("echo alive > {}", shell_marker.display()), "keys": ["Enter"]}
        }),
    ));
    support::wait_for_file(&shell_marker, Duration::from_secs(5));

    let _ = request(
        &api_socket,
        serde_json::json!({"id":"test:stop","method":"server.stop","params":{}}),
    );
    cleanup_test_base(&base);
}

#[test]
fn live_handoff_preserves_python_http_server() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let client_socket = runtime_dir.join("herdr-client.sock");
    let web_root = base.join("web");
    fs::create_dir_all(&web_root).unwrap();
    fs::write(
        web_root.join("index.html"),
        "hello-from-python-before-and-after",
    )
    .unwrap();
    let port = unused_local_port();

    let spawned = spawn_server(&config_home, &runtime_dir, &api_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));
    register_runtime_dir(&runtime_dir);

    let created = request(
        &api_socket,
        serde_json::json!({
            "id": "test:workspace:create",
            "method": "workspace.create",
            "params": {"cwd": web_root, "focus": true}
        }),
    );
    let pane_id = created["result"]["root_pane"]["pane_id"]
        .as_str()
        .unwrap()
        .to_string();

    assert_ok(request(
        &api_socket,
        serde_json::json!({
            "id": "test:pane:run-python",
            "method": "pane.send_input",
            "params": {
                "pane_id": pane_id,
                "text": format!("python3 -m http.server {port} --bind 127.0.0.1"),
                "keys": ["Enter"]
            }
        }),
    ));
    wait_for_http_contains(
        port,
        "hello-from-python-before-and-after",
        Duration::from_secs(10),
    );

    assert_ok(request(
        &api_socket,
        serde_json::json!({"id":"test:handoff","method":"server.live_handoff","params":{}}),
    ));
    drop(spawned);
    wait_for_api(&api_socket, Duration::from_secs(10));
    wait_for_http_contains(
        port,
        "hello-from-python-before-and-after",
        Duration::from_secs(10),
    );

    let _ = request(
        &api_socket,
        serde_json::json!({"id":"test:stop","method":"server.stop","params":{}}),
    );
    let _ = client_socket;
    cleanup_test_base(&base);
}

#[test]
fn live_handoff_preserves_http_servers_across_multiple_sessions() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let sessions = [
        (None, config_home.join("vqro-dev/vqro.sock")),
        (
            Some("work"),
            config_home.join("vqro-dev/sessions/work/vqro.sock"),
        ),
    ];
    let mut spawned = Vec::new();
    let mut ports = Vec::new();

    for (session_name, api_socket) in &sessions {
        let web_root = base.join(format!("web-{}", session_name.unwrap_or("default")));
        fs::create_dir_all(&web_root).unwrap();
        fs::write(
            web_root.join("index.html"),
            format!("hello-from-{}", session_name.unwrap_or("default")),
        )
        .unwrap();
        let port = unused_local_port();
        let server = if let Some(session_name) = session_name {
            spawn_named_session_server(&config_home, &runtime_dir, session_name)
        } else {
            spawn_default_session_server(&config_home, &runtime_dir)
        };
        wait_for_socket(api_socket, Duration::from_secs(10));
        let created = request(
            api_socket,
            serde_json::json!({
                "id": "test:workspace:create",
                "method": "workspace.create",
                "params": {"cwd": web_root, "focus": true}
            }),
        );
        let pane_id = created["result"]["root_pane"]["pane_id"]
            .as_str()
            .unwrap()
            .to_string();
        assert_ok(request(
            api_socket,
            serde_json::json!({
                "id": "test:pane:run-python",
                "method": "pane.send_input",
                "params": {
                    "pane_id": pane_id,
                    "text": format!("python3 -m http.server {port} --bind 127.0.0.1"),
                    "keys": ["Enter"]
                }
            }),
        ));
        wait_for_http_contains(
            port,
            &format!("hello-from-{}", session_name.unwrap_or("default")),
            Duration::from_secs(10),
        );
        spawned.push(server);
        ports.push((port, session_name.unwrap_or("default").to_string()));
    }
    register_runtime_dir(&runtime_dir);

    for (_session_name, api_socket) in &sessions {
        assert_ok(request(
            api_socket,
            serde_json::json!({"id":"test:handoff","method":"server.live_handoff","params":{}}),
        ));
    }
    drop(spawned);

    for (_session_name, api_socket) in &sessions {
        wait_for_api(api_socket, Duration::from_secs(10));
    }
    for (port, label) in ports {
        wait_for_http_contains(
            port,
            &format!("hello-from-{label}"),
            Duration::from_secs(10),
        );
    }

    for (_session_name, api_socket) in &sessions {
        let _ = request(
            api_socket,
            serde_json::json!({"id":"test:stop","method":"server.stop","params":{}}),
        );
    }
    cleanup_test_base(&base);
}

#[test]
fn live_handoff_bad_expected_protocol_rolls_back_old_server() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let marker = base.join("child.pid");
    let received_marker = base.join("received");

    let spawned = spawn_server(&config_home, &runtime_dir, &api_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));
    register_runtime_dir(&runtime_dir);

    let created = request(
        &api_socket,
        serde_json::json!({
            "id": "test:workspace:create",
            "method": "workspace.create",
            "params": {"cwd": "/tmp", "focus": true}
        }),
    );
    let pane_id = created["result"]["root_pane"]["pane_id"]
        .as_str()
        .unwrap()
        .to_string();
    let command = format!(
        "sh -c 'echo READY $$ > {}; while read line; do echo got:$line; echo got:$line >> {}; done'",
        marker.display(),
        received_marker.display()
    );
    assert_ok(request(
        &api_socket,
        serde_json::json!({
            "id": "test:pane:run",
            "method": "pane.send_input",
            "params": {"pane_id": pane_id, "text": command, "keys": ["Enter"]}
        }),
    ));
    support::wait_for_file(&marker, Duration::from_secs(5));
    let pid_text = fs::read_to_string(&marker).unwrap();
    let child_pid: u32 = pid_text.split_whitespace().last().unwrap().parse().unwrap();

    let failed = request(
        &api_socket,
        serde_json::json!({
            "id": "test:bad-handoff",
            "method": "server.live_handoff",
            "params": {"expected_protocol": 999999}
        }),
    );
    assert!(
        failed.get("error").is_some(),
        "bad protocol handoff should fail: {failed}"
    );
    wait_for_api(&api_socket, Duration::from_secs(5));
    assert_eq!(unsafe { libc::kill(child_pid as libc::pid_t, 0) }, 0);

    assert_ok(request(
        &api_socket,
        serde_json::json!({
            "id": "test:pane:send-after-failed-handoff",
            "method": "pane.send_input",
            "params": {"pane_id": pane_id, "text": "after-failed-handoff", "keys": ["Enter"]}
        }),
    ));
    wait_for_file_contains(
        &received_marker,
        "got:after-failed-handoff",
        Duration::from_secs(5),
    );
    wait_for_output(&api_socket, &pane_id, "got:after-failed-handoff");

    let _ = request(
        &api_socket,
        serde_json::json!({"id":"test:stop","method":"server.stop","params":{}}),
    );
    drop(spawned);
    cleanup_test_base(&base);
}

struct HandoffLineReader {
    stream: UnixStream,
    buffered: Vec<u8>,
    timeout: Duration,
}

impl HandoffLineReader {
    fn new(stream: &UnixStream, timeout: Duration) -> io::Result<Self> {
        Ok(Self {
            stream: stream.try_clone()?,
            buffered: Vec::new(),
            timeout,
        })
    }

    fn read_line(&mut self) -> io::Result<String> {
        let deadline = Instant::now() + self.timeout;
        loop {
            if let Some(newline) = self.buffered.iter().position(|byte| *byte == b'\n') {
                let bytes: Vec<_> = self.buffered.drain(..=newline).collect();
                let line = String::from_utf8(bytes).map_err(|err| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("handoff peer sent a non-UTF-8 line: {err}"),
                    )
                })?;
                return Ok(line.trim_end().to_string());
            }

            let now = Instant::now();
            if now >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!(
                        "timed out after {:?} waiting for a handoff line ({} partial bytes)",
                        self.timeout,
                        self.buffered.len()
                    ),
                ));
            }
            let remaining = deadline.saturating_duration_since(now);
            let timeout_ms = remaining
                .as_millis()
                .saturating_add(u128::from(
                    !remaining.subsec_nanos().is_multiple_of(1_000_000),
                ))
                .min(i32::MAX as u128) as i32;
            let mut poll_fd = libc::pollfd {
                fd: self.stream.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            let ready = unsafe { libc::poll(&mut poll_fd, 1, timeout_ms) };
            if ready == 0 {
                continue;
            }
            if ready < 0 {
                let err = io::Error::last_os_error();
                if err.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(io::Error::new(
                    err.kind(),
                    format!("polling the handoff socket failed: {err}"),
                ));
            }
            if poll_fd.revents & libc::POLLNVAL != 0 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "handoff socket became invalid while reading a line",
                ));
            }

            let mut chunk = [0_u8; 1024];
            match self.stream.read(&mut chunk) {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        format!(
                            "handoff peer closed before completing a line ({} partial bytes)",
                            self.buffered.len()
                        ),
                    ));
                }
                Ok(read) => self.buffered.extend_from_slice(&chunk[..read]),
                Err(err)
                    if matches!(
                        err.kind(),
                        io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
                    ) => {}
                Err(err) => {
                    return Err(io::Error::new(
                        err.kind(),
                        format!("reading from the handoff socket failed: {err}"),
                    ));
                }
            }
        }
    }
}

#[test]
fn handoff_line_reader_preserves_partial_and_complete_buffered_lines() {
    let (reader_stream, mut writer_stream) = UnixStream::pair().unwrap();
    reader_stream.set_nonblocking(true).unwrap();
    writer_stream.write_all(b"tial\nnext\n").unwrap();

    let mut reader = HandoffLineReader::new(&reader_stream, Duration::from_secs(1)).unwrap();
    reader.buffered.extend_from_slice(b"par");
    assert_eq!(reader.read_line().unwrap(), "partial");
    assert_eq!(reader.read_line().unwrap(), "next");

    reader.buffered.extend_from_slice(b"first\nsecond\n");
    assert_eq!(reader.read_line().unwrap(), "first");
    assert_eq!(reader.read_line().unwrap(), "second");
}

fn set_fd_nonblocking(fd: std::os::fd::RawFd) -> io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn read_until_contains(
    reader: &mut impl Read,
    needle: &[u8],
    timeout: Duration,
) -> io::Result<Vec<u8>> {
    assert!(!needle.is_empty());
    let deadline = Instant::now() + timeout;
    let mut output = Vec::new();
    let mut chunk = [0_u8; 1024];
    loop {
        if output.windows(needle.len()).any(|window| window == needle) {
            return Ok(output);
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!(
                    "timed out after {timeout:?} waiting for {:?} in PTY output ({} bytes read)",
                    String::from_utf8_lossy(needle),
                    output.len()
                ),
            ));
        }

        match reader.read(&mut chunk) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    format!(
                        "PTY closed before {:?} appeared ({} bytes read)",
                        String::from_utf8_lossy(needle),
                        output.len()
                    ),
                ));
            }
            Ok(read) => output.extend_from_slice(&chunk[..read]),
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                thread::sleep(remaining.min(Duration::from_millis(10)));
            }
            Err(err) => {
                return Err(io::Error::new(
                    err.kind(),
                    format!("reading rollback PTY output failed: {err}"),
                ));
            }
        }
    }
}

#[test]
fn read_until_contains_retries_nonblocking_errors_and_preserves_output() {
    struct RetryReader {
        step: usize,
    }

    impl Read for RetryReader {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            self.step += 1;
            match self.step {
                1 => {
                    buffer[..7].copy_from_slice(b"legacy-");
                    Ok(7)
                }
                2 => Err(io::Error::from(io::ErrorKind::WouldBlock)),
                3 => Err(io::Error::from(io::ErrorKind::Interrupted)),
                _ => {
                    buffer[..11].copy_from_slice(b"rollback-ok");
                    Ok(11)
                }
            }
        }
    }

    let output = read_until_contains(
        &mut RetryReader { step: 0 },
        b"legacy-rollback-ok",
        Duration::from_secs(1),
    )
    .unwrap();
    assert_eq!(output, b"legacy-rollback-ok");
}

fn send_legacy_handoff_fd(stream: &UnixStream, fd: std::os::fd::RawFd) {
    let mut byte = [1_u8];
    let mut iov = libc::iovec {
        iov_base: byte.as_mut_ptr().cast(),
        iov_len: byte.len(),
    };
    let control_len = unsafe { libc::CMSG_SPACE(std::mem::size_of::<libc::c_int>() as _) } as usize;
    let mut control = vec![0_u8; control_len];
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_iov = &mut iov;
    message.msg_iovlen = 1;
    message.msg_control = control.as_mut_ptr().cast();
    message.msg_controllen = control.len() as _;
    unsafe {
        let header = libc::CMSG_FIRSTHDR(&message);
        (*header).cmsg_level = libc::SOL_SOCKET;
        (*header).cmsg_type = libc::SCM_RIGHTS;
        (*header).cmsg_len = libc::CMSG_LEN(std::mem::size_of::<libc::c_int>() as _) as _;
        std::ptr::copy_nonoverlapping(
            (&fd as *const std::os::fd::RawFd).cast::<u8>(),
            libc::CMSG_DATA(header),
            std::mem::size_of::<libc::c_int>(),
        );
        message.msg_controllen = (*header).cmsg_len;
        assert_eq!(libc::sendmsg(stream.as_raw_fd(), &message, 0), 1);
    }
}

fn run_legacy_v1_source_harness(commit: bool) {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let client_socket = runtime_dir.join("herdr-client.sock");
    let handoff_socket = runtime_dir.join("legacy-handoff.sock");
    register_runtime_dir(&runtime_dir);
    fs::create_dir_all(config_home.join("vqro-dev")).unwrap();
    fs::create_dir_all(&runtime_dir).unwrap();
    fs::write(
        config_home.join("vqro-dev/config.toml"),
        "onboarding = false\n",
    )
    .unwrap();

    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();
    let mut shell = CommandBuilder::new("/bin/sh");
    shell.env("TERM", "xterm-256color");
    let mut shell_child = pair.slave.spawn_command(shell).unwrap();
    drop(pair.slave);
    let master_fd = pair.master.as_raw_fd().unwrap();
    let mut source_reader = pair.master.try_clone_reader().unwrap();
    let mut source_writer = pair.master.take_writer().unwrap();

    let snapshot = if commit {
        serde_json::json!({
            "version": 4,
            "workspaces": [],
            "repositories": [],
            "space_order": [],
            "active": null,
            "selected": 0
        })
    } else {
        serde_json::json!({
            "version": 4,
            "workspaces": [{
                "id": "w1",
                "custom_name": "legacy",
                "identity_cwd": "/tmp",
                "public_pane_numbers": {"1": 1},
                "next_public_pane_number": 2,
                "public_tab_numbers": [1],
                "next_public_tab_number": 2,
                "tabs": [{
                    "custom_name": null,
                    "layout": {"Pane": 1},
                    "panes": {"1": {"cwd": "/tmp"}},
                    "zoomed": false,
                    "focused": 1,
                    "root_pane": 1
                }],
                "active_tab": 0
            }],
            "repositories": [],
            "space_order": [],
            "active": 0,
            "selected": 0
        })
    };
    let panes = if commit {
        serde_json::json!([])
    } else {
        serde_json::json!([{
            "pane_id": 1,
            "child_pid": shell_child.process_id(),
            "rows": 24,
            "cols": 80,
            "cell_width_px": 0,
            "cell_height_px": 0,
            "keyboard_protocol_flags": 0
        }])
    };
    let manifest = serde_json::json!({
        "version": 1,
        "source_version": "legacy-harness",
        "source_protocol": 22,
        "expected_version": null,
        "expected_protocol": null,
        "snapshot": snapshot,
        "panes": panes
    });
    let session_path = config_home.join("vqro-dev/session.json");
    let mut legacy_bytes = serde_json::to_vec_pretty(&manifest["snapshot"]).unwrap();
    legacy_bytes.push(b'\n');
    fs::write(&session_path, &legacy_bytes).unwrap();

    let listener = UnixListener::bind(&handoff_socket).unwrap();
    let token = "legacy-token";
    let mut target = std::process::Command::new(env!("CARGO_BIN_EXE_vqro"))
        .arg("server")
        .arg("--handoff-import")
        .arg(&handoff_socket)
        .arg(token)
        .env("XDG_CONFIG_HOME", &config_home)
        .env("XDG_RUNTIME_DIR", &runtime_dir)
        .env("VQRO_SOCKET_PATH", &api_socket)
        .env("VQRO_CLIENT_SOCKET_PATH", &client_socket)
        .env("SHELL", "/bin/sh")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let (mut stream, _) = listener.accept().unwrap();
    let mut handoff_reader = HandoffLineReader::new(&stream, Duration::from_secs(30)).unwrap();
    assert_eq!(handoff_reader.read_line().unwrap(), token);
    serde_json::to_writer(&mut stream, &manifest).unwrap();
    stream.write_all(b"\n").unwrap();
    stream.flush().unwrap();
    assert_eq!(handoff_reader.read_line().unwrap(), "validated");
    if !commit {
        send_legacy_handoff_fd(&stream, master_fd);
    }
    assert_eq!(handoff_reader.read_line().unwrap(), "restored");
    assert_eq!(handoff_reader.read_line().unwrap(), "ready");
    assert!(
        !api_socket.exists(),
        "legacy ready must not expose target API"
    );
    assert_eq!(fs::read(&session_path).unwrap(), legacy_bytes);

    if commit {
        stream.write_all(b"committed\n").unwrap();
        stream.flush().unwrap();
        drop(source_reader);
        drop(source_writer);
        drop(pair.master);
        assert_eq!(handoff_reader.read_line().unwrap(), "owned");
        wait_for_api(&api_socket, Duration::from_secs(10));
        let published: serde_json::Value =
            serde_json::from_slice(&fs::read(&session_path).unwrap()).unwrap();
        assert_eq!(published["version"], current_snapshot_version());
        assert!(published["workspaces"].as_array().unwrap().is_empty());
        let _ = request(
            &api_socket,
            serde_json::json!({"id":"legacy:stop","method":"server.stop","params":{}}),
        );
    } else {
        drop(handoff_reader);
        drop(stream);
        assert!(support::wait_until(
            Duration::from_secs(5),
            Duration::from_millis(25),
            || target.try_wait().unwrap().is_some()
        ));
        source_writer
            .write_all(b"echo legacy-rollback-ok\n")
            .unwrap();
        source_writer.flush().unwrap();
        set_fd_nonblocking(master_fd).unwrap();
        read_until_contains(
            &mut source_reader,
            b"legacy-rollback-ok",
            Duration::from_secs(5),
        )
        .unwrap();
        assert_eq!(fs::read(&session_path).unwrap(), legacy_bytes);
    }

    let _ = shell_child.kill();
    let _ = target.kill();
    let _ = target.wait();
    cleanup_test_base(&base);
}

fn live_handoff_import_failure_rolls_back_old_server_at(failure_point: &str) {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let client_socket = runtime_dir.join("herdr-client.sock");
    let marker = base.join("child.pid");
    let received_marker = base.join("received");
    let v1_harness = (failure_point == "v1-only").then(|| {
        use std::os::unix::fs::PermissionsExt;

        fs::create_dir_all(&base).unwrap();
        let path = base.join("v1-handoff-receiver.py");
        fs::write(
            &path,
            r#"#!/usr/bin/env python3
import json
import socket
import sys

connection = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
connection.connect(sys.argv[3])
connection.sendall((sys.argv[4] + "\n").encode())
line = b""
while not line.endswith(b"\n"):
    chunk = connection.recv(4096)
    if not chunk:
        raise RuntimeError("source closed before manifest")
    line += chunk
manifest = json.loads(line)
if manifest.get("version") != 3:
    raise RuntimeError(f"expected handoff v3, got {manifest.get('version')}")
connection.sendall(b"unsupported handoff version 3\n")
"#,
        )
        .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    });

    let spawned = if v1_harness.is_some() {
        spawn_server(&config_home, &runtime_dir, &api_socket)
    } else {
        spawn_server_with_env(
            &config_home,
            &runtime_dir,
            &api_socket,
            &[("VQRO_TEST_HANDOFF_IMPORT_FAIL", failure_point)],
        )
    };
    wait_for_socket(&api_socket, Duration::from_secs(10));
    register_runtime_dir(&runtime_dir);

    let created = request(
        &api_socket,
        serde_json::json!({
            "id": "test:workspace:create",
            "method": "workspace.create",
            "params": {"cwd": "/tmp", "focus": true}
        }),
    );
    let pane_id = created["result"]["root_pane"]["pane_id"]
        .as_str()
        .unwrap()
        .to_string();
    let command = format!(
        "sh -c 'echo READY $$ > {}; while read line; do echo got:$line; echo got:$line >> {}; done'",
        marker.display(),
        received_marker.display()
    );
    assert_ok(request(
        &api_socket,
        serde_json::json!({
            "id": "test:pane:run",
            "method": "pane.send_input",
            "params": {"pane_id": pane_id, "text": command, "keys": ["Enter"]}
        }),
    ));
    support::wait_for_file(&marker, Duration::from_secs(5));
    let pid_text = fs::read_to_string(&marker).unwrap();
    let child_pid: u32 = pid_text.split_whitespace().last().unwrap().parse().unwrap();

    let params = v1_harness.as_ref().map_or_else(
        || serde_json::json!({}),
        |path| serde_json::json!({"import_exe": path}),
    );
    let failed = request(
        &api_socket,
        serde_json::json!({"id":"test:handoff-fail","method":"server.live_handoff","params":params}),
    );
    assert!(
        failed.get("error").is_some(),
        "{failure_point} handoff should fail: {failed}"
    );
    wait_for_api(&api_socket, Duration::from_secs(10));
    wait_for_socket(&client_socket, Duration::from_secs(5));
    assert_eq!(unsafe { libc::kill(child_pid as libc::pid_t, 0) }, 0);

    assert_ok(request(
        &api_socket,
        serde_json::json!({
            "id": "test:pane:send-after-import-failure",
            "method": "pane.send_input",
            "params": {"pane_id": pane_id, "text": failure_point, "keys": ["Enter"]}
        }),
    ));
    wait_for_file_contains(
        &received_marker,
        &format!("got:{failure_point}"),
        Duration::from_secs(5),
    );
    let _ = request(
        &api_socket,
        serde_json::json!({"id":"test:stop","method":"server.stop","params":{}}),
    );
    drop(spawned);
    cleanup_test_base(&base);
}

#[test]
fn live_handoff_v3_to_v1_rejection_rolls_back_source_and_preserves_pty() {
    live_handoff_import_failure_rolls_back_old_server_at("v1-only");
}

#[test]
fn legacy_v1_source_precommit_rollback_preserves_v4_and_source_pty() {
    run_legacy_v1_source_harness(false);
}

#[test]
fn legacy_v1_source_commit_publishes_v6_before_target_exposure() {
    run_legacy_v1_source_harness(true);
}

#[test]
fn postcommit_publication_failure_fails_closed_without_source_resume() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let marker = base.join("child.pid");
    let spawned = spawn_server_with_env(
        &config_home,
        &runtime_dir,
        &api_socket,
        &[("VQRO_TEST_HANDOFF_IMPORT_FAIL", "publish_after_commit")],
    );
    wait_for_socket(&api_socket, Duration::from_secs(10));
    register_runtime_dir(&runtime_dir);

    let created = request(
        &api_socket,
        serde_json::json!({
            "id": "test:workspace:create",
            "method": "workspace.create",
            "params": {"cwd": "/tmp", "focus": true}
        }),
    );
    let pane_id = created["result"]["root_pane"]["pane_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ok(request(
        &api_socket,
        serde_json::json!({
            "id": "test:pane:run",
            "method": "pane.send_input",
            "params": {
                "pane_id": pane_id,
                "text": format!("sh -c 'echo $$ > {}; sleep 30'", marker.display()),
                "keys": ["Enter"]
            }
        }),
    ));
    support::wait_for_file(&marker, Duration::from_secs(5));
    let child_pid: u32 = fs::read_to_string(&marker).unwrap().trim().parse().unwrap();
    let session_path = config_home.join("vqro-dev/session.json");
    let legacy = b"{\"version\":4,\"workspaces\":[],\"repositories\":[],\"space_order\":[],\"active\":null,\"selected\":0}\n";
    fs::write(&session_path, legacy).unwrap();

    let failed = request(
        &api_socket,
        serde_json::json!({"id":"test:handoff","method":"server.live_handoff","params":{}}),
    );
    assert!(
        failed.get("error").is_some(),
        "handoff should fail closed: {failed}"
    );
    assert!(support::wait_until(
        Duration::from_secs(5),
        Duration::from_millis(25),
        || UnixStream::connect(&api_socket).is_err()
    ));
    assert!(support::wait_until(
        Duration::from_secs(5),
        Duration::from_millis(25),
        || unsafe { libc::kill(child_pid as libc::pid_t, 0) } != 0
    ));
    assert_eq!(fs::read(&session_path).unwrap(), legacy);

    drop(spawned);
    cleanup_test_base(&base);
}

#[test]
fn live_handoff_after_restored_failure_rolls_back_old_server() {
    live_handoff_import_failure_rolls_back_old_server_at("after_restored");
}

fn tree_has_view(node: &serde_json::Value, pane_id: &str, extension: &str, view: &str) -> bool {
    match node {
        serde_json::Value::Object(object) => {
            if object.get("type").and_then(|kind| kind.as_str()) == Some("view")
                && object.get("pane_id").and_then(|id| id.as_str()) == Some(pane_id)
                && object.get("extension").and_then(|value| value.as_str()) == Some(extension)
                && object.get("view").and_then(|value| value.as_str()) == Some(view)
            {
                return true;
            }
            object
                .values()
                .any(|child| tree_has_view(child, pane_id, extension, view))
        }
        serde_json::Value::Array(items) => items
            .iter()
            .any(|item| tree_has_view(item, pane_id, extension, view)),
        _ => false,
    }
}

#[test]
fn live_handoff_preserves_a_view_pane_without_a_runtime() {
    // A view pane has no PTY. The live transfer must keep it as a view (not treat
    // it as a terminal and not drop it) and keep its public identity and scope.
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");

    let spawned = spawn_server(&config_home, &runtime_dir, &api_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));
    register_runtime_dir(&runtime_dir);
    let server_pid = spawned.child.process_id().expect("test server pid");

    let created = request(
        &api_socket,
        serde_json::json!({"id":"test:create","method":"workspace.create","params":{"cwd":"/tmp","focus":true}}),
    );
    let root_pane = created["result"]["root_pane"]["pane_id"]
        .as_str()
        .unwrap()
        .to_string();

    let added = request(
        &api_socket,
        serde_json::json!({"id":"test:view","method":"pane.view","params":{"pane_id":root_pane,"action":"add","extension":"vqro.tasks","view":"tasks","focus":false}}),
    );
    assert_ok(added.clone());
    let view_pane = added["result"]["view"]["pane_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ne!(view_pane, root_pane);

    // Capture the terminal identity before the transfer, so a replacement
    // identity cannot pass the post-transfer check.
    let terminal_before = request(
        &api_socket,
        serde_json::json!({"id":"test:term-before","method":"pane.get","params":{"pane_id":root_pane}}),
    );
    let terminal_id_before = terminal_before["result"]["pane"]["terminal_id"]
        .as_str()
        .expect("terminal id before transfer")
        .to_string();

    assert_ok(request(
        &api_socket,
        serde_json::json!({"id":"test:handoff","method":"server.live_handoff","params":{}}),
    ));
    let _replacement_pid =
        wait_for_replacement_server_pid(&runtime_dir, server_pid, Duration::from_secs(30));
    wait_for_api(&api_socket, Duration::from_secs(30));

    // The view still reports as a view with no terminal; the terminal keeps its
    // own identity.
    let view = request(
        &api_socket,
        serde_json::json!({"id":"test:view-get","method":"pane.get","params":{"pane_id":view_pane}}),
    );
    assert_eq!(
        view["result"]["pane"]["content"].as_str(),
        Some("view"),
        "view: {view}"
    );
    assert_eq!(
        view["result"]["pane"]["pane_id"].as_str(),
        Some(view_pane.as_str()),
        "the view's public pane identity is preserved: {view}"
    );
    assert!(
        view["result"]["pane"]["terminal_id"].is_null(),
        "a view has no terminal: {view}"
    );
    let terminal = request(
        &api_socket,
        serde_json::json!({"id":"test:term-get","method":"pane.get","params":{"pane_id":root_pane}}),
    );
    // A terminal omits `content` (the default), keeping its info byte-identical.
    assert!(
        terminal["result"]["pane"]["content"].is_null(),
        "a terminal omits the content kind: {terminal}"
    );
    assert_eq!(
        terminal["result"]["pane"]["terminal_id"].as_str(),
        Some(terminal_id_before.as_str()),
        "terminal identity is preserved through the live transfer: {terminal}"
    );
    assert_eq!(
        terminal["result"]["pane"]["pane_id"].as_str(),
        Some(root_pane.as_str()),
        "the public pane identity is preserved"
    );

    // The exported tree still holds the view leaf with the same pane id and
    // (extension, view) pair.
    let exported = request(
        &api_socket,
        serde_json::json!({"id":"test:tree","method":"layout.tree.export","params":{"tab_id":null,"pane_id":null}}),
    );
    assert!(
        tree_has_view(
            &exported["result"]["layout"]["root"],
            &view_pane,
            "vqro.tasks",
            "tasks"
        ),
        "the view leaf survives the live transfer with its identity: {exported}"
    );

    let _ = request(
        &api_socket,
        serde_json::json!({"id":"test:stop","method":"server.stop","params":{}}),
    );
    drop(spawned);
    cleanup_test_base(&base);
}
