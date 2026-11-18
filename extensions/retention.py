#!/usr/bin/env python3
"""prim extension: retention. Decides when finished resources are forgotten (`resource.forget`).

Backends keep finished terminals (herdr-core until `terminal.rm`), and the core keeps whatever its
providers report, so without a policy every exit stays forever. This is that policy; the core and
providers only supply the mechanism, and only for providers declaring `resource.forget`.

- A finished resource shown in a layout stays while any layout shows it.
- Once it was shown and no layout shows it any more (its last view closed, `tabs.close_ended`), it
  is forgotten.
- Finished resources never shown in a layout (background jobs, exited before being placed) keep the
  newest KEEP for the sidebar's "ended" items; older ones are forgotten.

Live resources are never touched (that is `resource.kill`). `retention.forget_ended` forgets every
unshown finished resource at once. Turn this extension off to keep every finished resource, as before.
"""
import os
import sys
import threading
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from prim_ext import Extension, PrimError, views_in  # noqa: E402
from prim_views import view_resource  # noqa: E402

KEEP = 64        # never-shown finished resources kept for the sidebar's ended items
SETTLE = 0.3     # seconds: batch bursts of events into one pass

ext = Extension("retention", event_types=["entries", "resource", "resource.removed", "provider"])
dirty = threading.Event()
was_shown = set()   # resource ids seen in a layout during this run
refused = set()     # ids a provider refused for good (unsupported/invalid); never retried


def number(rid):
    try:
        return int(rid.lstrip("r"))
    except ValueError:
        return 0


def plan(keep=KEEP):
    """Ids to forget now. Pure decision over the extension's caches."""
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

    def forgettable(r):
        caps = (providers.get(r.get("provider")) or {}).get("capabilities") or {}
        return r.get("state") == "finished" and r.get("available") and "resource.forget" in caps and r["id"] not in refused

    ended = [rid for rid, r in resources.items() if forgettable(r) and rid not in shown]
    closed = [rid for rid in ended if rid in was_shown]
    never = sorted((rid for rid in ended if rid not in was_shown), key=number, reverse=True)
    return closed + never[keep:]


def forget(rid):
    try:
        ext.prim.call("resource.forget", {"resource": rid})
    except PrimError as error:
        if error.kind in ("unsupported", "invalid"):
            refused.add(rid)  # retried only after a new fact about it (see `changed`)
        elif error.kind != "not_found":
            print(f"retention: forget {rid}: {error.kind}: {error}", file=sys.stderr, flush=True)


def worker():
    while True:
        dirty.wait()
        time.sleep(SETTLE)
        dirty.clear()
        for rid in plan():
            forget(rid)


@ext.command("retention.forget_ended", "Forget every finished terminal no layout shows, now (live ones are never touched)")
def forget_ended(args):
    ext.fresh()
    ids = plan(keep=0)
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
