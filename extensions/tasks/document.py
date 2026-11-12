"""Native attention and pane panels, from Tasks' own durable store.

Terminal placement is a host observation, not execution/acceptance authority.
Live owner status is joined by Vqro when it draws, so this module publishes the
durable meaning only: which task, who owes what, and since when.
"""
import datetime
import json
import re
from collections import Counter
from decision_queue import text as decision_text
from provider import MAX_RECORDS, canonical, checked_binding
from view import clean, debtors, durable_attention as attention, label, recipient

PREFIX = "context:attention."
VIEW_CONTRACT = "host.view.v1"
PANEL_VIEW = "tasks"
PANEL_ROW_LIMIT = 24
# Optional, inert presentation hints. Current hosts ignore this field; it grants
# no caller authority and must never become an executable document action.
AFFORDANCE_BYTES = 8192
AFFORDANCES_BYTES = 32 * 1024
DOCUMENT_BYTES = 256 * 1024
# The one word Vqro composes "waiting on ..." around.
OWED_TEXT = {
    "start": "start",
    "work/report": "report",
    "unblock": "unblock",
    "accept": "accept",
    "answer": "answer",
    "acceptor_missing": "accept",
}
CHIP_TONE = {"open": "muted", "in_progress": "accent", "blocked": "danger", "reported": "success"}
CHIP_TEXT = {"open": "open", "in_progress": "in progress", "blocked": "blocked", "reported": "reported"}


def badge_text(counts):
    return " · ".join(f"{counts[key]} {text}" for key, text in
                      (("human", "waiting on you"), ("blocked", "blocked"), ("unavailable", "unavailable")) if counts[key])


def bounded(text):
    return clean(text).encode("utf-8")[:240].decode("utf-8", errors="ignore")


def epoch(value):
    """Unix epoch seconds of a store instant, or None when unreadable."""
    if not value:
        return None
    try:
        return int(datetime.datetime.fromisoformat(value).timestamp())
    except ValueError:
        return None


def final(task):
    return task["state"] in ("accepted", "done")


def state_name(task):
    """The fixed state Vqro draws a Pi-style glyph for."""
    return {"open": "open", "in_progress": "in_progress", "blocked": "blocked", "reported": "reported",
            "accepted": "done", "done": "done"}.get(task["state"], "open")


def count_word(task):
    if final(task):
        return "done"
    if task["state"] == "blocked":
        return "blocked"
    if task["state"] == "in_progress":
        return "active"
    return "ready" if task.get("ready") else "waiting"


def summary(counts):
    """The panel header's own summary of the slice, in the todo widget's shape."""
    parts = [f"{counts['done']}/{sum(counts.values())} done"]
    parts.extend(f"{counts[word]} {word}" for word in ("active", "blocked", "ready", "waiting") if counts[word])
    return " · ".join(parts)


def human_debt(task):
    """Stored human debt, not proof that any particular client may act."""
    return task.get("owed_action") in ("accept", "answer") and any(
        binding.get("class") == "human_unverified" or binding.get("role") == "human_unverified"
        for binding in debtors(task.get("owed_to"))
    )


def human_affordance(task, unfinished_parents):
    """Describe only listed choices/acceptance; malformed hints stay read-only.

    The subject is opaque to the host. Any future invocation must independently
    authorize its caller and recheck this displayed revision in the provider.
    """
    if not human_debt(task) or task.get("waiting_on") or task["id"] in unfinished_parents:
        return None
    try:
        decision_text(task["id"], 128, "subject id")
        if type(task.get("revision")) is not int or not 1 <= task["revision"] <= 2**63 - 1:
            return None
        subject = {"id": task["id"], "revision": task["revision"]}
        if task.get("kind") == "task" and task["state"] == "reported" and task["owed_action"] == "accept":
            hint = {"kind": "action", "label": "Accept work", "subject": subject}
        elif task.get("kind") == "decision" and task["state"] == "open" and task["owed_action"] == "answer":
            terms = task.get("decision")
            if not isinstance(terms, dict) or terms.get("resolution") is not None:
                return None
            options = terms.get("options")
            if not isinstance(options, list) or not 2 <= len(options) <= 4 or type(terms.get("reversible")) is not bool:
                return None
            keys, choices = [], []
            for option in options:
                if not isinstance(option, dict) or set(option) != {"key", "label"}:
                    return None
                key = decision_text(option["key"], 32, "option key")
                if not all(c.isascii() and (c.isalnum() or c in "_-.") for c in key):
                    return None
                keys.append(key)
                choices.append({"key": key, "label": decision_text(option["label"], 256, "option label")})
            if len(set(keys)) != len(keys) or terms.get("default") not in keys:
                return None
            hint = {"kind": "choice", "label": "Answer", "subject": subject,
                    "options": choices, "default": terms["default"], "reversible": terms["reversible"]}
        else:
            return None
        hint["detail"] = decision_text(clean(task["title"]), 1024, "action detail")
        return hint if len(canonical(hint).encode()) <= AFFORDANCE_BYTES else None
    except (KeyError, TypeError, ValueError):
        return None


