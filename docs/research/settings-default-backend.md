# Default backend in Settings

Settings now has a Backends page with one ordinary select/pick list. Its options come from
`extension/*` declarations whose stored kind is `provider`, not only currently connected providers.
The selected value matches core's first sorted explicit default when legacy state has several;
a successful pick repairs that ambiguity. Disabled providers remain selectable and visibly disabled.

`settings.default_backend {value}` applies one workspace transaction over the known provider entries,
with each entry's snapshot version as its CAS expectation. It sets the chosen default and removes
other default flags without changing argv, enabled, or provider-specific fields. None uses an empty
value and clears the explicit preference; it does not disable core's existing first-backend fallback.
The core's normal undo restores the transaction's before-images together. No provider commands,
spawns, or independent enable/disable effects are part of choosing a default.

Every known provider is fenced in the same transaction, including unchanged peers, rather than
ignoring a concurrent default change to an otherwise untouched declaration. A canonical same-choice
pick is a no-op. Actual changed choices use normal extension declaration replacement: enabled provider
extensions restart. The chooser says so and never promises backend lifetime beyond the native backend.

Tests in `test_settings_backend.py` cover the single transaction, version guards, preserving other
fields, disabled selection, clearing defaults, coherent before-images for undo, option rendering,
refresh, invalid choices, no-provider fallback and no-op repeated selection. The existing Settings
catalogue/page-order tests remain unchanged except for supplying the normal SDK cache lock in the fake.
