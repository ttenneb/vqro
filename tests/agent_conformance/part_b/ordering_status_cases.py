# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Bennett Garcia
"""Part B submission identity, ordering visibility, advisory status and membership probes (I17, I22, T13, L10).

Only public primitives and the owned native oracle; every pass names its observed witness.
"""
import asyncio
import hashlib
import json

from .cases import case, invocations, settled
from ..wire import RpcError


def digest(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True, ensure_ascii=False, separators=(",", ":")).encode()).hexdigest()


def receipts_isolated(rows):
    """I17: every (delivery, record) pair carries a projected receipt naming exactly its own delivery, and the
    deliveries differ. Crossed but individually valid receipt envelopes fail."""
    deliveries = [delivery for delivery, _ in rows]
    return len(set(deliveries)) == len(deliveries) and all(
        ((record.get("result") or {}).get("receipt") or {}).get("delivery") == delivery for delivery, record in rows)


def operator_view_names(surfaces, successor, predecessor):
    """I22: some operator surface shows the successor held ordering_unknown together with the predecessor's id.
    A surface that names neither (for example empty attention) fails."""
    def rows(node):
        if isinstance(node, dict):
            yield node
            for value in node.values(): yield from rows(value)
        elif isinstance(node, list):
            for value in node: yield from rows(value)
    # Bound to one row: the row that names the successor must itself carry ordering_unknown and the predecessor
    # (a legend elsewhere saying ordering_unknown while the successor's row says paused does not count).
    for surface in surfaces:
        for row in rows(surface):
            own = [value for value in row.values() if not isinstance(value, (dict, list))]
            if successor in own and "ordering_unknown" in own and predecessor in json.dumps(row):
                return True
    return False


def frozen_set_kept(initial, *replays):
    """L10: every replay of the frozen scope action reports exactly the recorded target set (no extra or missing
    target, same receipt)."""
    return all(sorted(replay.get("targets") or []) == sorted(initial.get("targets") or [])
               and replay.get("receipt") == initial.get("receipt")
               and replay.get("selected", len(replay.get("targets") or [])) == initial.get("selected", len(initial.get("targets") or []))
               for replay in replays)


def status_effects(events):
    """T13: public events that a status report must not cause: core effects, notifications and status publications."""
    found = []
    for event in events:
        kind = event.get("type")
        data = event.get("data") if isinstance(event.get("data"), dict) else event
        if kind == "effect" or "notif" in str(kind) or (kind == "view.content" and data.get("view") == "agents.terminal_status"):
            found.append(kind if kind != "view.content" else "view.content:agents.terminal_status")
    return found


async def _receipts_attached(ctx):
    receipts = (await ctx.command("status")).get("receipts") or {}
    ctx.evidence.append({"kind": "receipt_stream_status", "receipts": receipts})
    return receipts.get("configured", 0) >= 1 and receipts.get("attached", 0) == receipts.get("configured")


async def operation(ctx, suffix="op"):
    """An op id in the current window; the serial is taken before the await so concurrent callers never share one."""
    ctx.serial += 1
    serial = ctx.serial
    window = await ctx.command("inbox.window")
    return window["epoch"] + f":b{serial}:{suffix}"


async def roll_window(ctx, peer, target):
    """Roll the operation window with refused mutations (a refusal also takes a window slot); returns the new epoch."""
    first = (await ctx.command("inbox.window"))["epoch"]
    for count in range(1, 2200):
        try:
            await ctx.command("inbox.send", {"op": await operation(ctx, "roll"), "submission": "x", "to": target,
                                             "mode": "start", "content": [{"type": "text", "text": "roll"}]}, peer=peer)
        except RpcError:
            pass
        if count % 32 == 0:
            epoch = (await ctx.command("inbox.window"))["epoch"]
            if epoch != first:
                return first, epoch, count
    ctx.check(False, "the operation window rolls within 2200 refused operations")


