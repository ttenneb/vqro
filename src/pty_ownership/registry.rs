//! Fixed-namespace ownership projection I/O. Never called by render.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

use super::{parse_record, OwnershipRecord, ServerKey, MAX_RECORD_BYTES, MAX_SERVERS};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RegistryEntry {
    pub endpoint: String,
    pub ownership: OwnershipRecord,
}

pub(crate) fn parse_entry(bytes: &[u8]) -> Result<RegistryEntry, String> {
    if bytes.len() > MAX_RECORD_BYTES {
        return Err("ownership entry exceeds bound".into());
    }
    let entry: RegistryEntry = serde_json::from_slice(bytes)
        .map_err(|error| format!("invalid ownership entry: {error}"))?;
    if entry.endpoint.is_empty()
        || entry.endpoint.len() > 4096
        || entry.endpoint.chars().any(char::is_control)
    {
        return Err("invalid ownership endpoint locator".into());
    }
    let record = serde_json::to_vec(&entry.ownership).map_err(|error| error.to_string())?;
    parse_record(&record)?;
    Ok(entry)
}

#[derive(Clone)]
pub(crate) struct RegistryDirectory {
    root: PathBuf,
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::other(message.into())
}

impl RegistryDirectory {
    pub(crate) fn existing(base: &Path) -> io::Result<Self> {
        crate::platform::validate_private_ownership_path(base, true)?;
        let parent = base.join("vqro");
        crate::platform::validate_private_ownership_path(&parent, true)?;
        let root = parent.join("pty-owners-v1");
        crate::platform::validate_private_ownership_path(&root, true)?;
        Ok(Self { root })
    }

    pub(crate) fn prepare(base: &Path) -> io::Result<Self> {
        // The OS-selected base must already exist; never create a fake runtime
        // directory or fall back to a session/config namespace.
        crate::platform::validate_private_ownership_path(base, true)?;
        let mut root = base.to_path_buf();
        for name in ["vqro", "pty-owners-v1"] {
            root.push(name);
            match crate::platform::create_private_package_directory(&root) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
            crate::platform::validate_private_ownership_path(&root, true)?;
        }
        Ok(Self { root })
    }

    pub(crate) fn record_path(&self, key: &ServerKey) -> PathBuf {
        self.root
            .join(format!("{}-{}-{}.json", key.boot_id, key.pid, key.birth))
    }

    fn lock(&self, exclusive: bool) -> io::Result<File> {
        crate::platform::validate_private_ownership_path(&self.root, true)?;
        let path = self.root.join(".lock");
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        crate::platform::private_ownership_file_options(&mut options);
        let file = options.open(&path)?;
        crate::platform::validate_private_ownership_path(&path, false)?;
        if exclusive {
            file.try_lock()
                .map_err(|error| invalid(error.to_string()))?;
        } else {
            file.try_lock_shared()
                .map_err(|error| invalid(error.to_string()))?;
        }
        Ok(file)
    }

    pub(crate) fn publish(&self, entry: &RegistryEntry) -> io::Result<()> {
        let bytes = serde_json::to_vec(entry).map_err(|error| invalid(error.to_string()))?;
        parse_entry(&bytes).map_err(invalid)?;
        let _lock = self.lock(true)?;
        let target = self.record_path(&entry.ownership.server);
        match std::fs::symlink_metadata(&target) {
            Ok(_) => crate::platform::validate_private_ownership_path(&target, false)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let temporary = self.root.join(format!(
            ".tmp-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let result = (|| {
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            crate::platform::private_ownership_file_options(&mut options);
            let mut file = options.open(&temporary)?;
            file.write_all(&bytes)?;
            file.flush()?;
            file.sync_all()?;
            drop(file);
            std::fs::rename(&temporary, &target)
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temporary);
        }
        result
    }

    #[cfg(test)]
    pub(crate) fn entries(&self) -> io::Result<Vec<RegistryEntry>> {
        self.entries_with_liveness(|key| super::ServerLiveness::Live(key.clone()))
    }

    pub(crate) fn entries_with_liveness(
        &self,
        mut observe: impl FnMut(&ServerKey) -> super::ServerLiveness,
    ) -> io::Result<Vec<RegistryEntry>> {
        let _lock = self.lock(false)?;
        let mut paths = Vec::new();
        let mut seen = 0;
        for entry in std::fs::read_dir(&self.root)? {
            seen += 1;
            if seen > MAX_SERVERS * 2 + 1 {
                return Err(invalid("ownership directory enumeration exceeds bound"));
            }
            let entry = entry?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                return Err(invalid("invalid ownership filename"));
            };
            if name == ".lock" || name.starts_with(".tmp-") {
                continue;
            }
            if !name.ends_with(".json") || paths.len() >= MAX_SERVERS {
                return Err(invalid("unknown or excessive ownership entries"));
            }
            paths.push(entry.path());
        }
        paths.sort();
        let mut entries = Vec::with_capacity(paths.len());
        let mut tty_count = 0;
        for path in paths {
            let name = path
                .file_stem()
                .and_then(|name| name.to_str())
                .ok_or_else(|| invalid("invalid ownership filename"))?;
            let mut parts = name.rsplitn(3, '-');
            let birth = parts
                .next()
                .and_then(|part| part.parse().ok())
                .ok_or_else(|| invalid("invalid ownership birth"))?;
            let pid = parts
                .next()
                .and_then(|part| part.parse().ok())
                .ok_or_else(|| invalid("invalid ownership PID"))?;
            let boot_id = parts
                .next()
                .ok_or_else(|| invalid("missing ownership boot namespace"))?
                .to_owned();
            let key = ServerKey {
                boot_id,
                pid,
                birth,
            };
            if !key.valid() {
                return Err(invalid("invalid ownership incarnation"));
            }
            match observe(&key) {
                super::ServerLiveness::Live(observed) if observed == key => {}
                super::ServerLiveness::Dead | super::ServerLiveness::Reused => continue,
                _ => return Err(invalid("ownership server liveness is unprovable")),
            }
            crate::platform::validate_private_ownership_path(&path, false)?;
            let mut options = OpenOptions::new();
            options.read(true);
            crate::platform::private_ownership_file_options(&mut options);
            let mut bytes = Vec::new();
            options
                .open(&path)?
                .take((MAX_RECORD_BYTES + 1) as u64)
                .read_to_end(&mut bytes)?;
            let entry = parse_entry(&bytes).map_err(invalid)?;
            if path != self.record_path(&entry.ownership.server) {
                return Err(invalid("ownership filename/incarnation mismatch"));
            }
            tty_count += entry.ownership.owned.len();
            if tty_count > super::MAX_OWNED_TTYS {
                return Err(invalid("aggregate owned TTY count exceeds bound"));
            }
            entries.push(entry);
        }
        Ok(entries)
    }

    pub(crate) fn remove(&self, key: &ServerKey) -> io::Result<()> {
        // Never enumerate/remove others' records or use a PID-only filename.
        let _lock = self.lock(true)?;
        let target = self.record_path(key);
        match std::fs::symlink_metadata(&target) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
            Ok(_) => {
                crate::platform::validate_private_ownership_path(&target, false)?;
                std::fs::remove_file(target)
            }
        }
    }
}
