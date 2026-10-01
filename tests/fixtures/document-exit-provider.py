#!/usr/bin/env python3
"""Real stdio document provider for exit/retirement regressions (private control root)."""
import json
import os
from pathlib import Path
import sys
import threading
import time

namespace, control_root = sys.argv[1:]
root = Path(control_root)


def watch_exit():
    while not (root / "exit").exists():
        time.sleep(0.01)
    os._exit(0)


threading.Thread(target=watch_exit, daemon=True).start()
print(json.dumps({"type": "hello", "protocol": 2, "service_id": namespace,
                  "requested_capabilities": ["host_document"],
                  "methods": ["host.document.render", "host.document.action"]}), flush=True)
for line in sys.stdin:
    request = json.loads(line)
    if request.get("type") == "welcome":
        continue
    if request.get("method") == "host.document.render":
        (root / "render-started").touch()
        while (root / "hold-render").exists():
            time.sleep(0.01)
        if (root / "die-on-render").exists():
            os._exit(0)
        snapshot = request["params"]
        document = {"contract": "host.document.v1", "namespace": namespace,
                    "generation": snapshot["generation"], "revision": snapshot["revision"],
                    "surface": "navigation", "target": {"kind": "tab", "tab_id": snapshot["scope_id"]},
                    "items": [{"id": "context:task.parent:published", "role": "section", "label": "Live context"}],
                    "actions": [], "required_capabilities": []}
        print(json.dumps({"type": "response", "id": request["id"], "result": document}), flush=True)
    elif request.get("method") == "host.document.action":
        (root / "action-ran").touch()
        print(json.dumps({"type": "response", "id": request["id"], "result": {}}), flush=True)
