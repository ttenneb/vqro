#!/usr/bin/env python3
"""Opt-in e2e: the same workspace behaves the same on herdr and on tmux ("same setup anywhere").

Two fresh private roots, each with both providers connected (vqro-herdr on a private herdr-core, vqro-tmux on its
private tmux server); they differ only in which one is the default backend. Each runs one scripted workspace:
a titled shell tab split in two, a renamed second tab, the sidebar on, tab marks on, and a fake Pi agent
(`pi.new`, tests/fixtures/fake_pi.py through the Pi bridge). What a client sees is captured as structure, not
pixels: the layout tree, views, resources (normalized ids), the sidebar's live rows, tab marks/states, available
commands, and the text client's outline. The two captures are compared; every difference is reported, and those
not listed as an expected backend capability difference (EXPECTED below) fail the test.

Usage: tests/e2e_same_setup.py TARGET_DIR [--keep-captures DIR]   (debug binaries in TARGET_DIR/debug)
Private /tmp roots; nothing live is touched. Stops only what it started.
"""

import sys
if sys.platform != "linux":
    print("SKIP e2e_same_setup: requires Linux /proc and namespace/process probes")
    raise SystemExit(77)

from python_interpreter import python
import json, os, re, shutil, socket, subprocess, sys, tempfile, time

TARGET = sys.argv[1]
KEEP = sys.argv[sys.argv.index("--keep-captures") + 1] if "--keep-captures" in sys.argv else None
VQRO, PHERDR, PTMUX = (os.path.join(TARGET, "debug", b) for b in ("vqro", "vqro-herdr", "vqro-tmux"))
HC = os.path.expanduser("~/.local/bin/herdr-core")
REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
CALL_TIMEOUT = 20
SHELL = "/bin/sh"  # the host shell the private roots declare ($SHELL): an empty argv must run it on every backend
results = []

# Differences that come from what each backend is, not from a bug: (path regex, why).
EXPECTED = [
    (r"^resources\.terminal\d+\.provider$", "each terminal names the backend that owns it"),
]
HOST = socket.gethostname()
# Differences that are known bugs, each with its finding (qol/same-setup-findings.md), matched by exact path and
# value shape so anything else fails. F1-F4 (tmux reported its window name, default pane title and no argv, and ran
# another default shell) are fixed; none is known now.
KNOWN = []


def check(name, ok, detail=""):
    results.append(bool(ok))
    print(("PASS " if ok else "FAIL ") + name + (f"\n{detail}" if detail and not ok else ""), flush=True)
    return ok


