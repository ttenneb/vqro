# Temporary compiled agent launch profile admission

Status: accepted (owner decision, 2026-10-04)

## Decision

Pi-specific profile policy lives outside the neutral host in
`extensions/vqro-pi/profile.rs`, alongside the neutral Pi SDK adapter. The owner
explicitly admits this **temporary compiled trusted profile adapter**. It is
not a sandboxed extension package, runtime package replaceability, or a new host
capability grant. Its native filesystem authority is the existing profile
implementation's authority, not something installation or discovery confers.

The host supplies a narrow internal launch-profile seam: preparation at plain
startup, fresh read-only pane environment selection, and a fresh optional notice
for a resolved executable. `extensions/defaults.rs` names one static entry.
Installed manifests, plugin roots, settings, labels and package capabilities
cannot add callbacks or acquire this admission. There is no registration,
priority, generic runner, new store or public endpoint method.

Tasks owns its compiled distribution declaration in `extensions/tasks/bundle.rs`.
The profile adapter owns its composed Pi resources and SYSTEM.md. The host keeps
neutral safe resource publication, content addressing/generations, conservative
plugin linking and platform filesystem primitives. Product paths, settings
transforms, exclusions, ownership exceptions and warning text stay adapter-owned.
The neutral profile/bundle glue contains no Pi/Tasks policy names. Existing agent
detection and integration inventories are not redesigned by this decision.

## Protected behavior

Preparation remains after default resource publication at plain startup, with
independent errors; CLI/remote early returns gain no installation effects.
Readiness is re-evaluated at the existing server-local pane/start seams, never
cached as startup success. Environment edits affect the pane command only, not
process-global environment or caller/identity authority. Notices are fixed
trusted text, not commands or installed-package output.

Retain existing settings bytes/derivation, opt-out, inherited-override removal,
warned fallback, Unix-only profile behavior, generation names/hash order and
registry locking/custom/disabled choices. Retain current marker, unreadable/
invalid-UTF8 settings, read-only, existence-only foreign-resource and special
SYSTEM/resource-directory/excluded-leaf link behavior; this refactor does not
harden or reinterpret those quirks. Human targets are not written through profile
symlinks. Pi itself may update linked auth/model files: a profile is not a sandbox.
No generation-1 codec, method shape, frozen field, persisted identity or grant
changes are authorized here.

## Removal condition

Remove this compiled admission only after a separately approved replacement
profile composition proves the same preparation/readiness/fallback/config safety
and first-run behavior using a reviewed, explicitly authorized contract available
to independent adapters. An installed package's presence or compatible manifest
alone is not that decision. Keep the trusted compiled path explicit until its
replacement and transition are accepted; do not quietly widen it to arbitrary
executables or package callbacks.