@case("I17")
async def shared_submission_ids_stay_per_principal(ctx):
    """Two principals both choose submission `s`: different deliveries; receipts, lookups and replies never mix; a rolled
    operation window leaves both pending records editable."""
    target = ctx.agents[0]["id"]
    peers = {}
    for name in ("i17-alice", "i17-bob"):
        await ctx.grant(name, name, ["send:admit"], "agent:" + target)
        peers[name] = await ctx.connect(name)
    window = (await ctx.command("inbox.window"))["epoch"]
    submission = window + ":shared"
    sent, ops = {}, {}
    for name, peer in peers.items():
        ops[name] = await operation(ctx, "send")
        sent[name] = await ctx.command("inbox.send", {"op": ops[name], "submission": submission, "to": target, "mode": "admit",
                                                      "content": [{"type": "text", "text": "from " + name}]}, peer=peer)
    alice, bob = sent["i17-alice"]["delivery"], sent["i17-bob"]["delivery"]
    ctx.check(alice != bob, "the same submission from two principals gets two deliveries", alice=alice, bob=bob)
    # Replies: a repeated submission (new op, same intent) answers each principal with its own delivery.
    for name, peer in peers.items():
        again = await ctx.command("inbox.send", {"op": await operation(ctx, "again"), "submission": submission, "to": target,
                                                 "mode": "admit", "content": [{"type": "text", "text": "from " + name}]}, peer=peer)
        ctx.check(again.get("delivery") == sent[name]["delivery"], "a repeated submission replies with that principal's own delivery",
                  principal=name, reply=again)
    # Lookups: each record and each op lookup belongs to its own principal only.
    for name, delivery in (("i17-alice", alice), ("i17-bob", bob)):
        record = await ctx.get(delivery)
        ctx.check(record["from"]["principal"] == name and record["content"] == [{"type": "text", "text": "from " + name}],
                  "a delivery lookup returns that principal's record", principal=name)
        own = await ctx.command("inbox.op", {"op": ops[name]}, peer=peers[name])
        ctx.check(own.get("delivery") == delivery, "an op lookup by its principal returns its own delivery", principal=name, lookup=own)
    crossed = await ctx.command("inbox.op", {"op": ops["i17-bob"]}, peer=peers["i17-alice"])
    ctx.check(crossed.get("delivery") != bob, "another principal's op id never resolves to its delivery", lookup=crossed)
    # A rolled operation window leaves both pending records editable.
    old, new, refused = await roll_window(ctx, peers["i17-alice"], target)
    ctx.check(old == window and new != window, "the operation window rolled", refusals=refused)
    for name, delivery in (("i17-alice", alice), ("i17-bob", bob)):
        record = await ctx.get(delivery)
        edited = await ctx.command("inbox.edit", {"op": await operation(ctx, "edit"), "delivery": delivery, "expect": record["revision"],
                                                  "content": [{"type": "text", "text": "edited by " + name}]}, peer=peers[name])
        ctx.check(edited.get("delivery") == delivery, "a pending record stays editable by its sender after the window rolled",
                  principal=name, reply=edited)
    # Receipts: each native input, outcome and typed receipt carries its own delivery. The agent declares insertion
    # correlation and the native publishes the receipt it stored for each actual insertion.
    from copy import deepcopy
    row = next(r for r in await ctx.resources() if r["id"] == target)
    caps = deepcopy(row["data"]["agent"]["caps"]); caps["agent.send"]["correlation"] = "insertion"; caps["rev"] = caps.get("rev", 1) + 1
    await ctx.native_set(0, caps=caps)
    # Changing caps reattaches coordination's receipt stream for this agent; wait until it is attached again.
    await ctx.wait(lambda: _receipts_attached(ctx), label="coordination receipt stream attached", timeout=20)
    # The runtime is running, so both admits queue: the native's own replies carry {delivery, stage: admitted}
    # (agent-v0 result shape). Each later idle point actually inserts one queued delivery and stores `inserted`.
    await ctx.native_set(0, state="running", run="i17-run")
    await ctx.scope("resume", "agent:" + target)
    rows = []
    for name, delivery in (("i17-alice", alice), ("i17-bob", bob)):
        record = await settled(ctx, delivery)
        calls = invocations(ctx, delivery=delivery)
        ctx.check(len(calls) == 1 and calls[0]["args"]["content"] == [{"type": "text", "text": "edited by " + name}]
                  and record["from"]["principal"] == name, "each delivery's native input stays its own", principal=name, state=record["state"])
    for name, delivery in (("i17-alice", alice), ("i17-bob", bob)):
        await ctx.native.call("point", ref=ctx.agents[0]["backend_ref"], point="idle")
        compacted = "final_seq" in await ctx.get(delivery)
        published = None
        try: published = await ctx.native.call("publish-receipt", ref=ctx.agents[0]["backend_ref"], delivery=delivery)
        except Exception as error: published = {"error": str(error)}  # e.g. a queued (never inserted) delivery
        try:
            record = await ctx.wait_command("inbox.get", lambda r: (r.get("result") or {}).get("receipt"), args={"delivery": delivery})
        except Exception:
            record = await ctx.get(delivery)  # graded below: a missing receipt cannot witness isolation
        ctx.evidence.append({"kind": "i17_receipt", "delivery": delivery, "compacted_before_publish": compacted,
                             "published": published, "receipt": (record.get("result") or {}).get("receipt")})
        rows.append((delivery, record))
    ctx.check(receipts_isolated(rows), "each delivery's typed receipt names exactly its own delivery",
              receipts={delivery: (record.get("result") or {}).get("receipt") for delivery, record in rows})


