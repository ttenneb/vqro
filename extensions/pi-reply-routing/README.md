# Pi reply-routing adapter (input provenance + origin replies)

A trusted same-user Pi extension, loaded with Pi's normal `--extension` option,
that makes every input an agent receives carry its provenance and sends an
agent-started turn's final text back to the agent that messaged it.

It is **not** durable messaging, a mailbox, an activation path or authenticated
identity. It uses only the existing `vqro agent prompt` and `vqro pane
report-metadata` CLI paths. The label is carried in the prompt text and is
self-declared: in trusted same-user scope it is spoofable and is not proof of who
sent a message. Host-owned caller identity belongs to the later thin-messaging
slice.

## Label grammar

`vqro agent prompt`, when run inside a Vqro pane that hosts a detected agent,
automatically prefixes the text with a readable single-line label (see
`src/cli/agent.rs`):

- `[agent <name> · reply expected] <text>` — an agent message that wants an answer
- `[agent <name> · reply] <text>` — a reply; never expects another reply (`--reply`)
- `[host notice] <text>` — a host notice; never expects a reply
- unlabelled text — a human; never routed

`<name>` is the sender's registered agent name, or its pane id when unnamed.

## Behavior

- On `input`, the adapter records the turn origin from the label and leaves the
  readable label in place for the model. `--no-label` suppresses the CLI prefix
  for scripts; human prompts are never labelled.
- At `agent_settled`, an agent-origin turn that expected a reply sends its final
  assistant text back to that agent with `vqro agent prompt <from> <text>
  --reply`, capped at `MAX_REPLY_BYTES`, and appends a `vqro-reply-routing`
  session entry. If the turn already messaged that same agent, it does not send
  again. A turn that produced no text records `no reply to X` and sets a bounded
  `idle` state label on the pane through the existing generic metadata channel.
- Host and human turns are never routed; a reply never expects a reply.
- It fails closed: without the managed Vqro environment, an unknown self
  identity, or an unparseable/absent label, nothing is sent.

## Enable

```text
pi --extension <absolute-path-to>/extensions/pi-reply-routing/index.ts
```

The pane needs `VQRO_ENV=1`, `VQRO_SOCKET_PATH`, an absolute `VQRO_BIN_PATH` and
`VQRO_PANE_ID`; mixed `HERDR_*` routing disables it. The sending agent should be
named (`vqro agent rename`) so replies address a stable name.

## Test

```sh
just pi-reply-routing-test   # bun test; fake pi, no server or model
```
