# Socket descriptor lifetime after long-soak churn (2026-11-18)

The six-hour soak on `e1b0b37` reported about15 additional core Unix sockets per hour. A constant process count
was not proof that the peers had disappeared: a live driver thread can retain its client endpoint indefinitely.
No production core timeout or transport change is justified by the isolated evidence below.

## Exact driver ownership reproduction

Only the original soak driver's `Peer` class was extracted with Python AST; none of its setup/main code ran.
Twenty clients attached and ended a stream against an entirely private fake core, then mimicked abandoned echo
clients. The old echo loop never closed its peer on restart. The old `Peer.close` closed only `sock`; its buffered
`makefile` and bound reader thread kept the same endpoint open.

| Stage | Core total/socket fds | Driver total/socket fds | Driver threads | Core clients |
|---|---|---|---|---|
| Baseline, one control client |12/5|4/1|2|1|
|20 abandoned echo clients|32/25|24/21|22|21|
|Call the original socket-only close|32/25|24/21|22|21|
|Shutdown transport and close makefiles|12/5|4/1|2|1|

The core correctly retained connections whose client endpoints were still open; shutting down those endpoints
released all descriptors without any core restart or patch. The coordinator assigned correction of the external
soak driver to its owner, prim-scale (shutdown/reader closure and finally-cleanup on reconnect). This branch does
not edit that owner's scratch files. The old source is backed up as `soak-before-fdfix.py` in the review scratch.
The long-run series contains62 echo restarts; other driver retry paths also used socket-only close. This is not a
claim that that one counter alone exactly attributes every extra descriptor in the full six-hour series.

## Independent core-side regression matrix

`tests/fd_lifecycle.rs` launches private core processes, counts `/proc/<core>/fd` and socket symlinks, and requires
three consecutive exact-baseline observations after **each** cycle, with a bounded cleanup deadline. No allowance
is widened to tolerate descriptor growth. All child environments are cleared and use private endpoints/state,
private herdr-core and private tmux. No real Pi calls or live sessions are involved.

| Event | Cycles | Expected and final total/socket fds |
|---|---:|---|
|Viewer connect/EOF|20|12/5|
|Client write-half shutdown while read half remains held|20|12/5|
|Stream attach/close, then client EOF|20|12/5|
|Client EOF with attached idle stream|20|12/5|
|Wire provider reconnect|20|12/5|
|Gap/reattach, same provider kept alive|20|13/6|
|External provider process SIGKILL|20|12/5|
|Actual prim-herdr SIGKILL with stream attached|20|12/5 after viewer EOF|
|Private native herdr-core disconnect/reconnect|10|13/6 connected,12/5 disconnected|
|Supervised extension SIGKILL/automatic restart|20|15/6 active,12/5 after disable|
|Real TUI connect/quit through private tmux|20|12/5|

The prim-herdr test also proves the still-healthy viewer connection remains intentionally open after its provider
is killed, then disappears after its own EOF. Explicit extension restarts reset the crash budget before the next
injected kill; no rapid-kill crash-budget failure is mistaken for successful reconnect coverage. Native reconnects
assert fresh core-assigned provider generations, not a guessed change in the backend's persistent instance id.

The matrix passed on unchanged production `e1b0b37`. It is now part of `scripts/check.sh full` as the Linux-only
`fd_lifecycle` opt-in suite. Platform-specific `/proc` instrumentation does not alter the transport contract.

Evidence: `<private-notes>`: `soak-owner-before.json`, `soak-owner-before.log`, frozen
`soak-peer-before.py` and class-only repro `soak-owner-probe.py`; `matrix-final.log` records all independent phases.
The original long-soak report/series remain under the scale review scratch. A corrected-driver long soak is the
appropriate next check for any residual slow trend, not an idle timeout that evicts legitimate quiet peers.