async def tombstone(sub, agent):
    """The coordination tombstone row for one agent, read from the public operational document."""
    row = (await sub.peer.call("operational.get", {"domain": "coord", "key": "operational/coord/agents/" + digest(agent)[0]})).get("entry")
    return ((row or {}).get("value") or {}).get(agent)


async def membership(sub, agent):
    return (await sub.peer.call("workspace.snapshot"))["entries"].get("doc/agents/members/" + agent)


@case("L10")
async def membership_is_ordinary_configuration(ctx):
    """With the protected domain full, a membership move still succeeds; its author's `u` and an explicit revert undo
    it; a scope action frozen before the undo keeps its recorded set; the tombstone keeps only {rN, revision}."""
    from .archive_cases import saturate_content
    from .launcher import tui
    from .scale_cases import declared_limits, sub_coordination
    limits = await declared_limits(ctx)
    count = (limits["domain_bytes"] + limits["quotas"]["target_bytes"] - 1) // limits["quotas"]["target_bytes"] + 2
    sub = await sub_coordination(ctx, "l10", max(3, count))
    try:
        mover = sub.agents[-1]["id"]
        old = {"project": "l10-old", "reports_to": None}
        await sub.peer.call("workspace.apply", {"ops": [{"op": "set", "key": "doc/agents/members/" + mover, "value": old, "expect": 0}]})
        await sub.send(target=mover, text="tombstone evidence for the mover")
        _, _, refusal = await saturate_content(ctx, sub, sub.agents[:-1], limits)
        try:
            await sub.send(target=sub.agents[0]["id"], text="p", send_new=True)
            full = None
        except RpcError as error:
            full = error.kind
        ctx.check(full in ("quota", "busy"), "the protected domain is full (a new send is refused)", refusal=full, fill=str(refusal)[:200])
        # A membership move is ordinary configuration: it still commits with the protected domain full.
        before = await membership(sub, mover)
        moved = await sub.peer.call("workspace.apply", {"ops": [{"op": "set", "key": "doc/agents/members/" + mover,
                                                                 "value": {"project": "l10-new", "reports_to": None}, "expect": before["version"]}]})
        ctx.check((await membership(sub, mover))["value"]["project"] == "l10-new", "a membership move succeeds with the domain full",
                  txn=moved.get("txn"))
        stone = await tombstone(sub, mover)
        ctx.check(stone is not None and set(stone.get("membership_ref") or {}) == {"rN", "revision"} and stone["membership_ref"]["rN"] == mover,
                  "the tombstone references membership only by {rN, revision}", membership_ref=(stone or {}).get("membership_ref"))
        # A scope action frozen before the undo.
        preview = await sub.command("scope", {"action": "inspect", "scope": "project:l10-new"})
        op = await operation(sub, "scope")
        frozen = await sub.command("scope", {"op": op, "action": "pause", "scope": "project:l10-new", "expect": preview["expect"]})
        ctx.check(mover in frozen.get("targets", []), "the scope action recorded the moved member", targets=frozen.get("targets"))
        # The author's own `u` (real TUI recovery binding) undoes the move.
        async with tui(sub, "l10-owner-tui") as ui:
            mark = len(ui.output); ui.send(b"\x07")
            await ui.text(b"undo", after=mark)
            ui.send(b"u")
            async def restored():
                row = await membership(sub, mover)
                return row and row["value"] == old
            await sub.wait(restored, label="author u restores the old membership", timeout=15)
        ctx.check((await membership(sub, mover))["value"] == old, "the author's u undoes the move with the domain full")
        # A member added to the project after the freeze (and after the author's u, so u undoes the move): a recomputed
        # selection would include it, the frozen one never.
        latecomer = sub.agents[0]["id"]
        late = await membership(sub, latecomer)
        await sub.peer.call("workspace.apply", {"ops": [{"op": "set", "key": "doc/agents/members/" + latecomer,
            "value": {"project": "l10-new", "reports_to": None}, "expect": late["version"] if late else 0}]})
        replay = await sub.command("inbox.op", {"op": op})
        # An explicit revert also undoes a move.
        current = await membership(sub, mover)
        again = await sub.peer.call("workspace.apply", {"ops": [{"op": "set", "key": "doc/agents/members/" + mover,
                                                                 "value": {"project": "l10-newer", "reports_to": None}, "expect": current["version"]}]})
        await sub.peer.call("workspace.revert", {"txn": again["txn"]})
        ctx.check((await membership(sub, mover))["value"] == old, "an explicit revert undoes the move with the domain full", txn=again["txn"])
        replay_after_revert = await sub.command("inbox.op", {"op": op})
        stored = (await sub.peer.call("operational.get", {"domain": "coord", "key": "operational/coord/scope/" + frozen["receipt"]})).get("entry") or {}
        stored_value = stored.get("value") or {}
        ctx.check(stored_value.get("control_revision") == frozen.get("control_revision") and sorted(stored_value.get("targets") or []) == sorted(frozen.get("targets") or []),
                  "the stored frozen selection keeps its control revision and targets after both undos",
                  stored={k: stored_value.get(k) for k in ("targets", "control_revision", "membership_revision")},
                  frozen={k: frozen.get(k) for k in ("targets", "control_revision", "receipt")})
        ctx.check(frozen_set_kept(frozen, replay, replay_after_revert) and latecomer not in (replay.get("targets") or []),
                  "the scope action frozen before the undo keeps exactly its recorded set (after u and after revert)",
                  frozen=frozen.get("targets"), after_undo=replay.get("targets"), after_revert=replay_after_revert.get("targets"),
                  latecomer=latecomer)
        stone = await tombstone(sub, mover)
        version = (await membership(sub, mover))["version"]
        # The reference is refreshed by K2's lifecycle sweep, so its revision may lag the live config; L10 constrains
        # its shape: only {rN, revision}, with no copied membership values anywhere in the tombstone.
        text = json.dumps(stone)
        ctx.check(set(stone["membership_ref"]) == {"rN", "revision"} and stone["membership_ref"]["rN"] == mover
                  and isinstance(stone["membership_ref"]["revision"], int) and not any(project in text for project in ("l10-old", "l10-new", "l10-newer")),
                  "the tombstone holds only {rN, revision} and no membership values", membership_ref=stone["membership_ref"],
                  live_version=version)
    finally:
        try:
            await asyncio.wait_for(sub.close(), 20)
        finally:
            ctx.evidence.extend(sub.evidence)


