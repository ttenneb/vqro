"""Native attention and pane panels, from Tasks' own durable store.

Terminal placement is a host observation, not execution/acceptance authority.
Live owner status is joined by Vqro when it draws, so this module publishes the
durable meaning only: which task, who owes what, and since when.
"""
import datetime
from collections import Counter
from view import clean, debtors, durable_attention as attention, label, recipient

PREFIX = "context:attention."
VIEW_CONTRACT = "host.view.v1"
PANEL_VIEW = "tasks"
PANEL_ROW_LIMIT = 24
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
            entries.append((creditor_placement["pane_id"], "them", owed, owner, owner_placement))
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


def panel_items(tasks, placements, room):
    """One panel view per pane with rows, bounded by the remaining item room."""
    items = []
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
            parent = task.get("parent")
            item["parent_id"] = f"panel:{pane_id}:task:{parent}" if parent in in_slice else root_id
            terminal = panel_terminal(row["counterparty"], row["counterparty_placement"])
            if terminal is not None:
                item["terminal"] = terminal
            items.append(item)
    return items


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
        if placement:
            row["terminal"] = {"terminal_id": task["owner"]["terminal_id"], "placement": placement}
        rows.append(row)
    for (kind, key), value in sorted(counts.items()):
        items.append({"id": PREFIX + f"badge:{kind}:{key}:{value['human']}:{value['blocked']}:{value['unavailable']}",
                      "role": "section", "label": bounded(badge_text(value)), "flags": {"attention": True}})
    room = max(0, 256 - len(items))
    attention_rows = rows[: room // 2]
    items.extend(attention_rows)
    items.extend(panel_items(tasks, placements, max(0, 256 - len(items))))
    if len(rows) > len(attention_rows):
        items.append({"id": PREFIX + "overflow:tasks", "role": "section", "label": f"{len(rows)} more · open full Tasks view"})
    if len(items) > 256:
        raise ValueError("attention scopes exceed document item bound")
    return {"contract": VIEW_CONTRACT, "namespace": "vqro.tasks", "generation": snapshot["generation"],
            "revision": snapshot["revision"], "surface": "navigation", "target": {"kind": "session"},
            "items": items, "actions": [], "required_capabilities": []}
