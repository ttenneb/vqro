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


# ---- C11: optional view facts -------------------------------------------------------------------------------------
# Native usage reports are fixture input in each wire's own native shape; the expected standard facts follow the
# agent-v0 §2.3 mapping (context.window/used, tokens input/output/cache_read/cache_write). Nothing is derived here:
# a section the native report lacks must stay absent in the facts.

def native_report(wire, *, window=None, used=None, tokens=None, cost=None):
    """(native report, standard usage expected from it) for one wire. `tokens` = (input, output, cache_read, cache_write)."""
    expected = {}
    if wire == "pi":
        report = {}
        if window is not None:
            report["contextUsage"] = {"contextWindow": window, "tokens": used}
            expected["context"] = {"window": window, **({"used": used} if used is not None else {})}
        if tokens is not None:
            report["tokens"] = {"input": tokens[0], "output": tokens[1], "cacheRead": tokens[2], "cacheWrite": tokens[3],
                                "total": sum(tokens)}
            expected["tokens"] = {"input": tokens[0], "output": tokens[1], "cache_read": tokens[2], "cache_write": tokens[3]}
        if cost is not None:
            report["cost"] = cost  # Pi's aggregate is a bare number: no currency/basis to preserve
        return report, expected
    if wire == "codex":
        # App-server reports the model window but no occupancy: `used` must stay absent (window-only runtime).
        report = {}
        if window is not None:
            report["modelContextWindow"] = window
            expected["context"] = {"window": window}
        if tokens is not None:
            total = {"inputTokens": tokens[0], "outputTokens": tokens[1], "cachedInputTokens": tokens[2],
                     "reasoningOutputTokens": 0, "totalTokens": sum(tokens)}
            report["total"] = total
            report["last"] = dict(total)  # last-turn totals: never context occupancy
            expected["tokens"] = {"input": tokens[0], "output": tokens[1], "cache_read": tokens[2]}
        return report, expected
    raise HarnessGap("C11 needs a native usage-report fixture for wire " + str(wire))


async def wait_usage(ctx, expected, label):
    try:
        return await ctx.wait_facts(usage_matches(expected), timeout=5)
    except CheckFailed:
        current = await ctx.facts()
        ctx.evidence.append({"kind": "c11_usage_mismatch", "form": label, "expected": expected,
                             "facts_usage": current.get("usage"), "incomplete": current.get("incomplete")})
        raise CheckFailed(f"C11 {label}: facts usage {current.get('usage')!r} never matched the native report {expected!r}")


async def report_usage(ctx, wire, report):
    await ctx.native.call("usage", report=report)
    if wire == "pi":
        # The Pi wire observes stats after a native settle; the settle itself carries no usage.
        await ctx.native.call("turn", state="settled")


def usage_matches(expected):
    def match(facts):
        usage = facts.get("usage")
        return isinstance(usage, dict) and {key: usage.get(key) for key in ("context", "tokens")} == {
            key: expected.get(key) for key in ("context", "tokens")}
    return match


def check_cost(usage, label):
    cost = (usage or {}).get("cost")
    if cost is None:
        return
    check(isinstance(cost.get("currency"), str) and cost.get("basis") in ("reported", "estimated")
          and isinstance(cost.get("amount"), (int, float)),
          f"C11 {label}: published cost lacks its native currency/basis: {cost}")


async def observe_native_state(ctx):
    facts = await ctx.facts()
    options = [row for row in facts.get("config", []) if row.get("type") == "select" and row.get("choices")]
    if "agent.configure" not in facts.get("caps", {}) or not options:
        raise HarnessGap("C11 native-cwd observation needs a declared select configuration option to confirm state")
    option = options[0]
    alternate = next((row["value"] for row in option["choices"] if row["value"] != option.get("value")), None)
    if alternate is None:
        raise HarnessGap("C11 configuration option has no alternate value")
    result = await ctx.invoke("agent.configure", {"id": option["id"], "value": alternate, "rev": option["rev"]})
    check(result.get("outcome") in ("observed", "accepted", "dispatched"), f"C11: declared configuration change failed: {result}")
    ctx.evidence.append({"kind": "c11_state_observation", "option": option["id"], "value": alternate, "outcome": result.get("outcome")})