@case("I22")
async def unknown_predecessor_blocks_visibly(ctx):
    """An unknown predecessor blocks dependent waiting mail; views and bounded get/list show held `ordering_unknown`
    plus the predecessor's delivery id; observation neither resolves nor resends it; an explicit authorized resolution
    lets scheduling re-evaluate the successor's other checks and never replays the predecessor."""
    target, ref = ctx.agents[0]["id"], ctx.agents[0]["backend_ref"]
    operator = LiveViews()  # the coordination operator view (K2's coord-status), from a public subscription
    watcher = await ctx.connect("owner")
    watcher.on("event", operator.on_event)
    await watcher.call("events.subscribe", {"view_content": {"names": ["coord-status"]}})
    await ctx.grant("i22-sender", "i22-sender", ["send:admit"], "agent:" + target)
    sender, other = await ctx.connect("i22-sender"), await ctx.connect("i22-other")
    async def send(text):
        op = await operation(ctx, "send")
        return await ctx.command("inbox.send", {"op": op, "submission": op, "to": target, "mode": "admit", "send_new": True,
                                                "content": [{"type": "text", "text": text}]}, peer=sender)
    await ctx.native.call("plan", ref=ref, op="agent.send", plan={"result": {"outcome": "indeterminate",
                                                                             "error": {"kind": "timeout", "message": "fixture: unknown"}}})
    predecessor = (await send("predecessor"))["delivery"]
    await ctx.scope("resume", "agent:" + target)
    unknown = await ctx.wait_command("inbox.get", lambda r: r.get("state") == "unknown", args={"delivery": predecessor})
    ctx.check(len(invocations(ctx, delivery=predecessor)) == 1, "the predecessor reached native once and is unknown", state=unknown["state"])
    await ctx.native.call("plan", ref=ref, op="agent.send", plan={})
    successor = (await send("successor"))["delivery"]
    held_row = await ctx.wait_command("inbox.get", lambda r: r.get("reason") == "ordering_unknown", args={"delivery": successor})
    listed = next((row for row in (await ctx.command("inbox.list", {"target": target}))["items"] if row["delivery"] == successor), {})
    attention = (await ctx.command("status")).get("attention") or {}
    surfaces = [attention, operator.content.get("coord-status")]
    ctx.check(held_row["state"] == "held" and listed.get("reason") == "ordering_unknown",
              "get and list show the successor held ordering_unknown", get=held_row.get("reason"), listed=listed.get("reason"))
    # Graded last so the remaining clauses are still witnessed when this one fails.
    ctx.evidence.append({"kind": "i22_operator_view", "named": operator_view_names(surfaces, successor, predecessor),
                         "attention": surfaces[0], "coord_status": surfaces[1]})
    names_predecessor = {"get": predecessor in json.dumps(held_row), "list": predecessor in json.dumps(listed),
                         "attention": predecessor in json.dumps(attention)}
    # Observation does not resolve or resend.
    for _ in range(3):
        await ctx.get(successor); await ctx.command("inbox.list", {"target": target}); await ctx.command("status")
    await asyncio.sleep(.3)
    ctx.check((await ctx.get(predecessor))["state"] == "unknown" and len(invocations(ctx, delivery=predecessor)) == 1
              and not invocations(ctx, delivery=successor), "observation neither resolves nor resends nor dispatches")
    try:
        await ctx.command("inbox.resolve", {"op": await operation(ctx, "resolve"), "delivery": predecessor, "as": "not_delivered"}, peer=other)
        unauthorized = None
    except RpcError as error:
        unauthorized = error.kind
    ctx.check(unauthorized is not None and (await ctx.get(predecessor))["state"] == "unknown",
              "an unauthorized resolution is refused and changes nothing", refusal=unauthorized)
    # Authorized resolution while paused: the successor's other eligibility (the pause) is re-evaluated.
    await ctx.scope("pause", "agent:" + target)
    resolved = await ctx.command("inbox.resolve", {"op": await operation(ctx, "resolve"), "delivery": predecessor, "as": "not_delivered"}, peer=sender)
    ctx.check(resolved.get("state") == "resolved", "the sender resolves the unknown predecessor explicitly", reply=resolved)
    paused = await ctx.wait_command("inbox.get", lambda r: r.get("reason") != "ordering_unknown", args={"delivery": successor})
    ctx.check(paused["state"] == "held" and paused.get("reason") == "paused" and not invocations(ctx, delivery=successor),
              "after resolution the successor's other checks apply (held paused, not dispatched)", reason=paused.get("reason"))
    await ctx.scope("resume", "agent:" + target)
    done = await settled(ctx, successor)
    ctx.check(len(invocations(ctx, delivery=successor)) == 1 and len(invocations(ctx, delivery=predecessor)) == 1,
              "the successor is dispatched once and the predecessor is never replayed", state=done["state"])
    ctx.check(names_predecessor["get"] and names_predecessor["list"],
              "get and list name the unknown predecessor's delivery id", names=names_predecessor, get=held_row, listed=listed)
    ctx.check(operator_view_names(surfaces, successor, predecessor),
              "an operator view shows the successor held ordering_unknown with the predecessor's id",
              attention=surfaces[0], coord_status=surfaces[1])


