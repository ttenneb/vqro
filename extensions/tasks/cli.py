#!/usr/bin/env python3
"""Tasks-owned CLI: syntax, output, exact bindings and uncertain-write recovery.

The neutral host foreground-execs this contribution. Caller authority is derived
freshly by the host from the API peer, never from this process's environment.
"""
import json
import os
import socket
import sys
import time
from messages import ACTIVATION_OFF_DETAIL, ACTIVATION_OFF_LABEL

SERVICE = "vqro.tasks.core"
TASK_USAGE = "usage: vqro task <create/decision TITLE --done-when TEXT [--parent ID]|plan FILE [--parent ID --rev N]|depends ID --on JSON --rev N|update ID --rev N [metadata options]|delegate ID --to NAME (or --owner JSON) --rev N|start/accept/done ID --rev N [done --evidence TEXT]|block/report/answer ID TEXT --rev N|revert ID --rev N|decision-timer ID --rev N --auto-at UTC|decisions|decisions-seen --through SEQ|decision-log [--after SEQ]|list/ready [--after ID]|tree|whoami|get ID|op ID [--caller JSON]>; decision requires --options JSON --default KEY --reversible BOOL --waiting-cost TEXT [--auto-at UTC]; explicit task --op requires --caller JSON from the original attempt; --json forces JSON"
MESSAGE_USAGE = "usage: vqro message <send TARGET TEXT [--reply-to ID]|list|claim [--idle]|edit ID TEXT --rev N|activation on|off> [--op ID] [--caller JSON] [--json]; explicit --op requires --caller from the original attempt; TARGET is a detected agent name, pane id, or an exact agent binding JSON {\"class\":\"agent\",\"terminal_id\":...,\"agent\":...,\"execution\":...}"
MUTATIONS = {"create", "decision", "delegate", "start", "block", "report", "accept", "done", "answer", "decision_seen", "decision_timer", "decision_revert", "plan", "depends", "update"}


ID_USAGE = "IDs accept exact values or unique prefixes (task_/msg_ optional, minimum 6 hex characters); ambiguous prefixes list candidates. Writes still require the inspected --rev."


def encoded(value):
    return json.dumps(value, ensure_ascii=True, separators=(",", ":"))


def visible(value):
    # JSON string escaping also removes terminal controls and bidi characters.
    return encoded(str(value))[1:-1]


def api(method, params=None, request_id="cli:tasks", timeout=6):
    path = os.environ.get("VQRO_SOCKET_PATH")
    if not path:
        raise OSError("VQRO_SOCKET_PATH unavailable; invoke this contribution through vqro")
    frame = (encoded({"id": request_id, "method": method, "params": params or {}}) + "\n").encode()
    if os.name == "nt":
        from windows_api import request
        return json.loads(request(path, frame, timeout))
    deadline = time.monotonic() + timeout
    def remaining():
        left = deadline - time.monotonic()
        if left <= 0:
            raise TimeoutError("API deadline elapsed; submitted writes may have committed")
        return left
    with socket.socket(socket.AF_UNIX) as stream:
        stream.settimeout(remaining())
        stream.connect(path)
        stream.settimeout(remaining())
        stream.sendall(frame)
        data = bytearray()
        while b"\n" not in data:
            stream.settimeout(remaining())
            part = stream.recv(65536)
            if not part:
                raise OSError("API closed without reply")
            data.extend(part)
            if len(data) > 1024 * 1024:
                raise OSError("API reply exceeds CLI bound")
    return json.loads(data.partition(b"\n")[0])


def invoke(payload):
    return api("extension.action.invoke", {"service_id": SERVICE, "payload": payload})


