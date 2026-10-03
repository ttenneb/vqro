//! Core in-tree extensions installed and enabled by default.
//!
//! The tasks provider/pane, the decision queue and messaging all live in the
//! trusted `vqro.tasks` legacy plugin, and the Pi reply-routing adapter plus the
//! agent-conventions skill ship with the binary. A plain `vqro` launch publishes
//! those embedded files as one immutable generation under the Vqro config
//! directory and links the plugin, so the core experience needs no manual
//! linking.
//!
//! Nothing here touches another product's state: everything is rooted at this
//! binary's own config directory, and the Pi adapter/skill are only offered to
//! Pi processes Vqro itself starts in its panes.

use std::io;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

const TASKS_PLUGIN_ID: &str = "vqro.tasks";

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

/// Content address for one managed generation.
///
/// This is a directory name, not a skip stamp: a linked provider always points at
/// a complete, immutable set of files rather than a tree another launch may be
/// rewriting.
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

fn generation_matches(dir: &Path, assets: &[(&str, &str)]) -> bool {
    assets.iter().all(|(name, body)| {
        std::fs::read(dir.join(name)).is_ok_and(|current| current == body.as_bytes())
    })
}

/// Publish a complete generation at its content address and return its directory.
///
/// Files are written to a staging directory first and swapped in with one rename,
/// so no launch ever mutates the generation a linked provider is reading.
fn publish(root: &Path, prefix: &str, assets: &[(&str, &str)]) -> io::Result<PathBuf> {
    let dir = generation_dir(root, prefix, assets);
    if generation_matches(&dir, assets) {
        return Ok(dir);
    }
    let staging = root.join(format!(".{prefix}.staging.{}", std::process::id()));
    if staging.exists() {
        std::fs::remove_dir_all(&staging)?;
    }
    std::fs::create_dir_all(&staging)?;
    for (name, body) in assets {
        let path = staging.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, body)?;
    }
    match std::fs::rename(&staging, &dir) {
        Ok(()) => Ok(dir),
        // Another launch published the same generation first; use theirs.
        Err(_) if generation_matches(&dir, assets) => {
            let _ = std::fs::remove_dir_all(&staging);
            Ok(dir)
        }
        // The address exists but is incomplete or tampered with: repair it.
        Err(_) => {
            std::fs::remove_dir_all(&dir)?;
            std::fs::rename(&staging, &dir)?;
            Ok(dir)
        }
    }
}

pub(crate) fn ensure_defaults() -> io::Result<()> {
    let root = bundled_root();
    std::fs::create_dir_all(&root)?;
    let tasks_dir = publish(&root, "tasks", &tasks_assets())?;
    publish(&root, "pi", &pi_assets())?;
    link_tasks_plugin(&root, &tasks_dir)
}

/// Point `vqro.tasks` at this generation.
///
/// A user-selected link is left exactly as it is, including a disabled state; one
/// of our own older generations is repointed while preserving its enabled choice.
fn link_tasks_plugin(root: &Path, tasks_dir: &Path) -> io::Result<()> {
    let current = tasks_dir.display().to_string();
    let manifest = tasks_dir.join("vqro-extension.toml").display().to_string();
    let existing = crate::persist::plugin_registry::load()
        .into_iter()
        .find(|plugin| plugin.plugin_id == TASKS_PLUGIN_ID);
    match existing {
        Some(plugin) if plugin.plugin_root == current => Ok(()),
        Some(plugin) if Path::new(&plugin.plugin_root).starts_with(root) => {
            crate::persist::plugin_registry::update(|plugins| {
                if let Some(plugin) = plugins
                    .iter_mut()
                    .find(|plugin| plugin.plugin_id == TASKS_PLUGIN_ID)
                {
                    plugin.plugin_root = current.clone();
                    plugin.manifest_path = manifest.clone();
                }
            })?;
            Ok(())
        }
        Some(_) => Ok(()),
        None => {
            crate::plugin_paths::ensure_plugin_user_dirs(TASKS_PLUGIN_ID)?;
            let mut plugin = crate::app::load_plugin_manifest(&manifest, true)
                .map_err(|(_, message)| io::Error::other(message))?;
            crate::persist::plugin_registry::update(|plugins| {
                plugin.load_order = plugins
                    .iter()
                    .map(|entry| entry.load_order)
                    .max()
                    .unwrap_or(0)
                    .saturating_add(1);
                plugins.push(plugin);
            })?;
            Ok(())
        }
    }
}

/// Extra Pi flags that load the bundled adapter and conventions skill.
///
/// Read-only: it only names the published generation, so starting a Pi never
/// hashes, writes or takes a lock on the App thread. A server that never saw a
/// plain-launch install simply gets no flags and Pi still starts.
pub(crate) fn pi_launch_flags() -> Vec<String> {
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
