"""Part A L3 branch helper for many-endpoint server hosts (Codex): a real host replacement with a late old-host end.

agent-v0-conformance L3: the host of a bridge that declares owns_lifecycle finishes -> its endpoints `ended`; "a late
end event from an old host incarnation ends nothing". The shared handler cases.host_lifecycle covers a process-owned
one-endpoint host. This module adds NO case registration: K1 delegates from host_lifecycle to
`many_host_replacement(ctx)` when `is_many_server(declaration, host)` holds (declared caps.host {kind: server,
endpoints: many} and public facts host.owns_lifecycle true), and keeps its inherited host_generation_fence there.

The core admits one live connection per provider name (src/provider_wire.rs: "provider name already has a live
connection"), so a replacement registers only after the old bridge's connection is gone, and a late end from the old
incarnation can only come from the old bridge coming back afterwards. The scenario stages exactly that with public
mechanism and the fixture's existing native controls (no new verb, no native field, no auth or model):
1. Safe mode disconnects the old bridge (incarnation A) without ending anything (as L4 does).
2. The harness-owned old bridge process is frozen so it cannot reconnect; safe mode is turned off.
3. A replacement bridge is launched through public resource.spawn bridge:true from the SAME expanded manifest argv
   (same --instance: same provider name, new random incarnation B), with fresh unique native control/ledger paths.
4. Work starts on the new host: a public send on one replacement endpoint; the fake app-server leaves it running.
5. The old app-server really exits (existing host_exit), and only then is the old bridge resumed into that end.
6. The replacement's work settles normally; then the replacement's own host really exits.
Graded from public facts (grade_replacement). The freeze is harness orchestration of a harness-owned process (Linux,
like the rest of this harness), not part of any contract.
"""

import asyncio
import hashlib
import os
from pathlib import Path
import secrets
import signal

from . import cases
from .core import Native
from .results import CheckFailed, HarnessGap, check

LATE_END_WINDOW_S = 3.0   # how long the resumed old bridge has to (wrongly) affect the replacement
SETUP_S = 10.0            # each bounded setup wait (the handler deadline is 45 s)
MAX_SOCKET_PATH = 100     # Unix socket paths stay under the 103-byte macOS limit


def host_of(facts):
    host = (facts or {}).get("host") or {}
    return {key: host.get(key) for key in ("id", "provider", "instance", "incarnation", "kind", "owns_lifecycle")}


def identity_of(rid, facts):
    """The public identity a run is bound to: endpoint id, its host incarnation, session and run."""
    facts = facts or {}
    session = facts.get("session") if isinstance(facts.get("session"), dict) else {}
    return {"endpoint": rid, "host_id": host_of(facts).get("id"), "instance": host_of(facts).get("instance"),
            "incarnation": host_of(facts).get("incarnation"), "session": session.get("id"), "run": facts.get("run")}


def is_many_server(declaration, host):
    """K1's delegation gate: declared caps.host {kind: server, endpoints: many} and public host.owns_lifecycle true."""
    return (isinstance(declaration, dict) and declaration.get("kind") == "server" and declaration.get("endpoints") == "many"
            and isinstance(host, dict) and host.get("owns_lifecycle") is True)