def pane_slices(tasks, placements):
    """Per-pane slices: counts over the whole slice, rows for open work.

    A task appears in its owner's pane as that agent's debt, and in the pane of
    the agent it is owed to as something they are waiting for. No placement, no
    row: the host's placement is an observation, never a guess.
    """
    by_id = {task["id"]: task for task in tasks}
    slices = {}
    for task in tasks:
        owner = task.get("owner") or {}
        creditor = recipient(task, by_id) or {}
        owner_placement = (placements or {}).get(owner.get("terminal_id"))
        creditor_placement = (placements or {}).get(creditor.get("terminal_id"))
        owed = OWED_TEXT.get(task.get("owed_action") or "", "work")
        entries = []
        # In the owner's pane the viewer owes the action; in the pane it is owed
        # to, the owner does. Vqro says which, and names the other side.
        if owner_placement:
            entries.append((owner_placement["pane_id"], "you", owed, creditor, creditor_placement))
        if creditor_placement and creditor_placement["pane_id"] != (owner_placement or {}).get("pane_id"):
            # A human answer is shown in its filer's actual pane, not in an
            # invented human terminal. This wording is presentation, not a
            # binding to the client currently viewing the document.
            waiting = "you" if human_debt(task) else "them"
            entries.append((creditor_placement["pane_id"], waiting, owed, owner, owner_placement))
        for pane_id, waiting, action, counterparty, counterparty_placement in entries:
            bucket = slices.setdefault(pane_id, {"counts": Counter(), "rows": []})
            bucket["counts"][count_word(task)] += 1
            if not final(task):
                bucket["rows"].append({
                    "task": task,
                    "state": state_name(task),
                    "waiting": waiting,
                    "action": action,
                    "counterparty": counterparty,
                    "counterparty_placement": counterparty_placement,
                    "since": epoch(task.get("owed_since") or task.get("updated_at")),
                })
    for bucket in slices.values():
        bucket["rows"].sort(key=lambda row: (row["task"]["state"] != "blocked",
                                             row["task"].get("owed_since") or row["task"].get("updated_at") or ""))
    return slices


def panel_terminal(binding, placement):
    if binding is None or placement is None or not binding.get("terminal_id"):
        return None
    if binding.get("role") == "acceptors" or binding.get("class") in ("human_unverified",):
        return None
    return {"terminal_id": binding["terminal_id"], "placement": placement}


MESSAGE_VIEW = "messages"
MESSAGE_LIMIT = 8
MESSAGE_PREVIEW = 60
# A durable id in an otherwise human sentence. The panel shows what the message is
# about, not the store's keys.
INTERNAL_ID = re.compile(r"\b(?P<kind>task|message|plan)_[0-9a-fA-F]{4,}\b")


def message_record(row):
    """One messages row as the store keeps it."""
    return {key: row[key] for key in row.keys()}


def message_evidence(store):
    """Complete bounded headers, never a preview or task binding as currentness.

    project() holds one read transaction across these headers and body previews.
    A cap+1 read proves completeness without hydrating all stored bodies.
    """
    scopes, known, replied = {}, {}, set()
    if store is None:
        return scopes, known, replied
    headers = store.db.execute(
        "SELECT id,sender,recipient,recipient_execution,state,reply_to FROM messages ORDER BY id LIMIT ?",
        (MAX_RECORDS + 1,)).fetchall()
    if len(headers) > MAX_RECORDS:
        raise ValueError("message evidence exceeds store quota")
    ids = {row["id"] for row in headers}
    for row in headers:
        for field in ("sender", "recipient"):
            key = row[field]
            if key not in known:
                binding = checked_binding(json.loads(key))
                if any(isinstance(value, str) and clean(value) != value for value in binding.values()):
                    raise ValueError("invalid recorded message identity")
                known[key] = binding
        recipient = known[row["recipient"]]
        if recipient.get("class") != "agent" or row["recipient_execution"] != recipient.get("execution"):
            raise ValueError("invalid recorded recipient execution")
        if row["state"] not in ("waiting", "claimed", "answered"):
            raise ValueError("invalid recorded message state")
        if row["reply_to"] is not None:
            if row["reply_to"] not in ids:
                raise ValueError("invalid recorded reply reference")
            replied.add(row["reply_to"])
        terminals = set()
        for role in ("sender", "recipient"):
            key = row[role]
            terminal = known[key].get("terminal_id")
            if not terminal:
                continue
            bucket = scopes.setdefault(terminal, {"total": 0, "waiting": 0, "contexts": {}})
            context = bucket["contexts"].setdefault((role, key), {"waiting": 0})
            if role == "recipient" and row["state"] == "waiting":
                bucket["waiting"] += 1
                context["waiting"] += 1
            terminals.add(terminal)
        for terminal in terminals:
            scopes[terminal]["total"] += 1
    return scopes, known, replied