def decode_reply(response):
    if not isinstance(response, dict):
        return "unknown", "invalid task outcome"
    if "error" in response:
        error = response["error"]
        if not isinstance(error, dict):
            return "unknown", "invalid action error"
        return ("rejected" if error.get("code") in ("action_rejected", "invalid_request", "caller_unknown") else "unknown", error.get("message", "invalid action error"))
    result = response.get("result")
    if not isinstance(result, dict) or result.get("type") != "extension_action_invoked":
        return "unknown", "unexpected action response"
    result = result.get("value")
    if isinstance(result, dict):
        if result.get("ok") is True and "value" in result:
            return "success", result["value"]
        if result.get("ok") is False and isinstance(result.get("error"), str):
            return "rejected", result["error"]
    return "unknown", "invalid task outcome"


def resolve_recipient(target):
    # An exact agent binding is passed through as given: the durable record is the
    # authority, only that exact execution can claim the message later, and the
    # provider revalidates the binding's exact shape before any write.
    if target.startswith("{"):
        try:
            return json.loads(target)
        except ValueError as error:
            raise ValueError(f"recipient binding is not valid JSON: {error}") from error
    response = api("agent.get", {"target": target}, "cli:message:resolve", 5)
    if "error" in response:
        raise ValueError(response["error"].get("message", "agent not found"))
    agent = response.get("result", {}).get("agent", {})
    if not all(isinstance(agent.get(key), str) and agent[key] for key in ("terminal_id", "agent", "execution")):
        raise ValueError("recipient is not a detected agent process with a provable execution")
    return {"class": "agent", **{key: agent[key] for key in ("terminal_id", "agent", "execution")}}


def resolve_owner_names(payload, resolve=resolve_recipient):
    owners = [payload] if payload["action"] == "delegate" else payload.get("nodes", []) if payload["action"] == "plan" else []
    for node in owners:
        if isinstance(node, dict) and isinstance(node.get("owner"), str):
            name = node["owner"]
            try:
                node["owner"] = resolve(name)
            except (ValueError, OSError) as error:
                raise ValueError(f"cannot resolve owner {name}: {error}") from error


def unsigned(value, message, positive=False):
    if not value or not value.isascii() or not value.isdecimal() or int(value) > 2**64 - 1:
        raise ValueError(message)
    number = int(value)
    if positive and not number:
        raise ValueError("--rev must be positive")
    return number


def read_plan(path):
    try:
        with open(path, "rb") as stream:
            data = stream.read(64 * 1024 + 1)
    except OSError as error:
        raise ValueError(f"cannot read plan: {error}") from error
    if len(data) > 64 * 1024:
        raise ValueError("plan exceeds 64 KiB request size")
    try:
        plan = json.loads(data)
    except (ValueError, UnicodeError) as error:
        raise ValueError(f"invalid plan JSON: {error}") from error
    if not isinstance(plan, dict) or set(plan) != {"nodes"} or not isinstance(plan["nodes"], list):
        raise ValueError("plan file must contain an object with only nodes")
    return plan["nodes"]


