//! Core in-tree extensions installed and enabled by default.
//!
//! The tasks provider/pane, the decision queue and messaging all live in the
//! trusted `vqro.tasks` legacy plugin, and the Pi reply-routing adapter plus the
//! agent-conventions skill ship with the binary. A plain `vqro` launch publishes
//! those embedded files as one immutable generation under the Vqro config
//! directory and links the plugin, so the core experience needs no manual
//! linking.
//!
//! Installs are conservative: a generation is written only when it is missing or
//! its content changed, files already present are never rewritten (so a user edit
//! inside a published generation stays), a user-linked replacement or an
//! explicitly disabled `vqro.tasks` is left as it is, and nothing is written
//! through a symlink. Set [`OPT_OUT_ENV`] to disable the installs and the Pi
//! flags entirely.
//!
//! Nothing here touches another product's state: everything is rooted at this
//! binary's own config directory, and the Pi adapter/skill are only offered to
//! Pi processes Vqro itself starts in its panes. `~/.pi/agent` and any Herdr
//! config are never written.

use std::io::{self, Write};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

const TASKS_PLUGIN_ID: &str = "vqro.tasks";

/// Environment variable that disables the bundled defaults when non-empty and
/// not `0`.
pub(crate) const OPT_OUT_ENV: &str = "VQRO_NO_BUNDLED_DEFAULTS";

const TASKS_MANIFEST: &str = include_str!("../extensions/tasks/vqro-extension.toml");
const TASKS_FILES: &[(&str, &str)] = &[
    (
        "provider.py",
        include_str!("../extensions/tasks/provider.py"),
    ),
    ("view.py", include_str!("../extensions/tasks/view.py")),
    (
        "document.py",
        include_str!("../extensions/tasks/document.py"),
    ),
    (
        "decision_queue.py",
        include_str!("../extensions/tasks/decision_queue.py"),
    ),
    (
        "messages.py",
        include_str!("../extensions/tasks/messages.py"),
    ),
    ("graph.py", include_str!("../extensions/tasks/graph.py")),
];
const PI_ADAPTER: &str = include_str!("../extensions/pi-reply-routing/index.ts");
const VQRO_SKILL: &str = include_str!("../skills/vqro/SKILL.md");

/// The tasks plugin files, keyed by path inside the generation directory.
fn tasks_assets() -> Vec<(&'static str, &'static str)> {
    let mut assets = vec![("vqro-extension.toml", TASKS_MANIFEST)];
    assets.extend_from_slice(TASKS_FILES);
    assets
}

/// The Pi adapter and conventions skill, keyed by path inside the generation.
fn pi_assets() -> [(&'static str, &'static str); 2] {
    [
        ("vqro-reply-routing.ts", PI_ADAPTER),
        ("skills/vqro/SKILL.md", VQRO_SKILL),
    ]
}

fn bundled_root() -> PathBuf {
    crate::config::config_dir().join("bundled")
}

fn opted_out() -> bool {
    std::env::var(OPT_OUT_ENV).is_ok_and(|value| !value.is_empty() && value != "0")
}