def message_subject(record):
    body = " ".join(bounded(record.get("body") or "").split())
    if not body:
        return "(no body)"
    # Internal ids are bookkeeping, not subject matter: "message from task_66c03c"
    # reads as "message about a task" rather than making the reader parse a hash.
    body = INTERNAL_ID.sub(lambda match: f"a {match.group('kind').lower()}", body)
    return body[:MESSAGE_PREVIEW].strip()


def recorded_identity(binding):
    if binding.get("class") == "agent":
        return f"{binding['agent']} · {binding['execution']}"
    return label(binding, None)


def message_rows(records, terminal_id, known, replied):
    """Bounded stored previews, not a current execution's actionable inbox."""
    rows = []
    for record in records:
        sender, recipient = known[record["sender"]], known[record["recipient"]]
        incoming = recipient.get("terminal_id") == terminal_id
        if not incoming and sender.get("terminal_id") != terminal_id:
            continue
        if incoming:
            state = "open" if record["state"] == "waiting" else "done"
            detail = record["state"]
            text = f"{label(sender, None)}: {message_subject(record)}"
        else:
            missing = record["id"] not in replied
            state = "reported" if missing else "done"
            detail = "no recorded reply" if missing else "recorded reply"
            text = message_subject(record)
        rows.append({"label": text, "state": state, "detail": detail,
                     "since": epoch(record.get("updated_at") or record.get("created_at")),
                     "counterparty": sender if incoming else None,
                     "context": ("recipient", record["recipient"]) if incoming else ("sender", record["sender"]),
                     "waiting": incoming and record["state"] == "waiting"})
    rows.sort(key=lambda row: not row["waiting"])
    return rows[: MESSAGE_LIMIT * 2]


def message_panel_items(placements, messages, evidence, room):
    """Complete recorded context is distinct from currentness and previews."""
    scopes, known, replied = evidence
    items, panes = [], {}
    for terminal_id, placement in sorted((placements or {}).items()):
        panes.setdefault(placement["pane_id"], terminal_id)
    candidates = [(pane, terminal, scopes.get(terminal, {"total": 0, "waiting": 0, "contexts": {}}))
                  for pane, terminal in panes.items()]
    # Recorded waiting first, then other stored context, then proven empty sets.
    for pane_id, terminal_id, scope in sorted(candidates, key=lambda entry: (
            not entry[2]["waiting"], not entry[2]["total"], entry[0])):
        if len(items) + (4 if scope["total"] else 3) > room:
            continue
        root_id = f"panel:{pane_id}:{MESSAGE_VIEW}"
        summary = f"UNRESOLVED · {scope['waiting']} recorded waiting" if scope["total"] else "settled"
        items.append({"id": root_id, "role": "section", "label": "Messages", "detail": summary,
                      "view": {"pane_id": pane_id, "view_id": MESSAGE_VIEW,
                               "state": "in_progress" if scope["waiting"] else "open"}})
        if not scope["total"]:
            items.append({"id": f"panel:{pane_id}:message:empty", "role": "section",
                          "parent_id": root_id, "label": "No messages", "view": {"state": "open"}})
            continue
        items.append({"id": f"panel:{pane_id}:message:context", "role": "section", "parent_id": root_id,
                      "label": "Current execution not established", "detail": f"{scope['total']} relevant stored messages",
                      "view": {"state": "open"}})
        contexts = sorted(scope["contexts"].items(), key=lambda entry: (-entry[1]["waiting"], entry[0]))
        context_ids = {}
        for index, ((role, key), count) in enumerate(contexts[:MESSAGE_LIMIT]):
            if len(items) + 2 >= room:
                break
            items.append({"id": f"panel:{pane_id}:message:identity:{index}", "role": "section", "parent_id": root_id,
                          "label": f"Recorded {role}: {recorded_identity(known[key])}",
                          "detail": f"{count['waiting']} recorded waiting" if role == "recipient" else "Stored sender context",
                          "view": {"state": "open"}})
            context_ids[(role, key)] = items[-1]["id"]
        shown = 0
        for index, row in enumerate(message_rows(messages, terminal_id, known, replied)):
            # A preview belongs under its exact recorded identity; do not strip
            # identity merely to fit another body after context truncation.
            if row["context"] not in context_ids:
                continue
            # Reserve a truthful omission row and the caller's overflow slot.
            if len(items) + 2 >= room:
                break
            item = {"id": f"panel:{pane_id}:message:{index}", "role": "section", "parent_id": context_ids[row["context"]],
                    "label": row["label"], "detail": row["detail"], "view": {"state": row["state"]}}
            if row["since"] is not None:
                item["view"]["since"] = row["since"]
            terminal = panel_terminal(row["counterparty"], (placements or {}).get(
                (row["counterparty"] or {}).get("terminal_id")))
            if terminal is not None:
                item["terminal"] = terminal
            items.append(item)
            shown += 1
        omitted, omitted_contexts = scope["total"] - shown, len(contexts) - len(context_ids)
        if omitted or omitted_contexts:
            items.append({"id": f"panel:{pane_id}:message:omitted", "role": "section", "parent_id": root_id,
                          "label": f"{omitted} message previews omitted · {omitted_contexts} recorded contexts omitted",
                          "view": {"state": "open"}})
    return items


