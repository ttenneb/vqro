//! Core in-tree extensions installed and enabled by default.
//!
//! The tasks provider/pane, the decision queue and messaging all live in the
//! trusted `vqro.tasks` legacy plugin, and the Pi reply-routing adapter plus the
//! agent-conventions skill ship with the binary. A plain `vqro` launch writes
//! those embedded files into the Vqro config directory and links the plugin, so
//! the core experience needs no manual linking.
//!
//! Nothing here touches another product's state: everything is rooted at this
//! binary's own config directory, and the Pi adapter/skill are only offered to
//! Pi processes Vqro itself starts in its panes.

use std::io;
use std::path::{Path, PathBuf};

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

struct BundledPaths {
    tasks_dir: PathBuf,
    pi_adapter: PathBuf,
    pi_skill: PathBuf,
}

fn bundled_root() -> PathBuf {
    crate::config::config_dir().join("bundled")
}

fn paths() -> BundledPaths {
    let root = bundled_root();
    BundledPaths {
        tasks_dir: root.join("tasks"),
        pi_adapter: root.join("pi").join("vqro-reply-routing.ts"),
        pi_skill: root.join("pi").join("skills").join("vqro").join("SKILL.md"),
    }
}

/// Write `body` unless the file already has exactly that content.
///
/// A plain launch can attach to a server that is already serving these files, so
/// skipping identical writes avoids racing a provider's `exec` with a truncate.
fn write_if_changed(path: &Path, body: &str) -> io::Result<()> {
    if std::fs::read(path).ok().as_deref() == Some(body.as_bytes()) {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, body)
}

/// Materialize the embedded extensions and link the tasks plugin if it is absent.
pub(crate) fn ensure_defaults() -> io::Result<()> {
    let paths = paths();
    write_if_changed(&paths.tasks_dir.join("vqro-extension.toml"), TASKS_MANIFEST)?;
    for &(name, body) in TASKS_FILES {
        write_if_changed(&paths.tasks_dir.join(name), body)?;
    }
    write_if_changed(&paths.pi_adapter, PI_ADAPTER)?;
    write_if_changed(&paths.pi_skill, VQRO_SKILL)?;
    link_tasks_plugin(&paths.tasks_dir)
}

/// Link the bundled plugin only when no `vqro.tasks` is registered.
///
/// A developer who linked their own tasks checkout keeps that link; this never
/// replaces an existing registration.
fn link_tasks_plugin(tasks_dir: &Path) -> io::Result<()> {
    if crate::persist::plugin_registry::load()
        .iter()
        .any(|plugin| plugin.plugin_id == TASKS_PLUGIN_ID)
    {
        return Ok(());
    }
    crate::plugin_paths::ensure_plugin_user_dirs(TASKS_PLUGIN_ID)?;
    let mut plugin = crate::app::load_plugin_manifest(&tasks_dir.display().to_string(), true)
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

/// Extra Pi flags that load the bundled adapter and conventions skill.
///
/// The files exist only after a plain `vqro` launch installed them; a Pi agent
/// started against a server that never saw that install still starts, without
/// them.
pub(crate) fn pi_launch_flags() -> Vec<String> {
    let paths = paths();
    if !paths.pi_adapter.is_file() || !paths.pi_skill.is_file() {
        return Vec::new();
    }
    vec![
        "--extension".into(),
        paths.pi_adapter.display().to_string(),
        "--skill".into(),
        paths.pi_skill.display().to_string(),
    ]
}
