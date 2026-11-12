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
from provider import canonical
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


def known_bindings(tasks, messages=()):
    """canonical key -> binding, from the records the extension already owns.

    A message stores its sender and recipient as canonical binding keys, so a
    name and a terminal link need this join: the task records carry their own
    bindings, and each message's keys are bindings too.
    """
    known = {}
    for task in tasks:
        for key in ("owner", "creator", "delegator"):
            binding = task.get(key)
            if isinstance(binding, dict):
                known[canonical(binding)] = binding
    for record in messages:
        for key in ("sender", "recipient"):
            try:
                binding = json.loads(record.get(key) or "null")
            except (TypeError, ValueError):
                continue
            if isinstance(binding, dict):
                known.setdefault(record[key], binding)
    return known


def executions_by_terminal(known):
    by_terminal = {}
    for binding in known.values():
        terminal, execution = binding.get("terminal_id"), binding.get("execution")
        if binding.get("class") == "agent" and terminal and execution:
            by_terminal.setdefault(terminal, execution)
    return by_terminal


def message_subject(record):
    body = " ".join(bounded(record.get("body") or "").split())
    if not body:
        return "(no body)"
    # Internal ids are bookkeeping, not subject matter: "message from task_66c03c"
    # reads as "message about a task" rather than making the reader parse a hash.
    body = INTERNAL_ID.sub(lambda match: f"a {match.group('kind').lower()}", body)
    return body[:MESSAGE_PREVIEW].strip()


def message_rows(records, execution, known):
    """One pane's messages: waiting pickups, claimed messages, missing replies.

    Show and jump only: sending and claiming stay in the CLI.
    """
    def counterpart(key):
        binding = known.get(key)
        if binding is None:
            return None, None
        return binding, label(binding, None)

    rows = []
    for record in records:
        sender_binding, sender_label = counterpart(record.get("sender"))
        recipient_binding, recipient_label = counterpart(record.get("recipient"))
        if record["state"] == "waiting" and record["recipient_execution"] == execution:
            rows.append({"label": f"{sender_label}: {message_subject(record)}",
                         "state": "open", "waiting": "you", "action": "claim",
                         "since": epoch(record.get("created_at")),
                         "counterparty": sender_binding, "kind": "waiting"})
        elif record["state"] in ("claimed", "answered") and record["recipient_execution"] == execution:
            rows.append({"label": f"{sender_label}: {message_subject(record)}",
                         "state": "done", "detail": record["state"],
                         "since": epoch(record.get("updated_at") or record.get("created_at")),
                         "counterparty": sender_binding, "kind": "claimed"})
    replied = {record.get("reply_to") for record in records if record.get("reply_to")}
    mine = {
        canonical(binding)
        for binding in known.values()
        if binding.get("execution") == execution
    }
    for record in records:
        if record.get("sender") not in mine or record["id"] in replied:
            continue
        _, recipient_label = counterpart(record.get("recipient"))
        rows.append({"label": message_subject(record),
                     "state": "reported", "detail": f"no reply to {recipient_label}",
                     "since": epoch(record.get("created_at")),
                     "counterparty": None, "kind": "no-reply"})
    return rows[: MESSAGE_LIMIT * 2]


def message_panel_items(tasks, placements, messages, known, room):
    """One messages view per placed pane, including a healthy empty inbox.

    Placement scopes presentation only. No record-derived execution is needed to
    show an empty view, and an empty row grants no message or activation rights.
    """
    items = []
    by_terminal = executions_by_terminal(known)
    panes = {}
    for terminal_id, placement in sorted((placements or {}).items()):
        panes.setdefault(placement["pane_id"], terminal_id)
    candidates = []
    for pane_id, terminal_id in panes.items():
        execution = by_terminal.get(terminal_id)
        rows = message_rows(messages, execution, known) if execution else []
        candidates.append((pane_id, rows))
    # Empty presentation must not evict existing message-bearing slices when
    # publication is tight. Keep the previous pane order among populated views.
    for pane_id, rows in sorted(candidates, key=lambda entry: (not entry[1], entry[0])):
        if len(items) + 3 > room:
            continue
        waiting = sum(1 for row in rows if row["kind"] == "waiting")
        missing = sum(1 for row in rows if row["kind"] == "no-reply")
        summary = " · ".join(
            part for part in (
                f"{waiting} waiting" if waiting else "",
                f"{missing} no reply" if missing else "",
            ) if part
        ) or "settled"
        root_id = f"panel:{pane_id}:{MESSAGE_VIEW}"
        items.append({"id": root_id, "role": "section", "label": "Messages",
                      "detail": summary,
                      "view": {"pane_id": pane_id, "view_id": MESSAGE_VIEW,
                               "state": "in_progress" if waiting else "open"}})
        if not rows:
            items.append({"id": f"panel:{pane_id}:message:empty", "role": "section",
                          "parent_id": root_id, "label": "No messages",
                          "view": {"state": "open"}})
        for index, row in enumerate(rows):
            if len(items) + 1 >= room:
                break
            item = {"id": f"panel:{pane_id}:message:{index}", "role": "section",
                    "parent_id": root_id, "label": row["label"],
                    "view": {"state": row["state"]}}
            if row.get("action"):
                item["view"]["waiting"] = row["waiting"]
                item["view"]["action"] = row["action"]
            elif row.get("detail"):
                item["detail"] = row["detail"]
            if row.get("since") is not None:
                item["view"]["since"] = row["since"]
            terminal = panel_terminal(row.get("counterparty"), (placements or {}).get(
                (row.get("counterparty") or {}).get("terminal_id")))
            if terminal is not None:
                item["terminal"] = terminal
            items.append(item)
    return items


def panel_items(tasks, placements, room):
    """One panel view per pane with rows, bounded by the remaining item room."""
    items = []
    unfinished_parents = {task.get("parent") for task in tasks if not final(task)}
    remaining = AFFORDANCES_BYTES
    for pane_id, bucket in sorted(pane_slices(tasks, placements).items()):
        if len(items) + 2 > room:
            break
        root_id = f"panel:{pane_id}:{PANEL_VIEW}"
        rows = bucket["rows"][:PANEL_ROW_LIMIT]
        in_slice = {row["task"]["id"] for row in rows}
        items.append({"id": root_id, "role": "section", "label": "Tasks",
                      "detail": summary(bucket["counts"]), "view": {
                          "pane_id": pane_id, "view_id": PANEL_VIEW,
                          "state": "in_progress" if bucket["counts"]["active"] else "open"}})
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
    messages = []
    if store is not None:
        # A current but unreadable message store is a provider error, not an
        # empty inbox. Missing installations were handled above without writes.
        messages = [message_record(row) for row in store.db.execute(
            "SELECT * FROM messages ORDER BY created_at DESC, id DESC LIMIT 200")]
    known = known_bindings(tasks, messages)
    items.extend(message_panel_items(
        tasks, placements, messages, known, max(0, 256 - len(items))))
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
    return document
