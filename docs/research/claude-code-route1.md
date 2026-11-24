# Claude Code route 1 — first vertical slice

`vqro-bridge-claude` owns one explicit native process using public stream-json/control stdin/stdout.
`extensions/claude_code.py` uses the existing launcher/request-route primitives; the native-TUI `claude`
route stays separate. No new terminal host, scheduler, persistence layer or account supervisor is added.

## Evidence and limits

Source target: Claude Code2.1.329 and its matched public TypeScript SDK0.3.329 (`sdk.d.ts` in
`@anthropic-ai/claude-agent-sdk`). Native semantics are source evidence, not a real-account/model qualification.
The regression fixture `tests/fixtures/fake_claude_stream.py` is synthetic: it imports no network client and
never executes Claude, a tool, login, profile or transcript. `tests/claude_provider.rs` pairs that process
with an in-memory fake core. `tests/claude_state.rs`, `claude_foundation.rs` and
`test_claude_code_launcher.py` cover projection/consent/framing/launcher boundaries separately.

The bridge does not run a version probe, login, updater, startup prompt or transcript scan. It initializes
control metadata and repeats that metadata-only initialization once to obtain the native full background
level. Claude itself owns its configured startup/auth/settings behavior. No `--bare` or settings-source
override is selected by vqro. The bridge refuses positional startup prompts and conflicting stream options.
An unknown native version remains observable but is not assigned a guessed literal-input guarantee.

Literal text uses native `client_composed:true`, preserving ordered text blocks and skipping native
slash-command/file-mention composition. This is not native-TUI input parity: those expansions must not be
inferred from typed text. Hooks may still transform input, so the first internal dispatch freezes
`may_transform:"unknown"` (including through ACK/receipt lookup/epoch changes). Proved owner text can stay
plain; other recorded senders receive the shared attribution-bound sender frame, always native user role.
User-provided sender/transform labels cannot create trust. Native user echo proves admission only;
`result.user_message_uuid(s)` supplies completion correlations. A native result UUID is reported as
`native.result_uuid`, not invented as a native query/run identifier.

Each native assistant content record has its own UUID even when several share an API message ID. They do
not overwrite each other. Tool IDs correlate lifecycle and permission withdrawal. Full consent/form data is
bounded and published before an answer is allowed; native interaction-only/unknown/oversize requests are
cancelled where representable, never flattened or auto-approved. A failed answer write is not retried.
A duplicate pending snapshot does not rearm an unknown/dispatched answer. Question-incarnation write tokens
revoke queued human/automatic responses at the actual shared writer's QUEUED-to-WRITING commit boundary;
a write already begun stays potentially acted, never relabelled no-effect. Unsettled claims survive view epochs.
Auth-status login output and private native control errors are not published. Oversize fields/entities are
omitted and marked incomplete, never shortened.

Interrupt reports the native control outcome, not a fabricated no-effect result or queue clear. The native
`still_queued` list is partial (UUID-stamped main-thread inputs only); `[]` is not a complete queue-empty
witness. Likewise `result.queued_turn_count` excludes system-generated entries. Canonical `queue` stays null.
Child records are not promoted to the parent's conversation. This slice does not implement child/task
control, native settings changes or complete history.

## Availability and future cutover witnesses

Blocks are turn scoped. A native terminal result ends that block scope, not the underlying account's quota
or billing condition. Positive current-turn subscription rate evidence plus a native blocking-limit result
retains `availability.native {subscription_limit:true,episode,session_id,terminal_reason:"blocking_limit"}`;
`episode` is the native result UUID. New work/input reservation, another result, native allowed-rate evidence
or a new epoch invalidates the old witness. Old rate evidence is not lent to a later result. No timer,
percentage, generic error or absent field proves subscription eligibility.

`data.agent._claude.input_ledger {v:0,source:"vqro-ledger",epoch,complete,pending}` is explicitly vqro-owned,
not native queue state. It counts reservations not terminally correlated, including unresolved old epochs.
Loss of retained evidence becomes `complete:false,pending:null`, not empty.

