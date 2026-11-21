#!/usr/bin/env python3
"""Check the vendored libghostty-vt tree (libghostty plan G0): every file matches the recorded manifest, every
local patch listed in vendor/libghostty-vt.patches.md is applied, and the pin metadata and notices exist.

  python3 scripts/check_vendor.py            check (the check.sh step)
  python3 scripts/check_vendor.py --write    rewrite the manifest after an intentional vendor change

Stdlib only. Run from anywhere inside the repository."""
import hashlib, json, pathlib, re, subprocess, sys

ROOT = pathlib.Path(__file__).resolve().parents[1]
TREE = ROOT / "vendor/libghostty-vt"
MANIFEST = ROOT / "vendor/libghostty-vt.sha256"
PATCHES = ROOT / "vendor/patches/libghostty-vt"
INDEX = ROOT / "vendor/libghostty-vt.patches.md"
REQUIRED = ["vendor/libghostty-vt.vendor.json", "vendor/libghostty-vt.notices.md", "vendor/libghostty-vt/LICENSE",
            "vendor/libghostty-vt/LIB_VERSION", "vendor/libghostty-vt/build.zig", "vendor/libghostty-vt/include/ghostty/vt.h"]
# Build output that zig may leave if someone builds in place; never part of the vendored source.
IGNORED = ("zig-out/", ".zig-cache/", "zig-cache/", "zig-pkg/")


def files():
    for p in sorted(TREE.rglob("*")):
        rel = p.relative_to(TREE).as_posix()
        if p.is_file() and not rel.startswith(IGNORED):
            yield rel, hashlib.sha256(p.read_bytes()).hexdigest()


def main():
    problems = [f"missing {r}" for r in REQUIRED if not (ROOT / r).is_file()]
    current = dict(files())
    if "--write" in sys.argv:
        MANIFEST.write_text("".join(f"{h}  {f}\n" for f, h in current.items()))
        print(f"wrote {MANIFEST.relative_to(ROOT)} ({len(current)} files)")
        return 0
    recorded = dict((f, h) for h, f in (line.split("  ", 1) for line in MANIFEST.read_text().splitlines() if line))
    for f in sorted(set(recorded) | set(current)):
        if f not in current:
            problems.append(f"vendored file removed: {f}")
        elif f not in recorded:
            problems.append(f"vendored file added: {f}")
        elif recorded[f] != current[f]:
            problems.append(f"vendored file changed: {f}")
    listed = set(re.findall(r"`vendor/patches/libghostty-vt/([^`]+\.patch)`", INDEX.read_text()))
    present = {p.name for p in PATCHES.glob("*.patch")}
    problems += [f"patch not listed in {INDEX.name}: {p}" for p in sorted(present - listed)]
    problems += [f"listed patch missing: {p}" for p in sorted(listed - present)]
    for p in sorted(present & listed):
        r = subprocess.run(["git", "apply", "-R", "--check", str(PATCHES / p)], cwd=ROOT, capture_output=True, text=True)
        if r.returncode:
            problems.append(f"patch not applied to the vendored tree: {p}: {r.stderr.strip()}")
    meta = json.loads((ROOT / "vendor/libghostty-vt.vendor.json").read_text())
    if not str(meta.get("source_commit", "")).startswith("9ad3c422"):
        problems.append(f"unexpected pin {meta.get('source_commit')} (a pin bump updates this check, the notices and the bindings check)")
    for p in problems:
        print("vendor:", p)
    print(f"vendor: {len(current)} files, {len(present)} patches: {'FAIL' if problems else 'ok'}")
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
