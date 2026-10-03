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
OWED_TEXT = {
    "start": "start",
    "work/report": "report",
    "unblock": "unblock",
    "accept": "accept",
    "answer": "answer",
    "acceptor_missing": "no acceptor",
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


def open_tasks(tasks):
    """Tasks that still owe someone something, oldest debt first."""
    pending = [task for task in tasks if task["state"] not in ("accepted", "done")]
    return sorted(pending, key=lambda task: (task["state"] != "blocked", task.get("owed_since") or task.get("updated_at") or ""))


def pane_rows(tasks, placements):
    """Per-pane rows: the agent in a pane owes, and waits for, these tasks.

    A pane's slice is keyed by the pane its agent's terminal is placed in. Each
    task appears in its owner's pane as a debt, and in the creditor's pane as
    something they are waiting for. No placement, no row.
    """
    by_id = {task["id"]: task for task in tasks}
    slices = {}
    for task in open_tasks(tasks):
        owner = task.get("owner") or {}
        owed_text = OWED_TEXT.get(task.get("owed_action") or "", "work")
        owner_placement = (placements or {}).get(owner.get("terminal_id"))
        creditor = recipient(task, by_id) or {}
        creditor_placement = (placements or {}).get(creditor.get("terminal_id"))
        row = {
            "task": task,
            "state": task["state"],
            "chip": CHIP_TEXT.get(task["state"], task["state"]),
            "tone": CHIP_TONE.get(task["state"], "muted"),
            "since": epoch(task.get("owed_since") or task.get("updated_at")),
        }
        if owner_placement:
            slices.setdefault(owner_placement["pane_id"], []).append({
                **row, "detail": f"you owe {owed_text}",
                "counterparty": creditor, "counterparty_placement": creditor_placement})
        if creditor_placement and creditor_placement["pane_id"] != (owner_placement or {}).get("pane_id"):
            slices.setdefault(creditor_placement["pane_id"], []).append({
                **row, "detail": f"owes {owed_text}",
                "counterparty": owner, "counterparty_placement": owner_placement})
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
    for pane_id, rows in sorted(pane_rows(tasks, placements).items()):
        if len(items) + 2 > room:
            break
        root_id = f"panel:{pane_id}:{PANEL_VIEW}"
        total = len(rows)
        rows = rows[:PANEL_ROW_LIMIT]
        in_slice = {row["task"]["id"] for row in rows}
        items.append({"id": root_id, "role": "section", "label": "Tasks", "view": {
            "pane_id": pane_id, "view_id": PANEL_VIEW, "edge": "right", "count": total}})
        for row in rows:
            if len(items) + 1 >= room:
                break
            task = row["task"]
            item = {"id": f"panel:{pane_id}:task:{task['id']}", "role": "section",
                    "label": bounded(task["title"]), "detail": bounded(row["detail"]),
                    "view": {"chip": {"text": row["chip"], "tone": row["tone"]}}}
            if row["since"] is not None:
                item["view"]["since"] = row["since"]
            parent = task.get("parent")
            item["parent_id"] = (f"panel:{pane_id}:task:{parent}" if parent in in_slice else root_id)
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
