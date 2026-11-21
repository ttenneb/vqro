"""Part A C11 (optional view facts) and C12 (generic causality wire). Public wire only.

C12 grades the core's generic `resource.invoke` causality pin (agent-v0 §2.5 "Generic wire pin"): explicit
`cause`/`correlation` in root params, in `args`, or both (matching) reach effect audits unchanged; conflicting or
malformed links are refused `invalid` before dispatch, with no native write and no effect audit. Bounds come from the
spec text (4 KiB encoded cause, 256-byte known strings, 64 containers, 256-byte correlation), exercised exactly at and
one past each bound. The graded component is the core: an adapter may definitely decline (after dispatch) a cause
its own contract does not accept, e.g. non-`asserted` provenance or a smaller size bound; such declines are recorded in
evidence, never counted as a core pass for the adapter's own receipt echo (C9 grades that).
"""
import json

from .cases import case, caps
from .results import CheckFailed, HarnessGap, check

KNOWN = ("kind", "id", "provenance", "endpoint", "epoch", "boot")


def encoded(value):
    return len(json.dumps(value, ensure_ascii=False, separators=(",", ":")).encode())


def padded(cause, size):
    """The same cause plus an extra bounded field, encoded to exactly `size` bytes."""
    base = dict(cause, pad="")
    out = dict(base, pad="p" * (size - encoded(base)))
    if encoded(out) != size:
        raise HarnessGap("C12 could not build an exact-size cause")
    return out


def nested(depth):
    """A value of exactly `depth` containers (arrays)."""
    value = "leaf"
    for _ in range(depth):
        value = [value]
    return value


def effect_events(ctx):
    return [row for row in ctx.events if row.get("type") == "effect"]


async def idle(ctx):
    await ctx.native.call("turn", state="settled")
    await ctx.wait_facts(lambda f: f.get("state") == "idle")


def base_args(ctx, mode, text):
    # Same origin construction as ctx.send: this connection's actual core hello fields, nothing inferred.
    origin = {"kind": "human", **getattr(ctx.peer, "core_origin", {})}
    return {"mode": mode, "delivery": ctx.delivery(), "content": [{"type": "text", "text": text}],
            "origin": origin, "render": ["user", "participant", "attributed_user"]}


async def final_audit(ctx, effect):
    def found():
        return next((row for row in ctx.events if row.get("type") == "effect" and row.get("data", {}).get("effect") == effect
                     and row["data"].get("outcome") is not None), None)
    try:
        return (await ctx.peer.wait(found))["data"]
    except Exception as error:
        raise CheckFailed("final effect audit was not delivered", component="core") from error


async def accepted_form(ctx, mode, label, *, root=None, args=None, adapter_may_decline=False):
    """The core must dispatch this form and audit its links unchanged. A non-`asserted` provenance is a caller claim an
    adapter may definitely decline after dispatch (its own policy); the core's audit is still graded."""
    await idle(ctx)
    call = base_args(ctx, mode, "C12-" + label + ":" + ctx.delivery())
    call.update(args or {})
    sent_args = json.loads(json.dumps(call))
    result = await ctx.invoke("agent.send", call, **(root or {}))
    error_data = (result.get("error") or {}).get("data") or {}
    dispatched = bool(error_data.get("effect")) or not result.get("rpc_error")
    check(dispatched, f"C12 {label}: valid causality form was refused before dispatch: {result}", component="core")
    declined = result.get("outcome") == "rejected" and error_data.get("kind") in ("unsupported", "invalid")
    check(result.get("outcome") == "accepted" or (adapter_may_decline and declined),
          f"C12 {label}: valid causality form was not accepted: {result}", component="adapter")
    effect_id = result.get("effect") or error_data.get("effect")
    audit = await final_audit(ctx, effect_id)
    expected = {}
    for field in ("cause", "correlation"):
        value = (root or {}).get(field, (args or {}).get(field))
        if value is not None:
            expected[field] = value
    for field in ("cause", "correlation"):
        check(audit.get(field) == expected.get(field),
              f"C12 {label}: effect audit {field} changed or invented: sent {expected.get(field)!r}, audit {audit.get(field)!r}",
              component="core")
    check(call == sent_args, f"C12 {label}: caller args were mutated", component="harness")
    ctx.evidence.append({"kind": "c12_accepted", "form": label, "effect": effect_id, "outcome": result.get("outcome"),
                         "adapter_declined": result.get("outcome") != "accepted" and (result.get("error") or {}).get("message"),
                         "audit": {field: audit.get(field) for field in ("cause", "correlation")},
                         "receipt_links": {field: (result.get("data", {}).get("receipt") or {}).get(field) for field in ("cause", "correlation")}})


