# Apache-2.0 and dependency notices (2026-11-18)

Human decision D25: Apache-2.0 for all original prim material, DCO1.1 sign-off/noCLA. Third parties keep their
own terms. Nothing is published/installed by this work. URLs and copyright holders are not invented.

## Root text / metadata

`LICENSE` is the official https://www.apache.org/licenses/LICENSE-2.0.txt, byte-identical (fetched without
installing tools): `cfc7749b96f63bd31c3c42b5c471bf756814053e847c10f3eb003417bc523d30`. `~/Projects/herdr/LICENSE` has the same text without
the official file's initial blank line (`c71d239df91726fc519c6eb72d318ec65820627232b2f796219e87dcf35d0ab4`).
Cargo and15 bundled manifests now declare Apache-2.0/the established author Bennett Garcia. README, NOTICE,
CONTRIBUTING and the extension guide establish scope, third-party exceptions and brand/official-build distinctions.
DCO1.1 is reproduced verbatim from https://developercertificate.org/ under its own verbatim-copy terms.

## Offline policy / deterministic inventory

`python3 scripts/license_check.py --write` runs offline locked cargo metadata, checks every package's SPDX
expression against the approved policy, and regenerates THIRD_PARTY_LICENSES.md. `--check` (also the default)
requires an exact current artifact; scripts/check.sh quick/full includes this check. No network/install tool runs.

The small parser handles AND/OR precedence, parentheses, Cargo's legacy slash-OR and explicit WITH exceptions.
Unknown/missing/malformed/disallowed terms fail closed. In addition to Apache-2.0/MIT/BSD-*/ISC/Zlib/Unlicense,
the maintainer explicitly approved Unicode-3.0, Unicode-DFS-2016, WTFPL and Apache-2.0 WITH LLVM-exception.
A permitted base does not implicitly permit an exception; AND requires both sides, OR a permitted alternative.

All214 metadata packages are policy-checked. The notice artifact covers all213 third-party packages, including
unbuilt optional/build/dev/target packages conservatively; it does not imply every package is shipped. There is
no hardcoded platform/feature filter that can hide a declaration or make scope depend on the checking machine.

Original archive licence/notice/copyright/authors files are retained and deduplicated by SHA-256. Source metadata
supplies authors/repositories verbatim, including null/empty when undeclared. Seven archives lack licence text:
r-efi5.3.0/6.0.0, ratatui-termina0.1.0, vtparse0.6.2, wezterm-input-types0.1.0 and the two winapi GNU architecture
archives0.4.0. Their declared MIT alternative uses labelled standard text from SPDX's license-list-data MIT.txt,
with source URL/hash, without filling its copyright template from guesses. A new missing-text licence without a
reviewed local standard text fails rather than silently omitting its grant.

The inventory is written as bytes, retaining original CRLF/trailing whitespace. Narrow .gitattributes entries
prevent line-ending rewriting of legal texts and exempt only the generated corpus from whitespace lint.

## No speculative dependency trim

Ratatui0.30.2's default features do not enable termwiz. `cargo tree --offline --locked --target all -i termwiz`
prints no dependency path. It appears only in the larger metadata/lock optional universe; it is not an active
backend dependency to remove. The maintainer's conditional trim was therefore not taken; dependency resolution
remains214 metadata packages before/after. No unrelated widget/macro/render feature was silently dropped.

## Evidence

Committed unit tests cover SPDX choices/precedence/exception rejection, original text/digest retention,
deterministic ordering, labelled fallbacks without invented holders, crate-path escape rejection, exact root
licence/manifest metadata, stale/missing inventory, and policy rejection even for unbuilt optional packages.
Red/green, inventory receipt, official text and full-gate receipt:
`<private-notes>`. Final counts/outcomes are in the task closeout report.
External runtime/backend terms remain a separate open question, not resolved or relicensed by this decision.
