# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Bennett Garcia
"""Exact public AS10c bindings. No target engine import or manufactured UI/receipt.

Fixture configuration pins the reviewed source, manifests, existing bridge/backend
images and deterministic fake Pi. It is not a proposed production protocol.
"""
import asyncio
from copy import deepcopy
from functools import wraps
import json
import os
from pathlib import Path
import shutil
import socket
import struct
import subprocess
import tempfile

from pi_provider_e2e import identity
from ..results import CheckFailed, FixtureFailed, HarnessGap
from .identity_backend import pinned_artifact, empty_terminal_inventory

ROUTE_PREFIX = "doc/agents/request-routes/"
NOTICE_PREFIX = "Native request notice (not an answer):\n"
ROUTES = {"human", "coordinator", "both"}


def route_key(agent):
    if not isinstance(agent, str) or not agent or "/" in agent or "\0" in agent:
        raise FixtureFailed("request route requires an actual opaque resource id")
    return ROUTE_PREFIX + agent


def native_notice(content):
    """Extract exactly one real text payload; native/record identity is graded later."""
    found = []
    for block in content if isinstance(content, list) else []:
        if not isinstance(block, dict) or block.get("type") != "text": continue
        text = block.get("text")
        if not isinstance(text, str) or NOTICE_PREFIX not in text: continue
        try: value = json.loads(text.split(NOTICE_PREFIX, 1)[1])
        except (ValueError, TypeError): continue
        if isinstance(value, dict): found.append(value)
    return found[0] if len(found) == 1 else None


def waiting_rows(panel):
    stack = list(panel.get("items", [])) if isinstance(panel, dict) else []
    rows, visited = [], 0
    while stack:
        row = stack.pop(); visited += 1
        if visited > 2048: raise CheckFailed("actual request panel exceeds the client row bound")
        if not isinstance(row, dict): continue
        if row.get("id") == "waiting": rows.append(row)
        if isinstance(row.get("children"), list): stack.extend(row["children"])
    return rows


def request_routing_case(function):
    @wraps(function)
    async def owned(ctx):
        try: return await function(ctx)
        finally:
            binding = getattr(ctx, "_request_routing_binding", None)
            if binding is not None: await binding.close()
    return owned


