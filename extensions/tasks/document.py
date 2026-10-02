"""Actionless native attention presentation, from Tasks' own durable store.

Terminal placement is a host observation, not execution/acceptance authority.
Live-owner categories deliberately remain in the full terminal view.
"""
from collections import Counter
from view import clean, debtors, human_debtor, label

PREFIX = "context:attention."


def attention(task, placements):
    if task["state"] in ("accepted", "done"):
        return set()
    result = set()
    owed = task.get("owed_to") or {}
    if any(human_debtor(binding) for binding in debtors(owed)) or task.get("owed_action") == "answer":
        result.add("human")
    if task["state"] == "blocked" or task.get("owed_action") == "acceptor_missing":
        result.add("blocked")
    if placements is not None:
        owner = task.get("owner") or {}
        if owner.get("terminal_id") and owner["terminal_id"] not in placements:
            result.add("unavailable")
        bindings = debtors(owed) if owed.get("role") == "acceptors" else []
        if bindings and all(binding.get("class") == "agent" and binding.get("terminal_id") not in placements for binding in bindings):
            result.add("unavailable")
    return result


def badge_text(counts):
    return " · ".join(f"{counts[key]} {text}" for key, text in
                      (("human", "waiting on you"), ("blocked", "blocked"), ("unavailable", "unavailable")) if counts[key])


def bounded(text):
    return clean(text).encode("utf-8")[:240].decode("utf-8", errors="ignore")


def project(store, snapshot):
    refs = snapshot.get("terminals")
    placements = None if refs is None else {row["terminal_id"]: row["placement"] for row in refs}
    tasks = [store.task(row[0]) for row in store.db.execute("SELECT id FROM tasks ORDER BY id")]
    items = [{"id": PREFIX + "view:vqro.tasks:tree", "role": "section", "label": "Tasks",
              "detail": "Read-only durable attention · live status in full view"}]
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
    room = max(0, 256 - len(items) - 1)
    items.extend(rows[:room])
    if len(rows) > room:
        items.append({"id": PREFIX + "overflow:tasks", "role": "section", "label": f"{len(rows) - room} more · open full Tasks view"})
    if len(items) > 256:
        raise ValueError("attention scopes exceed document item bound")
    return {"contract": "host.document.v1", "namespace": "vqro.tasks.core", "generation": snapshot["generation"],
            "revision": snapshot["revision"], "surface": "navigation", "target": {"kind": "session"},
            "items": items, "actions": [], "required_capabilities": []}
