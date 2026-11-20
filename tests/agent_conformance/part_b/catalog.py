# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Bennett Garcia
"""Known Part B titles; the executable inventory is derived from the pinned spec.
A missing executable probe is a harness failure.

Titles summarize selection only. Each executable probe must satisfy the full
normative input/assertion set; the draft text, version and hash accompany runs.
"""
from dataclasses import dataclass
import re


@dataclass(frozen=True)
class Case:
    id: str
    title: str
    commands: tuple[str, ...] = ()


GROUPS = {
    "I": (
        "claim durably precedes native invoke",
        "four crash cuts retain held/unknown/settled without replay",
        "uncertain claim never invokes; adopted publication matches",
        "all ordinary writers/reverts/imports/undo cannot resurrect operational records",
        "edit/withdraw/claim excludes stale payload delivery",
        "settled retention plus restart never requalifies delivery",
        "independent lanes and unqueued controls",
        "busy retry cap/backoff/guard/expiry; stale held for review",
        "atomic quotas, reserved compact outcomes and uncertain result writes",
        "evicted operation replay and changed-intent conflict across epochs",
        "interrupt_then_start partial steps and restart/deadline/pause fences",
        "session pin and follow-current guard recheck",
        "handover pending until all stages; CAS and gate preservation",
        "stale owner callback cannot invoke",
        "resolve needs sender/recipient/recover; never invokes",
        "pending_exists unless explicit send_new",
        "principal-scoped submission ids and editable pending across window roll",
        "sender fairness and quotas including send_new",
        "200-open burst/flood leaves declared control and answer headroom",
        "derived sender origin and unsupported rendering held",
        "eligible priority ordering, sender round-robin/FIFO and pending edit CAS",
        "unknown predecessor blocks with visible ordering_unknown and explicit resolution",
        "normal saturation preserves bounded controls and defensive archive-only funding",
    ),
    "H": (
        "crash before claim requires current-epoch resume",
        "claimed crash stays unknown through resume",
        "native held insertion during pause is not suspension or duplicate",
        "pause/claim race has one winner and outstanding evidence",
        "restored open gate and stale resume grant no permit",
        "scope membership CAS and committed-op frozen selection",
        "active member move/undo never changes its gate or earlier permit",
        "mixed scope member reasons and independent ready admission",
        "scope resume authority checked atomically for every member",
        "resume cannot renew expired/stale preempt",
        "owner restart invalidates permits but retains historical scope result",
        "controls/answers bypass held content; resume answers nothing",
        "handle rebind moves no membership/gate/claim",
        "nested/absent scope and stalled viewer stay bounded",
        "read-only startup exposes membership/mail/unknown without writes",
        "200-agent scope frozen at one revision with paged result",
    ),
    "G": (
        "connection identity is authority, declared label is not",
        "per-principal undo and explicit attributed revert",
        "additive union, expiry, subjects, missing grant and owner authority",
        "resolved strategy grant checked again at claim",
        "raw undeclared caller has no owner rights or default undo",
        "restart retains grants and attribution",
        "worker self-resume and administrative self-escalation refused",
        "grants.manage cannot widen target or rights",
        "callback/delegation propagation, scope/expiry and combined initiator",
        "query is not authorization; coordinated refusals write nothing",
        "shared-host tool identity is endpoint-evidenced or host, not first child",
        "owner/delegation mutation checks pre-transaction authority",
        "handle grants pin subject/target; rebinding transfers no right",
    ),
    "T": (
        "marker mapping, no default typing and missing-plugin hold",
        "point-specific eligibility, ordered bounded single-flight batches",
        "abort/context hooks never fabricate read",
        "released excludes inserted entries; take/insertion crash is unknown",
        "reconcile evidence, consumer fence and repeated-take history only",
        "late old claim report rejected after release/re-take",
        "paused pull holds, wake carries counts only",
        "pending edits versus too_late and byte-identical editor draft",
        "plugin supersedes marker by recorded handover",
        "context/no-entry means supplied, never inserted, with origin",
        "consumer content filtering never claims unsupported media",
        "declared Claude hook paths preserve supplied/read and native framing evidence",
        "bounded sequenced advisory terminal status is live-only and never authority",
        "per-record consumer budget holds oversized input without claim or truncation",
    ),
    "L": (
        "stop closes gate; exact declared host lifecycle or unsupported",
        "end holds pending, unknowns claims, rejects ended handle; no reroute",
        "900/hour identity cache bound and maximum-size summaries/outcomes",
        "full durable budget preserves late result reservation and recovery",
        "disconnected retire is not native ended evidence",
        "membership stays undoable with full protected domain; frozen scope unchanged",
        "archive crash/durability/edit/receipt fences and locator uncertainty",
    ),
    "S": ("combined 600-terminal/200-agent/3-viewer flood/reconnect headroom gate",),
}

CASES = tuple(Case(f"{group}{index}", title)
              for group, titles in GROUPS.items()
              for index, title in enumerate(titles, start=5 if group == "L" else 1))
BY_ID = {case.id: case for case in CASES}


def cases_from_spec(text):
    """Derive groups as well as IDs, including newly introduced normative groups."""
    if "# Part B:" not in text or "## Running" not in text:
        raise ValueError("conformance document lacks Part B boundaries")
    body=text.split("# Part B:",1)[1].split("## Running",1)[0]
    headings=list(re.finditer(r"^\*\*([A-Z])\.\s",body,re.M))
    if not headings: raise ValueError("conformance document has no normative case groups")
    if len({match.group(1) for match in headings})!=len(headings): raise ValueError("duplicate normative case group")
    cases=[]
    for index,match in enumerate(headings):
        group=match.group(1);section=body[match.end():headings[index+1].start() if index+1<len(headings) else len(body)]
        numbers={int(n) for n in re.findall(r"\b"+group+r"([0-9]+)\b",section)}
        for start,end in re.findall(r"\b"+group+r"([0-9]+)\s*[–-]\s*(?:"+group+r")?([0-9]+)\b",section):
            start,end=int(start),int(end)
            if end<start or end-start>10000: raise ValueError("invalid or unbounded normative case range")
            numbers.update(range(start,end+1))
        # A group without an inline numbered row is one case (the scale gate).
        if not numbers: numbers={1}
        cases.extend(BY_ID.get(group+str(n),Case(group+str(n),"normative "+group+str(n))) for n in sorted(numbers))
    return tuple(cases)


def select_cases(names=None,inventory=None):
    inventory=CASES if inventory is None else inventory
    known={case.id:case for case in inventory}
    if not names: return list(inventory)
    wanted=set(names);unknown=wanted-known.keys()
    if unknown: raise ValueError(f"unknown Part B cases: {sorted(unknown)}")
    return [case for case in inventory if case.id in wanted]