class Root:
    def __init__(self, backend):
        self.backend = backend
        self.r = tempfile.mkdtemp(prefix=f"pss{backend[0]}", dir="/tmp")
        for d in ("home", "st", "xst", "run", "cfg", "bin", "work"):
            os.makedirs(os.path.join(self.r, d))
        os.chmod(os.path.join(self.r, "run"), 0o700)
        shutil.copytree(os.path.join(REPO, "extensions"), os.path.join(self.r, "ext"),
                        ignore=shutil.ignore_patterns("node_modules", "opentui", "__pycache__"))
        os.symlink(python(), os.path.join(self.r, "bin", "python3"))
        os.symlink(os.path.join(TARGET, "debug", "vqro-bridge-pi"), os.path.join(self.r, "bin", "vqro-bridge-pi"))
        # The tmux-server extension's status helper, as an install provides it. Without it every status call failed
        # (ENOENT) once the owned server existed, and the extension, which declared its view only after a successful
        # call, never declared it when its first poll came after the tmux provider had started the server: the
        # e2e-same-setup failures under full-gate load (e332e800, e1b0e858).
        os.symlink(os.path.join(TARGET, "debug", "vqro-tmux"), os.path.join(self.r, "bin", "vqro-tmux"))
        os.symlink("/usr/bin/tmux", os.path.join(self.r, "bin", "tmux"))  # which the helper runs, as on a real PATH
        self.env = {"HOME": f"{self.r}/home", "PATH": f"{self.r}/bin", "TERM": "xterm-256color", "SHELL": SHELL,
                    "VQRO_STATE_DIR": f"{self.r}/st", "VQRO_SOCKET": f"{self.r}/p.sock", "VQRO_EXTENSIONS_DIR": f"{self.r}/ext",
                    "XDG_STATE_HOME": f"{self.r}/xst", "XDG_RUNTIME_DIR": f"{self.r}/run", "XDG_CONFIG_HOME": f"{self.r}/cfg",
                    "HC_SESSION": "same", "HC_SOCKET": f"{self.r}/hc.sock", "LANG": "C.UTF-8"}
        self.core = None
        self.forced = {}  # pid -> [parent, argv] of anything stop() had to kill (see stop)

    def call(self, method, params=None):
        deadline = time.monotonic() + CALL_TIMEOUT
        with socket.socket(socket.AF_UNIX) as s:
            s.settimeout(CALL_TIMEOUT)
            s.connect(self.env["VQRO_SOCKET"])
            buf = bytearray()
            for i, (m, p) in enumerate([("hello", {"client": "e2e-same-setup"}), (method, params or {})]):
                s.sendall((json.dumps({"jsonrpc": "2.0", "id": i, "method": m, "params": p}) + "\n").encode())
                while True:
                    if time.monotonic() >= deadline:
                        raise TimeoutError(f"{method}: no reply within {CALL_TIMEOUT} s")
                    end = buf.find(b"\n")
                    if end < 0:
                        s.settimeout(max(0.001, deadline - time.monotonic()))
                        chunk = s.recv(65536)
                        if not chunk:
                            raise ConnectionError(f"{method}: core closed the connection")
                        buf += chunk
                        continue
                    msg = json.loads(bytes(buf[:end]))
                    del buf[:end + 1]
                    if msg.get("id") == i:
                        break
        if "error" in msg:
            raise RuntimeError(f"{method}: {msg['error']}")
        return msg["result"]

    def wait(self, pred, timeout=30):
        end = time.time() + timeout
        while time.time() < end:
            try:
                v = pred()
                if v:
                    return v
            except Exception:
                pass
            time.sleep(0.25)
        return None

    def start(self):
        with open(f"{self.r}/serve.log", "w") as log:
            self.core = subprocess.Popen([VQRO, "serve", "--fake", "--safe"], env=self.env, stdout=log, stderr=subprocess.STDOUT)
        self.wait(lambda: os.path.exists(self.env["VQRO_SOCKET"]), 15)
        herdr = self.backend == "herdr"
        # Start extensions only after opting out: both backends run the same explicit layout script.
        self.call("workspace.apply", {"note": "providers", "ops": [
            {"op": "set", "key": "doc/settings/workspaces", "value": {"open_shell_when_empty": False}},
            {"op": "set", "key": "extension/herdr", "value": {"argv": ["/usr/bin/env", "PATH=", PHERDR, "--hc", HC, "--name", "herdr"],
                                                              "kind": "provider", "default": herdr}},
            {"op": "set", "key": "extension/tmux", "value": {"argv": ["/usr/bin/env", "PATH=", PTMUX, "--tmux", "/usr/bin/tmux", "--name", "tmux"],
                                                             "kind": "provider", "enabled": True, "default": not herdr}}]})
        self.call("command.invoke", {"name": "vqro.safe_mode", "args": {"on": False}})
        return self.wait(lambda: all(self.call("workspace.snapshot")["providers"].get(n, {}).get("state") == "connected"
                                     for n in ("herdr", "tmux")), 40)

    def stop(self):
        """Stop this root: the core (its providers and extensions follow it), then the backends (tmux, herdr-core).
        Anything of this root that outlives the backends' own stop is reported (pid, parent, program) and killed, and
        `self.forced` keeps it: a check of its own then fails, so something that survives or comes back stays visible
        without leaving processes behind."""
        try:
            self.call("core.shutdown")
        except Exception:
            pass
        if self.core:
            try:
                self.core.wait(15)
            except subprocess.TimeoutExpired:
                self.core.kill()

        def processes():
            """This root's processes: pid -> (parent pid, program name, argv)."""
            found = {}
            for pid in self.leftovers():
                try:
                    with open(f"/proc/{pid}/stat") as f:
                        parent = int(f.read().rsplit(")", 1)[1].split()[1])
                    with open(f"/proc/{pid}/cmdline", "rb") as f:
                        argv = f.read().split(b"\0")
                except (OSError, IndexError, ValueError):
                    continue
                found[int(pid)] = (parent, argv[0].rsplit(b"/", 1)[-1], argv)
            return found

        def backend_owned(pid, found):
            """A tmux server or herdr-core (by exact program name; vqro-tmux is a provider), or a program running in
            one of their panes (by ancestry)."""
            seen = set()
            while pid in found and pid not in seen:
                seen.add(pid)
                parent, program, _ = found[pid]
                if program in (b"tmux", b"herdr-core"):
                    return True
                pid = parent
            return False

        # Until the core's providers are gone, vqro-tmux could still act on its server: wait (bounded) for them.
        self.wait(lambda: all(backend_owned(pid, found) for found in [processes()] for pid in found), 15)
        end = time.time() + 5
        while True:
            subprocess.run([HC, "server", "stop"], env={**self.env, "PATH": ""}, capture_output=True)
            subprocess.run(["/usr/bin/tmux", "-N", "-S", f"{self.r}/st/tmux.sock", "kill-server"],
                           env={"HOME": f"{self.r}/home", "PATH": "/usr/bin:/bin"}, capture_output=True)
            if not self.leftovers() or time.time() > end:
                break
            time.sleep(0.5)
        left = processes()
        if left:
            report = {pid: [parent, b" ".join(argv).decode(errors="replace")[:160]] for pid, (parent, _, argv) in left.items()}
            print(f"STOP {self.backend}: survived the backends' own stop, killed: {report}", flush=True)
            self.forced.update(report)
            for pid in left:
                try:
                    os.kill(pid, 9)
                except OSError:
                    pass

    def leftovers(self):
        found = []
        for pid in os.listdir("/proc"):
            if pid.isdigit() and int(pid) != os.getpid():
                try:
                    with open(f"/proc/{pid}/environ", "rb") as env, open(f"/proc/{pid}/cmdline", "rb") as cmd:
                        if self.r.encode() in env.read() or self.r.encode() in cmd.read():
                            found.append(pid)
                except OSError:
                    pass
        return found


