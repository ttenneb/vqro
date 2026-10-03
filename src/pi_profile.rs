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
//!
//! Known limits (documented, not handled): a package source written with `./`,
//! `../` or another non-prefix parent-path form that normalizes outside the agent
//! dir, a package path under an entry the profile does not mirror, and a read-only
//! profile directory whose content still matches its marker.

use std::io;
use std::path::{Path, PathBuf};

use serde_json::Value;
use sha2::{Digest, Sha256};

/// Packages a Vqro-launched Pi must not load: Herdr-coupled or Vqro-competing.
const EXCLUDED_PACKAGES: &[&str] = &["pi-input-gate", "pi-tasking-herdr-adapter", "rpiv-todo"];
/// Human agent-dir directories mirrored entry by entry, with the names to drop,
/// so an excluded entry is absent and a new human entry still appears.
const EXCLUDED_SKILLS: &[&str] = &[
    "agent-messaging",
    "herdr",
    "herdr-pi-test-root",
    "herdr-pi-upgrade-rollback",
    "launch-owner",
    "check-session-loaded",
    "scratch-cleanup",
];
const EXCLUDED_EXTENSIONS: &[&str] = &["herdr-agent-state.ts"];
const PER_ENTRY_DIRS: &[(&str, &[&str])] = &[
    ("skills", EXCLUDED_SKILLS),
    ("extensions", EXCLUDED_EXTENSIONS),
];
/// Entries of the human's agent dir that are never mirrored into the profile.
/// `SYSTEM.md` is replaced by [`VQRO_SYSTEM_MD`] rather than linked.
const UNMIRRORED: &[&str] = &["settings.json", ".ready", "sessions", "tmp", "SYSTEM.md"];
/// The Vqro system prompt written into the profile.
const VQRO_SYSTEM_MD: &str = include_str!("../extensions/pi-profile/SYSTEM.md");
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
    Some(digest_hex(&bytes))
}

fn digest_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// The content address of the profile's own settings, or `None` when that path is
/// not a regular file (for example a symlink to a human file).
fn profile_settings_hash(profile: &Path) -> Option<String> {
    let path = profile.join("settings.json");
    if !std::fs::symlink_metadata(&path).ok()?.file_type().is_file() {
        return None;
    }
    Some(digest_hex(&std::fs::read(&path).ok()?))
}

