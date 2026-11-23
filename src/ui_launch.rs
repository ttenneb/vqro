//! `vqro ui [<client>]`: choose a UI client (D29). The built-in terminal client (`tui`, also `ratatui` or
//! `rescue`) is always available and is the default, so recovery never depends on an optional client. Other
//! clients are described by a client manifest, `<extensions dir>/clients/<name>.json`:
//! `{name, title?, summary?, kind: "client", argv: [program, args...], profiles?: [..], features?: [..]}`.
//! A relative program path in `argv` is resolved against that `clients` directory; the client runs in the
//! foreground with `VQRO_SOCKET` set and its own terminal. It is a client process, not a supervised extension.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

/// Names that select the built-in terminal client.
pub const BUILTIN: &[&str] = &["tui", "ratatui", "rescue"];

fn valid(name: &str) -> bool { !name.is_empty() && name.len() <= 64 && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') }

/// The manifest of a named client, checked: matching name, `kind: "client"`, a non-empty string argv.
pub fn find(extensions_dir: &Path, name: &str) -> Result<Value> {
    if !valid(name) { bail!("invalid client name {name:?}"); }
    let path = extensions_dir.join("clients").join(format!("{name}.json"));
    let text = std::fs::read_to_string(&path).with_context(|| format!("no client {name:?} ({} not found); `vqro ui --list` shows the clients, `vqro ui` runs the built-in one", path.display()))?;
    if text.len() > 32 * 1024 { bail!("client manifest {} exceeds 32 KiB", path.display()); }
    let manifest: Value = serde_json::from_str(&text).with_context(|| format!("invalid client manifest {}", path.display()))?;
    if manifest["name"] != name || manifest["kind"] != "client" { bail!("{} must have name {name:?} and kind \"client\"", path.display()); }
    let argv_ok = manifest["argv"].as_array().is_some_and(|a| !a.is_empty() && a.iter().all(Value::is_string));
    if !argv_ok { bail!("{} needs a non-empty string argv", path.display()); }
    Ok(manifest)
}

/// The built-in client, then every valid client manifest, as `{name, title, summary, builtin}`.
pub fn list(extensions_dir: &Path) -> Vec<Value> {
    let mut out = vec![json!({"name": "tui", "title": "Terminal UI (built in)", "summary": "The default client; always available for recovery.", "builtin": true})];
    let mut names: Vec<String> = std::fs::read_dir(extensions_dir.join("clients")).into_iter().flatten().flatten()
        .filter_map(|e| e.file_name().to_str()?.strip_suffix(".json").map(String::from)).collect();
    names.sort();
    for name in names {
        if let Ok(m) = find(extensions_dir, &name) {
            out.push(json!({"name": name, "title": m["title"], "summary": m["summary"], "builtin": false}));
        }
    }
    out
}

/// The command that runs a client from its manifest, with extra arguments appended and the core endpoint set.
pub fn command(extensions_dir: &Path, manifest: &Value, socket: &Path, extra: &[String]) -> Result<std::process::Command> {
    command_with_python(extensions_dir, manifest, socket, extra, crate::program::python)
}

fn command_with_python(
    extensions_dir: &Path, manifest: &Value, socket: &Path, extra: &[String],
    python: impl FnOnce() -> std::result::Result<(PathBuf, Vec<String>), String>,
) -> Result<std::process::Command> {
    let dir = extensions_dir.join("clients");
    let argv: Vec<&str> = manifest["argv"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
    let resolve = |arg: &str| -> PathBuf { let p = PathBuf::from(arg); if p.is_relative() && dir.join(&p).is_file() { dir.join(p) } else { p } };
    // Keep discovery's actionable error; never retry through a different implicit executable search.
    let (program, leading) = match argv[0] { "python3" => python().map_err(anyhow::Error::msg)?, other => (resolve(other), Vec::new()) };
    let mut cmd = std::process::Command::new(program);
    cmd.args(leading).args(argv[1..].iter().map(|a| resolve(a))).args(extra).current_dir(&dir).env("VQRO_SOCKET", socket);
    Ok(cmd)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn python_discovery_failure_is_returned_without_a_fallback_command() {
        let manifest = json!({"argv": ["python3", "text.py"]});
        let error = command_with_python(Path::new("ext"), &manifest, Path::new("endpoint"), &[],
            || Err("no Python 3 found; set VQRO_PYTHON".into())).unwrap_err();
        assert_eq!(error.to_string(), "no Python 3 found; set VQRO_PYTHON");
    }

    #[test]
    fn python_discovery_keeps_program_and_leading_args_literal() {
        let manifest = json!({"argv": ["python3", "text.py"]});
        let cmd = command_with_python(Path::new("ext"), &manifest, Path::new("endpoint"), &[],
            || Ok(("C:/Program Files/Python/py.exe".into(), vec!["-3".into()]))).unwrap();
        assert_eq!(cmd.get_program(), "C:/Program Files/Python/py.exe");
        assert_eq!(cmd.get_args().next().unwrap(), "-3");
    }

    #[test]
    fn other_clients_do_not_discover_python() {
        let manifest = json!({"argv": ["other-client"]});
        let cmd = command_with_python(Path::new("ext"), &manifest, Path::new("endpoint"), &[],
            || panic!("non-Python client must not discover Python")).unwrap();
        assert_eq!(cmd.get_program(), "other-client");
    }

    #[test]
    fn manifests_are_checked_listed_and_resolved_against_the_clients_directory() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("clients")).unwrap();
        std::fs::write(root.path().join("clients/text.json"), r#"{"name":"text","kind":"client","title":"Text","argv":["python3","text.py"]}"#).unwrap();
        std::fs::write(root.path().join("clients/text.py"), "").unwrap();
        std::fs::write(root.path().join("clients/bad.json"), r#"{"name":"other","kind":"client","argv":["x"]}"#).unwrap();
        std::fs::write(root.path().join("clients/noargv.json"), r#"{"name":"noargv","kind":"client"}"#).unwrap();
        let m = find(root.path(), "text").unwrap();
        assert!(find(root.path(), "bad").is_err());
        assert!(find(root.path(), "noargv").is_err());
        assert!(find(root.path(), "../text").is_err());
        assert!(format!("{:#}", find(root.path(), "missing").unwrap_err()).contains("vqro ui --list"));
        let names: Vec<Value> = list(root.path()).iter().map(|c| c["name"].clone()).collect();
        assert_eq!(names, vec![json!("tui"), json!("text")]);
        let cmd = command_with_python(root.path(), &m, Path::new("/run/p.sock"), &["--once".into()],
            || Ok(("python3".into(), Vec::new()))).unwrap();
        assert_eq!(cmd.get_program(), "python3");
        let args: Vec<_> = cmd.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        assert_eq!(args, vec![root.path().join("clients").join("text.py").to_string_lossy().into_owned(), "--once".into()]);
        assert!(cmd.get_envs().any(|(k, v)| k == "VQRO_SOCKET" && v == Some(std::ffi::OsStr::new("/run/p.sock"))));
    }
}