class LiveViews:
    """Latest published content per live view, from a public events subscription (view_content)."""
    def __init__(self):
        self.content, self.events = {}, []
    def on_event(self, event):
        self.events.append(event)
        data = event.get("data") if isinstance(event.get("data"), dict) else event
        if event.get("type") == "view.content" and isinstance(data, dict) and "view" in data:
            self.content[data["view"]] = data.get("content")
    def table(self):
        return ((self.content.get("agents.terminal_status") or {}).get("data")) or {}
    def count(self, view):
        return sum(1 for e in self.events if e.get("type") == "view.content" and ((e.get("data") or e).get("view") == view))


async def start_extension(ctx, name, ready):
    """Enable one of the target's own extensions through ordinary workspace configuration and wait until ready."""
    await ctx.peer.call("workspace.apply", {"ops": [{"op": "set", "key": "extension/" + name,
        "value": {"enabled": True, "argv": ["/usr/bin/python3", str(ctx.root / "ext" / (name + ".py"))]}}]})
    await ctx.wait(ready, label="extension " + name + " ready", timeout=30)
    ctx.track_children()


@case("T13")
async def advisory_terminal_status(ctx):
    """agents.status: OSC-shaped live data, unknown states become unknown, per-terminal sequence ordering (even across
    apps), invalid reports change nothing, count bound returns overloaded without recording, concurrent publication
    never regresses, reports drop at terminal end and on extension restart, stored seq advances silently, the sidebar
    prefers OSC status and no report never implies idle."""
    views = LiveViews()
    monitor = await ctx.connect("owner")
    monitor.on("event", views.on_event)
    await monitor.call("events.subscribe", {"view_content": {"names": ["agents.terminal_status", "sidebar", "sidebar.status"]}})
    async def has_command():
        names = [row.get("name") for row in (await ctx.peer.call("command.list")).get("commands", [])]
        return "agents.status" in names
    await start_extension(ctx, "agents", has_command)
    await start_extension(ctx, "sidebar", lambda: "sidebar" in views.content)
    terminals = [(await ctx.peer.call("resource.spawn", {"argv": ["sh"]}, timeout=30))["resource"]["id"] for _ in range(4)]
    a, b, c, d = terminals
    async def report(terminal, app, state, seq, peer=None):
        args = {"terminal": terminal, "app": app, "seq": seq}
        if state is not None: args["state"] = state
        return await (peer or ctx.peer).call("command.invoke", {"name": "agents.status", "args": args}, timeout=30)
    async def table_has(predicate, label):
        return await ctx.wait(lambda: predicate(views.table()), label=label, timeout=10)
    # No report never implies idle (sidebar), then OSC-shaped live data with state mapping.
    await asyncio.sleep(.3)
    ctx.check(a not in views.table(), "no report publishes no state for a terminal")
    def sidebar_entry(terminal):
        found = []
        def walk(node):
            if isinstance(node, dict):
                if terminal in json.dumps(node, ensure_ascii=False) and "badges" in node: found.append(node)
                for value in node.values(): walk(value)
            elif isinstance(node, list):
                for value in node: walk(value)
        walk(views.content.get("sidebar"))
        return found[-1] if found else None
    unreported = sidebar_entry(a)
    ctx.check(unreported is None or "idle" not in json.dumps(unreported.get("badges")),
              "no report never shows idle in the sidebar", entry=unreported)
    async def fence():
        return {"native": len(ctx.native.records("invoke")), "events": len(views.events),
                "history": (await ctx.peer.call("workspace.history", {"limit": 1}))["txns"][-1]["id"]}
    async def fenced(start, label):
        """Valid and foreign reports are live-only: no core effect or notice, no native call, no workspace transaction."""
        effects = [kind for kind in status_effects(views.events[start["events"]:]) if not kind.startswith("view.content")]
        native = len(ctx.native.records("invoke")) - start["native"]
        history = (await ctx.peer.call("workspace.history", {"limit": 1}))["txns"][-1]["id"]
        ctx.check(not effects and native == 0 and history == start["history"],
                  label + ": no core effect, native call or durable write", effects=effects, native_calls=native,
                  history=[start["history"], history])
    valid_fence = await fence()
    reply = await report(a, "claude", "requires_action", 5)
    await table_has(lambda t: t.get(a, {}).get("seq") == 5, "first report published")
    entry = views.table()[a]
    ctx.check(reply == {"ok": True, "applied": True} and entry["state"] == "requires_action" and entry["status"] == {"state": "blocked", "app": "claude"}
              and entry["app"] == "claude" and isinstance(entry.get("at_ms"), int), "a report publishes OSC-shaped live data", entry=entry)
    await ctx.wait(lambda: (views.content.get("sidebar.status") or {}).get("label") == "1 blocked", label="sidebar falls back to the report", timeout=10)
    ctx.check((views.content.get("sidebar.status") or {}).get("label") == "1 blocked",
              "without OSC status the sidebar falls back to the tier-1 report (1 blocked)", status=views.content.get("sidebar.status"))
    await fenced(valid_fence, "the first valid report")
    # With OSC status the sidebar prefers it: a blocked report for a terminal whose OSC status says working adds no block.
    osc = (await ctx.peer.call("resource.spawn", {"argv": ["agent", "--status-every-ms", "600000", "--status-phase-ms", "50"]}))["resource"]["id"]
    async def osc_working():
        rows = (await ctx.peer.call("resource.list", {"id": osc}))["resources"]
        return rows and (rows[0].get("status") or {}).get("state") == "working"
    await ctx.wait(osc_working, label="terminal OSC status working", timeout=10)
    await report(osc, "claude", "requires_action", 1)
    await table_has(lambda t: t.get(osc, {}).get("state") == "requires_action", "report for the OSC terminal published")
    await asyncio.sleep(.5)
    ctx.check((views.content.get("sidebar.status") or {}).get("label") == "1 blocked",
              "the sidebar prefers the terminal's OSC status over a tier-1 report", status=views.content.get("sidebar.status"))
    for state, terminal in (("no-such-state", b), (None, c)):
        await report(terminal, "app", state, 1)
    await table_has(lambda t: t.get(b, {}).get("state") == "unknown" and t.get(c, {}).get("state") == "unknown", "unknown states")
    ctx.check(views.table()[b]["status"]["state"] == "unknown" and views.table()[c]["status"]["state"] == "unknown",
              "unknown and missing states become unknown")
    # Duplicate and lower sequences are ignored per terminal, even from a different app.
    for seq, app in ((5, "other"), (4, "other"), (5, "claude")):
        stale = await report(a, app, "idle", seq)
        ctx.check(stale.get("applied") is False and stale.get("seq") == 5, "a duplicate or lower seq is ignored", seq=seq, app=app, reply=stale)
    await asyncio.sleep(.2)
    ctx.check(views.table()[a]["app"] == "claude" and views.table()[a]["state"] == "requires_action", "the ignored reports changed nothing")
    # Invalid reports: refused, nothing published, no notice, no native effect; a huge seq cannot poison later reports.
    before_events, before_seq = len(views.events), views.table()[a]["seq"]
    native_before = len(ctx.native.records("invoke"))
    history_before = (await ctx.peer.call("workspace.history", {"limit": 1}))["txns"][-1]["id"]
    def effects_since(start):
        return status_effects(views.events[start:])
    invalid = [("x" * 65, "app", "idle", 9), (a, "", "idle", 9), (a, "a" * 65, "idle", 9), (a, "bad\x07app", "idle", 9),
               (a, "app", "idle", -1), (a, "app", "idle", 2 ** 53), (a, "app", "idle", 10 ** 100), (a, "app", "idle", 1.5)]
    kinds = []
    for args in invalid:
        try:
            await report(*args); kinds.append("applied")
        except RpcError as error:
            kinds.append(error.kind)
    await asyncio.sleep(.3)
    native_after = len(ctx.native.records("invoke"))
    history_after = (await ctx.peer.call("workspace.history", {"limit": 1}))["txns"][-1]["id"]
    ctx.check(all(kind == "invalid" for kind in kinds) and views.table()[a]["seq"] == before_seq and not effects_since(before_events)
              and native_after == native_before and history_after == history_before,
              "invalid reports are refused with no publication, notice, core effect, native call or durable write",
              kinds=kinds, effects=effects_since(before_events), native_calls=native_after - native_before,
              history=[history_before, history_after], other_events=[e.get("type") for e in views.events[before_events:]])
    later = await report(a, "claude", "running", 6)
    await table_has(lambda t: t.get(a, {}).get("seq") == 6, "a valid later report after a huge invalid seq")
    ctx.check(later.get("applied") is True, "a huge invalid seq cannot poison a later valid report")
    # Stored seq advances for unchanged state/app without a publication; published seq lags until another change.
    published = views.count("agents.terminal_status")
    silent = await report(a, "claude", "running", 7)
    await asyncio.sleep(.3)
    ctx.check(silent.get("applied") is True and views.count("agents.terminal_status") == published and views.table()[a]["seq"] == 6,
              "an unchanged report advances the stored seq without a publication", reply=silent)
    stale = await report(a, "claude", "idle", 7)
    ctx.check(stale.get("applied") is False and stale.get("seq") == 7, "the stored seq advanced (7 is now stale)", reply=stale)
    await report(d, "app", "idle", 1)
    await table_has(lambda t: t.get(a, {}).get("seq") == 7, "another terminal's change publishes the lagging seq")
    ctx.check(views.table()[a]["seq"] == 7, "the lagging published seq catches up on another terminal's publication")
    # Concurrent publication never lets an older table overwrite a newer one.
    concurrent_fence = await fence()
    start = len(views.events)
    await asyncio.gather(*(report(b, "app", ("idle", "running")[n % 2], 10 + n) for n in range(40)))
    await asyncio.sleep(.5)
    published = [((e.get("data") or e).get("content") or {}).get("data", {}).get(b, {}).get("seq") for e in views.events[start:]
                 if e.get("type") == "view.content" and (e.get("data") or e).get("view") == "agents.terminal_status"]
    published = [seq for seq in published if seq is not None]
    probe = await report(b, "app", "running", 49)
    # Published seq may lag (unchanged reports do not publish); an older table must never land after a newer one.
    ctx.check(published == sorted(published) and views.table()[b]["state"] == "running" and probe.get("seq") == 49
              and probe.get("applied") is False, "concurrent reports never publish an older table after a newer one",
              published=published, final=views.table()[b], stored=probe)
    await fenced(concurrent_fence, "40 concurrent valid reports")
    # Reporter ownership (spec marks it pending C2; the integrator grades it): another principal's report must be refused
    # not_authorized without applying. Graded last so the remaining clauses are still witnessed.
    other = await ctx.connect("t13-other")
    foreign_fence = await fence()
    before_other = dict(views.table().get(c, {}))
    try: owner_check = await report(c, "intruder", "requires_action", 50, peer=other)
    except RpcError as error: owner_check = {"refused": error.kind}
    await asyncio.sleep(.3)
    await fenced(foreign_fence, "a foreign principal's report")
    # The terminal's launcher-tagged principal may report: the launcher supplies VQRO_PRINCIPAL at spawn (core-protocol
    # generic spawn), and that principal connects with it as its hello identity.
    launched = (await ctx.peer.call("resource.spawn", {"argv": ["sh"], "env": {"VQRO_PRINCIPAL": "t13-launched"}}, timeout=30))["resource"]["id"]
    launcher_peer = await ctx.connect("t13-launched")
    try: launched_reply = await report(launched, "launched-app", "running", 1, peer=launcher_peer)
    except RpcError as error: launched_reply = {"refused": error.kind}
    await table_has(lambda t: t.get(launched, {}).get("app") == "launched-app", "launcher-tagged report published") if launched_reply.get("applied") else None
    ownership = {"reply": owner_check, "before": before_other, "after": views.table().get(c),
                 "launcher_tagged": {"terminal": launched, "principal": "t13-launched", "reply": launched_reply,
                                     "published": views.table().get(launched)}}
    # Reports drop at removal (a live terminal resource removed by its provider, no end) as well as at end.
    await ctx.native.call("add", ref="t13-removed", fields={"kind": "terminal"})
    async def removable():
        rows = [row for row in await ctx.resources() if row["backend_ref"] == "t13-removed" and row.get("available")]
        return rows[0]["id"] if rows else None
    removed = await ctx.wait(removable, label="a provider terminal to remove", timeout=10)
    await report(removed, "app", "idle", 1)
    await table_has(lambda t: removed in t, "report for the provider terminal published")
    await ctx.native.call("remove", ref="t13-removed")
    await table_has(lambda t: removed not in t, "a removed terminal's report is dropped")
    ctx.check(removed not in views.table(), "a report is dropped at terminal removal (no end)", terminal=removed)
    # Reports drop at terminal end.
    await ctx.peer.call("resource.kill", {"resource": d})
    await table_has(lambda t: d not in t, "an ended terminal's report is dropped")
    ctx.check(d not in views.table(), "a report is dropped at terminal end")
    try: ended_reply = await report(d, "app", "idle", 99)
    except RpcError as error: ended_reply = {"refused": error.kind}
    ctx.check(ended_reply.get("refused") == "not_found", "an ended terminal cannot report", reply=ended_reply)
    # Count bound: more than 1024 reporting terminals returns overloaded without recording anything.
    reporting = len(views.table())
    extra = [(await ctx.peer.call("resource.spawn", {"argv": ["sh"]}, timeout=30))["resource"]["id"] for _ in range(1025 - reporting)]
    for start in range(0, len(extra) - 1, 64):
        await asyncio.gather(*(report(t, "bulk", "idle", 1) for t in extra[start:min(start + 64, len(extra) - 1)]))
    try: over = await report(extra[-1], "bulk", "idle", 1)
    except RpcError as error: over = {"refused": error.kind}
    await asyncio.sleep(.5)
    ctx.check(over.get("refused") == "overloaded" and extra[-1] not in views.table() and len(views.table()) == 1024,
              "a 1025th reporting terminal is refused overloaded and nothing is recorded", reply=over, published=len(views.table()))
    # Structural proof that the 960 KiB table bound is unreachable with 1024 legal entries: publish a worst-case legal
    # report (app of 64 four-byte characters, maximum seq, longest state), measure its actual published entry, widen the
    # terminal id to its 64-character maximum, and multiply by the 1024-entry count bound.
    widest = "\U0001F600" * 64
    await report(a, widest, "requires_action", 2 ** 53 - 1)
    await table_has(lambda t: t.get(a, {}).get("app") == widest, "worst-case entry published")
    entry = json.dumps({a: views.table()[a]}, ensure_ascii=False, separators=(",", ":")).encode()
    worst = len(entry) + (64 - len(a.encode()))
    ctx.check(1024 * worst <= 960 * 1024, "the 960 KiB bound is unreachable: 1024 worst-case legal entries fit under it",
              worst_entry_bytes=worst, table_bytes=1024 * worst, bound=960 * 1024)
    # Reports are lost on extension restart.
    await ctx.peer.call("command.invoke", {"name": "vqro.extension.restart", "args": {"name": "agents"}})
    # The old registration can still be listed while the extension restarts; a refused report records nothing, so
    # retry not_found/stale_generation (bounded, not dispatched) until the restarted extension answers.
    async def restarted():
        try: return await report(a, "claude", "idle", 1)
        except RpcError as error:
            if error.kind in ("not_found", "stale_generation"): return None  # refused before dispatch
            raise
    again = await ctx.wait(restarted, label="agents.status answered by the restarted extension", timeout=30)
    ctx.check(again.get("applied") is True, "reports are lost on extension restart (seq 1 applies again)", reply=again)
    ctx.check(ownership["launcher_tagged"]["reply"].get("applied") is True
              and (ownership["launcher_tagged"]["published"] or {}).get("app") == "launched-app",
              "the terminal's launcher-tagged principal may report", **ownership["launcher_tagged"])
    ctx.check(ownership["reply"].get("refused") == "not_authorized" and ownership["after"] == ownership["before"],
              "another principal's report is refused not_authorized without applying", **ownership)
