"""Independent request probes for Part A R3 and R4 (review findings R8/R9). K1 wires them into its handlers.

- `answer_acknowledgement(ctx)`: R3. `accepted` only with a native acknowledgement, else `dispatched`. A valid answer
  is actually sent and the independent native ledger must show exactly one handoff to that native request. The
  declaration and an identical public outcome are never proof by themselves.
- `positive_relative_timeout(ctx)`: an additional R4 vector, selected by `agent.respond.native_timeout ==
  "unrepresentable_positive"` (Pi). An untimed dialog stays answerable; a dialog with a positive native relative
  timeout is cancelled once, is never approvable and never gains an invented absolute expiry.

Every pass carries an observed native witness in `ctx.evidence`. A branch the fixture cannot drive is a HarnessGap.
Nothing here is registered or certifies an implementation.
"""
import asyncio

from .results import CheckFailed, HarnessGap, check

HANDED_OVER = ("accepted", "dispatched")
# A positive relative timeout in milliseconds, far beyond the probe: expiry is never simulated by waiting.
NATIVE_TIMEOUT_MS = 600000
NATIVE_ACK_KINDS = ("answer_ack",)  # ledger kinds a fixture writes when the native side acknowledges an answer


def native_answers(ctx, native_id, after):
    return [row for row in ctx.native.records("answer", after=after) if row.get("request") == native_id]


def native_acks(ctx, native_id, after):
    return [row for row in ctx.native.records(after=after)
            if row.get("kind") in NATIVE_ACK_KINDS and row.get("request") == native_id]


async def settle_native(ctx, native_id, after, count=1):
    """Wait for `count` native answers to `native_id`, then a short quiet period to see any duplicate."""
    try:
        await ctx.native.wait(lambda rows: len([row for row in rows if row["seq"] > after and row["kind"] == "answer"
                                                and row.get("request") == native_id]) >= count)
    except CheckFailed:
        pass  # the caller judges what (if anything) arrived
    await asyncio.sleep(.1)
    return native_answers(ctx, native_id, after)


def subcase(ctx, name, status, **details):
    ctx.evidence.append({"kind": "subcase", "case": name, "status": status, **details})


async def answer_acknowledgement(ctx):
    from .cases import answer, caps, make_request, rejection, valid_response
    declaration = await caps(ctx, "agent.respond")
    ack = declaration.get("ack")
    if ack not in HANDED_OVER:
        raise HarnessGap("unknown request acknowledgement declaration " + repr(ack))
    if ack == "accepted":
        # The positive branch needs the fixture to hold back the native acknowledgement and release it, so both
        # sides of that boundary can be observed. No current fixture can; never fake a native reply.
        raise HarnessGap("R3 accepted branch: the native fixture cannot gate/drop an answer acknowledgement")
    subcase(ctx, "R3.accepted_ack_boundary", "not-applicable", exclusion={"path": "agent.respond.ack", "actual": ack})

    native_id, request = await make_request(ctx)
    response = valid_response(request)
    mark = ctx.native.mark()
    result = await answer(ctx, request, response)
    outcome = result.get("outcome")
    answers = await settle_native(ctx, native_id, mark)
    check(outcome in HANDED_OVER, f"valid answer was not handed over: {result}")
    check(answers, "no native handoff for a valid answer (declaration and public outcome alone are not proof)")
    check(len(answers) == 1, f"a valid answer reached the native request {len(answers)} times")
    native = answers[0]
    check(native.get("pending", True) is True, "the native answer did not target a pending native request")
    normalized = native.get("normalized_response")
    check(isinstance(normalized, dict) and normalized.get("action") not in ("cancel", "_invalid"),
          f"the native handoff is not the valid answer: {normalized}")
    if request["method"] in ("confirm", "text"):
        check(normalized == response, "the answer changed on its way to the native request")
    acks = native_acks(ctx, native_id, mark)
    # A later state query or fence is not an acknowledgement of the answer: only an ack row for this request counts.
    if outcome == "accepted":
        check(acks, "answer reported accepted without a native acknowledgement")
    check(outcome == ack, f"answer acknowledgement over/underclaimed: declared {ack}, got {outcome}")

    # First valid answer wins: a repeat is stale and never reaches the native side again.
    repeat_mark = ctx.native.mark()
    rejection(await answer(ctx, request, response), kinds=("stale",))
    await asyncio.sleep(.1)
    check(not native_answers(ctx, native_id, repeat_mark), "an already-answered request was sent to the native side again")
    ctx.evidence.append({"kind": "answer_acknowledgement", "declared": ack, "outcome": outcome,
                         "request": request["request"], "revision": request["revision"], "scope": request.get("scope"),
                         "native_request": native_id, "native_answer_seq": native["seq"],
                         "native_acks": len(acks), "repeat_rejected_stale": True})


