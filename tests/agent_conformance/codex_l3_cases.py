"""Part A L3 for many-endpoint server hosts (Codex): a real host replacement with a late old-host end.

agent-v0-conformance L3: "a late end event from an old host incarnation ends nothing". The shared handler
(cases.host_lifecycle) covers a process-owned one-endpoint host and raises HarnessGap for anything else. This module
registers L3 as a wrapper that delegates to it unchanged unless the adapter declares caps.host {kind: server,
endpoints: many} and its facts report an owns_lifecycle host; then it runs the real replacement scenario below.

The core admits one live connection per provider name (src/provider_wire.rs: "provider name already has a live
connection"), so a replacement registers only after the old bridge's connection is gone, and a late end from the old
incarnation can only come from the old bridge coming back afterwards. The scenario stages exactly that, with public
mechanism and the fixture's existing native controls only (no native field is invented):
1. Old bridge (incarnation A) serves its endpoints. Safe mode disconnects it without ending anything (as L4 does).
2. The old bridge process is frozen so it cannot reconnect; safe mode is turned off.
3. A replacement bridge is launched through public resource.spawn bridge:true from the SAME manifest argv (same
   --instance label: same provider name, new random incarnation B), with its own native control socket and ledger.
4. Work starts on the new host: a public send on one of its endpoints; the fake app-server leaves that turn running.
5. The old app-server really exits (existing host_exit control), and only then is the old bridge resumed, so its
   end evidence for incarnation A arrives after the replacement is live.
6. Graded from public facts and events: no replacement endpoint ends or disappears; the replacement's run is unchanged
   and still running; it then settles and stays live; no ended fact is ever attributed to incarnation B; the old
   endpoints are never resurrected as live.
The freeze is test orchestration of the harness-owned bridge process (Linux, like the rest of this harness); it is
not part of any contract.
"""

import asyncio
import os
from pathlib import Path
import signal

from . import cases
from .cases import case
from .core import Native
from .results import CheckFailed, HarnessGap, check

LATE_END_WINDOW_S = 3.0   # how long the resumed old bridge has to (wrongly) affect the replacement
SETUP_S = 10.0            # each bounded setup wait (the handler deadline is 45 s)


def host_of(facts):
    host = (facts or {}).get("host") or {}
    return {key: host.get(key) for key in ("id", "provider", "instance", "incarnation", "kind", "owns_lifecycle")}


def is_many_server(declaration, host):
    """The branch this module owns: declared caps.host {kind: server, endpoints: many} with an owns_lifecycle host."""
    return (isinstance(declaration, dict) and declaration.get("kind") == "server" and declaration.get("endpoints") == "many"
            and host.get("kind") == "server" and host.get("owns_lifecycle") is True)


def grade_replacement(obs):
    """Pure grading of one replacement run from public observations only:
    old_incarnation, new_incarnation, old_provider, new_provider, old_ids, new_ids, work_rid,
    run_before (the work endpoint's run while running, before the late end),
    after_end {rid: facts or None} and available_after {rid: bool} for the replacement endpoints once the late end
    window passed, old_after {rid: (available, facts) or None} for the old endpoints then,
    settled (the work endpoint's facts after the native settle),
    history [(rid, facts)] every resource fact observed from the late end onward,
    order (the old host ended and its bridge resumed only after the replacement registered with running work),
    late_return {old_native_exit_rows, old_bridge_live_after_window, provider_after}: the old host's own ledger end and
    that its bridge was resumed afterwards (whether it then exits or keeps retrying is recorded, not graded), and the
    provider's public state after the window.
    Returns [(ok, message, component)]."""
    a, b = obs.get("old_incarnation"), obs.get("new_incarnation")
    new_ids = obs.get("new_ids") or []
    results = []
    def add(ok, message, component="adapter"):
        results.append((bool(ok), message, component))
    add(isinstance(a, str) and isinstance(b, str) and a and b and a != b and obs.get("old_provider") == obs.get("new_provider")
        and new_ids and not set(new_ids) & set(obs.get("old_ids") or []),
        "a real replacement: same provider, new host incarnation, new endpoints", "harness")
    add(obs.get("order") is True,
        "the old host ended and its bridge resumed only after the replacement registered with running work", "harness")
    late = obs.get("late_return") or {}
    add("exit" in (late.get("old_native_exit_rows") or []) and late.get("old_bridge_resumed") is True,
        "the old host really ended (its own native ledger) and its bridge was resumed into that end", "harness")
    add(((late.get("provider_after") or {}).get("instance") or "").endswith(":" + str(b))
        and (late.get("provider_after") or {}).get("state") == "connected",
        "the provider is still the replacement's connected instance after the old incarnation's late return")
    after = obs.get("after_end") or {}
    add(all(isinstance(after.get(rid), dict) and after[rid].get("lifecycle") != "ended" for rid in new_ids),
        "the late old-host end ended no replacement endpoint")
    add(all((obs.get("available_after") or {}).get(rid) is True for rid in new_ids),
        "every replacement endpoint stayed available after the late old-host end")
    work = after.get(obs.get("work_rid")) or {}
    run = obs.get("run_before")
    add(run is not None and work.get("state") == "running" and work.get("run") == run,
        "the replacement's running work was preserved across the late old-host end (same run, still running)")
    settled = obs.get("settled") or {}
    add(settled.get("state") == "idle" and settled.get("lifecycle") == "live" and settled.get("completion") is not None,
        "the replacement's work then completed normally and its endpoint stayed live")
    add(not any(isinstance(facts, dict) and facts.get("lifecycle") == "ended" and host_of(facts).get("incarnation") == b
                for _, facts in obs.get("history", [])),
        "no ended fact was ever attributed to the replacement host incarnation")
    add(not any(row is not None and row[0] is True for row in (obs.get("old_after") or {}).values()),
        "the old incarnation's endpoints were not resurrected as available by its late return")
    return results