def grade_replacement(obs):
    """Pure grading of one replacement run from public observations only (see many_host_replacement for how each is
    observed). Returns [(ok, message, component)]."""
    a, b = obs.get("old_incarnation"), obs.get("new_incarnation")
    new_ids = obs.get("new_ids") or []
    results = []
    def add(ok, message, component="adapter"):
        results.append((bool(ok), message, component))
    add(isinstance(a, str) and isinstance(b, str) and a and b and a != b and obs.get("old_provider") == obs.get("new_provider")
        and new_ids and not set(new_ids) & set(obs.get("old_ids") or []),
        "a real replacement: same provider, new host incarnation, new endpoints", "harness")
    add(obs.get("same_image") is True, "the replacement bridge runs the same pinned adapter image", "fixture")
    add(obs.get("order") is True,
        "the old host ended and its bridge resumed only after the replacement registered with running work", "harness")
    late = obs.get("late_return") or {}
    add("exit" in (late.get("old_native_rows") or []) and late.get("old_bridge_resumed") is True,
        "the old host really ended (its own native ledger) and its bridge was resumed into that end", "harness")
    after = obs.get("after_end") or {}
    add(all(isinstance(after.get(rid), dict) and after[rid].get("lifecycle") != "ended" for rid in new_ids),
        "the late old-host end ended no replacement endpoint")
    add(all((obs.get("available_after") or {}).get(rid) is True for rid in new_ids),
        "every replacement endpoint stayed available after the late old-host end")
    before, kept = obs.get("work_before") or {}, obs.get("work_after") or {}
    add(before.get("run") is not None and before.get("incarnation") == b and kept == before
        and (after.get(before.get("endpoint")) or {}).get("state") == "running",
        "the replacement's running work kept its exact identity (endpoint, host incarnation, session, run) across the "
        "late old-host end")
    settled = obs.get("settled") or {}
    add(settled.get("state") == "idle" and settled.get("lifecycle") == "live" and settled.get("completion") is not None
        and host_of(settled).get("incarnation") == b,
        "the replacement's work then completed normally and its endpoint stayed live on incarnation B")
    add(not any(isinstance(facts, dict) and facts.get("lifecycle") == "ended" and host_of(facts).get("incarnation") == b
                for _, facts in obs.get("history", [])),
        "no ended fact was attributed to the replacement incarnation before its own host ended")
    add(not any(row is not None and row[0] is True for row in (obs.get("old_after") or {}).values()),
        "the old incarnation's endpoints were not resurrected as available by its late return")
    provider = late.get("provider_after") or {}
    add((provider.get("instance") or "").endswith(":" + str(b)) and provider.get("state") == "connected",
        "the provider is still the replacement's connected instance after the old incarnation's late return")
    own = obs.get("own_end") or {}
    add(new_ids and all(isinstance(own.get(rid), dict) and own[rid].get("lifecycle") == "ended"
                        and host_of(own[rid]).get("incarnation") == b for rid in new_ids),
        "the replacement's own host end (owns_lifecycle) ended its endpoints, attributed to incarnation B")
    return results


def replacement_argv(argv, old_paths, new_paths):
    """The same expanded manifest argv for a second bridge, with only its private native control/ledger paths swapped.
    Each old path must occur exactly once, and --instance must be pinned (else a relaunch is another provider)."""
    if "--instance" not in argv or any(argv.count(path) != 1 for path in old_paths):
        return None
    return [new_paths.get(value, value) for value in argv]


async def poll(predicate, timeout, interval=0.05):
    """Bounded polling of an async predicate; its first truthy value, or None at the deadline."""
    loop = asyncio.get_running_loop()
    deadline = loop.time() + timeout
    while loop.time() < deadline:
        value = await predicate()
        if value:
            return value
        await asyncio.sleep(interval)
    return None


def signal_owned(process, sig):
    """Signal a harness-owned process only if it is still the same process (pid and start identity)."""
    from pi_provider_e2e import live
    if process and live(process):
        os.kill(process["pid"], sig)
        return True
    return False


def executable(process):
    """pid, birth and the executable's sha256 of a live owned process (None where unavailable)."""
    if not process:
        return None
    try:
        digest = hashlib.sha256(Path(f"/proc/{process['pid']}/exe").read_bytes()).hexdigest()
    except OSError:
        digest = None
    return {"pid": process.get("pid"), "birth": process.get("birth"), "sha256": digest}


def ledger_summary(native):
    rows = native.records() if Path(native.path).exists() else []
    return {"path": str(native.path), "rows": len(rows), "last": [row.get("kind") for row in rows[-6:]]}


