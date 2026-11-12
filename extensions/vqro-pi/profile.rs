//! A Vqro-only Pi profile.
//!
//! Pi reads its entire configuration from one directory, selected by
//! `PI_CODING_AGENT_DIR`. A plain Vqro launch builds a source-keyed shadow of
//! `extensions.pi_source_dir` (default `~/.pi/agent`): settings derived from it with the
//! Herdr-coupled packages removed, and a symlink to every other entry (auth,
//! models, packages, resources). A Pi Vqro starts therefore keeps the human's
//! normal auth, models and settings without loading `pi-input-gate`,
//! `pi-tasking-herdr-adapter`, `rpiv-todo` or `pi-system-prompt`.
//!
//! Only the human's `settings.json` is read; Vqro never writes the human's
//! settings or packages, and never writes through a symlink. The profile
//! references `auth.json` and `models-store.json` by symlink, so Pi's own runtime
//! token/catalog refresh may still update those human files. This is a profile,
//! not a sandbox. Both overrides absent retain the default warned fallback;
//! either explicit override makes an invalid/unreadable/unprepared selection fail
//! managed launch. Opt-out disables defaults, and cannot bypass an explicit choice.
//!
//! Known limits (documented, not handled): a package source written with `./`,
//! `../` or another non-prefix parent-path form that normalizes outside the agent
//! dir, a package path under an entry the profile does not mirror, and a read-only
//! profile directory whose content still matches its marker. Only the legacy
//! default treats any settings read error as empty. Explicit sources must exist
//! and reject settings read errors other than a missing settings file.
//!
//! Readiness means the profile settings match the human's current settings AND the
//! bundled adapter and vqro skill are linked and present; a preparation that
//! cannot publish them fails and leaves the profile unselected. Default warned
//! fallback clears inherited profile env and loads default Pi config without the
//! Vqro skill/adapter/Tasks consumer. Explicit managed choices never fall back.

use std::io;
use std::path::{Path, PathBuf};

use serde_json::Value;
use sha2::{Digest, Sha256};

/// Temporary compiled profile admission, not runtime package execution.
/// See docs/next/architecture/temporary-agent-launch-profile-admission.md.
pub(crate) const BUNDLE: crate::bundled::DefaultBundle = crate::bundled::DefaultBundle {
    prefix: "pi",
    assets: &[
        ("vqro-pi.ts", include_str!("index.ts")),
        super::tasks::PROFILE_RESOURCE,
        (
            "skills/vqro/SKILL.md",
            include_str!("../../skills/vqro/SKILL.md"),
        ),
    ],
    plugin: None,
};

#[cfg(unix)]
fn generation_dir() -> PathBuf {
    crate::bundled::generation_dir(
        &crate::bundled::bundled_root(),
        BUNDLE.prefix,
        BUNDLE.assets,
    )
}

/// One operation-local selection, shared by preparation/readiness/launch binding.
struct Selection {
    executable: String,
    source: Option<PathBuf>,
    explicit: bool,
}

impl Selection {
    fn resolve(config: &crate::config::Config) -> io::Result<Self> {
        let explicit =
            config.extensions.pi_executable.is_some() || config.extensions.pi_source_dir.is_some();
        let executable = config.extensions.pi_executable.as_deref().unwrap_or("pi");
        if executable.is_empty()
            || executable.chars().any(char::is_control)
            || (!Path::new(executable).is_absolute()
                && (executable.chars().any(char::is_whitespace)
                    || executable.contains(['/', '\\'])))
        {
            return Err(io::Error::other(
                "Pi executable must be one name or absolute path",
            ));
        }
        let source = match config.extensions.pi_source_dir.as_deref() {
            None => human_agent_dir(),
            Some(value) => {
                let path = if let Some(relative) = value.strip_prefix("~/") {
                    home_dir().map(|home| home.join(relative))
                } else if Path::new(value).is_absolute() {
                    Some(PathBuf::from(value))
                } else {
                    None
                };
                Some(path.ok_or_else(|| {
                    io::Error::other("Pi source directory must be absolute or ~/ relative")
                })?)
            }
        };
        if explicit && source.is_none() {
            return Err(io::Error::other(
                "explicit Pi source requires HOME or an absolute source directory",
            ));
        }
        // Resolve existing aliases once: namespace and resource reads use exactly
        // the same identity. Missing defaults retain their accepted empty behavior.
        let source = source.map(|path| path.canonicalize().unwrap_or(path));
        Ok(Self {
            executable: executable.into(),
            source,
            explicit,
        })
    }

    fn profile_dir(&self) -> PathBuf {
        let identity = self
            .source
            .as_ref()
            .map(|path| path.as_os_str().as_encoded_bytes())
            .unwrap_or_default();
        crate::config::config_dir().join(format!("pi-agent-{}", &digest_hex(identity)[..16]))
    }

    fn source_text(&self) -> io::Result<String> {
        let Some(source) = &self.source else {
            return Ok(String::new());
        };
        if !self.explicit {
            return Ok(std::fs::read_to_string(source.join("settings.json")).unwrap_or_default());
        }
        if !source.is_dir() {
            return Err(io::Error::other(format!(
                "selected Pi source directory unavailable: {}",
                source.display()
            )));
        }
        match std::fs::read_to_string(source.join("settings.json")) {
            Ok(text) => Ok(text),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(String::new()),
            Err(error) => Err(io::Error::other(format!(
                "selected Pi settings unreadable at {}: {error}",
                source.display()
            ))),
        }
    }
}

pub(crate) fn apply_pane_env(
    config: &crate::config::Config,
    command: &mut portable_pty::CommandBuilder,
) {
    if opted_out() {
        return;
    }
    if let Ok(selected) = Selection::resolve(config) {
        if profile_ready_for(&selected) {
            command.env("PI_CODING_AGENT_DIR", selected.profile_dir());
            return;
        }
    }
    command.env_remove("PI_CODING_AGENT_DIR");
}

