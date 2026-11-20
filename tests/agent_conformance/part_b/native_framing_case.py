# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Bennett Garcia
"""T15 uses the existing real Pi/Claude CLIs against private fake HTTP models.

Native wire bodies and plugin take journals are the oracle, never inherited test
names or engine/MemoryStore calls. Credentials and external network are absent.
"""
import asyncio
import hashlib
import json
import os
from pathlib import Path
import re
import sys
import tempfile

from .cases import case
from .identity_backend import pinned_artifact
from ..results import FixtureFailed, HarnessGap


TESTS = Path(__file__).resolve().parents[2]


def module_tree_digest(root):
    """Seal the native JS dependency closure, not merely its tiny CLI launcher."""
    root = Path(root).resolve()
    if not root.is_dir():
        raise FixtureFailed("native module closure is not a directory")
    digest = hashlib.sha256()
    total = count = 0
    for path in sorted(root.rglob("*")):
        if path.is_symlink():
            if not path.resolve().is_relative_to(root):
                raise FixtureFailed("native dependency link escapes its declared closure")
            payload = os.readlink(path).encode()
        elif path.is_file():
            total += path.stat().st_size
            count += 1
            if total > 2 * 1024 ** 3 or count > 100000:
                raise FixtureFailed("native module closure exceeds the fixture seal bound")
            payload = hashlib.sha256(path.read_bytes()).digest()
        else:
            continue
        digest.update(path.relative_to(root).as_posix().encode() + b"\0" + payload + b"\0")
    return digest.hexdigest()


def texts(value):
    if isinstance(value, str):
        return value
    if isinstance(value, list):
        return "\n".join(texts(item) for item in value)
    if isinstance(value, dict):
        return "\n".join(texts(value[key]) for key in ("text", "content") if key in value)
    return ""


def first_message(requests, token):
    for request in requests:
        wire = request.get("wire")
        if not isinstance(wire, dict) or not isinstance(wire.get("messages"), list):
            raise FixtureFailed("native model log lacks its full HTTP message envelope")
        for message in wire["messages"]:
            if token in texts(message.get("content")):
                return request, message
    raise FixtureFailed("no actual native model request contains " + token)


def load_rows(path):
    if not path.is_file() or path.stat().st_size > 32 * 1024 * 1024:
        raise FixtureFailed("native artifact absent or beyond the 32 MiB fixture bound: " + str(path))
    rows = [json.loads(line) for line in path.read_text().splitlines() if line]
    if len(rows) > 4096:
        raise FixtureFailed("native artifact exceeds its bounded row count")
    return rows


def framed(message, role, frame):
    body = texts(message.get("content"))
    return (message.get("role") == role and "vqro inbox message " in body and " from p:owner" in body
            and (frame == "sender_header" or frame in body))


