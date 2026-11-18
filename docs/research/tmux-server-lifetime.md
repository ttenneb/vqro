# Prim-owned tmux server lifetime

Owner-confirmed application of D14: tmux owns terminal execution and native server lifetime. The provider is only a
connection. Core stop/crash, safe mode, disabling/editing an extension and replacing the prim executable must not kill
tmux terminals. No private-server destructor or watchdog remains. Configured `--tmux-socket` servers are unchanged.

## Location and identity

Without `--tmux-socket`, prim-tmux creates or reuses `<prim state directory>/tmux.sock`. New servers retain a
marked, exited `true` pane solely to keep the native control session alive; no login shell is started. That internal
pane is excluded from resources and terminal-count confirmations. Existing unmarked panes (including an older
bootstrap shell that may now contain user work) are never retroactively hidden or killed.
It never uses the default tmux
server. The actual core root comes from `hello.state_dir` when `PRIM_STATE_DIR` is not explicitly set. It must be an
absolute, short root: the native socket path is under100 bytes; an unsuitable root is rejected, not silently replaced
with an ephemeral path. This is native backend implementation, not a new endpoint format in the core contract (D22).

The native server-lifetime nonce (`@prim_control_instance`), PID and start time remain the instance identity. Provider
and core restarts keep the instance and terminal IDs. A server restart creates a new nonce and new terminal IDs even
when tmux reuses `%0`. Lost output/OSC status is still lossy; reconnect supplies repaint, never input/history replay.

## Controls

The independent **tmux-server** extension is enabled by default. It keeps `tmux.server.status` and `tmux.server.stop`
registered, including while the tmux provider is disconnected or disabled. With no owned server they report “No
prim-owned tmux server running”. A **tmux server** status-menu item appears only while that server exists. It opens a
live panel with the terminal count and a Stop action using the existing generic menu/input primitives; settings has no
tmux-specific code. Only server-presence transitions change contributions; changing counts use `view.publish`.
The extension resolves the owned endpoint once and stats it every five seconds. No helper is spawned while the path
is absent. A stale socket is queried twice before caching absence until its filesystem identity changes; normal tmux
restarts replace that endpoint. Explicit commands always query immediately and invalidate cached absence on success.
A new endpoint is discovered at the next polling tick, without a per-second helper process when tmux is unused.

Stop asks the user to type `STOP N` and names all N currently observed terminals. Changed instance/count requires fresh
confirmation. Before the native action the extension transactionally disables enabled direct `prim-tmux` provider
entries without `--tmux-socket`, so a future core start will not recreate the server. Custom wrapper argv cannot be
identified reliably and must be disabled explicitly. Configured external servers are neither disabled nor killed.
The helper checks the instance and count again, then fences kill-server by the nonce on the same native server queue;
it never redirects a stale confirmation to a replacement server. Concurrent native clients are not frozen: the count
is a current observation, and the confirmed action stops the entire server.

The helper reports native acceptance, not a remote-delivery guarantee. A timeout/error after submission can be
indeterminate; never automatically retry a stop. A still-running provider waits after server loss without creating a
replacement. Enabling/restarting the provider creates an absent owned server again, as requested.

### Recovery with extensions off / core unavailable

Safe mode intentionally runs no control extension. Use the direct helper with the explicit state root:

```
prim-tmux server status --state-dir /absolute/prim-state
prim-tmux server stop --state-dir /absolute/prim-state
```

The second command prints the count and reads `STOP N` from stdin. `--tmux BIN` selects a matching native tmux CLI.
For programmatic confirmed calls, supply both `--expect-instance INSTANCE` and `--confirm-terminals N` from status.
Direct CLI stop does not rewrite workspace policy: also disable the tmux provider if it should remain off on future
core/provider starts. No running core is required when `--state-dir` is supplied. Native tmux may leave a stale socket
file after shutdown; status queries native liveness, not just filesystem existence.

## Cutover: legacy private servers

**Do not reinstall/stop an old provider with valuable live private tmux terminals without an intentional cutover.**
The old binary uses a temporary socket and kills its server on exit (including through its old watchdog). This patch
cannot retrospectively remove that behavior, and does not automatically adopt/migrate legacy servers. Finish/migrate
that work first or explicitly approve its shutdown; then enable the new provider. Existing configured external tmux
servers need no migration. No live install or live-session migration is part of this change.

## Evidence

`tests/tmux_provider.rs` covers provider disconnect, SIGKILL and inherited lifetime EOF with explicitly stopped test
servers. `tests/cases/tmux_lifetime.rs` additionally exercises actual-core disable/enable, safe mode, provider/core
crash, reinstall-like core replacement, stable IDs, changed server IDs and management while the provider is disabled.
`tests/test_tmux_server.py` covers absent-server UI, confirmation, stale count/instance rejection, private-only provider
disable, menu/live separation and bounded helper timeout uncertainty. All fixtures use private roots and sockets.

Possible future generic mechanism: command unregister/availability would let an extension withdraw palette commands
without disconnecting; this release intentionally adds no core API.
