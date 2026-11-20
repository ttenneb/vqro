# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Bennett Garcia
"""Real G14 child via the pinned vqro-herdr and a brand-new private HC service.

Reuses e2e_herdr_drift.py's public spawn/forget and private HC lifecycle pattern.
Never prestart HC: the adapter must scrub its environment before HC auto-starts.
No real Herdr socket, credentials, model/runtime or target engine is accessed.
"""
import asyncio
import hashlib
import json
from pathlib import Path
import subprocess
import shutil

from ..results import CheckFailed, FixtureFailed
from .context import PublicCoordination


IDENTITY_KEYS = ("VQRO_PRINCIPAL", "VQRO_EXTENSION", "VQRO_EXTENSION_TOKEN", "VQRO_EXTENSION_LIFETIME")


def child_identity_is_scrubbed(observed):
    if (not isinstance(observed, dict) or type(observed.get("pid")) is not int or observed["pid"] <= 0 or
            "principal" not in observed or observed["principal"] is not None and not isinstance(observed["principal"], str) or
            not isinstance(observed.get("present"), dict) or set(observed["present"]) != set(IDENTITY_KEYS) or
            any(type(value) is not bool for value in observed["present"].values())):
        raise FixtureFailed("malformed actual backend child observation")
    return observed["principal"] is None and not any(observed["present"].values())


def empty_terminal_inventory(observed):
    if not isinstance(observed, dict) or not isinstance(observed.get("terminals"), list):
        raise FixtureFailed("private HC inventory lacks its actual terminal list")
    return not observed["terminals"]


def pinned_artifact(declaration):
    if not isinstance(declaration, dict) or not declaration.get("path") or not declaration.get("sha256"):
        raise FixtureFailed("G14 executing backend needs an explicit immutable artifact pin")
    path = Path(declaration["path"])
    if not path.is_file() or hashlib.sha256(path.read_bytes()).hexdigest() != declaration["sha256"]:
        raise FixtureFailed("G14 backend artifact pin mismatch")
    return path


