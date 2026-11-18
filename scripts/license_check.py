#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Offline SPDX policy and reproducible Cargo licence/notice inventory (stdlib only)."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tomllib

ROOT = Path(__file__).resolve().parents[1]
OUTPUT = "THIRD_PARTY_LICENSES.md"
ALLOWED = frozenset({"Apache-2.0", "MIT", "ISC", "Zlib", "Unlicense",
    # Explicit maintainer approval, 2026-11-18; exceptions are not inferred from the base licence.
    "Unicode-3.0", "Unicode-DFS-2016", "WTFPL", "Apache-2.0 WITH LLVM-exception"})
ROOT_LICENSE_SHA = "cfc7749b96f63bd31c3c42b5c471bf756814053e847c10f3eb003417bc523d30"
MISSING_TEXT = "licence text not shipped in the crate; standard text for the declared licence"
STANDARD = {"MIT": ROOT / "scripts/license-texts/MIT.txt", "Apache-2.0": ROOT / "LICENSE"}
STANDARD_SOURCES = {"MIT": "https://raw.githubusercontent.com/spdx/license-list-data/main/text/MIT.txt",
                    "Apache-2.0": "https://www.apache.org/licenses/LICENSE-2.0.txt"}


def satisfying_licenses(expression):
    """Parse AND/OR/parentheses/WITH; return one approved choice, or None. Fail closed on syntax."""
    if not isinstance(expression, str) or not expression.strip():
        raise ValueError("missing SPDX expression")
    # Cargo's legacy slash spelling is an OR, not an AND.
    text = expression.replace("/", " OR ")
    tokens = re.findall(r"[A-Za-z0-9][A-Za-z0-9.+-]*|[()]|\S", text)
    pos = 0

    def factor():
        nonlocal pos
        if pos == len(tokens):
            raise ValueError(f"incomplete SPDX expression: {expression}")
        token = tokens[pos]; pos += 1
        if token == "(":
            value = disjunction()
            if pos == len(tokens) or tokens[pos] != ")":
                raise ValueError(f"unbalanced SPDX expression: {expression}")
            pos += 1
            return value
        if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9.+-]*", token) or token in {"AND", "OR", "WITH"}:
            raise ValueError(f"invalid SPDX token: {token}")
        if pos < len(tokens) and tokens[pos] == "WITH":
            pos += 1
            if pos == len(tokens) or not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9.+-]*", tokens[pos]) or tokens[pos] in {"AND", "OR", "WITH"}:
                raise ValueError("missing SPDX exception")
            token += " WITH " + tokens[pos]; pos += 1
        approved = token in ALLOWED or (" WITH " not in token and token.startswith("BSD-"))
        return frozenset({token}) if approved else None

    def conjunction():
        nonlocal pos
        value = factor()
        while pos < len(tokens) and tokens[pos] == "AND":
            pos += 1; right = factor()
            value = value | right if value is not None and right is not None else None
        return value

    def disjunction():
        nonlocal pos
        value = conjunction()
        while pos < len(tokens) and tokens[pos] == "OR":
            pos += 1; right = conjunction()  # Always parse, even after an approved branch.
            if value is None: value = right
        return value

    try:
        result = disjunction()
    except RecursionError as error:
        raise ValueError("SPDX expression too deeply nested") from error
    if pos != len(tokens): raise ValueError(f"unexpected SPDX token: {tokens[pos]}")
    return result


def allowed_expression(expression):
    return satisfying_licenses(expression) is not None


def check_project(root):
    package = tomllib.loads((root / "Cargo.toml").read_text())["package"]
    if package.get("license") != "Apache-2.0": raise ValueError("vqro package must declare Apache-2.0")
    if hashlib.sha256((root / "LICENSE").read_bytes()).hexdigest() != ROOT_LICENSE_SHA:
        raise ValueError("root LICENSE differs from the approved Apache-2.0 text")
    for path in sorted((root / "extensions/manifests").glob("*.json")):
        manifest = json.loads(path.read_text())
        if manifest.get("license") != "Apache-2.0" or manifest.get("author") != "Bennett Garcia":
            raise ValueError(f"bundled manifest licence/author missing: {path.name}")


def archive_files(package):
    root = Path(package["manifest_path"]).parent.resolve()
    files = set()
    for path in root.rglob("*"):
        if path.is_file() and (re.match(r"(?i)^(licen[sc]e|copying|copyright|notice|authors)([._-].*)?$", path.name)
                or any(part.upper() in {"LICENSES", "LICENCES"} for part in path.relative_to(root).parts[:-1])):
            files.add(path)
    if package.get("license_file"):
        declared = Path(package["license_file"])
        files.add(declared if declared.is_absolute() else root / declared)
    result = []
    for path in files:
        if not path.resolve().is_relative_to(root): raise ValueError(f"licence file outside crate: {package['name']}")
    for path in sorted(files, key=lambda p: p.relative_to(root).as_posix()):
        data = path.read_bytes()
        legal = bool(re.match(r"(?i)^(licen[sc]e|copying)", path.name)) or any(part.upper() in {"LICENSES", "LICENCES"} for part in path.relative_to(root).parts[:-1])
        if package.get("license_file") and path == (root / package["license_file"]): legal = True
        result.append((path.relative_to(root).as_posix(), data, legal))
    return result


