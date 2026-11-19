#!/usr/bin/env python3
"""vqro extension: retention. Decides when finished resources are forgotten (`resource.forget`).

Backends keep finished terminals (herdr-core until `terminal.rm`), and the core keeps whatever its
providers report, so without a policy every exit stays forever. This is that policy; the core and
providers only supply the mechanism, and only for providers declaring `resource.forget`.

- A finished resource shown in a layout stays while any layout shows it.
- Once it was shown and no layout shows it any more (its last view closed, `tabs.close_ended`), it
  is forgotten after a GRACE period. Forgetting is irreversible but closing a tab is undoable (D24):
  `u` within the grace shows the terminal again, which cancels the pending forget.
- Finished resources never shown in a layout (background jobs, exited before being placed) keep the
  newest KEEP for the sidebar's "ended" items; older ones are forgotten.

Backend execution is never touched (ending it is `resource.kill`). Disconnected agents' cached facts
are retired after60 seconds continuously unavailable, using `resource.forget_cached`, never native forget.
Reconnection cancels that grace; unavailable live terminals are not expired. An agent whose bridge
(host) terminal has finished, or was removed after this run saw it, cannot reconnect: it is retired at
once. A host merely not reported yet (e.g. right after a core restart) keeps the grace. The pending
retirement times are published as live data `retention.agents` ({data: {id: {forget_at}}}, wall-clock
seconds) so agent UIs can say when a disconnected agent will go; nothing else depends on it. Orphan bridge-marked panels
are pruned only when no layout references them. `retention.forget_ended` remains finished-only.
Turn this extension off to disable the policy; core mechanical admission limits still apply.
"""
import os
import sys
import threading
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from vqro_ext import Extension, VqroError, views_in  # noqa: E402
from vqro_views import view_resource  # noqa: E402

KEEP = 64        # never-shown finished resources kept for the sidebar's ended items
SETTLE = 0.3     # seconds: batch bursts of events into one pass
GRACE = 60.0     # seconds an unshown, once-shown resource waits, so undo can still bring it back
if "--grace" in sys.argv:  # tests use a short grace
    GRACE = float(sys.argv[sys.argv.index("--grace") + 1])

ext = Extension("retention", event_types=["entries", "resource", "resource.removed", "provider"])
dirty = threading.Event()
was_shown = set()   # resource ids seen in a layout during this run
unshown_at = {}     # once-shown id -> monotonic time it was first seen unshown
unavailable_at = {}  # disconnected agent -> continuous unavailable grace
forget_wall = {}  # same grace as a wall-clock deadline, fixed when it starts (for display only)
hosts_seen = set()  # host terminal ids observed during this run (removal after that is evidence)
published = [None]  # last `retention.agents` data, to publish only changes
RETENTION = "retention.agents"
policy_lock = threading.RLock()
refused = set()     # ids a provider refused for good (unsupported/invalid); never retried


def number(rid):
    try:
        return int(rid.lstrip("r"))
    except ValueError:
        return 0


def plan(keep=KEEP, grace=None, include_unavailable=True, cached_due=None):
    """(ids to forget now, seconds until the next expiry); capture each candidate's scope too."""
    with policy_lock:
        return _plan(keep, grace, include_unavailable, cached_due)


def _plan(keep, grace, include_unavailable, cached_due):
    grace = GRACE if grace is None else grace
    now = time.monotonic()
    with ext.cache_lock:
        shown = set()
        for key, entry in ext.entries.items():
            if key.startswith("layout/") and isinstance(entry.get("value"), dict):
                for view in views_in(entry["value"]):
                    rid = view_resource(ext, view)
                    if rid:
                        shown.add(rid)
        resources = dict(ext.resources)
        providers = dict(ext.providers)
    was_shown.intersection_update(resources)
    was_shown.update(shown & set(resources))
    for rid in list(unshown_at):
        if rid in shown or rid not in was_shown:
            del unshown_at[rid]  # shown again (e.g. undo) or gone: no pending forget
    for rid in was_shown - shown:
        unshown_at.setdefault(rid, now)

    agents = {rid: r for rid, r in resources.items() if r.get("kind") == "agent"}
    hosts_seen.update(r["host"] for r in agents.values() if r.get("host") in resources)
    unavailable = {rid for rid, r in agents.items() if not r.get("available", False)}
    for rid in list(unavailable_at):
        if rid not in unavailable:
            del unavailable_at[rid]
            forget_wall.pop(rid, None)
    for rid in unavailable:
        if rid not in unavailable_at:
            unavailable_at[rid] = now
            forget_wall[rid] = round(time.time() + grace)
    dead = {rid for rid in unavailable if host_dead(agents[rid].get("host"), resources)}
    expired = [rid for rid in unavailable if rid in dead or now - unavailable_at[rid] >= grace] if include_unavailable else []
    unavailable_waits = [unavailable_at[rid] + grace - now for rid in unavailable if rid not in expired] if include_unavailable else []
    if include_unavailable:
        pending = {rid: {"forget_at": forget_wall[rid]} for rid in unavailable if rid not in expired}
        hosts_seen.intersection_update(set(resources) | {r.get("host") for r in agents.values()})

    def forgettable(r):
        caps = (providers.get(r.get("provider")) or {}).get("capabilities") or {}
        return r.get("state") == "finished" and r.get("available") and "resource.forget" in caps and r["id"] not in refused

    ended = [rid for rid, r in resources.items() if forgettable(r) and rid not in shown]
    closed = [rid for rid in ended if rid in was_shown]
    due = [rid for rid in closed if now - unshown_at[rid] >= grace]
    waits = unavailable_waits + [unshown_at[rid] + grace - now for rid in closed if rid not in due]
    never = sorted((rid for rid in ended if rid not in was_shown), key=number, reverse=True)
    if cached_due is not None:
        cached_due.update(expired)
    if include_unavailable:
        publish_pending(pending)
    return expired + due + never[keep:], (min(waits) if waits else None)


