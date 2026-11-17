# Explicit safe-mode recovery and replaced executables — 2026-11-17

Human decisions: D21 safe mode starts no extensions/provider links, no essential exemption; D22 confirmed.
Implementation rebased onto4e326a5 (backends-as-extensions + QA shutdown fencing). No transitional native startup/
exit hook remains. `prim serve` uses declared herdr/tmux providers; explicit native test adapters also stay stopped
in safe mode and are not offered as hidden recovery choices.

The session-only `safe_mode_allow` never writes enabled declarations/history. Allowed disabled declarations may
run temporarily; repeated healthy allowance is idempotent. Enter/re-enter clears choices, fences all old provider
sources, stops owned extensions/transport links, and leaves backend-owned terminals alone. Registration rechecks
safe-mode owner admission after token waits, alongside QA's shutdown admission check. Snapshot owner history and
kind:provider classify the Rust chooser. Herdr/tmux, descriptions, checkboxes, Enable selected and Leave safe mode
are generated locally through existing panel mechanics; prefix recovery stays reachable inside the popup.

`client::executable_path` remembers argv[0]'s resolved install path (preserving symlinks), with a Linux deleted-inode
suffix fallback; ensure_core, build identification and re-exec share it. Running-image build identity is cached,
while the TUI separately compares installed metadata to detect replacement. CLI stdout uses fallible writes and
exits quietly for BrokenPipe instead of panicking.

Evidence in `~/.local/share/herdr/scratch/prim-independent-review/`:
- `safe-stdout-before.log`: actual BrokenPipe panic before the fix; deterministic private-endpoint test is green.
- `safe-replace-before.log`: a private copy of the old installed image reproduces unlinked-inode restart failure.
  New smoke copies only into its private root, replaces that copy, and checks core restart, re-exec image and input.
- `safe-rebased-full.json`: **scripts/check.sh full PASS18/18**, zero leftovers;187 Rust passed (18 optional ignored),
  104 Python passed, zero warnings. Includes private provider/Pi/agents/UI tests, both scale probes and replacement
  TUI smoke. Final gate logs are linked from the receipt.
- Unit/socket tests cover empty startup, temporary disabled allowance without edits, idempotence, owner admission/
  revocation, restart clearing, builtin chooser grouping/visibility, prefix recovery and deleted-path normalization.

Earlier full attempt exposed an obsolete Pi fixture (--safe formerly kept its native backend) and the broad
leftover scan observing another agent's active test root; no foreign process was signalled. After integration,
manual-provider fixtures run normal mode with no autostart declarations, migration fixtures stay safe, and private
smoke installs also include sibling provider programs. The final rebased gate is authoritative.

No live prim/hc/tmux was changed or installed. Parent owns integration and cutover. Windows transport remains
unimplemented; D22 confirmation does not claim otherwise. Safe mode is recovery, not a same-user security sandbox.
