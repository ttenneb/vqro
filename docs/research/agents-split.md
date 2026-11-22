# Granular agents and Pi extensions (2026-11-17)

Human-directed split: agents is runtime-neutral; pi owns launch/settings. No core/Rust changes.

- agents observes kind agent, caches public provider capabilities via prim_ext, and offers only declared native actions. It owns neutral agents-rN panels, read-only optional provider agent-rN output, generic facts fallback, request picker and Show agents. No launcher, Pi import, or runtime default.
- pi owns pi.new/settings/configure, New Pi agent/prefix a, literal argv and bridge:true backend execution. It invokes agents.show or falls back to the real host terminal through tabs.show. Disabling either UI extension leaves backend-owned execution alone.
- Existing doc/settings/agents runtimes are copied once to absent doc/settings/pi, preserving source and existing destination. Owner-scoped activation reconciliation removes moved agents-owned launch contributions without deleting Pi-owned ones.
- Shared bounded/control-safe text helpers live in prim_display.py. Provider snapshot/event caching is generic SDK mechanism; events wrap provider metadata in state.
- Integration caught reused/invalid backend names: Pi leaves naming to the backend. It also caught inventory-not-yet-restored panel deletion invalidating referenced layouts: neutral panels are retained, display unknown/stale facts and are not deleted underneath tabs.

Verification: scripts/check.sh full PASS17/17, including zero-warning build,154 ordinary Rust tests,99 Python tests, all opt-in provider/transport suites, TUI flows, default scale and no leftovers. Full receipt: <private-notes>

Private E2E drives actual prefix/menu/input/confirmation, all Pi agent effects, restart/no replay, then disables pi and operates an independent non-Pi abort-only provider with arbitrary data; undeclared prompt is absent. Disabling agents leaves Pi running and a new Pi launch opens a real terminal fallback. Core restart restores commands/panels and native identity. Final full-gate E2E receipt/captures: <private-notes>

All tests are private and fake-model only; no real prompts/provider calls, copied credentials, live services or install. Usage: extensions/agents.md and extensions/pi.md. Native request forms retain existing single-line editor input; Windows/remote/model-backed turns are not claimed.
