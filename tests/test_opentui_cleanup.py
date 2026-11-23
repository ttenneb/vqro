"""The OpenTUI failure path stops only this fixture's Bun clients, including a TERM-resistant one."""
import ast
import io
from pathlib import Path
import signal
import types
import unittest


class OpenTuiCleanup(unittest.TestCase):
    def test_private_bun_cleanup_escalates_without_touching_other_clients(self):
        source = ast.parse((Path(__file__).parent / "e2e_opentui.py").read_text())
        node = next(n for n in source.body if isinstance(n, ast.FunctionDef) and n.name == "stop_bun_clients")
        env = {"HOME": "/private/home", "VQRO_SOCKET": "/private/p.sock"}
        alive, killed, clock = {10, 11, 12}, [], [0.0]
        def readlink(path):
            pid = int(path.split("/")[2])
            if pid not in alive:
                raise FileNotFoundError
            return "/bin/bun" if pid != 12 else "/bin/provider"
        def opened(path, mode):
            pid = int(path.split("/")[2])
            values = env if pid != 11 else {"HOME": "/someone/else", "VQRO_SOCKET": "/else.sock"}
            return io.BytesIO(b"\0".join(f"{k}={v}".encode() for k, v in values.items()))
        def kill(pid, sig):
            killed.append((pid, sig))
            if sig == signal.SIGKILL:
                alive.remove(pid)
        namespace = {"BUN": "/bin/bun", "ENV": env, "signal": signal, "open": opened,
                     "os": types.SimpleNamespace(listdir=lambda _: [str(p) for p in alive], readlink=readlink,
                                                 path=types.SimpleNamespace(realpath=lambda p: p), kill=kill),
                     "time": types.SimpleNamespace(monotonic=lambda: clock[0], sleep=lambda seconds: clock.__setitem__(0, clock[0] + seconds))}
        exec(compile(ast.Module(body=[node], type_ignores=[]), "e2e_opentui.py", "exec"), namespace)
        namespace["stop_bun_clients"]()
        self.assertEqual(killed, [(10, signal.SIGTERM), (10, signal.SIGKILL)])
        self.assertEqual(alive, {11, 12})
        # The helper is called by finally, not only by the happy path.
        final = next(n for n in source.body if isinstance(n, ast.Try)).finalbody
        self.assertTrue(any(isinstance(n, ast.Call) and isinstance(n.func, ast.Name) and n.func.id == "stop_bun_clients"
                            for statement in final for n in ast.walk(statement)))