def panel_items(tasks, placements, room, *, empty_only=False):
    """Task slices, or healthy empty placed panes after populated publication.

    Placement scopes presentation only; an empty view asserts no current agent
    identity and grants no action authority. Never replace an omitted task slice
    with an empty root, even when its open rows or publication room run out.
    """
    slices = pane_slices(tasks, placements)
    if empty_only:
        slices = {placement["pane_id"]: {"counts": Counter(), "rows": []}
                  for placement in (placements or {}).values() if placement["pane_id"] not in slices}
    items = []
    unfinished_parents = {task.get("parent") for task in tasks if not final(task)}
    remaining = AFFORDANCES_BYTES
    for pane_id, bucket in sorted(slices.items()):
        if len(items) + 2 > room:
            break
        root_id = f"panel:{pane_id}:{PANEL_VIEW}"
        rows = bucket["rows"][:PANEL_ROW_LIMIT]
        in_slice = {row["task"]["id"] for row in rows}
        items.append({"id": root_id, "role": "section", "label": "Tasks",
                      "detail": summary(bucket["counts"]), "view": {
                          "pane_id": pane_id, "view_id": PANEL_VIEW,
                          "state": "in_progress" if bucket["counts"]["active"] else "open"}})
        if empty_only:
            items.append({"id": f"panel:{pane_id}:task:empty", "role": "section",
                          "parent_id": root_id, "label": "No tasks", "view": {"state": "open"}})
        for row in rows:
            if len(items) + 1 >= room:
                break
            task = row["task"]
            item = {"id": f"panel:{pane_id}:task:{task['id']}", "role": "section",
                    "label": bounded(task["title"]),
                    "view": {"state": row["state"], "waiting": row["waiting"], "action": row["action"]}}
            if row["since"] is not None:
                item["view"]["since"] = row["since"]
            hint = human_affordance(task, unfinished_parents)
            if hint is not None:
                size = len(canonical(hint).encode())
                if size <= remaining:
                    item["view"]["affordance"] = hint
                    remaining -= size
            parent = task.get("parent")
            item["parent_id"] = f"panel:{pane_id}:task:{parent}" if parent in in_slice else root_id
            terminal = panel_terminal(row["counterparty"], row["counterparty_placement"])
            if terminal is not None:
                item["terminal"] = terminal
            items.append(item)
    return items


def attention_row_target(task, placements):
    """The terminal link a sidebar attention entry should carry, or None.

    A click goes to the owner's pane. When the task is waiting on the human there is
    no pane of theirs to go to, so it goes to the pane of the agent that reported the
    task instead, which is where the human usually acts on it. When neither has a live
    placement the entry carries no link. The package supplies execution warnings
    only for agent-owned work; a human does not disappear with their terminal.
    """
    owed_to = task.get("owed_to") or {}
    on_the_human = owed_to.get("class") == "human_unverified" or owed_to.get("role") == "human_unverified"
    candidates = [task.get("creator")] if on_the_human else [task.get("owner"), task.get("creator")]
    for candidate in candidates:
        if candidate is None:
            continue
        terminal_id = candidate.get("terminal_id")
        placement = (placements or {}).get(terminal_id)
        if terminal_id and placement is not None:
            return {"terminal_id": terminal_id, "placement": placement}
    return None