def parse_payload(args):
    if not args:
        raise ValueError(TASK_USAGE)
    action = {"decisions": "decision_digest", "decisions-seen": "decision_seen", "decision-log": "decision_log", "decision-timer": "decision_timer", "revert": "decision_revert"}.get(args[0], args[0])
    payload = {"action": action}
    mutation = action in MUTATIONS
    if action in ("list", "ready", "whoami", "decision_digest", "decision_seen", "decision_log"):
        remaining = args[1:]
    elif action == "plan" and len(args) >= 2:
        payload["nodes"] = read_plan(args[1]); remaining = args[2:]
    elif action in ("block", "report", "answer") and len(args) >= 3:
        payload.update(id=args[1], text=args[2]); remaining = args[3:]
    elif action == "get" and len(args) == 2:
        payload["id"] = args[1]; remaining = []
    elif action in ("decision_timer", "decision_revert", "create", "decision", "delegate", "start", "accept", "done", "depends", "update", "op") and len(args) >= 2:
        payload["title" if action in ("create", "decision") else "operation_id" if action == "op" else "id"] = args[1]
        remaining = args[2:]
    else:
        raise ValueError(TASK_USAGE)
    if len(remaining) % 2:
        raise ValueError(TASK_USAGE)
    seen = set()
    for flag, value in zip(remaining[::2], remaining[1::2]):
        if flag in seen:
            raise ValueError("duplicate option")
        seen.add(flag)
        key = flag.removeprefix("--").replace("-", "_")
        if action == "decision" and flag == "--options":
            payload["options"] = json.loads(value)
        elif action == "decision" and flag in ("--default", "--waiting-cost"):
            payload[key] = value
        elif action == "decision" and flag == "--reversible":
            if value not in ("true", "false"):
                raise ValueError("--reversible must be true or false")
            payload[key] = value == "true"
        elif action in ("decision", "decision_timer") and flag == "--auto-at":
            payload[key] = value or None
        elif action == "answer" and flag == "--note":
            payload[key] = value
        elif (action, flag) in (("decision_seen", "--through"), ("decision_log", "--after")):
            payload[key] = unsigned(value, "decision sequence/revision must be an unsigned integer")
        elif action in ("create", "decision", "plan") and flag == "--parent":
            payload[key] = value
        elif action == "done" and flag == "--evidence":
            payload["text"] = value
        elif (action == "depends" and flag == "--on") or (action in ("create", "decision") and flag == "--depends-on"):
            payload["depends_on"] = json.loads(value)
            if not isinstance(payload["depends_on"], list):
                raise ValueError("dependencies must be a JSON list")
        elif action in ("create", "decision", "update") and flag in ("--done-when", "--brief-path", "--due-at"):
            payload[key] = value if value or flag == "--done-when" else None
        elif action in ("create", "decision", "update") and flag in ("--estimate-work-ms", "--estimate-wait-ms", "--actual-work-ms", "--actual-wait-ms"):
            payload[key] = unsigned(value, "task timing must be nonnegative integer milliseconds")
        elif mutation and flag == "--op":
            payload["operation_id"] = value
        elif mutation and flag == "--caller":
            payload["expected_caller"] = json.loads(value)
        elif mutation and action not in ("create", "decision") and flag == "--rev":
            payload["revision"] = unsigned(value, "--rev must be a positive integer", True)
        elif action == "delegate" and flag in ("--to", "--owner"):
            if "owner" in payload:
                raise ValueError("use only one of --to NAME or --owner JSON")
            payload["owner"] = value if flag == "--to" else json.loads(value)
        elif action in ("list", "ready") and flag == "--after":
            payload[key] = value
        elif action == "op" and flag == "--caller":
            payload["lookup_caller"] = json.loads(value)
        else:
            raise ValueError(TASK_USAGE)
    if mutation and "operation_id" in payload and "expected_caller" not in payload:
        raise ValueError("an explicit --op requires --caller with the original attempt's binding (use task whoami for a fresh explicit ID)")
    if mutation and action not in ("create", "decision", "decision_seen") and (action != "plan" or "parent" in payload) and "revision" not in payload:
        raise ValueError("--rev is required; inspect the task before writing")
    if action in ("create", "decision") and "done_when" not in payload and "operation_id" not in payload:
        raise ValueError("--done-when is required: 1-3 observable lines")
    if action == "decision" and "operation_id" not in payload and any(key not in payload for key in ("options", "default", "reversible", "waiting_cost")):
        raise ValueError("decision requires --options, --default, --reversible and --waiting-cost; --auto-at is optional")
    for expected, field, message in (("decision_seen", "through", "--through is required: explicitly acknowledge the observed sequence"), ("decision_timer", "auto_at", "--auto-at is required: UTC deadline or empty to cancel"), ("depends", "depends_on", "--on is required: JSON list of prerequisite task IDs"), ("delegate", "owner", "--to NAME or --owner JSON is required")):
        if action == expected and field not in payload:
            raise ValueError(message)
    if action == "plan" and "revision" in payload and "parent" not in payload:
        raise ValueError("plan --rev selects an existing --parent")
    return payload, mutation


