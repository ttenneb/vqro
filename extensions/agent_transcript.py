"""The agent pane's conversation: panel rows from an agent's item-stream entities (agent-v0 section 4).

Pure rendering, runtime-neutral: the same rows for every agent, built only from what the stream carries. Senders come
from the recorded `origin`, never from text; anything the agent doesn't report (times, durations) is left out, never
guessed. Rows use base panel features (`wrap` text, `children`/`expanded`), so every client can show them.
"""
import json
import time

from vqro_display import plain

ROLE_LABEL = {"thought": "Thinking", "coordination": "Coordination", "system": "System"}
TOOL_ICON = {"pending": "…", "running": "…", "completed": "✓", "failed": "✗", "cancelled": "⊘"}
RECEIPT = {"recorded": "recorded", "held": "held", "admitted": "admitted", "queued": "queued",
           "inserted": "in the conversation", "handled": "handled", "supplied": "supplied", "read": "read",
           "unknown": "unknown"}
MARKDOWN_ROW_BYTES = 60 * 1024  # under the 64 KiB per formatted row (core-protocol "Formatted text")
LINE_BYTES = 4096  # one row; longer lines are cut with a mark (display only, never a request's question)
TOOL_LINES = 20  # lines of a tool's input or output shown before "… N more lines"
BUDGET = 160 * 1024  # encoded bytes of the conversation in one panel; older blocks give way first


def clip(text, size=LINE_BYTES):
    wire = text.encode()
    return text if len(wire) <= size else wire[:size - 3].decode("utf-8", errors="ignore") + "…"


def when(ms):
    return time.strftime("%H:%M", time.localtime(ms / 1000)) if type(ms) is int and ms > 0 else None


def duration(start, end):
    if type(start) is not int or type(end) is not int or end < start:
        return None
    seconds = (end - start) / 1000
    return f"{seconds:.1f}s" if seconds < 10 else f"{seconds:.0f}s" if seconds < 120 else f"{seconds / 60:.0f}m"


def lines(text, style=None, limit=None):
    """Wrapped text rows, one per line (control-safe); `limit` keeps the first lines and says how many more."""
    out = str(text).split("\n")
    more = len(out) - limit if limit and len(out) > limit else 0
    rows = [{"type": "text", "wrap": True, "text": clip(plain(line)), **({"style": style} if style else {})}
            for line in (out[:limit] if more else out)]
    if more:
        rows.append({"type": "text", "style": "dim", "text": f"… {more} more lines"})
    return rows


def markdown_rows(text, style=None):
    """A block the agent marks `format: "markdown"`: whole rows (feature `markdown`; clients without it show the
    raw text, wrapped). Split at line breaks only when longer than one row may be."""
    rows, chunk = [], []
    for line in plain(text).split("\n"):
        if chunk and len("\n".join(chunk + [line]).encode()) > MARKDOWN_ROW_BYTES:
            rows.append(chunk)
            chunk = []
        chunk.append(clip(line, MARKDOWN_ROW_BYTES))
    rows.append(chunk)
    return [{"type": "text", "wrap": True, "format": "markdown", "text": "\n".join(c), **({"style": style} if style else {})}
            for c in rows]


def content_rows(content, style=None, limit=None):
    """A message's or tool's `content`: always an array of typed blocks (agent-v0 section 3.2)."""
    rows = []
    for block in content if isinstance(content, list) else []:
        if not isinstance(block, dict):
            continue
        kind = block.get("type")
        if kind == "text" and isinstance(block.get("text"), str):
            # Plain is the default; only an explicit markdown block is rendered as markdown (agent-v0 section 3.2).
            markdown = block.get("format") == "markdown" and not limit
            rows += markdown_rows(block["text"], style) if markdown else lines(block["text"], style, limit)
        elif kind in ("image", "audio"):
            rows.append({"type": "text", "style": "dim", "text": f"[{kind}: {plain(str(block.get('mime') or '?'))}]"})
        elif kind in ("resource_link", "resource"):
            rows.append({"type": "text", "style": "dim", "text": "[" + clip(plain(str(block.get("name") or block.get("uri") or kind)), 512) + "]"})
        else:
            rows.append({"type": "text", "style": "dim", "text": f"[{plain(str(kind))} content]"})
    return rows


