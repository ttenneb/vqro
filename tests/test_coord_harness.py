"""The tier-1 test harness (tests/coord_harness.py) never leaves a core or its extensions running when it fails to
start. A stub core stands in for vqro: it starts a long-lived child (as the core starts its extensions) and then
fails startup in a chosen way. No real core, model or network is used."""
import os
from pathlib import Path
import sys
import tempfile
import textwrap
import time
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parent))
from coord_harness import Harness  # noqa: E402

STUB = textwrap.dedent("""\
    #!/usr/bin/env python3
    import os, socket, subprocess, sys, time
    child = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(600)"])
    with open(os.path.join(os.environ["VQRO_STATE_DIR"], "pids"), "w") as f:
        f.write(f"{os.getpid()} {child.pid}")
    mode = os.environ.get("STUB_MODE", "{mode}")
    if mode == "hello":  # listen, but close every connection before answering hello
        s = socket.socket(socket.AF_UNIX)
        s.bind(os.environ["VQRO_SOCKET"])
        s.listen(8)
        while True:
            s.accept()[0].close()
    time.sleep(600)  # "silent": never create the socket
    """)


def alive(pid):
    try:
        os.kill(pid, 0)
    except (ProcessLookupError, PermissionError):  # gone (a PermissionError means the pid now belongs to another user)
        return False
    # A zombie is dead for our purposes (its parent has not reaped it yet). The process can also exit between
    # the check above and this read: reading /proc/<pid>/stat then fails with ESRCH or ENOENT, which is gone too.
    try:
        return Path(f"/proc/{pid}/stat").read_text().split(") ", 1)[1][0] not in "ZX"
    except (ProcessLookupError, FileNotFoundError, IndexError):
        return False


def gone(pid, deadline):
    """True once `pid` stays gone for two consecutive probes (an exiting process can briefly read as gone between
    states), or False if it is still alive at the deadline."""
    misses = 0
    while time.time() < deadline:
        misses = misses + 1 if not alive(pid) else 0
        if misses >= 2:
            return True
        time.sleep(0.05)
    return not alive(pid)


class StartupFailureTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="vq-harness-")
        self.root = Path(self.temp.name)

    def tearDown(self):
        # Never leak the stub's processes, even when the harness under test fails to stop them.
        for path in self.root.glob("*/st/pids"):
            for pid in map(int, path.read_text().split()):
                try:
                    os.kill(pid, 9)
                except ProcessLookupError:
                    pass
        self.temp.cleanup()

    def failed_start(self, mode, **kwargs):
        stub = self.root / f"stub-{mode}"
        stub.write_text(STUB.replace("{mode}", mode))
        stub.chmod(0o700)
        root = self.root / mode
        with self.assertRaises(Exception):
            Harness(str(root), str(stub), inbox="fake", **kwargs)
        core, child = map(int, (root / "st" / "pids").read_text().split())
        deadline = time.time() + 5
        return gone(core, deadline), gone(child, deadline)

    def test_core_that_never_opens_its_socket_is_stopped_with_its_children(self):
        core, child = self.failed_start("silent", startup_s=1)
        self.assertTrue(core, "the core outlived a failed start")
        self.assertTrue(child, "a process the core started outlived a failed start")

    def test_failure_after_the_socket_exists_stops_everything_too(self):
        core, child = self.failed_start("hello")
        self.assertTrue(core, "the core outlived a failed hello")
        self.assertTrue(child, "a process the core started outlived a failed hello")


if __name__ == "__main__":
    unittest.main()
