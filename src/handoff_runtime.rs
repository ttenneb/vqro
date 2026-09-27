#[cfg(unix)]
use serde::{Deserialize, Serialize};
#[cfg(unix)]
use std::os::fd::OwnedFd;

/// Long-lived pane runtime transferred during server replacement.
///
/// Handoff preserves server-owned session state such as PTYs, processes, agent
/// identity, and durable plugin/session metadata. It intentionally does not
/// preserve transient coordination such as in-flight requests, waits,
/// subscriptions, client sockets, or pane-to-pane messages; clients reconnect
/// and retry those operations after replacement.
#[cfg(unix)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct HandoffRuntimeState {
    pub pane_id: u32,
    /// Present in handoff v2. Kept raw until the bounded manifest and snapshot
    /// bijection have been validated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_id: Option<String>,
    pub child_pid: u32,
    pub rows: u16,
    pub cols: u16,
    pub cell_width_px: u32,
    pub cell_height_px: u32,
    #[serde(default)]
    pub keyboard_protocol_flags: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keyboard_protocol_ansi: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_state: Option<crate::pane::InputState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initial_history_ansi: Option<String>,
}

#[cfg(unix)]
impl HandoffRuntimeState {
    pub fn with_pane_id(mut self, pane_id: crate::layout::PaneId) -> Self {
        self.pane_id = pane_id.raw();
        self
    }

    pub fn with_terminal_id(mut self, terminal_id: &crate::terminal::TerminalId) -> Self {
        self.terminal_id = Some(terminal_id.as_str().to_owned());
        self
    }
}

#[derive(Debug)]
pub(crate) struct ImportedHandoffRuntime {
    #[cfg(unix)]
    pub master_fd: OwnedFd,
    #[cfg(unix)]
    pub state: HandoffRuntimeState,
}