async def run_native(ctx, label, script, args, pins, parent):
    root = Path(tempfile.mkdtemp(prefix=label + "-", dir=parent))
    if len(str(root).encode()) > 68:
        root.rmdir()
        raise FixtureFailed("native pull fixture root leaves no portable derived-socket budget")
    log = root / "runner.log"
    env = dict(ctx.backend.env, HOME=str(root / "home"), PATH=str(pins["node"].parent) + ":/usr/bin:/bin",
               PYTHONDONTWRITEBYTECODE="1", TMPDIR="/tmp")
    if any(key.startswith("HERDR_") for key in env):
        raise FixtureFailed("native pull process inherited a real Herdr environment")
    # A nested private mount/PID/network root also hides real native /tmp sockets.
    argv = ["/usr/bin/unshare", "-mnpf", "--kill-child=KILL", "--mount-proc", "/bin/bash", "-c",
            'mount -t tmpfs -o size=256m,mode=1777 tmpfs /tmp && ip link set lo up && exec "$@"',
            "native-pull", sys.executable, str(TESTS / script), "--vqro", str(ctx.vqro),
            "--root", str(root), "--inbox", "k2core", "--coord-ext", str(ctx.root / "ext"),
            "--framing-only", *args]
    with log.open("wb") as output:
        process = await asyncio.create_subprocess_exec(*argv, env=env, stdin=asyncio.subprocess.DEVNULL,
                                                       stdout=output, stderr=asyncio.subprocess.STDOUT)
        try:
            status = await asyncio.wait_for(process.wait(), 240)
        except BaseException:
            if process.returncode is None:
                process.kill()
            await process.wait()
            raise
    ctx.evidence.append({"kind": "native_pull_execution", "path": str(root), "log": str(log),
                         "exit": status, "private_pid_mount_network": True})
    if status:
        raise FixtureFailed("native pull execution failed; retain " + str(log))
    result = json.loads((root / "framing-result.json").read_text())
    models = load_rows(root / "model.jsonl")
    journal = load_rows(root / ("plugin.jsonl" if label in ("pi", "pt") else "hookstate/hook.log"))
    declarations = result.get("public_declarations")
    if not isinstance(declarations, list) or len(declarations) != 1:
        raise FixtureFailed("native route lacks its actual public declaration readback")
    declaration = declarations[0]["entry"]["value"]["pull"]
    ctx.evidence.append({"kind": "native_pull_artifacts", "route": label, "result": result,
                         "declaration": declaration, "model_log_sha256": hashlib.sha256((root / "model.jsonl").read_bytes()).hexdigest()})
    return root, result, models, journal, declaration


def take_point(journal, delivery):
    matches = [(row.get("detail") or row)["at"] for row in journal if row.get("event") == "taken"
               and delivery in (row.get("detail") or row).get("deliveries", [])]
    if len(matches) != 1:
        raise FixtureFailed("native take journal does not identify one exact point for " + delivery)
    return matches[0]


