"""Part A C11 (optional view facts) and C12 (generic causality wire). Public wire only.

C12 grades the core's generic `resource.invoke` causality pin (agent-v0 §2.5 "Generic wire pin") at the adapter
boundary: a test-owned provider registered over the public provider wire receives the core's dispatched effect, so
root-only, args-only and matching dual `cause`/`correlation` are checked for exact dispatched args AND unchanged
effect audits; conflicting or malformed links must be refused `invalid` with no dispatch and no audit. The same forms
then run against the implementation's own adapter endpoint: audits unchanged, refusals never reach the native
runtime, and any definite post-dispatch adapter decline is recorded for its contract owner (it is neither a core
failure nor a permission for the adapter to refuse inside the generic bound). Bounds come from the spec text
(4 KiB encoded cause, 256-byte known strings, 64 containers, 256-byte correlation), at and one past each bound.

C11 binds usage/context/session facts to supplied native reports. Coverage limits, stated rather than hidden: these
fixtures supply no priced cost (currency/basis/plan), so only its absence is graded; neither adapter emits run or
thought entities here, so that clause is only a no-apportionment negative on the entities that do exist.
"""
import asyncio
import json

from .cases import audit_fence, case, caps
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


class WitnessProvider:
    """A test-owned provider registered over the PUBLIC provider wire: the core dispatches agent.send effects to it,
    so the exact args the core hands an adapter are observed at that boundary (agent-v0 §2.5 "args unchanged").
    No core hook, no production test effect; owned and closed by the handler."""
    NAME = "c12-dispatch-witness"

    def __init__(self, ctx):
        self.ctx, self.received, self.pending, self.serial = ctx, [], {}, 0
        self.reader = self.writer = self.task = None

    async def start(self):
        self.reader, self.writer = await asyncio.open_unix_connection(str(self.ctx.backend.env["VQRO_SOCKET"]), limit=1 << 20)
        self.task = asyncio.create_task(self._read())
        await self.call("hello", {"client": self.NAME})
        send = {"modes": {"start": {"busy": "reject"}}, "origin": [{"as": "attributed_user", "role": "user", "frame": "sender_header"}]}
        registration = await self.call("provider.register", {
            "name": self.NAME, "role": "backend", "instance": "c12", "software": {"name": self.NAME, "version": "0"},
            "provider_version": "0", "capabilities": {"agent": {"v": 0, "modules": {}}, "agent.send": send}})
        native = {"v": 0, "state": "idle", "lifecycle": "live", "session": {"id": "s1"}, "epoch": "e1",
                  "caps": {"rev": 1, "agent.send": send}, "incomplete": []}
        await self.call("provider.sync", {"generation": registration["generation"], "final": True, "resources": [
            {"ref": "c12", "incarnation": "i1", "kind": "agent", "state": "live", "status": {"state": "idle"},
             "data": {"agent": native}}]})
        for _ in range(200):
            rows = (await self.ctx.peer.call("resource.list", {"provider": self.NAME}))["resources"]
            row = next((r for r in rows if r.get("available") and r.get("kind") == "agent"), None)
            if row:
                self.rid = row["id"]
                return self
            await asyncio.sleep(.02)
        raise HarnessGap("C12 dispatch witness provider never became an available agent resource")

    async def call(self, method, params):
        self.serial += 1
        future = asyncio.get_running_loop().create_future()
        self.pending[self.serial] = future
        await self._send({"jsonrpc": "2.0", "id": self.serial, "method": method, "params": params})
        return await asyncio.wait_for(future, 5)

    async def _send(self, row):
        self.writer.write(json.dumps(row, separators=(",", ":")).encode() + b"\n")
        await self.writer.drain()

    async def _read(self):
        while True:
            line = await self.reader.readline()
            if not line:
                return
            row = json.loads(line)
            if "method" in row and "id" in row:
                # A core-dispatched effect: record the exact params, then a definite native-shaped acknowledgement.
                params = row.get("params") or {}
                if len(self.received) < 64:
                    self.received.append(json.loads(json.dumps(params)))
                args = params.get("args") or {}
                await self._send({"jsonrpc": "2.0", "id": row["id"], "result": {
                    "outcome": "accepted", "data": {"receipt": {"delivery": args.get("delivery"), "stage": "admitted"}}}})
            elif "id" in row:
                future = self.pending.pop(row["id"], None)
                if future is not None and not future.done():
                    if "error" in row:
                        future.set_exception(HarnessGap("C12 witness provider call refused: " + json.dumps(row["error"])[:300]))
                    else:
                        future.set_result(row.get("result"))

    async def close(self):
        if self.writer is not None:
            self.writer.close()
        if self.task is not None:
            self.task.cancel()
            await asyncio.gather(self.task, return_exceptions=True)


