//! Best-effort rolling recovery images. These are never startup authority.
#[cfg(test)]
#[path = "recovery_tests.rs"]
mod tests;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const INTERVAL: Duration = Duration::from_secs(15 * 60);
const LIMIT: usize = 48;

pub(super) fn preserve(path: &Path) {
    if let Err(error) = preserve_at(path, SystemTime::now()) {
        tracing::warn!(%error, path = %path.display(), "could not preserve session recovery image");
    }
}

fn preserve_at(path: &Path, now: SystemTime) -> io::Result<()> {
    let directory = path.with_file_name("session-snapshots");
    let existing = match recovery_files(&directory) {
        Ok(files) => files,
        Err(error) if error.kind() == io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(error),
    };
    if let Some((_, latest)) = existing.last() {
        // A future mtime means clock rollback, not an indefinite cadence pause.
        if now
            .duration_since(std::fs::metadata(latest)?.modified()?)
            .is_ok_and(|age| age < INTERVAL)
        {
            return Ok(());
        }
    }
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if !file.metadata()?.is_file() {
        return Err(io::Error::other("session recovery source is not a file"));
    }
    use std::io::Read;
    let mut bytes = Vec::new();
    file.take(super::snapshot::MAX_SESSION_SNAPSHOT_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > super::snapshot::MAX_SESSION_SNAPSHOT_BYTES {
        return Ok(());
    }
    let Ok(text) = std::str::from_utf8(&bytes) else {
        return Ok(());
    };
    let Ok(snapshot) = super::snapshot::parse_snapshot(text) else {
        return Ok(());
    };
    if snapshot.workspaces.is_empty() {
        return Ok(());
    }
    if let Some((_, latest)) = existing.last() {
        if let Ok(previous) = std::fs::read_to_string(latest) {
            if let Ok(previous) = super::snapshot::parse_snapshot(&previous) {
                if super::snapshot::layout_fingerprint(&snapshot).is_some_and(|binding| {
                    super::snapshot::layout_fingerprint(&previous).as_ref() == Some(&binding)
                }) {
                    return Ok(());
                }
            }
        }
    }
    std::fs::create_dir_all(&directory)?;
    let timestamp = now
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let timestamp = match existing.last() {
        Some((previous, _)) => timestamp.max(
            previous
                .checked_add(1)
                .ok_or_else(|| io::Error::other("recovery sequence exhausted"))?,
        ),
        None => timestamp,
    };
    let mut random = [0; 16];
    crate::platform::fill_secure_random(&mut random)?;
    let suffix: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
    let backup = directory.join(format!("session-{timestamp:039}-{suffix}.json"));
    let pending = backup.with_extension("pending");
    let mut published = false;
    let result = (|| {
        let mut output = crate::platform::create_private_state_file(&pending)?;
        output.write_all(&bytes)?;
        output.sync_all()?;
        drop(output);
        // Publish without replacing any preexisting file, even on collision.
        std::fs::hard_link(&pending, &backup)?;
        published = true;
        crate::platform::sync_containing_directory(&backup)?;
        std::fs::remove_file(&pending)?;
        crate::platform::sync_containing_directory(&directory)?;
        prune(&existing, LIMIT)?;
        crate::platform::sync_containing_directory(&backup)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&pending);
        if published {
            let _ = std::fs::remove_file(&backup);
            let _ = crate::platform::sync_containing_directory(&backup);
        }
    }
    result
}

fn recovery_files(directory: &Path) -> io::Result<Vec<(u128, PathBuf)>> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some(fields) = name
            .strip_prefix("session-")
            .and_then(|s| s.strip_suffix(".json"))
        else {
            continue;
        };
        let Some((stamp, suffix)) = fields.split_once('-') else {
            continue;
        };
        if stamp.len() == 39
            && stamp.bytes().all(|b| b.is_ascii_digit())
            && suffix.len() == 32
            && suffix
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            if let Ok(stamp) = stamp.parse() {
                files.push((stamp, entry.path()));
            }
        }
    }
    files.sort();
    Ok(files)
}

fn prune(older: &[(u128, PathBuf)], keep: usize) -> io::Result<()> {
    let mut remaining = older.len().saturating_sub(keep.saturating_sub(1));
    let mut failure = None;
    for (_, path) in older {
        if remaining == 0 {
            return Ok(());
        }
        match std::fs::remove_file(path) {
            Ok(()) => remaining -= 1,
            Err(error) => failure = Some(error),
        }
    }
    if remaining > 0 {
        if let Some(error) = failure {
            return Err(error);
        }
    }
    Ok(())
}
