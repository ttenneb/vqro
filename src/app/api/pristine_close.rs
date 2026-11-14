//! Conditional lifecycle cleanup uses host-owned runtime admission, never a
//! caller-supplied assertion that a terminal is unused.
use super::responses::{encode_error, encode_success};
use crate::{
    api::schema::{PaneCloseIfPristineTerminalParams, PaneTarget, ResponseResult},
    app::App,
};

impl App {
    pub(super) fn handle_pristine_terminal_close(
        &mut self,
        id: String,
        params: PaneCloseIfPristineTerminalParams,
    ) -> String {
        let Some((workspace, pane)) = self.parse_current_public_pane_id(&params.pane_id) else {
            return encode_error(
                id,
                "pristine_close_rejected",
                "pane moved or is unavailable; retained",
            );
        };
        let Some(terminal_id) = self.state.workspaces[workspace].terminal_id(pane).cloned() else {
            return encode_error(
                id,
                "pristine_close_rejected",
                "pane has no terminal; retained",
            );
        };
        if terminal_id.as_str() != params.expected_terminal_id {
            return encode_error(
                id,
                "pristine_close_rejected",
                "terminal attachment changed; retained",
            );
        }
        let Some(terminal) = self.state.terminals.get(&terminal_id) else {
            return encode_error(
                id,
                "pristine_close_rejected",
                "terminal state unavailable; retained",
            );
        };
        if terminal.is_agent_terminal()
            || terminal.managed_agent_kind().is_some()
            || terminal.pending_agent_resume_plan.is_some()
            || terminal.launch_argv.is_some()
        {
            return encode_error(
                id,
                "pristine_close_rejected",
                "terminal launch/work is present; retained",
            );
        }
        let Some(runtime) = self.terminal_runtimes.get(&terminal_id) else {
            return encode_error(
                id,
                "pristine_close_rejected",
                "runtime unavailable; retained",
            );
        };
        if super::super::agents::available_shell_name(runtime).is_none() {
            return encode_error(
                id,
                "pristine_close_rejected",
                "foreground shell is busy or unknown; retained",
            );
        }
        // This owned claim closes the SAME gate used by both input admissions.
        // No mutex spans OS work or ordinary close (shutdown locks it again).
        // Failure drops only our unchanged claim and restores usable input;
        // successful shutdown invalidates the claim so Drop cannot reopen it.
        let Some(_claim) = runtime.claim_pristine_close() else {
            return encode_error(
                id,
                "pristine_close_rejected",
                "terminal is not provably fresh and unused; retained",
            );
        };
        match self.close_pane(
            id.clone(),
            &PaneTarget {
                pane_id: params.pane_id,
            },
        ) {
            Ok(()) => encode_success(id, ResponseResult::Ok {}),
            Err(response) => response,
        }
    }
}

#[cfg(all(test, unix))]
#[path = "panes/tests/pristine_close.rs"]
mod tests;
