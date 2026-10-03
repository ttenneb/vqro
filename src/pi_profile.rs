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
//! Only the human's `settings.json` is read; nothing under `~/.pi/agent` is ever
//! written. The profile dir itself is Vqro-managed. Set [`crate::bundled::OPT_OUT_ENV`]
//! to disable the profile along with the bundled defaults.

use std::io;
use std::path::{Path, PathBuf};

use serde_json::Value;

/// Packages a Vqro-launched Pi must not load: Herdr-coupled or Vqro-competing.
const EXCLUDED_PACKAGES: &[&str] = &["pi-input-gate", "pi-tasking-herdr-adapter", "rpiv-todo"];
/// Entries of the human's agent dir that are never mirrored into the profile.
const UNMIRRORED: &[&str] = &["settings.json", "sessions", "tmp"];
/// Package spec prefixes, which carry no local path.
const SPEC_PREFIXES: &[&str] = &["npm:", "git:", "http:", "https:"];

/// One warning line shown in a pane when a Vqro-launched Pi cannot be isolated.
pub(crate) const UNAVAILABLE_WARNING: &str =
    "vqro: Vqro Pi profile unavailable; Herdr extensions may load";

fn opted_out() -> bool {
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

/// True when Pi Vqro starts should use the profile.
pub(crate) fn profile_ready() -> bool {
    !opted_out() && profile_dir().join("settings.json").is_file()
}

/// True when the profile is needed (the human has Pi settings) but could not be
/// built, so a Vqro-launched Pi must be warned.
pub(crate) fn warning_needed() -> bool {
    if !cfg!(unix) || opted_out() || profile_ready() {
        return false;
    }
    human_agent_dir().is_some_and(|human| human.join("settings.json").is_file())
}

/// Build or refresh the profile. A no-op when the human has no Pi settings (then
/// there is nothing to exclude) or when the defaults are opted out.
pub(crate) fn ensure_profile() -> io::Result<()> {
    if opted_out() || !cfg!(unix) {
        return Ok(());
    }
    let Some(human) = human_agent_dir() else {
        return Ok(());
    };
    let Ok(text) = std::fs::read_to_string(human.join("settings.json")) else {
        return Ok(());
    };
    let mut settings: Value = serde_json::from_str(&text)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    derive_packages(&mut settings, &human);
    let profile = profile_dir();
    std::fs::create_dir_all(&profile)?;
    write_if_changed(
        &profile.join("settings.json"),
        &serde_json::to_string_pretty(&settings)?,
    )?;
    mirror_entries(&human, &profile)
}

/// Drop the excluded packages and absolutize relative local sources.
///
/// Relative sources resolve against the agent dir they were written in, so they
/// must be rewritten for a different profile directory.
fn derive_packages(settings: &mut Value, human: &Path) {
    let Some(packages) = settings.get_mut("packages").and_then(Value::as_array_mut) else {
        return;
    };
    packages.retain(|entry| !is_excluded(entry));
    for entry in packages.iter_mut() {
        let Some(source) = package_source_mut(entry) else {
            continue;
        };
        if !is_spec(source) && Path::new(source.as_str()).is_relative() {
            let resolved = human.join(source.as_str()).to_string_lossy().into_owned();
            *source = resolved;
        }
    }
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

/// The package's own name, ignoring a spec prefix, scope and version suffix.
fn package_name(source: &str) -> &str {
    let without_spec = SPEC_PREFIXES
        .iter()
        .find_map(|prefix| source.strip_prefix(prefix))
        .unwrap_or(source);
    let tail = without_spec
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(without_spec);
    match tail.rsplit_once('@') {
        Some((name, version)) if !name.is_empty() && !version.is_empty() => name,
        _ => tail,
    }
}

fn is_spec(source: &str) -> bool {
    SPEC_PREFIXES
        .iter()
        .any(|prefix| source.starts_with(prefix))
}

fn write_if_changed(path: &Path, body: &str) -> io::Result<()> {
    if std::fs::read(path).ok().as_deref() == Some(body.as_bytes()) {
        return Ok(());
    }
    std::fs::write(path, body)?;
    tracing::info!(path = %path.display(), "wrote the Vqro Pi profile settings");
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
            "/opt/packages/pi-tasking-herdr-adapter",
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
    fn package_name_handles_specs_scopes_and_versions() {
        assert_eq!(package_name("/opt/packages/pi-input-gate"), "pi-input-gate");
        assert_eq!(
            package_name("npm:avtc-pi-ask-user-question@1.0.5"),
            "avtc-pi-ask-user-question"
        );
        assert_eq!(package_name("npm:@blazer2k/searxng-suite"), "searxng-suite");
        assert_eq!(
            package_name("/x/pi-tasking-herdr-adapter"),
            "pi-tasking-herdr-adapter"
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