class PublicRequestRouting:
    def __init__(self, ctx):
        self.ctx = ctx
        self.config = ctx.manifest.get("request_routing")
        if not isinstance(self.config, dict) or self.config.get("coord_revision") != ctx.manifest.get("coord_revision"):
            raise HarnessGap("reviewed request-routing binding needs an exact immutable source/fixture pin")
        self.prepared = False
        self.launch_prepared = False
        self.native_root = None
        self.hc = None
        self.backend_started = False
        self.closed = False
        self.hosts = []

    async def prepare(self):
        if self.prepared: return
        ctx = self.ctx
        # These real manifests are staged before restarting the private core. No
        # dependencies are stripped, and no default-installed extension is used.
        for name, descriptor in self.config.get("manifests", {}).items():
            if name not in ("agents", "pi", "herdr"): raise FixtureFailed("unexpected routing fixture manifest")
            original = pinned_artifact(descriptor)
            shutil.copyfile(original, ctx.root / "ext/manifests" / (name + ".json"))
        if not all((ctx.root / "ext/manifests" / (name + ".json")).is_file() for name in ("agents", "pi")):
            raise HarnessGap("real agents and Pi manifests are required by the routing witness")
        # Only the backend's derived sockets need a short path. Every endpoint is
        # still private; the native oracle reconnects facts only, never input.
        parent = Path(self.config.get("runtime_parent", ""))
        if not parent.is_absolute() or not parent.is_dir(): raise FixtureFailed("private native runtime parent required")
        self.native_root = Path(tempfile.mkdtemp(prefix="qr-", dir=parent))
        if len(str(self.native_root).encode()) > 64: raise FixtureFailed("routing native root exceeds socket budget")
        for name in ("home", "config", "state", "run"): (self.native_root / name).mkdir(mode=0o700)
        ctx.backend.env.update(HOME=str(self.native_root / "home"), XDG_CONFIG_HOME=str(self.native_root / "config"),
            XDG_STATE_HOME=str(self.native_root / "state"), XDG_RUNTIME_DIR=str(self.native_root / "run"),
            HC_SOCKET=str(self.native_root / "h"), HC_SESSION="q")
        if any(key.startswith("HERDR_") for key in ctx.backend.env): raise FixtureFailed("real Herdr identity inherited")
        await ctx.restart(crash=False)
        commands = (await ctx.peer.call("command.list"))["commands"]
        ctx.check(any(row["name"] == "coord.request_route" for row in commands),
            "mandatory routing command exists on the explicitly pinned target", commands=commands)
        await self.enable("agents", "management", "agents.py")
        self.prepared = True

    async def enable(self, name, kind, source, *, argv=None, default=False):
        ctx = self.ctx
        key = "extension/" + name
        previous = await ctx.peer.call("workspace.get", {"key": key})
        value = {"kind": kind, "enabled": True, "argv": argv or ["/usr/bin/python3", str(ctx.root / "ext" / source)]}
        if default: value["default"] = True
        result = await ctx.peer.call("workspace.apply", {"ops": [{"op": "set", "key": key, "value": value,
            "expect": previous["version"] if previous else 0}], "note": "owned public routing fixture activation"})
        async def ready():
            snapshot = await ctx.peer.call("workspace.snapshot")
            row = snapshot.get("providers", {}).get(name) if kind == "provider" else snapshot.get("extensions", {}).get(name)
            if kind == "provider": return row if row and row.get("connected") and row.get("synced") else None
            commands = (await ctx.peer.call("command.list"))["commands"]
            return any(row["name"].startswith(name + ".") for row in commands)
        await ctx.wait(ready, label="actual managed " + name)
        ctx.track_children()
        ctx.evidence.append({"kind": "routing_fixture_activation", "extension": name, "transaction": result})

    async def inspect(self, agent):
        ctx = self.ctx
        entry = await ctx.peer.call("workspace.get", {"key": route_key(agent)})
        public = await ctx.command("request_route", {"action": "inspect", "agent": agent})
        ctx.check(public.get("key") == route_key(agent) and public.get("agent") == agent and
            type(public.get("version")) is int and public.get("version") == (entry["version"] if entry else 0),
            "public inspect names the exact approved ordinary key/version", entry=entry, public=public)
        value = entry.get("value") if entry else None
        configured = value.get("route") if isinstance(value, dict) else None
        valid = isinstance(configured, str) and configured in ROUTES
        ctx.check(json.dumps(public.get("configured"), sort_keys=True) == json.dumps(configured, sort_keys=True) and
            isinstance(public.get("route"), str) and public["route"] in ROUTES and isinstance(public.get("human"), bool),
            "inspect reports the actual raw preference and a typed effective route", entry=entry, public=public)
        if not valid or configured == "human":
            ctx.check(public["route"] == "human" and public["human"] is True and public.get("coordinator") is None and
                (not entry or valid or public.get("fallback_reason") == "invalid_request_route"),
                "missing/malformed configuration has actual public human fallback", public=public)
        elif public["route"] != "human":
            ctx.check(public["route"] == configured and public["human"] is (configured == "both") and
                isinstance(public.get("coordinator"), str) and bool(public["coordinator"]),
                "effective coordinator route is consistent with configured preference", public=public)
        return {"agent": agent, "route": configured if valid else "human",
            "entry": entry, "public": public}

    async def configure(self, agent, route, peer):
        ctx = self.ctx
        previous = await ctx.peer.call("workspace.get", {"key": route_key(agent)})
        before = {row["id"] for row in (await ctx.peer.call("workspace.history", {"limit": 10000}))["txns"]}
        reply = await ctx.command("request_route", {"op": await ctx.operation("request-route"), "action": "set",
            "agent": agent, "route": route, "expect": previous["version"] if previous else 0}, peer=peer)
        observed = await self.inspect(agent)
        entry = observed["entry"]
        ctx.check(entry is not None and entry["value"] == {"route": route},
            "real route command writes the exact public value", reply=reply, entry=entry)
        history = (await ctx.peer.call("workspace.history", {"limit": 10000}))["txns"]
        txns = [row for row in history if row["id"] not in before and any(change.get("key") == route_key(agent)
            and change.get("after_version") == entry["version"] for change in row.get("changes", []))]
        ctx.check(len(txns) == 1, "configuration has one actual matching ordinary attributed transaction", history=txns)
        return dict(observed, transaction=txns[0], reply=reply)

    async def raw_configure(self, agent, value):
        ctx = self.ctx
        entry = await ctx.peer.call("workspace.get", {"key": route_key(agent)})
        op = {"op": "delete" if value is None else "set", "key": route_key(agent),
              "expect": entry["version"] if entry else 0}
        if value is not None: op["value"] = value
        reply = await ctx.peer.call("workspace.apply", {"ops": [op], "note": "owned missing/malformed request-route recovery probe"})
        observed = await self.inspect(agent)
        ctx.check(observed["entry"] is None if value is None else observed["entry"] is not None and observed["entry"]["value"] == value,
            "raw recovery probe is an actual public transaction, not a store edit", reply=reply, observed=observed)
        return observed

    async def observe(self, agent, question, match_mail):
        ctx = self.ctx
        shown = await ctx.peer.call("command.invoke", {"name": "agents.show", "args": {"resource": agent}})
        expected = await self.inspect(agent)
        last = {}
        async def published():
            nonlocal last
            resources = (await ctx.peer.call("resource.list"))["resources"]
            resource = next((row for row in resources if row["id"] == agent), None)
            if not resource: return None
            native = resource.get("data", {}).get("agent", {})
            actual = next((row for row in native.get("requests", []) if row.get("request") == question["request"]
                and row.get("revision") == question["revision"]), None)
            ctx.check(actual is not None and actual.get("status") == "pending" and actual.get("scope") == question["scope"]
                and actual.get("expires_at_ms") == question.get("expires_at_ms"),
                "routing never answers or renews the actual native pending question", native=actual)
            snapshot = await ctx.peer.call("workspace.snapshot")
            live = snapshot.get("live", {})
            projection = live.get("coord-requests-" + agent)
            panel = live.get("agents-" + agent)
            last = {"projection": projection, "panel": panel}
            if not isinstance(projection, dict) or not isinstance(panel, dict): return None
            if (projection.get("agent") != agent or projection.get("key") != route_key(agent) or
                type(projection.get("version")) is not int or
                projection.get("version") != (expected["entry"]["version"] if expected["entry"] else 0)): return None
            notices = projection.get("notices")
            if not isinstance(notices, list) or not isinstance(projection.get("human"), bool): return None
            keys = projection.get("request_keys")
            if not isinstance(keys, list) or len(keys) != 1 or not all(isinstance(key, str) and key for key in keys): return None
            reason = projection.get("fallback_reason")
            if reason == "notification_pending": return None
            waiting = waiting_rows(panel)
            human = []
            if projection["human"]:
                if len(waiting) != 1 or question["title"] not in str(waiting[0].get("text", "")): return None
                if reason and str(reason) not in str(waiting[0].get("description", "")): return None
                human = [dict(waiting[0], reason=reason, source_view="agents-" + agent)]
            elif waiting: return None
            page = await ctx.command("inbox.list", {})
            ctx.check(isinstance(page.get("items"), list) and not page.get("next"),
                "actual small routing fixture inbox is fully observed, not recipient-filtered/truncated", page=page)
            mail = []
            for record in page["items"]:
                tag = record.get("request_notice")
                if not isinstance(tag, dict) or tag.get("agent") != agent or tag.get("key") not in keys: continue
                calls = [row for row in ctx.native.records("invoke") if row.get("op") == "agent.send"
                    and row.get("args", {}).get("delivery") == record["delivery"]]
                payload = native_notice(record.get("content"))
                if payload is None and calls: payload = native_notice(calls[0].get("args", {}).get("content"))
                ctx.check(payload is not None, "current source-tagged notice has actual inspectable content", record=record, calls=calls)
                ctx.check(match_mail(payload, agent, question, native.get("epoch")) and
                    payload.get("expires_at_ms") == question.get("expires_at_ms") and payload.get("question") == actual,
                    "actual notice carries exact native identity/scope/expiry and original summary", payload=payload)
                sender = record.get("from") or {}
                if sender.get("actor") is None:
                    full = await ctx.peer.call("operational.get", {"domain": "coord", "key": "operational/coord/message/" + record["delivery"]})
                    sender = ((full.get("entry") or {}).get("value") or {}).get("from") or sender
                if calls:
                    receiver = next((row for row in resources if row["id"] == record["to"]["agent"]), {})
                    call = calls[0]
                    claim = ((call.get("witness") or {}).get("entry") or {}).get("value") or {}
                    ctx.check(len(calls) == 1 and call.get("ref") == receiver.get("backend_ref") and
                        call.get("args", {}).get("mode") == "admit" and claim.get("state") == "claimed" and
                        claim.get("delivery") == record["delivery"] and claim.get("to", {}).get("agent") == receiver.get("id") and
                        claim.get("to", {}).get("incarnation") == receiver.get("incarnation") and claim.get("request_notice") == tag,
                        "native content witness belongs to the exact claimed notice and receiver", call=call, receiver=receiver)
                    if not sender.get("actor"): sender = claim.get("from") or sender
                ctx.check(sender.get("principal") == "extension:coord" and sender.get("actor") == "extension:coord" and
                    isinstance(sender.get("author"), str) and bool(sender["author"]),
                    "notice sender is actual coord, never owner or requester", sender=sender, record=record)
                if not calls:
                    ctx.check(record.get("state") in ("held", "recorded") and not record.get("attempt"),
                        "unclaimed held mail proves only durable routing, never native delivery", record=record)
                mail.append(dict(payload, to=record["to"]["agent"], delivery=record["delivery"], source_key=tag.get("key"),
                    record=record, native_calls=calls))
            if projection.get("coordinator") and not reason:
                if not any(row.get("request") == question["request"] and row.get("revision") == question["revision"] and
                    row.get("source_key") in keys and any(item["source_key"] == row.get("source_key") and
                        item["delivery"] == row.get("delivery") for item in mail) for row in notices): return None
                if not mail: return None
            observation = {"mail": mail, "human": human, "projection": projection, "panel": panel, "shown": shown}
            ctx.evidence.append({"kind": "actual_public_request_routing", "agent": agent, "question": question,
                "native_epoch": native.get("epoch"), "route": expected, **deepcopy(observation)})
            return observation
        try: return await ctx.wait(published, label="actual current request routing projection and pinned human row")
        except CheckFailed:
            ctx.evidence.append({"kind": "unmet_request_routing_observation", **last})
            raise

    async def prepare_launch(self):
        if self.launch_prepared: return
        ctx = self.ctx
        backend = self.config.get("backend", {})
        herdr = pinned_artifact(backend.get("herdr"))
        self.hc = pinned_artifact(backend.get("hc"))
        if not (ctx.root / "ext/manifests/herdr.json").is_file(): raise HarnessGap("pinned executing backend manifest required")
        self.backend_started = True
        await self.enable("herdr", "provider", None, argv=[str(herdr), "--hc", str(self.hc), "--name", "herdr"], default=True)
        await self.enable("pi", "management", "pi.py")
        bridge, original_fake = pinned_artifact(self.config.get("pi_bridge")), pinned_artifact(self.config.get("fake_pi"))
        fake = ctx.root / "controlled-fake-pi.py"
        shutil.copyfile(original_fake, fake)
        key = "doc/settings/pi"
        current = await ctx.peer.call("workspace.get", {"key": key})
        runtime = {"name": "q-controlled-pi", "argv": ["/usr/bin/python3", str(fake), "--bridge-e2e", "--trace-file", str(ctx.root / "fake-pi.jsonl")],
            "bridge_argv": [str(bridge)]}
        await ctx.peer.call("workspace.apply", {"ops": [{"op": "set", "key": key, "value": {"runtimes": [runtime]},
            "expect": current["version"] if current else 0}], "note": "controlled fake-model Pi through the actual public launcher"})
        # Capture actual owned daemon identities by kernel socket peer, not names.
        for path in self.native_root.rglob("*"):
            if not path.is_socket(): continue
            with socket.socket(socket.AF_UNIX) as peer:
                peer.settimeout(2); peer.connect(str(path))
                pid, uid, _ = struct.unpack("3i", peer.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, 12))
            if uid != os.getuid(): raise FixtureFailed("private HC peer is not owned")
            record = identity(pid)
            if record and record not in ctx.backend.tracked: ctx.backend.tracked.append(record)
        self.launch_prepared = True

    async def launch(self, launcher, role):
        await self.prepare_launch()
        ctx = self.ctx
        if role == "coordinator":
            # This is real ordinary standing, not authority from role/name args.
            await ctx.native_set(1, principal="worker:q1-coordinator")
            parent = ctx.agents[1]["id"]
            key = "doc/agents/members/" + parent
            entry = await ctx.peer.call("workspace.get", {"key": key})
            await ctx.command("membership.set", {"op": await ctx.operation("q-parent"), "agent": parent,
                "project": None, "reports_to": None, "expect": entry["version"] if entry else 0})
            await ctx.grant("q1-launch-standing", "worker:q1-coordinator", ["configure", "membership", "answer"], "*")
        # pi.new chooses defaults from the authenticated C2 caller. No asserted role
        # or on_behalf_of body field is sent. An uncertain/not-ready spawn is never replayed.
        result = await launcher.call("command.invoke", {"name": "pi.new", "args": {"runtime": "q-controlled-pi"}}, timeout=18)
        if result.get("host"): self.hosts.append(result["host"])
        ctx.check(isinstance(result.get("agent"), str) and result.get("ready") is not False,
            "real shipped Pi launcher observes a child, without a native model", result=result)
        resource = next((row for row in (await ctx.peer.call("resource.list", {"host": result["host"]}))["resources"]
            if row["id"] == result["agent"] and row.get("available") and row.get("kind") == "agent"), None)
        ctx.check(resource is not None and resource.get("host") == result["host"],
            "launcher receipt is corroborated by actual hosted resource facts", result=result, resource=resource)
        inspected = await self.inspect(result["agent"])
        expected_route = "coordinator" if role == "coordinator" else "human"
        ctx.check(inspected["entry"] is not None and inspected["entry"]["value"] == {"route": expected_route},
            "actual launcher initialization explicitly records its caller-derived route", inspected=inspected)
        history = (await ctx.peer.call("workspace.history", {"limit": 10000}))["txns"]
        initial = [row for row in history if any(change.get("key") == route_key(result["agent"]) and
            change.get("after_version") == inspected["entry"]["version"] for change in row.get("changes", []))]
        expected_principal = "worker:q1-coordinator" if role == "coordinator" else "owner"
        ctx.check(len(initial) == 1 and initial[0].get("principal") == expected_principal and
            initial[0].get("actor") == "extension:coord" and initial[0].get("initiator") == expected_principal,
            "launcher initialization attribution comes from actual C2, never role/body assertions", history=initial)
        if role == "coordinator":
            ctx.check(inspected["public"].get("coordinator") == ctx.agents[1]["id"] and
                inspected["public"].get("route") == "coordinator" and inspected["public"].get("human") is False,
                "coordinator launch has actual recorded parent and current answer standing", inspected=inspected)
        return dict(inspected, launch_evidence={"reply": result, "resource": resource, "history": initial})

    async def hc_call(self, *argv):
        result = await asyncio.to_thread(subprocess.run, [str(self.hc), *argv], env=self.ctx.backend.env,
            capture_output=True, text=True, timeout=8)
        if result.returncode: raise FixtureFailed("private HC cleanup command failed: " + result.stderr[-1024:])
        return json.loads(result.stdout)

    async def close(self):
        if self.closed: return
        self.closed = True
        ctx = self.ctx
        error = None
        try:
            if self.backend_started:
                # Fresh private service/core scope: enumerate even after an uncertain
                # spawn reply. No retry, no real user terminal can belong here.
                rows = (await ctx.peer.call("resource.list", {"provider": "herdr"}))["resources"]
                for row in rows:
                    if row.get("kind") != "terminal": continue
                    if row.get("state") != "finished":
                        await ctx.peer.call("resource.invoke", {"resource": row["id"], "op": "terminal.kill", "args": {}}, timeout=8)
                        async def finished(rid=row["id"]):
                            current = (await ctx.peer.call("resource.list", {"id": rid}))["resources"]
                            return any(item["id"] == rid and item.get("state") == "finished" for item in current)
                        await ctx.wait(finished, label="owned launcher host native end")
                    await ctx.peer.call("resource.forget", {"resource": row["id"]}, timeout=8)
                inventory = await self.hc_call("ls", "--all", "--json")
                ctx.check(empty_terminal_inventory(inventory), "all exact owned launcher terminals are removed", component="harness", native=inventory)
        except BaseException as failure: error = failure
        try:
            ctx.track_children()
            await asyncio.to_thread(ctx.backend.stop, False)
            if self.backend_started:
                stopped = await self.hc_call("server", "stop")
                if not isinstance(stopped.get("stopped"), bool): raise FixtureFailed("private HC stop lacks observed result")
                ctx.evidence.append({"kind": "routing_private_hc_stop", "reply": stopped})
        except BaseException as failure: error = failure
        if self.native_root and error is None: shutil.rmtree(self.native_root)
        ctx.evidence.append({"kind": "request_routing_fixture_cleanup", "private_root_removed": not self.native_root or not self.native_root.exists(),
            "hosts_observed": self.hosts, "error": str(error) if error else None})
        if error: raise FixtureFailed("owned routing fixture cleanup did not complete") from error


async def route_operation(ctx, operation, match_mail, **params):
    binding = getattr(ctx, "_request_routing_binding", None)
    if binding is None:
        binding = PublicRequestRouting(ctx)
        ctx._request_routing_binding = binding
    await binding.prepare()
    if operation == "ensure": return {"prepared": True}
    if operation == "configure": return await binding.configure(params["agent"], params["route"], params.get("peer") or ctx.peer)
    if operation == "raw_configure": return await binding.raw_configure(params["agent"], params["value"])
    if operation == "inspect": return await binding.inspect(params["agent"])
    if operation == "observe": return await binding.observe(params["agent"], params["question"], match_mail)
    if operation == "launch": return await binding.launch(params["launcher"], params["role"])
    raise HarnessGap("unknown normalized request-routing binding operation: " + operation)
