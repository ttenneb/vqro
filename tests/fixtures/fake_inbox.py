"""Fake coordination inbox for plugin tests: the pull surface of `coordination-v0.md` §7 in the dialect of the
default coordination extension (K2, branch coord-ext): `coord.inbox.*` commands through the real core, op ids
prefixed with the window epoch, records whose state is their evidence stage, and a counts-only wake on the
`coord-status` live view. Not the coordination extension: no grants, gates, lanes, budgets or persistence. Every
call is appended to the log given as argv[1] (the test oracle). `tests/fixtures/coord_k2.py` runs K2 itself.

`take {accept: [...], max_bytes}` (as K2: default `["text"]` and 32 KiB) names the block types a plugin can insert and
the largest content it takes (compact UTF-8 JSON bytes); other records stay held `unsupported_content` or
`too_large_for_consumer`. `max_bytes` is ignored while `fixture.fault {no_max_bytes}` is set (an older inbox); `{window_fail}` makes
`coord.inbox.window` fail; `{op_fail: n}` fails the next n `coord.inbox.op` calls;
`{reconcile_page: n}` settles at most n claims per `coord.inbox.reconcile` page; `{reconcile_extra: n}` adds n
pages that settle nothing. A lost
take reply is inspected with `coord.inbox.op {op}`: claims, never content. `agents.status` records tier-1 status reports (K3's command, not in this inbox).
"""
import json, os, sys, threading, time

sys.path.insert(0, os.environ.get("VQRO_EXT_PATH", os.path.join(os.path.dirname(__file__), "..", "..", "extensions")))
from vqro_ext import Extension, VqroError  # noqa: E402

ELIGIBLE = {"turn": {"boundary"}, "settle": {"boundary", "settle"}, "idle": {"start", "boundary", "settle"}}
LEVEL = {"claimed": 0, "supplied": 1, "inserted": 1, "read": 2, "handled": 2}
BLOCKS = {"text": ("text",), "image": ("mime", "data"), "audio": ("mime", "data"), "resource_link": ("uri",), "resource": ("uri",)}
WINDOW = {"epoch": "w:fake", "from": 0, "to": 1024}
LOG = sys.argv[1] if len(sys.argv) > 1 else os.devnull
ext = Extension("coord-fake")
lock = threading.RLock()
records, ops, consumers = {}, {}, {}
FAULT = {}
seq = [0, 0]  # admission order, wake revision


def log(event, **kw):
    with open(LOG, "a") as f:
        f.write(json.dumps({"t": time.time(), "event": event, **kw}) + "\n")


def wake(agent):
    # Every wake carries a new revision: the core drops a view.publish whose content is unchanged.
    seq[1] += 1
    pending = {a: sum(1 for r in records.values() if r["to"] == a and r["state"] == "held")
               for a in {r["to"] for r in records.values()} | {agent}}
    try:
        ext.vqro.call("view.publish", {"view": "coord-status", "content": {"wake": {"pending": pending, "seq": seq[1]}}})
    except VqroError as e:
        log("wake_failed", error=str(e))


def valid_content(content):
    """agent.send content is always an array of typed blocks (coordination decision 2026-11-19)."""
    if not isinstance(content, list) or not content:
        raise VqroError("content must be a non-empty array of typed blocks", "invalid")
    for b in content:
        if not isinstance(b, dict) or b.get("type") not in BLOCKS or any(k not in b for k in BLOCKS[b["type"]]):
            raise VqroError(f"invalid content block {b!r:.80}", "invalid")
    return content


def mutation(fn):
    """op ids are caller-chosen, window-prefixed; an identical repeat returns the original result."""
    def run(args):
        with lock:
            op = args.get("op")
            if op in ops:  # as K2: a cached op is answered before its window is checked
                return ops[op]
            if not isinstance(op, str) or not op.startswith(WINDOW["epoch"] + ":"):
                raise VqroError("operation is outside its window", "expired_operation")
            result = dict(fn(args), window=dict(WINDOW))
            # A repeated take returns its claims (history), never content again.
            ops[op] = result if "records" not in result else dict(result, records=[], historical=True)
            if FAULT.get("take_delay_s") and "records" in result and result["records"]:
                delay = FAULT.pop("take_delay_s")
                log("fault_delay", seconds=delay)
                time.sleep(delay)  # the caller's reply deadline passes: a lost take reply
                if FAULT.pop("rotate_window", False):  # and the window moves on meanwhile
                    WINDOW["epoch"] = "w:fake" + str(time.time_ns())
                    log("window_rotated", epoch=WINDOW["epoch"])
            return result
    return run


