"""e2e_same_setup Root.stop (review of 16051890, lead-secondary tmux-optin-review.md): the tmux server is stopped only
after vqro-tmux is gone (a suffix match once classified the provider as a backend), and the /proc reads close their
files. Root.stop is extracted from the e2e source and run against deterministic fakes: no process, socket or tmux."""
import ast
import io
from pathlib import Path
import subprocess
from types import SimpleNamespace
import unittest

SOURCE = (Path(__file__).resolve().parent / "e2e_same_setup.py").read_text()


def stop_method():
    root = next(n for n in ast.parse(SOURCE).body if isinstance(n, ast.ClassDef) and n.name == "Root")
    return next(n for n in root.body if isinstance(n, ast.FunctionDef) and n.name == "stop")


def run_stop(provider_exits_at):
    """Fake root: provider vqro-tmux (pid 11) exits at `provider_exits_at` seconds (None: never); tmux (pid 22) is
    started again whenever it is stopped while the provider still runs."""
    events, opened = [], []
    clock = SimpleNamespace(now=0.0)
    processes = {11: b"/fake/target/debug/vqro-tmux\0--tmux\0", 22: b"/usr/bin/tmux\0-S\0"}

    def sleep(seconds):
        clock.now += seconds
        if provider_exits_at is not None and clock.now >= provider_exits_at and 11 in processes:
            processes.pop(11)
            events.append("provider_exited")

    def run(argv, **kwargs):
        if "kill-server" in argv:
            events.append("backend_stop")
            processes.pop(22, None)
            if 11 in processes:
                processes[22] = b"/usr/bin/tmux\0"
                events.append("backend_restarted_by_provider")
        return SimpleNamespace(returncode=0)

    def fake_open(path, mode):
        handle = io.BytesIO(processes[int(path.split("/")[2])])
        opened.append(handle)
        return handle

    class Fake:
        env, r, core = {"HOME": "/fake"}, "/fake", None
        def call(self, name): events.append("core_shutdown")
        def leftovers(self): return list(processes)
        def wait(self, predicate, timeout):
            end = clock.now + timeout
            while clock.now < end:
                if predicate():
                    return True
                sleep(0.25)
            return None

    namespace = {"time": SimpleNamespace(time=lambda: clock.now, sleep=sleep), "HC": "/fake/hc",
                 "subprocess": SimpleNamespace(run=run, TimeoutExpired=subprocess.TimeoutExpired), "open": fake_open}
    exec(compile(ast.fix_missing_locations(ast.Module(body=[stop_method()], type_ignores=[])), "<Root.stop>", "exec"), namespace)
    fake = Fake()
    namespace["stop"](fake)
    return events, fake.leftovers(), opened


class RootStop(unittest.TestCase):
    def test_the_tmux_server_is_stopped_only_after_the_provider_exits(self):
        events, leftovers, _ = run_stop(provider_exits_at=0.5)
        self.assertLess(events.index("provider_exited"), events.index("backend_stop"), events)
        self.assertNotIn("backend_restarted_by_provider", events)
        self.assertEqual(leftovers, [])

    def test_a_provider_that_never_exits_leaves_leftovers_for_the_final_check(self):
        events, leftovers, _ = run_stop(provider_exits_at=None)
        self.assertTrue(leftovers, "fail closed: the e2e's leftovers check then fails")

    def test_proc_reads_close_their_files(self):
        _, _, opened = run_stop(provider_exits_at=0.5)
        self.assertTrue(opened)
        self.assertTrue(all(handle.closed for handle in opened), "every /proc read is closed")


if __name__ == "__main__":
    unittest.main()
