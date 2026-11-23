"""The tier-1 test harness (tests/coord_harness.py) never leaves a core or its extensions running when it fails to
start. A stub core stands in for vqro: it starts a long-lived child (as the core starts its extensions) and then
fails startup in a chosen way. No real core, model or network is used."""
import gc
import os
from pathlib import Path
import sys
import tempfile
import textwrap
import time
import unittest
from unittest import mock
import warnings

sys.path.insert(0, str(Path(__file__).resolve().parent))
from coord_harness import Client, Harness  # noqa: E402
import socket  # noqa: E402
import threading  # noqa: E402

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
    if mode == "mute":  # listen and keep every connection open, but never answer anything
        s = socket.socket(socket.AF_UNIX)
        s.bind(os.environ["VQRO_SOCKET"])
        s.listen(8)
        held = []
        while True:
            held.append(s.accept()[0])
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


@unittest.skipIf(sys.platform == "win32", "private controller uses Unix socket, shebang and /proc process-lifetime fixtures")
@unittest.skipUnless(sys.platform == 'linux', 'startup cleanup probe reads Linux /proc process state')
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
        # Nothing the harness opened (log files, sockets, process handles) may be left for the GC to find.
        with warnings.catch_warnings(record=True) as caught:
            warnings.simplefilter("always", ResourceWarning)
            with self.assertRaises(Exception):
                Harness(str(root), str(stub), inbox="fake", **kwargs)
            gc.collect()
        leaks = [str(w.message) for w in caught if issubclass(w.category, ResourceWarning)]
        self.assertEqual(leaks, [], "a failed start leaves nothing open")
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

    def test_a_core_that_never_answers_fails_startup_within_its_bound(self):
        """Testing audit item 3: a silent peer used to hold the harness in readline until killed from outside."""
        started = time.monotonic()
        core, child = self.failed_start("mute", startup_s=2)
        self.assertLess(time.monotonic() - started, 15, "startup waited past its bound for a silent core")
        self.assertTrue(core, "the core outlived a failed start")
        self.assertTrue(child, "a process the core started outlived a failed start")


class SilentPeerTests(unittest.TestCase):
    @unittest.skipIf(sys.platform == "win32", "private controller fixture listens on AF_UNIX")
    def test_a_call_to_a_silent_peer_times_out_closes_and_is_not_retried(self):
        with tempfile.TemporaryDirectory(prefix="vq-silent-") as d:
            path = os.path.join(d, "s.sock")
            server = socket.socket(socket.AF_UNIX)
            server.bind(path)
            server.listen(1)
            server.settimeout(30)
            received, closed = [], threading.Event()

            def peer():  # accept, read what the client sends, never answer
                try:
                    conn, _ = server.accept()
                except OSError:
                    return
                with conn:
                    # A hang backstop, not a latency budget: the client's own
                    # timeout must close the stream before the observer gives up.
                    deadline = time.monotonic() + 30
                    try:
                        while time.monotonic() < deadline:
                            conn.settimeout(max(0.001, deadline - time.monotonic()))
                            data = conn.recv(65536)
                            if not data:
                                closed.set()
                                break
                            received.append(data)
                    except OSError:
                        pass
            thread = threading.Thread(target=peer, daemon=True)
            thread.start()
            try:
                with warnings.catch_warnings(record=True) as caught:
                    warnings.simplefilter("always", ResourceWarning)
                    with self.assertRaises(TimeoutError) as error:
                        Client(path, timeout=0.5)
                    gc.collect()
            finally:
                thread.join(30)
                server.close()
            self.assertFalse(thread.is_alive(), "silent peer observer did not finish")
            self.assertTrue(closed.is_set(), "client did not close before the peer's hang backstop")
            self.assertIn("hello: no reply within 0.5 s", str(error.exception))
            self.assertEqual(b"".join(received).count(b'"method": "hello"'), 1, "sent once, never retried")
            self.assertEqual([str(w.message) for w in caught if issubclass(w.category, ResourceWarning)], [])


    def test_silent_peer_receive_uses_the_remaining_call_deadline(self):
        # Deadline arithmetic is deterministic. CPU time would exclude the
        # blocking receive, and wall time would charge descheduling/GC to it.
        peer = mock.Mock()
        peer.recv.side_effect = socket.timeout
        with mock.patch.object(socket, "AF_UNIX", 12345, create=True), \
                mock.patch("coord_harness.socket.socket", return_value=peer), \
                mock.patch("coord_harness.time.monotonic", side_effect=[10, 10.125, 10.25]):
            with self.assertRaisesRegex(TimeoutError, "hello: no reply within 0.5 s"):
                Client("unused.sock", timeout=0.5)
        self.assertEqual(peer.settimeout.call_args_list, [mock.call(0.5), mock.call(0.5), mock.call(0.25)])
        peer.recv.assert_called_once_with(65536)
        peer.sendall.assert_called_once()
        self.assertTrue(peer.close.called, "a timed-out stream is unusable")

    def answering_peer(self, d, fragment_gap):
        """A peer that answers hello with one valid reply line, sent whole (fragment_gap None) or in 8-byte fragments
        `fragment_gap` seconds apart. Returns (path, thread, requests received)."""
        path = os.path.join(d, "s.sock")
        server = socket.socket(socket.AF_UNIX)
        server.bind(path)
        server.listen(1)
        received = []

        def peer():
            try:
                conn, _ = server.accept()
                with conn:
                    conn.settimeout(5)
                    data = b""
                    while b"\n" not in data:
                        chunk = conn.recv(65536)
                        if not chunk:
                            return
                        data += chunk
                    received.append(data)
                    reply = b'{"jsonrpc": "2.0", "id": 1, "result": {"you": "p:peer"}}\n'
                    if fragment_gap is None:
                        conn.sendall(reply)
                    else:
                        for start in range(0, len(reply), 8):
                            conn.sendall(reply[start:start + 8])
                            time.sleep(fragment_gap)
            except OSError:
                pass  # the client closed on its deadline
            finally:
                server.close()
        thread = threading.Thread(target=peer, daemon=True)
        thread.start()
        return path, thread, received

    @unittest.skipIf(sys.platform == "win32", "private controller fixture listens on AF_UNIX")
    def test_an_immediate_reply_is_accepted(self):
        with tempfile.TemporaryDirectory(prefix="vq-silent-") as d:
            path, thread, received = self.answering_peer(d, None)
            client = Client(path, timeout=0.25)
            client.close()
            thread.join(6)
            self.assertEqual(client.me, "p:peer")

    @unittest.skipIf(sys.platform == "win32", "private controller fixture listens on AF_UNIX")
    def test_a_reply_dripped_past_the_deadline_times_out(self):
        """Review of 04148cc5: fragments each within any per-receive timeout kept readline alive; a valid reply that
        finished after the total deadline was accepted (0.601 s for a 0.25 s deadline). The total deadline holds."""
        with tempfile.TemporaryDirectory(prefix="vq-silent-") as d:
            path, thread, received = self.answering_peer(d, 0.075)  # ~7 fragments: about 0.5 s in all
            started = time.monotonic()
            with self.assertRaises(TimeoutError) as error:
                Client(path, timeout=0.25)
            elapsed = time.monotonic() - started
            thread.join(6)
            self.assertLess(elapsed, 0.4, "the dripped reply outlasted the total deadline")
            self.assertIn("hello: no reply within 0.25 s", str(error.exception))
            self.assertEqual(len(received), 1, "sent once, never retried")


if __name__ == "__main__":
    unittest.main()
