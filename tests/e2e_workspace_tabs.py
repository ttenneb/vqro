#!/usr/bin/env python3
"""Private end-to-end check: each workspace has its own tabs, over the public protocol.

A scratch core (`vqro serve --fake`, in-memory fake backend only) runs the bundled tabs, panes, workspaces
and sidebar extensions. Two workspaces get separate tabs; a new tab spawns in the workspace's folder;
switching back restores the tabs without spawning; switching is the invoker's undoable change (D24); with
the workspaces extension disabled, tabs still work on `main` and the sidebar still lists every layout.
No live vqro, hc/herdr-core or tmux is touched (private env, `VQRO_HERDR=/bin/false`).

usage: e2e_workspace_tabs.py <cargo target dir>   (uses <target>/debug/vqro)
"""
from python_interpreter import python
from extension_layout import copy_packages
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import sys
import tempfile
import threading
import time

REPO = Path(__file__).resolve().parents[1]
EXTENSIONS = ("tabs", "panes", "workspaces", "sidebar")


class Client:
    def __init__(self, endpoint):
        self.sock = socket.socket(socket.AF_UNIX)
        self.sock.connect(endpoint)
        self.reader, self.writer = self.sock.makefile("rb"), self.sock.makefile("wb", buffering=0)
        self.lock, self.cv, self.ident, self.replies = threading.Lock(), threading.Condition(), 0, {}
        self.thread = threading.Thread(target=self.read, daemon=True)
        self.thread.start()
        self.you = self.call("hello", {"client": "e2e-workspace-tabs", "identity": {"kind": "owner"}})["you"]

    def read(self):
        for wire in self.reader:
            row = json.loads(wire)
            if "method" not in row:
                with self.cv:
                    self.replies[row["id"]] = row
                    self.cv.notify_all()

    def call(self, method, params=None, ok=True):
        with self.lock:
            self.ident += 1
            ident = self.ident
            self.writer.write((json.dumps({"jsonrpc": "2.0", "id": ident, "method": method, "params": params or {}})+"\n").encode())
        with self.cv:
            assert self.cv.wait_for(lambda: ident in self.replies, 60), method
            row = self.replies.pop(ident)
        if ok:
            assert "error" not in row, (method, params, row)
            return row["result"]
        return row

    def run(self, command, **args):
        return self.call("command.invoke", {"name": command, "args": args})

    def close(self):
        try:
            self.sock.shutdown(socket.SHUT_RDWR)
        except OSError:
            pass
        self.thread.join(2)
        self.reader.close(); self.writer.close(); self.sock.close()


def wait(predicate, what, timeout=30):
    end = time.monotonic()+timeout
    while time.monotonic() < end:
        value = predicate()
        if value:
            return value
        time.sleep(.05)
    raise AssertionError("timed out: "+what)


def walk(n):
    yield n
    for c in n.get("children", []):
        yield from walk(c)
    for t in n.get("tabs", []):
        yield from walk(t["node"])


def views(n):
    return [x["view"] for x in walk(n) if "view" in x]


