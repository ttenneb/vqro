# Soak-4 P3 follow-up (2026-11-18)

Baselinef9a537c; private tests only, no live install/session effects.

## Failed extensions: retain evidence and make explicit recovery visible

The soak report deliberately SIGKILLed pi.py three times within five minutes. That correctly exhausted D15's
existing budget. Recent crashes reaching0 later is also correct, but was not enough to explain a latched failure.

Lean choice: **stay stopped until explicitly restarted/changed**, not a slow automatic retry. The evidence is
intentional repeated kills, not a transient fault that a timer demonstrably heals. An automatic retry would weaken
the budget, re-execute extension startup work without a user action, and add timer/rearm policy. No retry timer or
safe-mode lifecycle change was introduced.

- Core keeps a frozen failure cause/time/count and last3 timestamped exit reasons, independently of the sliding
  recent count and existing lifetime total. Startup failures also get a time/cause. Explicit restart/change clears
  the current failure/budget, not historical exit evidence; facts remain ephemeral per core/declaration lifetime.
- Settings shows local times, recent/total/frozen counts, reasons and Restart instructions.
- The persistent native failed badge is clickable. Built-in prefix recovery contains explicit per-extension
  restart/clear-failure entries, so broken/off Settings cannot hide recovery. Disabled/safe-disallowed entries
  remain stopped; core admission rules are unchanged.
- Deterministic core tests age the count by an hour, preserve failure evidence/total, confirm no automatic retry,
  and prove explicit restart retains history without bypassing safe mode. Native click/render and physical TUI
  tests restore a private missing executable, disable Settings, then recover through the native badge/menu.

## tabs.show: bounded CAS retries and a coherent cache snapshot

Baseline already used prim_ext.retrying, with its default3 attempts. A deterministic4-conflict writer reproduces
exhaustion. Navigation now allows8 attempts, still bounded with existing jitter/backoff, recomputing from fresh
state. Only CAS-safe layout/view operations repeat; spawn is outside the retry loop. Unknown outcomes/errors do
not get retried as conflicts. Creation uses compare-and-set guards as well.

A separate deterministic notification/command race reproduced active_layout returning a version paired with a
deleted value. That can turn into a generic extension exception, not a CAS conflict. The SDK now copies the
single entry's version/value under its existing cache lock, without holding it across RPC. Tests preserve all
concurrent writer tabs, create the requested tab once, and assert no spawn replay.

The historical soak's three generic extension errors were labelled *likely* CAS losses; without their stack
traces, their exact cause remains unverified. These two reproduced failure paths are fixed rather than claiming
that inference as proven or blindly retrying arbitrary exceptions.

Evidence: red/green logs and full-gate receipt in
`<private-notes>`; final results are in the task closeout report. No model calls.
