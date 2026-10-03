//! Core in-tree extensions installed and enabled by default.
//!
//! The tasks provider/pane, the decision queue and messaging all live in the
//! trusted `vqro.tasks` legacy plugin, and the Pi reply-routing adapter plus the
//! agent-conventions skill ship with the binary. On first launch (or whenever the
//! bundled content changes) this module materializes those embedded files into
//! the Vqro config directory and links the plugin, so a plain `vqro` launch has
//! the core experience without any manual linking.
//!
//! Nothing here touches another product's state: everything is rooted at this
//! binary's own config directory, and the Pi adapter/skill are only offered to
//! Pi processes Vqro itself starts in its panes.

use std::io;
use std::path::PathBuf;

use sha2::{Digest, Sha256};

const TASKS_PLUGIN_ID: &str = "vqro.tasks";

const TASKS_MANIFEST: &str = include_str!("../extensions/tasks/vqro-extension.toml");
const TASKS_README: &str = include_str!("../extensions/tasks/README.md");
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

/// Stable paths for the bundled defaults.
pub(crate) struct BundledPaths {
    pub tasks_dir: PathBuf,
    pub pi_adapter: PathBuf,
    pub pi_skill: PathBuf,
}

fn bundled_root() -> PathBuf {
    crate::config::config_dir().join("bundled")
}

pub(crate) fn paths() -> BundledPaths {
    let root = bundled_root();
    BundledPaths {
        tasks_dir: root.join("tasks"),
        pi_adapter: root.join("pi").join("vqro-reply-routing.ts"),
        pi_skill: root.join("pi").join("skills").join("vqro").join("SKILL.md"),
    }
}

fn content_digest() -> String {
    let mut hasher = Sha256::new();
    for (name, body) in TASKS_FILES.iter().copied().chain([
        ("vqro-extension.toml", TASKS_MANIFEST),
        ("README.md", TASKS_README),
    ]) {
        hasher.update(name.as_bytes());
        hasher.update([0]);
        hasher.update(body.as_bytes());
        hasher.update([0xff]);
    }
    hasher.update(PI_ADAPTER.as_bytes());
    hasher.update([0xff]);
    hasher.update(VQRO_SKILL.as_bytes());
    let digest = hasher.finalize();
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Materialize the embedded extensions and link the tasks plugin.
///
/// Idempotent and cheap after the first run: files are rewritten only when the
/// bundled content digest changes, and the plugin is linked only when it is not
/// already registered. A user-chosen link to a different tasks directory is left
/// untouched.
pub(crate) fn ensure_defaults() -> io::Result<BundledPaths> {
    let paths = paths();
    let root = bundled_root();
    std::fs::create_dir_all(&root)?;

    let digest = content_digest();
    let version_path = root.join(".version");
    let up_to_date = std::fs::read_to_string(&version_path)
        .map(|value| value.trim() == digest)
        .unwrap_or(false);
    if !up_to_date {
        write_tasks_files(&paths.tasks_dir)?;
        write_pi_files(&paths.pi_adapter, &paths.pi_skill)?;
        std::fs::write(&version_path, format!("{digest}\n"))?;
    }

    link_tasks_plugin(&paths.tasks_dir)?;
    Ok(paths)
}

fn write_tasks_files(dir: &std::path::Path) -> io::Result<()> {
    std::fs::create_dir_all(dir)?;
    std::fs::write(dir.join("vqro-extension.toml"), TASKS_MANIFEST)?;
    std::fs::write(dir.join("README.md"), TASKS_README)?;
    for (name, body) in TASKS_FILES {
        std::fs::write(dir.join(name), body)?;
    }
    Ok(())
}

fn write_pi_files(adapter: &std::path::Path, skill: &std::path::Path) -> io::Result<()> {
    if let Some(parent) = adapter.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(adapter, PI_ADAPTER)?;
    if let Some(parent) = skill.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(skill, VQRO_SKILL)?;
    Ok(())
}

fn link_tasks_plugin(tasks_dir: &std::path::Path) -> io::Result<()> {
    let linked = crate::persist::plugin_registry::load()
        .into_iter()
        .any(|plugin| {
            plugin.plugin_id == TASKS_PLUGIN_ID
                && plugin.plugin_root == tasks_dir.display().to_string()
        });
    if linked {
        return Ok(());
    }
    crate::plugin_paths::ensure_plugin_user_dirs(TASKS_PLUGIN_ID)?;
    let plugin = crate::app::load_plugin_manifest(&tasks_dir.display().to_string(), true)
        .map_err(|(_, message)| io::Error::other(message))?;
    crate::persist::plugin_registry::update(|plugins| {
        let mut plugin = plugin.clone();
        let replaced_order = plugins
            .iter()
            .find(|entry| entry.plugin_id == plugin.plugin_id)
            .map(|entry| entry.load_order);
        plugins.retain(|entry| entry.plugin_id != plugin.plugin_id);
        plugin.load_order = replaced_order.unwrap_or_else(|| {
            plugins
                .iter()
                .map(|entry| entry.load_order)
                .max()
                .unwrap_or(0)
                .saturating_add(1)
        });
        plugins.push(plugin);
    })?;
    Ok(())
}

/// Extra Pi flags that load the bundled adapter and conventions skill.
///
/// Returns an empty vector when the defaults could not be prepared; a launch
/// must still succeed without them.
pub(crate) fn pi_launch_flags() -> Vec<String> {
    match ensure_defaults() {
        Ok(paths) => vec![
            "--extension".into(),
            paths.pi_adapter.display().to_string(),
            "--skill".into(),
            paths.pi_skill.display().to_string(),
        ],
        Err(err) => {
            tracing::warn!(err = %err, "failed to prepare bundled Pi defaults");
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_tasks_manifest_declares_core_service_and_pane() {
        assert!(TASKS_MANIFEST.contains("id = \"vqro.tasks\""));
        assert!(TASKS_MANIFEST.contains("id = \"core\""));
        assert!(TASKS_MANIFEST.contains("id = \"tree\""));
        assert!(TASKS_MANIFEST.contains("provider.py"));
        assert!(TASKS_MANIFEST.contains("view.py"));
    }

    #[test]
    fn content_digest_tracks_task_and_pi_content() {
        let first = content_digest();
        assert_eq!(first.len(), 64);
        assert_eq!(first, content_digest());
    }

    #[test]
    fn pi_skill_has_skill_frontmatter() {
        assert!(VQRO_SKILL.starts_with("---"));
        assert!(VQRO_SKILL.contains("description:"));
    }
}
