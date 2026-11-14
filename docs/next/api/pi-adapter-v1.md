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
  unknown. Final text replaces intermediate text even when the last turn is empty;
  aborted/error turns expose no final answer. `input_observed` reports submission.
  `input_message_started` reports SDK `message_start` for user/custom messages,
  with `messageRole`, bounded `text` and `text_truncated`. Custom appends can emit
  this observation without triggering model input; it is not consumption,
  delivery/acceptance evidence, or permission to activate a turn.
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

Unknown idle identity is re-observed using the existing editor observation clock,
paced at most once per 30 seconds independently of editor text or report success.
It reuses the same bounded host identity probe, adds no timer/API/event shape, and
only updates neutral cached identity after session/generation/run and idle/unknown
checks. Idle reads are single-flight with lifecycle/delivery reads; lifecycle and
fresh delivery serialize then make their own fresh observation, never borrow an
earlier read. Healthy known sessions add no recurring identity probes. Observation
itself sends no input and cannot adopt a prior execution's addressed queue.

Shutdown invalidates retained interfaces/in-flight work and unregisters discovery
and consumers. Facts never request input themselves. The adapter does not poll
mailboxes, invoke Tasks commands, interpret reply expectation or publish no-reply
labels. It keeps no message/approval/work record of truth.

## Tasks composition

`extensions/tasks/pi.ts` consumes this interface. Its existing mailbox interval
may be armed while identity is unknown, but performs no list, claim or delivery
until a freshly observed execution is present. Existing activation, token/current
and fresh-delivery checks still gate every effect. Tasks owns durable message
claim/activation polling, message rendering, explicit-send evidence, reply caps,
recipient policy and no-reply observations. Disabled/unavailable Tasks cannot
activate a durable message or record a reply. A late successful
claim is recorded undelivered rather than injected into a different run; an
uncertain claim retains CLI recovery information and is not blindly replayed.
Tasks gives message mutations a bounded 15-second outer execution budget, longer
than the CLI's two six-second identity/write API deadlines plus process overhead,
so an inner timeout can return the original caller/operation recovery text.
Unknown automatic replies retain that same recovery text; neither path retries
an uncertain mutation or treats its receipt as delivery/acceptance.

Reply recording — exact sender bindings, `reply_to` on every automatic answer,
no automatic answer to an answer, and no automatic reply unless the run's own
untruncated input is observed to carry the delivered message id — is Tasks consumer policy, documented with Tasks in
[Tasks conventions](task-conventions.md) and
[`extensions/tasks/README.md`](../../extensions/tasks/README.md).
The consumer has no broader automatic-reply policy for every claimed message.
After ownership is armed, Tasks permits one complete initial message-start
observation carrying that delivery marker. Subsequent submitted input or other
user/custom message starts revoke automatic reply ownership for that run,
including if a delivery marker appears again later. Revocation is monotonic;
only a separate delivery-owned run can arm a new reply. Tool/status observations
cannot revoke or establish ownership. Input revisions are rechecked after the
asynchronous Tasks availability read before recording an automatic reply.

The profile carries the neutral adapter and separately loaded Tasks consumer;
bundling describes distribution only, not host capability authority. Existing
profile opt-out, safe symlink/generation publication and warned fallback limits
remain. Profile policy and resource declarations live outside core under the explicitly
[temporary compiled launch-profile admission](../architecture/temporary-agent-launch-profile-admission.md).
The internal host seam does not extend this SDK interface, grant package authority
or expose runtime registration of launch profiles.
