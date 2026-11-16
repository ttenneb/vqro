# E1: agent input semantics (pi RPC, Claude Code stream-json)

Goal: measure when input actually takes effect in each runtime, so the agent capability model (D4) rests on
observed behaviour, not docs. Also check that a restarted reader loses nothing and replays nothing.

## Prerequisites

- `pi` at a version whose RPC mode has `steer`, `follow_up`, `abort` (researched at 1.5.2; 0.87.1 untested).
  Check `docs/rpc-commands.md` in the installed version.
- `claude` with stream-json flags (verified on 2.1.325): `-p --input-format stream-json --output-format stream-json
  --include-partial-messages --replay-user-messages --permission-prompts host`.
- **A controlled provider** (preferred): a local mock model endpoint that both runtimes call (pi custom provider;
  Claude Code via a different API base URL, to verify). It scripts responses and tool calls with exact timing, and logs
  every request, so "when did the steer reach the model" is read from the request trace, not inferred from output.
- Real provider credentials for a confirmation run (small cost).
- A scratch workspace directory the agents may edit.

## Harness (~300 lines Rust, throwaway)

1. Spawn each agent over pipes (not a PTY).
2. Append every stdout line, and every command sent, to `e1.jsonl` with a monotonic timestamp and byte offset.
3. Read stdin commands: `prompt <text>`, `steer <text>`, `queue <text>`, `interrupt`, `approve`, `deny`.
   Map them to each runtime's native call; if a runtime has no native call, log `unsupported` (don't emulate).
4. Scripted mode: run the scenarios below with fixed delays.

## Scenarios (each on both runtimes)

| # | Scenario | Record |
|---|---|---|
| S1 | Prompt a task with a slow tool call (e.g. `sleep 10; ls`). During the tool call, send `steer "also count the files"` | When the steer appeared in the model's context: mid-tool, after the tool, next turn, or never |
| S2 | While working, send `queue "now summarise"` twice | Order, whether both ran, one turn or two, whether a turn boundary is visible in events |
| S3 | `interrupt` mid-stream, then `prompt` again | Stop reason, partial output kept or dropped, whether the queued items survived |
| S4 | A task that needs permission | Request event shape; answering `deny`; what happens if never answered |
| S5 | Kill the harness reader mid-turn, restart it from the last offset | No events lost, nothing re-sent; agent still running |
| S6 | Kill the agent, resume its session | What resumes: history, pending queue, approvals (expect: not the in-progress turn) |
| S7 | Drop the acknowledgement of a write (kill the reader right after `write`) | Can the stdin offset resolve the `indeterminate` state (D10)? |

## Outputs

- `research/e1-results.md`: per scenario, per runtime, with log excerpts.
- Update the capability table in `architecture.md` and close the "unverified" cells.
- A yes/no on the roadmap kill question for E1.