def command(name, title):
    return ext.command(name, title)


@command("coord.inbox.window", "window")
def window(args):
    if FAULT.get("window_fail"):
        raise VqroError("test: window unavailable", "unavailable")
    return dict(WINDOW)


@command("coord.inbox.op", "inspect an op: a take returns its claims, never content")
def op(args):
    with lock:
        if FAULT.get("op_fail", 0) > 0:  # the inspection itself is lost
            FAULT["op_fail"] -= 1
            log("op_failed", op=args.get("op"))
            raise VqroError("test: inspection unavailable", "unavailable")
        saved = ops.get(args.get("op"))
        log("op", op=args.get("op"), found=saved is not None)
        if saved is None:
            return {"state": "unknown", "window": dict(WINDOW)}
        if "claims" in saved:
            live = [c for c in saved["claims"] if records[c["delivery"]].get("claim") == c["claim"]]
            return {"state": saved["state"], "claims": live, "records": [], "historical": True, "window": dict(WINDOW)}
        return dict(saved, historical=True)


@command("coord.inbox.send", "test: admit a message")
@mutation
def send(args):
    valid_content(args.get("content"))
    seq[0] += 1
    d = f"d:{seq[0]}"
    records[d] = {"delivery": d, "to": args["to"], "from": {"principal": args.get("from", "p:owner")},
                  "origin": {"principal": args.get("from", "p:owner"), "kind": "tool", "actor": "client:test"}, "mode": args["mode"],
                  "content": args["content"], "render": args.get("render"), "state": "held", "reason": None, "rev": 1, "order": seq[0]}
    log("sent", delivery=d, mode=args["mode"])
    wake(args["to"])
    return {"delivery": d, "state": "held"}


@command("coord.inbox.edit", "test: edit while held")
@mutation
def edit(args):
    valid_content(args.get("content"))
    r = records[args["delivery"]]
    if r["state"] != "held":
        raise VqroError(f"too_late: {r['state']}", "conflict")
    r.update(content=args["content"], rev=r["rev"] + 1, reason=None)
    log("edited", delivery=r["delivery"])
    wake(r["to"])
    return {"state": "held", "rev": r["rev"]}


@command("coord.inbox.withdraw", "test: withdraw while held")
@mutation
def withdraw(args):
    r = records[args["delivery"]]
    if r["state"] != "held":
        raise VqroError(f"too_late: {r['state']}", "conflict")
    r.update(state="withdrawn", reason="sender_withdrawn")
    log("withdrawn", delivery=r["delivery"])
    return {"state": "withdrawn"}


@command("coord.inbox.get", "one record")
def get(args):
    with lock:
        return records.get(args["delivery"]) or {"state": "unknown"}


@command("coord.inbox.take", "pull: claim eligible records at a safe point")
@mutation
def take(args):
    agent, at = args["agent"], args["at"]
    if at not in ELIGIBLE:
        raise VqroError("take must name a declared safe point", "invalid")
    if consumers.get(agent) != args.get("consumer"):
        raise VqroError("consumer must reconcile before taking", "conflict")
    if any(r["to"] == agent and r["state"] == "claimed" for r in records.values()):
        return {"state": "held", "records": [], "held": "claim_in_flight"}
    out, held = [], "no_eligible_input"
    for r in sorted(records.values(), key=lambda r: r["order"]):
        if len(out) >= min(int(args.get("limit", 16)), 16):
            break
        if r["to"] == agent and r["state"] == "held" and r["mode"] in ELIGIBLE[at]:
            accepts = args.get("accept", ["text"])  # as K2: block types the plugin inserts without loss
            if any(b["type"] not in accepts for b in r["content"]):
                r["reason"] = held = "unsupported_content"
                continue
            size = len(json.dumps(r["content"], ensure_ascii=False, separators=(",", ":")).encode())
            if not FAULT.get("no_max_bytes") and size > args.get("max_bytes", 32768):
                r["reason"] = held = "too_large_for_consumer"
                continue
            claim = f"claim:{r['delivery']}.{len(r.get('attempts', [])) + 1}"
            r.update(state="claimed", reason=None, claim=claim, consumer=args["consumer"], at=at)
            r.setdefault("attempts", []).append({"claim": claim, "at": at, "t": time.time()})
            out.append({"delivery": r["delivery"], "from": r["from"], "origin": r["origin"], "render": r["render"],
                        "mode": r["mode"], "content": r["content"], "claim": claim})
    log("take", agent=agent, at=at, deliveries=[o["delivery"] for o in out])
    if out:
        wake(agent)
    return {"state": "claimed" if out else "held", "records": out, "claims": [{"delivery": o["delivery"], "claim": o["claim"]} for o in out],
            "held": None if out else held}