def project(store, snapshot):
    # The provider already owns a read transaction. Direct/test consumers get the
    # same complete header/body snapshot, never separate autocommit observations.
    own_read = store is not None and not store.db.in_transaction
    if own_read:
        store.db.execute("BEGIN")
    try:
        return project_snapshot(store, snapshot)
    finally:
        if own_read:
            store.db.rollback()


def project_snapshot(store, snapshot):
    refs = snapshot.get("terminals")
    placements = None if refs is None else {row["terminal_id"]: row["placement"] for row in refs}
    # No database yet is a fresh empty installation. A present unreadable or
    # unsupported database is rejected by the provider, never silently empty.
    tasks = [] if store is None else [store.task(row[0]) for row in store.db.execute("SELECT id FROM tasks ORDER BY id")]
    items = [{"id": PREFIX + "view:vqro.tasks:tree", "role": "section", "label": "Tasks",
              "detail": "Read-only durable attention · live status in full view"}]
    items.extend({"id": PREFIX + f"measure:{index}", "role": "section", "label": text}
                 for index, text in enumerate(("waiting on you", "blocked", "unavailable")))
    counts, rows = {}, []
    for task in tasks:
        categories = attention(task, placements)
        if not categories:
            continue
        placement = (placements or {}).get((task.get("owner") or {}).get("terminal_id"))
        scopes = [("session", "global")] if not placement else [("workspace", placement["workspace_id"]), ("tab", placement["tab_id"])]
        for scope in scopes:
            counts.setdefault(scope, Counter()).update(categories)
        owner = label(task.get("owner"), None)
        row = {"id": PREFIX + "row:" + task["id"], "role": "section", "label": bounded(task["title"]),
               "detail": bounded(f"{owner} · {task.get('owed_action') or task['state']}"), "flags": {"attention": True}}
        target = attention_row_target(task, placements)
        if target is not None:
            row["terminal"] = target
        elif (task.get("owner") or {}).get("class") == "agent":
            row["detail"] = "not running"
        rows.append(row)
    for (kind, key), value in sorted(counts.items()):
        items.append({"id": PREFIX + f"badge:{kind}:{key}:{value['human']}:{value['blocked']}:{value['unavailable']}",
                      "role": "section", "label": bounded(badge_text(value)), "flags": {"attention": True}})
    room = max(0, 256 - len(items))
    attention_rows = rows[: room // 2]
    items.extend(attention_rows)
    items.extend(panel_items(tasks, placements, max(0, 256 - len(items))))
    evidence = message_evidence(store)
    messages = []
    if store is not None:
        # A current but unreadable message store is a provider error, not an
        # empty inbox. Missing installations were handled above without writes.
        messages = [message_record(row) for row in store.db.execute(
            "SELECT * FROM messages ORDER BY created_at DESC, id DESC LIMIT 200")]
    items.extend(message_panel_items(
        placements, messages, evidence, max(0, 256 - len(items))))
    # Preserve every existing populated task/message slice's priority. Empty
    # Tasks roots use only remaining room, never conceal read/binding failures.
    items.extend(panel_items(tasks, placements,
                             max(0, 256 - len(items) - (len(rows) > len(attention_rows))), empty_only=True))
    if len(rows) > len(attention_rows):
        items.append({"id": PREFIX + "overflow:tasks", "role": "section", "label": f"{len(rows)} more · open full Tasks view"})
    if len(items) > 256:
        raise ValueError("attention scopes exceed document item bound")
    document = {"contract": VIEW_CONTRACT, "namespace": "vqro.tasks", "generation": snapshot["generation"],
                "revision": snapshot["revision"], "surface": "navigation", "target": {"kind": "session"},
                "items": items, "actions": [], "required_capabilities": []}
    if len(canonical(document).encode()) > DOCUMENT_BYTES:
        # Hints must not make an otherwise readable document unavailable.
        for item in items:
            item.get("view", {}).pop("affordance", None)
    if len(canonical(document).encode()) > DOCUMENT_BYTES:
        raise ValueError("document exceeds bounded publication size")
    return document