async def witnessed_form(ctx, witness, label, *, root=None, args=None):
    """Core-graded: dispatched once, args reach the adapter boundary exactly as sent, and the audit carries the links."""
    call = base_args(ctx, "start", "C12-" + label + ":" + ctx.delivery())
    call.update(args or {})
    facts = {"v": 0, "epoch": "e1", "caps_rev": 1}
    sent = {**facts, **json.loads(json.dumps(call))}
    before = len(witness.received)
    result = await ctx.invoke("agent.send", dict(sent), rid=witness.rid, defaults=False, **(root or {}))
    check(result.get("outcome") == "accepted", f"C12 {label}: valid causality form was not dispatched: {result}", component="core")
    received = witness.received[before:]
    check(len(received) == 1 and received[0].get("op") == "agent.send",
          f"C12 {label}: the core did not dispatch exactly one agent.send effect", component="core")
    check(received[0].get("args") == sent,
          f"C12 {label}: dispatched args differ from the caller's args: sent {sent!r}, adapter received {received[0].get('args')!r}",
          component="core")
    audit = (await audit_fence(ctx, result))["data"]
    for field in ("cause", "correlation"):
        expected = (root or {}).get(field, (args or {}).get(field))
        check(audit.get(field) == expected,
              f"C12 {label}: effect audit {field} changed or invented: sent {expected!r}, audit {audit.get(field)!r}", component="core")
    ctx.evidence.append({"kind": "c12_witnessed", "form": label, "effect": result.get("effect"),
                         "dispatched_args_equal": True, "audit": {field: audit.get(field) for field in ("cause", "correlation")},
                         "dispatch_links": {field: received[0].get(field) for field in ("cause", "correlation")}})


async def witnessed_refusal(ctx, witness, label, *, root=None, args=None):
    call = {"v": 0, "epoch": "e1", "caps_rev": 1, **base_args(ctx, "start", "C12-refuse-" + label), **(args or {})}
    before, audits = len(witness.received), len(effect_events(ctx))
    result = await ctx.invoke("agent.send", call, rid=witness.rid, defaults=False, **(root or {}))
    error = result.get("error") or {}
    kind = (error.get("data") or {}).get("kind", error.get("kind"))
    check(result.get("outcome") != "accepted" and kind == "invalid",
          f"C12 {label}: malformed causality was not refused invalid before dispatch: {result}", component="core")
    check(len(witness.received) == before, f"C12 {label}: refused causality was dispatched to the adapter boundary", component="core")
    new = [row for row in effect_events(ctx)[audits:] if row.get("data", {}).get("op") == "agent.send"]
    check(not new, f"C12 {label}: refused causality still produced an effect audit", component="core")
    ctx.evidence.append({"kind": "c12_witnessed_refusal", "form": label, "error": error.get("message")})


