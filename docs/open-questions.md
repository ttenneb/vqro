# Open questions

vqro is a working prototype, and fluid. This list is ranked by how much an answer could change the primitives.
Close a question by linking the decision, commit or experiment that answered it.

## Open
1. **Agent input semantics.** When does mid-turn input take effect in each runtime, and which Claude Code route
   should the adapter use? (`experiments/e1-agent-input.md`, roadmap step 1)
2. **Remote bridges.** Local bridges reach the core over the local endpoint (opaque `VQRO_SOCKET`, D22 proposed)
   with a one-time token. For other hosts and WSL2 the mechanism must be backend-neutral (D23 proposed; the
   herdr-core doc relay is rejected): stdio connector (`VQRO_CONNECT`), forwarding from the vqro side, or a backend
   byte channel as an optional transport. Backlog, except that WSL needs it early. Still open: what an agent view
   needs beyond today's panels.
3. **Capability granularity.** How fine should backend capabilities be before extensions fill up with special
   cases? Answered only by a second backend (roadmap step 2). First cut: `spec/provider-v0.md` section 3.
7. **One layout on clients of different sizes.** Today each client renders the shared layout at its own size;
   terminal size follows herdr-core's latest-active-viewer rule. No vqro-level rule yet.
8. **Extension state across versions.** Namespace schema versions and a snapshot at replace time, so rollback
   restores data as well as code.
9. **Core protocol shape for third parties.** Generated schema and a compatibility gate (candidate rules in
   `research/protocols-and-ui.md`) before anything freezes.
10. **Agent-to-agent addressing.** Node vs durable agent identity vs execution (vqro lessons). An extension
    concern; core IDs must support it.
12. **Glyph widths.** Nerd Font and emoji widths vary by terminal and font; icon sets declare a width, but some
    terminals may still draw wider.
13. **External runtime/backend terms** (not changed by D25), for Legal Counsel: pi_agent_rust's rider,
    Rex third-party-client terms and automating Claude Code. vqro's own original-material licence is decided:
    Apache-2.0 with DCO sign-off/no CLA (D25); this does not relicense third parties. Nothing is published here.
16. **Unit of configuration for hosts.** One provider extension entry per host (`herdr@rohan`), as proposed, or one
    provider managing several hosts?

## Backlog (decided to wait)
- **Shared versus per-client settings.** Recommendation on file: shared by default; per-window override only for
  terminal-dependent choices (icons/glyphs, theme). Backlogged by the human 2026-11-17.
- **Keys typed while a starved TUI is behind a flood.** Rejected and reported, never delivered late (D10). Keep; a
  protocol change (input still accepted on a stream closed for slow output) is backlogged.

## Answered (kept for the record)
- **vqro's own licence/contributions:** Apache-2.0 throughout original material; DCO sign-off, no CLA (D25).
- **Undo of extension-made changes:** user-started extension writes count as the user's (D24).
- **Workspace authority:** agents may change anything; cooperative, no grants for now (D24, D8).
- **Second backend:** tmux, built as `vqro-tmux` (provider-v0, private vqro-owned server).
- **Providers in safe mode:** safe mode starts with nothing; a chooser enables providers per session (D21 revised).
- **Provider defaults:** admission limits in provider-v0 §9; load budgets set from measurements (`test-plan.md`, Scale budgets).
- **How extension views render:** declarative panels plus terminal views (`spec/core-protocol.md`, Panels); the
  hybrid (widgets inside a terminal stream) is untested.
- **Where vqro's own state lives:** the vqro store (`~/.local/state/vqro/workspace.json`), through transactions;
  derived UI in live content (D15, `view.publish`).
- **Extension lifecycle:** crash budget, live replacement on entry change, safe mode (`src/ext.rs`).
- **Embedded VT:** the `vt100` crate.
- **How agents call vqro:** `vqro call` / `vqro run` over the core socket.
- **Size rule for panes while dragging:** resize once on release (`src/tui/mod.rs`, drag handling).
