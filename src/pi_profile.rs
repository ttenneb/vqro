//! A Vqro-only Pi profile.
//!
//! Pi reads its entire configuration from one directory, selected by
//! `PI_CODING_AGENT_DIR`. A plain Vqro launch builds a shadow of the human's
//! `~/.pi/agent` there: a `settings.json` derived from the human's with the
//! Herdr-coupled packages removed, and a symlink to every other entry (auth,
//! models, packages, resources). A Pi Vqro starts therefore keeps the human's
//! normal auth, models and settings without loading `pi-input-gate`,
//! `pi-tasking-herdr-adapter` or `rpiv-todo`.
//!
//! Only the human's `settings.json` is read; Vqro never writes the human's
//! settings or packages, and never writes through a symlink. The profile
//! references `auth.json` and `models-store.json` by symlink, so Pi's own runtime
//! token/catalog refresh may still update those human files. This is a profile,
//! not a sandbox: a deliberate fallback or opt-out loads the human's normal Pi
//! configuration. Set [`crate::bundled::OPT_OUT_ENV`] to disable it.

use std::io;
use std::path::{Path, PathBuf};

use serde_json::Value;
use sha2::{Digest, Sha256};

/// Packages a Vqro-launched Pi must not load: Herdr-coupled or Vqro-competing.
const EXCLUDED_PACKAGES: &[&str] = &["pi-input-gate", "pi-tasking-herdr-adapter", "rpiv-todo"];
/// Entries of the human's agent dir that are never mirrored into the profile.
const UNMIRRORED: &[&str] = &["settings.json", ".ready", "sessions", "tmp"];
/// Package spec prefixes, which carry no local path.
const SPEC_PREFIXES: &[&str] = &["npm:", "git:", "ssh:", "file:", "http:", "https:"];
/// Settings keys whose string-array values are paths or patterns resolved
/// against the agent dir. `sessionDir`/`shellPath` are deliberately excluded:
/// Pi resolves those against the process CWD.
const PATH_LIST_KEYS: &[&str] = &["extensions", "skills", "prompts", "themes"];
/// Leading resource-filter operators Pi recognizes only at string start.
const FILTER_OPERATORS: &[char] = &['!', '+', '-'];
/// Holds the content address of the settings a completed preparation used, so
/// presence alone never selects a rejected, partial or stale profile.
const READY_MARKER: &str = ".ready";

/// One warning line shown in a pane when a Vqro-launched Pi cannot be isolated.
pub(crate) const UNAVAILABLE_WARNING: &str =
    "vqro: Vqro Pi profile unavailable; Herdr extensions may load";

pub(crate) fn opted_out() -> bool {
    std::env::var(crate::bundled::OPT_OUT_ENV).is_ok_and(|value| !value.is_empty() && value != "0")
}