/// Bind per launch, not through the parent shell's possibly stale birth env.
/// `env` arguments are ordinary argv, escaped by the existing platform builder.
pub(crate) fn launch(
    config: &crate::config::Config,
    kind_executable: &str,
) -> io::Result<Option<crate::agent_profiles::ProfileLaunch>> {
    if kind_executable != "pi" {
        return Ok(None);
    }
    let selected = Selection::resolve(config)?;
    if selected.explicit && opted_out() {
        return Err(io::Error::other(
            "explicit Pi selection requires bundled profiles; VQRO_NO_BUNDLED_DEFAULTS is set",
        ));
    }
    #[cfg(not(unix))]
    {
        if selected.explicit {
            return Err(io::Error::other(
                "explicit Pi source profiles are unsupported on this platform",
            ));
        }
        Ok(Some(crate::agent_profiles::ProfileLaunch {
            argv: vec![selected.executable],
            prefix: Vec::new(),
            notice: None,
        }))
    }
    #[cfg(unix)]
    {
        if selected.explicit {
            selected.source_text()?;
            if !crate::platform::unix_executable_available(&selected.executable) {
                return Err(io::Error::other(format!(
                    "selected Pi executable unavailable: {}",
                    selected.executable
                )));
            }
        }
        let ready = profile_ready_for(&selected);
        if selected.explicit && !ready {
            return Err(io::Error::other("selected Vqro Pi profile is unprepared or stale; start Vqro again with this configuration"));
        }
        // The pane PATH may intentionally contain only agent executables. Pi's
        // standard Unix launcher already relies on /usr/bin/env; do not add a
        // separate PATH lookup dependency for the environment binding itself.
        let mut prefix = vec!["/usr/bin/env".into()];
        if ready {
            prefix.push(format!(
                "PI_CODING_AGENT_DIR={}",
                selected.profile_dir().display()
            ));
        } else {
            // Default fallback must actively clear an old pane's source binding.
            prefix.extend(["-u".into(), "PI_CODING_AGENT_DIR".into()]);
        }
        Ok(Some(crate::agent_profiles::ProfileLaunch {
            argv: vec![selected.executable],
            prefix,
            notice: (!ready && !opted_out() && selected.source.is_some())
                .then_some(UNAVAILABLE_WARNING),
        }))
    }
}

#[cfg(test)]
fn notice(executable: &str) -> Option<&'static str> {
    (executable == "pi" && warning_needed()).then_some(UNAVAILABLE_WARNING)
}

