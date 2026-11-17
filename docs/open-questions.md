# Open questions

prim is a working prototype, and fluid. This list is ranked by how much an answer could change the primitives.
Close a question by linking the decision, commit or experiment that answered it.

## Open
1. **Agent input semantics.** When does mid-turn input take effect in each runtime, and which Claude Code route
   should the adapter use? (`experiments/e1-agent-input.md`, roadmap step 1)
2. **Remote bridges.** Proposed (D21, `spec/provider-v0.md` section 6): bridges reach the core over the local
   socket with a one-time token, and from other hosts through their host's provider (`provider.relay`). Human to
   confirm, or choose a small per-host relay, or local bridges only for now. Still open: what an agent view needs
   beyond today's panels.
3. **Capability granularity.** How fine should backend capabilities be before extensions fill up with special
   cases? Answered only by a second backend (roadmap step 2). First cut: `spec/provider-v0.md` section 3.
4. **Undo of extension-made changes.** The default revert skips extension writes, so a theme or keymap the user
   picked through an extension can't be undone with `u`. Attribute user-initiated extension writes to the user?
5. **Shared versus per-client settings.** Sidebar view, icons, theme and prefix live in workspace docs shared by every
   UI. Some may want to be per client (a terminal without Nerd Fonts, a light-background terminal).
6. **Workspace authority.** Grant granularity for agents changing layouts, commands and state (cooperative, D8).
7. **One layout on clients of different sizes.** Today each client renders the shared layout at its own size;
   terminal size follows herdr-core's latest-active-viewer rule. No prim-level rule yet.
8. **Extension state across versions.** Namespace schema versions and a snapshot at replace time, so rollback
   restores data as well as code.
9. **Core protocol shape for third parties.** Generated schema and a compatibility gate (candidate rules in
   `research/protocols-and-ui.md`) before anything freezes.
10. **Agent-to-agent addressing.** Node vs durable agent identity vs execution (vqro lessons). An extension
    concern; core IDs must support it.
11. **Second backend.** tmux recommended (ubiquitous, lossy status; checked on 3.7b, 2026-11-16: ids reused after a
    server restart, output dropped while paused, session-scoped output). Zellij (OSC 7501 and library mode since
    0.46) unverified. Human to confirm.
12. **Glyph widths.** Nerd Font and emoji widths vary by terminal and font; icon sets declare a width, but some
    terminals may still draw wider.
13. **Licensing and terms**, for Legal Counsel: pi_agent_rust's rider, Rex third-party-client terms, automating
    Claude Code, and prim's own licence before publishing.
14. **Providers in safe mode.** Proposed (D21): providers marked `essential` keep running in safe mode. Human to
    confirm.
15. **Provider defaults.** Default deadline for forwarded calls; slow-viewer policy (gap and repaint vs close);
    per-connection byte caps. Proposed: set them from the scale harness, not by guess.
16. **Unit of configuration for hosts.** One provider extension entry per host (`herdr@rohan`), as proposed, or one
    provider managing several hosts?

## Answered (kept for the record)
- **How extension views render:** declarative panels plus terminal views (`spec/core-protocol.md`, Panels); the
  hybrid (widgets inside a terminal stream) is untested.
- **Where prim's own state lives:** the prim store (`~/.local/state/prim/workspace.json`), through transactions;
  derived UI in live content (D15, `view.publish`).
- **Extension lifecycle:** crash budget, live replacement on entry change, safe mode (`src/ext.rs`).
- **Embedded VT:** the `vt100` crate.
- **How agents call prim:** `prim call` / `prim run` over the core socket.
- **Size rule for panes while dragging:** resize once on release (`src/tui/mod.rs`, drag handling).