/// True when Pi Vqro starts should use the profile: a real (not symlinked)
/// profile directory whose settings are the exact bytes this profile prepared
/// from the human's current settings.
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
    let Some((recorded_source, recorded_profile)) = marker.trim().split_once(':') else {
        return false;
    };
    let Some(source) = human_agent_dir().and_then(|human| source_stamp(&human)) else {
        return false;
    };
    let Some(profile_hash) = profile_settings_hash(&profile) else {
        return false;
    };
    source == recorded_source && profile_hash == recorded_profile
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
    // Drop the previous readiness first: a preparation that fails must not keep an
    // earlier profile selected. (A read-only directory cannot be cleared; its
    // marker then no longer matches below.)
    if std::fs::symlink_metadata(&profile).is_ok_and(|meta| meta.file_type().is_dir()) {
        let _ = std::fs::remove_file(profile.join(READY_MARKER));
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
    derive_paths(&mut settings, &human);
    std::fs::create_dir_all(&profile)?;
    let settings_path = profile.join("settings.json");
    crate::bundled::safe_under(&profile, &settings_path)?;
    let body = serde_json::to_string_pretty(&settings)?;
    write_if_changed(&settings_path, &body)?;
    mirror_entries(&human, &profile)?;
    let system = profile.join("SYSTEM.md");
    crate::bundled::safe_under(&profile, &system)?;
    write_if_changed(&system, VQRO_SYSTEM_MD)?;
    let marker = profile.join(READY_MARKER);
    crate::bundled::safe_under(&profile, &marker)?;
    // Address the exact bytes prepared from this source, not a later reread, so an
    // edit between the write and the marker leaves the profile unselected.
    write_if_changed(
        &marker,
        &format!(
            "{}:{}\n",
            digest_hex(text.as_bytes()),
            digest_hex(body.as_bytes())
        ),
    )?;
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
///
/// A path or pattern that stays inside the agent dir is left relative: the
/// profile mirrors the human layout with symlinks, so the same relative entry
/// resolves to the same file (and keeps matching its own glob). Only a path that
/// escapes the agent dir is rewritten, because it would resolve elsewhere.
fn absolutize(human: &Path, source: &str) -> Option<String> {
    if !escapes_agent_dir(source) {
        return None;
    }
    Some(human.join(source).to_string_lossy().into_owned())
}

fn escapes_agent_dir(source: &str) -> bool {
    if source.is_empty()
        || source.starts_with('~')
        || is_spec(source)
        || Path::new(source).is_absolute()
    {
        return false;
    }
    source == ".." || source.starts_with("../") || source.starts_with("..\\")
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

/// The package's own name, ignoring a spec prefix, scope, `@ref`, `.git` and any
/// empty path components (a directory source like `local//pkg/`).
fn package_name(source: &str) -> &str {
    let without_spec = SPEC_PREFIXES
        .iter()
        .find_map(|prefix| source.strip_prefix(prefix))
        .unwrap_or(source);
    let tail = without_spec
        .split(['/', '\\'])
        .rfind(|part| !part.is_empty())
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
        if let Some((_, excluded)) = PER_ENTRY_DIRS
            .iter()
            .find(|(directory, _)| *directory == name.as_ref())
        {
            mirror_directory(&entry.path(), &target, excluded)?;
        } else {
            link_or_skip(&entry.path(), &target)?;
        }
    }
    Ok(())
}

/// Mirror a human resource directory entry by entry, dropping excluded names.
///
/// A whole-directory symlink from an earlier profile is replaced by a real
/// directory, so an excluded entry is absent while a new one still appears.
#[cfg(unix)]
fn mirror_directory(source: &Path, target: &Path, excluded: &[&str]) -> io::Result<()> {
    if !source.is_dir() {
        return Ok(());
    }
    if std::fs::symlink_metadata(target).is_ok_and(|meta| meta.file_type().is_symlink()) {
        std::fs::remove_file(target)?;
    }
    std::fs::create_dir_all(target)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if excluded.contains(&name.as_ref()) {
            continue;
        }
        link_or_skip(&entry.path(), &target.join(name.as_ref()))?;
    }
    Ok(())
}

#[cfg(unix)]
fn link_or_skip(source: &Path, target: &Path) -> io::Result<()> {
    if std::fs::symlink_metadata(target).is_ok() {
        return Ok(());
    }
    std::os::unix::fs::symlink(source, target)?;
    tracing::info!(path = %target.display(), "linked a Pi profile entry");
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
        // A directory source with a trailing or duplicated separator still names
        // the package.
        assert_eq!(package_name("local/pi-input-gate/"), "pi-input-gate");
        assert_eq!(package_name("local//pi-input-gate//"), "pi-input-gate");
        assert_eq!(package_name("npm:@scope/rpiv-todo/"), "rpiv-todo");
    }

    #[test]
    fn relative_resource_paths_keep_their_filter_operator_and_base() {
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
        // Entries inside the agent dir keep their relative base (the profile
        // mirrors that layout), so operators and globs still match; only an
        // escaping path is rewritten.
        assert_eq!(
            value["extensions"],
            serde_json::json!([
                "extensions/local.ts",
                "!**/blocked.ts",
                "-old.ts",
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

    #[test]
    fn the_vqro_system_prompt_is_embedded() {
        assert!(VQRO_SYSTEM_MD.contains("Vqro pane"));
        assert!(VQRO_SYSTEM_MD.contains("Working with Vqro"));
        assert!(!VQRO_SYSTEM_MD.contains("Herdr"));
    }

    #[cfg(unix)]
    #[test]
    fn mirror_directory_drops_excluded_entries_and_keeps_others() {
        let root = std::env::temp_dir().join(format!("vqro-pi-mirror-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let human = root.join("human");
        let profile = root.join("profile");
        std::fs::create_dir_all(human.join("skills")).unwrap();
        for name in ["herdr", "keep-me"] {
            std::fs::write(human.join("skills").join(name), "x").unwrap();
        }
        mirror_directory(&human.join("skills"), &profile.join("skills"), &["herdr"]).unwrap();
        assert!(!profile.join("skills/herdr").exists());
        assert!(std::fs::symlink_metadata(profile.join("skills/keep-me"))
            .unwrap()
            .file_type()
            .is_symlink());
        let _ = std::fs::remove_dir_all(&root);
    }
}
