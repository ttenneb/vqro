# Agents gear popup latency (2026-11-21)

The delay was the unconditional `Extension.fresh()` in `Agents.pane_menu`, not a bridge request.
It reloads `workspace.snapshot`, including every unrelated live transcript, before building a tiny popup.
Opening now uses the extension's event-fed cache. A missing agent retries once against a snapshot;
Bridge terminal still refreshes before acting. Existing settings/stop handlers retain their validation.
No asynchronous loading row is needed: the remaining path is already fast.

## Private public-wire A/B

Run `tests/agent_popup_latency.py BINARY --before-agents BASE_AGENTS_PY --scratch PRIVATE_DIR --report REPORT --samples 10`.
The harness creates isolated cores with explicit private HOME, state, extensions and backend endpoints;
no model, native agent, Herdr server or live workspace is used. It warms 128 rows of 128 characters per
synthetic agent. Each variant has three warmups and ten measured commands. Timing wrappers exist only
in disposable extension copies. A 100 ms p95 bound protects the cache-based path at both scales.

Final run used the private debug build at base d10ca93a, alongside the quick gate:

| Agents | Snapshot bytes | Before median / p95 ms | After median / p95 ms |
| --- | ---: | ---: | ---: |
| 30 | 698414 | 826.67 / 957.16 | 1.19 / 1.94 |
| 200 | 4614535 | 3616.65 / 3700.45 | 0.92 / 1.15 |

At 200 agents, median snapshot reload was 3614.24 ms, gear construction 0.038 ms and
publication 0.818 ms. Afterward: no snapshot; construction 0.013 ms, publication 0.228 ms.
The popup was 218 encoded bytes. There were zero nested `command.invoke` calls (no bridge round trip).
An earlier non-profiled 20-sample run independently measured 604.80 → 1.16 ms at 30 agents and
3684.03 → 0.73 ms at 200.

Limitations: these are command-reply timings, not terminal input-to-paint measurements. Synthetic
agents do not stream items, so this wire fixture measures the non-streaming gear menu; sender filters,
renames, removal, live view updates, missing-agent refresh and bridge revalidation are covered by
`tests/test_agent_pane.py`. This is not a native Pi/Codex soak or a claim about large sender lists.

Evidence retained at `<private-notes>` (per-run receipts and
phase timing files are under its roots). Initial quick passed build/Python but failed the unrelated
Rust slow-consumer test `slow_consumer_closes_the_writing_stream_and_spares_its_idle_sibling_and_other_connections`.
That test passed immediately in isolation; a complete second quick passed all 8 stages, zero skipped:
`<private-notes>`.
No Rust code changed. The initial failure is recorded, not suppressed. A further quick rerun in the
shared gate slot passed all 8 stages, zero skipped:
`<private-notes>`.