/// Packages a Vqro-launched Pi must not load: Herdr-coupled or Vqro-competing.
const EXCLUDED_PACKAGES: &[&str] = &[
    "pi-input-gate",
    "pi-tasking-herdr-adapter",
    "rpiv-todo",
    "pi-system-prompt",
];
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
#[cfg(unix)]
const UNMIRRORED: &[&str] = &["settings.json", ".ready", "sessions", "tmp", "SYSTEM.md"];
/// The Vqro system prompt written into the profile.
const VQRO_SYSTEM_MD: &str = include_str!("SYSTEM.md");
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
#[cfg(test)]
fn profile_dir() -> PathBuf {
    Selection::resolve(&crate::config::Config::default())
        .unwrap()
        .profile_dir()
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

fn human_agent_dir() -> Option<PathBuf> {
    home_dir().map(|home| home.join(".pi").join("agent"))
}

/// The content address of the human's settings, or `None` when it is unreadable.
/// The content address of the human's settings; a missing file is the empty
/// settings, so a resource-only human agent dir still gets a profile.
fn source_stamp(selected: &Selection) -> Option<String> {
    if selected.explicit {
        return selected
            .source_text()
            .ok()
            .map(|text| digest_hex(text.as_bytes()));
    }
    selected
        .source
        .as_ref()
        .map(|human| digest_hex(&std::fs::read(human.join("settings.json")).unwrap_or_default()))
}

fn digest_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Pi writes these two top-level display bookkeeping fields on startup. Ignore
/// only those in the SHADOW stamp; source bytes and every other value/array order
/// remain guarded. Shared by marker publication and readiness verification.
fn shadow_settings_stamp(bytes: &[u8]) -> Option<String> {
    let mut settings: Value = serde_json::from_slice(bytes).ok()?;
    let object = settings.as_object_mut()?;
    object.remove("lastChangelogVersion");
    object.remove("extensionChangelogVersions");
    Some(digest_hex(&serde_json::to_vec(&settings).ok()?))
}

/// `None` for unreadable, non-object or non-regular shadow settings.
fn profile_settings_hash(profile: &Path) -> Option<String> {
    let path = profile.join("settings.json");
    if !std::fs::symlink_metadata(&path).ok()?.file_type().is_file() {
        return None;
    }
    shadow_settings_stamp(&std::fs::read(&path).ok()?)
}

/// True when Pi Vqro starts should use the profile: a real (not symlinked)
/// profile directory whose guarded settings match this preparation and whose
/// source settings still match byte-for-byte.
#[cfg(test)]
fn profile_ready() -> bool {
    Selection::resolve(&crate::config::Config::default())
        .is_ok_and(|selected| profile_ready_for(&selected))
}

fn profile_ready_for(selected: &Selection) -> bool {
    if opted_out() || !cfg!(unix) {
        return false;
    }
    let profile = selected.profile_dir();
    if std::fs::symlink_metadata(&profile).is_ok_and(|meta| meta.file_type().is_symlink()) {
        return false;
    }
    let Ok(marker) = std::fs::read_to_string(profile.join(READY_MARKER)) else {
        return false;
    };
    // Private cache v1 markers used raw shadow bytes. Deliberately require a
    // startup reprepare rather than interpreting them as semantic v2 stamps.
    let Some((recorded_source, recorded_profile)) = marker
        .trim()
        .strip_prefix("settings-v2:")
        .and_then(|marker| marker.split_once(':'))
    else {
        return false;
    };
    let Some(source) = source_stamp(selected) else {
        return false;
    };
    let Some(profile_hash) = profile_settings_hash(&profile) else {
        return false;
    };
    // Readiness also attests that the bundled adapter and vqro skill are linked
    // and present, not only that the settings match.
    if !profile.join("skills").join("vqro").exists()
        || !profile.join("extensions").join("vqro-pi.ts").exists()
        || !profile.join("extensions").join("tasks-pi.ts").exists()
    {
        return false;
    }
    source == recorded_source && profile_hash == recorded_profile
}

/// True when a profile is wanted but not ready, so a Vqro-launched Pi must be
/// warned. Any human HOME counts, because the profile also carries the bundled
/// adapter and vqro skill.
#[cfg(test)]
fn warning_needed() -> bool {
    if !cfg!(unix) || opted_out() || profile_ready() {
        return false;
    }
    human_agent_dir().is_some()
}

/// Build or refresh the profile.
///
/// A no-op when the human has no Pi settings (then there is nothing to exclude)
/// or when the defaults are opted out. The ready marker records the content
/// address of the settings used, so a rejected, partial or stale preparation is
/// never selected even when the marker cannot be rewritten.
#[cfg(test)]
fn ensure_profile() -> io::Result<()> {
    ensure_configured_profile(&crate::config::Config::default())
}

pub(crate) fn ensure_configured_profile(config: &crate::config::Config) -> io::Result<()> {
    let selected = Selection::resolve(config)?;
    if opted_out() || !cfg!(unix) {
        return Ok(());
    }
    let profile = selected.profile_dir();
    // Never follow a symlinked profile directory into a human file.
    crate::bundled::safe_under(&profile, &profile)?;
    // Drop the previous readiness first: a preparation that fails must not keep an
    // earlier profile selected. (A read-only directory cannot be cleared; its
    // marker then no longer matches below.)
    if std::fs::symlink_metadata(&profile).is_ok_and(|meta| meta.file_type().is_dir()) {
        let _ = std::fs::remove_file(profile.join(READY_MARKER));
    }
    let Some(human) = selected.source.as_ref() else {
        return Ok(());
    };
    // A missing settings.json is the empty settings, and a missing agent dir means
    // there is nothing to mirror; either way the profile still carries the bundled
    // adapter, vqro skill and SYSTEM.md, so a Vqro pane gets them.
    let text = selected.source_text()?;
    let mut settings: Value = if text.trim().is_empty() {
        Value::Object(serde_json::Map::new())
    } else {
        serde_json::from_str(&text)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?
    };
    if !settings.is_object() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Pi settings must be a JSON object",
        ));
    }
    derive_packages(&mut settings, human);
    derive_paths(&mut settings, human);
    drop_excluded_resources(&mut settings);
    std::fs::create_dir_all(&profile)?;
    let settings_path = profile.join("settings.json");
    crate::bundled::safe_under(&profile, &settings_path)?;
    let body = serde_json::to_string_pretty(&settings)?;
    write_if_changed(&settings_path, &body)?;
    if human.is_dir() {
        mirror_entries(human, &profile)?;
    }
    link_profile_assets(&profile)?;
    let system = profile.join("SYSTEM.md");
    // A profile built before SYSTEM.md was replaced holds our own symlink to the
    // human's file. Unlink the link (never its target) before writing ours, so an
    // upgrade does not fall back to the global config.
    if std::fs::symlink_metadata(&system).is_ok_and(|meta| meta.file_type().is_symlink()) {
        std::fs::remove_file(&system)?;
    }
    crate::bundled::safe_under(&profile, &system)?;
    write_if_changed(&system, VQRO_SYSTEM_MD)?;
    let marker = profile.join(READY_MARKER);
    crate::bundled::safe_under(&profile, &marker)?;
    // Address the exact bytes prepared from this source, not a later reread, so an
    // edit between the write and the marker leaves the profile unselected.
    write_if_changed(
        &marker,
        &format!(
            "settings-v2:{}:{}\n",
            digest_hex(text.as_bytes()),
            shadow_settings_stamp(body.as_bytes())
                .ok_or_else(|| io::Error::other("invalid prepared Pi shadow settings"))?
        ),
    )?;
    Ok(())
}

/// True only for our own previous links: a `pi-<16 hex>` generation directory
/// directly under the bundled root, holding the neutral/Tasks Pi resources or
/// `skills/vqro`. The generation name rule matches the tasks generations. Anything
/// else - another bundled path, a custom or user generation, or a link outside -
/// is foreign and preserved.
#[cfg(unix)]
fn is_owned_pi_asset(root: &Path, path: &Path) -> bool {
    let Ok(relative) = path.strip_prefix(root) else {
        return false;
    };
    let mut components = relative.components();
    let Some(first) = components.next() else {
        return false;
    };
    let name = first.as_os_str().to_string_lossy();
    let Some(generation) = name.strip_prefix("pi-") else {
        return false;
    };
    if generation.len() != 16 || !generation.chars().all(|digit| digit.is_ascii_hexdigit()) {
        return false;
    }
    let rest: PathBuf = components.collect();
    matches!(
        rest.to_str(),
        Some("vqro-reply-routing.ts" | "vqro-pi.ts" | "tasks-pi.ts")
    ) || rest.as_path() == Path::new("skills").join("vqro")
}

