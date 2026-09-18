//! Session persistence — save/restore workspaces, layouts, and working directories.
//!
//! Stored at `~/.config/herdr/session.json`.
//! Optional pane screen history is stored separately at `session-history.json`.
//! Installed plugins are persisted separately at `plugins.json`.

// Kept separate from ordinary session saves so rejected bytes remain authoritative.
pub(crate) mod coordinator;
pub(crate) mod extension_state;
mod io;
pub mod plugin_registry;
mod restore;
mod snapshot;

#[cfg(test)]
pub use self::io::load;
#[cfg(test)]
pub(crate) use self::io::publish_handoff_snapshot_to_path;
#[cfg(unix)]
pub(crate) use self::io::stage_handoff_snapshot;
pub use self::io::{clear, clear_history, save};
pub(crate) use self::io::{
    load_for_startup, load_history_for_startup, StartupHistoryLoad, StartupSessionLoad,
};
pub(crate) use self::restore::validate_snapshot_admission;
#[cfg(unix)]
pub use self::restore::{handoff_pane_aliases, restore_handoff};
pub use self::restore::{restore, restore_delegations};
pub use self::snapshot::{
    capture, capture_history, DirectionSnapshot, LayoutSnapshot, SessionHistorySnapshot,
    SessionSnapshot, TabSnapshot, WorkspaceSnapshot,
};
pub(crate) use self::snapshot::{
    normalize_identity_changes, parse_snapshot, terminal_id_for_pane_snapshot, validate_snapshot,
    LEGACY_SNAPSHOT_WRITER_VERSION, SNAPSHOT_VERSION,
};

pub(crate) fn collections_checkpoint_paths() -> coordinator::PersistencePaths {
    let directory = crate::session::data_dir();
    coordinator::PersistencePaths {
        session: directory.join("session.json"),
        extension_state: directory.join("extension-state-v1.json"),
        journal: directory.join("session-extension-state.journal.json"),
    }
}

pub(crate) fn recover_collections_checkpoint(
) -> Result<coordinator::StartupRecovery, coordinator::PersistenceError> {
    let paths = collections_checkpoint_paths();
    let ownership = coordinator::PersistenceOwnership::try_acquire(&paths)?;
    coordinator::recover_checkpoint_at_startup(&ownership, &paths)
}

pub(crate) fn persist_collections_checkpoint(
    session: &SessionSnapshot,
    extension_state: &crate::extension_state::DurableExtensionStateV1,
) -> Result<(), coordinator::PersistenceError> {
    let paths = collections_checkpoint_paths();
    let ownership = coordinator::PersistenceOwnership::try_acquire(&paths)?;
    coordinator::persist_checkpoint(&ownership, &paths, session, extension_state)
}
#[cfg(test)]
pub(crate) use self::snapshot::{PaneAgentSessionSnapshot, PaneSnapshot};