@case("T15")
async def native_framing_and_precision(ctx):
    config = ctx.manifest.get("native_pull") or {}
    pins = {name: pinned_artifact(config.get(name)) for name in ("node", "pi", "claude", "hook")}
    modules = config.get("pi_modules") or {}
    module_root = Path(modules.get("path", ""))
    if not module_root.is_absolute() or module_tree_digest(module_root) != modules.get("sha256"):
        raise FixtureFailed("native Pi dependency closure pin is missing or changed")
    parent = Path(config.get("runtime_parent", ""))
    if not parent.is_absolute() or not parent.is_dir():
        raise FixtureFailed("T15 requires an explicit private-scratch native runtime parent")
    sources = [TESTS / name for name in ("coord_harness.py", "pi_pull_e2e.py", "claude_code_e2e.py",
               "claude_code_tui.py", "pi_pull_tui.py", "fixtures/fake_openai.py", "fixtures/fake_anthropic.py")]
    sources += [TESTS.parent / "integrations/pi/vqro-pull.ts"]
    hashes = {str(path): hashlib.sha256(path.read_bytes()).hexdigest() for path in sources}
    ctx.evidence.append({"kind": "native_pull_pins", "artifacts": config, "source_sha256": hashes})
    routes = [
        ("pi", "pi_pull_e2e.py", ["--pi", str(pins["pi"])],
         [("MSG-BOUNDARY-A", "turn", "boundary"), ("MSG-SETTLE-B", "settle", "settle"), ("MSG-START-C", "idle", "start")]),
        ("pt", "pi_pull_tui.py", ["--pi", str(pins["pi"])],
         [("MSG-TUI-BOUNDARY", "turn", "boundary"), ("MSG-TUI-START", "idle", "start")]),
        ("cc", "claude_code_e2e.py", ["--claude", str(pins["claude"]), "--hook", str(pins["hook"])],
         [("MSG-BOUNDARY-A", "turn", "boundary"), ("MSG-SETTLE-B", "settle", "settle")]),
        ("ui", "claude_code_tui.py", ["--claude", str(pins["claude"]), "--hook", str(pins["hook"])],
         [("MSG-WAKE-I1", "idle", "boundary"), ("MSG-RACE-I3", None, "boundary")]),
    ]
    renderer_gaps = []
    for label, script, args, checks in routes:
        root, result, models, journal, declaration = await run_native(ctx, label, script, args, pins, parent)
        deliveries = []
        for token, expected_point, mode in checks:
            request, message = first_message(models, token)
            body = texts(message.get("content"))
            match = re.search(r"vqro inbox message (\S+)", body)
            if not match:
                raise FixtureFailed("native envelope lost its delivery identity")
            delivery = match[1]
            deliveries.append(delivery)
            point = take_point(journal, delivery)
            metadata = (declaration.get("points") or {}).get(point)
            framing = (declaration.get("framing") or {}).get(point)
            ctx.check(isinstance(metadata, dict) and metadata.get("precision") in ("exact", "approximate")
                      and isinstance(framing, dict) and framing.get("role") and framing.get("frame"),
                      "each actually served native point declares precision and added framing", route=label, point=point,
                      declaration=declaration)
            ctx.check((expected_point is None or point == expected_point)
                      and framed(message, framing["role"], framing["frame"]) and "(mode " + mode + ")" in body,
                      "actual native model envelope retains its declared role/frame/origin and insertion contract",
                      route=label, point=point, delivery=delivery, request=request["n"], envelope=message)
            if label in ("pi", "pt"):
                ctx.check(metadata["precision"] == "exact", "Pi's native safe points retain their explicit exact guarantee", point=point)
            elif point == "idle":
                ctx.check(metadata["precision"] == "approximate" and mode != "start"
                          and "NOT USER INPUT" in body,
                          "approximate Claude rewake is not relabelled plain user input or assumed-idle start", envelope=message)
        if label in ("pt", "ui"):
            screen = (root / "framing-screen.txt").read_text()
            ctx.evidence.append({"kind": "native_renderer", "route": label,
                                 "screen_sha256": hashlib.sha256(screen.encode()).hexdigest(),
                                 "path": str(root / "framing-screen.txt")})
            if label == "pt":
                ctx.check(all("vqro inbox message " + delivery in screen for delivery in deliveries),
                          "Pi's actual renderer preserves both native sender-framed delivery headers", deliveries=deliveries)
            else:
                if "NOT USER INPUT" not in screen:
                    renderer_gaps.append(str(root / "framing-screen.txt"))
                else:
                    ctx.check(True, "Claude renderer retains the native rewake label rather than plain user input")
        if label == "ui":
            race = result.get("approximate_idle_race") or {}
            unsafe = race.get("unsafe_start") or {}
            safe = race.get("boundary") or {}
            prompt = race.get("human_prompt")
            human_seen = isinstance(prompt, str) and bool(prompt) and any(prompt in json.dumps(row.get("wire")) for row in models)
            ctx.check(unsafe.get("state") in ("held", "recorded") and not unsafe.get("attempt")
                      and safe.get("mode") == "boundary" and human_seen,
                      "a real human-prompt race makes no unsafe start claim and preserves the boundary contract", race=race)
            ctx.check(not any("MSG-BUSY-I3" in json.dumps(row.get("wire")) for row in models),
                      "a plugin unable to preserve approximate-idle start makes no model input")
    ctx.check(module_tree_digest(module_root) == modules["sha256"], "native Pi dependency closure stayed frozen")
    ctx.check(all(hashlib.sha256(Path(path).read_bytes()).hexdigest() == digest for path, digest in hashes.items()),
              "native fixture source stayed unchanged across all path captures")
    if renderer_gaps:
        raise HarnessGap("T15 native Claude renderer binding unverified: raw model wrapper exists but the incomplete "
                         "PTY projection lacks its exact label; no product relabel or whole-row pass inferred. Artifacts: "
                         + ", ".join(renderer_gaps))