/// Content address for one managed generation.
///
/// A linked provider always points at a complete, immutable set of files, so a
/// changed embedded set publishes a new directory instead of rewriting a live
/// one.
fn generation_id(assets: &[(&str, &str)]) -> String {
    let mut hasher = Sha256::new();
    for (name, body) in assets {
        hasher.update(name.as_bytes());
        hasher.update([0]);
        hasher.update(body.as_bytes());
        hasher.update([0xff]);
    }
    hasher
        .finalize()
        .iter()
        .take(8)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn generation_dir(root: &Path, prefix: &str, assets: &[(&str, &str)]) -> PathBuf {
    root.join(format!("{prefix}-{}", generation_id(assets)))
}

/// Refuse to install through a symlink anywhere below `root`.
///
/// A pre-existing symlink at the bundled root, a generation directory, a nested
/// directory or an asset path must never redirect a managed write to an
/// unrelated file. Same-user, non-adversarial scope; this only preserves
/// unrelated data.
fn safe_under(root: &Path, path: &Path) -> io::Result<()> {
    let relative = path.strip_prefix(root).map_err(|_| {
        io::Error::other(format!(
            "bundled path escapes the config root: {}",
            path.display()
        ))
    })?;
    if std::fs::symlink_metadata(root).is_ok_and(|meta| meta.file_type().is_symlink()) {
        return Err(io::Error::other(format!(
            "refusing to install into a symlinked bundled root: {}",
            root.display()
        )));
    }
    let mut current = root.to_path_buf();
    for component in relative.components() {
        current.push(component);
        if std::fs::symlink_metadata(&current).is_ok_and(|meta| meta.file_type().is_symlink()) {
            return Err(io::Error::other(format!(
                "refusing to write through a symlink: {}",
                current.display()
            )));
        }
    }
    Ok(())
}

/// Write one asset to `path` through a fresh temp file and log the final path.
///
/// `create_new` refuses an existing (or symlinked) temp path, so a pre-existing
/// link cannot redirect the write.
fn write_asset(root: &Path, path: &Path, report: &Path, body: &str) -> io::Result<()> {
    safe_under(root, path)?;
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("bundled asset has no parent directory"))?;
    std::fs::create_dir_all(parent)?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| io::Error::other("bundled asset has no file name"))?;
    let temporary = parent.join(format!(".{name}.tmp.{}", std::process::id()));
    safe_under(root, &temporary)?;
    if std::fs::symlink_metadata(&temporary).is_ok() {
        std::fs::remove_file(&temporary)?;
    }
    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(body.as_bytes())?;
        drop(file);
        std::fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result?;
    tracing::info!(path = %report.display(), bytes = body.len(), "published bundled asset");
    Ok(())
}

/// Publish one complete generation and return its directory.
///
/// A generation directory appears only through one atomic rename of a complete
/// staging directory, so its existence already means it is complete. An existing
/// generation is trusted and never read back or rewritten, so a user edit inside
/// a published generation stays. A changed embedded set has a different content
/// address, so it lands in a new directory and never rewrites a generation a
/// provider may be reading.
fn publish_generation(root: &Path, prefix: &str, assets: &[(&str, &str)]) -> io::Result<PathBuf> {
    let dir = generation_dir(root, prefix, assets);
    safe_under(root, &dir)?;
    if dir.is_dir() {
        return Ok(dir);
    }
    let staging = root.join(format!(".{prefix}.staging.{}", std::process::id()));
    safe_under(root, &staging)?;
    if staging.exists() {
        std::fs::remove_dir_all(&staging)?;
    }
    for (name, body) in assets {
        write_asset(root, &staging.join(name), &dir.join(name), body)?;
    }
    match std::fs::rename(&staging, &dir) {
        Ok(()) => Ok(dir),
        // Another launch published the same generation first; use theirs.
        Err(_) if dir.is_dir() => {
            let _ = std::fs::remove_dir_all(&staging);
            Ok(dir)
        }
        Err(error) => {
            let _ = std::fs::remove_dir_all(&staging);
            Err(error)
        }
    }
}

pub(crate) fn ensure_defaults() -> io::Result<()> {
    if opted_out() {
        return Ok(());
    }
    let root = bundled_root();
    std::fs::create_dir_all(&root)?;
    let tasks_dir = publish_generation(&root, "tasks", &tasks_assets())?;
    publish_generation(&root, "pi", &pi_assets())?;
    link_tasks_plugin(&root, &tasks_dir)
}

