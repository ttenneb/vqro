# tmux provider, provider side (2026-11-17)

The tmux backend as an out-of-process provider against `spec/provider-v0.md` section 9 (pinned), built on the
standalone `prim::tmux` client (`tmux-client.md`). Tested against an in-test fake core and a private tmux 3.7b
server; not yet against the real core, whose section 9 side is still being built (branch `provider-wire`).

## Pieces

- `src/provider_client.rs`: a small Rust provider-side helper, the counterpart of `extensions/prim_provider.py`.
  - **Registration:** `hello`, then exactly one `provider.register`. The generation goes on every later message in
    both directions: core requests with a wrong generation get `stale_generation`, and a core notification with a
    wrong generation ends the connection.
  - **Sync and upserts:** syncs are chunked under the record limit, and upserts are refused until the first sync
    has started.
  - **Requests:** `effect.invoke` and `stream.open` arrive as `Request`s on a channel, each with exactly one reply.
    A dropped request replies `backend` "outcome unknown". The provider never sends `indeterminate`.
  - **Effects:** `deadline_ms` becomes a local monotonic deadline. Duplicate effect ids are `conflict` (bounded
    ledger), and undeclared ops are `unsupported`.
  - **Streams:** a stream waits for its open reply, never has more than the window outstanding, and numbers decoded
    bytes contiguously from 0. A gap or close ends it.
  - **Commands and bounds:** input, resize and close commands arrive in order. Bad input gets `stream.error
    {outcome: rejected}`. Inbound records are capped at 1 MiB, and the outgoing queue is byte-budgeted.
- `src/bin/prim-tmux.rs`: the provider. `prim-tmux [--tmux-socket PATH] [--session NAME] [--tmux BIN] [--name NAME]`,
  connecting to `$PRIM_SOCKET`. It honours the extension lifetime channel. Run it as an extension, for example
  `extension/tmux {argv: ["prim-tmux", "--tmux-socket", "/path"]}` (an `essential` provider once D21 lands).

## Behaviour

- **Server.** With `--tmux-socket` it attaches to an existing server and never kills it. Without it, it starts a
  private server under `/tmp` and kills that server on exit. It never touches the default tmux server.
- **Identity.** `instance` is the `prim::tmux` server identity: pid, start time and a server-lifetime nonce. Pane ids
  are only reused by a new server, which has a new instance, so `incarnation` is always `""`.
- **Resources.** Each pane is a `terminal` resource:
  - `name` (window name), `title`, `cwd`;
  - `argv` when prim spawned the pane, else `[]`;
  - `data {session, window, command, pid}`.
- **Topology.** One control client per session, because tmux sends a client output only for panes linked into its
  session. New sessions are attached on the next reconcile.
- **Reconcile.** Panes are re-listed (`list-panes -a`) after structural notifications and every 3 s, because tmux
  has no event history and no pane-exited notification. A vanished pane is `gone` with reason `killed` if this
  provider killed it, else `exited`. Exit codes are not available (tmux removes exited panes).
- **Status.** OSC 7501 is observed in each pane's output and parsed per the Program Status Protocol rev 0.4:
  `key=value` pairs joined by `:`, with `msg` and `title` in base64. Each report replaces its record (the root, or a
  child by `id` under `records`), and `state=clear` removes records. Capability `status.osc7501 {stored: false,
  lossy: true}`: tmux stores nothing, and status is lost while a pane is paused.
- **Effects.**
  - `terminal.spawn` runs `new-window -d` in the spawn session, with `name`, `cwd` and `env` (`-e`). An empty `argv`
    uses tmux's `default-shell`, which is the shell of the tmux server's environment, not necessarily the provider's.
  - `terminal.kill` runs `kill-pane` and replies `observed` with finished facts; a `resource.gone {killed}` follows.
  - Outcomes: a tmux rejection is the `rejected` outcome. A lost acknowledgement is a JSON-RPC `backend` error,
    "outcome unknown". Nothing is resent.
- **Streams.**
  - **Initial data:** always `repaint`, a visible-screen capture prefixed with `ESC[H ESC[2J`, never replayed
    history. `history_lines` is ignored (capability `terminal.output {history: false, repaint: "screen"}`).
  - **Input:** goes in at most 16 KiB `send-keys -H` chunks. If its outcome is unknown, the stream closes rather
    than claiming rejection.
  - **Resize:** sets the control client's size for the pane's window (`refresh-client -C`, latest viewer wins).
- **Gaps.** A stream ends with `stream.gap` when:
  - tmux pauses the pane (`paused`); the pane is then resumed (capture, continue);
  - this provider's subscriber lags because the core returns credit slower than the pane produces output
    (`viewer_slow`);
  - the session's control client is dropped (`backend_dropped`).

  The viewer recovers with a fresh `stream.open`, which starts with a new repaint.

## Findings

1. **A tmux command that times out behind an output flood drops that session's whole control client.** This is the
   `prim::tmux` policy: a timeout ends the attachment and never resends. When it happens:
   - every stream of that session ends with `backend_dropped`;
   - status for its panes stops until the next reconcile re-attaches the session (within 3 s) and restarts the
     watchers.

   Seen by stalling the provider (SIGSTOP) for 3 s during a flood. Across runs the stream ended with `paused`,
   `viewer_slow` or `backend_dropped`; recovery worked every time.

   **Since 2026-11-17 (tmux-timeout):** read-only command timeouts are no longer fatal (`tmux-client.md`,
   Timeouts). A reconcile `list-panes` or a repaint capture stuck behind a flood now fails alone, and the session's
   control client stays. A mutating command (spawn, kill, input) that times out still drops it, because its
   outcome is unknown.
2. **tmux format output replaces tabs, and without a UTF-8 locale also non-ASCII bytes, with `_`.** The pane list
   therefore uses an ASCII separator token. A title or path that contains that token would be mis-split (unlikely,
   not handled).
3. **Killing prim-tmux with SIGKILL leaves its control clients attached** until the server notices the dead pipes
   (tmux control clients pass their stdin and stdout to the server). In the tests the private server is killed by
   pid as a fallback. In the field, a crashed provider's attachments end when tmux sees EOF on those pipes
   (unverified timing).

## Spec and brief notes (no divergence)

- The brief asked for `events.resync false` and `layout.native true`. Section 9.2 makes non-object capability values
  `invalid` (absent means unsupported), so `events.resync` is absent and `layout.native` is `{}`.
- Extra parameters, all allowed by 9.2 since unknown names and parameters are accepted:
  - `terminal.exit {exit_code: false}`;
  - `terminal.spawn {env, cwd, name}`;
  - `identity {refs_survive_backend_restart: false}`.

## Reproduce

```sh
T=$HOME/.local/share/herdr/scratch/prim-scale-target
env -i HOME=$HOME PATH=$PATH CARGO_TARGET_DIR=$T cargo test --test provider_client
env -i HOME=$HOME PATH=$PATH CARGO_TARGET_DIR=$T cargo test --test tmux_provider -- --ignored
```

The opt-in tests need `/usr/bin/tmux` 3.7b. Each uses its own `/tmp` socket and private `HOME`, kills only its own
server (by pid if `kill-server` fails), and stops and resumes only its own provider process.