async def accepted_form(ctx, mode, label, *, root=None, args=None, adapter_may_decline=False):
    """The core must dispatch this form and audit its links unchanged. A non-`asserted` provenance is a caller claim an
    adapter may definitely decline after dispatch (its own policy); the core's audit is still graded."""
    await idle(ctx)
    call = base_args(ctx, mode, "C12-" + label + ":" + ctx.delivery())
    call.update(args or {})
    result = await ctx.invoke("agent.send", call, **(root or {}))
    error_data = (result.get("error") or {}).get("data") or {}
    dispatched = bool(error_data.get("effect")) or not result.get("rpc_error")
    check(dispatched, f"C12 {label}: valid causality form was refused before dispatch: {result}", component="core")
    declined = result.get("outcome") == "rejected" and error_data.get("kind") in ("unsupported", "invalid")
    check(result.get("outcome") == "accepted" or (adapter_may_decline and declined),
          f"C12 {label}: valid causality form was not accepted: {result}", component="adapter")
    effect_id = result.get("effect") or error_data.get("effect")
    audit = (await audit_fence(ctx, {"effect": effect_id}))["data"]
    expected = {}
    for field in ("cause", "correlation"):
        value = (root or {}).get(field, (args or {}).get(field))
        if value is not None:
            expected[field] = value
    for field in ("cause", "correlation"):
        check(audit.get(field) == expected.get(field),
              f"C12 {label}: effect audit {field} changed or invented: sent {expected.get(field)!r}, audit {audit.get(field)!r}",
              component="core")
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

    limits = dict(future, id="i" * 256, provenance="v" * 256, kind="k" * 256)
    accepted = [
        ("root-only", {"cause": effect, "correlation": corr}, None, False),
        ("args-only", None, {"cause": dict(entity, provenance="asserted"), "correlation": corr}, False),
        ("args-only-native-provenance", None, {"cause": entity}, True),
        ("dual-matching", {"cause": future, "correlation": corr}, {"cause": future, "correlation": corr}, True),
        ("strings-256", {"cause": limits, "correlation": "c" * 256}, None, True),
        ("cause-4096", None, {"cause": padded(effect, 4096)}, True),
        ("depth-64", {"cause": dict(effect, extra=nested(63))}, None, True),
    ]
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

    # 1. CORE (graded): a public test-owned provider observes the exact dispatched args at the adapter boundary.
    factory = getattr(ctx, "dispatch_witness_factory", None)  # unit tests substitute an in-process witness
    witness = await (factory() if factory else WitnessProvider(ctx)).start()
    try:
        for label, root, args, _ in accepted:
            await witnessed_form(ctx, witness, label, root=root, args=args)
        for label, root, args in refusals:
            await witnessed_refusal(ctx, witness, label, root=root, args=args)
    finally:
        await witness.close()

    # 2. The implementation's own adapter endpoint: the same forms reach dispatch with audits unchanged, refusals
    # never reach the native runtime. A definite post-dispatch decline is the adapter's own contract and is recorded
    # for its owner (it is neither a core failure nor evidence that the adapter may refuse within the generic bound).
    for label, root, args, may_decline in accepted:
        await accepted_form(ctx, mode, label, root=root, args=args, adapter_may_decline=may_decline)
    await idle(ctx)
    for label, root, args in refusals:
        await refused_form(ctx, mode, label, root=root, args=args)
    declined = [row["form"] for row in ctx.evidence if row.get("kind") == "c12_accepted" and row.get("adapter_declined")]
    ctx.evidence.append({"kind": "c12_adapter_declined_forms", "forms": declined,
                         "note": "adapter-side refusal within the generic bound; routed to the contract/adapter owner"})
    ctx.evidence.append({"kind": "c12_bounds", "cause_bytes": 4096, "string_bytes": 256, "containers": 64,
                         "correlation_bytes": 256, "accepted_forms": len(accepted), "refused_forms": len(refusals)})


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
            # Fields as decoded by src/codex.rs:644-669 and docs/research/codex-adapter.md:50 (no other native fields).
            report["total"] = {"inputTokens": tokens[0], "outputTokens": tokens[1], "cachedInputTokens": tokens[2],
                               "totalTokens": tokens[0] + tokens[1]}
            report["last"] = {"totalTokens": tokens[0] + tokens[1]}  # last-turn total: never context occupancy
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


def no_priced_cost(usage, report, label):
    """None of these native reports supplies a currency, a reported/estimated basis or a plan, so the facts may carry
    no `usage.cost` at all, in this report and in every later transition (never a local price or a stale carry-over).
    A positive priced-cost report is not available from these fixtures (an explicit coverage limit)."""
    priced = any(key in json.dumps(report) for key in ("currency", "basis", "plan"))
    if priced:
        raise HarnessGap("C11 fixture unexpectedly supplied priced-cost facts; positive preservation is not graded here")
    check("cost" not in (usage or {}), f"C11 {label}: usage.cost {(usage or {}).get('cost')!r} published without native priced facts")


