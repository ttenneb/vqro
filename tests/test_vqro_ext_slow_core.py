"""A slow core must not turn into failed extensions (soak 2 P1-B). Real vqro_ext in a child process
against a scripted core over a Unix socket; replies can be withheld past the client deadline."""
import json
import os
import pathlib
import shutil
import socket
import subprocess
import sys
import tempfile
import threading
import time
import unittest

EXT_DIR = str(pathlib.Path(__file__).resolve().parents[1] / "extensions")


class ScriptedCore:
    """Answers like the core; `policy(method, msg)` may return "hold" to never answer that request."""

    def __init__(self, policy=None):
        self.dir = tempfile.mkdtemp(prefix="pslow", dir="/tmp")
        self.path = os.path.join(self.dir, "s")
        self.srv = socket.socket(socket.AF_UNIX)
        self.srv.bind(self.path)
        self.srv.listen(1)
        self.policy = policy or (lambda method, msg: None)
        self.seq, self.version, self.value = 1, 1, {"n": 0}
        self.calls = []
        self.conn = None
        self.lock = threading.Lock()

    def start(self, code, env=None, stdin=None):
        self.child = subprocess.Popen([sys.executable, "-c", code, EXT_DIR], stdin=stdin, stdout=subprocess.PIPE,
                                      stderr=subprocess.STDOUT, env={"PATH": os.environ.get("PATH", ""), "VQRO_SOCKET": self.path,
                                                                   "VQRO_EXT_DEADLINE_S": "0.5", **(env or {})})
        self.srv.settimeout(10)
        self.conn, _ = self.srv.accept()
        threading.Thread(target=self.serve, daemon=True).start()

    def send(self, obj):
        with self.lock:
            self.conn.sendall((json.dumps(obj) + "\n").encode())

    def snapshot(self):
        return {"epoch": "E", "seq": self.seq, "entries": {"doc/x": {"version": self.version, "value": self.value}}, "resources": []}

    def event(self):
        self.seq += 1
        self.version += 1
        self.value = {"n": self.version}
        self.send({"jsonrpc": "2.0", "method": "event", "params": {"epoch": "E", "seq": self.seq, "type": "entries",
                   "data": {"changes": [{"key": "doc/x", "version": self.version, "value": self.value}]}}})

    def serve(self):
        buf = b""
        try:
            while True:
                data = self.conn.recv(65536)
                if not data:
                    return
                buf += data
                while b"\n" in buf:
                    line, buf = buf.split(b"\n", 1)
                    m = json.loads(line)
                    method = m.get("method")
                    self.calls.append(method)
                    if "id" not in m or method is None:
                        continue
                    if self.policy(method, m) == "hold":
                        continue  # no answer: the client's deadline expires
                    result = {"epoch": "E", "seq": self.seq} if method == "hello" else self.snapshot() if method == "workspace.snapshot" else {}
                    self.send({"jsonrpc": "2.0", "id": m["id"], "result": result})
        except OSError:
            return

    def output(self, wait=0.0):
        time.sleep(wait)
        alive = self.child.poll() is None
        self.child.kill()
        out = self.child.communicate()[0].decode(errors="replace")
        return alive, out

    def close(self):
        if getattr(self, "child", None) and self.child.poll() is None:
            self.child.kill()
            self.child.wait()
        try:
            self.conn and self.conn.close()
        except OSError:
            pass
        self.srv.close()
        shutil.rmtree(self.dir, ignore_errors=True)


CHILD = r'''
import sys, time
sys.path.insert(0, sys.argv[1])
from vqro_ext import Extension
ext = Extension("slowtest", event_types=["entries"])
slow = [True]
@ext.on_event
def seen(kind, data):
    if slow[0]:
        slow[0] = False
        time.sleep(1.0)  # 300 events overflow the queue meanwhile: a local gap
    print("LISTEN", kind, ext.value("doc/x"), flush=True)
print("READY", flush=True)
ext.run()
'''


