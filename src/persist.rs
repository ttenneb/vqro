//! Session persistence — save/restore workspaces, layouts, and working directories.
//!
//! Stored at `~/.config/herdr/session.json`.
//! Optional pane screen history is stored separately at `session-history.json`.
//! Installed plugins are persisted separately at `plugins.json`.

// Kept separate from ordinary session saves so rejected bytes remain authoritative.
mod convert_v7;
pub(crate) mod coordinator;
pub(crate) mod extension_state;
mod io;
pub mod plugin_registry;
mod recovery;
mod restore;
mod snapshot;
mod view_aliases;

#[cfg(test)]
pub use self::io::load;
#[cfg(all(test, unix))]
pub(crate) use self::io::publish_handoff_snapshot_to_path;
pub use self::io::{clear, clear_history, save};
#[cfg(unix)]
pub(crate) use self::io::{durable_collections_action_sequence, stage_handoff_snapshot};
pub(crate) use self::io::{
    load_for_startup, load_history_for_startup, StartupHistoryLoad, StartupSessionLoad,
};
#[cfg(unix)]
pub use self::restore::restore_handoff;
pub(crate) use self::restore::restored_worktree_space_membership;
#[cfg(unix)]
pub(crate) use self::restore::validate_snapshot_admission;
pub use self::restore::{restore, restore_delegations};
#[cfg(any(unix, test))]
pub(crate) use self::snapshot::terminal_id_for_pane_snapshot;
#[cfg(any(unix, test))]
pub(crate) use self::snapshot::LEGACY_SNAPSHOT_WRITER_VERSION;
pub use self::snapshot::{
    capture, capture_history, DirectionSnapshot, LayoutSnapshot, SessionHistorySnapshot,
    SessionSnapshot, TabSnapshot, WorkspaceSnapshot,
};
#[cfg(unix)]
pub(crate) use self::snapshot::{deserialize_handoff_snapshot, normalize_identity_changes};
pub(crate) use self::snapshot::{
    parse_snapshot, reserve_empty_collection, validate_snapshot, PREDECESSOR_SNAPSHOT_VERSION,
    SNAPSHOT_VERSION,
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
    for parent in [paths.session.parent(), paths.extension_state.parent()]
        .into_iter()
        .flatten()
    {
        std::fs::create_dir_all(parent).map_err(|source| coordinator::PersistenceError {
            source,
            outcome: coordinator::PersistenceOutcome::NotPublished,
        })?;
    }
    let ownership = coordinator::PersistenceOwnership::try_acquire(&paths)?;
    coordinator::recover_checkpoint_at_startup(&ownership, &paths)
}

pub(crate) fn persist_collections_checkpoint(
    session: &SessionSnapshot,
    extension_state: &crate::extension_state::DurableExtensionStateV1,
) -> Result<(), coordinator::PersistenceError> {
    let paths = collections_checkpoint_paths();
    persist_collections_checkpoint_at(&paths, session, extension_state)
}

#[cfg_attr(test, allow(dead_code))]
pub(crate) fn persist_collections_checkpoint_at(
    paths: &coordinator::PersistencePaths,
    session: &SessionSnapshot,
    extension_state: &crate::extension_state::DurableExtensionStateV1,
) -> Result<(), coordinator::PersistenceError> {
    let ownership = coordinator::PersistenceOwnership::try_acquire(paths)?;
    coordinator::persist_checkpoint(&ownership, paths, session, extension_state)
}
#[cfg(all(test, unix))]
pub(crate) use self::snapshot::{PaneAgentSessionSnapshot, PaneSnapshot};
