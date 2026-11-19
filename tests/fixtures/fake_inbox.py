"""Fake coordination inbox for adapter tests (A3): just the pull surface of `coordination-v0.md` §7, served as
commands through the real core (`command.invoke`). Not the coordination extension (K2): no grants, lanes,
budgets or persistence. Every call is appended to --log for the test oracle.

Commands: inbox.send / edit / get / list (test helpers), inbox.take, inbox.report, inbox.reconcile, inbox.op,
agents.terminal_status (records tier-1 status). Wake: `view.publish inbox-wake-<agent> {pending}` (counts only).
"""
import json, os, sys, threading, time

sys.path.insert(0, os.environ.get("VQRO_EXT_PATH", os.path.join(os.path.dirname(__file__), "..", "..", "extensions")))
from vqro_ext import Extension, VqroError  # noqa: E402

ELIGIBLE = {"turn": {"boundary"}, "settle": {"boundary", "settle"}, "idle": {"start", "boundary", "settle"}}
ORDER = {"supplied": 1, "inserted": 2, "handled": 2, "read": 3}
LOG = sys.argv[1] if len(sys.argv) > 1 else os.devnull
ext = Extension("inbox")
lock = threading.RLock()
records, ops, consumers, statuses = {}, {}, {}, []
seq = [0, 0]  # admission order, wake revision


def log(event, **kw):
    with open(LOG, "a") as f:
        f.write(json.dumps({"t": time.time(), "event": event, **kw}) + "\n")


def wake(agent):
    # The core drops a view.publish whose content is unchanged, so every wake carries a change counter: an edit
    # of a held record (same pending count) must still wake the plugin.
    pending = sum(1 for r in records.values() if r["to"] == agent and r["state"] == "held")
    seq[1] += 1
    try:
        ext.vqro.call("view.publish", {"view": f"inbox-wake-{agent}", "content": {"pending": pending, "rev": seq[1], "items": []}})
    except VqroError as e:
        log("wake_failed", error=str(e))


def once(args, fn):
    op = args.get("op")
    if op is not None and op in ops:
        return ops[op]
    result = fn()
    if op is not None:
        ops[op] = result
    return result


BLOCKS = {"text": ("text",), "image": ("mime", "data"), "audio": ("mime", "data"), "resource_link": ("uri",), "resource": ("uri",)}


def valid_content(content):
    """agent.send content is always an array of typed blocks (coordination decision 2026-11-19)."""
    if not isinstance(content, list) or not content:
        raise VqroError("content must be a non-empty array of typed blocks", "invalid")
    for b in content:
        if not isinstance(b, dict) or b.get("type") not in BLOCKS or any(k not in b for k in BLOCKS[b["type"]]):
            raise VqroError(f"invalid content block {b!r:.80}", "invalid")
        if b["type"] == "resource" and not ("text" in b or "data" in b):
            raise VqroError("resource needs text or data", "invalid")
    return content


@ext.command("inbox.send", "test: admit a message")
def send(args):
    valid_content(args.get("content"))
    with lock:
        seq[0] += 1
        d = f"d{seq[0]}"
        records[d] = {"delivery": d, "to": args["to"], "from": args.get("from", "p:owner"), "mode": args["mode"],
                      "content": args["content"], "state": "held", "rev": 1, "order": seq[0]}
        log("sent", delivery=d, mode=args["mode"])
        wake(args["to"])
        return {"delivery": d, "state": "held"}


@ext.command("inbox.edit", "test: edit while held")
def edit(args):
    with lock:
        valid_content(args.get("content"))
        r = records[args["delivery"]]
        if r["state"] != "held":
            raise VqroError(f"too_late: {r['state']}", "conflict")
        r.update(content=args["content"], rev=r["rev"] + 1, reason=None)
        log("edited", delivery=r["delivery"])
        wake(r["to"])  # coordination-v0 §7: wake on admission, edit, resume and handover
        return {"state": "held", "rev": r["rev"]}


