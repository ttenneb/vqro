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
A public transaction claims `doc/workspaces/first-shell` before either bootstrap or a first
`tabs.open` spawns. The winner deletes that claim atomically with placing the observed terminal;
a concurrent first-shell caller coalesces instead of spawning again. Only a definite rejected
spawn releases an unused claim. An uncertain effect or observed-but-unplaced terminal keeps the
claim across extension/core restart, with an inspection warning. It never expires and replays.
After inspecting terminals, explicit workspace transactions can repair the layout/claim.

Availability reactions run off the SDK listener, queue behind an in-flight bootstrap, and refresh
state before deciding. This avoids dropping the sole backend-ready event and allows layout-only
CAS retries without rerunning spawn or blocking the cache event listener.

## Evidence

- `tests/test_empty_scratch.py`: activation, setting off/invalid, nonempty restart, selected workspace,
  exact placeholder versus user panels, no backend followed by availability, both orderings of
  prefix-n/bootstrap, queued availability, and indeterminate spawn retained across restart.
- `tests/test_sidebar_empty.py`: RED with no-layout refusal, GREEN with main/reset recovery,
  without workspaces or any spawn; already-visible sidebar remains visible on recovery.
- `tests/fixtures/client/window.json`: missing/dangling defaults, absent main, valid default
  precedence, and deleted pinned layout with main fallback; Python RED before resolver changes.
- Private Linux fake-core/TUI checks: pristine start shows Scratch, one fake terminal and sidebar;
  prefix s hides/shows it. With workspaces disabled and no configured backend ready, prefix s
  creates the neutral placeholder and sidebar with zero terminals. Badge/prefix recovery remains
  available. UI captures wait for the committed layout event, not a fixed early screenshot delay.