def parse_message(args):
    if not args:
        raise ValueError(MESSAGE_USAGE)
    command = args[0]
    if command in ("list", "claim"):
        payload = {"action": "message_" + command}; remaining = args[1:]
    elif command in ("send", "edit") and len(args) >= 3:
        payload = {"action": "message_" + command, "body": args[2], "target" if command == "send" else "id": args[1]}; remaining = args[3:]
    elif command == "activation" and len(args) >= 2 and args[1] in ("on", "off"):
        payload = {"action": "message_activation", "activation": args[1] == "on"}; remaining = args[2:]
    else:
        raise ValueError(MESSAGE_USAGE)
    index = 0
    while index < len(remaining):
        flag = remaining[index]
        if flag == "--idle" and command == "claim":
            payload["idle"] = True; index += 1; continue
        if index + 1 >= len(remaining):
            raise ValueError(f"{flag} requires a value")
        value = remaining[index + 1]
        if flag == "--op" and command != "list":
            if not value: raise ValueError("--op id must not be empty")
            payload["operation_id"] = value
        elif flag == "--caller" and command != "list":
            payload["expected_caller"] = json.loads(value)
        elif flag == "--reply-to" and command == "send":
            payload["reply_to"] = value
        elif flag == "--rev" and command == "edit":
            payload["revision"] = unsigned(value, "--rev requires a message revision number")
        else:
            raise ValueError(f"unknown option: {flag}")
        index += 2
    if "operation_id" in payload and "expected_caller" not in payload:
        raise ValueError("an explicit --op requires --caller with the original attempt's binding (use task whoami for a fresh explicit ID)")
    if command == "edit" and "revision" not in payload:
        raise ValueError("edit requires the inspected --rev revision")
    return payload, command != "list"


def output_options(kind, args):
    args = list(args)
    forced = bool(args and args[0] == "--json")
    if forced: args.pop(0)
    if not args: return args, forced
    minimum = ({"send": 3, "edit": 3, "activation": 2} if kind == "message" else {"block": 3, "report": 3, "answer": 3, **dict.fromkeys(("create", "decision", "plan", "get", "op", "delegate", "start", "accept", "done", "depends", "update", "revert", "decision-timer"), 2)}).get(args[0], 1)
    kept, index = args[:minimum], minimum
    while index < len(args):
        if args[index] == "--json":
            forced = True; index += 1
        else:
            count = 1 if args[index] == "--idle" else 2
            kept.extend(args[index:index + count]); index += count
    return kept, forced


def render_tree(tasks):
    records, children = {}, {}
    for task in tasks:
        task_id, parent = task.get("id"), task.get("parent")
        if not isinstance(task_id, str): raise ValueError("missing task id")
        if task_id in records: raise ValueError("duplicate task id")
        if parent is not None and not isinstance(parent, str): raise ValueError("invalid parent")
        records[task_id] = task; children.setdefault(parent, []).append(task_id)
    if any(parent is not None and parent not in records for parent in children):
        raise ValueError("missing parent; tree may have changed during paginated read")
    for values in children.values(): values.sort()
    stack = [(task_id, 0) for task_id in reversed(children.get(None, []))]
    visited, lines = set(), []
    while stack:
        task_id, depth = stack.pop()
        if task_id in visited: raise ValueError("task cycle")
        visited.add(task_id); task = records[task_id]; indent = "  " * min(depth, 16)
        note = f" (depth {depth})" if depth > 16 else ""
        lines.append(f"{indent}{visible(task_id)} [{visible(task.get('kind'))} / {visible(task.get('state'))} r{task.get('revision')}]{note} {visible(task.get('title'))}")
        lines.append(f"{indent}  owner {encoded(task.get('owner'))} | creator {encoded(task.get('creator'))} | owes {encoded(task.get('owed_to'))} {encoded(task.get('owed_action'))} since {encoded(task.get('owed_since'))}")
        stack.extend((child, depth + 1) for child in reversed(children.get(task_id, [])))
    if len(visited) != len(records): raise ValueError("task cycle or disconnected parent graph")
    return "\n".join(lines) + "\n" if tasks else "(no tasks)\n"


