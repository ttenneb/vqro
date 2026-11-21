"""Part A request-fixture branches for C6 (non-confirm facts pressure), R2 (native withdrawal) and R6 (background vs
foreground withdrawal). Public wire plus the private fixture ledger only; no native field is invented: every native
request/withdrawal used here is one the fixture emits in the runtime's own protocol and that the adapter's decoder
reads (Codex: src/codex.rs:698 serverRequest/resolved, :821-905 approval/user-input requests).

The functions are called from the corresponding cases.py handlers when the declared request methods/scopes take
these branches; the shared registry is untouched.
"""
import asyncio
import json

from .results import CheckFailed, HarnessGap, check

FACTS_BOUND = 64 << 10
SUMMARY_FIELDS = ("request", "revision", "method", "title")  # agent-v0 facts request summary
WAIT_FACTS_TIMEOUT = "public agent facts did not reach the required state"  # core.py wait_facts diagnostic prefix


def encoded(value):
    return len(json.dumps(value, ensure_ascii=False, separators=(",", ":")).encode())


def pending(facts):
    return [row for row in facts.get("requests", []) if row.get("status", "pending") == "pending"]


async def c6_non_confirm(ctx, declaration):
    """Facts pressure through actual native request detail for a non-confirm method (permission or form)."""
    from .cases import make_request
    method = next((name for name in ("permission", "form") if name in declaration.get("methods", [])), None)
    if method is None:
        raise HarnessGap("C6 needs a confirm, permission or form request fixture for the declared methods")
    if set(declaration.get("scopes", [])) & {"session", "run"}:
        await ctx.ensure_session()  # session/run-scoped requests need a live native session
    # Each request carries ~3 KiB of real native detail (permission: the native command; form: the question text).
    detail = "C6-native-request-detail:" + "x" * 3072
    options = {"command": detail, "title": "C6 approval"} if method == "permission" else {"message": detail}
    made, overflow = [], None
    mark = ctx.native.mark()
    for index in range(16):
        attempt = ctx.native.mark()
        try:
            _, request = await make_request(ctx, method, **options)
        except CheckFailed as error:
            # Only "never became observable" can be the adapter's budget refusal; anything else is a real failure.
            # HarnessGap/FixtureFailed subclasses and any other assertion are never budget pressure.
            if type(error) is not CheckFailed or not str(error).startswith(WAIT_FACTS_TIMEOUT):
                raise
            issued = [row.get("request") for row in ctx.native.records("request", after=attempt)]
            if len(issued) != 1 or not isinstance(issued[0], str) or not issued[0]:
                raise CheckFailed("C6: the unobservable attempt has no single native request identity", component="fixture")
            overflow = issued[0]
            ctx.evidence.append({"kind": "c6_request_not_observable", "index": index, "native_request": overflow,
                                 "error": str(error)[:300]})
            break
        made.append(request)
        facts = await ctx.facts()
        check(encoded(facts) <= FACTS_BOUND, f"agent facts exceeded 64 KiB under native request pressure ({index + 1} requests)")
    check(made, "C6: no native request with large detail became observable")
    for request in made:
        params = request.get("params") or {}
        preserved = params.get("command") if method == "permission" else json.dumps(params.get("questions"))
        check(isinstance(preserved, str) and detail in preserved,
              "C6: the request entity lost the actual native request detail", component="adapter")
    facts = await ctx.facts()
    summaries = {row["request"]: row for row in facts.get("requests", [])}
    for request in made:
        summary = summaries.get(request["request"])
        check(summary is not None and summary.get("method") == request["method"] and summary.get("revision") == request["revision"],
              "C6: a pending request summary disagrees with its entity")
        # Every summary field the entity carries must be present and equal, or explicitly marked incomplete.
        cut = {key for key in SUMMARY_FIELDS if key in request and summary.get(key, object()) != request[key]}
        if cut:
            incomplete = summary.get("incomplete")
            check(incomplete is True or (isinstance(incomplete, list) and cut <= set(incomplete)),
                  f"C6: summary fields {sorted(cut)} were cut without an incomplete marker")
    answers = ctx.native.records("answer", after=mark)
    if overflow is not None:
        # Over the adapter's pending-request budget: exactly one definite native cancellation of THAT request.
        check(len(answers) == 1 and answers[0].get("request") == overflow
              and answers[0].get("normalized_response") == {"action": "cancel"},
              "C6: a request beyond the facts/request budget was not cancelled exactly once (or another request was)")
    else:
        check(not answers, "C6: native requests were answered during facts pressure")
    ctx.evidence.append({"kind": "bounded_facts", "method": method, "requests": len(made), "over_budget_cancelled": overflow,
                         "fact_bytes": encoded(facts), "native_detail_bytes": len(detail.encode()),
                         "full_request_detail_preserved": True})