def tmux_server_view(root):
    """The tmux-server extension's availability, apart from the parity diff (vqro-lead-secondary's judgement): its
    view is declared (the extension declares it at start, whatever its helper does), and its panel shows the owned
    server running: both roots run it, since both have the tmux provider connected."""
    snap = lambda: root.call("workspace.snapshot")
    texts = lambda: [i.get("text", "") for i in ((snap().get("live", {}).get("tmux-server") or {}).get("items") or [])]
    declared = root.wait(lambda: "view/tmux-server" in snap()["entries"], 15)
    running = declared and root.wait(lambda: any(t.startswith("Running") for t in texts()), 15)
    return bool(declared), bool(running), texts() if declared else []


def scenario(root):
    """The same workspace, built only through public commands. Returns a note of anything that could not be done."""
    c = root.call
    snap = lambda: c("workspace.snapshot")
    commands = ("tabs.open", "tabs.rename", "panes.split", "sidebar.toggle", "pi.new", "tabs.set")
    if not root.wait(lambda: set(commands) <= {x["name"] for x in c("command.list")["commands"] if x.get("available")}, 40):
        return "extension commands never became available"
    native = [python(), os.path.join(REPO, "tests/fixtures/fake_pi.py"), "--bridge-e2e"]
    # The production bridge (the default launcher, which execs vqro-bridge-pi): its PATH on the terminal host is
    # this root's bin (python3 and vqro-bridge-pi only), so the agent declares its real native capabilities.
    c("workspace.apply", {"note": "fake Pi runtime", "ops": [{"op": "set", "key": "doc/settings/pi", "value": {"runtimes": [
        {"name": "fake", "argv": native, "bridge_env": {"PATH": f"{root.r}/bin"}}]}}]})
    # Tab 1: a shell that sets its program title, then splits; tab 2: a plain shell renamed "build".
    c("command.invoke", {"name": "tabs.open", "args": {"argv": ["/bin/sh", "-c", "printf '\\033]0;titled\\007'; exec /bin/sh"],
                                                       "cwd": f"{root.r}/work"}})
    c("command.invoke", {"name": "tabs.open", "args": {"argv": ["/bin/sh"], "cwd": f"{root.r}/work"}})
    main = lambda: snap()["entries"]["layout/main"]
    tabs_node = lambda: next(n for n in walk(main()["value"]) if "tabs" in n)
    path_of = lambda tab: next(p for p, n in walk_paths(main()["value"]) if "tabs" in n) + [tab]
    tab_path = path_of(0)[:-1]
    c("command.invoke", {"name": "tabs.rename", "args": {"text": "build", "context": {"layout": "main", "layout_version": main()["version"],
                                                                                       "tab_path": tab_path, "tab_index": 1}}})
    c("command.invoke", {"name": "panes.split", "args": {"dir": "right", "context": {
        "layout": "main", "layout_version": main()["version"], "path": path_of(0), "tab_path": tab_path, "tab_index": 0}}})
    # Tab 3: a single shell that sets its program title (program-titled tabs), and tab 4: a plain shell.
    c("command.invoke", {"name": "tabs.open", "args": {"argv": ["/bin/sh", "-c", "printf '\\033]0;solo\\007'; exec /bin/sh"],
                                                       "cwd": f"{root.r}/work"}})
    c("command.invoke", {"name": "tabs.open", "args": {"argv": ["/bin/sh"], "cwd": f"{root.r}/work"}})
    c("command.invoke", {"name": "sidebar.toggle"})
    c("command.invoke", {"name": "tabs.set", "args": {"field": "marks", "value": True}})
    root.wait(lambda: any(r.get("name") == "fake" for r in (snap()["entries"].get("doc/settings/pi") or {}).get("value", {}).get("runtimes", [])), 10)
    created = c("command.invoke", {"name": "pi.new", "args": {"runtime": "fake", "cwd": f"{root.r}/work"}})
    if not root.wait(lambda: any(r["kind"] == "agent" and r.get("available") for r in snap()["resources"]), 40):
        return f"no agent registered (pi.new: {created})"
    if not root.wait(lambda: {"titled", "solo"} <= {r.get("title") for r in snap()["resources"]}, 15):
        return "the program titles titled and solo were never reported"
    if not root.wait(lambda: len((snap().get("live", {}).get("sidebar") or {}).get("items", [])) > 0, 15):
        return "the sidebar never published rows"
    if not root.wait(lambda: any((((r.get("data") or {}).get("agent") or {}).get("caps")) for r in snap()["resources"] if r["kind"] == "agent"), 20):
        return "the agent never declared its capabilities"
    time.sleep(2)  # nothing to observe: let late facts (titles, cwd, agent metadata) land before the capture
    return None


