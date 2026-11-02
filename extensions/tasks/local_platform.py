"""Tasks-local OS adapters; no caller authority is inferred from these helpers."""
import os
from functools import lru_cache
from pathlib import Path
import queue
import re
import threading

MAX_FRAME = 262144
_SID = re.compile(r"S-1-(?:0|[1-9][0-9]{0,14})(?:-(?:0|[1-9][0-9]{0,9})){1,15}\Z")


def valid_sid(value):
    if not isinstance(value, str) or not _SID.fullmatch(value):
        return False
    fields = value.split("-")[2:]
    return int(fields[0]) < 2**48 and all(int(part) < 2**32 for part in fields[1:])


def account(binding):
    if "uid" in binding and "sid" not in binding:
        return ("uid", binding["uid"])
    if "sid" in binding and "uid" not in binding:
        return ("sid", binding["sid"])
    return None


@lru_cache(maxsize=1)
def _windows_account():
    # Presentation only; the host independently captures authority per request.
    from windows_api import current_sid
    return ("sid", current_sid())


def local_account():
    if os.name == "nt":
        return _windows_account()
    return ("uid", os.geteuid())


def state_directory(environ=None, windows=None):
    environ = os.environ if environ is None else environ
    windows = os.name == "nt" if windows is None else windows
    if windows:
        from pathlib import PureWindowsPath
        root = environ.get("LOCALAPPDATA", "")
        if not root or "\x00" in root or not PureWindowsPath(root).is_absolute() or PureWindowsPath(root).drive.startswith("\\\\"):
            raise ValueError("LOCALAPPDATA must be an absolute local app-data path; or use --data-dir")
        return str(PureWindowsPath(root) / "vqro/extensions/vqro.tasks")
    return str(Path(environ.get("XDG_STATE_HOME", str(Path.home() / ".local/state"))) / "vqro/extensions/vqro.tasks")


def stdin_lines():
    import atexit
    import msvcrt
    import sys
    descriptor = sys.stdin.fileno()
    msvcrt.setmode(descriptor, os.O_BINARY)
    lines = InputLines(lambda size: os.read(descriptor, size))
    atexit.register(lines.close)
    return lines


class InputLines:
    """One process-lifetime daemon reader for Windows stdin, one queued bounded frame.

    close() stops a producer waiting on its queue. A reader blocked in inherited
    stdin exits with the provider process; no replacement reader is ever started.
    EOF/failure is delivered in order, timeout never consumes a frame.
    """
    def __init__(self, read):
        self.read = read
        self.frames = queue.Queue(maxsize=1)
        self.stopped = threading.Event()
        self.thread = threading.Thread(target=self._run, daemon=True)
        self.thread.start()

    def _put(self, value):
        while not self.stopped.is_set():
            try:
                self.frames.put(value, timeout=0.1)
                return
            except queue.Full:
                pass

    def _run(self):
        pending = bytearray()
        try:
            while not self.stopped.is_set():
                block = self.read(min(65536, MAX_FRAME + 1 - len(pending)))
                if not block:
                    if pending:
                        raise ValueError("provider input ended inside a frame")
                    raise EOFError("provider input closed")
                pending.extend(block)
                while b"\n" in pending:
                    line, _, rest = pending.partition(b"\n")
                    if len(line) > MAX_FRAME:
                        raise ValueError("provider input frame exceeds bound")
                    self._put(line)
                    pending[:] = rest
                if len(pending) > MAX_FRAME:
                    raise ValueError("provider input frame exceeds bound")
        except Exception as error:
            self._put(error)

    def get(self, timeout):
        try:
            item = self.frames.get(timeout=timeout)
        except queue.Empty:
            return None
        if isinstance(item, Exception):
            raise item
        return item

    def close(self):
        self.stopped.set()