def host_dead(host, resources):
    """The bridge terminal has finished, or was removed after we saw it: the bridge can't come back.

    Absent without having been seen (inventory not synced yet after a restart) is not evidence.
    """
    if not host:
        return False
    r = resources.get(host)
    if r is None:
        return host in hosts_seen
    return r.get("kind") == "terminal" and r.get("state") == "finished"


def publish_pending(pending):
    """Derived live data only (never history); rounded so an unchanged grace republishes nothing."""
    if pending == published[0]:
        return
    try:
        ext.publish(RETENTION, {"data": pending})
        published[0] = pending
    except VqroError as error:
        print(f"retention: publish pending: {error.kind}: {error}", file=sys.stderr, flush=True)


def forget(rid, cached=False):
    try:
        # Never reinterpret a stale cached candidate as a native effect after a reconnect race.
        ext.vqro.call("resource.forget_cached" if cached else "resource.forget", {"resource": rid})
    except VqroError as error:
        if error.kind in ("unsupported", "invalid"):
            refused.add(rid)  # retried only after a new fact about it (see `changed`)
        elif error.kind != "not_found":
            print(f"retention: forget {rid}: {error.kind}: {error}", file=sys.stderr, flush=True)


def prune_bridge_views():
    """Only orphan bridge declarations, never user panels or any layout-referenced view."""
    def attempt():
        with ext.cache_lock:
            shown = {view for key, entry in ext.entries.items() if key.startswith("layout/")
                     for view in views_in(entry.get("value", {}))}
            ops = []
            for key, entry in ext.entries.items():
                rid = next((key[len(prefix):] for prefix in ("view/agent-status-", "view/agent-") if key.startswith(prefix)), "")
                value = entry.get("value", {})
                if not isinstance(value, dict):
                    continue
                bridge = value.get("bridge")
                if (rid.startswith("r") and rid[1:].isascii() and rid[1:].isdigit()
                        and rid not in ext.resources and key[5:] not in shown
                        and value.get("kind") == "panel" and isinstance(bridge, dict)
                        and bridge.get("ref") == "agent" and isinstance(bridge.get("instance"), str)):
                    ops.append({"op": "delete", "key": key, "expect": ext.version(key)})
        if ops:
            ext.apply(ops, "retention: orphan bridge views")
    try:
        ext.retrying(attempt)
    except VqroError as error:
        if error.kind != "invalid":
            raise
        # A newly added layout reference can invalidate deletion after our scan. The
        # transaction did not apply; defer cleanup until the next cache update.


def worker():
    wait = None
    while True:
        dirty.wait(timeout=wait)
        time.sleep(SETTLE)
        dirty.clear()
        if ext.degraded:
            # Forgetting is irreversible: never on a cache known to miss events (slow core).
            # Recovery reruns listeners with `missed`, which sets `dirty` again.
            wait = 5.0
            continue
        cached_due = set()
        ids, wait = plan(cached_due=cached_due)
        for rid in ids:
            forget(rid, cached=rid in cached_due)
        try:
            prune_bridge_views()
        except VqroError as error:
            print(f"retention: view cleanup: {error.kind}: {error}", file=sys.stderr, flush=True)
        if wait is not None:
            wait = max(wait, SETTLE)


@ext.command("retention.forget_ended", "Forget every finished terminal no layout shows, now (live ones are never touched)")
def forget_ended(args):
    ext.fresh()
    ids, _ = plan(keep=0, grace=0, include_unavailable=False)  # explicit finished-only request
    for rid in ids:
        forget(rid)
    return {"forgotten": len(ids)}


@ext.on_event
def changed(kind, data):
    if kind == "resource":
        refused.discard(data.get("id"))
    dirty.set()


threading.Thread(target=worker, daemon=True, name="retention").start()
dirty.set()
ext.run()
