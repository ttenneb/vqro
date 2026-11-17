//! Backends as provider extensions: no in-process backend by default, default extensions installed
//! once per state with migrations for existing installs, a deterministic default provider, and the
//! core's endpoint in every spawned terminal's environment.

use std::path::Path;
use std::time::Duration;

use prim::client;
use prim::provider_client::{Config, Outcome, Provider, Request};
use prim::rpc::Peer;
use prim::server::{start, Backend};
use serde_json::{json, Value};

fn defaults(dir: &Path) {
    std::fs::write(dir.join("defaults.json"), json!({"version": 2, "extensions": [
        {"name": "tabs", "argv": ["python3", "tabs.py"]},
        {"name": "panes", "argv": ["python3", "panes.py"]},
        {"name": "herdr", "argv": ["prim-herdr", "--name", "herdr"], "kind": "provider", "default": true, "since": 2, "description": "herdr-core"},
        {"name": "tmux", "argv": ["prim-tmux", "--name", "tmux"], "kind": "provider", "enabled": false, "since": 2, "description": "private tmux"},
    ]}).to_string()).unwrap();
}

/// Defaults migration uses safe mode; manual-provider cases use normal mode with no declarations.
fn core(root: &Path, name: &str, defaults_dir: Option<&Path>) -> (prim::core::Shared, std::path::PathBuf) {
    let socket = root.join(format!("{name}.sock"));
    let core = start(socket.clone(), root.join("state"), defaults_dir.is_some(), Backend::Extensions, defaults_dir.map(Path::to_path_buf)).unwrap();
    (core, socket)
}

fn entries(core: &prim::core::Shared) -> Value {
    core.lock().unwrap().snapshot()["entries"].clone()
}

#[tokio::test]
async fn serve_without_fake_runs_no_in_process_backend() {
    let root = tempfile::Builder::new().prefix("pbx").tempdir_in("/tmp").unwrap();
    let (core, socket) = core(root.path(), "a", None);
    assert!(core.lock().unwrap().snapshot()["providers"].as_object().unwrap().is_empty());
    let (c, _, _) = client::connect(&socket, "t").await.unwrap();
    let err = c.call("resource.spawn", json!({"argv": ["sh"]})).await.unwrap_err();
    assert_eq!(err.kind(), "unavailable", "{err:?}");
}

#[tokio::test]
async fn first_run_installs_all_defaults_with_provider_fields() {
    let root = tempfile::Builder::new().prefix("pbx").tempdir_in("/tmp").unwrap();
    let d = tempfile::tempdir().unwrap();
    defaults(d.path());
    let (core, _) = core(root.path(), "a", Some(d.path()));
    let e = entries(&core);
    for name in ["tabs", "panes", "herdr", "tmux"] {
        assert!(e[format!("extension/{name}")].is_object(), "{name}");
    }
    let herdr = &e["extension/herdr"]["value"];
    assert_eq!((herdr["kind"].as_str(), herdr["default"].as_bool(), herdr["argv"][0].as_str()), (Some("provider"), Some(true), Some("prim-herdr")));
    assert_eq!(e["extension/tmux"]["value"]["enabled"], false);
    assert_eq!(e["extension/tmux"]["value"]["description"], "private tmux");
    assert_eq!(e["doc/prim/defaults"]["value"]["version"], 2);
}

#[tokio::test]
async fn existing_installs_gain_new_defaults_once_and_removals_stick() {
    let root = tempfile::Builder::new().prefix("pbx").tempdir_in("/tmp").unwrap();
    let d = tempfile::tempdir().unwrap();
    // A state from before versioned defaults: tabs installed, panes removed by the user.
    std::fs::write(d.path().join("defaults.json"), json!({"extensions": [
        {"name": "tabs", "argv": ["python3", "tabs.py"]}, {"name": "panes", "argv": ["python3", "panes.py"]}]}).to_string()).unwrap();
    {
        let (core, socket) = core(root.path(), "v1", Some(d.path()));
        let (c, _, _) = client::connect(&socket, "t").await.unwrap();
        // As written by cores before versioned defaults: no `version` in the record.
        c.call("workspace.apply", json!({"ops": [{"op": "delete", "key": "extension/panes"},
            {"op": "set", "key": "extension/tabs", "value": {"argv": ["python3", "my-tabs.py"]}},
            {"op": "set", "key": "doc/prim/defaults", "value": {"installed": ["tabs", "panes"], "from": d.path().to_string_lossy()}}]})).await.unwrap();
        assert!(entries(&core)["doc/prim/defaults"]["value"]["version"].is_null());
        prim::server::shutdown(&core).await;
    }
    defaults(d.path());
    let (core2, socket2) = core(root.path(), "v2", Some(d.path()));
    let e = entries(&core2);
    assert!(e["extension/herdr"].is_object() && e["extension/tmux"].is_object(), "new defaults added to an existing install");
    assert!(e["extension/panes"].is_null(), "an extension the user removed is not re-added");
    assert_eq!(e["extension/tabs"]["value"]["argv"][1], "my-tabs.py", "existing entries are left as they are");
    assert_eq!(e["doc/prim/defaults"]["value"]["version"], 2);
    let (c, _, _) = client::connect(&socket2, "t").await.unwrap();
    let txns = c.call("workspace.history", json!({"limit": 100})).await.unwrap()["txns"].as_array().unwrap().len();
    // Removing a migrated default sticks too, and a restart changes nothing (idempotent).
    c.call("workspace.apply", json!({"ops": [{"op": "delete", "key": "extension/tmux"}]})).await.unwrap();
    prim::server::shutdown(&core2).await;
    let (core3, socket3) = core(root.path(), "v3", Some(d.path()));
    assert!(entries(&core3)["extension/tmux"].is_null());
    let (c3, _, _) = client::connect(&socket3, "t").await.unwrap();
    let after = c3.call("workspace.history", json!({"limit": 100})).await.unwrap()["txns"].as_array().unwrap().len();
    assert_eq!(after, txns + 1, "only the user's delete; no new defaults transaction");
}