def replacement_argv(argv, old_paths, new_paths):
    """The same manifest argv for a second bridge, with only its private native control/ledger paths swapped."""
    if "--instance" not in argv or not all(path in argv for path in old_paths):
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


@case("L3")
async def host_lifecycle_with_replacement(ctx):
    facts = await ctx.facts()
    declaration, host = facts.get("caps", {}).get("host", {}), host_of(facts)
    if not is_many_server(declaration, host):
        return await cases.host_lifecycle(ctx)  # the existing process/one branch, unchanged
    from pi_provider_e2e import identity
    root = Path(ctx.root)
    old_paths = [str(ctx.native.endpoint), str(ctx.native.path)]
    native_b = Native(root / "c3.sock", root / "native-l3.jsonl")
    argv = replacement_argv(ctx.argv, old_paths, {old_paths[0]: str(native_b.endpoint), old_paths[1]: str(native_b.path)})
    if argv is None:
        raise HarnessGap("L3 replacement needs the manifest argv to pin --instance and the native control/ledger paths "
                         "(a relaunch without a stable instance label is a different provider, not a replacement)")
    image = ctx.metadata.get("adapter_image", {})
    bridge_a = {"pid": image.get("pid"), "birth": image.get("birth")} if image.get("pid") else None
    if not bridge_a:
        raise HarnessGap("L3 replacement needs the old bridge's process identity to hold it off the core")
    old_ids = sorted(row["id"] for row in ctx.agents)
    old_provider = ctx.agents[0].get("provider")
    a = host.get("incarnation")
    # The terminal backend that hosts the bridges (it must be back from safe mode before the replacement spawns).
    terminal_provider = next((row.get("provider") for row in await ctx.resources() if row.get("id") == ctx.host), None)
    if not terminal_provider:
        raise HarnessGap("L3 replacement cannot identify the terminal backend hosting the bridge")
    frozen = safe = False
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
        # 3. The replacement: same manifest argv, own native control/ledger.
        spawned = await ctx.peer.call("resource.spawn", {"bridge": True, "name": "agent-conformance-l3-replacement",
            "argv": argv, "cwd": str(root / "work"), "env": {}, "deadline_ms": 1500})
        check(spawned.get("outcome") == "observed", "replacement bridge launch not observed", component="fixture")
        terminal = await asyncio.to_thread(ctx.backend.hc, "get", spawned["resource"]["backend_ref"])
        bridge_b = identity(terminal["pid"])
        if bridge_b:
            ctx.backend.tracked.append(bridge_b)
        async def replaced():
            rows = [row for row in await ctx.resources() if row.get("kind") == "agent" and row.get("available")
                    and row["id"] not in old_ids]
            fresh = [row for row in rows if host_of((row.get("data") or {}).get("agent")).get("incarnation") not in (None, a)]
            return fresh if len(fresh) >= len(old_ids) else None
        new_rows = await poll(replaced, SETUP_S)
        if not new_rows:
            raise HarnessGap("replacement bridge did not register its endpoints within the bounded setup")
        new_ids = sorted(row["id"] for row in new_rows)
        b = host_of(new_rows[0]["data"]["agent"]).get("incarnation")
        native_process = identity((await native_b.call("state"))["pid"])
        if native_process:
            ctx.backend.tracked.append(native_process)
        # 4. New-host work: a public send on one replacement endpoint; the app-server leaves the turn running.
        work_rid = new_ids[0]
        await ctx.ensure_session(rid=work_rid)
        await ctx.send("L3-replacement-work", rid=work_rid)
        running = await ctx.wait_facts(lambda f: f.get("state") == "running" and f.get("run") is not None,
                                       rid=work_rid, timeout=5)
        run_before = running.get("run")
        # 5. The late old-host end: the old app-server exits, then its bridge comes back.
        history_mark = len(ctx.resource_history)
        ledger_mark = ctx.native.mark()
        await ctx.native.call("host_exit", code=0)
        resumed = signal_owned(bridge_a, signal.SIGCONT)
        frozen = False
        order = True  # steps 1-4 were each observed before step 5 began
        await asyncio.sleep(LATE_END_WINDOW_S)
        from pi_provider_e2e import live
        provider_after = (await ctx.peer.call("workspace.snapshot")).get("providers", {}).get(old_provider) or {}
        late_return = {"old_native_exit_rows": [row.get("kind") for row in ctx.native.records(after=ledger_mark)][-8:],
                       "old_bridge_resumed": resumed, "old_bridge_live_after_window": bool(live(bridge_a)),
                       "provider_after": {k: provider_after.get(k) for k in ("instance", "generation", "state", "error")}}
        listed = {row["id"]: row for row in await ctx.resources()}
        def agent(rid): return ((listed.get(rid) or {}).get("data") or {}).get("agent")
        after_end = {rid: agent(rid) for rid in new_ids}
        available_after = {rid: (listed.get(rid) or {}).get("available") for rid in new_ids}
        old_after = {rid: ((listed[rid].get("available"), agent(rid)) if rid in listed else None) for rid in old_ids}
        history = [(resource.get("id"), (resource.get("data") or {}).get("agent"))
                   for _, _, resource, _ in list(ctx.resource_history)[history_mark:]]
        # 6. The replacement's work completes normally afterwards.
        await native_b.call("turn", state="settled")
        try:
            settled = await ctx.wait_facts(lambda f: f.get("state") == "idle", rid=work_rid, timeout=5)
        except CheckFailed:
            settled = await ctx.facts(work_rid)
        obs = {"old_incarnation": a, "new_incarnation": b, "old_provider": old_provider,
               "new_provider": new_rows[0].get("provider"), "old_ids": old_ids, "new_ids": new_ids,
               "work_rid": work_rid, "run_before": run_before, "after_end": after_end,
               "available_after": available_after, "old_after": old_after, "settled": settled,
               "history": history, "order": order, "late_return": late_return}
        brief = lambda f: {k: (f or {}).get(k) for k in ("state", "run", "lifecycle")}
        ctx.evidence.append({"kind": "l3_replacement", "old_host": host, "old_ids": old_ids,
                             "new_host": host_of(new_rows[0]["data"]["agent"]), "new_ids": new_ids,
                             "work_rid": work_rid, "run_before": run_before,
                             "after_end": {rid: brief(f) for rid, f in after_end.items()},
                             "old_after_end": {rid: ({"available": row[0]} | brief(row[1])) if row else "absent"
                                               for rid, row in old_after.items()},
                             "settled": brief(settled), "old_bridge": bridge_a, "replacement_bridge": bridge_b,
                             "late_return": late_return})
        results = grade_replacement(obs)
        for ok, message, component in results:
            if component == "harness":
                check(ok, message, component="harness")
        for ok, message, component in results:
            check(ok, message, component=component)
    finally:
        if frozen:
            signal_owned(bridge_a, signal.SIGCONT)  # never leave the old bridge frozen
        if safe:
            await ctx.peer.call("command.invoke", {"name": "vqro.safe_mode", "args": {"on": False}})
        try:
            await native_b.call("host_exit", code=0)
        except Exception:
            pass  # already gone; tracked processes are reaped by the fixture's owned cleanup