`background:[]` requires an actual complete native `background_tasks_changed.tasks:[]` level.
`_claude.background_level {v:0,source:"native:background_tasks_changed",process_incarnation,revision,complete}`
binds it to the same `data.agent.host.incarnation`; revision is a positive observed-level counter. Missing,
malformed or not-yet-projected nonempty sets stay unknown/incomplete. Levels are per process, so conversation
changes must not discard a live background level merely because its event carries another session ID.
Protocol failure/native process replacement invalidates the witness.

These observations do not implement policy or claim that native internal queues are observable. The
`claude.restart` primitive guards exact source/session/account/episode, the complete empty owned ledger and
native background level inside its admission fence. Common policy/claims and any new visible continuation
remain outside the adapter.

## Guarded owned-process restart

The private bridge flag `--restart-helper '["/ABS/HOST_PYTHON","/ABS/HELPER"]'` precedes the native `--`.
It selects execution-host helper configuration, never executable/environment arguments from public restart
input. Omitted configuration has no restart capability; an unavailable helper does not affect initial launch.
The versioned operation takes `{v,epoch,session,account,episode,target:{account,ref,host,revision},
allow_background_loss?}`. The loss flag defaults false; unknown/incomplete/running native background refuses.
True bypasses ONLY background, never the source/host/ledger/main-turn checks. Human disclosure and AUTO's
always-false choice belong to the calling policy, not this primitive.

Execution host comes from the core-acknowledged provider registration's `host` (the same authenticated value
as provider snapshot metadata), never native process `data.agent.host`. A private metadata-only preflight
`check --handle REF --provider anthropic --kind claude_subscription` is bounded by both the effect deadline
and35seconds, accepts only bounded `{status:ready}`, and never forwards helper stderr/secrets. Failure dispatches
no owned stop (A may change independently). After revalidation, stop A; only a successfully observed native
wait status, including signal termination, permits replacement. Wait error/unknown refuses before generation
reset or new exec and reports restart unknown. Then invoke that same private
helper prefix with `launch --handle REF --provider anthropic --kind claude_subscription --` followed by
original controlled native settings/cwd and literal `--resume EXACT_SESSION`. No fork, rescue flag, retry,
second supervisor, fallback to A or input replay is introduced. The helper, not the bridge, retrieves/sets auth.
The newer helper/callback is supplied by the accounts stream; an older incompatible helper safely fails preflight.

Generation checks occur under the observation state lock; old observations/initializers/automatic-response
failures cannot mutate the replacement epoch. The existing CLI supervisor explicitly waits through an owned
restart instead of treating its expected old-child exit as final provider termination. Both stdin-thread and
signal-task lifetime publication take the EXISTING native admission fence, also held from the last ending
sample through replacement initialization. Observed lifetime ending forbids later input admission or child
dispatch. Lifetime publication waits behind effects already admitted ahead of it in the existing FIFO fence;
each has its own absolute deadline (restart preflight is independently limited to35seconds). Publication then
stops the CURRENT owned process, including after failed initialization. The delay includes that earlier admitted
work, not just the currently running restart, and stop is not immediate under contention. An OS spawn cannot
be pre-empted: these are deadline-accounting bounds, not a hard kernel-latency guarantee. The same bound is
shown in the agent pane's restart lifetime notice. No second intent or priority mechanism is introduced.
Actual initialization success, not PID allocation,
is `started:true`; native same-session readback confirms resume, missing readback stays explicitly unconfirmed,
and mismatch/refusal is not success. Requested session is never copied into observed session. Configured B
publishes only known id/provider/kind with `vqro:restart` sources and omits unknown label; it is not authenticated
identity. Native/helper/model execution is still synthetic qualification only.

## Native children, execution work and plans

