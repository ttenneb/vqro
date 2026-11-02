# vqro

An early-stage, open, extensible agentic multiplexer, forked from Herdr. Read the
[README](README.md) for project status and the
[agentic multiplexer direction](docs/next/architecture/agentic-multiplexer-direction.md)
for the vision.

## Scope

These instructions apply to every agent working in this repository. Earlier
Herdr maintainer, machine-specific, and release-channel procedures do not
authorize Vqro work. Inherited workflow files remain in the tree: Herdr release
jobs are guarded to the canonical repository, Vqro fork side-effect jobs are
guarded, and full validation is opt-in. Do not treat these files as Vqro release
or install authority (see [Inherited Herdr surfaces](#inherited-herdr-surfaces)).

Vqro is a private, owner-directed project. Do not open unsolicited issues or
implementation pull requests. The owner explicitly authorizes each specific
outside task; a delegate may communicate that decision, but cannot infer it
from an issue or repository access. Herdr's inherited
`.github/APPROVED_CONTRIBUTORS` list grants no Vqro authority, and the
inherited PR Gate is guarded to `herdrdev/herdr`, not Vqro intake enforcement.
Inherited issue templates and the issue chooser still direct people to Herdr;
do not file a Vqro issue until the owner approves a Vqro-compatible template.

## Direction and invariants

Vqro keeps terminal execution, identity, persistence, recovery, capability
enforcement, and package execution in a neutral host. Product meaning and
workflow policy should live in replaceable extensions that use the same public
contracts available to independent developers.

### Enforced today

These rules are implemented and bound by accepted decisions. Changing them
requires a new or superseding architecture decision.

- **The host owns terminals, sessions, and identity.** Durable PTYs, stable
  workspace, tab, pane, and terminal identities, persistence, and recovery stay
  host-owned. A package failure fails closed for that package's authority, not
  for the session: generic workspace, tab, pane, and PTY recovery remain usable.
  See [Collections authority transition](docs/next/architecture/collections-authority-transition.md).
- **Installation and discovery grant no authority.** Packages are exact,
  verified artifacts. Curated or bundled status describes distribution only.
  Host capabilities are explicit, revocable leases.
  See [extension platform status](docs/next/api/extension-platform-status.md).
- **Package-to-package calls require exact, explicit consent.** A service
  binding approves one exact consumer artifact and one exact provider artifact.
  The host selects the provider from the binding; calls are bounded,
  non-recursive, run off the App actor, and give providers no host calls.
  See [Extension Service Fabric R0/R1](docs/next/architecture/extension-service-fabric-r0-r1.md).
- **Extension Doctor keeps host-derived facts host-owned.** Its consumer cannot
  alter the host-supplied observation before the provider evaluates it, and a
  restart rebuilds current state rather than replaying stale results. This is
  enforced for that product path only; its full product audit is still pending.
  See [Extension Health R1 product composition](docs/next/architecture/extension-health-r1-product.md).
- **Endpoint changes are explicit and versioned.** During development there is
  no compatibility floor (human-approved 2026-10-04); prefer new codecs and
  methods over reinterpretation. See [Stable client endpoint contract](#stable-client-endpoint-contract).
- **Render is pure and multiplicative paths stay narrow.** See
  [Principles](#principles) and [Multiplicative performance paths](#multiplicative-performance-paths).

### Design rules for new work

These rules come from the direction note. They are not yet fully implemented.
New work must follow them; a deliberate deviation requires a proposed
architecture decision under `docs/next/architecture/`.

- **One authoritative owner per fact.** The host owns terminals, sessions, and
  identity; a package owns its declared semantic domain; external systems such
  as Git own their own state. Vqro projects and composes those facts but does
  not duplicate their authority.
- **No hidden privileged paths.** Any first-party admission uses a documented,
  reviewable mechanism that third parties can eventually reach. Transitional
  exceptions, such as the exact-pin admission of Collections and Spaces, are
  named with their removal conditions.
- **PTYs are canonical for execution and recovery only.** They are not the
  source of truth for work, messages, or authority.
- **Messages have their own record.** Durable agent messages have their own
  record of truth. Injecting text into a terminal or editor may be a delivery
  adapter, but it is never the message of record or evidence of delivery.
- **Durable messages and volatile observations are separate lanes.** Do not
  stretch one mechanism to provide both guarantees.
- **Activation is authorized.** An agent turn starts only from a human action
  or an approved, recorded policy. Queued state, receipts, and status changes
  must never start one by themselves.
- **A receipt is not acceptance.** Keep transport, admission, execution, and
  accepted outcome as distinct states.
- **Extension work is bounded.** It must not scale with terminal bytes, client
  count, or render frames.
- **Integrations and workflows are adapters, not ontology.** Specific agents,
  pane IDs, Collections, and any delegation hierarchy are replaceable
  compositions. Agent integrations belong behind a neutral adapter boundary.
- **Evidence before generality.** Build a real vertical slice with a real
  consumer before designing generic infrastructure for it.

### Upstream Herdr

Vqro takes upstream Herdr changes only as selective, reviewed ports: one pull
request per port, stating the upstream commit or pull request, why Vqro needs
it, and how it was adapted. Do not merge upstream branches wholesale.

## Engineering Rules

### Principles

- **State is separated from runtime.** `AppState` is pure data, testable without PTYs or async. `PaneState` is separate from `PaneRuntime`. Workspace logic doesn't need real terminals.
- **Render is pure.** `compute_view()` handles geometry and mutations. `render()` takes `&AppState` and only draws. Never mutate state during render.
- **No god objects.** If a module is doing too many things, split it. `app/` is already split into state, actions, and input. Keep it that way.
- **Platform code is isolated.** OS-specific behavior lives in the matching `src/platform/<os>.rs` file, with only shared traits, types, wrappers, and testable contracts in `src/platform/mod.rs`. Core modules don't have `#[cfg(target_os)]`.
- **Detection is decoupled.** The detector reads a screen snapshot, never touches the parser or viewport state.
- **Screen detection is evidence-based.** When changing `src/detect/manifests/`, first capture the relevant bottom-buffer state with `vqro agent read <pane> --source detection --format text` and, when styling or alternate screen behavior matters, `--format ansi`. Decide which visible controls are invariant, which are alternatives, and encode them as explicit AND/OR gates. Do not match whole-pane incidental text, and do not use the user-visible viewport for agent status because users can scroll it.
- **UI patterns should be reused.** Vqro is a mouse-first TUI. New dialogs, onboarding, settings, and post-update flows should follow the existing UI/UX language and interaction patterns instead of inventing one-off screens. Prefer reusing existing modal/screen structure, affordances, and close actions so the app feels consistent.

### Multiplicative performance paths

Treat work reachable from view computation, rendering, background-pane resizing,
PTY parsing, detection, and client frame fanout as multiplicative. Before adding
work, identify its frequency and cardinality: per byte, event, or render × panes,
tabs, or workspaces × attached clients.

Inside pane-scaled render and layout loops:

- Use narrow terminal-state accessors. Do not collect aggregate input state,
  format terminal snapshots, inspect process trees, perform filesystem I/O, or
  allocate when one scalar fact is enough.
- Keep terminal-core lock duration minimal.
- Preserve hidden-source and retained-render early exits. Hidden panes still
  parse output, but their output must not trigger presentation work merely to
  keep terminal or detection state current.
- When a change adds or widens work in one of these loops, profile fixed geometry
  with 1 and at least 15 populated panes and report the scaling delta. Use
  `just bench-render-scale` to exercise both background-workspace and active-pane
  cardinality when applicable.

Prefer deterministic operation or architecture tests to wall-clock CI limits.
Performance benchmarks are supporting evidence, not substitutes for behavioral
coverage.

### Runtime/client boundary

Vqro is moving toward a server-owned runtime protocol with the TUI as one client. New work should not deepen the current server/TUI coupling.

Before adding state, API fields, events, commands, or socket messages, classify the feature:

- Shared runtime/session fact: belongs in server state and should be exposed through the JSON API/event path when practical.
- TUI presentation state: belongs only in the TUI/client layer.

Do not add new shared behavior that only works through the private TUI client socket. Use neutral server/API names, not UI-surface names like sidebar, row, card, or widget.

Examples:

- Pane/agent metadata, process state, terminal state, events: server/runtime.
- Sidebar layout, token placement, colors, selection, modals, mouse/viewport state: TUI/client.
- Workspace/tab/pane remain shared session organization for now, but avoid making them mandatory identity for unrelated runtime features.

### Stable client endpoint contract

The client-owned TUI endpoint generation is independent from the private
same-install protocol. Vqro terminal admission requires endpoint generation 2
or private protocol 24; generation 1 and private 22/23 are explicitly retired
(owner-approved 2026-11-01). Unchanged named codecs remain v1; raw JSON API
`vqro.api.v1` and extension/adapter pins are unchanged. See
[terminal admission decision](docs/next/architecture/terminal-client-admission-v2.md).
This makes no promise of interoperability with Herdr. Coexistence is enforced by
Vqro's separate binary, sockets, and state; remote use is currently disabled.

**Development-period decision (human-approved 2026-10-04): there is no
compatibility floor until community/ecosystem building begins.** Frozen codecs
and fixtures may be retired with an explicit decision, replacement list and
reason, and a same-PR documentation update. Do not silently reinterpret
generation 1. Prefer new codec/method names, optional advertised actions and
`Unknown` fallbacks; the rules below remain the default when practical.

Session v8 itself did not retire generation 1 or alter its named binary codecs;
the later terminal admission decision above explicitly retires that generation.
`layout.tree.export` is a new optional advertised method with the neutral
`layout.tree.v1` JSON result. Existing layout methods keep their selected-only
pane semantics. Session v8 originally used private protocol 23 / generation 1;
the later terminal admission decision raises these to 24 / 2 without changing
bincode-reachable payload shapes. The inherited stable v0.9.0 and preview
2026-09-08-62431dbd033b published protocol 22 / endpoint generation 1.
Session versions are independent.
See [tree export and conversion](docs/next/api/layout-tree-v1.md).

Fixture disposition for this change: endpoint hello/welcome/snapshot v1,
endpoint method-shapes v1, handoff-manifest v1 and hidden-member-handoff v7 stay
frozen as input/compatibility contracts. The inline v4 Collection writer golden
is retained as `tests/fixtures/session/collection-layout-v4.json` conversion
input; its writer expectation becomes a canonical-tree conversion expectation.
Tree output and v8 persistence get separate expectations; no old fixture is
rewritten to bless a wire change.

- Named core codecs are immutable. Do not add, remove, reorder, or reinterpret fields or enum variants reachable from a published codec. Introduce a new codec name and keep the old codec as a fallback instead.
- Keep baseline JSON handshake and snapshot fields required. New JSON fields must be optional or have field-specific defaults; new enum values need an `Unknown` fallback where older clients can safely ignore them.
- Add server features through advertised API methods and optional snapshot data when possible. A missing optional feature must disable only that action, not reject the connection.
- Do not change the meaning or load-bearing parameter shape of an advertised endpoint method. If an old server could ignore a new field and incorrectly report success, add a new method name or a separately advertised capability and omit that field without it.
- Missing methods, rejections, timeouts, and unavailable servers are client-local outcomes. They must not disconnect other compatible servers, and typing in a pane must not dismiss their notices.
- Frozen endpoint fixtures, bincode digests, wire-tag tests, and `tests/fixtures/endpoint-method-shapes-v1.json` are compatibility contracts. Never update a generation-1 expectation merely to bless a wire change; create and negotiate a new codec or method.
- Existing-value digests cannot detect an appended enum variant. Review every enum reachable from a frozen codec as append-closed even when tests remain green.

## Workflow

- Do substantive work on a dedicated branch in its own worktree. Small documentation fixes may use a lighter workflow when the maintainer prefers it.
- Before committing, propose the commit message and get alignment.
- Open issues or pull requests and push branches only for an exact task explicitly authorized by the Vqro owner. Keep branches based on the current `master` and rerun relevant local validation after integration; preserve published history rather than rebasing it without approval.
- After opening or updating a pull request, watch checks to completion (for example with `gh pr checks --watch`). Fix failures or explain exactly why one does not apply.
- Never merge a pull request. The maintainer performs the final merge.
- Follow [`CONTRIBUTING.md`](CONTRIBUTING.md) for owner-directed intake. The inherited `.github/MAINTAINERS` and `.github/APPROVED_CONTRIBUTORS` files do not grant Vqro contribution or merge authority.
- Normal feature and fix work does not edit the root `README.md`, root `CHANGELOG.md`, or the files listed under [Inherited Herdr surfaces](#inherited-herdr-surfaces) unless the change is specifically about them.
- Put proposed and accepted architecture decisions under `docs/next/architecture/`, following its status convention. Keep normative contract availability under `docs/next/api/`.
- Put local PRDs, temporary owners and clocks, test receipts, planning notes, and exploratory specs under `.local/prd/`; `.local/` is ignored and locally controlled. Do not use an ignored PRD as the only source for an accepted cross-worktree architecture decision, public contract availability, authority boundary, or compatibility obligation. Have local execution ledgers link to tracked sources rather than restating them. Source comments should describe capability state or removal conditions instead of referring only to locally defined milestone numbers.

### Open process questions

These are undecided. Do not assume an answer; raise them with the maintainer when
your work depends on one:

- release channels, versioning, and when to bump `PROTOCOL_VERSION`;
- automated review bots or additional required checks on pull requests;
- when the repository opens to outside contributors, and how that changes the
  contribution flow.

## Testing

Use `just` recipes by default instead of invoking cargo or scripts directly.

```bash
just test               # cargo nextest + maintenance script tests
just check              # formatting check + cargo nextest + maintenance script tests
```

Vqro requires Zig 0.16.0 to build the vendored `libghostty-vt`. If another Zig is first on `PATH`, set `ZIG` to the 0.16.0 executable.

Run formatting and relevant local behavioral tests for the changed surface and report exact passes, failures, and untested risks. For substantive code, use `just ci` locally when feasible; local `just check` with already-installed tools is optional and useful, never a landing gate. Only NEW local tool/SDK installs and additional paid MANUAL hosted Rust/macOS/Windows/Nix/Collections/ConPTY dispatches need explicit owner approval. Automatic paid Ubuntu advisory smoke follows the owner's standing budget policy, but this does not override an active budget HOLD or itself authorize pushing a branch. Never mask failures with `continue-on-error` or claim an unrun platform passed.

Windows MSVC cross-compilation from Unix requires SDK/CRT headers and libraries.
Only after explicit owner approval for the tool and SDK download, install `xwin` with `cargo install xwin --locked`, then run
`just setup-windows-cross` once and accept Microsoft's SDK license when prompted.
This downloads the SDK directly from Microsoft; no Windows machine is required.
The SDK and Zig libc configuration live at `~/.local/share/herdr/windows-cross/`,
shared by worktrees. `just windows-lint` and the Windows stage of `just check`
use this configuration automatically. To use another SDK, set
`LIBGHOSTTY_VT_WINDOWS_LIBC` to its Zig libc configuration file.
Setup accepts `--accept-license` for explicit noninteractive license acceptance;
normal checks never download the SDK. Native Windows builds auto-detect their
installed SDK. Native Linux/macOS builds do not need the Windows SDK.

Unit tests live next to the code (`#[cfg(test)] mod tests`). New `AppState` or `Workspace` behavior should be testable with `AppState::test_new()` and `Workspace::test_new()` without PTYs.

For broad refactors or release-risk regressions, classify the risk before editing. Treat changes as refactor-risk when they touch two or more core surfaces, persisted state, protocol/API IDs, workspace/tab/pane identity, restore/handoff, agent detection authority, or UI/input state projection. Before moving code, identify the protected behavior and add or name characterization tests. Identity/state refactors should use the test-only invariants `AppState::assert_invariants_for_test()` or `Workspace::assert_invariants_for_test()` with adversarial state from `AppState::test_with_adversarial_identity_state()` or `Workspace::test_adversarial_identity_state()`. Run a roundtable for broad refactors and release-risk regressions, not for routine local fixes.

When testing a new Vqro build from inside an existing Vqro session, use
`cargo run -- ...` and clear inherited Vqro socket overrides so the debug
binary talks to the debug `vqro-dev` server instead of an installed server:

```bash
env -u VQRO_SOCKET_PATH -u VQRO_CLIENT_SOCKET_PATH cargo run -- <command>
```

## Agent Detection Updates

Agent detection changes should use the manifest hot-reload loop against a disposable named Vqro session. Drive the real agent UI through the Vqro CLI/API into the target state. Read the pane with `vqro agent read <pane> --source detection --format text` and inspect matching with `vqro agent explain <pane> --json`. Update the bundled manifest in `src/detect/manifests/<agent>.toml`, copy that manifest to the local override path at `~/.config/vqro/agent-detection/<agent>.toml`, then run `vqro server reload-agent-manifests` against the session under test. Before writing the override, check whether one already exists; never overwrite or remove a pre-existing override without alignment. Once the rule is correct, remove the temporary override or restore the previous one exactly so the committed bundled manifest remains the source of truth.

Do not add large agent-specific full-screen fixture suites for routine manifest tuning. Keep Rust tests focused on manifest parsing, rule semantics, skip-state semantics, source precedence, cache reload behavior, and update flow. Use live pane reads for agent-specific screen evidence.

## Vendored libghostty-vt

`vendor/libghostty-vt.vendor.json` records the upstream source commit currently vendored.

Local patches on top of the vendored source must be tracked in `vendor/libghostty-vt.patches.md` and stored as patch files under `vendor/patches/libghostty-vt/`. Each entry should say why the patch exists, the related issue, upstream PR/discussion, vendored base commit, touched files, verification, and the exact removal condition.

When updating libghostty-vt, check every active patch in `vendor/libghostty-vt.patches.md`. If the new upstream commit contains the fix, remove the local patch and index entry, then rerun the listed verification. If not, reapply the patch on top of the new vendored source.

`just check` runs maintenance tests that verify local libghostty-vt patch files are listed in the index and reverse-apply cleanly against the vendored tree. Do not leave a patch file untracked or an indexed patch unapplied.

## Commit Style

Use lowercase conventional commits, no emojis, and no AI co-author lines. Keep subjects descriptive.

Before committing, propose the commit message and get alignment.

When a commit relates to a GitHub issue, add a commit body line `refs #<issue-number>` after the subject:

```text
fix: handle pane focus

refs #82
```

## Code Conventions

- Rust: no `unwrap()` in production code. Use `tracing` for logging. Use `#[allow]` only with a comment explaining why.
- Rust platform-specific code must be compile-gated. Put OS APIs and substantial OS behavior in `src/platform/`; when platform checks are needed elsewhere, use `#[cfg(windows)]`, `#[cfg(unix)]`, or target-specific `#[cfg(...)]` on imports, fields, functions, impls, and match arms so Windows-only code does not compile into Unix builds and Unix-only code does not compile into Windows builds. Use `cfg!(...)` only for pure cross-platform policy constants whose branches both compile on every target.
- Don't add dependencies without a reason. Check whether existing dependencies cover the need first.
- When changing the server/client wire protocol incompatibly, raise the `src/protocol/wire.rs::PROTOCOL_VERSION` bump decision in the pull request; Vqro has no release channel yet and its bump policy is still open. Live handoff and client compatibility compare this version. Update hardcoded protocol expectations and manual protocol fixtures in tests in the same change as any bump.

## Inherited Herdr surfaces

These files still describe Herdr, not Vqro. Do not follow them as Vqro process
or product documentation, and do not extend them for Vqro without an explicit
decision:

- `CHANGELOG.md`, `SPONSORS.md`, `docs/versions/`, `docs/preview/`, and `distribution/` (Herdr release history, published docs, install scripts, update feeds, and the remote agent-detection catalog);
- `docs/next/README.md`, `docs/next/README.zh-CN.md`, and `docs/next/CHANGELOG.md` (Herdr's staged release files);
- `docs/next/website/` (user documentation drafts, only partly updated for Vqro);
- `skills/herdr/` and `.agents/skills/herdr-*`;
- the release, preview, and PR-gate jobs, guarded to `herdrdev/herdr`; the website-deploy and label-next-release-issues side-effect jobs are also guarded in Vqro. Distribution remains path-filtered advisory docs/manifest validation, while full CI, Nix, and Windows ARM64 validation are manual opt-in. None authorizes a Vqro release, Herdr deployment, or issue-close effect.
