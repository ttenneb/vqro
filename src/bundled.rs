//! Neutral publication and linking of compiled default resources.
//!
//! The extension-side catalog declares content and distribution choices. The
//! host publishes those immutable generations and conservatively links declared
//! plugins, without knowing agent settings, resource policy or product names.
//!
//! Installs are conservative: a generation is written only when it is missing or
//! its content changed. A file that is already there is never rewritten (so a
//! user edit inside a published generation stays), a missing managed file is
//! restored from the embedded content, a path that is not a regular file is
//! refused, user-linked replacements and explicitly disabled plugins are left
//! as they are, and nothing is written through a symlink. Set [`OPT_OUT_ENV`] to
//! disable the defaults. All publication is rooted at this binary's config dir.

use std::io::{self, Write};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// Environment variable that disables the bundled defaults when non-empty and
/// not `0`.
pub(crate) const OPT_OUT_ENV: &str = "VQRO_NO_BUNDLED_DEFAULTS";

pub(crate) struct DefaultBundle {
    pub(crate) prefix: &'static str,
    pub(crate) assets: &'static [(&'static str, &'static str)],
    pub(crate) plugin: Option<DefaultPlugin>,
}

#[derive(Clone, Copy)]
pub(crate) struct DefaultPlugin {
    pub(crate) id: &'static str,
    pub(crate) manifest: &'static str,
    pub(crate) legacy_directory: &'static str,
}

