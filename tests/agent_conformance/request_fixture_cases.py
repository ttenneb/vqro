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
    made, refused = [], 0
    mark = ctx.native.mark()
    for index in range(16):
        try:
            _, request = await make_request(ctx, method, **options)
        except CheckFailed as error:
            # A request the adapter cannot represent is cancelled once, never silently dropped (checked below).
            refused += 1
            ctx.evidence.append({"kind": "c6_request_not_observable", "index": index, "error": str(error)[:300]})
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
        cut = {key for key, value in summary.items() if key in request and value != request[key]}
        if cut:
            incomplete = summary.get("incomplete")
            check(incomplete is True or (isinstance(incomplete, list) and cut <= set(incomplete)),
                  f"C6: summary fields {sorted(cut)} were cut without an incomplete marker")
    answers = ctx.native.records("answer", after=mark)
    if refused:
        # Over the adapter's pending-request budget: exactly one definite native cancellation, no silent loss.
        check(len(answers) == 1 and answers[0].get("normalized_response") == {"action": "cancel"},
              "C6: a request beyond the facts/request budget was silently dropped instead of cancelled once")
    ctx.evidence.append({"kind": "bounded_facts", "method": method, "requests": len(made), "over_budget_cancelled": refused,
                         "fact_bytes": encoded(facts), "native_detail_bytes": len(detail.encode()),
                         "full_request_detail_preserved": True})


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
    if "permission" not in declaration.get("methods", []):
        raise HarnessGap("R6 needs a foreground permission request fixture")
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
    stale = await answer(ctx, foreground, valid_response(foreground))
    check(stale.get("outcome") not in ("accepted", "dispatched"), "R6: the withdrawn foreground request was still answerable")
    mark = ctx.native.mark()
    result = await answer(ctx, background, valid_response(background))
    check(result.get("outcome") in ("accepted", "dispatched"), f"R6: the background request was no longer answerable: {result}")
    await ctx.native.wait(lambda rows: any(r["seq"] > mark and r["kind"] == "answer" for r in rows))
    answered = ctx.native.records("answer", after=mark)
    check(len(answered) == 1 and answered[0].get("request") == background_id, "R6: background answer reached the wrong native request")
    ctx.evidence.append({"kind": "r6_background_foreground", "background": background["request"], "foreground": foreground["request"],
                         "withdrawn_native": [row.get("request") for row in withdrawn], "background_answered": True})
