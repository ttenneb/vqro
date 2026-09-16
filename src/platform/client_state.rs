use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_BACKUP_ID: AtomicU64 = AtomicU64::new(1);

pub(crate) fn resolve_write_target(path: &Path) -> std::io::Result<std::path::PathBuf> {
    let mut target = path.to_path_buf();
    for _ in 0..40 {
        let metadata = match std::fs::symlink_metadata(&target) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(target),
            Err(error) => return Err(error),
        };
        if !metadata.file_type().is_symlink() {
            return Ok(target);
        }
        let link = std::fs::read_link(&target)?;
        target = if link.is_absolute() {
            link
        } else {
            target.parent().unwrap_or_else(|| Path::new(".")).join(link)
        };
    }
    Err(std::io::Error::other("too many state file symlinks"))
}

#[cfg(not(windows))]
pub(crate) fn create_private_state_file(path: &Path) -> std::io::Result<std::fs::File> {
    super::create_remote_ssh_config_file(path)
}

#[cfg(windows)]
pub(crate) fn create_private_state_file(path: &Path) -> std::io::Result<std::fs::File> {
    super::windows::create_remote_ssh_config_file(path)
}

#[cfg(not(windows))]
pub(crate) fn replace_file(source: &Path, destination: &Path) -> std::io::Result<()> {
    std::fs::rename(source, destination)
}

#[cfg(windows)]
pub(crate) fn replace_file(source: &Path, destination: &Path) -> std::io::Result<()> {
    super::windows::replace_file(source, destination)
}

#[cfg(not(windows))]
pub(crate) fn sync_parent_directory(path: &Path) -> std::io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("state path has no parent directory"))?;
    std::fs::File::open(parent)?.sync_all()
}

#[cfg(windows)]
pub(crate) fn sync_parent_directory(_path: &Path) -> std::io::Result<()> {
    // replace_file uses MOVEFILE_WRITE_THROUGH on Windows.
    Ok(())
}

/// Replace a synced temporary file and restore the previous file if publishing
/// cannot be made durable. Callers retain ownership of `source` on pre-commit
/// failure and may remove it during cleanup.
pub(crate) fn durable_replace_file(source: &Path, destination: &Path) -> std::io::Result<()> {
    let backup = destination.with_extension(format!(
        "backup.{}.{}",
        std::process::id(),
        NEXT_BACKUP_ID.fetch_add(1, Ordering::Relaxed)
    ));
    let had_previous = destination.exists();
    if had_previous {
        std::fs::copy(destination, &backup)?;
        std::fs::OpenOptions::new()
            .write(true)
            .open(&backup)?
            .sync_all()?;
    }
    if let Err(error) = replace_file(source, destination) {
        let _ = std::fs::remove_file(&backup);
        return Err(error);
    }
    if let Err(sync_error) = sync_parent_directory(destination) {
        let rollback = if had_previous {
            replace_file(&backup, destination)
        } else {
            std::fs::remove_file(destination)
        };
        let _ = sync_parent_directory(destination);
        return match rollback {
            Ok(()) => Err(sync_error),
            Err(rollback_error) => Err(std::io::Error::other(format!(
                "{sync_error}; durable replacement rollback failed: {rollback_error}"
            ))),
        };
    }
    if had_previous && std::fs::remove_file(&backup).is_ok() {
        let _ = sync_parent_directory(destination);
    }
    Ok(())
}