/// A wire provider declaring terminal.spawn (env as given) whose spawn requests are reported on `seen`.
async fn provider(socket: &Path, name: &str, role: &str, env_capability: Option<bool>, seen: tokio::sync::mpsc::UnboundedSender<(String, Value)>) -> Provider {
    let mut spawn = json!({});
    if let Some(env) = env_capability {
        spawn["env"] = json!(env);
    }
    let caps = json!({"terminal.spawn": spawn, "terminal.output": {"raw": true}});
    let mut config = Config::new(name, format!("{name}-instance"), caps.as_object().unwrap().clone());
    config.role = role.into();
    config.extension = Some(name.into());
    let (p, mut requests) = Provider::connect(tokio::net::UnixStream::connect(socket).await.unwrap(), config).await.unwrap();
    p.full_sync(vec![]).await.unwrap();
    let name = name.to_string();
    tokio::spawn(async move {
        let mut n = 0;
        while let Some(r) = requests.recv().await {
            if let Request::Effect(e) = r {
                n += 1;
                let _ = seen.send((name.clone(), e.args.clone()));
                e.respond(Ok(Outcome::Observed(Some(json!({"facts": {"ref": format!("t{n}"), "kind": "terminal", "state": "live"}})))));
            }
        }
    });
    p
}

async fn spawn(c: &Peer, params: Value) -> Value {
    tokio::time::timeout(Duration::from_secs(5), c.call("resource.spawn", params)).await.unwrap().unwrap()
}

#[tokio::test]
async fn spawns_go_to_the_extension_marked_default_whatever_registers_first() {
    let root = tempfile::Builder::new().prefix("pbx").tempdir_in("/tmp").unwrap();
    let (core, socket) = core(root.path(), "a", None);
    let (c, _, _) = client::connect(&socket, "t").await.unwrap();
    c.call("workspace.apply", json!({"ops": [{"op": "set", "key": "extension/herdr",
        "value": {"argv": ["prim-herdr"], "kind": "provider", "default": true, "enabled": false}}]})).await.unwrap();
    let (tx, mut seen) = tokio::sync::mpsc::unbounded_channel();
    // Registration order: another backend first, then the default one.
    let _tmux = provider(&socket, "tmux", "backend", None, tx.clone()).await;
    let _herdr = provider(&socket, "herdr", "backend", None, tx.clone()).await;
    spawn(&c, json!({"argv": ["sh"]})).await;
    assert_eq!(seen.recv().await.unwrap().0, "herdr", "the extension marked default wins, whatever registered first");
    drop(core);
}

#[tokio::test]
async fn first_backend_is_the_default_without_a_marked_one() {
    let root = tempfile::Builder::new().prefix("pbx").tempdir_in("/tmp").unwrap();
    let (_core, socket) = core(root.path(), "a", None);
    let (c, _, _) = client::connect(&socket, "t").await.unwrap();
    let (tx, mut seen) = tokio::sync::mpsc::unbounded_channel();
    let _tmux = provider(&socket, "tmux", "backend", None, tx.clone()).await;
    let _other = provider(&socket, "other", "backend", None, tx.clone()).await;
    spawn(&c, json!({"argv": ["sh"]})).await;
    assert_eq!(seen.recv().await.unwrap().0, "tmux");
}

#[tokio::test]
async fn every_spawn_gets_the_core_endpoint_but_not_a_bridge_token() {
    let root = tempfile::Builder::new().prefix("pbx").tempdir_in("/tmp").unwrap();
    let (_core, socket) = core(root.path(), "a", None);
    let (c, _, _) = client::connect(&socket, "t").await.unwrap();
    let (tx, mut seen) = tokio::sync::mpsc::unbounded_channel();
    let _p = provider(&socket, "herdr", "backend", None, tx.clone()).await;
    let _noenv = provider(&socket, "noenv", "backend", Some(false), tx.clone()).await;
    spawn(&c, json!({"provider": "herdr", "argv": ["sh"], "env": {"A": "b"}})).await;
    let (_, args) = seen.recv().await.unwrap();
    assert_eq!(args["env"]["PRIM_SOCKET"].as_str(), Some(socket.to_str().unwrap()));
    assert_eq!(args["env"]["A"], "b");
    assert!(args["env"].get("PRIM_BRIDGE_TOKEN").is_none(), "the bridge credential stays opt-in");
    spawn(&c, json!({"provider": "herdr", "argv": ["sh"], "env": {"PRIM_SOCKET": "/custom"}})).await;
    assert_eq!(seen.recv().await.unwrap().1["env"]["PRIM_SOCKET"], "/custom", "an explicit value is kept");
    // A provider that cannot pass an environment still spawns (without it).
    spawn(&c, json!({"provider": "noenv", "argv": ["sh"]})).await;
    let (_, args) = seen.recv().await.unwrap();
    assert!(args["env"].as_object().is_none_or(|e| e.is_empty()), "{args}");
}
