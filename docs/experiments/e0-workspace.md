# E0: workspace programmability

> **Status (2026-11-16):** **Answered by building** (D16): extensions build the whole default workspace (tabs, panes, gestures, sidebar, settings, themes, keys) from public primitives, are replaced live and contained by the crash budget, and are reverted with transactions. Not done as specified: fake-source fixtures and the extension kill -9 / late-crash matrix. vqro is a working prototype, and fluid.

Goal: prove the workspace primitives (`spec/workspace-v0.md`) can carry substantially different workspaces built
entirely by extensions, with live replacement and rollback, before building the core (D14, `test-plan.md`).

Keep it throwaway. vqro spent heavily on its extension platform (consent, admission, an extension UI framework);
E0 must answer the question, not become that platform.

## Setup

- A throwaway core implementing only: views, layout documents with compare-and-set, commands, bindings, focus,
  namespaced state, and extension load/replace/rollback.
- **Fake execution sources**: scripted terminals (replayed byte streams, including OSC 7501) and scripted agents
  (replayed event streams with turns, tool calls and permission requests). No real processes or models.
- A minimal TUI client that renders whatever the layout says.

## Workspaces to build, as extensions only

| | A: terminal-centric | B: agent-centric |
|---|---|---|
| Layout | collapsible live terminals, tabs | agent list + conversation view + pending-requests queue |
| Views | terminal views, status badges from 7501 | agent views, request cards |
| Commands | split, collapse, focus-next | approve, deny, steer, open-raw |
| Bindings | tmux-like prefix keys | single keys contextual on agent state |

## Tests

1. Build A, then B, with **zero host changes**. Every host change needed is logged as a finding.
2. Switch A ↔ B live; fake processes keep producing output; nothing is lost within buffer bounds.
3. Replace one extension with a new version live.
4. Ship a deliberately broken version (crashes on load, crashes later, writes bad layout); rollback restores the
   previous version without touching processes or other extensions.
5. A scripted "agent author" (an extension driven only by the public protocol) adds a view and a binding to B.

## Outputs

- `research/e0-results.md`: host changes needed, primitives missing or unused, how extension views rendered (declarative,
  in-terminal, or both), rollback behaviour.
- Updates to `spec/workspace-v0.md`.
- Yes/no on the kill question.
