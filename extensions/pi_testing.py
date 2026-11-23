"""Explicit, per-launch Pi setup-testing assets; never edits the user's Pi profile."""
from pathlib import Path

from vqro_ext import VqroError


def testing_environment(ext, extensions_dir):
    """Bind this plugin to the managed selection, not ambient endpoint/path overrides."""
    info = getattr(ext, "info", {})
    result = {variable: info[key] for variable, key in
              (("VQRO_TESTING_SOCKET", "socket"), ("VQRO_TESTING_STATE_DIR", "state_dir"))
              if isinstance(info.get(key), str) and info[key]}
    if result:
        result["VQRO_TESTING_EXTENSIONS_DIR"] = str(Path(extensions_dir).resolve())
    return result


def testing_args(extensions_dir):
    root = Path(extensions_dir)
    candidates = (root / "tier1", root.parent / "integrations/pi")
    for base in candidates:
        plugin = base / "vqro-testing.ts"
        skill = base / "skills/vqro-setup-testing/SKILL.md"
        if plugin.is_file() and skill.is_file():
            return ["-e", str(plugin), "--skill", str(skill)]
    raise VqroError("Pi setup-testing plugin/skill is missing; install matching assets (no automatic retry)", "unavailable")