async def positive_relative_timeout(ctx):
    from .cases import answer, caps, fresh_view, make_request, rejection, valid_response
    declaration = await caps(ctx, "agent.respond")
    selected = declaration.get("native_timeout")
    if selected != "unrepresentable_positive":
        subcase(ctx, "R4.positive_relative_timeout", "not-applicable",
                exclusion={"path": "agent.respond.native_timeout", "actual": selected})
        return
    method = next((name for name in ("confirm", "choice", "text") if name in declaration.get("methods", [])), None)
    if method is None:
        raise HarnessGap("R4 timeout vector has no native dialog fixture for the declared methods")

    # An untimed dialog of the same method stays answerable: one public entity, one real native response.
    untimed_id, request = await make_request(ctx, method)
    check(request.get("status") == "pending" and not request.get("incomplete"), "untimed dialog is not answerable")
    mark = ctx.native.mark()
    result = await answer(ctx, request, valid_response(request))
    check(result.get("outcome") in HANDED_OVER, f"untimed dialog answer not handed over: {result}")
    answers = await settle_native(ctx, untimed_id, mark)
    check(len(answers) == 1 and answers[0].get("normalized_response", {}).get("action") not in ("cancel", "_invalid"),
          f"untimed dialog did not get exactly one native answer: {answers}")
    untimed_seq = answers[0]["seq"]

    # The same dialog with a positive native relative timeout: cancelled once as unrepresentable.
    view = await fresh_view(ctx)
    known = {key for key in view.reducer.entities if key[0] == "request"}
    pending_before = {row["request"] for row in (await ctx.facts()).get("requests", [])}
    trace_cut = view.reducer.sequence
    trace_dropped = view.reducer.trace_dropped
    timed_id = "R4-timed-" + ctx.delivery()
    mark = ctx.native.mark()
    await ctx.native.call_fields("request", {"request": timed_id, "method": method, "timeout": NATIVE_TIMEOUT_MS,
                                             "title": "R4 timed dialog"})
    issued = [row for row in ctx.native.records("request", after=mark) if row.get("request") == timed_id]
    check(len(issued) == 1 and issued[0].get("timeout") == NATIVE_TIMEOUT_MS
          and issued[0].get("native_event", {}).get("id") == timed_id
          and issued[0].get("native_event", {}).get("timeout") == NATIVE_TIMEOUT_MS,
          "fixture did not emit the exact positive native relative timeout", component="fixture")
    answers = await settle_native(ctx, timed_id, mark)
    check(answers, "a dialog with a positive native timeout was never answered (not cancelled as unrepresentable)")
    check(len(answers) == 1, f"the timed dialog got {len(answers)} native responses")
    check(answers[0].get("normalized_response") == {"action": "cancel"},
          f"the timed dialog was answered, not cancelled: {answers[0].get('normalized_response')}")
    await asyncio.sleep(.1)
    facts = await ctx.facts()
    new_pending = {row["request"] for row in facts.get("requests", [])} - pending_before
    check(not new_pending, f"the timed dialog left an approvable public request: {sorted(new_pending)}")
    check(view.reducer.trace_dropped == trace_dropped, "timed-dialog trace overflowed before its transient state could be checked", component="harness")
    appeared = [row["value"] for key, row in view.reducer.entities.items() if key[0] == "request" and key not in known]
    transient = [row["value"] for row in view.reducer.trace if row.get("op") == "put" and row.get("kind") == "request"
                 and ("request", row.get("id")) not in known and row.get("seq", -1) > trace_cut]
    for entity in transient + appeared + [row for row in facts.get("requests", []) if row["request"] not in pending_before]:
        check("expires_at_ms" not in entity, "an absolute expiry was invented for a relative native timeout")
        check(entity.get("status") != "pending" or bool(entity.get("incomplete")),
              "the timed dialog is visible as a complete pending request")
        probe_mark = ctx.native.mark()
        rejection(await answer(ctx, entity, {"value": True}), kinds=("invalid", "stale"))
        check(not native_answers(ctx, timed_id, probe_mark), "the timed dialog was approved after its cancel")
    ctx.evidence.append({"kind": "positive_relative_timeout", "declared": selected, "method": method,
                         "untimed_native_request": untimed_id, "untimed_native_answer_seq": untimed_seq,
                         "timed_native_request": timed_id, "native_relative_timeout_ms": NATIVE_TIMEOUT_MS,
                         "timed_native_cancel_seq": answers[0]["seq"], "native_responses": 1,
                         "native_timeout_request_seq": issued[0]["seq"], "native_event": issued[0]["native_event"],
                         "visible_transient_entities": len(transient), "approvable_after": False})
