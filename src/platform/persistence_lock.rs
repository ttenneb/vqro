use std::path::Path;

#[allow(dead_code)] // Used when the staged persistence coordinator is wired into production.
#[derive(Debug)]
pub(crate) enum PersistenceLockError {
    Busy,
    Io(std::io::Error),
}

impl std::fmt::Display for PersistenceLockError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Busy => formatter.write_str("persistence ownership is held by another process"),
            Self::Io(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for PersistenceLockError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Busy => None,
            Self::Io(error) => Some(error),
        }
    }
}

#[derive(Debug)]
pub(crate) struct PersistenceLockGuard {
    _platform: super::PlatformPersistenceLock,
}

#[allow(dead_code)] // Used when the staged persistence coordinator is wired into production.
pub(crate) fn try_lock_persistence(
    path: &Path,
) -> Result<PersistenceLockGuard, PersistenceLockError> {
    super::try_lock_platform_persistence(path)
        .map(|guard| PersistenceLockGuard { _platform: guard })
}
