use std::io::{Read, Write};
use std::path::Path;

use crate::extension_state::{DurableExtensionStateV1, MAX_STATE_STORE_BYTES};

#[derive(Debug)]
pub(crate) enum ExtensionStateLoad {
    Missing,
    Loaded(DurableExtensionStateV1),
    Rejected { bytes: Vec<u8>, error: String },
}

#[derive(Debug)]
pub(crate) struct ExtensionStateSaveError {
    pub(crate) source: std::io::Error,
    /// The authoritative path was already replaced; callers must not continue from stale memory.
    pub(crate) replaced: bool,
}

impl std::fmt::Display for ExtensionStateSaveError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.source.fmt(formatter)
    }
}

pub(crate) fn load(path: &Path) -> ExtensionStateLoad {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return ExtensionStateLoad::Missing;
        }
        Err(error) => {
            return ExtensionStateLoad::Rejected {
                bytes: Vec::new(),
                error: error.to_string(),
            };
        }
    };
    if file
        .metadata()
        .is_ok_and(|metadata| metadata.len() > MAX_STATE_STORE_BYTES)
    {
        return ExtensionStateLoad::Rejected {
            bytes: Vec::new(),
            error: "extension state exceeds the maximum encoded size".into(),
        };
    }
    let mut bytes = Vec::new();
    if let Err(error) = file
        .take(MAX_STATE_STORE_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
    {
        return ExtensionStateLoad::Rejected {
            bytes,
            error: error.to_string(),
        };
    }
    if bytes.len() as u64 > MAX_STATE_STORE_BYTES {
        return ExtensionStateLoad::Rejected {
            bytes,
            error: "extension state exceeds the maximum encoded size".into(),
        };
    }
    let store = match serde_json::from_slice::<DurableExtensionStateV1>(&bytes) {
        Ok(store) => store,
        Err(error) => {
            return ExtensionStateLoad::Rejected {
                bytes,
                error: error.to_string(),
            };
        }
    };
    if let Err(error) = store.validate() {
        return ExtensionStateLoad::Rejected {
            bytes,
            error: format!("{error:?}"),
        };
    }
    ExtensionStateLoad::Loaded(store)
}

pub(crate) fn save_atomically(
    path: &Path,
    store: &DurableExtensionStateV1,
) -> Result<(), ExtensionStateSaveError> {
    store.validate().map_err(|error| ExtensionStateSaveError {
        source: std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("invalid extension state: {error:?}"),
        ),
        replaced: false,
    })?;
    let mut encoded =
        serde_json::to_vec_pretty(store).map_err(|source| ExtensionStateSaveError {
            source: source.into(),
            replaced: false,
        })?;
    encoded.push(b'\n');
    if encoded.len() as u64 > MAX_STATE_STORE_BYTES {
        return Err(ExtensionStateSaveError {
            source: std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "extension state exceeds the maximum encoded size",
            ),
            replaced: false,
        });
    }
    let parent = path.parent().ok_or_else(|| ExtensionStateSaveError {
        source: std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "extension state path has no parent",
        ),
        replaced: false,
    })?;
    std::fs::create_dir_all(parent).map_err(|source| ExtensionStateSaveError {
        source,
        replaced: false,
    })?;
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temporary = path.with_extension(format!("tmp-{}-{nonce}", std::process::id()));
    let write_result = (|| -> Result<(), ExtensionStateSaveError> {
        let mut file =
            crate::platform::create_private_state_file(&temporary).map_err(|source| {
                ExtensionStateSaveError {
                    source,
                    replaced: false,
                }
            })?;
        file.write_all(&encoded)
            .and_then(|()| file.sync_all())
            .map_err(|source| ExtensionStateSaveError {
                source,
                replaced: false,
            })?;
        drop(file);
        crate::platform::replace_file(&temporary, path).map_err(|source| {
            ExtensionStateSaveError {
                source,
                replaced: false,
            }
        })?;
        crate::platform::sync_parent_directory(parent).map_err(|source| ExtensionStateSaveError {
            source,
            replaced: true,
        })
    })();
    if write_result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    write_result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_round_trip_and_rejected_bytes_are_preserved() {
        let directory = std::env::temp_dir().join(format!(
            "vqro-extension-state-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("extension-state.json");
        let store = DurableExtensionStateV1::new("session:test".into(), 1).unwrap();
        save_atomically(&path, &store).unwrap();
        let ExtensionStateLoad::Loaded(loaded) = load(&path) else {
            panic!("store should load");
        };
        assert_eq!(loaded, store);

        std::fs::write(&path, b"not-json").unwrap();
        let ExtensionStateLoad::Rejected { bytes, error } = load(&path) else {
            panic!("corrupt bytes should be rejected");
        };
        assert_eq!(bytes, b"not-json");
        assert!(!error.is_empty());
        assert_eq!(std::fs::read(&path).unwrap(), b"not-json");
        std::fs::remove_dir_all(directory).unwrap();
    }
}
