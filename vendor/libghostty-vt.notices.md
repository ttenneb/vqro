# libghostty-vt in vqro: provenance and notice inventory

**What.** Ghostty's terminal emulation library (libghostty-vt). The TUI's `libghostty` terminal engine links it
statically.

**Pin.** Source dist `libghostty-vt-1.0.2-dev` (LIB_VERSION `1.0.2-dev+9ad3c42`), Ghostty commit
9ad3c422222e45b32c5f9ab9f4cd32e0e3694ef7, 2026-11-20 (`libghostty-vt.vendor.json`). It builds with Zig 0.17.1 (the
human's Q-GH-4 decision, 2026-11-21: a user-local install at `~/.local/zig-0.17.1`, which build.rs finds; 0.15.2
stays). The previous pin was c5a21edf (2026-07-14, Zig 0.15.2).

**Provenance.** A shallow clone of github.com/ghostty-org/ghostty at 9ad3c422, then
`zig build dist -Demit-lib-vt -Doptimize=ReleaseFast` with Zig 0.17.1. The archive `libghostty-vt-1.0.2-dev.tar.gz`
had sha256 936a97c5ff880ed754da00d5e41f20d83c0c8da3d5f7f680a4c1fb9f43e498cf, and was extracted unchanged into
`vendor/libghostty-vt`. There are no local patches (`libghostty-vt.patches.md`). `scripts/check_vendor.py` checks each
file against `libghostty-vt.sha256`.

**Zig 0.17.1.** `zig-x86_64-linux-0.17.1.tar.xz` from ziglang.org, with sha256
3581863e3cbcfd0fce0efc682ce7e9e93da96668a977791d356a26d2a7e9b17a. It matches the published `index.json`, and its
minisign signature verifies against Zig's published key `RWSGOq2NVecA2UPNdBUZykf1CCb147pkmdtYxgb3Ti+JO/wCYvhbAb/U`
(Ed25519, the file and the trusted comment).

**What the static library contains.** Taken from `ar t` of a
`-Demit-lib-vt -Dvt-features=-all,+grid-introspection,+color` build of this tree (as build.rs builds it) on
x86_64-linux-gnu, 2026-11-21. Leaving out Kitty graphics also leaves out the Wuffs image decoder, which a full build
links (Apache-2.0):
- Ghostty's VT code;
- Ghostty's SIMD helpers (`codepoint_width`, `index_of`, `base64`, `vt`);
- Google Highway;
- simdutf;
- Zig's compiler_rt.

uucode's Unicode tables are compiled into the VT code. Other Zig packages that `zig build` fetches (libxev, vaxis,
wayland, z2d, ...) are not linked into lib-vt.

| Component | Where it comes from | Licence | Notice to retain |
|---|---|---|---|
| Ghostty (libghostty-vt) | `vendor/libghostty-vt` | MIT | `vendor/libghostty-vt/LICENSE`: "Copyright (c) 2024 Mitchell Hashimoto, Ghostty contributors" |
| Google Highway 1.2.0 (`highway-66486a10…`) | Zig package (`pkg/highway/build.zig.zon`, lazy) | Apache-2.0 OR BSD-3-Clause | its `LICENSE` and `LICENSE-BSD3` ("Copyright (c) The Highway Project Authors") |
| simdutf 9.0.0 (amalgamated) | `vendor/libghostty-vt/pkg/simdutf/vendor/simdutf.cpp` | Apache-2.0 OR MIT (upstream simdutf). The amalgamation carries no licence header, so the upstream texts at tag v9.0.0 are kept in `vendor/notices/simdutf/`. | upstream LICENSE-MIT and LICENSE-APACHE |
| uucode 0.2.0 | Zig package (`build.zig.zon`) | MIT ("Copyright (c) 2026 Jacob Sandlund"), plus its `licenses/LICENSE_unicode` (Unicode-3.0) for the UCD data and `licenses/LICENSE_Bjoern_Hoehrmann` (MIT) | `LICENSE.md` and both files in its `licenses/` |
| Zig compiler_rt | the Zig 0.17.1 toolchain (`lib/compiler_rt`) | MIT (Zig's `LICENSE`) | Zig's `LICENSE` |
| musl, in compiler_rt | 18 files of Zig's `lib/compiler_rt` say they are ported from musl, which is MIT (0.15.2 had 16 such files; the old inventory missed them) | MIT ("Copyright © 2005-2020 Rich Felker, et al.") | musl's `COPYRIGHT` |
| FFI declarations | `src/tui/ghostty/bindings.rs`, written by hand for vqro against the headers above, checked by `bindings_check.c` | vqro's own | none |

All of these are on vqro's licence allowlist (Apache-2.0, MIT, BSD-3-Clause, Unicode-3.0). `vendor/notices.json`
lists them with their texts (kept in `vendor/notices/` where the vendored tree or Zig package doesn't ship them).
`scripts/license_check.py` checks each licence against the allowlist and each text against its sha256, and renders
them into `THIRD_PARTY_LICENSES.md`. Re-take the `ar t` inventory when the pin or Zig changes.

**Zig packages and offline builds.** `build.rs` points Zig's package directory (`ZIG_LOCAL_PKG_DIR`) outside the
vendored tree: `LIBGHOSTTY_VT_ZIG_PKG_DIR`, by default `$XDG_CACHE_HOME/vqro/zig-pkg` or `~/.cache/vqro/zig-pkg`.
Packages are named by their content hash, so one directory serves every checkout and target.

- **First build on a machine:** Zig fetches every package `build.zig.zon` lists, about 310 MB, from
  `deps.files.ghostty.org`. Each is checked against its pinned hash.
- **Offline builds:** these work from a package directory holding only the two packages the library links. Measured
  2026-11-21 with no network (`unshare -rn`):
  - `uucode-0.2.0-ZZjBPm6FVgBcY-79AHV8ckQj86z0JwOyVyj89VeZrEiv`;
  - `N-V-__8AAGmZhABbsPJLfbqrh6JTHsXhY6qCaLAQyx25e0XE` (Highway's source).

  Together they are 15 MB. Copy them from a filled directory to build on a machine without network. simdutf ships
  inside the vendored tree (`pkg/simdutf`).
- **Not supported:** Zig 0.17.1's `--system` offline flag panics, so it isn't offered.
