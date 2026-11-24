"""Powerless Windows launch controls; share the native-tested vqro_process helper."""
import ast
import asyncio
import copy
import json
import os
from pathlib import Path
import subprocess
import runpy
import sys
import unittest
from unittest import mock
from types import SimpleNamespace

EXTENSIONS = Path(__file__).resolve().parents[1] / "extensions"
sys.path.insert(0, str(EXTENSIONS))
import vqro_process

NO_WINDOW = 0x08000000
BACKGROUND = {
    ("accounts_keychain.py", "_CommandStore._call"),
    ("pi_bridge/client.py", "PiRpcClient.start"),
    ("pi_bridge/launch.py", "supports_testing"),
    ("testing_integration.py", "extension_main.start"),
    ("testing_perf.py", "extension_main.start"),
    ("testing_unit.py", "tests_row"),
    ("tier1.py", "Tier1.run"),
    ("tmux_server.py", "TmuxServer.call"),
    ("vqro_testing.py", "process_group_members"),
    ("vqro_testing.py", "Sandbox.canary"),
    ("vqro_testing.py", "Sandbox.start"),
    ("vqro_testing.py", "Sandbox.stop"),
    ("workspaces.py", "git"),
    ("workspaces.py", "new_worktree"),
}
INTERACTIVE = {("links.py", "launch")}


class SpawnInventory(ast.NodeVisitor):
    def __init__(self, filename):
        self.filename, self.scope, self.sites = filename, [], {}

    def visit_ClassDef(self, node):
        self.scope.append(node.name)
        self.generic_visit(node)
        self.scope.pop()

    visit_FunctionDef = visit_AsyncFunctionDef = visit_ClassDef

    def visit_Call(self, node):
        target = ast.unparse(node.func)
        synchronous = target.startswith("subprocess.") and target.rsplit(".", 1)[-1] in {
            "Popen", "run", "call", "check_call", "check_output", "getoutput", "getstatusoutput"}
        asynchronous = isinstance(node.func, ast.Attribute) and node.func.attr in {
            "subprocess_exec", "subprocess_shell", "create_subprocess_exec", "create_subprocess_shell"}
        injected = self.filename == "accounts_keychain.py" and target == "self.run"
        if synchronous or asynchronous or injected:
            key = self.filename, ".".join(self.scope)
            if key in self.sites:
                raise AssertionError(f"multiple spawn sites need separate audit entries: {key}")
            self.sites[key] = node
        self.generic_visit(node)


def inventory():
    sites = {}
    for source in EXTENSIONS.rglob("*.py"):
        visitor = SpawnInventory(source.relative_to(EXTENSIONS).as_posix())
        visitor.visit(ast.parse(source.read_text(encoding="utf-8")))
        sites.update(visitor.sites)
    return sites


def capture_options(call, *, filename=None):
    """Evaluate actual expanded kwargs against an inert mocked spawn, never extension entrypoints."""
    keywords = [copy.deepcopy(keyword) if keyword.arg is None else ast.keyword(arg=keyword.arg, value=ast.Constant(None))
                for keyword in call.keywords]
    expression = ast.fix_missing_locations(ast.Expression(ast.Call(func=ast.Name(id="spawn", ctx=ast.Load()),
                                                                  args=[], keywords=keywords)))
    spawn = mock.Mock()
    namespace = {"spawn": spawn, "background_kwargs": vqro_process.background_kwargs, "group_options": {}}
    if filename == "pi_bridge/launch.py":
        namespace["_background_kwargs"] = vqro_process.background_kwargs
    eval(compile(expression, "<background-spawn-keywords>", "eval"), namespace)
    return spawn.call_args.kwargs


def platform(windows):
    # Replace this helper's os binding, not global os.name (which changes pathlib behavior on Linux).
    return mock.patch.object(vqro_process, "os", SimpleNamespace(name="nt" if windows else "posix"))