/// Point `vqro.tasks` at this generation.
///
/// The decision and the mutation happen under one registry lock, so a concurrent
/// first launch or explicit link cannot duplicate the entry between a check and a
/// write. A user-selected link is left exactly as it is, including a disabled
/// state; one of our own older generations is repointed while preserving its
/// enabled choice.
fn link_tasks_plugin(root: &Path, tasks_dir: &Path) -> io::Result<()> {
    let current = tasks_dir.display().to_string();
    let manifest = tasks_dir.join("vqro-extension.toml").display().to_string();
    let candidate = crate::app::load_plugin_manifest(&manifest, true)
        .map_err(|(_, message)| io::Error::other(message))?;
    crate::plugin_paths::ensure_plugin_user_dirs(TASKS_PLUGIN_ID)?;
    crate::persist::plugin_registry::update(|plugins| {
        match plugins
            .iter_mut()
            .find(|plugin| plugin.plugin_id == TASKS_PLUGIN_ID)
        {
            Some(plugin) => {
                if plugin.plugin_root != current && Path::new(&plugin.plugin_root).starts_with(root)
                {
                    plugin.plugin_root = current.clone();
                    plugin.manifest_path = manifest.clone();
                }
            }
            None => {
                let mut plugin = candidate.clone();
                plugin.load_order = plugins
                    .iter()
                    .map(|entry| entry.load_order)
                    .max()
                    .unwrap_or(0)
                    .saturating_add(1);
                plugins.push(plugin);
            }
        }
    })?;
    Ok(())
}

/// Extra Pi flags that load the bundled adapter and conventions skill.
///
/// Read-only: it only names the published generation, so starting a Pi never
/// hashes user content, writes, or takes a lock on the App thread. A server that
/// never saw a plain-launch install, or a launch that opted out, simply gets no
/// flags and Pi still starts.
pub(crate) fn pi_launch_flags() -> Vec<String> {
    if opted_out() {
        return Vec::new();
    }
    let dir = generation_dir(&bundled_root(), "pi", &pi_assets());
    let adapter = dir.join("vqro-reply-routing.ts");
    let skill = dir.join("skills").join("vqro").join("SKILL.md");
    if !adapter.is_file() || !skill.is_file() {
        return Vec::new();
    }
    vec![
        "--extension".into(),
        adapter.display().to_string(),
        "--skill".into(),
        skill.display().to_string(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assets() -> [(&'static str, &'static str); 2] {
        [("one.py", "first"), ("nested/two.py", "second")]
    }

    fn temp_root(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("vqro-bundled-{name}-{}", std::process::id()))
    }

    #[test]
    fn publish_writes_a_complete_generation() {
        let root = temp_root("complete");
        let _ = std::fs::remove_dir_all(&root);
        let dir = publish_generation(&root, "gen", &assets()).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join("one.py")).unwrap(),
            "first"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("nested/two.py")).unwrap(),
            "second"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn publish_trusts_an_existing_generation() {
        let root = temp_root("trust");
        let _ = std::fs::remove_dir_all(&root);
        let dir = publish_generation(&root, "gen", &assets()).unwrap();
        std::fs::write(dir.join("one.py"), "edited").unwrap();
        std::fs::remove_file(dir.join("nested/two.py")).unwrap();
        let again = publish_generation(&root, "gen", &assets()).unwrap();
        // The existing generation is trusted, never read back or rewritten.
        assert_eq!(again, dir);
        assert_eq!(
            std::fs::read_to_string(dir.join("one.py")).unwrap(),
            "edited"
        );
        assert!(!dir.join("nested/two.py").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn changed_content_publishes_a_new_generation() {
        let root = temp_root("changed");
        let _ = std::fs::remove_dir_all(&root);
        let first = publish_generation(&root, "gen", &assets()).unwrap();
        let changed = [("one.py", "first"), ("nested/two.py", "changed")];
        let second = publish_generation(&root, "gen", &changed).unwrap();
        assert_ne!(first, second);
        assert_eq!(
            std::fs::read_to_string(first.join("nested/two.py")).unwrap(),
            "second"
        );
        assert_eq!(
            std::fs::read_to_string(second.join("nested/two.py")).unwrap(),
            "changed"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn publish_refuses_a_symlinked_generation() {
        let root = temp_root("symlink");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let outside =
            std::env::temp_dir().join(format!("vqro-bundled-outside-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&outside);
        std::fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, generation_dir(&root, "gen", &assets())).unwrap();
        assert!(publish_generation(&root, "gen", &assets()).is_err());
        assert!(std::fs::read_dir(&outside).unwrap().next().is_none());
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&outside);
    }
}