@case("C11")
async def optional_view_facts(ctx):
    wire = ctx.manifest.get("wire")
    if wire not in ("pi", "codex"):
        raise HarnessGap("C11 native usage fixture exists for the pi and codex wires only")
    facts = await ctx.ensure_session()
    native = await ctx.native.call("state")
    if "cwd" not in native or "model" not in native:
        raise HarnessGap("C11 fixture state lacks its native cwd/model oracle")

    # Session facts follow the runtime's own reports, never the bridge/viewer process or a guessed model.
    if wire == "pi":
        reported_cwd = "/c11-native-reported-cwd"
        await ctx.native.call("native_cwd", cwd=reported_cwd)
        # A declared configuration change is confirmed by re-reading native state; that read carries the cwd.
        await observe_native_state(ctx)
    else:
        reported_cwd = native["cwd"]
    try:
        facts = await ctx.wait_facts(lambda f: (f.get("session") or {}).get("cwd") == reported_cwd, timeout=5)
    except CheckFailed:
        current = await ctx.facts()
        ctx.evidence.append({"kind": "c11_session_mismatch", "reported_cwd": reported_cwd, "facts_session": current.get("session")})
        raise CheckFailed(f"C11: session.cwd {(current.get('session') or {}).get('cwd')!r} is not the native-reported {reported_cwd!r}")
    native = await ctx.native.call("state")  # model/effort truth after any configuration change
    facts = await ctx.wait_facts(lambda f: ((f.get("session") or {}).get("model") or {}).get("id") == native["model"], timeout=5)
    model = (facts.get("session") or {}).get("model") or {}
    check(model.get("id") == native["model"], f"C11: session.model.id {model.get('id')!r} is not the native model {native['model']!r}")
    if native.get("effort") is None:
        check("effort" not in model, f"C11: session.model.effort {model.get('effort')!r} was guessed; native reported none")
    else:
        check(model.get("effort") == native["effort"], f"C11: effort {model.get('effort')!r} != native {native['effort']!r}")
    ctx.evidence.append({"kind": "c11_session", "native": {"cwd": reported_cwd, "model": native.get("model"), "effort": native.get("effort")},
                         "facts": {"cwd": facts["session"].get("cwd"), "model": model}})

    # 1. A full native report: window, occupancy (where the runtime reports it), token and cache totals, bare cost.
    report, expected = native_report(wire, window=200000, used=4321, tokens=(1100, 220, 33, 4), cost=0.75)
    await report_usage(ctx, wire, report)
    facts = await wait_usage(ctx, expected, "full")
    check_cost(facts["usage"], "full")
    if isinstance(report.get("cost"), (int, float)):
        # The native aggregate is a bare number: no currency or reported/estimated basis to preserve.
        check("cost" not in facts["usage"], "C11: a bare native cost number became a priced standard cost (currency/basis invented)")
    ctx.evidence.append({"kind": "c11_usage", "form": "full", "native": report, "facts": facts["usage"],
                         "incomplete": facts.get("incomplete")})

    # 2. Window only: occupancy stays absent; last-turn totals never become context.used.
    report, expected = native_report(wire, window=128000, tokens=(500, 60, 0, 0))
    await report_usage(ctx, wire, report)
    facts = await wait_usage(ctx, expected, "window_only")
    context = facts["usage"].get("context") or {}
    check("used" not in context, f"C11: window-only report published occupancy {context.get('used')!r}")
    ctx.evidence.append({"kind": "c11_usage", "form": "window_only", "native": report, "facts": facts["usage"]})

    # 3. Sections the runtime stops reporting stay unknown, never zero.
    report, expected = native_report(wire, window=96000)
    await report_usage(ctx, wire, report)
    facts = await wait_usage(ctx, expected, "tokens_absent")
    check("tokens" not in facts["usage"], f"C11: absent native token totals published as {facts['usage'].get('tokens')!r}")
    check_cost(facts["usage"], "window")
    ctx.evidence.append({"kind": "c11_usage", "form": "tokens_absent", "native": report, "facts": facts["usage"]})

    # 4. Run and thought entities carry usage only when natively reported; this runtime reported none for them.
    # One ordinary turn (input, a native model call and output, settle) gives the adapter run/message entities to
    # publish; the fixture reports usage only at the session level, so none of them may carry usage.
    await idle(ctx)
    result = await ctx.send("C11 run observation")
    check(result.get("outcome") == "accepted", f"C11: run observation send not accepted: {result}")
    await ctx.native.call("turn", state="start")
    await ctx.native.call("model_call")
    await ctx.native.call("retain", count=1, text="C11 assistant output")
    await ctx.native.call("turn", state="settled")
    await ctx.wait_facts(lambda f: f.get("state") == "idle")
    view = await ctx.attach()
    await ctx.wait_view(view, lambda reducer: reducer.snapshots > 0)
    carried = [(key, row["value"].get("usage")) for key, row in view.reducer.entities.items()
               if key[0] in ("run", "message") and "usage" in row["value"]]
    check(not carried, f"C11: run/thought usage was apportioned without a native report: {carried[:2]}")
    ctx.evidence.append({"kind": "c11_entities", "runs": sum(1 for key in view.reducer.entities if key[0] == "run"),
                         "messages": sum(1 for key in view.reducer.entities if key[0] == "message"), "usage_carried": 0})
