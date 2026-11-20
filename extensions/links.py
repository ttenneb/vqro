#!/usr/bin/env python3
"""vqro extension: links. Opens what a client found under Ctrl-click or a right-click in a terminal pane.

Clients detect plain URLs and file paths in terminal text (core-protocol.md "Links") and pass the link as
invocation context `link` `{kind: "url"|"path", text, line?, column?}`. What opening means is this extension's
choice (D17):
- a URL goes to the platform's opener (`xdg-open`, macOS `open`, Windows `start`; D12);
- a file opens in `$VISUAL`/`$EDITOR` (else `vi`) in a split beside the pane (`panes.split` with argv), at its
  line when one was given (`+LINE`, which vi, vim, nvim, nano, emacs and helix accept);
- a directory goes to the opener too.
Relative paths resolve against the terminal's current folder (`resource.cwd`, the foreground process's when the
backend reports it), else the layout's folder. Files are looked for on this extension's host (the core's).

`doc/settings/links` `{opener?: argv, editor?: argv}` overrides both programs (argv lists; the link is appended).
Commands: `links.open {link?}` (else the context's link). Only a user's click or menu choice opens anything: agents'
tool calls never open files by themselves (the human's AS10e answer).
"""
import os
import shlex
import subprocess
import sys
import threading

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from vqro_ext import Extension, VqroError  # noqa: E402

ext = Extension("links", event_types=["entries", "resource", "resource.removed"])
SETTING = "doc/settings/links"


def setting(field):
    value = (ext.value(SETTING) or {}).get(field) if isinstance(ext.value(SETTING), dict) else None
    return value if isinstance(value, list) and value and all(isinstance(a, str) and a for a in value) else None


def opener():
    if setting("opener"):
        return setting("opener")
    if sys.platform == "darwin":
        return ["open"]
    if sys.platform.startswith("win"):
        return ["cmd", "/c", "start", ""]
    return ["xdg-open"]


def editor():
    if setting("editor"):
        return setting("editor")
    for var in ("VISUAL", "EDITOR"):
        if os.environ.get(var, "").strip():
            return shlex.split(os.environ[var])
    return ["vi"]


def launch(argv, what):
    """Start the opener detached; report what it said (D10): a failed start now, a failing exit as a notice."""
    try:
        process = subprocess.Popen(argv, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE,
                                   start_new_session=True)
    except OSError as error:
        raise VqroError(f"could not start {argv[0]} ({error.strerror}); set {SETTING}.opener", "unavailable") from None

    def reap():
        _, err = process.communicate()
        if process.returncode:
            detail = err.decode(errors="replace").strip().splitlines()[-1:] or [f"exit {process.returncode}"]
            ext.notify(f"{argv[0]} could not open {what}: {detail[0][:200]}", "error")
    threading.Thread(target=reap, daemon=True).start()
    return {"opened": what, "with": argv[0]}


def resolve(path, ctx, args):
    path = os.path.expanduser(path)
    if os.path.isabs(path):
        return path
    resource = ext.resources.get(ctx.get("resource") or "") or {}
    base = resource.get("cwd") if isinstance(resource.get("cwd"), str) else None
    base = base or ext.layout_folder(args) or os.path.expanduser("~")
    return os.path.normpath(os.path.join(base, path))


@ext.command("links.open", "Open a link from a terminal pane: a URL in the browser, a file in an editor split (args: link)")
def open_link(args):
    ctx = args.get("context") or {}
    link = args.get("link") or ctx.get("link")
    if not isinstance(link, dict) or link.get("kind") not in ("url", "path") or not isinstance(link.get("text"), str) or not link["text"]:
        raise VqroError("no link here (Ctrl-click a URL or file path in a terminal)", "invalid")
    if link["kind"] == "url":
        return launch(opener() + [link["text"]], link["text"])
    path = resolve(link["text"], ctx, args)
    if os.path.isdir(path):
        return launch(opener() + [path], path)
    if not os.path.isfile(path):
        raise VqroError(f"no such file: {path}", "not_found")
    line = link.get("line")
    argv = editor() + ([f"+{line}"] if isinstance(line, int) and line > 0 else []) + [path]
    if "path" not in ctx:
        raise VqroError("open a file from a pane (Ctrl-click or its menu)", "invalid")
    return ext.vqro.call("command.invoke", {"name": "panes.split", "args": {
        "dir": "right", "argv": argv, "cwd": os.path.dirname(path), "context": ctx}})


ext.contribute({
    "menu/links.open-url": {"where": "pane", "label": "Open link", "command": "links.open", "when": {"link_kind": "url"}, "order": 1},
    "menu/links.open-file": {"where": "pane", "label": "Open file in editor", "command": "links.open", "when": {"link_kind": "path"}, "order": 1},
})
ext.run()
