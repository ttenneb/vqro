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
control, native settings changes, complete history or account restart.

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

These observations do not implement policy or claim that native internal queues are observable. A later
`claude.restart` slice must guard exact source/session/account/episode, the owned ledger and native background
level inside its admission fence; stop only its owned process and observe exit before a new exec. It must not
replay input. Common policy/claims and any new visible continuation remain outside the adapter.

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
