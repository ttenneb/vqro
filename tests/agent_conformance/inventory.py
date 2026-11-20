"""Capture the normative main contract independently of the executable catalog."""
from dataclasses import dataclass
import hashlib
import os
from pathlib import Path
import re
import subprocess

SPEC = "docs/spec/agent-v0-conformance.md"
GROUPS = "CDORML"


def part_a_ids(text):
    try:
        tests = text.split("## A3. Tests", 1)[1].split("## A4.", 1)[0]
    except IndexError as error:
        raise ValueError("normative Part A test boundaries missing") from error
    found = set()
    # A3 explicitly marks definitions; plain IDs (including ranges in prose)
    # are references, not extra normative cases.
    for match in re.finditer(r"\*\*([CDORML])([1-9][0-9]*)(?:[–-]([CDORML]?)([1-9][0-9]*))?\*\*", tests):
        group, start, end_group, end = match.groups()
        start, end = int(start), int(end or start)
        if end_group and end_group != group or not start <= end <= 1000:
            raise ValueError("invalid normative case range")
        defined = {f"{group}{index}" for index in range(start, end + 1)}
        if found & defined:
            raise ValueError("duplicate normative case definition")
        found.update(defined)
    if not found or set(ident[0] for ident in found) != set(GROUPS):
        raise ValueError("normative Part A groups missing")
    return sorted(found, key=lambda ident: (GROUPS.index(ident[0]), int(ident[1:])))


@dataclass(frozen=True)
class Contract:
    text: str
    commit: str | None
    source: str

    @property
    def ids(self):
        return part_a_ids(self.text)

    def metadata(self):
        return {"path": SPEC, "commit": self.commit, "source": self.source,
                "sha256": hashlib.sha256(self.text.encode()).hexdigest(),
                "main_verified": self.commit is not None}


def main_contract():
    repo = Path(__file__).resolve().parents[2]
    # Resolve once, then read that immutable object, not a racing main ref.
    env = {"PATH": os.defpath, "GIT_CONFIG_NOSYSTEM": "1"}
    try:
        def git(*args):
            return subprocess.run(["git", "--no-pager", "-C", str(repo), *args], env=env,
                                  check=True, capture_output=True, timeout=2).stdout
        commit = git("rev-parse", "--verify", "main^{commit}").decode().strip()
        if not re.fullmatch(r"[0-9a-f]{40}", commit):
            raise ValueError("main did not resolve to an immutable commit")
        text = git("show", f"{commit}:{SPEC}").decode()
        contract = Contract(text, commit, "git-main")
    except (OSError, subprocess.SubprocessError, ValueError):
        # Source exports can run diagnostics, but cannot assert current-main completeness.
        contract = Contract((repo / SPEC).read_text(), None, "bundled-unverified")
    contract.ids  # Refuse malformed documents before starting any fixture.
    return contract
