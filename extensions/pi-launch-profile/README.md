# Pi launch/profile adapter (first Vqro compatibility slice)

`launch.mjs` is a **trusted same-user local executable extension**, not a built-in Vqro role policy. It invokes only the public `vqro pane get`, `vqro pane process-info`, and `vqro pane run` CLI operations. It never reads a role file, credential, raw session, or socket: Pi itself reads the **explicitly selected** profile through its normal `--append-system-prompt` option. The caller supplies the existing Vqro Pi status integration's absolute `--status-extension` path, loaded by Pi's normal `--extension` option. `--session`, `--model`, and `--thinking` are optional; when omitted Pi uses its own settings.

On Linux/macOS with an idle `sh`, `bash`, or `zsh` pane, invoke with Node 22+. Windows and other shells fail closed; no platform parity is claimed:

```text
node extensions/pi-launch-profile/launch.mjs \
  --pane <vqro-pane-id> --name <local-label> \
  --role-profile <absolute-pi-prompt-file> \
  --status-extension <absolute-vqro-agent-state.ts> \
  --pi-bin <absolute-pi-executable> \
  [--session <absolute-private-pi-session.jsonl>] \
  [--model <provider/id>] [--thinking <level>]
```

The adapter requires `VQRO_ENV=1`, `VQRO_SOCKET_PATH` and an absolute `VQRO_BIN_PATH` (or explicit `--vqro-bin`). It refuses a mixed `HERDR_ENV` or Herdr socket override. The target pane identity and foreground shell are checked before sending one shell-quoted command; the adapter then waits for `pane get` to show Pi activity. There is no atomic shell precondition: another actor can change the pane between check and send. A timeout means Pi **may already be running**; inspect the pane, never retry blindly. The label is local only (`name_registered: false`), not an addressed agent name. `agent start` is not used because its CLI wait currently requires `agent get`, which is gated while the bundled `vqro.agent-status` provider is disabled. It returns `role: requested` only. An error or missing role file must not be interpreted as loaded. In Pi, inspect `/system-prompt` (or `/system-prompt-pane` when separately configured) to establish which role text Pi assembled; **neither proves final provider-visible bytes**. Vqro's Pi status integration independently reports session/activity through the public `pane.report_agent_session` and `pane.report_agent` API. No task, mailbox, automatic delegation, or acceptance behavior is claimed.

This slice requires no new server method or frozen generation-1 endpoint change. Neutral named-agent authority remains for a later slice. It is deliberately not a replacement for Herdr's `launch-role-agent.sh`, which enforces a different workspace and Gate contract. It does not configure or bypass Pi input-gate: if a project enables Herdr-specific mailbox/Gate behavior, leave it off in a disposable Vqro checkout until a Vqro mailbox adapter is delivered. No launch into the human's real Pi configuration, installed integrations, or running Vqro dogfood is authorized by these examples.

Local no-network tests: `node --test extensions/pi-launch-profile/launch.test.mjs`. The tests inject a fake Vqro CLI result and cannot start an agent.
