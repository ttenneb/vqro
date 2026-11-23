"""Explicit, per-launch Pi setup-testing assets; never edits the user's Pi profile."""
from pathlib import Path
import sys


def testing_environment(ext, extensions_dir):
    """Bind this plugin to the managed selection, not ambient endpoint/path overrides."""
    info = getattr(ext, "info", {})
    result = {variable: info[key] for variable, key in
              (("VQRO_TESTING_SOCKET", "socket"), ("VQRO_TESTING_STATE_DIR", "state_dir"))
              if isinstance(info.get(key), str) and info[key]}
    if result:
        result["VQRO_TESTING_EXTENSIONS_DIR"] = str(Path(extensions_dir).resolve())
    return result


def testing_args(extensions_dir, ext=None):
    root = Path(extensions_dir)
    candidates = (root / "tier1", root.parent / "integrations/pi")
    for base in candidates:
        plugin = base / "vqro-testing.ts"
        skill = base / "skills/vqro-setup-testing/SKILL.md"
        if plugin.is_file() and skill.is_file():
            return ["-e", str(plugin), "--skill", str(skill)]
    # Testing is cooperative: missing optional assets must not stop native Pi work.
    testing_notice(ext, "testing plugin unavailable: Pi setup-testing plugin/skill is missing; launching without testing hooks (NOT VERIFIED)")
    return []


def rpc_testing_args(extensions_dir, ext=None):
    """One named bridge field, never native -e or a development-tree fallback."""
    root = Path(extensions_dir).resolve()
    plugin = root / "tier1/vqro-testing.ts"
    skill = root / "tier1/skills/vqro-setup-testing/SKILL.md"
    try:
        if plugin.resolve(strict=True) == plugin and skill.resolve(strict=True).is_relative_to(root) and plugin.is_file() and skill.is_file():
            return ["--testing-plugin", str(plugin)]
    except (OSError, RuntimeError):
        pass
    testing_notice(ext, "testing plugin unavailable: installed RPC testing assets are missing; launching without testing hooks (NOT VERIFIED)")
    return []


def testing_notice(ext, message):
    """A per-extension-lifetime notice, never a native launch veto."""
    if ext is not None and not getattr(ext, "_testing_unavailable_notified", False):
        ext._testing_unavailable_notified = True
        try:
            ext.notify(message, "warn")
        except Exception:
            pass  # A warning transport failure must not turn optional testing into a launch veto.
        try:
            ext.log(message)
        except Exception:
            try:
                print(message, file=sys.stderr, flush=True)
            except OSError:
                pass
