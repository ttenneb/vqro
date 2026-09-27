#!/usr/bin/env python3
"""Minimal vqro.extension-service.v1 host.document provider."""
import json
import os
import sys

SERVICE_ID = os.environ["VQRO_EXTENSION_SERVICE_ID"]
print(json.dumps({
    "type": "hello",
    "protocol": 2,
    "service_id": SERVICE_ID,
    "requested_capabilities": ["host_document", "focus_terminal"],
    "methods": ["host.document.render"],
}), flush=True)

welcome = json.loads(sys.stdin.readline())
if welcome.get("type") != "welcome":
    raise SystemExit(1)


def member_item(member, parent=None, selected=False):
    terminal = member["terminal"]
    terminal_id = terminal["terminal_id"]
    item_id = "terminal:" + terminal_id
    return ({
        "id": item_id,
        **({"parent_id": parent} if parent else {}),
        "role": "terminal",
        "label": member["label"],
        "detail": member["lifecycle"],
        "terminal": terminal,
        "action_ids": ["focus:" + terminal_id],
        "flags": {"selected": selected, "expanded": False, "disabled": False, "attention": False},
    }, {
        "id": "focus:" + terminal_id,
        "label": "Focus terminal",
        "operation": {"kind": "host", "effect": {"kind": "focus_terminal", "terminal_id": terminal_id}},
        "subject_id": item_id,
        "confirmation": "none",
    })


for line in sys.stdin:
    request = json.loads(line)
    if request.get("type") != "request" or request.get("method") != "host.document.render":
        print(json.dumps({"type": "response", "id": request.get("id", 0), "error": "unsupported method"}), flush=True)
        continue
    snapshot = request["params"]
    items, actions = [], []
    for group in snapshot.get("groups", []):
        group_id = "group:" + group["group_id"]
        items.append({
            "id": group_id, "role": "group", "label": group.get("label") or "Group",
            "detail": f'{len(group["members"])} terminals', "action_ids": [],
            "flags": {"selected": False, "expanded": True, "disabled": False, "attention": False},
        })
        for member in group["members"]:
            item, action = member_item(member, group_id, member["terminal"]["terminal_id"] == group.get("selected_terminal_id"))
            items.append(item); actions.append(action)
    for member in snapshot.get("ungrouped", []):
        item, action = member_item(member)
        items.append(item); actions.append(action)
    document = {
        "contract": "host.document.v1",
        "namespace": "example.reference-document",
        "generation": snapshot["generation"],
        "revision": snapshot["revision"],
        "surface": "navigation",
        "target": {"kind": "tab", "tab_id": snapshot["scope_id"]},
        "items": items,
        "actions": actions,
        "required_capabilities": ["focus_terminal"],
    }
    print(json.dumps({"type": "response", "id": request["id"], "result": document}), flush=True)