class Names:
    """Who a message is from, by the recorded origin (agent-v0 section 3.2) and the workspace's resources."""

    def __init__(self, resources, agent, owner=None):
        self.resources, self.agent, self.owner = resources, agent, owner

    def agent_name(self, rid):
        r = self.resources.get(rid) or {}
        return plain(str(r.get("name") or rid))

    def sender(self, value):
        role = value.get("role")
        if role == "assistant":
            return self.agent_name(self.agent), None
        if role in ROLE_LABEL:
            return ROLE_LABEL[role], "dim" if role in ("thought", "system") else None
        origin = value.get("origin") if isinstance(value.get("origin"), dict) else {}
        if origin.get("kind") == "agent" or origin.get("agent"):
            rid = origin.get("agent")
            name = self.agent_name(rid) if isinstance(rid, str) else plain(str(origin.get("principal") or "an agent"))
            return f"Message from {name}" + (f" (agent {plain(rid)})" if isinstance(rid, str) else ""), "accent"
        principal = origin.get("principal")
        if origin.get("kind") == "human" and principal in (None, "undeclared", self.owner):
            return "You", "bold"
        if isinstance(principal, str):
            return "Message from " + plain(principal), "accent"
        return "User", "bold"  # no recorded origin: never assumed to be you


def message_rows(eid, value, names, receipts):
    sender, style = names.sender(value)
    head = [sender]
    if when(value.get("at_ms")):
        head.append(when(value["at_ms"]))
    delivery = value.get("delivery")
    if isinstance(delivery, str) and delivery in receipts:
        head.append(RECEIPT.get(receipts[delivery], plain(str(receipts[delivery]))))
    if value.get("final") is False:
        head.append("writing…")
    rows = [{"type": "header", "id": "m:" + eid, "text": " · ".join(head), **({"style": style} if style else {})}]
    quoted = value.get("quoted")
    if isinstance(quoted, dict) and quoted.get("label"):
        rows.append({"type": "text", "style": "dim", "text": "quoting " + clip(plain(str(quoted["label"])), 256) + " (claimed)"})
    body_style = "dim" if value.get("role") in ("thought", "system") else None
    rows += content_rows(value.get("content"), body_style)
    return rows


def tool_row(eid, value):
    status = value.get("status") if isinstance(value.get("status"), str) else "unknown"
    took = duration(value.get("started_ms"), value.get("ended_ms"))
    children = []
    if "input" in value:
        raw = value["input"] if isinstance(value["input"], str) else json.dumps(value["input"], ensure_ascii=False, indent=1)
        children += lines(raw, "muted", TOOL_LINES)
    for loc in value.get("locations") if isinstance(value.get("locations"), list) else []:
        if isinstance(loc, dict) and isinstance(loc.get("path"), str):
            line = f":{loc['line']}" if type(loc.get("line")) is int else ""
            children.append({"type": "text", "style": "accent", "text": clip(plain(loc["path"] + line), 1024)})
    children += content_rows(value.get("content"), None, TOOL_LINES)
    return {"id": "t:" + eid, "text": f"{TOOL_ICON.get(status, '?')} " + clip(plain(str(value.get("title") or "tool")), 512),
            "description": " · ".join(x for x in (status, took) if x), "style": "error" if status == "failed" else None,
            "children": children, "expanded": True}


def group_row(tools):
    counts = {}
    for _, value in tools:
        counts[value.get("status")] = counts.get(value.get("status"), 0) + 1
    starts = [v.get("started_ms") for _, v in tools if type(v.get("started_ms")) is int]
    ends = [v.get("ended_ms") for _, v in tools if type(v.get("ended_ms")) is int]
    took = duration(min(starts), max(ends)) if starts and ends else None
    summary = [f"{len(tools)} tool" + ("s" if len(tools) != 1 else "")] + ([took] if took else [])
    marks = " ".join(f"{icon}{counts[s]}" for s, icon in (("completed", "✓"), ("failed", "✗"), ("running", "…"))
                     if counts.get(s))
    return {"id": "g:" + tools[0][0], "text": "Activity · " + " · ".join(summary), "description": marks,
            "children": [tool_row(eid, value) for eid, value in tools], "expanded": True}


def amount(n):
    return f"{n / 1_000_000:.1f}M" if n >= 1_000_000 else f"{n / 1000:.0f}k" if n >= 10_000 else f"{n / 1000:.1f}k" if n >= 1000 else str(n)