class BackgroundProcessTests(unittest.TestCase):
    def test_inventory_covers_every_subprocess_site(self):
        self.assertEqual(set(inventory()), BACKGROUND | INTERACTIVE)
        for filename in {filename for filename, _ in BACKGROUND}:
            tree = ast.parse((EXTENSIONS / filename).read_text(encoding="utf-8"))
            if filename == "pi_bridge/launch.py":
                loader = next(node for node in tree.body if isinstance(node, ast.FunctionDef) and node.name == "_background_kwargs")
                expected = ast.parse('''
source = Path(__file__).resolve().parent.parent / "vqro_process.py"
spec = importlib.util.spec_from_file_location("_vqro_bridge_process", source)
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
return module.background_kwargs()
''')
                self.assertEqual([ast.dump(node) for node in loader.body], [ast.dump(node) for node in expected.body],
                                 "isolated launcher must load only installed vqro_process and return its shared kwargs")
                continue
            self.assertTrue(any(isinstance(node, ast.ImportFrom) and node.module == "vqro_process"
                                and any(alias.name == "background_kwargs" for alias in node.names)
                                for node in tree.body), filename + " must import the shared launch helper")

    def test_isolated_bridge_loader_uses_the_same_installed_background_helper(self):
        module=runpy.run_path(str(EXTENSIONS/'pi_bridge/launch.py'),run_name='isolated_launch_test')
        loader=SimpleNamespace(exec_module=mock.Mock())
        for windows in (False,True):
            with self.subTest(windows=windows),platform(windows), \
                 mock.patch.object(subprocess,'CREATE_NO_WINDOW',NO_WINDOW,create=True), \
                 mock.patch.object(module['importlib'].util,'spec_from_file_location',return_value=SimpleNamespace(loader=loader)) as source, \
                 mock.patch.object(module['importlib'].util,'module_from_spec',return_value=vqro_process):
                self.assertEqual(module['_background_kwargs'](),{'creationflags':NO_WINDOW} if windows else {})
                self.assertEqual(source.call_args.args[1],EXTENSIONS/'vqro_process.py')
                loader.exec_module.assert_called_with(vqro_process)

    def test_all_background_sites_add_windows_flag_but_unix_has_no_creationflags(self):
        sites = inventory()
        for windows in (False, True):
            with platform(windows), mock.patch.object(subprocess, "CREATE_NO_WINDOW", NO_WINDOW, create=True):
                for key in sorted(BACKGROUND):
                    with self.subTest(windows=windows, site=key):
                        options = capture_options(sites[key], filename=key[0])
                        if windows:
                            self.assertEqual(options.get("creationflags"), NO_WINDOW)
                        else:
                            self.assertNotIn("creationflags", options)

    def test_desktop_opener_stays_interactive(self):
        with platform(True), mock.patch.object(subprocess, "CREATE_NO_WINDOW", NO_WINDOW, create=True):
            self.assertNotIn("creationflags", capture_options(inventory()[("links.py", "launch")]))

    def test_helper_returns_fresh_kwargs_without_touching_stdio_or_session_options(self):
        with platform(True), mock.patch.object(subprocess, "CREATE_NO_WINDOW", NO_WINDOW, create=True):
            first = vqro_process.background_kwargs()
            self.assertEqual(first, {"creationflags": NO_WINDOW})
            first["stdin"] = "caller change"
            self.assertEqual(vqro_process.background_kwargs(), {"creationflags": NO_WINDOW})
        with platform(False):
            self.assertEqual(vqro_process.background_kwargs(), {})

    @unittest.skipUnless(os.name == "nt", "native Windows console allocation and redirected stdio")
    def test_native_child_has_no_console_and_retains_stdio(self):
        code = (
            "import ctypes,json,sys; k=ctypes.WinDLL('kernel32'); "
            "k.GetConsoleWindow.restype=ctypes.c_void_p; "
            "print(json.dumps({'console': k.GetConsoleWindow() or 0, 'input': sys.stdin.read()})); "
            "sys.stderr.write('stderr intact')"
        )
        environment = {key: value for key, value in os.environ.items() if not key.startswith("HERDR_")}
        result = subprocess.run([sys.executable, "-c", code], input=b"stdin intact", stdout=subprocess.PIPE,
                                stderr=subprocess.PIPE, env=environment, timeout=15, check=True,
                                **vqro_process.background_kwargs())
        self.assertEqual(json.loads(result.stdout), {"console": 0, "input": "stdin intact"})
        self.assertEqual(result.stderr, b"stderr intact")


class AsyncBackgroundProcessTests(unittest.IsolatedAsyncioTestCase):
    async def test_pi_rpc_start_passes_flag_and_preserves_its_stdio(self):
        from pi_bridge.client import PiRpcClient
        loop = asyncio.get_running_loop()
        def discard(coroutine):
            coroutine.close()
            return mock.Mock()
        for windows in (False, True):
            with self.subTest(windows=windows), platform(windows), \
                    mock.patch.object(subprocess, "CREATE_NO_WINDOW", NO_WINDOW, create=True), \
                    mock.patch.object(loop, "subprocess_exec", new_callable=mock.AsyncMock) as spawn, \
                    mock.patch.object(asyncio.subprocess, "Process", return_value=mock.Mock()), \
                    mock.patch.object(asyncio, "create_task", side_effect=discard):
                spawn.return_value = (mock.Mock(), SimpleNamespace(owned_exit=asyncio.Event()))
                await PiRpcClient.start(["pi", "--mode", "rpc"], cwd="project", env={"test": "private"})
                options = spawn.call_args.kwargs
                self.assertEqual((options["stdin"], options["stdout"], options["stderr"]), (subprocess.PIPE,) * 3)
                self.assertEqual((options["cwd"], options["env"]), ("project", {"test": "private"}))
                self.assertEqual(spawn.call_args.args[1:], ("pi", "--mode", "rpc"))
                if windows:
                    self.assertEqual(options["creationflags"], NO_WINDOW)
                else:
                    self.assertNotIn("creationflags", options)


if __name__ == "__main__":
    unittest.main()
