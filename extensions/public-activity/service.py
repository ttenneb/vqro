#!/usr/bin/env python3
"""Trusted example: all runtime facts arrive through the public service pipe.

No runtime files, network or socket access. Group names, order and role labels
are presentation owned here, not in the host. Metadata is display-only.
"""
import json
import os
import sys

NAMESPACE = "example.public-activity"


def project(snapshot):
    items = []
    for state in ("blocked", "working", "idle", "unknown"):
        records = [row for row in snapshot["activities"] if row["activity"] == state]
        if not records:
            continue
        group = "activity:" + state
        items.append({"id": group, "role": "group", "label": state.title(),
                      "flags": {"expanded": True}, "action_ids": []})
        for row in sorted(records, key=lambda row: (-row["state_change_seq"], row["terminal"]["terminal_id"])):
            labels = row.get("labels", {})
            label = labels.get("display") or labels.get("agent") or "Agent"
            role_status = labels.get("state_" + state)
            if role_status:
                label += " · " + role_status
            # UTF-8 cap below the host's text bound, without splitting characters.
            label = label.encode("utf-8")[:240].decode("utf-8", errors="ignore")
            items.append({"id": "terminal:" + row["terminal"]["terminal_id"],
                          "parent_id": group, "role": "terminal", "label": label,
                          "detail": "Attributed by " + row["source"],
                          "terminal": row["terminal"], "action_ids": [],
                          "flags": {"attention": state == "blocked"}})
    return {"contract": "host.document.v1", "namespace": NAMESPACE,
            "generation": snapshot["generation"], "revision": snapshot["revision"],
            "surface": "navigation", "target": {"kind": "session"},
            "items": items, "actions": [], "required_capabilities": []}


def main():
    print(json.dumps({"type": "hello", "protocol": 2,
                      "service_id": os.environ["VQRO_EXTENSION_SERVICE_ID"],
                      "requested_capabilities": ["host_document"],
                      "methods": ["host.document.render"]}), flush=True)
    if json.loads(sys.stdin.readline()).get("type") != "welcome":
        raise SystemExit(1)
    for line in sys.stdin:
        request = json.loads(line)
        reply = {"type": "response", "id": request.get("id", 0)}
        if request.get("type") != "request" or request.get("method") != "host.document.render":
            reply["error"] = "unsupported method"
        else:
            reply["result"] = project(request["params"])
        print(json.dumps(reply), flush=True)


if __name__ == "__main__":
    main()