async def refused_form(ctx, mode, label, *, root=None, args=None):
    call = base_args(ctx, mode, "C12-refuse-" + label)
    call.update(args or {})
    mark, audits = ctx.native.mark(), len(effect_events(ctx))
    result = await ctx.invoke("agent.send", call, **(root or {}))
    error = result.get("error") or {}
    kind = (error.get("data") or {}).get("kind", error.get("kind"))
    check(result.get("outcome") != "accepted" and kind == "invalid",
          f"C12 {label}: malformed causality was not refused invalid before dispatch: {result}", component="core")
    check(not ctx.native.writes(mark), f"C12 {label}: refused causality reached the native runtime", component="core")
    new = [row for row in effect_events(ctx)[audits:] if row.get("data", {}).get("op") == "agent.send"]
    check(not new, f"C12 {label}: refused causality still produced an effect audit", component="core")
    ctx.evidence.append({"kind": "c12_refused", "form": label, "error": error.get("message"), "native_writes": 0})


@case("C12")
async def generic_causality_wire(ctx):
    send = await caps(ctx, "agent.send")
    mode = next((name for name in ("start", "admit") if name in send.get("modes", {})), None)
    if mode is None:
        raise HarnessGap("C12 needs a declared start/admit mode for its carrier operation")
    facts = await ctx.facts()
    boot = (await ctx.peer.call("workspace.snapshot"))["epoch"]
    endpoint, epoch = ctx.agents[0]["id"], facts["epoch"]
    effect = {"kind": "effect", "id": "c12-effect", "boot": boot, "provenance": "asserted"}
    entity = {"kind": "entity", "id": "c12-entity", "endpoint": endpoint, "epoch": epoch, "provenance": "native"}
    future = {"kind": "c12-future-kind", "id": "c12-x", "provenance": "c12-future-provenance",
              "extra": {"bounded": [1, 2, {"three": None}]}}
    corr = "C12-thread"

    # Accepted forms: root-only, args-only, matching dual; unknown enums/extra fields; every bound exactly at its limit.
    await accepted_form(ctx, mode, "root-only", root={"cause": effect, "correlation": corr})
    await accepted_form(ctx, mode, "args-only", args={"cause": dict(entity, provenance="asserted"), "correlation": corr})
    await accepted_form(ctx, mode, "args-only-native-provenance", args={"cause": entity}, adapter_may_decline=True)
    await accepted_form(ctx, mode, "dual-matching", root={"cause": future, "correlation": corr},
                        args={"cause": future, "correlation": corr}, adapter_may_decline=True)
    limits = dict(future, id="i" * 256, provenance="v" * 256, kind="k" * 256)
    await accepted_form(ctx, mode, "strings-256", root={"cause": limits, "correlation": "c" * 256}, adapter_may_decline=True)
    await accepted_form(ctx, mode, "cause-4096", args={"cause": padded(effect, 4096)}, adapter_may_decline=True)
    await accepted_form(ctx, mode, "depth-64", root={"cause": dict(effect, extra=nested(63))}, adapter_may_decline=True)

    # Refused forms: conflicts, types, missing qualifiers, one past every bound, in root and args positions.
    refusals = [
        ("conflicting-cause", {"cause": effect}, {"cause": dict(effect, id="other")}),
        ("conflicting-correlation", {"correlation": corr}, {"correlation": corr + "-other"}),
        ("cause-not-object", {"cause": "c12"}, None),
        ("missing-kind", {"cause": {"id": "x", "provenance": "asserted"}}, None),
        ("kind-not-string", {"cause": dict(future, kind=5)}, None),
        ("missing-provenance", None, {"cause": {"kind": "delivery", "id": "x"}}),
        ("entity-without-endpoint", {"cause": {k: v for k, v in entity.items() if k != "endpoint"}}, None),
        ("request-without-epoch", {"cause": {"kind": "request", "id": "q1", "endpoint": endpoint, "provenance": "asserted"}}, None),
        ("effect-without-boot", None, {"cause": {k: v for k, v in effect.items() if k != "boot"}}),
        ("qualifier-not-string", {"cause": dict(effect, boot=7)}, None),
        ("string-257", {"cause": dict(future, id="i" * 257)}, None),
        ("control-character", {"cause": dict(future, id="a\u0007b")}, None),
        ("cause-4097", None, {"cause": padded(effect, 4097)}),
        ("depth-65", {"cause": dict(future, extra=nested(64))}, None),
        ("correlation-empty", {"correlation": ""}, None),
        ("correlation-257", None, {"correlation": "c" * 257}),
        ("correlation-not-string", {"correlation": 5}, None),
    ]
    await idle(ctx)
    for label, root, args in refusals:
        await refused_form(ctx, mode, label, root=root, args=args)
    declined = [row["form"] for row in ctx.evidence if row.get("kind") == "c12_accepted" and row.get("adapter_declined")]
    # Adapter-specific stricter limits are its own declared/agent-v0 contract, not the core's generic wire; recorded.
    ctx.evidence.append({"kind": "c12_adapter_declined_forms", "forms": declined})
    ctx.evidence.append({"kind": "c12_bounds", "cause_bytes": 4096, "string_bytes": 256, "containers": 64,
                         "correlation_bytes": 256, "accepted_forms": 7, "refused_forms": len(refusals)})
