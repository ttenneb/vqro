# Neutral Vqro Pi adapter v1

Available: bundled `extensions/vqro-pi/index.ts`, using the installed Pi SDK's
normal extension resources and per-runtime event bus. Independent consumers use
the same public interface; no host method, service broker or durable store is
introduced. This is trusted in-process composition, not authenticated isolation.

Load order is independent: listen on `vqro:pi-adapter:v1:ready`, then emit
`vqro:pi-adapter:v1:discover`. Discovery/registration is synchronous. The supplied
version1 interface provides:

- `snapshot()` / `current(token)`: copied observed state and session/run/execution
  fences, not a reservation or authority lease.
- `onFact(listener)`: copied neutral session/input/run/turn/tool/settled/closed
  observations, sequence and SDK state. Consumers cannot mutate producer state
  by changing a supplied observation. Each textual field is bounded at64KiB;
  prompt/final/tool/input truncation is explicit. Origin labels are tagged
  **self_declared**, never authenticated identities. Unknown evidence stays
  unknown. Final text replaces intermediate text even when the last turn is empty.
- `beforeRun(callback)`: an asynchronous context contribution **awaited inside
  the SDK `before_agent_start` hook**. Returning `{message, discard?}` contributes
  context only after current-run/execution/shutdown checks; `discard` reports a
  rejected late contribution back to its owner. The SDK bus itself does not
  await listeners. Combined context is bounded at128KiB; failed consumers
  contribute nothing. Registrations are bounded to16 and can unsubscribe.
- `exec(argv, timeout)`: bounded CLI transport through the supplied Vqro binary;
  the adapter does not choose commands, recipients or product policy.
- `deliver(content, token)`: explicit queued SDK input with fresh execution and
  shutdown/run checks, at most 128KiB. Both SDK idle and busy paths use followUp
  queuing. A transport result is not agent acceptance; do not replay uncertain
  effects or infer understanding from a receipt.

Shutdown invalidates retained interfaces/in-flight work and unregisters discovery
and consumers. Facts never request input themselves. The adapter does not poll
mailboxes, invoke Tasks commands, interpret reply expectation or publish no-reply
labels. It keeps no message/approval/work record of truth.

## Tasks composition

`extensions/tasks/pi.ts` consumes this interface. Tasks owns durable message
claim/activation polling, message rendering, explicit-send evidence, reply caps,
recipient policy and no-reply observations. Disabled/unavailable Tasks cannot
activate a durable message or record a reply. A late successful
claim is recorded undelivered rather than injected into a different run; an
uncertain claim retains CLI recovery information and is not blindly replayed.

Reply recording — exact sender bindings, `reply_to` on every automatic answer,
no automatic answer to an answer, and no automatic reply unless the run's own
prompt is the delivered text — is Tasks consumer policy, documented with Tasks in
[Tasks conventions](task-conventions.md) and
[`extensions/tasks/README.md`](../../extensions/tasks/README.md).
The consumer has no broader automatic-reply policy for every claimed message.

The profile carries the neutral adapter and separately loaded Tasks consumer;
bundling describes distribution only, not host capability authority. Existing
profile opt-out, safe symlink/generation publication and warned fallback limits
remain. The optional profile-module reorganization is deferred.
