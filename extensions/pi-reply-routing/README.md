# Pi reply-routing adapter (input provenance + origin replies)

A trusted same-user Pi extension (`--extension`) that labels every agent input
with its provenance and sends an agent-started turn's final text back to the agent
that messaged it. It is not durable messaging, a mailbox, activation or
authenticated identity: it reuses only `vqro agent prompt` and `vqro pane
report-metadata`. The label is self-declared and spoofable in trusted same-user
scope.

- `vqro agent prompt` auto-labels from a detected agent pane:
  `[agent <name> · reply expected]`, or `[agent <name> · reply]` with `--reply`.
  Unlabelled text is human input. Neither is routed.
- At `agent_settled`, an agent-origin reply-expecting turn sends its final text
  back with `vqro agent prompt <from> <text> --reply` (capped at `MAX_REPLY_BYTES`)
  and appends a session entry; an empty turn records `no reply to X` and an `idle`
  pane label. A turn that already messaged the sender does not send again.
- It fails closed without the managed Vqro environment, a parsed label, or a
  provable self identity, and the run's origin and final text are frozen for that
  run so a queued follow-up cannot claim it.

```text
pi --extension <repo>/extensions/pi-reply-routing/index.ts
```

Test: `just pi-reply-routing-test` (bun, fake pi, no server or model).