/// Link the neutral adapter, Tasks consumer and vqro skill into the profile, so
/// every Pi using it - including one the human types in a Vqro pane - loads them.
/// The links are refreshed when the bundled generation changes. A missing bundled
/// asset fails the preparation, so readiness is never claimed without them.
#[cfg(unix)]
fn link_profile_assets(profile: &Path) -> io::Result<()> {
    let generation = generation_dir();
    let Some(root) = generation.parent() else {
        return Err(io::Error::other("bundled Pi generation has no parent"));
    };
    // Retire only our old product-bearing adapter link. Preserve foreign files.
    let legacy = profile.join("extensions").join("vqro-reply-routing.ts");
    if std::fs::read_link(&legacy).is_ok_and(|target| is_owned_pi_asset(root, &target)) {
        std::fs::remove_file(&legacy)?;
    }
    let assets = [
        (
            generation.join("skills").join("vqro"),
            profile.join("skills").join("vqro"),
        ),
        (
            generation.join("vqro-pi.ts"),
            profile.join("extensions").join("vqro-pi.ts"),
        ),
        (
            generation.join("tasks-pi.ts"),
            profile.join("extensions").join("tasks-pi.ts"),
        ),
    ];
    for (source, target) in assets {
        if !source.exists() {
            return Err(io::Error::other(format!(
                "bundled Pi asset is missing: {}",
                source.display()
            )));
        }
        let Some(parent) = target.parent() else {
            return Err(io::Error::other("profile asset has no parent"));
        };
        std::fs::create_dir_all(parent)?;
        crate::bundled::safe_under(profile, parent)?;
        let owned = match std::fs::read_link(&target) {
            Ok(existing) if existing == source => continue,
            // Our own link into an older generation: replace it.
            Ok(existing) => is_owned_pi_asset(root, &existing),
            // Absent (ours to create) or a real file or directory (leave it).
            Err(_) => std::fs::symlink_metadata(&target).is_err(),
        };
        if !owned {
            continue;
        }
        let _ = std::fs::remove_file(&target);
        crate::platform::resource_symlink(&source, &target)?;
        tracing::info!(path = %target.display(), "linked a Vqro Pi profile asset");
    }
    Ok(())
}

