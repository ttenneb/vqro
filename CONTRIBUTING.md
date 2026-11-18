# Contributing

Original contributions to prim are licensed under Apache-2.0, including the core, bundled extensions, SDKs and
protocol specifications. See [LICENSE](LICENSE). Upstream third-party material retains its own terms and notices.

Contributions use the **Developer Certificate of Origin 1.1 (DCO)**, not a Contributor License Agreement (CLA).
Certify each contribution by adding a sign-off to its commit message, for example with `git commit -s`:

```text
Signed-off-by: Your Name <your.email@example.org>
```

Use your own contributor identity and only sign off work you have the right to submit. Keep other contributors'
notices/sign-offs when carrying their work. No retroactive rewrite of existing history is required by this policy.
The source of the certificate below is https://developercertificate.org/.

## Developer Certificate of Origin

```text
Developer Certificate of Origin
Version 1.1

Copyright (C) 2004, 2006 The Linux Foundation and its contributors.

Everyone is permitted to copy and distribute verbatim copies of this
license document, but changing it is not allowed.


Developer's Certificate of Origin 1.1

By making a contribution to this project, I certify that:

(a) The contribution was created in whole or in part by me and I
    have the right to submit it under the open source license
    indicated in the file; or

(b) The contribution is based upon previous work that, to the best
    of my knowledge, is covered under an appropriate open source
    license and I have the right under that license to submit that
    work with modifications, whether created in whole or in part
    by me, under the same open source license (unless I am
    permitted to submit under a different license), as indicated
    in the file; or

(c) The contribution was provided directly to me by some other
    person who certified (a), (b) or (c) and I have not modified
    it.

(d) I understand and agree that this project and the contribution
    are public and that a record of the contribution (including all
    personal information I submit with it, including my sign-off) is
    maintained indefinitely and may be redistributed consistent with
    this project or the open source license(s) involved.
```

## Dependency notices

`scripts/license_check.py` uses only Python's standard library and offline, locked Cargo metadata. Run
`python3 scripts/license_check.py --write` after changing dependencies, then review `THIRD_PARTY_LICENSES.md`.
The quick/full check gates validate SPDX choices and reject a stale inventory. `AND` requires every term; `OR`
needs one approved branch; `WITH` exceptions need explicit approval. Legacy Cargo `/` means `OR`.

The approved set is Apache-2.0, MIT, BSD-*, ISC, Zlib, Unlicense, and the explicitly approved Unicode-3.0,
Unicode-DFS-2016, WTFPL and Apache-2.0 WITH LLVM-exception. Unknown terms, missing declarations or unreviewed
exceptions fail closed. Ask the maintainer before changing this set; do not infer approval from a base licence.
Missing archive texts use labelled standard text for a reviewed declared licence and the crate's actual metadata,
not an invented copyright holder. New missing-text licences without a reviewed local standard text fail closed.

## Name and official distributions

Apache-2.0 does not grant trademark rights (section 6). The prim name/branding identify the project; modified
builds must not imply that they are official or endorsed. The maintainer designates official builds and the
maintained extension repository. This does not restrict the copyright permissions in Apache-2.0 or claim a
registered trademark. No public distribution or repository URL is established here.