async def observe_native_state(ctx):
    facts = await ctx.facts()
    options = [row for row in facts.get("config", []) if row.get("type") == "select" and row.get("choices")]
    if "agent.configure" not in facts.get("caps", {}) or not options:
        raise HarnessGap("C11 native-cwd observation needs a declared select configuration option to confirm state")
    # Prefer the advertised model-category option: the model then changes only through configuration, never a facts
    # write, and session.model must follow the runtime's report of it.
    option = next((row for row in options if row.get("category") == "model"), options[0])
    alternate = next((row["value"] for row in option["choices"] if row["value"] != option.get("value")), None)
    if alternate is None:
        raise HarnessGap("C11 configuration option has no alternate value")
    before = await ctx.native.call("state")
    mark = ctx.native.mark()
    result = await ctx.invoke("agent.configure", {"id": option["id"], "value": alternate, "rev": option["rev"]})
    check(result.get("outcome") in ("observed", "accepted", "dispatched"), f"C11: declared configuration change failed: {result}")
    ctx.evidence.append({"kind": "c11_state_observation", "option": option["id"], "category": option.get("category"),
                         "value": alternate, "outcome": result.get("outcome")})
    return option, before, mark


async def model_route_witness(ctx, observed=None):
    """Model changes go through the advertised model-category config option: one native mutation recorded by the
    fixture, then session.model follows the runtime's own report. Absent such an option the route is undeclared and
    this clause is recorded as unexercised (no facts-write substitute is attempted)."""
    if observed is None:
        facts = await ctx.facts()
        if "agent.configure" not in facts.get("caps", {}) or not any(
                row.get("category") == "model" for row in facts.get("config", [])):
            ctx.evidence.append({"kind": "c11_model_route", "declared": False,
                                 "note": "no advertised model-category config option; clause unexercised, not passed"})
            return
        observed = await observe_native_state(ctx)
    option, before, mark = observed
    if option.get("category") != "model":
        ctx.evidence.append({"kind": "c11_model_route", "declared": False, "observation_option": option["id"],
                             "note": "no advertised model-category config option; clause unexercised, not passed"})
        return
    writes = ctx.native.writes(mark)
    check(len(writes) >= 1, "C11: the model-category configuration change reached no native mutation")
    after = await ctx.native.call("state")
    check(after.get("model") != before.get("model"), f"C11: the native model did not change ({before.get('model')!r})")
    facts = await ctx.wait_facts(lambda f: ((f.get("session") or {}).get("model") or {}).get("id") == after["model"], timeout=5)
    ctx.evidence.append({"kind": "c11_model_route", "declared": True, "option": option["id"],
                         "native_before": before.get("model"), "native_after": after["model"],
                         "native_writes": [row.get("command") for row in writes], "facts_model": facts["session"]["model"]})


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
        observed = await observe_native_state(ctx)
    else:
        reported_cwd, observed = native["cwd"], None
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
    await model_route_witness(ctx, observed)

    # 1. A full native report: window, occupancy (where the runtime reports it), token and cache totals, bare cost.
    report, expected = native_report(wire, window=200000, used=4321, tokens=(1100, 220, 33, 4), cost=0.75)
    await report_usage(ctx, wire, report)
    facts = await wait_usage(ctx, expected, "full")
    no_priced_cost(facts["usage"], report, "full (bare native cost number)")
    ctx.evidence.append({"kind": "c11_usage", "form": "full", "native": report, "facts": facts["usage"],
                         "incomplete": facts.get("incomplete")})

    # 2. Window only: occupancy stays absent; last-turn totals never become context.used.
    report, expected = native_report(wire, window=128000, tokens=(500, 60, 0, 0))
    await report_usage(ctx, wire, report)
    facts = await wait_usage(ctx, expected, "window_only")
    context = facts["usage"].get("context") or {}
    check("used" not in context, f"C11: window-only report published occupancy {context.get('used')!r}")
    no_priced_cost(facts["usage"], report, "window_only")
    ctx.evidence.append({"kind": "c11_usage", "form": "window_only", "native": report, "facts": facts["usage"]})

    # 3. Sections the runtime stops reporting stay unknown, never zero.
    report, expected = native_report(wire, window=96000)
    await report_usage(ctx, wire, report)
    facts = await wait_usage(ctx, expected, "tokens_absent")
    check("tokens" not in facts["usage"], f"C11: absent native token totals published as {facts['usage'].get('tokens')!r}")
    no_priced_cost(facts["usage"], report, "tokens_absent")
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