def facts_footer(facts, name):
    """One line of the agent's own facts (agent-v0 "Optional view facts"): folder, name, model, context, tokens, cost.
    Only what the agent reports; an absent fact is left out, never guessed or derived."""
    session = facts.get("session") if isinstance(facts.get("session"), dict) else {}
    usage = facts.get("usage") if isinstance(facts.get("usage"), dict) else {}
    parts = []
    if isinstance(session.get("cwd"), str):
        parts.append(session["cwd"])
    parts.append(name)
    model = session.get("model") if isinstance(session.get("model"), dict) else {}
    if isinstance(model.get("label") or model.get("id"), str):
        parts.append(str(model.get("label") or model["id"]) + (f" ({model['effort']})" if isinstance(model.get("effort"), str) else ""))
    context = usage.get("context") if isinstance(usage.get("context"), dict) else {}
    used, window = context.get("used"), context.get("window")
    if type(used) is int and type(window) is int and window > 0:
        parts.append(f"ctx {100 * used // window}% {amount(used)}/{amount(window)}")
    elif type(window) is int:
        parts.append(f"ctx ?/{amount(window)}")  # occupancy unknown, never derived
    tokens = usage.get("tokens") if isinstance(usage.get("tokens"), dict) else {}
    if type(tokens.get("input")) is int and type(tokens.get("output")) is int:
        parts.append(f"↑{amount(tokens['input'])} ↓{amount(tokens['output'])}")
    cost = usage.get("cost") if isinstance(usage.get("cost"), dict) else {}
    if isinstance(cost.get("amount"), (int, float)) and isinstance(cost.get("currency"), str):
        money = f"{cost['amount']:.2f} {cost['currency']}" if cost["currency"] != "USD" else f"${cost['amount']:.2f}"
        parts.append(("~" if cost.get("basis") == "estimated" else "") + money
                     + (f" ({plain(str(cost['plan']))})" if isinstance(cost.get("plan"), str) else ""))
    return {"type": "text", "pin": "bottom", "style": "dim", "text": clip(plain(" · ".join(parts)), 1024)}


def rows(entities, names, coverage=None):
    """Panel rows for an agent's conversation, oldest first, bounded to BUDGET encoded bytes (newest kept).
    `entities` maps (kind, id) to (rev, value) as in vqro_items.ItemView."""
    receipts = {eid: v.get("stage") for (kind, eid), (_, v) in entities.items()
                if kind == "receipt" and isinstance(v, dict) and isinstance(v.get("stage"), str)}
    timeline = sorted(((v.get("position"), kind, eid, v) for (kind, eid), (_, v) in entities.items()
                       if isinstance(v, dict) and type(v.get("position")) is int and kind in ("message", "tool", "plan", "compaction")),
                      key=lambda t: (t[0], t[1], t[2]))
    blocks, tools = [], []
    for _, kind, eid, value in timeline:
        if kind == "tool":
            tools.append((eid, value))
            continue
        if tools:
            blocks.append([group_row(tools)])
            tools = []
        if kind == "message":
            blocks.append(message_rows(eid, value, names, receipts))
        elif kind == "compaction":
            blocks.append([{"type": "text", "style": "dim", "text": "· System · context compacted"}])
        else:  # plan: shown whole, generically
            blocks.append([{"type": "header", "text": "Plan"}] + lines(json.dumps(value.get("entries", value), ensure_ascii=False, indent=1), "muted", 40))
    if tools:
        blocks.append([group_row(tools)])
    out, size, dropped = [], 0, 0
    for block in reversed(blocks):
        cost = len(json.dumps(block, ensure_ascii=False).encode())
        if out and size + cost > BUDGET:
            dropped = len(blocks) - len(out)
            break
        out.append(block)
        size += cost
    shown = [row for block in reversed(out) for row in block]
    while len(shown) > 1 and len(json.dumps(shown, ensure_ascii=False).encode()) > BUDGET:
        del shown[:max(1, len(shown) // 8)]  # one block larger than the budget: keep its newest rows
        dropped = dropped or 1
    if dropped:
        shown.insert(0, {"type": "text", "style": "dim", "text": f"… {dropped} earlier blocks not shown here (pane limit)"})
    if coverage in ("tail", "state_only"):
        shown.insert(0, {"type": "text", "style": "warn", "text": "Earlier history isn't available from the agent (it sent only "
                         + ("recent history" if coverage == "tail" else "its current state") + ")."})
    return shown