@ext.command("inbox.get", "test: one record")
def get(args):
    with lock:
        return records.get(args["delivery"])


@ext.command("inbox.list", "test: records for a target")
def lst(args):
    with lock:
        return {"records": [r for r in records.values() if r["to"] == args["target"]]}


@ext.command("inbox.take", "pull: claim eligible records at a safe point")
def take(args):
    with lock:
        def run():
            agent, at = args["agent"], args["at"]
            if consumers.get(agent) != args["consumer"]:
                raise VqroError("stale consumer", "conflict")
            out = []
            for r in sorted(records.values(), key=lambda r: r["order"]):
                if len(out) >= min(int(args.get("limit", 16)), 16):
                    break
                if r["to"] == agent and r["state"] == "held" and r["mode"] in ELIGIBLE[at]:
                    accepts = args.get("content")  # block types the plugin can insert without flattening
                    if accepts is not None and any(b["type"] not in accepts for b in r["content"]):
                        r["reason"] = "unsupported_content"
                        continue
                    claim = f"c{r['delivery']}.{len(r.get('attempts', [])) + 1}"
                    r.update(state="claimed", claim=claim, consumer=args["consumer"], at=at, stage=None)
                    r.setdefault("attempts", []).append({"claim": claim, "at": at, "t": time.time()})
                    out.append({"delivery": r["delivery"], "from": r["from"], "origin": None, "mode": r["mode"],
                                "content": r["content"], "claim": claim})
            log("take", agent=agent, at=at, deliveries=[o["delivery"] for o in out])
            if out:
                wake(agent)
            return {"records": out}
        return once(args, run)


@ext.command("inbox.report", "pull: report evidence for a claim")
def report(args):
    with lock:
        def run():
            r = records.get(args["delivery"])
            if r is None or r.get("claim") != args["claim"] or r["state"] not in ("claimed", "delivered"):
                raise VqroError("stale claim", "stale")
            stage = args["stage"]
            if stage == "released":
                r.update(state="held", claim=None, stage=None)
                wake(r["to"])
            else:
                if ORDER.get(stage, 0) <= ORDER.get(r.get("stage"), 0):
                    raise VqroError(f"stage {stage} does not move forward from {r.get('stage')}", "invalid")
                r.update(state="delivered", stage=stage)
            log("report", delivery=r["delivery"], stage=stage)
            return {"state": r["state"], "stage": r.get("stage")}
        return once(args, run)


@ext.command("inbox.reconcile", "pull: new consumer reconciles open claims from runtime evidence")
def reconcile(args):
    with lock:
        def run():
            agent, seen = args["agent"], {x["delivery"]: x.get("entry") for x in args.get("records", [])}
            consumers[agent] = args["consumer"]
            inserted, unknown = [], []
            for r in records.values():
                if r["to"] != agent or r["state"] not in ("claimed", "delivered"):
                    continue
                if r["state"] == "delivered" and r.get("stage") in ("inserted", "read", "handled"):
                    continue
                if r["delivery"] in seen:
                    r.update(state="delivered", stage="inserted", evidence={"entry": seen[r["delivery"]]})
                    inserted.append(r["delivery"])
                else:
                    r.update(state="unknown")  # never resent
                    unknown.append(r["delivery"])
            log("reconcile", agent=agent, consumer=args["consumer"], inserted=inserted, unknown=unknown)
            wake(agent)
            return {"inserted": inserted, "unknown": unknown}
        return once(args, run)


@ext.command("inbox.op", "lookup a call result by op")
def op(args):
    with lock:
        return ops.get(args["op"], {"state": "unknown"})


@ext.command("agents.terminal_status", "tier-1 status report (placeholder name)")
def terminal_status(args):
    with lock:
        statuses.append(args)
        log("status", **{k: args.get(k) for k in ("agent", "state", "source")})
        return {"ok": True}


log("ready")
ext.run()
