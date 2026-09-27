#!/usr/bin/env python3
import json, os, sys
print(json.dumps({"type":"hello","protocol":2,"service_id":os.environ["VQRO_EXTENSION_SERVICE_ID"],"requested_capabilities":["action_choices"],"methods":["host.action.choices.v1","host.action.choice.invoke.v1"]}), flush=True)
for line in sys.stdin:
    message = json.loads(line)
    if message.get("type") == "welcome":
        continue
    print(json.dumps({"type":"response","id":message["id"],"result":{"context_revision":1,"choices":[]}}), flush=True)