/// The Vqro-managed Pi config directory.
pub(crate) fn profile_dir() -> PathBuf {
    crate::config::config_dir().join("pi-agent")
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

fn human_agent_dir() -> Option<PathBuf> {
    home_dir().map(|home| home.join(".pi").join("agent"))
}

/// The content address of the human's settings, or `None` when it is unreadable.
fn source_stamp(human: &Path) -> Option<String> {
    let bytes = std::fs::read(human.join("settings.json")).ok()?;
    Some(
        Sha256::digest(&bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect(),
    )
}

/// True when Pi Vqro starts should use the profile: a real (not symlinked)
/// profile directory whose marker matches the human's current settings.
pub(crate) fn profile_ready() -> bool {
    if opted_out() || !cfg!(unix) {
        return false;
    }
    let profile = profile_dir();
    if std::fs::symlink_metadata(&profile).is_ok_and(|meta| meta.file_type().is_symlink()) {
        return false;
    }
    let Ok(marker) = std::fs::read_to_string(profile.join(READY_MARKER)) else {
        return false;
    };
    let Some(stamp) = human_agent_dir().and_then(|human| source_stamp(&human)) else {
        return false;
    };
    marker.trim() == stamp
}

/// True when a profile is wanted (the human has Pi settings) but is not ready, so
/// a Vqro-launched Pi must be warned.
pub(crate) fn warning_needed() -> bool {
    if !cfg!(unix) || opted_out() || profile_ready() {
        return false;
    }
    human_agent_dir().is_some_and(|human| human.join("settings.json").is_file())
}

/// Build or refresh the profile.
///
/// A no-op when the human has no Pi settings (then there is nothing to exclude)
/// or when the defaults are opted out. The ready marker records the content
/// address of the settings used, so a rejected, partial or stale preparation is
/// never selected even when the marker cannot be rewritten.
pub(crate) fn ensure_profile() -> io::Result<()> {
    if opted_out() || !cfg!(unix) {
        return Ok(());
    }
    let profile = profile_dir();
    // Never follow a symlinked profile directory into a human file.
    crate::bundled::safe_under(&profile, &profile)?;
    let Some(human) = human_agent_dir() else {
        return Ok(());
    };
    let Ok(text) = std::fs::read_to_string(human.join("settings.json")) else {
        return Ok(());
    };
    let mut settings: Value = serde_json::from_str(&text)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    derive_packages(&mut settings, &human);
    derive_paths(&mut settings, &human);
    std::fs::create_dir_all(&profile)?;
    let settings_path = profile.join("settings.json");
    crate::bundled::safe_under(&profile, &settings_path)?;
    write_if_changed(&settings_path, &serde_json::to_string_pretty(&settings)?)?;
    mirror_entries(&human, &profile)?;
    let marker = profile.join(READY_MARKER);
    crate::bundled::safe_under(&profile, &marker)?;
    let stamp = source_stamp(&human)
        .ok_or_else(|| io::Error::other("human Pi settings disappeared during preparation"))?;
    write_if_changed(&marker, &format!("{stamp}\n"))?;
    Ok(())
}

/// Drop the excluded packages and absolutize relative local sources.
///
/// Relative sources resolve against the agent dir they were written in, so they
/// must be rewritten for a different profile directory. Specs and `~` paths keep
/// their meaning and are left alone.
fn derive_packages(settings: &mut Value, human: &Path) {
    let Some(packages) = settings.get_mut("packages").and_then(Value::as_array_mut) else {
        return;
    };
    packages.retain(|entry| !is_excluded(entry));
    for entry in packages.iter_mut() {
        let Some(source) = package_source_mut(entry) else {
            continue;
        };
        if let Some(resolved) = absolutize(human, source) {
            *source = resolved;
        }
    }
}

/// Rewrite relative path settings Pi resolves against the agent dir, keeping any
/// leading `!`/`+`/`-` filter operator in place. `sessionDir`/`shellPath` are not
/// touched: Pi resolves those against the process CWD, not the agent dir.
fn derive_paths(settings: &mut Value, human: &Path) {
    let Some(object) = settings.as_object_mut() else {
        return;
    };
    for key in PATH_LIST_KEYS {
        if let Some(Value::Array(items)) = object.get_mut(*key) {
            for item in items.iter_mut() {
                if let Value::String(value) = item {
                    if let Some(resolved) = absolutize_filter(human, value) {
                        *value = resolved;
                    }
                }
            }
        }
    }
}

/// Absolutize a resource path or pattern, preserving a leading filter operator.
fn absolutize_filter(human: &Path, entry: &str) -> Option<String> {
    let rest = entry.strip_prefix(FILTER_OPERATORS).unwrap_or(entry);
    let operator = &entry[..entry.len() - rest.len()];
    Some(format!("{operator}{}", absolutize(human, rest)?))
}

/// The absolute form of a relative local path, or `None` when it is not one.
fn absolutize(human: &Path, source: &str) -> Option<String> {
    if source.is_empty()
        || source.starts_with('~')
        || is_spec(source)
        || Path::new(source).is_absolute()
    {
        return None;
    }
    Some(human.join(source).to_string_lossy().into_owned())
}

fn is_excluded(entry: &Value) -> bool {
    package_source(entry).is_some_and(|source| EXCLUDED_PACKAGES.contains(&package_name(source)))
}

fn package_source(entry: &Value) -> Option<&str> {
    match entry {
        Value::String(source) => Some(source.as_str()),
        Value::Object(object) => object.get("source").and_then(Value::as_str),
        _ => None,
    }
}

fn package_source_mut(entry: &mut Value) -> Option<&mut String> {
    match entry {
        Value::String(source) => Some(source),
        Value::Object(object) => match object.get_mut("source")? {
            Value::String(source) => Some(source),
            _ => None,
        },
        _ => None,
    }
}

/// The package's own name, ignoring a spec prefix, scope, `@ref` and `.git`.
fn package_name(source: &str) -> &str {
    let without_spec = SPEC_PREFIXES
        .iter()
        .find_map(|prefix| source.strip_prefix(prefix))
        .unwrap_or(source);
    let tail = without_spec
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(without_spec);
    // Split the pin/ref first: a `.git` suffix precedes an `@ref`.
    let name = match tail.rsplit_once('@') {
        Some((name, reference)) if !name.is_empty() && !reference.is_empty() => name,
        _ => tail,
    };
    name.strip_suffix(".git").unwrap_or(name)
}

/// Any URL scheme or known spec prefix; such a source is not a local path.
fn is_spec(source: &str) -> bool {
    source.contains("://")
        || SPEC_PREFIXES
            .iter()
            .any(|prefix| source.starts_with(prefix))
}

fn write_if_changed(path: &Path, body: &str) -> io::Result<()> {
    if std::fs::read(path).ok().as_deref() == Some(body.as_bytes()) {
        return Ok(());
    }
    std::fs::write(path, body)?;
    tracing::info!(path = %path.display(), "prepared a Vqro Pi profile file");
    Ok(())
}

#[cfg(unix)]
fn mirror_entries(human: &Path, profile: &Path) -> io::Result<()> {
    for entry in std::fs::read_dir(human)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if UNMIRRORED.contains(&name.as_ref()) || name.ends_with(".log") {
            continue;
        }
        let target = profile.join(name.as_ref());
        if std::fs::symlink_metadata(&target).is_ok() {
            continue;
        }
        std::os::unix::fs::symlink(entry.path(), &target)?;
        tracing::info!(path = %target.display(), "linked a Pi profile entry");
    }
    Ok(())
}

#[cfg(not(unix))]
fn mirror_entries(_human: &Path, _profile: &Path) -> io::Result<()> {
    // Unreachable: ensure_profile returns before this off unix.
    Err(io::Error::other(
        "the Vqro Pi profile requires symlink support",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(sources: &[&str]) -> Value {
        serde_json::json!({ "theme": "dracula", "packages": sources })
    }

    #[test]
    fn derive_drops_excluded_packages_and_keeps_the_rest() {
        let mut value = settings(&[
            "npm:@blazer2k/searxng-suite",
            "/opt/packages/pi-input-gate",
            "git:https://example.com/o/pi-tasking-herdr-adapter.git",
            "../../Projects/pi-budget",
            "/opt/packages/rpiv-todo",
        ]);
        derive_packages(&mut value, Path::new("/home/h/.pi/agent"));
        let kept: Vec<&str> = value["packages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry.as_str().unwrap())
            .collect();
        assert_eq!(
            kept,
            [
                "npm:@blazer2k/searxng-suite",
                "/home/h/.pi/agent/../../Projects/pi-budget"
            ]
        );
        assert_eq!(value["theme"], "dracula");
    }

    #[test]
    fn specs_and_tilde_sources_are_left_alone() {
        let mut value = settings(&[
            "ssh://git@example.com/o/kept.git",
            "~/Projects/pi-budget",
            "git://example.com/o/x",
        ]);
        derive_packages(&mut value, Path::new("/home/h/.pi/agent"));
        let kept: Vec<&str> = value["packages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry.as_str().unwrap())
            .collect();
        assert_eq!(
            kept,
            [
                "ssh://git@example.com/o/kept.git",
                "~/Projects/pi-budget",
                "git://example.com/o/x"
            ]
        );
    }

    #[test]
    fn package_name_handles_specs_scopes_versions_and_git() {
        assert_eq!(package_name("/opt/packages/pi-input-gate"), "pi-input-gate");
        assert_eq!(
            package_name("npm:avtc-pi-ask-user-question@1.0.5"),
            "avtc-pi-ask-user-question"
        );
        assert_eq!(package_name("npm:@blazer2k/searxng-suite"), "searxng-suite");
        assert_eq!(
            package_name("git:https://example.com/o/rpiv-todo.git"),
            "rpiv-todo"
        );
        assert_eq!(
            package_name("git:https://example.com/o/pi-input-gate.git@v1"),
            "pi-input-gate"
        );
    }

    #[test]
    fn relative_resource_paths_keep_their_filter_operator() {
        let mut value = serde_json::json!({
            "sessionDir": "./sessions",
            "shellPath": "./bin/bash",
            "extensions": [
                "extensions/local.ts",
                "!**/blocked.ts",
                "-old.ts",
                "+../../Projects/ok.ts",
                "~/local",
                "npm:x",
            ],
        });
        derive_paths(&mut value, Path::new("/home/h/.pi/agent"));
        // sessionDir/shellPath resolve against the CWD, so they are left alone.
        assert_eq!(value["sessionDir"], "./sessions");
        assert_eq!(value["shellPath"], "./bin/bash");
        assert_eq!(
            value["extensions"],
            serde_json::json!([
                "/home/h/.pi/agent/extensions/local.ts",
                "!/home/h/.pi/agent/**/blocked.ts",
                "-/home/h/.pi/agent/old.ts",
                "+/home/h/.pi/agent/../../Projects/ok.ts",
                "~/local",
                "npm:x"
            ])
        );
    }

    #[test]
    fn object_package_entries_are_filtered_and_absolutized() {
        let mut value = serde_json::json!({
            "packages": [
                { "source": "/opt/rpiv-todo", "extensions": [] },
                { "source": "../../Projects/pi-system-prompt" },
            ]
        });
        derive_packages(&mut value, Path::new("/home/h/.pi/agent"));
        let kept = value["packages"].as_array().unwrap();
        assert_eq!(kept.len(), 1);
        assert_eq!(
            kept[0]["source"],
            "/home/h/.pi/agent/../../Projects/pi-system-prompt"
        );
    }
}
