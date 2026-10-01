#!/usr/bin/env python3
"""Fixture publisher of extension-owned relationship context over neutral reads.

No runtime delegation records, private sockets/files, task acceptance or launch.
The future work extension, not this demonstration, owns real tasks and threads.
"""
import json
import os
import sys

NAMESPACE = "example.public-relationships"


def project(snapshot):
    members = list(snapshot.get("ungrouped", []))
    for group in snapshot.get("groups", []):
        members.extend(group["members"])
    live = [member for member in members if member["lifecycle"] == "live"]
    items = []
    if live:
        root = "context:task.parent:fixture-task"
        items.append({"id": root, "role": "section",
                      "label": "Fixture task (extension context)", "action_ids": [],
                      "flags": {"expanded": True}})
        for member in live:
            terminal = member["terminal"]
            items.append({"id": "context:task.owner:" + terminal["terminal_id"],
                          "parent_id": root, "role": "terminal",
                          "label": "Owner context", "terminal": terminal,
                          "action_ids": [], "flags": {"expanded": True}})
    return {"contract": "host.document.v1", "namespace": NAMESPACE,
            "generation": snapshot["generation"], "revision": snapshot["revision"],
            "surface": "navigation", "target": {"kind": "tab", "tab_id": snapshot["scope_id"]},
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