async def many_host_replacement(ctx):
    """The L3 branch for an owns_lifecycle server host with many endpoints. Called by K1 from cases.host_lifecycle after
    is_many_server(...) holds; host_generation_fence stays with the caller."""
    from pi_provider_e2e import identity, live
    facts = await ctx.facts()
    declaration, host = facts.get("caps", {}).get("host", {}), host_of(facts)
    if not is_many_server(declaration, host):
        raise HarnessGap("many_host_replacement called for a host that is not a declared owns_lifecycle server/many host")
    root = Path(ctx.root)
    nonce = secrets.token_hex(4)
    native_b = Native(root / f"l3-{nonce}.sock", root / f"l3-{nonce}.jsonl")
    if len(str(native_b.endpoint).encode()) > MAX_SOCKET_PATH or native_b.endpoint.exists() or native_b.path.exists():
        raise HarnessGap("no fresh short private control socket path for the replacement fixture")
    old_paths = [str(ctx.native.endpoint), str(ctx.native.path)]
    argv = replacement_argv(ctx.argv, old_paths, {old_paths[0]: str(native_b.endpoint), old_paths[1]: str(native_b.path)})
    if argv is None:
        raise HarnessGap("L3 replacement needs the expanded manifest argv to pin --instance and to carry the native "
                         "control/ledger paths exactly once (a relaunch without a stable label is another provider)")
    image = ctx.metadata.get("adapter_image", {})
    bridge_a = {"pid": image.get("pid"), "birth": image.get("birth")} if image.get("pid") else None
    if not bridge_a or not image.get("sha256"):
        raise HarnessGap("L3 replacement needs the old bridge's process identity and image to hold it off the core")
    old_ids = sorted(row["id"] for row in ctx.agents)
    old_provider = ctx.agents[0].get("provider")
    a = host.get("incarnation")
    terminal_provider = next((row.get("provider") for row in await ctx.resources() if row.get("id") == ctx.host), None)
    if not terminal_provider:
        raise HarnessGap("L3 replacement cannot identify the terminal backend hosting the bridge")
    frozen = safe = False
    bridge_b = native_b_process = None
    teardown = {}
    try:
        # 1-2. Disconnect the old bridge without ending anything, then hold it off the core.
        await ctx.peer.call("command.invoke", {"name": "vqro.safe_mode", "args": {"on": True}})
        safe = True
        await cases.lifecycle_wait(ctx, lambda: all(ctx.resource_cache.get(rid, {}).get("available") is False for rid in old_ids),
                                   "old bridge disconnect was not observable")
        frozen = signal_owned(bridge_a, signal.SIGSTOP)
        check(frozen, "old bridge process is not live to hold off the core", component="harness")
        await ctx.peer.call("command.invoke", {"name": "vqro.safe_mode", "args": {"on": False}})
        safe = False
        async def backend_back():
            providers = (await ctx.peer.call("workspace.snapshot")).get("providers", {})
            return (providers.get(terminal_provider) or {}).get("state") == "connected"
        if not await poll(backend_back, SETUP_S):
            raise HarnessGap("terminal backend did not reconnect after safe mode within the bounded setup")
        # 3. The replacement: same expanded manifest argv, fresh native control/ledger.
        spawned = await ctx.peer.call("resource.spawn", {"bridge": True, "name": "agent-conformance-l3-replacement",
            "argv": argv, "cwd": str(root / "work"), "env": {}, "deadline_ms": 1500})
        check(spawned.get("outcome") == "observed", "replacement bridge launch not observed", component="fixture")
        terminal = await asyncio.to_thread(ctx.backend.hc, "get", spawned["resource"]["backend_ref"])
        bridge_b = identity(terminal["pid"])
        if bridge_b:
            ctx.backend.tracked.append(bridge_b)
        image_b = executable(bridge_b)
        async def replaced():
            rows = [row for row in await ctx.resources() if row.get("kind") == "agent" and row.get("available")
                    and row["id"] not in old_ids]
            fresh = [row for row in rows if host_of((row.get("data") or {}).get("agent")).get("incarnation") not in (None, a)]
            return fresh if len(fresh) >= len(old_ids) else None
        new_rows = await poll(replaced, SETUP_S)
        if not new_rows:
            raise HarnessGap("replacement bridge did not register its endpoints within the bounded setup")
        new_ids = sorted(row["id"] for row in new_rows)
        new_host = host_of(new_rows[0]["data"]["agent"])
        b = new_host.get("incarnation")
        native_b_process = identity((await native_b.call("state"))["pid"])
        if native_b_process:
            ctx.backend.tracked.append(native_b_process)
        native_b_image = executable(native_b_process)
        # 4. New-host work: a public send on one replacement endpoint; the app-server leaves the turn running.
        work_rid = new_ids[0]
        await ctx.ensure_session(rid=work_rid)
        await ctx.send("L3-replacement-work", rid=work_rid)
        running = await ctx.wait_facts(lambda f: f.get("state") == "running" and f.get("run") is not None,
                                       rid=work_rid, timeout=5)
        work_before = identity_of(work_rid, running)
        # 5. The late old-host end: the old app-server exits, then its bridge comes back.
        history_mark = len(ctx.resource_history)
        ledger_mark = ctx.native.mark()
        await ctx.native.call("host_exit", code=0)
        resumed = signal_owned(bridge_a, signal.SIGCONT)
        frozen = False
        order = True  # steps 1-4 were each observed before step 5 began
        await asyncio.sleep(LATE_END_WINDOW_S)
        provider_after = (await ctx.peer.call("workspace.snapshot")).get("providers", {}).get(old_provider) or {}
        late_return = {"old_native_rows": [row.get("kind") for row in ctx.native.records(after=ledger_mark)][-8:],
                       "old_bridge_resumed": resumed, "old_bridge_live_after_window": bool(live(bridge_a)),
                       "provider_after": {k: provider_after.get(k) for k in ("instance", "generation", "state", "error")}}
        listed = {row["id"]: row for row in await ctx.resources()}
        def agent(rid): return ((listed.get(rid) or {}).get("data") or {}).get("agent")
        after_end = {rid: agent(rid) for rid in new_ids}
        available_after = {rid: (listed.get(rid) or {}).get("available") for rid in new_ids}
        old_after = {rid: ((listed[rid].get("available"), agent(rid)) if rid in listed else None) for rid in old_ids}
        work_after = identity_of(work_rid, after_end.get(work_rid))
        history = [(resource.get("id"), (resource.get("data") or {}).get("agent"))
                   for _, _, resource, _ in list(ctx.resource_history)[history_mark:]]
        # 6. The replacement's work completes normally, then its own host really ends.
        await native_b.call("turn", state="settled")
        try:
            settled = await ctx.wait_facts(lambda f: f.get("state") == "idle", rid=work_rid, timeout=5)
        except CheckFailed:
            settled = await ctx.facts(work_rid)
        await native_b.call("host_exit", code=0)
        def own_end_seen():
            rows = {rid: (ctx.resource_cache.get(rid) or {}).get("data", {}).get("agent") for rid in new_ids}
            return rows if all((row or {}).get("lifecycle") == "ended" for row in rows.values()) else None
        try:
            await ctx.peer.wait(own_end_seen, timeout=SETUP_S)
        except (asyncio.TimeoutError, Exception):
            pass  # graded below from what was observed
        own_end = {rid: (ctx.resource_cache.get(rid) or {}).get("data", {}).get("agent") for rid in new_ids}
        async def replacement_gone():
            return not (native_b_process and live(native_b_process)) and not (bridge_b and live(bridge_b))
        await poll(replacement_gone, 3.0)  # bounded; whatever remains is reaped by the fixture's owned cleanup
        teardown = {"native_b_live": bool(native_b_process and live(native_b_process)),
                    "bridge_b_live": bool(bridge_b and live(bridge_b)), "tracked_for_owned_cleanup": True}
        obs = {"old_incarnation": a, "new_incarnation": b, "old_provider": old_provider,
               "new_provider": new_rows[0].get("provider"), "old_ids": old_ids, "new_ids": new_ids,
               "same_image": bool(image_b and image_b.get("sha256") == image.get("sha256")),
               "work_before": work_before, "work_after": work_after, "after_end": after_end,
               "available_after": available_after, "old_after": old_after, "settled": settled,
               "history": history, "order": order, "late_return": late_return, "own_end": own_end}
        brief = lambda f: {k: (f or {}).get(k) for k in ("state", "run", "lifecycle")} | {
            "incarnation": host_of(f).get("incarnation")}
        ctx.evidence.append({"kind": "l3_replacement", "old_host": host, "new_host": new_host,
                             "old_ids": old_ids, "new_ids": new_ids, "work_before": work_before, "work_after": work_after,
                             "after_end": {rid: brief(f) for rid, f in after_end.items()},
                             "old_after_end": {rid: ({"available": row[0]} | brief(row[1])) if row else "absent"
                                               for rid, row in old_after.items()},
                             "settled": brief(settled), "own_end": {rid: brief(f) for rid, f in own_end.items()},
                             "late_return": late_return,
                             "executables": {"old_bridge": {"pid": image.get("pid"), "birth": image.get("birth"),
                                                            "sha256": image.get("sha256")},
                                             "replacement_bridge": image_b,
                                             "replacement_native": native_b_image},
                             "ledgers": {"old": ledger_summary(ctx.native), "replacement": ledger_summary(native_b)},
                             "replacement_argv_swapped": {"control": str(native_b.endpoint), "ledger": str(native_b.path)},
                             "teardown": teardown})
        results = grade_replacement(obs)
        for ok, message, component in results:
            if component in ("harness", "fixture"):
                check(ok, message, component=component)
        for ok, message, component in results:
            check(ok, message, component=component)
    finally:
        if frozen:
            signal_owned(bridge_a, signal.SIGCONT)  # never leave the old bridge frozen
        if safe:
            await ctx.peer.call("command.invoke", {"name": "vqro.safe_mode", "args": {"on": False}})
        if native_b_process and live(native_b_process):
            try:
                await native_b.call("host_exit", code=0)
            except Exception:
                pass  # tracked processes are reaped by the fixture's owned cleanup either way