class SlowCore(unittest.TestCase):
    def test_recovery_snapshot_timeouts_degrade_and_recover_instead_of_exiting(self):
        held = {"n": 0}
        def policy(method, msg):
            # After startup, withhold the next three snapshots (recovery attempts) past the deadline.
            if method == "workspace.snapshot" and core.calls.count("workspace.snapshot") > 1 and held["n"] < 3:
                held["n"] += 1
                return "hold"
        core = ScriptedCore(policy)
        try:
            core.start(CHILD)
            deadline = time.time() + 5
            while "events.subscribe" not in core.calls and time.time() < deadline:
                time.sleep(0.05)
            for _ in range(300):
                core.event()
            time.sleep(1.5)
            core.event()  # arrives while degraded: the stale cache must not drive listeners
            deadline = time.time() + 20  # startup + 3 withheld + the answered retry (bounded backoff)
            while core.calls.count("workspace.snapshot") < 5 and time.time() < deadline:
                time.sleep(0.1)
            alive, out = core.output(wait=1)
        finally:
            core.close()
        self.assertEqual(held["n"], 3, out)
        self.assertNotIn("recovery failed", out)
        self.assertTrue(alive, "a slow core must not make the extension exit:\n" + out)
        self.assertIn("degraded", out)
        self.assertIn("recovered", out)
        lines = out.splitlines()
        during = lines[next(i for i, l in enumerate(lines) if "degraded:" in l):next(i for i, l in enumerate(lines) if "recovered after" in l)]
        self.assertFalse([l for l in during if l.startswith("LISTEN")], "no reactions on a known-stale cache:\n" + out)
        last = [l for l in out.splitlines() if l.startswith("LISTEN")][-1]
        self.assertIn("missed", last, "listeners recompute once the cache is current again:\n" + out)
        self.assertIn(f"'n': {core.version}", last, out)

    def test_startup_waits_out_a_slow_core(self):
        held = {"n": 0}
        def policy(method, msg):
            if method in ("hello", "workspace.snapshot", "events.subscribe") and held["n"] < 3:
                held["n"] += 1
                return "hold"
        core = ScriptedCore(policy)
        try:
            core.start(CHILD)
            deadline = time.time() + 15
            while "events.subscribe" not in core.calls[3:] and time.time() < deadline:
                time.sleep(0.1)
            alive, out = core.output(wait=1)
        finally:
            core.close()
        self.assertTrue(alive, out)
        self.assertIn("READY", out)
        self.assertNotIn("Traceback", out)

    def test_startup_contributions_wait_out_a_slow_core_but_unsafe_calls_do_not_resend(self):
        held = {"apply": 0, "notify": 0}
        def policy(method, msg):
            if method in ("workspace.apply", "ui.notify") and held[method.split(".")[-1]] < 2:
                held[method.split(".")[-1]] += 1
                return "hold"
        code = r'''
import sys
sys.path.insert(0, sys.argv[1])
from vqro_ext import Extension, VqroError
ext = Extension("slowtest")
ext.contribute({"menu/slowtest.x": {"where": "pane", "label": "X", "command": "x"}})  # compare-and-set: resent
try:
    ext.notify("hello")  # not resend-safe: reported, never repeated
except VqroError as e:
    print("NOTIFY", e.kind, flush=True)
print("READY", flush=True)
ext.run()
'''
        core = ScriptedCore(policy)
        try:
            core.start(code)
            deadline = time.time() + 15
            while "events.subscribe" not in core.calls and time.time() < deadline:
                time.sleep(0.1)
            alive, out = core.output(wait=0.5)
        finally:
            core.close()
        self.assertTrue(alive, out)
        self.assertIn("READY", out)
        self.assertEqual(core.calls.count("workspace.apply"), 3, "two withheld CAS writes, then answered")
        self.assertIn("NOTIFY timeout", out)
        self.assertEqual(core.calls.count("ui.notify"), 1)

    def test_a_dead_core_connection_still_exits_for_supervision(self):
        core = ScriptedCore()
        try:
            core.start(CHILD)
            deadline = time.time() + 5
            while "events.subscribe" not in core.calls and time.time() < deadline:
                time.sleep(0.05)
            core.conn.shutdown(socket.SHUT_RDWR)
            rc = core.child.wait(timeout=5)
        finally:
            core.close()
        self.assertEqual(rc, 0)

    def test_normal_interpreter_exit_with_a_lifetime_pipe_is_not_a_fatal_error(self):
        # Soak 2: "Fatal Python error: _enter_buffered_busy: could not acquire lock for <stdin>".
        child = subprocess.Popen([sys.executable, "-c", "import sys, time; sys.path.insert(0, sys.argv[1]); import vqro_ext; time.sleep(0.2)", EXT_DIR],
                                 stdin=subprocess.PIPE, stderr=subprocess.PIPE, env={"PATH": os.environ.get("PATH", ""), "VQRO_EXTENSION_LIFETIME": "stdin"})
        try:
            rc = child.wait(timeout=10)  # stdin stays open: the lifetime thread is mid-read at shutdown
            err = child.stderr.read().decode()
        finally:
            child.stdin.close()
        self.assertNotIn("Fatal Python error", err)
        self.assertEqual(rc, 0)


if __name__ == "__main__":
    unittest.main()
