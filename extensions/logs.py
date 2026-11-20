#!/usr/bin/env python3
"""vqro extension: logs. An in-app viewer for extension logs (`<state dir>/ext-<name>.log`, core host).

It is also the review's P2 test of an unfamiliar interaction model (a following, filterable log tail) built
only from public primitives; `docs/research/log-viewer-p2.md` records every host change it forced.

- `view/logs.tail` has its own kind `logs.tail` (a native tail a richer client may render) with
  `fallback: "logs.rows"`: a panel every client renders. No client renders `logs.tail` today, so everyone sees
  the fallback; nothing here depends on a client-specific renderer.
- The panel asks clients to `follow` the end while the reader stays there; `/` filters as in any panel.
- Lines are read from the end of the current file (and its rotated `.1` when the current one is short), bounded
  to READ_BYTES and MAX_LINES, control sequences removed. While a view is open or was opened recently, the file
  is checked once a second (a stat; reread only on change).

Commands: `logs.open {name?}` shows the viewer (popup), `logs.show {value}` switches extension, `logs.refresh`.
Turn it off to lose the viewer; logs keep being written by the core.
"""
import os
import re
import sys
import threading
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from vqro_ext import Extension, VqroError, views_in  # noqa: E402
from vqro_display import label  # noqa: E402

READ_BYTES = 128 * 1024
MAX_LINES = 400
LINE_CHARS = 500
ACTIVE_SECONDS = 600     # keep tailing this long after the last open/switch (popups are client-local)
ANSI = re.compile(r"\x1b\[[0-9;?]*[ -/]*[@-~]|\x1b\][^\x07\x1b]*(\x07|\x1b\\)|\x1b[@-_]")
ERROR = re.compile(r"error|traceback|panicked|failed|exception", re.I)
WARN = re.compile(r"warn|degraded|retrying|timeout", re.I)

ext = Extension("logs", event_types=["entries", "extension"])
lock = threading.RLock()
state = {"name": None, "stamp": None, "until": 0.0}


def log_path(name, suffix=""):
    root = ext.info.get("state_dir")
    return os.path.join(root, f"ext-{name}.log{suffix}") if root else None


def declared():
    return sorted(k[len("extension/"):] for k in ext.entries if k.startswith("extension/"))


def stamp(path):
    try:
        st = os.stat(path)
        return (st.st_mtime_ns, st.st_size)
    except OSError:
        return None


def tail(name):
    """The last lines of an extension's log (rotated file first if needed), cleaned and bounded."""
    data = b""
    for path in (log_path(name), log_path(name, ".1")):
        if not path:
            continue
        try:
            with open(path, "rb") as f:
                f.seek(0, os.SEEK_END)
                size = f.tell()
                want = READ_BYTES - len(data)
                f.seek(max(0, size - want))
                data = f.read(want) + data
        except OSError:
            continue
        if len(data) >= READ_BYTES or data.count(b"\n") >= MAX_LINES:
            break
    lines = data.decode("utf-8", "replace").splitlines()[-MAX_LINES:]
    return [label(ANSI.sub("", line).replace("\t", "    "), LINE_CHARS) for line in lines]


def style(line):
    return "error" if ERROR.search(line) else "warn" if WARN.search(line) else None


def content():
    name = state["name"]
    names = declared()
    rows = [{"type": "select", "text": "Extension", "value": name, "command": "logs.show",
             "options": [{"label": n, "value": n} for n in names]},
            {"type": "chips", "chips": [{"text": "Refresh", "command": "logs.refresh"},
                                        {"text": "Open in a tab", "command": "tabs.show", "args": {"view": "logs.tail", "title": "Logs"}}]}]
    if not ext.info.get("state_dir"):
        return {"title": "Extension logs", "toolbar": rows, "items": [{"type": "text", "text": "This core has no state directory, so there are no log files."}]}
    if name is None:
        return {"title": "Extension logs", "toolbar": rows, "items": [{"type": "text", "text": "Choose an extension."}]}
    lines = tail(name)
    items = [{"type": "text", "text": line or " ", "focusable": True, **({"style": style(line)} if style(line) else {})} for line in lines]
    if not items:
        items = [{"type": "text", "text": f"No log lines yet ({log_path(name)})."}]
    return {"title": f"Log: {name}", "follow": True, "toolbar": rows,
            "items": [{"type": "text", "style": "dim", "text": f"{log_path(name)} · last {len(lines)} lines · / filters"}] + items}


def publish(force=False):
    with lock:
        if state["name"] is None or state["name"] not in declared():
            names = declared()
            state["name"] = names[0] if names else None
        current = stamp(log_path(state["name"])) if state["name"] else None
        if not force and current == state["stamp"]:
            return
        state["stamp"] = current
        ext.publish("logs.rows", content())


def active():
    if time.monotonic() < state["until"]:
        return True
    return any("logs.tail" in views_in(e.get("value") or {}) or "logs.rows" in views_in(e.get("value") or {})
               for k, e in list(ext.entries.items()) if k.startswith("layout/"))


def poll():
    while True:
        time.sleep(1)
        try:
            if active():
                publish()
        except VqroError:
            pass  # a slow core: try again next second


@ext.command("logs.open", "Show extension logs (args: name)")
def open_logs(args):
    with lock:
        name = args.get("name") or args.get("value")
        if name:
            if name not in declared():
                raise VqroError(f"no extension {name}", "not_found")
            state["name"] = name
        state["until"] = time.monotonic() + ACTIVE_SECONDS
        publish(force=True)
    return {"client": {"popup": {"view": "logs.tail", "width": 120}}}


@ext.command("logs.show", "Switch the log viewer to an extension (args: value)")
def show(args):
    name = args.get("value") or args.get("name")
    if name not in declared():
        raise VqroError(f"no extension {name}", "not_found")
    with lock:
        state["name"] = name
        state["until"] = time.monotonic() + ACTIVE_SECONDS
        publish(force=True)


@ext.command("logs.refresh", "Reread the shown log")
def refresh(args):
    with lock:
        state["until"] = time.monotonic() + ACTIVE_SECONDS
        publish(force=True)


@ext.on_event
def on_event(t, d):
    if t == "missed" or (t == "entries" and any(c["key"].startswith("extension/") for c in d.get("changes", []))):
        publish(force=True)


ext.contribute({
    # Its own kind for a native tail; every client renders the fallback panel today (D29).
    "view/logs.tail": {"kind": "logs.tail", "title": "Extension logs", "fallback": "logs.rows"},
    "view/logs.rows": {"kind": "panel", "title": "Extension logs", "items": [{"type": "text", "text": "no content: the logs extension is starting or stopped"}]},
})
publish(force=True)
threading.Thread(target=poll, daemon=True, name="logs-tail").start()
ext.run()