def cargo_output(arguments):
    return subprocess.run(["cargo", *arguments], cwd=ROOT,
        env={**os.environ, "CARGO_NET_OFFLINE": "true"}, capture_output=True, text=True, check=True, timeout=60).stdout


def render(metadata):
    members = set(metadata["workspace_members"])
    texts = {}; records = []
    for package in sorted(metadata["packages"], key=lambda p: (p["name"], p["version"], p.get("source") or "")):
        if package["id"] in members:
            if package.get("license") != "Apache-2.0": raise ValueError("workspace licence must be Apache-2.0")
            continue
        expression = package.get("license")
        choice = satisfying_licenses(expression)
        if choice is None: raise ValueError(f"licence outside allowlist: {package['name']} {package['version']}: {expression}")
        files = archive_files(package)
        record = {key: package.get(key) for key in ("name", "version", "license", "authors", "repository")}
        record["source"] = package.get("source") or "path dependency"
        record["approved_choice"] = sorted(choice)
        record["files"] = []
        for name, data, _ in files:
            digest = hashlib.sha256(data).hexdigest(); texts[digest] = data.decode("utf-8")
            record["files"].append({"path": name, "sha256": digest, "origin": "crate archive"})
        if not any(legal for _, _, legal in files):
            record["notice"] = MISSING_TEXT
            for identifier in sorted(choice):
                if identifier not in STANDARD:
                    raise ValueError(f"no reviewed standard text for missing archive licence: {package['name']}: {identifier}")
                data = STANDARD[identifier].read_bytes(); digest = hashlib.sha256(data).hexdigest()
                texts[digest] = data.decode("utf-8")
                record["files"].append({"path": f"standard/{identifier}", "sha256": digest,
                    "origin": MISSING_TEXT, "source": STANDARD_SOURCES[identifier]})
        records.append(record)
    result = ["# Third-party licences and notices\n\n",
        "Generated offline by `python3 scripts/license_check.py --write`; do not edit.\n\n",
        "Policy and notices cover every Cargo metadata package, conservatively including unbuilt\n",
        "optional, build, dev and target-specific packages; inclusion does not claim they are shipped.\n",
        "Upstream terms remain upstream terms, not vqro's Apache-2.0 licence. Choices satisfy the approved\n",
        "allowlist; original supplied notices are retained even for other offered licence alternatives.\n",
        "When an archive omits a licence, standard text is labelled explicitly; declared authors/repositories\n",
        "are metadata, not an invented copyright attribution. Preserve these notices with distributions.\n\n",
        f"Packages: {len(records)}. Approved identifiers: {', '.join(sorted(ALLOWED))}; BSD-*.\n\n"]
    for record in records:
        result += [f"## {record['name']} {record['version']}\n\n", "```json\n", json.dumps(record, indent=2, ensure_ascii=False), "\n```\n\n"]
    result.append("# Licence and notice texts\n\nTexts are deduplicated by SHA-256 of original bytes.\n\n")
    for digest, text in sorted(texts.items()):
        fence = "`" * max(3, max((len(m.group())+1 for m in re.finditer(r"`+", text)), default=3))
        result += [f"## {digest}\n\n", fence + "text\n", text, "" if text.endswith("\n") else "\n", fence + "\n\n"]
    return "".join(result)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    modes = parser.add_mutually_exclusive_group()
    modes.add_argument("--write", action="store_true", help="regenerate the checked-in notice inventory")
    modes.add_argument("--check", action="store_true", help="verify policy and byte-identical generated inventory (default)")
    args = parser.parse_args()
    try:
        check_project(ROOT)
        metadata = json.loads(cargo_output(["metadata", "--offline", "--locked", "--format-version", "1"]))
        content = render(metadata); target = ROOT / OUTPUT
        if args.write: target.write_bytes(content.encode("utf-8"))
        elif not target.exists() or target.read_bytes() != content.encode("utf-8"):
            raise ValueError(f"{OUTPUT} stale/missing; run python3 scripts/license_check.py --write")
        print(f"licences OK: {len(metadata['packages'])} packages; {OUTPUT} {'written' if args.write else 'current'}")
        return 0
    except (ValueError, OSError, subprocess.SubprocessError) as error:
        print(f"licence check failed: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