#[cfg(not(unix))]
fn link_profile_assets(_profile: &Path) -> io::Result<()> {
    // Unreachable: ensure_profile returns before this off unix.
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

/// Drop excluded-by-name entries from settings resource lists, so an explicit
/// absolute path cannot re-enable an excluded extension or skill.
fn drop_excluded_resources(settings: &mut Value) {
    let Some(object) = settings.as_object_mut() else {
        return;
    };
    for &(key, excluded) in PER_ENTRY_DIRS {
        if let Some(Value::Array(items)) = object.get_mut(key) {
            items.retain(|item| match item {
                Value::String(value) => {
                    let name = if key == "skills" {
                        skill_name(value)
                    } else {
                        resource_name(value)
                    };
                    !excluded.contains(&name)
                }
                _ => true,
            });
        }
    }
}

/// The name a resource path or pattern selects, ignoring a filter operator and any
/// empty or trailing path components.
fn resource_name(entry: &str) -> &str {
    let rest = entry.strip_prefix(FILTER_OPERATORS).unwrap_or(entry);
    rest.split(['/', '\\'])
        .rfind(|part| !part.is_empty())
        .unwrap_or(rest)
}

/// The skill a settings entry selects. The standard skill file `SKILL.md` names
/// its parent directory; anything else names itself.
fn skill_name(entry: &str) -> &str {
    let rest = entry.strip_prefix(FILTER_OPERATORS).unwrap_or(entry);
    let parts: Vec<&str> = rest
        .split(['/', '\\'])
        .filter(|part| !part.is_empty())
        .collect();
    match parts.last() {
        Some(&"SKILL.md") if parts.len() >= 2 => parts[parts.len() - 2],
        Some(&last) => last,
        None => rest,
    }
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
    // An excluded leaf can be left in the profile by an earlier layout; drop our
    // own symlink, never a real file.
    for name in excluded {
        let leaf = target.join(name);
        if std::fs::symlink_metadata(&leaf).is_ok_and(|meta| meta.file_type().is_symlink()) {
            std::fs::remove_file(&leaf)?;
        }
    }
    Ok(())
}

#[cfg(unix)]
fn link_or_skip(source: &Path, target: &Path) -> io::Result<()> {
    if std::fs::symlink_metadata(target).is_ok() {
        return Ok(());
    }
    crate::platform::resource_symlink(source, target)?;
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

    #[cfg(unix)]
    struct ProfileFixture {
        root: PathBuf,
        saved: Vec<(&'static str, Option<std::ffi::OsString>)>,
        _lock: crate::integration::IntegrationEnvLock,
    }

    #[cfg(unix)]
    impl ProfileFixture {
        fn new(name: &str) -> Self {
            let lock = crate::integration::integration_env_lock();
            let root = std::env::temp_dir()
                .join(format!("profile-lifecycle-{name}-{}", std::process::id()));
            std::fs::create_dir_all(root.join("home/.pi/agent")).unwrap();
            let saved = [
                "HOME",
                "XDG_CONFIG_HOME",
                crate::bundled::OPT_OUT_ENV,
                "PI_CODING_AGENT_DIR",
            ]
            .into_iter()
            .map(|key| (key, std::env::var_os(key)))
            .collect();
            std::env::set_var("HOME", root.join("home"));
            std::env::set_var("XDG_CONFIG_HOME", root.join("config"));
            std::env::remove_var(crate::bundled::OPT_OUT_ENV);
            std::env::remove_var("PI_CODING_AGENT_DIR");
            Self {
                root,
                saved,
                _lock: lock,
            }
        }

        fn prepare(&self) {
            crate::bundled::ensure_defaults().unwrap();
            ensure_profile().unwrap();
        }

        fn pane_override(&self) -> Option<PathBuf> {
            let mut command = portable_pty::CommandBuilder::new("sh");
            crate::integration::apply_pane_base_env(&mut command);
            command.get_env("PI_CODING_AGENT_DIR").map(PathBuf::from)
        }
    }

    #[cfg(unix)]
    impl Drop for ProfileFixture {
        fn drop(&mut self) {
            use std::os::unix::fs::PermissionsExt;
            if profile_dir().exists() {
                let _ =
                    std::fs::set_permissions(profile_dir(), std::fs::Permissions::from_mode(0o700));
            }
            for (key, value) in &self.saved {
                match value {
                    Some(value) => std::env::set_var(key, value),
                    None => std::env::remove_var(key),
                }
            }
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[cfg(unix)]
    #[test]
    fn profile_lifecycle_rechecks_readiness_and_preserves_fallback() {
        let fixture = ProfileFixture::new("fresh");
        let human = human_agent_dir().unwrap();
        std::fs::write(human.join("settings.json"), "{\"theme\":\"old\"}").unwrap();
        std::env::set_var("PI_CODING_AGENT_DIR", "/inherited");
        assert!(!profile_ready());
        assert!(warning_needed());
        assert_eq!(notice("pi"), Some(UNAVAILABLE_WARNING));
        assert_eq!(notice("other-agent"), None);
        assert_eq!(fixture.pane_override(), None);
        fixture.prepare();
        assert!(profile_ready());
        assert_eq!(fixture.pane_override(), Some(profile_dir()));
        assert!(!warning_needed());
        std::fs::write(human.join("settings.json"), "{\"theme\":\"new\"}").unwrap();
        assert!(!profile_ready());
        assert_eq!(fixture.pane_override(), None);
        ensure_profile().unwrap();
        assert!(profile_ready());
        std::fs::write(profile_dir().join("settings.json"), "{}").unwrap();
        assert!(!profile_ready());
        ensure_profile().unwrap();
        let resource = profile_dir().join("extensions/vqro-pi.ts");
        std::fs::remove_file(&resource).unwrap();
        assert!(!profile_ready());
        // A preserved foreign resource is existence-only readiness, not proof
        // that its content/owner is ours. Preserve this existing behavior.
        std::fs::write(&resource, "foreign").unwrap();
        assert!(profile_ready());
        std::fs::write(profile_dir().join(READY_MARKER), "bad marker").unwrap();
        assert!(!profile_ready());
        ensure_profile().unwrap();
        let marker = profile_dir().join(READY_MARKER);
        let saved = fixture.root.join("saved-marker");
        std::fs::rename(&marker, &saved).unwrap();
        std::os::unix::fs::symlink(&saved, &marker).unwrap();
        assert!(profile_ready(), "marker symlinks currently remain readable");
        std::fs::write(human.join("settings.json"), "{").unwrap();
        assert!(ensure_profile().is_err());
        assert!(!profile_ready());
        assert_eq!(fixture.pane_override(), None);
        assert!(warning_needed());
        for value in ["", "0"] {
            std::env::set_var(crate::bundled::OPT_OUT_ENV, value);
            assert!(!opted_out());
        }
        std::env::set_var(crate::bundled::OPT_OUT_ENV, "1");
        assert!(opted_out());
        assert!(!warning_needed());
        assert_eq!(fixture.pane_override(), Some(PathBuf::from("/inherited")));
        use std::os::unix::ffi::OsStringExt;
        std::env::set_var(
            crate::bundled::OPT_OUT_ENV,
            std::ffi::OsString::from_vec(vec![0xff]),
        );
        assert!(!opted_out(), "non-Unicode opt-out currently fails env::var");
    }

    #[cfg(unix)]
    #[test]
    fn profile_lifecycle_preserves_read_errors_and_readonly_marker_quirks() {
        use std::os::unix::fs::PermissionsExt;
        let fixture = ProfileFixture::new("errors");
        let settings = human_agent_dir().unwrap().join("settings.json");
        std::fs::create_dir(&settings).unwrap();
        fixture.prepare();
        assert!(profile_ready(), "unreadable settings prepare as empty");
        std::fs::remove_dir(&settings).unwrap();
        std::fs::write(&settings, [0xff]).unwrap();
        ensure_profile().unwrap();
        assert!(
            !profile_ready(),
            "raw source hash differs from empty UTF8 preparation"
        );
        std::fs::remove_file(&settings).unwrap();
        ensure_profile().unwrap();
        assert!(profile_ready());
        std::fs::remove_file(profile_dir().join("SYSTEM.md")).unwrap();
        std::fs::set_permissions(profile_dir(), std::fs::Permissions::from_mode(0o555)).unwrap();
        assert!(ensure_profile().is_err());
        assert!(
            profile_ready(),
            "unchanged retained marker can still match after readonly failure"
        );
    }

    #[cfg(unix)]
    #[test]
    fn profile_lifecycle_preserves_special_link_replacement_and_human_targets() {
        let fixture = ProfileFixture::new("links");
        let human = human_agent_dir().unwrap();
        for name in ["settings.json", "auth.json", "models-store.json"] {
            std::fs::write(human.join(name), "{}").unwrap();
        }
        std::fs::write(human.join("SYSTEM.md"), "human system").unwrap();
        std::fs::create_dir_all(human.join("extensions")).unwrap();
        std::fs::write(human.join("extensions/keep.ts"), "human extension").unwrap();
        fixture.prepare();
        let profile = profile_dir();
        std::fs::remove_file(profile.join("SYSTEM.md")).unwrap();
        std::os::unix::fs::symlink(human.join("SYSTEM.md"), profile.join("SYSTEM.md")).unwrap();
        std::fs::remove_dir_all(profile.join("extensions")).unwrap();
        std::os::unix::fs::symlink(human.join("extensions"), profile.join("extensions")).unwrap();
        ensure_profile().unwrap();
        assert!(!std::fs::symlink_metadata(profile.join("SYSTEM.md"))
            .unwrap()
            .file_type()
            .is_symlink());
        assert!(!std::fs::symlink_metadata(profile.join("extensions"))
            .unwrap()
            .file_type()
            .is_symlink());
        let excluded = profile.join("extensions/herdr-agent-state.ts");
        std::os::unix::fs::symlink(human.join("extensions/keep.ts"), &excluded).unwrap();
        let legacy = profile.join("extensions/vqro-reply-routing.ts");
        std::os::unix::fs::symlink(
            crate::config::config_dir().join("bundled/pi-0123456789abcdef/vqro-reply-routing.ts"),
            &legacy,
        )
        .unwrap();
        ensure_profile().unwrap();
        assert!(std::fs::symlink_metadata(excluded).is_err());
        assert!(std::fs::symlink_metadata(&legacy).is_err());
        std::fs::write(&legacy, "foreign legacy file").unwrap();
        ensure_profile().unwrap();
        assert_eq!(
            std::fs::read_to_string(legacy).unwrap(),
            "foreign legacy file"
        );
        for name in ["settings.json", "auth.json", "models-store.json"] {
            assert_eq!(std::fs::read_to_string(human.join(name)).unwrap(), "{}");
        }
        assert_eq!(
            std::fs::read_to_string(human.join("SYSTEM.md")).unwrap(),
            "human system"
        );
        assert_eq!(
            std::fs::read_to_string(human.join("extensions/keep.ts")).unwrap(),
            "human extension"
        );
    }

    #[cfg(unix)]
    #[test]
    fn profile_lifecycle_preserves_generation_bytes_and_registry_choices() {
        let fixture = ProfileFixture::new("distribution");
        fixture.prepare();
        // Content-address characterization, not a frozen wire pin: deliberate
        // asset edits change these addresses. The Tasks address was independently
        // recomputed over all 11 declared assets in order, including ids.py and
        // the negotiated caller/view-action adapter, with generation_id separators.
        // The Pi address includes the neutral adapter's paced idle identity
        // recovery and Tasks consumer resource; the skill asset is unchanged.
        assert_eq!(generation_dir().file_name().unwrap(), "pi-8491e8d971703a5c");
        let installed = crate::persist::plugin_registry::load();
        let tasks = installed
            .iter()
            .find(|entry| entry.plugin_id == "vqro.tasks")
            .unwrap();
        assert_eq!(
            Path::new(&tasks.plugin_root).file_name().unwrap(),
            "tasks-605583fe4b19d324"
        );
        crate::persist::plugin_registry::update(|entries| {
            entries
                .iter_mut()
                .find(|entry| entry.plugin_id == "vqro.tasks")
                .unwrap()
                .enabled = false;
        })
        .unwrap();
        crate::bundled::ensure_defaults().unwrap();
        assert!(
            !crate::persist::plugin_registry::load()
                .iter()
                .find(|entry| entry.plugin_id == "vqro.tasks")
                .unwrap()
                .enabled
        );
        let custom = fixture.root.join("custom-tasks").display().to_string();
        crate::persist::plugin_registry::update(|entries| {
            let entry = entries
                .iter_mut()
                .find(|entry| entry.plugin_id == "vqro.tasks")
                .unwrap();
            entry.plugin_root = custom.clone();
            entry.manifest_path = format!("{custom}/vqro-extension.toml");
        })
        .unwrap();
        crate::bundled::ensure_defaults().unwrap();
        let entries = crate::persist::plugin_registry::load();
        let entry = entries
            .iter()
            .find(|entry| entry.plugin_id == "vqro.tasks")
            .unwrap();
        assert_eq!(entry.plugin_root, custom);
        assert!(!entry.enabled);
        assert!(generation_dir().exists());
    }

    #[cfg(unix)]
    #[test]
    fn only_shadow_changelog_bookkeeping_is_ignored_by_readiness() {
        let fixture = ProfileFixture::new("changelog-stamp");
        fixture.prepare();
        let profile = profile_dir();
        let settings_path = profile.join("settings.json");
        let mut value: Value =
            serde_json::from_slice(&std::fs::read(&settings_path).unwrap()).unwrap();
        value["lastChangelogVersion"] = serde_json::json!("1.5.0");
        std::fs::write(&settings_path, serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(
            profile_ready(),
            "Pi's own changelog display update must not invalidate a later launch"
        );
        value["extensionChangelogVersions"] =
            serde_json::json!({"{\"source\":\"npm:fixture\",\"scope\":\"user\"}": "1.0.0"});
        std::fs::write(&settings_path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
        assert!(profile_ready());
        value["lastChangelogVersion"] = serde_json::json!("next");
        std::fs::write(&settings_path, serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(
            profile_ready(),
            "serialization and display-key updates are not policy changes"
        );
        for (key, changed) in [
            ("theme", serde_json::json!("changed")),
            ("defaultModel", serde_json::json!("changed")),
            ("defaultProvider", serde_json::json!("changed")),
            ("packages", serde_json::json!(["npm:changed"])),
            ("extensions", serde_json::json!(["changed.ts"])),
            ("skills", serde_json::json!(["changed.md"])),
            ("prompts", serde_json::json!(["changed.md"])),
            ("themes", serde_json::json!(["changed.json"])),
            ("shellPath", serde_json::json!("changed")),
            (
                "nested",
                serde_json::json!({"lastChangelogVersion": "not-top-level", "extensionChangelogVersions": {"x": "1"}}),
            ),
        ] {
            let mut changed_value = value.clone();
            changed_value[key] = changed;
            std::fs::write(&settings_path, serde_json::to_vec(&changed_value).unwrap()).unwrap();
            assert!(!profile_ready(), "guarded key {key} changed");
        }
        for invalid in ["[", "[]", "null", "\"string\""] {
            std::fs::write(&settings_path, invalid).unwrap();
            assert!(
                !profile_ready(),
                "invalid/non-object settings must not be empty-ready"
            );
        }
        std::fs::write(&settings_path, serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(profile_ready());
        let marker = std::fs::read_to_string(profile.join(READY_MARKER)).unwrap();
        let legacy = marker.strip_prefix("settings-v2:").unwrap();
        std::fs::write(profile.join(READY_MARKER), legacy).unwrap();
        assert!(
            !profile_ready(),
            "old cache markers deliberately require reprepare"
        );
        std::fs::write(profile.join(READY_MARKER), marker).unwrap();
        let human = human_agent_dir().unwrap();
        std::fs::write(human.join("settings.json"), "{}\n").unwrap();
        assert!(
            !profile_ready(),
            "source byte changes are never projected away"
        );
        std::fs::write(
            human.join("settings.json"),
            "{\"extensions\":[\"a.ts\",\"b.ts\"]}",
        )
        .unwrap();
        fixture.prepare();
        let mut ordered: Value =
            serde_json::from_slice(&std::fs::read(&settings_path).unwrap()).unwrap();
        ordered["extensions"] = serde_json::json!(["b.ts", "a.ts"]);
        std::fs::write(&settings_path, serde_json::to_vec(&ordered).unwrap()).unwrap();
        assert!(!profile_ready(), "array order stays guarded");
    }

    #[cfg(unix)]
    #[test]
    fn explicit_sources_are_isolated_and_unprepared_switches_fail_closed() {
        let fixture = ProfileFixture::new("selected-sources");
        fixture.prepare();
        let mut config = crate::config::Config::default();
        config.extensions.pi_executable = Some("/bin/true".into());
        let source_a = fixture.root.join("source a;$literal");
        let source_b = fixture.root.join("source b");
        for (source, marker) in [(&source_a, "a"), (&source_b, "b")] {
            std::fs::create_dir(source).unwrap();
            std::fs::write(source.join("settings.json"), "{}").unwrap();
            std::fs::write(source.join("resource.txt"), marker).unwrap();
        }
        config.extensions.pi_source_dir = Some(source_a.to_string_lossy().into());
        assert!(
            launch(&config, "pi").is_err(),
            "explicit unprepared source must not launch"
        );
        ensure_configured_profile(&config).unwrap();
        let a = Selection::resolve(&config).unwrap();
        let launch_a = launch(&config, "pi").unwrap().unwrap();
        assert_eq!(
            launch_a.prefix[1],
            format!("PI_CODING_AGENT_DIR={}", a.profile_dir().display())
        );
        assert!(launch_a.notice.is_none());
        config.extensions.pi_source_dir = Some(source_b.to_string_lossy().into());
        let b = Selection::resolve(&config).unwrap();
        assert_ne!(a.profile_dir(), b.profile_dir());
        assert!(
            launch(&config, "pi").is_err(),
            "same settings do not prepare another source"
        );
        ensure_configured_profile(&config).unwrap();
        assert_eq!(
            std::fs::read_to_string(a.profile_dir().join("resource.txt")).unwrap(),
            "a"
        );
        assert_eq!(
            std::fs::read_to_string(b.profile_dir().join("resource.txt")).unwrap(),
            "b"
        );
        let launch_b = launch(&config, "pi").unwrap().unwrap();
        let mut argv: Vec<String> = launch_b.prefix.into_iter().chain(launch_b.argv).collect();
        // Reuse the actual bound command with a deterministic executable, in a
        // pane whose inherited value is the old profile. No parent env mutation.
        argv.pop();
        argv.extend([
            "/bin/sh".into(),
            "-c".into(),
            "printf '%s' \"$PI_CODING_AGENT_DIR\"".into(),
        ]);
        let output = std::process::Command::new(&argv[0])
            .args(&argv[1..])
            .env("PI_CODING_AGENT_DIR", a.profile_dir())
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            b.profile_dir().to_string_lossy()
        );
        std::fs::create_dir(source_b.join("broken")).unwrap();
        std::fs::remove_file(source_b.join("settings.json")).unwrap();
        std::fs::create_dir(source_b.join("settings.json")).unwrap();
        assert!(ensure_configured_profile(&config).is_err());
        assert!(
            launch(&config, "pi").is_err(),
            "unreadable explicit settings are not empty-ready"
        );
        config.extensions.pi_source_dir = Some("relative/source".into());
        assert!(launch(&config, "pi").is_err());
        config.extensions.pi_source_dir =
            Some(fixture.root.join("missing").to_string_lossy().into());
        assert!(ensure_configured_profile(&config).is_err());
        assert!(launch(&config, "pi").is_err());
        // A source-only explicit selection keeps the executable default.
        config.extensions.pi_executable = None;
        config.extensions.pi_source_dir = Some(source_a.to_string_lossy().into());
        assert_eq!(Selection::resolve(&config).unwrap().executable, "pi");
        std::env::set_var(crate::bundled::OPT_OUT_ENV, "1");
        assert!(
            launch(&config, "pi").is_err(),
            "opt-out cannot silently change an explicit source"
        );
    }

    #[cfg(unix)]
    #[test]
    fn default_fallback_unsets_stale_pane_env_and_executable_override_is_literal() {
        use std::os::unix::fs::PermissionsExt;
        let fixture = ProfileFixture::new("bound-fallback");
        let config = crate::config::Config::default();
        let fallback = launch(&config, "pi").unwrap().unwrap();
        assert_eq!(fallback.argv, ["pi"]);
        assert_eq!(
            fallback.prefix,
            ["/usr/bin/env", "-u", "PI_CODING_AGENT_DIR"]
        );
        assert_eq!(fallback.notice, Some(UNAVAILABLE_WARNING));
        let output = std::process::Command::new(&fallback.prefix[0])
            .args(&fallback.prefix[1..3])
            .args([
                "/bin/sh",
                "-c",
                "printf '%s' \"${PI_CODING_AGENT_DIR-unset}\"",
            ])
            .env("PI_CODING_AGENT_DIR", "/old/source-a")
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"unset");
        fixture.prepare();
        let executable = fixture.root.join("pi1 binary;$literal");
        std::fs::write(
            &executable,
            "#!/bin/sh\nprintf '%s' \"$PI_CODING_AGENT_DIR\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut config = config;
        config.extensions.pi_executable = Some(executable.to_string_lossy().into());
        let selected = launch(&config, "pi").unwrap().unwrap();
        assert_eq!(selected.argv.last().unwrap(), &executable.to_string_lossy());
        let command_argv: Vec<String> = selected.prefix.into_iter().chain(selected.argv).collect();
        let command = crate::platform::interactive_shell_command(&command_argv, "bash").unwrap();
        let output = std::process::Command::new("/bin/bash")
            .args(["-c", &command])
            .env("PATH", &fixture.root)
            .env("PI_CODING_AGENT_DIR", "/old/source-a")
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, profile_dir().to_string_lossy().as_bytes());
        // Absolute executable matching is still Pi-kind scoped; invalid Pi
        // configuration cannot warn or change a non-Pi launch.
        config.extensions.pi_executable = Some("/missing/pi1".into());
        assert!(launch(&config, "pi")
            .unwrap_err()
            .to_string()
            .contains("executable unavailable"));
        assert!(launch(&config, "claude").unwrap().is_none());
        for invalid in ["", "pi1 --model x", "relative/pi1", "pi1\n"] {
            config.extensions.pi_executable = Some(invalid.into());
            assert!(launch(&config, "pi").is_err());
        }
    }

    #[test]
    fn derive_drops_excluded_packages_and_keeps_the_rest() {
        let mut value = settings(&[
            "npm:@blazer2k/searxng-suite",
            "/opt/packages/pi-input-gate",
            "git:https://example.com/o/pi-tasking-herdr-adapter.git",
            "../../Projects/pi-budget",
            "/opt/packages/rpiv-todo",
            "../../Projects/pi-system-prompt",
            "npm:pi-system-prompt@1.0.0",
            "git:https://example.com/o/pi-system-prompt.git@v1",
            "/opt//pi-system-prompt//",
            "npm:pi-system-prompt-extra",
            "../../Projects/keep-pi-system-prompt",
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
                "/home/h/.pi/agent/../../Projects/pi-budget",
                "npm:pi-system-prompt-extra",
                "/home/h/.pi/agent/../../Projects/keep-pi-system-prompt"
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
                { "source": "../../Projects/pi-budget", "skills": ["skills/*.md"] },
            ]
        });
        derive_packages(&mut value, Path::new("/home/h/.pi/agent"));
        let kept = value["packages"].as_array().unwrap();
        assert_eq!(kept.len(), 1);
        assert_eq!(
            kept[0]["source"],
            "/home/h/.pi/agent/../../Projects/pi-budget"
        );
    }

    #[cfg(unix)]
    #[test]
    fn owned_pi_asset_matches_only_our_generation_paths() {
        let root = Path::new("/cfg/bundled");
        assert!(is_owned_pi_asset(
            root,
            Path::new("/cfg/bundled/pi-0123456789abcdef/vqro-reply-routing.ts")
        ));
        assert!(is_owned_pi_asset(
            root,
            Path::new("/cfg/bundled/pi-0123456789abcdef/skills/vqro")
        ));
        // A non-16-hex or non-hex generation name is not ours.
        assert!(!is_owned_pi_asset(
            root,
            Path::new("/cfg/bundled/pi-nothex/vqro-reply-routing.ts")
        ));
        assert!(!is_owned_pi_asset(
            root,
            Path::new("/cfg/bundled/pi-0123456789abcde/vqro-reply-routing.ts")
        ));
        // A foreign or custom bundled path is preserved.
        assert!(!is_owned_pi_asset(
            root,
            Path::new("/cfg/bundled/custom/vqro-reply-routing.ts")
        ));
        assert!(!is_owned_pi_asset(
            root,
            Path::new("/cfg/bundled/pi-0123456789abcdef/other")
        ));
        assert!(!is_owned_pi_asset(
            root,
            Path::new("/elsewhere/pi-0123456789abcdef/skills/vqro")
        ));
    }

    #[test]
    fn source_stamp_of_a_missing_settings_file_is_the_empty_digest() {
        let root = std::env::temp_dir().join(format!("vqro-pi-stamp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let selected = Selection {
            executable: "pi".into(),
            source: Some(root.clone()),
            explicit: false,
        };
        assert_eq!(source_stamp(&selected), Some(digest_hex(b"")));
        std::fs::write(root.join("settings.json"), "{}").unwrap();
        assert_ne!(source_stamp(&selected), Some(digest_hex(b"")));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn explicit_resource_paths_are_filtered_by_name() {
        let mut value = serde_json::json!({
            "extensions": [
                "/abs/herdr-agent-state.ts",
                "keep-extension.ts",
                "!../herdr-agent-state.ts",
            ],
            "skills": [
                "/abs/skills/herdr/SKILL.md",
                "/abs/skills/check-session-loaded/SKILL.md",
                "herdr",
                "/abs/skills/keep-skill/SKILL.md",
            ],
        });
        drop_excluded_resources(&mut value);
        assert_eq!(
            value["extensions"],
            serde_json::json!(["keep-extension.ts"])
        );
        assert_eq!(
            value["skills"],
            serde_json::json!(["/abs/skills/keep-skill/SKILL.md"])
        );
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