def walk(n):
    yield n
    for c in n.get("children", []):
        yield from walk(c)
    for t in n.get("tabs", []):
        yield from walk(t["node"])


def walk_paths(n, path=()):
    yield list(path), n
    for i, c in enumerate(n.get("children", [])):
        yield from walk_paths(c, path + (i,))
    for i, t in enumerate(n.get("tabs", [])):
        yield from walk_paths(t["node"], path + (i,))


def capture(root):
    """What a client sees, with run-specific identifiers replaced by stable labels."""
    snap = root.call("workspace.snapshot")
    resources = snap["resources"]
    order = sorted(resources, key=lambda r: (r["kind"], int(re.sub(r"\D", "", r["id"]) or 0)))
    label = {r["id"]: f"{r['kind']}{i}" for i, r in enumerate(order)}
    text = json.dumps(snap)
    for rid in sorted(label, key=len, reverse=True):
        text = re.sub(rf"(?<![\w-]){re.escape(rid)}(?![\w])", label[rid], text)
    text = text.replace(root.r, "<root>")
    # Names a run generates (the Pi bridge's provider instance and terminal names) carry random suffixes.
    text = re.sub(r"pi-[0-9a-f]{8}-[0-9a-f-]{27}", "pi-<instance>", text)
    text = re.sub(r"\bpi-[0-9a-f]{32}\b", "pi-<instance>", text)  # the Rust bridge's provider instance
    text = re.sub(r"pi-bridge-[0-9a-f]{12}", "pi-bridge-<id>", text)
    snap = json.loads(text)
    keep = ("kind", "state", "available", "stale", "provider", "name", "title", "cwd", "argv", "status", "host")
    out = {
        "layout": snap["entries"].get("layout/main", {}).get("value"),
        # view/tmux-server is left out of the parity diff: its availability is its own check (tmux_server_view).
        "views": {k[5:]: {f: v["value"].get(f) for f in ("kind", "resource", "title", "fallback")}
                  for k, v in snap["entries"].items() if k.startswith("view/") and k != "view/tmux-server"},
        "resources": {label_: {f: r.get(f) for f in keep} for label_, r in ((r["id"], r) for r in snap["resources"])},
        "agents": {r["id"]: ((r.get("data") or {}).get("agent") or {}).get("caps")  # with their values (modes, limits)
                   for r in snap["resources"] if r["kind"] == "agent"},
        "sidebar": [{f: row.get(f) for f in ("text", "badges", "style")} for row in (snap.get("live", {}).get("sidebar") or {}).get("items", [])],
        "commands": sorted(c["name"] for c in root.call("command.list")["commands"] if c.get("available")),
    }
    outline = subprocess.run([VQRO, "ui", "text", "--", "--once", "show"], env=root.env, capture_output=True, text=True, timeout=30)
    out["text_client"] = re.sub(r"\b(terminal|agent)\d+\b", r"\1#", (outline.stdout + outline.stderr).replace(root.r, "<root>")).splitlines()
    return out