def expiry_exclusion(declaration):
    """R2 'expired' is excluded only by an explicit declaration: `unrepresentable_positive` (positive native timeouts
    the adapter cannot represent, Pi) or `none` (the runtime has no request expiry at all, Codex app-server)."""
    value = declaration.get("native_timeout")
    return value if value in ("unrepresentable_positive", "none") else None


async def r2_native_withdrawal(ctx, declaration):
    """The runtime itself stops waiting (its own resolution notice): an answer is then refused before any native write.
    Returns a reason when the declared runtime has no representable native expiry, so the caller keeps an honest gap."""
    from .cases import make_request, answer, valid_response, rejection
    native_id, request = await make_request(ctx)
    mark = ctx.native.mark()
    await ctx.native.call("resolve_request", request=native_id)
    await ctx.wait_facts(lambda f: request["request"] not in {row["request"] for row in pending(f)}, timeout=5)
    result = await answer(ctx, request, valid_response(request))
    rejection(result, kinds=("stale",))
    check(not ctx.native.writes(mark), "an answer to a natively withdrawn request reached the native runtime")
    ctx.evidence.append({"kind": "r2_native_withdrawal", "native_request": native_id, "request": request["request"],
                         "answer": result.get("error", {}).get("message")})
    return ("the runtime protocol has no request expiry signal (requests end only by its own resolution notice) and the "
            "adapter declares no agent.respond.native_timeout; 'expired' cannot be exercised")


async def r6_background_foreground(ctx, declaration):
    """background_requests: true. A background (session) request and a foreground run-blocking permission are both
    pending; an interrupt makes the runtime stop waiting only on the foreground one."""
    from .cases import make_request, answer, valid_response
    if not {"permission", "form"} <= set(declaration.get("methods", [])):
        raise HarnessGap("R6 background/foreground fixture needs declared permission (foreground) and form (background) methods")
    scopes = set(declaration.get("scopes", []))
    if not {"session", "run"} <= scopes:
        raise HarnessGap("R6 background/foreground scenario needs declared session and run scopes")
    await ctx.native.call("turn", state="start")
    await ctx.wait_facts(lambda f: f.get("state") == "running")
    # A non-blocking user-input request from another (background) turn, as the runtime protocol allows.
    background_id, background = await make_request(ctx, "form", scope="session", is_blocking=False, run="r6-background-turn",
                                                   message="R6 background")
    foreground_id, foreground = await make_request(ctx, "permission", command="R6 foreground")
    check(background["scope"].get("run") != foreground["scope"].get("run"),
          "R6: background request was bound to the foreground run", component="adapter")
    both = await ctx.facts()
    check({background["request"], foreground["request"]} <= {row["request"] for row in pending(both)},
          "R6: background and foreground requests were not both pending")
    mark = ctx.native.mark()
    interrupt = await ctx.invoke("agent.interrupt", {})
    check(interrupt.get("outcome") in ("accepted", "dispatched", "observed"), f"R6: interrupt was not handed over: {interrupt}")
    facts = await ctx.wait_facts(lambda f: foreground["request"] not in {row["request"] for row in pending(f)}, timeout=5)
    remaining = {row["request"] for row in pending(facts)}
    check(background["request"] in remaining, "R6: interrupt withdrew the background request the runtime still waits on")
    withdrawn = [row for row in ctx.native.records(after=mark) if row["kind"] == "request_cancelled"]
    check([row.get("request") for row in withdrawn] == [foreground_id],
          "R6: the native runtime withdrew a different set than the foreground request", component="fixture")
    check(not ctx.native.records("answer", after=mark), "R6: withdrawal produced a native answer instead of a runtime resolution")
    # The background request stays answerable; the withdrawn foreground one is refused before native write.
    from .cases import rejection
    mark = ctx.native.mark()
    stale = await answer(ctx, foreground, valid_response(foreground))
    rejection(stale, kinds=("stale",))
    check(not ctx.native.writes(mark), "R6: an answer to the withdrawn foreground request reached the native runtime")
    mark = ctx.native.mark()
    result = await answer(ctx, background, valid_response(background))
    check(result.get("outcome") in ("accepted", "dispatched"), f"R6: the background request was no longer answerable: {result}")
    await ctx.native.wait(lambda rows: any(r["seq"] > mark and r["kind"] == "answer" for r in rows))
    answered = ctx.native.records("answer", after=mark)
    check(len(answered) == 1 and answered[0].get("request") == background_id, "R6: background answer reached the wrong native request")
    ctx.evidence.append({"kind": "r6_background_foreground", "background": background["request"], "foreground": foreground["request"],
                         "withdrawn_native": [row.get("request") for row in withdrawn], "background_answered": True})