pub(crate) fn bundled_root() -> PathBuf {
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

pub(crate) fn generation_dir(root: &Path, prefix: &str, assets: &[(&str, &str)]) -> PathBuf {
    root.join(format!("{prefix}-{}", generation_id(assets)))
}

/// Refuse to install through a symlink anywhere below `root`.
///
/// A pre-existing symlink at the root, a managed directory, a nested directory or
/// a managed file must never redirect a managed write to an unrelated file.
/// Same-user, non-adversarial scope; this only preserves unrelated data. Shared
/// by default-resource publication and trusted launch profiles.
pub(crate) fn safe_under(root: &Path, path: &Path) -> io::Result<()> {
    let relative = path.strip_prefix(root).map_err(|_| {
        io::Error::other(format!(
            "managed path escapes the managed root: {}",
            path.display()
        ))
    })?;
    if std::fs::symlink_metadata(root).is_ok_and(|meta| meta.file_type().is_symlink()) {
        return Err(io::Error::other(format!(
            "refusing to install into a symlinked directory: {}",
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
/// A missing generation is written to a staging directory and swapped in with one
/// rename. An existing generation is completed: a file that is already there is
/// left untouched (a user edit stays), a missing file is restored from the
/// embedded content, and a path that is not a regular file is refused rather than
/// counted as present. A changed embedded set has a different content address, so
/// it lands in a new directory and never rewrites a generation a provider may be
/// reading.
fn publish_generation(root: &Path, prefix: &str, assets: &[(&str, &str)]) -> io::Result<PathBuf> {
    let dir = generation_dir(root, prefix, assets);
    safe_under(root, &dir)?;
    if !dir.is_dir() {
        let staging = root.join(format!(".{prefix}.staging.{}", std::process::id()));
        safe_under(root, &staging)?;
        if staging.exists() {
            std::fs::remove_dir_all(&staging)?;
        }
        for (name, body) in assets {
            write_asset(root, &staging.join(name), &dir.join(name), body)?;
        }
        match std::fs::rename(&staging, &dir) {
            Ok(()) => {}
            // Another launch published the same generation first; use theirs.
            Err(_) if dir.is_dir() => {
                let _ = std::fs::remove_dir_all(&staging);
            }
            Err(error) => {
                let _ = std::fs::remove_dir_all(&staging);
                return Err(error);
            }
        }
    }
    for (name, body) in assets {
        let path = dir.join(name);
        safe_under(root, &path)?;
        match std::fs::symlink_metadata(&path) {
            // Present and the right type: leave it exactly as it is.
            Ok(meta) if meta.file_type().is_file() => {}
            Ok(_) => {
                return Err(io::Error::other(format!(
                    "refusing to treat a non-file as a bundled asset: {}",
                    path.display()
                )));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                write_asset(root, &path, &path, body)?;
            }
            Err(error) => return Err(error),
        }
    }
    Ok(dir)
}

pub(crate) fn ensure_defaults() -> io::Result<()> {
    if opted_out() {
        return Ok(());
    }
    let root = bundled_root();
    std::fs::create_dir_all(&root)?;
    // Publish the complete composition before touching registry links, retaining
    // the previous preparation/error order.
    let directories = crate::default_extensions::BUNDLES
        .iter()
        .map(|bundle| publish_generation(&root, bundle.prefix, bundle.assets))
        .collect::<io::Result<Vec<_>>>()?;
    for (bundle, directory) in crate::default_extensions::BUNDLES.iter().zip(directories) {
        if let Some(plugin) = bundle.plugin {
            link_default_plugin(&root, &directory, bundle.prefix, plugin)?;
        }
    }
    Ok(())
}

/// Point a declared default plugin at this generation.
///
/// The decision and the mutation happen under one registry lock, so a concurrent
/// first launch or explicit link cannot duplicate the entry between a check and a
/// write. A user-selected link is left exactly as it is, including a disabled
/// state; one of our own older generations is repointed while preserving its
/// enabled choice.
/// True when the registered root is one of our own generation directories (or the
/// older unversioned location), so it may be repointed.
///
/// The `bundled/` subtree is reserved for these generations: an arbitrary user
/// path elsewhere is never treated as ours, and a path under `bundled/` that is
/// not one of our addresses is left alone too.
pub(crate) fn owned_generation(
    root: &Path,
    registered: &str,
    prefix: &str,
    legacy_directory: &str,
) -> bool {
    let path = Path::new(registered);
    if path.parent() != Some(root) {
        return false;
    }
    if path == root.join(legacy_directory) {
        return true;
    }
    path.file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_prefix(prefix))
        .and_then(|suffix| suffix.strip_prefix('-'))
        .is_some_and(|suffix| {
            suffix.len() == 16 && suffix.chars().all(|digit| digit.is_ascii_hexdigit())
        })
}

fn link_default_plugin(
    root: &Path,
    directory: &Path,
    prefix: &str,
    declaration: DefaultPlugin,
) -> io::Result<()> {
    let current = directory.display().to_string();
    let manifest = directory.join(declaration.manifest).display().to_string();
    let candidate = crate::app::load_plugin_manifest(&manifest, true)
        .map_err(|(_, message)| io::Error::other(message))?;
    crate::plugin_paths::ensure_plugin_user_dirs(declaration.id)?;
    crate::persist::plugin_registry::update(|plugins| {
        match plugins
            .iter_mut()
            .find(|entry| entry.plugin_id == declaration.id)
        {
            Some(plugin) => {
                if plugin.plugin_root != current
                    && owned_generation(
                        root,
                        &plugin.plugin_root,
                        prefix,
                        declaration.legacy_directory,
                    )
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
    fn publish_keeps_edits_and_restores_missing_files() {
        let root = temp_root("repair");
        let _ = std::fs::remove_dir_all(&root);
        let dir = publish_generation(&root, "gen", &assets()).unwrap();
        std::fs::write(dir.join("one.py"), "edited").unwrap();
        std::fs::remove_file(dir.join("nested/two.py")).unwrap();
        let again = publish_generation(&root, "gen", &assets()).unwrap();
        // The edit stays; the deleted file is restored from the embedded content.
        assert_eq!(again, dir);
        assert_eq!(
            std::fs::read_to_string(dir.join("one.py")).unwrap(),
            "edited"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("nested/two.py")).unwrap(),
            "second"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn publish_refuses_a_non_file_asset() {
        let root = temp_root("nonfile");
        let _ = std::fs::remove_dir_all(&root);
        let dir = publish_generation(&root, "gen", &assets()).unwrap();
        std::fs::remove_file(dir.join("nested/two.py")).unwrap();
        std::fs::create_dir(dir.join("nested/two.py")).unwrap();
        assert!(publish_generation(&root, "gen", &assets()).is_err());
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