Pinned SDK0.3.329/CLI2.1.329 supplies per-process background membership levels and separate task lifecycle edges.
Membership replaces only the background witness; leaving a level does NOT complete a task. Positive live work
not covered by that witness revokes its completeness (including a stale empty level); only a fresh native full
level restores it. Known older/same-terminal run edges do not reopen work, and the edge never invents a complete
membership set or rewrites the parent's rate episode. Native run ids are
ordered only within their task; older runs cannot overwrite a later run. Conflicting terminal evidence is
unknown/incomplete. A backgrounded `main-session` task is not fabricated as a child.

Positive producer `agent_id` or an unambiguous observed spawn `tool_use_id` binds scoped child messages/tools/requests.
Unknown or over-bound scope is omitted/incomplete, never root content or an approvable root form. Child UUIDs/tool
ids cannot update root receipts, session, foreground availability or tool/consent identity. Consent settlement
is scope-specific; RECENT tombstones retain their own scope, so known-root unknown consent still makes root
phase unknown while known-child tombstones stay outside it. A permission body's documented agent_id wins
over extra wrapper metadata; context-free snapshots retain the positively known scope of a live native question. Root phase
ignores child questions, but restart separately refuses ANY unresolved native consent, preserving its safety fence.
It also refuses positively known DIRECT-root foreground work before stop for both background-loss choices;
that guard does not rewrite root phase or background membership. Nested work under a known background parent
is not inferred to be root foreground, and unknown ownership is never guessed. Known child/unknown question
scope survives context-free changed snapshots in both pending and RECENT records until native settlement.
No child endpoint/principal/inbox or send/resume/close is invented.

Only successful structured `tool_use_result`, correlated to one observed tool result/name, supplies plan values:
TodoWrite.newTodos/TaskList.tasks are whole snapshots; TaskCreate/Get seed partial observed items. TaskUpdate's
updatedFields does NOT contain actual values and never authorizes copying tool input (hooks may change it).
Only explicit statusChange supplies that changed status; unknown IDs/fields remain incomplete until native readback.
Tool task tracking is a plan, NOT native background execution. Lists without native owner/id are namespaced by
observing context; producing a shared list does not prove ownership, so owner stays absent and observer child is
only native metadata. An `active_goal` event is observed separately; null clears the objective but does not infer
completion, and tokens_at_start is not reported as used tokens or a budget.

Native `stop_task {task_id}` alone backs epoch-guarded `agent.object` cancel for observed tasks/children on qualified
2.1.329. ACK means accepted, not stopped; terminal projection waits for actual native evidence. Unknown versions,
objects/actions/kinds and raw native target overrides refuse before dispatch. Root interrupt remains `all_work`;
`perTaskStopAffordance` and `forwardSubagentText` are not sent. Text appears only if the native default emits it.
Forwarding text later would require emission-only source evidence and explicit opt-in; it is not silently enabled.
No plan CRUD/model-prompt emulation or pane per-task button is added. Stores/items/identities are bounded; omitted
fields/rows are marked incomplete, never shortened or inferred empty.

## Item lifetime

Items have bounded entities/retention and tail coverage, not a complete native transcript. Process exit and
stdout-reader completion are distinct. The reader reports EOF, forced cutoff and protocol failure separately;
cutoff is never called EOF. Public lifecycle remains `ending` until the final item cut, so the ordinary publisher
cannot retire clients before late records. Final frames drain before normal item close; the SDK reason field
carries `native_reader_closed`, not a generic provider-drop fallback. A bounded unconfirmed tail is labelled
`native_tail_unconfirmed`/incomplete, not claimed EOF or complete history. Retired publisher/initializer tasks
are joined before the final cut. Each item stream retains its handle after output close/gap until the core's
existing `stream.close` retirement acknowledgement (normal real-core relay drained, not merely provider enqueue).
Missing/disconnected/timed-out retirement stays unconfirmed and never publishes a successful ended cut.
The existing acknowledged `provider.sync` (reply once applied) then fences final
item data/terminal markers and facts before intentional disconnect. An outer five-second deadline includes
outgoing-budget/channel enqueue stalls as well as the ACK wait; timeout stays unconfirmed and is not replayed.
The bridge stops only its owned child
on shutdown; it adds no persistence or stronger descendant lifetime guarantee than the native/backend layer.
