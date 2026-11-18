#!/usr/bin/env python3
"""Offline, copy-only prim -> vqro state migration. Never starts, stops or contacts a core.

Before running: close old UIs/launchers, stop the old core with `prim stop`, and keep a backup.
Then run this utility, install the new vqro binaries/extensions separately, and start vqro.
The source is retained unchanged. An existing destination is never overwritten; a copy failure
leaves an explicitly reported incomplete destination for manual inspection, never an automatic retry.
No symlinks, native backend changes, process signals, or input/effect replay are performed.

Only known workspace references, complete prim.* command names, installed paths, SDK module names,
reserved environment keys and the built-in keymap profile are translated, including undo before/after
values. Transaction authors/notes/counters, arbitrary user text/project paths and identity/token files
stay unchanged. Review custom executable code or nonstandard install paths manually; use the explicit
extension-directory options for the private old-build rehearsal.

Existing agent bridges/UIs retain old endpoints in their process environments. The human's one-time
endpoint handoff/relaunch is separate; this utility creates no compatibility links or runtime aliases.
The new core reads only VQRO_* names.
"""
import argparse
from collections import Counter
from contextlib import contextmanager
import json
import os
from pathlib import Path
import re
import shutil
import stat
import sys


class MigrationError(ValueError):
    pass


COMMAND = re.compile(r"^prim\.[A-Za-z0-9_.-]+$")
QUARANTINE = re.compile(r"^(view/)?prim-depth-quarantine-*$")
BINARIES = {"prim": "vqro", "prim-herdr": "vqro-herdr", "prim-tmux": "vqro-tmux"}
MODULES = {f"prim_{name}": f"vqro_{name}" for name in
           ("ext", "provider", "provider_stream", "display", "views")}
TRANSIENT = {"serve.pid", ".workspace.lock", "prim.sock", "vqro.sock"}


def state_home():
    return Path(os.environ.get("XDG_STATE_HOME", Path.home() / ".local/state"))


class Translator:
    def __init__(self, source, target, old_extensions, extensions):
        self.counts = Counter()
        self.old_extensions = str(old_extensions)
        self.paths = sorted({
            str(source): str(target),
            str(state_home() / "prim"): str(target),
            str(old_extensions): str(extensions),
        }.items(), key=lambda pair: len(pair[0]), reverse=True)

    def text(self, value):
        if value.startswith("doc/prim/"):
            self.counts["workspace_references"] += 1
            return "doc/vqro/" + value[len("doc/prim/"):]
        if COMMAND.fullmatch(value):
            self.counts["commands"] += 1
            return "vqro." + value[len("prim."):]
        if QUARANTINE.fullmatch(value):
            self.counts["recovery_views"] += 1
            return value.replace("prim-depth-quarantine", "vqro-depth-quarantine", 1)
        for before, after in self.paths:
            if value == before or value.startswith(before + os.sep):
                suffix = value[len(before):]
                if suffix == os.sep + "prim.sock":
                    suffix = os.sep + "vqro.sock"
                parts = suffix.split(os.sep)
                leaf = Path(parts[-1])
                if before == self.old_extensions and leaf.suffix in {".py", ".md"} and leaf.stem in MODULES:
                    parts[-1] = MODULES[leaf.stem] + leaf.suffix
                self.counts["paths"] += 1
                return after + os.sep.join(parts)
        return value

    def node(self, value, entry_key, env=False):
        if isinstance(value, str):
            return self.text(value)
        if isinstance(value, list):
            return [self.node(item, entry_key, env) for item in value]
        if not isinstance(value, dict):
            return value
        result = {}
        for key, item in value.items():
            renamed = self.text(key)
            if env and key.startswith("PRIM_"):
                renamed = "VQRO_" + key[len("PRIM_"):]
                self.counts["environment_names"] += 1
            if renamed in result:
                raise MigrationError(f"renamed dictionary key collision in {entry_key}: {renamed}")
            if key == "PYTHONPATH" and env and isinstance(item, str):
                updated = os.pathsep.join(self.text(part) for part in item.split(os.pathsep))
            else:
                updated = self.node(item, entry_key, key in {"env", "bridge_env"})
            if key in {"argv", "bridge_argv"} and isinstance(item, list):
                if item and isinstance(item[0], str) and Path(item[0]).name in BINARIES:
                    updated[0] = BINARIES[Path(item[0]).name]
                    self.counts["logical_executables"] += 1
                for index in range(1, len(item)):
                    if item[index - 1] == "-m" and isinstance(item[index], str) and item[index] in MODULES:
                        updated[index] = MODULES[item[index]]
                        self.counts["sdk_modules"] += 1
            if entry_key == "doc/keys/profile" and key == "profile" and item == "prim":
                updated = "vqro"
                self.counts["keymap_profiles"] += 1
            if key == "args" and value.get("command") == "keys.profile" and isinstance(updated, dict):
                if updated.get("value") == "prim":
                    updated["value"] = "vqro"
                    self.counts["keymap_profiles"] += 1
            result[renamed] = updated
        return result

    def entry(self, key, entry):
        if not isinstance(entry, dict) or "value" not in entry:
            raise MigrationError(f"invalid workspace entry: {key}")
        return {**entry, "value": self.node(entry["value"], key)}

    def workspace(self, data):
        if not isinstance(data, dict) or not isinstance(data.get("entries"), dict):
            raise MigrationError("workspace.json must contain an entries object")
        entries = {}
        for key, entry in data["entries"].items():
            renamed = self.text(key)
            if renamed in entries:
                raise MigrationError(f"renamed workspace key collision: {renamed}")
            entries[renamed] = self.entry(key, entry)
        data["entries"] = entries
        history = data.get("history", [])
        if not isinstance(history, list):
            raise MigrationError("workspace history must be an array")
        for txn in history:
            for change in txn.get("changes", []):
                key = change["key"]
                change["key"] = self.text(key)
                # Current compact history has before/after_version; legacy after.value is supported too.
                for field in ("before", "after"):
                    if isinstance(change.get(field), dict) and "value" in change[field]:
                        change[field] = self.entry(key, change[field])
        return data


