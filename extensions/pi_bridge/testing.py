"""Constrained installed testing-plugin admission and native inventory evidence."""
from pathlib import Path

COMMAND = "vqro-testing-status"
SELECTORS = ("VQRO_TESTING_SOCKET", "VQRO_TESTING_STATE_DIR")


def controlled(argv):
    if not argv:
        raise ValueError("explicit Pi argv required")
    for arg in argv[1:]:
        if arg in ("-e", "-p", "--print") or arg.startswith(("--extension", "--prompt-template")):
            raise ValueError("literal route forbids explicit interceptors/templates/print mode")
    return list(argv)


class Plugin:
    def __init__(self, path, root, principal=None):
        root = Path(root).resolve(strict=True)
        self.path = Path(path).resolve(strict=True)
        self.skill = (root / "tier1/skills/vqro-setup-testing/SKILL.md").resolve(strict=True)
        if (len(str(self.path).encode("utf-8")) > 4096 or len(str(self.skill).encode("utf-8")) > 4096
                or self.path != root / "tier1/vqro-testing.ts" or not self.skill.is_relative_to(root)
                or not self.path.is_file() or not self.skill.is_file()):
            raise ValueError("testing plugin and skill must be regular installed files")
        self.principal = principal if isinstance(principal, str) and 0 < len(principal.encode("utf-8")) <= 128 else None
        self.loaded = False

    def arguments(self):
        return ["-e", str(self.path), "--skill", str(self.skill)]

    def observe(self, rows):
        rows = rows if isinstance(rows, list) else []
        def matches(name, source, path):
            return any(isinstance(row, dict) and row.get("name") == name and row.get("source") == source
                       and isinstance(row.get("sourceInfo"), dict) and row["sourceInfo"].get("path") == str(path) for row in rows)
        self.loaded = matches(COMMAND, "extension", self.path) and matches("skill:vqro-setup-testing", "skill", self.skill)

    def fact(self):
        return {"state": "loaded" if self.loaded else "unverified", "path": str(self.path), "skill": str(self.skill),
                "principal": self.principal, "source": "rpc:get_commands"}