def tree_read():
    tasks, cursor = [], None
    while True:
        payload = {"action": "list"}
        if cursor: payload["after"] = cursor
        status, value = decode_reply(invoke(payload))
        if status != "success": raise ValueError(f"task tree unavailable: {value}")
        rows = value.get("tasks")
        if not isinstance(rows, list): raise ValueError("invalid task page")
        tasks.extend(rows)
        if len(tasks) > 10000: raise ValueError("task tree exceeds extension quota")
        next_cursor = value.get("next_cursor")
        if not isinstance(next_cursor, str): break
        if not rows or (cursor is not None and next_cursor <= cursor): raise ValueError("non-progressing task cursor")
        cursor = next_cursor
    render_tree(tasks)  # validate even JSON output; never present a torn graph as healthy
    return {"tasks": tasks}


def render_decisions(value):
    pending, automatic = value.get("needs_you_now", []), value.get("auto_since_look", [])
    counts, more = value.get("counts", {}), value.get("more", {})
    lines = [f"Decisions: {len(pending)}{'+' if more.get('needs_you_now') else ''} pending; {len(automatic)}{'+' if more.get('auto_since_look') else ''} auto since last look. Total auto {counts.get('auto')}, reverted {counts.get('revert')}."]
    if pending:
        task = pending[0]; lines.append(f"Oldest waiting: {visible(task.get('created_at', 'unknown'))} {visible(task['title'])} ({visible(task['id'])})")
    for task in pending:
        lines.append(f"{visible(task['id'])}: {visible(task['title'])}")
        decision = task.get("decision")
        if decision is None:
            lines.append("  Legacy: human free-text answer; no automatic default."); continue
        timer = "manual" if decision.get("auto_at") is None else f"auto at {visible(decision['auto_at'])} (no on-time guarantee)"
        lines.append(f"  Default {visible(decision['default'])} | {'reversible' if decision['reversible'] else 'irreversible'} | {timer}")
        lines.append("  Options: " + "; ".join(f"{visible(option['key'])}={visible(option['label'])}" for option in decision["options"]))
        lines.append("  Waiting cost: " + visible(decision["waiting_cost"]))
    for task in automatic:
        result = task["result"]; lines.append(f"Auto {visible(task['id'])}: {visible(result['option'])} at {visible(result['applied_at'])} (applied late by {result['late_ms']} ms)")
    lines.append(f"Explicit last look: vqro task decisions-seen --through {value.get('ack_through')}" if value.get("last_look_known") else "Last-look UID unknown here; run the digest/acknowledgement from a human shell.")
    return "\n".join(lines)


def human_output(value, short_ids=None):
    if not isinstance(value, dict): return encoded(value)
    if "task" in value: return render_tree([dict(value["task"], parent=None)]).rstrip()
    if "tasks" in value and isinstance(value["tasks"], list):
        # A page/list need not contain all parents, unlike the full tree.
        return "\n".join(f"{visible((short_ids or {}).get(task['id'], task['id']))} [{visible(task.get('state'))} r{task.get('revision')}] {visible(task.get('title'))}" for task in value["tasks"]) or "(no tasks)"
    if "message" in value: messages = [value["message"]]
    elif "claimed" in value: messages = value["claimed"]
    elif "waiting" in value: messages = value["waiting"] + value.get("messages", [])
    else: return "\n".join(f"{visible(key)}: {encoded(item)}" for key, item in value.items())
    return "\n".join(f"{visible(message['id'])} [{visible(message.get('state'))} r{message.get('revision')}] {visible(message['body'])}" for message in messages) or "(no messages)"


