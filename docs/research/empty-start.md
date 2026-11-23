# Empty Scratch startup

A pristine store had no layout: defaults installed extensions, but only explicit `tabs.open`,
`workspaces.main`, or recovery reset created main. `sidebar.toggle` therefore refused with
`no active layout yet`. Existing main was also ignored when the shared default was absent or dangling.
A legacy zero-tab main rendered a blank body; normal transactions already reject empty tab arrays.

## Layering

- Clients resolve an existing pinned layout, then an existing shared default, then existing main.
  They never initialize policy or terminals. A nameless `client.layout` intentionally returns to
  the shared/default-main fallback. Shared `window.json` conformance cases cover all three models.
- Workspaces owns default-on `doc/settings/workspaces.open_shell_when_empty`, exposed in Settings.
  Activation/backend availability creates one Scratch shell, including headless core starts, but
  never adds one to nonempty main or overrides a valid selected workspace. Only the exact core
  recovery placeholder is replaceable; other panels survive. Scratch gets its initial sidebar
  when the sidebar contribution becomes available.
- Sidebar has a backend-independent path: select existing main or invoke public neutral recovery
  reset, then show the sidebar. This works without workspaces. Built-in badge/prefix recovery
  remains unchanged; neutral reset still does not spawn a shell.

## Effect safety

`vqro_scratch.FirstShell` is narrow extension policy, not a new core lock or spawn guarantee.
Only the automatic bootstrap claims `doc/workspaces/first-shell` before spawning. It deletes that
claim atomically with placing the observed terminal. A definitely rejected automatic spawn releases
its unused claim. An uncertain effect or observed-but-unplaced terminal keeps the claim across
extension/core restart; it never expires and replays. The actionable warning directs the user to
prefix n for a new shell. Explicit `tabs.open` and `workspaces.main` never take or wait on this claim:
user actions must not be suppressed by an automatic-start recovery marker. Once Scratch has a tab,
bootstrap CAS-deletes any moot claim, including after a layout event from a successful user action.
If n finishes first, bootstrap does nothing; a genuine overlap can yield an extra shell, which is
preferable to blocking n. This deliberately replaces the initial shared-claim coalescing approach.

Availability reactions run off the SDK listener, queue behind an in-flight bootstrap, and refresh
state before deciding. This avoids dropping the sole backend-ready event and allows layout-only
CAS retries without rerunning spawn or blocking the cache event listener.

## Evidence

- `tests/test_empty_scratch.py`: activation, setting off/invalid, nonempty restart, selected workspace,
  exact placeholder versus user panels, no backend followed by availability, explicit n during
  an in-flight/uncertain automatic start, stale-claim cleanup on activation/layout events,
  queued availability, and indeterminate automatic spawn retained across restart.
- `tests/test_sidebar_empty.py`: RED with no-layout refusal, GREEN with main/reset recovery,
  without workspaces or any spawn; already-visible sidebar remains visible on recovery.
- `tests/fixtures/client/window.json`: missing/dangling defaults, absent main, valid default
  precedence, and deleted pinned layout with main fallback; Python RED before resolver changes.
- Private Linux fake-core/TUI checks: pristine start shows Scratch, one fake terminal and sidebar;
  prefix s hides/shows it. With workspaces disabled and no configured backend ready, prefix s
  creates the neutral placeholder and sidebar with zero terminals. Badge/prefix recovery remains
  available. UI captures wait for the committed layout event, not a fixed early screenshot delay.
