# Adversarial review of early research (2026-11-16)

Reviewer: a subagent with read access to the clones and binaries. Test scripts are in `.run/` (not committed).
VERIFIED = checked by source or command; REASONED = argument only.
Finding 2 was re-checked independently afterwards and confirmed.

## Findings (ranked)

1. **Status on tmux/RMUX is lost under load, and nothing on the server keeps it.** VERIFIED
   - Slow control client, 3 MB of output: tmux delivered ~17 KB in 12 s and never delivered
     the OSC 7501 report. RMUX dropped the control client after ~343 KB.
   - `capture-pane -e` drops 7501, so prim cannot recover the state.
   - A control client gets `%output` only for its attached session. A second session gave nothing.
   - OSC sequences can be split across `%output` lines, so the parser must keep state per pane.
   - Impact: prim would need a daemon that holds a client per session and stores status,
     or a PTY wrapper (`prim exec`), or must declare status unsupported on these backends.
2. **tmux and RMUX need separate control-mode dialects.** VERIFIED (re-checked)
   - RMUX has no `refresh-client -B` and no `pause-after`.
3. **Shared verb names, different meanings.** VERIFIED
   - send: hc pastes, waits for human quiet, then presses Enter, and refuses a blocked program.
     Rex types the text. tmux sends raw keys immediately.
   - wait: hc returns on any non-working status (a program with no status counts as working
     until it exits; a timeout is a check-in). Rex `--wait` waits for exit. tmux has none.
   - parent/child: hc sets the parent implicitly from the caller's environment, and children
     die with their parent. hc types notes into parent agents. The others have nothing like this.
   - events: hc has seq, heartbeat and `missed` resync. Rex documents no seq or replay.
     tmux/RMUX lose events under load.
4. **herdr-core is already an opinionated agent runtime.** VERIFIED
   - It has templates, `machine.exec`, file read/write/edit, caller identity, notes and remote
     machines. A lowest-common-denominator wrapper would hide most of that.
5. **The spikes prove less than the doc claims.** VERIFIED
   - hc_spike read the wrong ID field (`address` is the real one) and fell back to names.
   - cm_spike never printed the `@prim_role` read-back.
   - Output from all panes was merged into one buffer, so status was never attributed to a pane.
   - Fixed sleeps throughout. Wait was not tested on tmux, RMUX or Rex.
   - REASONED: strict UTF-8 decoding could kill the reader thread on a split multibyte character.
6. **Metadata isn't common either.** VERIFIED
   - Rex block labels are a single string of up to 128 characters. hc labels are key=value.
     tmux has `@options`, but RMUX can't subscribe to changes.
7. **Don't write into the `herdr-app` document.** VERIFIED
   - rodeo's `Document` has no catch-all for unknown fields and no version, so prim keys would
     be dropped. It's a UI document, not a neutral layout. Use a separate `prim` document.
8. **Rex through its CLI won't hold up for a long-lived adapter.** VERIFIED/REASONED
   - Every call opens a new client. No seq or replay on events. The protocol is undocumented
     and changing fast. A prim client competes with the human's client for block ownership.
     Licence and terms are unknown.
9. **Gaps not yet considered.** REASONED unless marked
   - Trust: a backend socket gives shell access, so an agent with prim access can drive any
     other agent, including answering its permission prompts.
   - Caller identity: `HC_TERMINAL_ID`, `TMUX_PANE`, Rex unknown. prim needs a "who am I".
   - Multiple writers, how agents call prim (CLI or library), Windows.
   - ID persistence: hc keeps `t1…` across a restart (VERIFIED). tmux `%N` IDs reset.
   - Which agents besides Claude Code emit 7501 is still unverified.

## Questions to answer before designing primitives

1. Is prim a protocol (hc-shaped, other backends adapted to it) or a lowest-common-denominator wrapper?
2. Who owns status and event history on tmux/RMUX: a prim daemon, a `prim exec` wrapper, or nobody?
3. What do `send` and `wait` mean in prim? Chosen per call, or fixed?
4. What is the trust and identity model when agents call prim?
5. Is Rex in scope before its licence, protocol docs and API stability are known?
