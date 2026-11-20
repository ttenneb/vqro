#!/usr/bin/env python3
"""Preview the agent pane's conversation rows from a REAL Pi session file, in a private vqro core (no agent runs).

Maps a range of a Pi session JSONL (assistant messages with thinking and toolCall blocks, tool results, user messages
and mailbox messages from other agents) to item-stream entities the way the Pi bridge publishes them, renders them with
extensions/agent_transcript.py, and publishes the rows as a live panel, so `vqro tui` (or the tmux harness) shows the
screen the human would see. For screen-by-screen checks against the redesign mockup. Never touches the user's core.

  python3 tests/agent_pane_preview.py SESSION.jsonl --from 172 --to 188 --socket /private/v.sock [--hold 600]
"""
import argparse
import json
import pathlib
import socket
import sys
import time
from datetime import datetime

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1] / "extensions"))
import agent_transcript  # noqa: E402


def ms(stamp):
    if isinstance(stamp, (int, float)):
        return int(stamp)
    try:
        return int(datetime.fromisoformat(str(stamp).replace("Z", "+00:00")).timestamp() * 1000)
    except ValueError:
        return None


def entities(rows):
    """Pi session records -> {(kind, id): (rev, value)} as the Pi bridge would publish them (approximately)."""
    out, position, calls = {}, 0, {}
    for n, row in enumerate(rows):
        message = row.get("message") if isinstance(row.get("message"), dict) else {}
        role = message.get("role")
        at = ms(row.get("timestamp"))
        if row.get("type") == "custom_message":
            text = str(row.get("content") or "")
            first, _, rest = text.partition("\n")
            sender = first[len("Message from "):].strip() if first.startswith("Message from ") else "another agent"
            position += 1
            out[("message", f"c{n}")] = (1, {"position": position, "role": "user", "final": True, "at_ms": at,
                                             "origin": {"kind": "human", "principal": sender},
                                             "content": [{"type": "text", "text": rest or text}]})
        elif role == "assistant":
            position += 1  # the message first, then its tool calls (as the bridge numbers them)
            blocks, tools = [], []
            for block in message.get("content") or []:
                if block.get("type") == "thinking":
                    blocks.append({"type": "thought", "text": block.get("thinking", "")})
                elif block.get("type") == "text":
                    blocks.append({"type": "text", "text": block.get("text", ""), "format": "markdown"})
                elif block.get("type") == "toolCall":
                    blocks.append({"type": "toolCall"})
                    tools.append(block)
            out[("message", f"m{n}")] = (1, {"position": position, "role": "assistant", "final": True, "at_ms": at,
                                             "content": blocks})
            for block in tools:
                position += 1
                calls[block.get("id")] = ("tool", block.get("id"))
                out[("tool", block.get("id"))] = (1, {"position": position, "title": block.get("name"),
                                                      "input": block.get("arguments"), "status": "running",
                                                      "started_ms": at})
        elif role == "toolResult" and message.get("toolCallId") in calls:
            key = calls[message["toolCallId"]]
            _, tool = out[key]
            tool.update(status="failed" if message.get("isError") else "completed", ended_ms=at,
                        content=[c for c in message.get("content") or [] if c.get("type") == "text"])
        elif role == "user":
            content = message.get("content")
            text = content if isinstance(content, str) else "\n".join(c.get("text", "") for c in content or [] if isinstance(c, dict))
            position += 1
            out[("message", f"u{n}")] = (1, {"position": position, "role": "user", "final": True, "at_ms": at,
                                             "content": [{"type": "text", "text": text}]})
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("session")
    ap.add_argument("--from", dest="start", type=int, default=0)
    ap.add_argument("--to", dest="end", type=int, default=None)
    ap.add_argument("--socket", required=True)
    ap.add_argument("--name", default="Pi")
    ap.add_argument("--hold", type=float, default=600)
    a = ap.parse_args()
    rows = [json.loads(line) for line in open(a.session) if line.strip()][a.start:a.end]
    names = agent_transcript.Names({"r7": {"name": a.name}}, "r7")
    items = agent_transcript.rows(entities(rows), names, "all_retained")
    s = socket.socket(socket.AF_UNIX)
    s.connect(a.socket)
    f = s.makefile("rb")
    n = [0]

    def call(method, params):
        n[0] += 1
        s.sendall((json.dumps({"jsonrpc": "2.0", "id": n[0], "method": method, "params": params}) + "\n").encode())
        while True:
            msg = json.loads(f.readline())
            if msg.get("id") == n[0]:
                return msg

    call("hello", {"client": "pane-preview"})
    snap = call("workspace.snapshot", {})["result"]["entries"]
    version = lambda key: snap.get(key, {}).get("version", 0)
    print(call("workspace.apply", {"ops": [
        {"op": "set", "key": "view/preview", "value": {"kind": "panel", "title": "Pi r7 (preview)", "items": []}, "expect": version("view/preview")},
        {"op": "set", "key": "layout/main", "value": {"tabs": [{"title": "Preview", "node": {"view": "preview"}}], "active": 0}, "expect": version("layout/main")},
        {"op": "set", "key": "workspace/active", "value": {"layout": "main"}, "expect": version("workspace/active")}],
        "note": "pane preview"}).get("error"))
    print(call("view.publish", {"view": "preview", "content": {"title": "Pi r7 (preview)", "follow": True, "items": items}}).get("error"))
    sys.stdout.flush()
    time.sleep(a.hold)  # live content lasts as long as this connection


if __name__ == "__main__":
    main()
