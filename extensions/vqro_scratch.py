"""Automatic first-shell policy for Scratch; explicit user commands never take its claim.

The claim is not a lease: an uncertain automatic effect is never silently repeated after restart.
A user can always open a new shell; once Scratch has a tab the old claim is moot and removed.
"""
import copy
import uuid

from vqro_ext import VqroError, first_tabs
from vqro_display import key_labels, label

CLAIM = "doc/workspaces/first-shell"
PLACEHOLDER = {"kind": "panel", "title": "vqro", "items": [
    {"text": "No terminals yet."}, {"text": "Open the command palette", "command": "client.palette"}]}


def placeholder(ext, node):
    return node == {"view": "vqro-empty"} and ext.value("view/vqro-empty") == PLACEHOLDER


def empty(ext, root=None):
    """No user tabs. Panels outside the tabs are preserved when the first shell is added."""
    root = ext.value("layout/main") if root is None else root
    if root is None:
        return True
    if "tabs" in root:
        return all(placeholder(ext, tab["node"]) for tab in root["tabs"])
    if "view" in root:
        return (ext.value("view/" + root["view"]) or {}).get("kind") == "panel"
    return all(empty(ext, child) for child in root.get("children", []))


def shell_tree(ext, root, tab):
    """Replace only the known recovery placeholder; keep arbitrary user panels/splits."""
    def clean(node):
        if placeholder(ext, node):
            return {"tabs": [], "active": 0}
        node = copy.deepcopy(node)
        if "tabs" in node:
            node["tabs"] = [t for t in node["tabs"] if not placeholder(ext, t["node"])]
        if "children" in node:
            node["children"] = [clean(c) for c in node["children"]]
        return node
    root = clean(root) if root is not None else {"tabs": [], "active": 0}
    tabs = first_tabs(root)
    if tabs is None:
        root = {"tabs": [{"title": "main", "node": root}], "active": 0}
        tabs = root
    tabs["tabs"].append(tab)
    tabs["active"] = len(tabs["tabs"]) - 1
    return root


def new_shell_hint(ext):
    prefs = ext.value("doc/settings/tui")
    prefs = prefs if isinstance(prefs, dict) else {}
    prefix = prefs.get("prefix")
    prefix = prefix if isinstance(prefix, str) and prefix else "C-g"
    return f"press {key_labels(label(prefix, 64), prefs.get('key_labels'))} n for a new one."


def unconfirmed_notice(ext):
    return "An earlier automatic Scratch shell was not confirmed; " + new_shell_hint(ext)


def clear_moot_claim(ext):
    def attempt():
        ext.fresh()
        if not empty(ext) and ext.value(CLAIM) is not None:
            ext.apply([{"op": "delete", "key": CLAIM, "expect": ext.version(CLAIM)}], "Scratch: clear moot automatic claim")
    ext.retrying(attempt, attempts=8)


class FirstShell:
    """Claim before spawn; delete in the same transaction that places its observed terminal."""
    def __init__(self, ext):
        self.ext, self.claimed, self.dispatched = ext, False, False
        self.token = uuid.uuid4().hex

    def __enter__(self):
        def claim():
            self.ext.fresh()
            if not empty(self.ext):
                clear_moot_claim(self.ext)
                return False  # a user action or previous automatic attempt already populated Scratch
            if self.ext.value(CLAIM) is not None:
                self.ext.notify(unconfirmed_notice(self.ext), "warn")
                return False
            self.ext.apply([{"op": "set", "key": CLAIM, "value": {"attempt": self.token}, "expect": 0}], "Scratch: claim first shell")
            self.ext.fresh()
            return True
        self.claimed = self.ext.retrying(claim, attempts=8)
        return self if self.claimed else None

    def spawn(self, params):
        self.dispatched = True
        return self.ext.vqro.call("resource.spawn", params)["resource"]

    def finish_ops(self):
        # Read our exact claim, not a newer caller's generation.
        if self.ext.value(CLAIM) != {"attempt": self.token}:
            raise VqroError("Scratch first-shell claim changed; inspect the observed terminal", "conflict")
        return [{"op": "delete", "key": CLAIM, "expect": self.ext.version(CLAIM)}]

    def __exit__(self, kind, error, tb):
        rejected = isinstance(error, VqroError) and isinstance(error.data, dict) and error.data.get("outcome") == "rejected"
        if self.claimed and (not self.dispatched or rejected):
            self.ext.fresh()
            if self.ext.value(CLAIM) == {"attempt": self.token}:
                self.ext.apply(self.finish_ops(), "Scratch: release unused first-shell claim")
        # An observed-but-unplaced terminal, timeout or lost acknowledgement keeps its claim. No replay.
        return False
