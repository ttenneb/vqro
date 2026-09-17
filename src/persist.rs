//! Session persistence — save/restore workspaces, layouts, and working directories.
//!
//! Stored at `~/.config/herdr/session.json`.
//! Optional pane screen history is stored separately at `session-history.json`.
//! Installed plugins are persisted separately at `plugins.json`.

// Kept separate from ordinary session saves so rejected bytes remain authoritative.
pub(crate) mod extension_state;
mod io;
pub mod plugin_registry;
mod restore;
mod snapshot;

#[cfg(test)]
pub use self::io::load;
#[cfg(test)]
pub(crate) use self::io::publish_handoff_snapshot_to_path;
pub use self::io::{clear, clear_history, save};
pub(crate) use self::io::{
    load_for_startup, load_history_for_startup, publish_handoff_snapshot, StartupHistoryLoad,
    StartupSessionLoad,
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
    normalize_terminal_identities, parse_snapshot, terminal_id_for_pane_snapshot, validate_snapshot,
};
#[cfg(test)]
pub(crate) use self::snapshot::{PaneAgentSessionSnapshot, PaneSnapshot};
