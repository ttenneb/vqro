# QA3 upgrade and recovery corrections — 2026-11-17

## Result

User-shaped native-herdr migration now has a non-ignored regression. It writes state through the in-process
adapter, keeps the legacy v1 defaults record and a real terminal view/tab, then starts CLI `serve` normally.
Actual defaults add `herdr` and the split Pi launcher; actual `prim-herdr` discovers hc itself. There is no
hand-written provider declaration or `--hc` override. Resource identity/layout survive, the New Pi agent action
returns, and an unnamed spawn uses herdr. Removing Pi remains sticky.

Provider programs remain logical names in defaults, resolving at each supervised start: running executable's
sibling, then PATH, with Windows `.exe` naming. Only unchanged installed default entries with old absolute
program paths are repaired. Edited/custom entries remain untouched. Missing programs retain actionable failure
text; hello/snapshot `backend_notice`, built-in status and recovery descriptions make absence visible. A declared
default must register before unnamed spawns; a faster alternative backend cannot accidentally receive them.

Recovery corrections:
- Snapshot/reconnect does not reopen a dismissed chooser or discard unapplied selections.
- Per-lifetime random supervisor credentials authenticate provider owners; claimed hello names alone do not.
  Registration rechecks the still-running lifetime after waits. SDKs send the credential, never publish it;
  hc's transport environment strips supervisor/core-only variables before potential service startup.
- Terminal endpoint injection includes `PRIM_SOCKET_SOURCE=terminal`. An explicit scratch `PRIM_STATE_DIR`
  overrides that ambient endpoint. Clear the marker for an intentional explicit endpoint/state pair.
- Valid running-image paths win over shadowed argv[0]; verified install symlinks and deleted-image recovery remain.
- Safe entry retains failed status/last-exit evidence. Leaving reopens configured test/embedder adapters, once.
- Private Pi E2Es assert safe-mode resume preserves Pi/bridge processes, agent identity and native effect trace.

## Evidence

Scratch evidence root: `<private-notes>`.
- `qa3-before.log`: baseline `6e9f168` fails eight new integration cases, including wrong-backend dispatch,
  touching a simulated ambient endpoint and accepting a forged supervised name.
- `qa3-green.log`: 100 unit and 12 focused integration tests pass, zero warnings.
- `qa3-full-final.json` (clean code commit `8a1bf22`): scoped `scripts/check.sh full`, with `XDG_RUNTIME_DIR=/run/user/1000` and the user-bus variable
  present: 18/18 pass; 204 Rust tests pass, 19 optional ignored in the regular suite; 112 Python tests pass;
  private optional suites, both Pi variants, scale budgets and no-leftover check pass.
- `qa3-realistic-copy-final.log`: approved one-time read-only copies of the human's workspace/resources into a
  mode-0700 private scratch root. 57-entry shape, one layout and four active identity IDs retained; identities
  remapped to private hc terminals, copied declarations disabled, new backend started through normal defaults
  and its own hc lookup. Unnamed spawn and unchanged layout verified. No source values logged, source files and
  live sockets untouched. The 136 KiB approved copy was deleted after checking no processes/sockets used it;
  private hc/test roots were torn down. No copy entered git.

Optional reproduction: set `QA3_STATE_COPY` to an explicitly approved private pair of workspace/resources files,
then run `cargo test --test herdr_provider copied_realistic -- --ignored --nocapture`. Without that opt-in input,
this case returns without accessing user state. It remaps identities: it does not establish continuity with the
human's live backend, which must never be contacted by a test.

One initial unit run hit the pre-existing store-lock release race; repeat and full gate passed. Switching the
archived baseline and working source through one Cargo target required `cargo clean -p prim` before green;
otherwise Cargo reused baseline integration artifacts. No installs, live restart, push or merge performed.
Windows suffix behavior has a unit test, not a Windows runtime run. Tmux lifetime and interpreter/bridge placement
policy remain owned separately. Work estimate: 60 minutes plus 20 minutes verification/wait; actual about
76 minutes (approximately 66 work, 10 verification/wait), delivered before the 13:15 EST deadline.
