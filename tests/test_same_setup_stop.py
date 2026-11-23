"""e2e_same_setup Root.stop, run on deterministic fakes (no process, socket or tmux): the extracted method from the
e2e source, with fake /proc, clock, subprocess and os.kill.

- The backends are stopped only after the provider (vqro-tmux) is gone; programs in a backend's panes do not hold the
  wait (review of 16051890 and the lead's ee05078f gate: the provider-name suffix match, then a wait that pane
  programs could never satisfy).
- A server that survives its own stop is reported, killed and kept in `forced`, so its own check fails visibly.
- Every /proc read is closed."""
import ast
import io
from pathlib import Path
import subprocess
from types import SimpleNamespace
import unittest

SOURCE = (Path(__file__).resolve().parent / "e2e_same_setup.py").read_text(encoding="utf-8")
PROVIDER, SERVER, PANE = 11, 22, 33


def stop_method():
    root = next(n for n in ast.parse(SOURCE).body if isinstance(n, ast.ClassDef) and n.name == "Root")
    return next(n for n in root.body if isinstance(n, ast.FunctionDef) and n.name == "stop")


def run_stop(provider_exits_at, server_obeys_kill=True):
    """Provider vqro-tmux exits at `provider_exits_at` seconds (None: never); the tmux server runs a shell in a pane;
    kill-server ends both unless `server_obeys_kill` is false, and a live provider starts the server again."""
    events, opened, killed = [], [], []
    clock = SimpleNamespace(now=0.0)
    procs = {PROVIDER: (1, b"/target/debug/vqro-tmux\0--tmux\0"), SERVER: (1, b"/usr/bin/tmux\0-S\0/r/st/tmux.sock\0"),
             PANE: (SERVER, b"/bin/sh\0")}

    def sleep(seconds):
        clock.now += seconds
        if provider_exits_at is not None and clock.now >= provider_exits_at and PROVIDER in procs:
            procs.pop(PROVIDER)
            events.append("provider_exited")

    def run(argv, **kwargs):
        if "kill-server" in argv:
            events.append("backend_stop")
            if server_obeys_kill:
                procs.pop(SERVER, None)
                procs.pop(PANE, None)
                if PROVIDER in procs:
                    procs[SERVER] = (1, b"/usr/bin/tmux\0")
                    events.append("backend_restarted_by_provider")
        return SimpleNamespace(returncode=0)

    def fake_open(path, mode="r"):
        pid, what = int(path.split("/")[2]), path.split("/")[3]
        parent, cmdline = procs[pid]
        handle = io.BytesIO(cmdline) if what == "cmdline" else io.StringIO(f"{pid} (x) S {parent} 0 0")
        opened.append(handle)
        return handle

    def kill(pid, signal):
        killed.append(pid)
        procs.pop(pid, None)

    class Fake:
        env, r, core, backend, forced = {"HOME": "/r"}, "/r", None, "tmux", {}
        def call(self, name): events.append("core_shutdown")
        def leftovers(self): return [str(pid) for pid in procs]
        def wait(self, predicate, timeout):
            end = clock.now + timeout
            while clock.now < end:
                if predicate():
                    return True
                sleep(0.25)
            return None

    namespace = {"time": SimpleNamespace(time=lambda: clock.now, sleep=sleep), "HC": "/fake/hc", "print": lambda *a, **k: None,
                 "subprocess": SimpleNamespace(run=run, TimeoutExpired=subprocess.TimeoutExpired), "open": fake_open,
                 "os": SimpleNamespace(kill=kill)}
    exec(compile(ast.fix_missing_locations(ast.Module(body=[stop_method()], type_ignores=[])), "<Root.stop>", "exec"), namespace)
    fake = Fake()
    fake.forced = {}
    namespace["stop"](fake)
    return SimpleNamespace(events=events, leftovers=fake.leftovers(), opened=opened, killed=killed, forced=fake.forced, seconds=clock.now)


class RootStop(unittest.TestCase):
    def test_backends_stop_after_the_provider_and_pane_programs_do_not_hold_the_wait(self):
        r = run_stop(provider_exits_at=0.5)
        self.assertLess(r.events.index("provider_exited"), r.events.index("backend_stop"), r.events)
        self.assertNotIn("backend_restarted_by_provider", r.events)
        self.assertEqual((r.leftovers, r.forced, r.killed), ([], {}, []))
        self.assertLess(r.seconds, 2, "the wait ends when the provider is gone, not after its whole bound")

    def test_a_server_that_survives_its_own_stop_is_reported_and_killed(self):
        r = run_stop(provider_exits_at=0.5, server_obeys_kill=False)
        self.assertEqual(sorted(r.forced), [SERVER, PANE], r.forced)
        self.assertEqual(sorted(r.killed), [SERVER, PANE])
        self.assertEqual(r.leftovers, [])

    def test_a_provider_that_never_exits_is_killed_and_reported_too(self):
        r = run_stop(provider_exits_at=None)
        self.assertIn(PROVIDER, r.forced)
        self.assertEqual(r.leftovers, [])

    def test_proc_reads_close_their_files(self):
        r = run_stop(provider_exits_at=0.5)
        self.assertTrue(r.opened)
        self.assertTrue(all(handle.closed for handle in r.opened), "every /proc read is closed")


if __name__ == "__main__":
    unittest.main()