def inventory(source):
    files, directories, skipped = [], [], []
    for parent, dirs, names in os.walk(source, followlinks=False):
        for name in dirs + names:
            path = Path(parent) / name
            relative = path.relative_to(source)
            mode = path.lstat().st_mode
            if stat.S_ISLNK(mode):
                raise MigrationError(f"source contains a symlink; review manually: {relative}")
            if stat.S_ISSOCK(mode) or (not stat.S_ISDIR(mode) and relative.as_posix() in TRANSIENT):
                skipped.append(str(relative))
            elif stat.S_ISDIR(mode):
                directories.append(relative)
            elif stat.S_ISREG(mode):
                files.append(relative)
            else:
                raise MigrationError(f"unsupported source file type: {relative}")
    if Path("workspace.json") not in files:
        raise MigrationError("source has no regular workspace.json")
    return files, directories, skipped


@contextmanager
def stopped_source(source):
    """Coordinate with the current core's state lock, without IPC, signals or source writes."""
    try:
        handle = (source / ".workspace.lock").open("r+b")
    except FileNotFoundError:
        # Older/offline exports may omit the lock; the operator must still stop the old core first.
        yield
        return
    try:
        try:
            if os.name == "nt":
                import msvcrt
                msvcrt.locking(handle.fileno(), msvcrt.LK_NBLCK, 1)
            else:
                import fcntl
                fcntl.flock(handle.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
        except OSError as error:
            raise MigrationError("old core is running or its state lock is unavailable; stop it first") from error
        yield
    finally:
        handle.close()


def migrate(source, target, old_extensions=None, extensions=None):
    source, target = Path(source), Path(target)
    if source.is_symlink() or not source.is_dir():
        raise MigrationError("source must be a real directory, not a symlink")
    if target.exists() or target.is_symlink():
        raise MigrationError("target already exists; refusing to overwrite it")
    source, target = source.resolve(), target.resolve()
    if target == source or target.is_relative_to(source):
        raise MigrationError("target must be outside the old state directory")
    files, directories, skipped = inventory(source)
    translator = Translator(source, target,
        Path(old_extensions or Path.home() / ".local/share/prim/extensions").resolve(),
        Path(extensions or Path.home() / ".local/share/vqro/extensions").resolve())
    with stopped_source(source):
        workspace = translator.workspace(json.loads((source / "workspace.json").read_bytes()))
        encoded = (json.dumps(workspace, ensure_ascii=False, indent=2, allow_nan=False) + "\n").encode()
        target.parent.mkdir(parents=True, exist_ok=True)
        # Exclusive mkdir reserves the destination without a rename-overwrite race or symlinks.
        target.mkdir(mode=0o700)
        try:
            for relative in directories:
                (target / relative).mkdir(mode=0o700)
            for relative in files:
                destination = target / relative
                if relative == Path("workspace.json"):
                    destination.write_bytes(encoded)
                else:
                    shutil.copyfile(source / relative, destination)
                destination.chmod(0o600)
        except Exception as error:
            raise MigrationError(f"copy incomplete at {target}; old state is untouched; inspect target before retrying") from error
    return {"source": str(source), "target": str(target), "copied_files": len(files),
            "skipped_transient_files": sorted(skipped), "rewrites": dict(translator.counts),
            "unchanged_identity_files": [name for name in ("resources.json", "runtime-tokens.json")
                                          if Path(name) in files],
            "old_state_untouched": True, "symlinks_created": 0}


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--source", type=Path, default=state_home() / "prim",
                        help="stopped old state; default: XDG_STATE_HOME/prim or ~/.local/state/prim")
    parser.add_argument("--target", type=Path, default=state_home() / "vqro",
                        help="new absent state; default: XDG_STATE_HOME/vqro or ~/.local/state/vqro")
    parser.add_argument("--old-extensions-dir", type=Path, help="old path prefix for the private rehearsal")
    parser.add_argument("--extensions-dir", type=Path, help="new path prefix; default: ~/.local/share/vqro/extensions")
    args = parser.parse_args()
    try:
        report = migrate(args.source, args.target, args.old_extensions_dir, args.extensions_dir)
    except (MigrationError, OSError, ValueError, TypeError, KeyError) as error:
        parser.exit(1, f"migration refused: {error}\n")
    print(json.dumps(report, indent=2))


if __name__ == "__main__":
    main()