def main():
    target = Path(sys.argv[1]).resolve()
    vqro = target/"debug/vqro"
    root = Path(tempfile.mkdtemp(prefix="pws", dir="/tmp"))
    core, client, checks = None, None, []
    try:
        for name in ("home", "state", "run", "ext", "a", "b"):
            (root/name).mkdir()
        for source in (REPO/"extensions").glob("*.py"):
            shutil.copy2(source, root/"ext"/source.name)
        copy_packages(REPO/'extensions',root/'ext')
        (root/"ext/defaults.json").write_text(json.dumps({"version": 1, "extensions": [
            {"name": n, "argv": [python(), str(root/"ext"/(n+".py"))]} for n in EXTENSIONS]}))
        env = {"HOME": str(root/"home"), "PATH": "/usr/bin:/bin", "LANG": "C.UTF-8",
               "VQRO_STATE_DIR": str(root/"state"), "VQRO_SOCKET": str(root/"p.sock"), "VQRO_EXTENSIONS_DIR": str(root/"ext"),
               "XDG_RUNTIME_DIR": str(root/"run"), "XDG_STATE_HOME": str(root/"state"), "XDG_CONFIG_HOME": str(root/"home"),
               "HC_SOCKET": str(root/"h.sock"), "HC_SESSION": "e2e-workspace-tabs", "VQRO_HERDR": '["/bin/false"]'}
        with (root/"serve.log").open("w") as log:
            core = subprocess.Popen([str(vqro), "serve", "--fake", "--safe"], env=env, cwd=root, stdout=log, stderr=subprocess.STDOUT)
        wait(lambda: (root/"p.sock").exists(), "core socket")
        client = Client(env["VQRO_SOCKET"])
        # This fixture supplies its own layouts; disable bootstrap before enabling extensions.
        client.call("workspace.apply", {"ops": [{"op": "set", "key": "doc/settings/workspaces", "value": {"open_shell_when_empty": False}}]})
        client.call("command.invoke", {"name": "vqro.safe_mode", "args": {"on": False}})
        snap = lambda: client.call("workspace.snapshot")
        wait(lambda: {"tabs.open", "workspaces.open", "workspaces.switch", "sidebar.tab"} <= {c["name"] for c in client.call("command.list")["commands"]},
             "extension commands")

        def entry(key):
            e = client.call("workspace.get", {"key": key})
            return e["value"] if e else None

        def active():
            return entry("workspace/active")["layout"]

        def tabs_of(layout):
            return next(n for n in walk(entry(f"layout/{layout}")) if "tabs" in n)["tabs"]

        def resources():
            return {r["id"]: r for r in snap()["resources"]}

        a, b = str(root/"a"), str(root/"b")
        # Before any workspace: an ordinary tab lands in main.
        client.run("tabs.open")
        assert active() == "main"
        client.run("workspaces.add", text=a)
        client.run("workspaces.add", text=b)
        client.run("workspaces.open", path=a)
        la = active()
        assert la.startswith("ws-") and len(tabs_of(la)) == 1, la
        assert entry(f"doc/layout/{la}") == {"cwd": a, "title": "a"}
        rid = entry("view/"+views(tabs_of(la)[0]["node"])[0])["resource"]
        assert resources()[rid]["cwd"] == a, resources()[rid]
        checks.append("workspace a: own layout, one shell in its folder")
        client.run("tabs.open")
        assert len(tabs_of(la)) == 2
        new = entry("view/"+views(tabs_of(la)[1]["node"])[0])["resource"]
        assert resources()[new]["cwd"] == a
        assert len(tabs_of("main")) == 1
        checks.append("new tab in workspace a spawned in a; main untouched")
        client.run("workspaces.open", path=b)
        lb = active()
        assert lb != la and len(tabs_of(lb)) == 1
        count = len(resources())
        client.run("workspaces.open", path=a)
        assert active() == la and len(tabs_of(la)) == 2 and len(resources()) == count
        checks.append("workspace b separate; switching back to a restores its two tabs without spawning")
        # D24: the switch is the invoker's change; default undo takes it back.
        last = client.call("workspace.history", {"limit": 1})["txns"][0]
        assert last.get("principal") == "owner" and last.get("initiator") == "owner" and last["actor"] == "extension:workspaces", last
        assert last["author"].startswith("client:"), last
        client.run("vqro.revert")
        assert active() == lb, active()
        checks.append("switch attributed to the invoker (D24) and undone by default undo")
        # Sidebar: the workspace row holds its tabs.
        def sidebar_row(prefix):
            def find(rows):
                for r in rows:
                    if r.get("id", "").startswith(prefix):
                        return r
                    hit = find(r.get("children", []))
                    if hit:
                        return hit
            return find((snap().get("live", {}).get("sidebar") or {}).get("items", []))
        ws_a = wait(lambda: (lambda r: r if r and sum(1 for c in r.get("children", []) if c["id"].startswith("tab:")) == 2 else None)(sidebar_row("ws:"+a)), "sidebar tabs under workspace a")
        tab = next(c for c in ws_a["children"] if c["id"] == f"tab:{la}:0")
        client.run(tab["command"], **tab["args"])
        assert active() == la and next(n for n in walk(entry(f"layout/{la}")) if "tabs" in n)["active"] == 0
        checks.append("sidebar lists a's tabs; clicking one switches to it")
        # Workspaces off: tabs still work on main, the sidebar lists every layout.
        client.run("vqro.extension.disable", name="workspaces")
        wait(lambda: not snap()["extensions"]["workspaces"]["running"], "workspaces stopped")
        # main is listed either way; without workspace data the workspaces' layouts join it.
        wait(lambda: sidebar_row(f"layout:{la}") and sidebar_row(f"layout:{lb}"), "sidebar layouts without workspaces")
        main_row = sidebar_row("layout:main")
        client.run(main_row["command"], **main_row["args"])
        assert active() == "main"
        client.run("tabs.open")
        assert len(tabs_of("main")) == 2
        newest = entry("view/"+views(tabs_of("main")[1]["node"])[0])["resource"]
        assert resources()[newest].get("cwd") in (None, ""), resources()[newest]
        checks.append("workspaces disabled: tabs work on main; sidebar lists all layouts")
        assert all(r.get("state") == "live" for r in resources().values()), "nothing was killed"
        print(json.dumps({"result": "pass", "checks": checks}, indent=2))
    finally:
        if client:
            try:
                client.call("core.shutdown")
            except Exception:
                pass
            client.close()
        if core:
            try:
                core.wait(timeout=10)
            except subprocess.TimeoutExpired:
                core.terminate(); core.wait(timeout=5)
        shutil.rmtree(root, ignore_errors=True)


if __name__ == "__main__":
    main()