@command("coord.inbox.report", "pull: report evidence for a claim")
@mutation
def report(args):
    r = records.get(args.get("delivery"))
    if r is None or r.get("claim") != args.get("claim") or consumers.get(r["to"]) != r.get("consumer"):
        raise VqroError("stale claim or consumer", "conflict")
    stage = args["stage"]
    if stage == "released":
        if r["state"] != "claimed":
            raise VqroError("cannot release already supplied input", "conflict")
        r.update(state="held", reason="consumer_released", claim=None)
        wake(r["to"])
    else:
        if stage not in LEVEL or LEVEL[stage] < LEVEL.get(r["state"], 99):
            raise VqroError("report must advance the original claim", "conflict")
        if stage in ("inserted", "supplied") and r["state"] not in ("claimed", stage):
            raise VqroError("report cannot change evidence branches", "conflict")
        r.update(state=stage, reason=None)
    log("report", delivery=r["delivery"], stage=stage)
    return {"state": r["state"], "delivery": r["delivery"]}


@command("coord.inbox.reconcile", "pull: new consumer reconciles open claims from runtime evidence")
@mutation
def reconcile(args):
    agent, seen = args["agent"], {x["delivery"]: x.get("entry") for x in args.get("records", [])}
    consumers[agent] = args["consumer"]
    if FAULT.get("reconcile_extra", 0) > 0:  # a page that settled nothing yet (still reconciling)
        FAULT["reconcile_extra"] -= 1
        return {"state": "reconciling", "records": [], "remaining": 1}
    changed, page = [], FAULT.get("reconcile_page", 1 << 30)  # as K2: bounded pages, `reconciling` until done
    candidates = [r for r in records.values() if r["to"] == agent and r.get("consumer") != args["consumer"]
                  and (r["state"] == "claimed" or r["state"] == "unknown" and seen.get(r["delivery"]))]
    if len(candidates) > page:
        for r in candidates[:page]:
            evidenced = bool(seen.get(r["delivery"]))
            r.update(state="inserted" if evidenced else "unknown", reason=None if evidenced else "consumer_restarted",
                     evidence={"entry": seen.get(r["delivery"])} if evidenced else None, consumer=args["consumer"] + ":settled")
            changed.append({"delivery": r["delivery"], "state": r["state"]})
        log("reconcile", agent=agent, consumer=args["consumer"], records=changed, remaining=len(candidates) - page)
        return {"state": "reconciling", "records": changed, "remaining": len(candidates) - page}
    for r in records.values():
        if r["to"] == agent and r["state"] in ("claimed", "unknown") and r.get("consumer"):
            evidenced = bool(seen.get(r["delivery"]))
            r.update(state="inserted" if evidenced else "unknown", reason=None if evidenced else "consumer_restarted",
                     evidence={"entry": seen.get(r["delivery"])} if evidenced else None)
            changed.append({"delivery": r["delivery"], "state": r["state"]})
    log("reconcile", agent=agent, consumer=args["consumer"], records=changed)
    wake(agent)
    return {"state": "reconciled", "records": changed}


@command("coord.inbox.resolve", "a principal's judgement on an unknown record")
@mutation
def resolve(args):
    r = records[args["delivery"]]
    if r["state"] != "unknown":
        raise VqroError("only unknown records are resolved", "conflict")
    r.update(state="resolved", resolution=args["as"])
    return {"state": "resolved", "delivery": r["delivery"]}


@command("fixture.fault", "test: inject one fault (take_delay_s: hold the next non-empty take reply)")
def fault(args):
    FAULT.update(args)
    return {"fault": FAULT}


@command("agents.status", "tier-1 status report (K3 shape: terminal, app, state, seq)")
def terminal_status(args):
    log("status", terminal=args.get("terminal"), app=args.get("app"), state=args.get("state"), seq=args.get("seq"))
    return {"ok": True, "applied": True}


log("ready")
ext.run()