async def backend_child_environment(parent):
    pins = parent.manifest.get("identity_backend", {})
    herdr, hc = pinned_artifact(pins.get("herdr")), pinned_artifact(pins.get("hc"))
    manifest = pinned_artifact(pins.get("manifest"))
    ctx = PublicCoordination(parent.root / "b", parent.manifest, parent.vqro)
    # Both runner and shared context intentionally freeze only coord's manifest.
    # Pin/stage this real provider manifest explicitly, never strip dependencies.
    shutil.copyfile(manifest, ctx.root / "ext/manifests/herdr.json")
    ctx.metadata["herdr_manifest_sha256"] = hashlib.sha256(manifest.read_bytes()).hexdigest()
    hc_socket = ctx.root / "h"
    if len(str(hc_socket).encode()) > 103: raise FixtureFailed("G14 private HC socket exceeds portable bound")
    if hc_socket.exists(): raise FixtureFailed("G14 private HC endpoint already exists; never adopt it")
    runtime = ctx.root / "xr"
    runtime.mkdir(mode=0o700, exist_ok=True)
    state = ctx.root / "hs"
    state.mkdir(mode=0o700, exist_ok=True)
    # HC derives an additional session-suffixed socket (observed native 109-byte
    # rejection with 'g14-private'); a one-character private session fits <103 bytes.
    ctx.backend.env.update(HC_SOCKET=str(hc_socket), HC_SESSION="g",
        XDG_RUNTIME_DIR=str(runtime), XDG_STATE_HOME=str(state),
        VQRO_PRINCIPAL="worker:must-not-leak")
    if any(key.startswith("HERDR_") for key in ctx.backend.env):
        raise FixtureFailed("G14 inherited real Herdr environment")
    artifact = ctx.root / "backend-env.json"
    script = ctx.root / "backend-env.py"
    script.write_text("import json,os,pathlib\n" +
        "target=pathlib.Path(" + repr(str(artifact)) + "); temporary=target.with_suffix('.tmp')\n" +
        "temporary.write_text(json.dumps({'pid':os.getpid(),'principal':os.environ.get('VQRO_PRINCIPAL'),'present':{key:key in os.environ for key in " + repr(IDENTITY_KEYS) + "}})); temporary.replace(target)\n")
    rid = None
    started = False
    removed = False
    forget_attempted = False
    primary_error = None
    cleanup_error = None
    native_before_stop = None

    async def hc_call(*argv):
        result = await asyncio.to_thread(subprocess.run, [str(hc), *argv], env=ctx.backend.env,
            capture_output=True, text=True, timeout=8)
        if result.returncode: raise FixtureFailed("private HC command failed: " + result.stderr[-1024:])
        return json.loads(result.stdout)

    try:
        await ctx.start_core()
        # This starts the actual managed provider. Its hc connect auto-starts only the
        # explicitly private service; nothing else starts it before the witness.
        started = True
        activation = await ctx.peer.call("workspace.apply", {"ops": [{"op": "set", "key": "extension/herdr",
            "value": {"argv": [str(herdr), "--hc", str(hc), "--name", "herdr"],
                      "kind": "provider", "enabled": True}, "expect": 0}], "note": "owned executing backend witness"})
        async def ready():
            provider = (await ctx.peer.call("workspace.snapshot"))["providers"].get("herdr", {})
            return provider if provider.get("synced") and provider.get("connected") else None
        try: provider = await ctx.wait(ready, label="private executing herdr backend")
        except CheckFailed as error:
            ctx.evidence.append({"kind": "backend_fixture_startup_failure", "snapshot": await ctx.peer.call("workspace.snapshot")})
            raise FixtureFailed("owned executing backend did not become ready") from error
        ctx.track_children()
        spawned = await ctx.peer.call("resource.spawn", {"provider": "herdr", "argv": ["/usr/bin/python3", str(script)],
            "name": "owned-g14-environment", "cwd": str(ctx.root / "work")}, timeout=8)
        rid = spawned["resource"]["id"]
        await ctx.wait(lambda: artifact.is_file(), label="actual backend child's environment artifact")
        observed = json.loads(artifact.read_text())
        ctx.check(child_identity_is_scrubbed(observed),
            "actual executing backend child inherits no caller identity/token variables", component="core", observation=observed)
        async def finished():
            resources = (await ctx.peer.call("resource.list", {"provider": "herdr"}))["resources"]
            return next((row for row in resources if row["id"] == rid and row["state"] == "finished"), None)
        end = await ctx.wait(finished, label="owned backend probe native exit")
        ctx.check(end.get("exit", {}).get("exit_code") == 0, "actual backend probe exits successfully", component="harness", resource=end)
        forget_attempted = True
        forgotten = await ctx.peer.call("resource.forget", {"resource": rid}, timeout=8)
        native_before_stop = await hc_call("ls", "--all", "--json")
        removed = empty_terminal_inventory(native_before_stop)
        ctx.check(removed, "every created terminal is removed from private HC", component="harness", native=native_before_stop)
        ctx.evidence.append({"kind": "executing_backend_environment", "pins": pins, "activation": activation,
            "provider": provider, "spawn": spawned, "child": observed, "finished": end, "forget": forgotten,
            "native_terminal_count_after_forget": len(native_before_stop.get("terminals", []))})
    except BaseException as error:
        primary_error = error
    finally:
        if rid is not None and not removed and not forget_attempted:
            try:
                # One cleanup attempt for our exact resource, never replay an uncertain forget.
                await ctx.peer.call("resource.invoke", {"resource": rid, "op": "terminal.kill", "args": {}}, timeout=8)
                forget_attempted = True
                await ctx.peer.call("resource.forget", {"resource": rid}, timeout=8)
                native_before_stop = await hc_call("ls", "--all", "--json")
                removed = empty_terminal_inventory(native_before_stop)
            except Exception as error: cleanup_error = error
        try:
            ctx.track_children()
            # Stop the owning core/provider before HC, so no connect loop can restart it.
            await asyncio.to_thread(ctx.backend.stop, False)
            if started:
                # HC's actual session socket is derived, not necessarily HC_SOCKET itself.
                stopped = await hc_call("server", "stop")
                ctx.evidence.append({"kind": "private_hc_stop", "reply": stopped})
                if not isinstance(stopped.get("stopped"), bool):
                    raise FixtureFailed("private HC stop lacks its explicit result")
        except Exception as error: cleanup_error = error
        finally:
            try: await ctx.close()
            except Exception as error: cleanup_error = error
        if not cleanup_error:
            shutil.rmtree(state)  # only this disposable service's state, after owned processes stop
        ctx.evidence.append({"kind": "executing_backend_cleanup", "removed_exact_terminal": removed,
            "private_socket": str(hc_socket), "private_hc_state_removed": not state.exists(),
            "cleanup_error": type(cleanup_error).__name__ if cleanup_error else None})
        parent.assertions += ctx.assertions
        parent.evidence.append({"kind": "executing_backend_fixture", "evidence": ctx.evidence})
    if cleanup_error: raise FixtureFailed("owned executing backend cleanup did not complete") from cleanup_error
    if primary_error: raise primary_error