def execute(payload, mutation, call=invoke, resolve=resolve_recipient):
    """Return outcome status/value, preserving original scope on uncertain writes."""
    try:
        resolve_owner_names(payload, resolve)
        if "target" in payload:
            payload["recipient"] = resolve(payload.pop("target"))
    except (ValueError, OSError) as error:
        return "rejected", f"{error}; no write attempted"
    original = None
    if mutation:
        if "operation_id" not in payload:
            # Failure to obtain entropy is a pre-effect error, never a guessed ID.
            payload["operation_id"] = ("msg-op-" if payload["action"].startswith("message_") else "") + os.urandom(16).hex()
        try:
            status, identity = decode_reply(call({"action": "whoami"}))
            original = identity.get("caller") if status == "success" and isinstance(identity, dict) else None
        except (OSError, ValueError):
            original = None
        if not isinstance(original, dict) or original.get("class") not in ("agent", "human_unverified"):
            return "rejected", "task caller unavailable; no write attempted"
        if "expected_caller" in payload and payload["expected_caller"] != original:
            return "rejected", "original caller changed; no write attempted; inspect the original operation scope"
        payload["expected_caller"] = original
    try:
        status, value = decode_reply(call(payload))
    except (OSError, ValueError) as error:
        status, value = "unknown", str(error)
    if status == "unknown" and mutation:
        op = payload["operation_id"]
        value = f"outcome unknown; op {op}\noriginal caller: {encoded(original)}\nlookup first: vqro task op {op} --caller '{encoded(original)}'"
    elif status == "unknown":
        value = f"task read unavailable: {value}"
    if status == "success" and payload["action"] == "op" and value.get("authoritative") is not True:
        return "unscoped", value
    return status, value


def main(args=None):
    args = list(sys.argv[1:] if args is None else args)
    if not args or args[0] not in ("task", "message"):
        print(TASK_USAGE, file=sys.stderr); return 2
    kind = args.pop(0)
    args, forced = output_options(kind, args)
    if not args or args[0] in ("help", "--help", "-h") or (len(args) == 2 and args[1] in ("--help", "-h")):
        print((TASK_USAGE if kind == "task" else MESSAGE_USAGE) + "\n" + ID_USAGE); return 0 if args else 2
    json_output = forced or not sys.stdout.isatty()
    try:
        if kind == "task" and args[0] == "tree":
            if len(args) != 1: raise ValueError("usage: vqro task tree [--json]")
            value = tree_read()
            print(encoded(value) if json_output else render_tree(value["tasks"]), end="\n" if json_output else "")
            return 0
        payload, mutation = parse_payload(args) if kind == "task" else parse_message(args)
        status, value = execute(payload, mutation)
        if status in ("success", "unscoped"):
            if payload["action"] == "message_list" and value.get("activation") is False:
                # Keep JSON stdout intact for adapters and scripts; the same
                # human-visible notice is available in redirected/TTY output.
                print(f"{ACTIVATION_OFF_LABEL}. {ACTIVATION_OFF_DETAIL}", file=sys.stderr)
            short_ids = None
            if not json_output and payload["action"] in ("list", "ready"):
                from ids import short_forms
                # Resolve against the whole durable namespace, not just this page.
                # Failure of this optional presentation read keeps full IDs usable.
                try:
                    short_ids = short_forms(task["id"] for task in tree_read()["tasks"])
                except (OSError, ValueError):
                    pass
            print(encoded(value) if json_output else render_decisions(value) if payload["action"] == "decision_digest" else human_output(value, short_ids))
            if status == "unscoped":
                print("not an authoritative result for the original caller; use --caller with its binding", file=sys.stderr); return 2
            return 0
        print(value, file=sys.stderr); return 1
    except (ValueError, UnicodeError) as error:
        print(str(error), file=sys.stderr); return 2
    except OSError as error:
        print(str(error), file=sys.stderr); return 1


if __name__ == "__main__":
    raise SystemExit(main())