def differences(a, b, path=""):
    if isinstance(a, dict) and isinstance(b, dict):
        for k in sorted(set(a) | set(b)):
            yield from differences(a.get(k, "<missing>"), b.get(k, "<missing>"), f"{path}.{k}" if path else k)
    elif isinstance(a, list) and isinstance(b, list) and len(a) == len(b):
        for i, (x, y) in enumerate(zip(a, b)):
            yield from differences(x, y, f"{path}[{i}]")
    elif a != b:
        yield path, a, b


captures, roots = {}, []
try:
    for backend in ("herdr", "tmux"):
        root = Root(backend)
        roots.append(root)
        check(f"{backend}: both providers connected, {backend} the default", root.start())
        declared, reported, texts = tmux_server_view(root)
        check(f"{backend}: the tmux-server extension declares its view", declared)
        check(f"{backend}: the tmux-server panel shows the owned server running", reported, str(texts))
        problem = scenario(root)
        check(f"{backend}: the scripted workspace was built", problem is None, problem or "")
        captures[backend] = capture(root)
        root.stop()
    if KEEP:
        os.makedirs(KEEP, exist_ok=True)
        for backend, cap in captures.items():
            with open(os.path.join(KEEP, f"capture-{backend}.json"), "w") as f:
                json.dump(cap, f, indent=2, ensure_ascii=False)
    found = list(differences(captures["herdr"], captures["tmux"]))
    unexpected = []
    for path, a, b in found:
        why = next((why for pattern, why in EXPECTED if re.search(pattern, path)), None)
        known = next((name for name, rule in KNOWN if rule(path, a, b)), None)
        kind = f"expected: {why}" if why else f"known bug: {known}" if known else "UNEXPECTED"
        print(f"DIFF {path}: herdr={json.dumps(a, ensure_ascii=False)[:200]} tmux={json.dumps(b, ensure_ascii=False)[:200]}  ({kind})", flush=True)
        if why is None and known is None:
            unexpected.append(path)
    check("the same workspace looks the same to a client on herdr and on tmux (apart from expected differences)",
          not unexpected, "\n".join(unexpected))
    # Honest reporting on both backends (findings F1-F4): no name nobody gave, no title no program set, and a
    # spawn without argv runs, and reports, the host's $SHELL.
    for backend, cap in captures.items():
        owners = {k: r.get("provider") for k, r in cap["resources"].items() if r["kind"] == "terminal"}
        check(f"{backend}: every terminal (shells, split, agent bridge) belongs to the default backend, {backend}",
              len(owners) == 6 and set(owners.values()) == {backend}, owners)
        caps = next(iter(cap["agents"].values()), None)
        check(f"{backend}: the agent declares native capabilities (production bridge), compared with their values",
              isinstance(caps, dict) and caps.get("agent.send"), caps)
        terms = {k: r for k, r in cap["resources"].items() if r["kind"] == "terminal" and not str(r.get("name") or "").startswith("pi-bridge")}
        check(f"{backend}: terminals nobody named have no name (F1)", all(r.get("name") is None for r in terms.values()),
              {k: r.get("name") for k, r in terms.items()})
        titled = {k: r.get("title") for k, r in terms.items()}
        check(f"{backend}: only programs that set a title have one, never the host name (F2)",
              sorted(t for t in titled.values() if t is not None) == ["solo", "titled"] and HOST not in titled.values(), titled)
        check(f"{backend}: the split's default shell is the host's $SHELL and is reported (F3, F4)",
              any(r.get("argv") == [SHELL] for r in terms.values()), {k: r.get("argv") for k, r in terms.items()})
        check(f"{backend}: a program-titled tab shows its program's title", "[solo₃]" in "\n".join(cap["text_client"]),
              "\n".join(cap["text_client"]))
finally:
    for root in roots:
        root.stop()
        stray = root.wait(lambda: not root.leftovers() and [True], 10)
        check(f"{root.backend}: nothing left running", bool(stray), str(root.leftovers()))
        check(f"{root.backend}: its backends' own stop ended everything (nothing had to be killed)", not root.forced, str(root.forced))
        if all(results):
            shutil.rmtree(root.r, ignore_errors=True)
print(f"passed {sum(results)} failed {len(results) - sum(results)}")
sys.exit(0 if all(results) else 1)
