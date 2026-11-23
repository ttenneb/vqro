# RPC setup-testing follow-up: evidence boundary and deferred findings

The optional `--testing-plugin` bridge field is admitted separately from native argv. It is anchored to
`vqro::paths::extensions_dir()` (legacy: its installed package root), selecting only installed
`tier1/vqro-testing.ts` and its derived skill. Missing/refused optional assets leave ordinary Pi launching
available with NOT VERIFIED; explicit native interceptor flags remain refused. Default host launchers negotiate
support before one actual launch, never retry after a possible effect. Configured unknown launchers stay unsupported.

`data.agent.testing_plugin` reports the native process's own `get_commands` command and skill paths. Only a matching
marker registered after hooks/tool setup establishes `loaded`. This is registration evidence, not loaded-byte
attestation, skill-body loading, or proof of every future hook. The existing per-launch principal selects a display
scope, not authority: one observed launch never upgrades the global view or unverified launch rows. An unavailable
provider cannot establish current coverage. Arbitrary custom principal reuse is not a new identity/lifetime guarantee.

`tests/e2e_testing_rpc.py` uses a private core, fake terminal capture provider and the real Rust bridge. A native RPC
double imports the actual plugin, drives its hooks, verifies a recorded current Unit plus refused finish for missing
Integration, and asserts no provider scheduling/continuation. It also exercises older-bridge help downgrade, denied
native `-e`, and outside-plugin refusal without a launch veto. Optional actual-Pi coverage performs only startup
inventory/state reads in a private HOME/agent/temp root: no prompt, credentials or model request. These are distinct
proofs, not a claim that the model-free double is the actual Pi runtime or that Windows execution was qualified.

## Deferred by explicit parent ruling: existing HC passthrough

No HC filter tightening is included. The pre-existing filters in:

- The shared `src/native_process.rs::native_env`, called by Pi's `src/bin/vqro-bridge-pi/native.rs::native_env`,
  strips HERDR names and all but the VQRO allowlist, but **does not test the HC prefix**. `HC_SOCKET`, `HC_SESSION`, and any other `HC_*` values already present reach the native RPC child.
- `src/bin/vqro-bridge-pi/main.rs::startup_version` calls that same filter, so its `--version` subprocess has the
  same existing HC passthrough.
- `extensions/pi_bridge/program.py::Bridge.__init__` has the same HERDR/VQRO-only test and therefore forwards HC
  values to the legacy child.

The follow-up adds only `VQRO_TESTING_SOCKET` and `VQRO_TESTING_STATE_DIR` to Pi's VQRO allowlists. Main's shared
native-process extraction is retained: `Process::spawn_with_env` lets Pi supply its environment setup without
copying process/lifetime/background-flag code or relaxing the default shared/Codex filter. The wire codec remains
only a codec. Explicit tests retain the default filter's refusal of the two Pi-only names. HC passthrough
needs a separate compatibility/security decision by the secondary/lead, not an unreviewed tightening here. Tests
must use private HC selectors (or no HC environment), never the user's endpoints. These findings are source facts,
not a live-environment or credential inspection.

## Platform review boundary

Admission uses host paths; canonicalization and Pi's reported path spelling must agree. Shared Rust/legacy bridge
changes need platform source review before shipping. Existing reserved-prefix matching is case-sensitive; Windows
case-alias handling and the existing native Windows setup-helper limitations must not be inferred solved by these
changes. New loading facts never globally certify native Windows, loaded executable images, arbitrary wrappers,
unmanaged imports, or a missing/broken testing checker.
