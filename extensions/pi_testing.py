"""Explicit, per-launch Pi setup-testing assets; never edits the user's Pi profile."""
from pathlib import Path

from vqro_ext import VqroError


def testing_environment(ext):
    """Bind only this plugin to the managed core's actual endpoint/state, not a default install."""
    info = getattr(ext, "info", {})
    return {variable: info[key] for variable, key in
            (("VQRO_TESTING_SOCKET", "socket"), ("VQRO_TESTING_STATE_DIR", "state_dir"))
            if isinstance(info.get(key), str) and info[key]}


def testing_args(extensions_dir):
    root = Path(extensions_dir)
    candidates = (root / "tier1", root.parent / "integrations/pi")
    for base in candidates:
        plugin = base / "vqro-testing.ts"
        skill = base / "skills/vqro-setup-testing/SKILL.md"
        if plugin.is_file() and skill.is_file():
            return ["-e", str(plugin), "--skill", str(skill)]
    raise VqroError("Pi setup-testing plugin/skill is missing; install matching assets (no automatic retry)", "unavailable")
