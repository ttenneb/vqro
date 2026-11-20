"""The agent pane's conversation: panel rows from an agent's item-stream entities (agent-v0 section 4).

Pure rendering, runtime-neutral: the same rows for every agent, built only from what the stream carries. Senders come
from the recorded `origin`, never from text; anything the agent doesn't report (times, durations) is left out, never
guessed. Rows use base panel features (`wrap` text, `children`/`expanded`), so every client can show them.
"""
import json
import re
import time

from vqro_display import plain

ANSI = re.compile(r"\x1b(?:\[[0-?]*[ -/]*[@-~]|\][^\x07\x1b]*(?:\x07|\x1b\\)|[@-Z\\-_])")  # CSI, OSC, 2-byte escapes
ACTORS = {"agents": "agents UI"}  # recorded actor -> how it reads in a label
ROLE_LABEL = {"thought": "Thinking", "coordination": "Coordination", "system": "System"}
TOOL_ICON = {"pending": "…", "running": "…", "completed": "✓", "failed": "✗", "cancelled": "⊘"}
RECEIPT = {"recorded": "recorded", "held": "held", "admitted": "admitted", "queued": "queued",
           "inserted": "in the conversation", "handled": "handled", "supplied": "supplied", "read": "read",
           "unknown": "unknown"}
MARKDOWN_ROW_BYTES = 60 * 1024  # under the 64 KiB per formatted row (core-protocol "Formatted text")
LINE_BYTES = 4096  # one row; longer lines are cut with a mark (display only, never a request's question)
TOOL_LINES = 20  # lines of a tool's input or output shown before "… N more lines"
BUDGET = 160 * 1024  # encoded bytes of the conversation in one panel; older blocks give way first
FOLD_LINES = 40  # compact view: longer messages fold after this many lines

# View settings (AS10: nothing hidden by default; per pane, with workspace defaults). Each kind is shown, replaced by a
# one-line summary (expandable), or hidden with a count left in its place.
KINDS = ("thinking", "tools", "system")
MODES = ("show", "summary", "hide")
SUMMARY_FIELDS = ("time", "tokens", "tools", "lines", "words", "files")
DEFAULT_VIEW = {"thinking": "show", "tools": "show", "system": "show", "summary": ["time", "tokens", "tools"],
                "density": "detailed", "hidden_senders": []}


def view_settings(*layers):
    """Workspace defaults, then this pane's overrides; anything invalid falls back to the default."""
    out = dict(DEFAULT_VIEW)
    for layer in layers:
        if not isinstance(layer, dict):
            continue
        for kind in KINDS:
            if layer.get(kind) in MODES:
                out[kind] = layer[kind]
        if isinstance(layer.get("summary"), list):
            out["summary"] = [f for f in SUMMARY_FIELDS if f in layer["summary"]]
        if layer.get("density") in ("detailed", "compact"):
            out["density"] = layer["density"]
        if isinstance(layer.get("hidden_senders"), list):
            out["hidden_senders"] = [k for k in layer["hidden_senders"] if isinstance(k, str)][:64]
    return out


def clip(text, size=LINE_BYTES):
    wire = text.encode()
    return text if len(wire) <= size else wire[:size - 3].decode("utf-8", errors="ignore") + "…"


COPY_BYTES = 16 * 1024  # a row's copy payload; larger ones are left out (y then copies the selected line)


def copyable(text):
    """The copy payload for a row, or None when it would cost more of the pane's budget than it is worth."""
    return text if text and len(text.encode()) <= COPY_BYTES else None


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
    # focusable: the reader can move through the conversation, and `follow` keeps the newest row in view.
    rows = [{"type": "text", "wrap": True, "focusable": True, "text": clip(plain(line)), **({"style": style} if style else {})}
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
    return [{"type": "text", "wrap": True, "focusable": True, "format": "markdown", "text": "\n".join(c),
             **({"style": style} if style else {})} for c in rows]


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
        self._principals = None

    def by_principal(self, principal):
        """The one resource the launcher tagged with this principal (resource.principal; an agent over its terminal),
        or None when no single resource does."""
        if self._principals is None:
            # Only a tag exactly one agent carries names it (a multi-endpoint host may tag several, or its terminal and
            # its agent share one): an ambiguous tag names nobody, and the principal itself is shown.
            holders = {}
            for rid, r in self.resources.items():
                tag = r.get("principal") if isinstance(r, dict) else None
                if isinstance(tag, str) and tag != "undeclared":
                    holders.setdefault(tag, []).append((rid, r))
            self._principals = {}
            for tag, found in holders.items():
                agents = [h for h in found if h[1].get("kind") == "agent"]
                pick = agents if agents else found
                if len(pick) == 1:
                    self._principals[tag] = pick[0]
        return self._principals.get(principal)

    def agent_name(self, rid):
        r = self.resources.get(rid) or {}
        return plain(str(r.get("name") or rid))

    def sender(self, value):
        """(label, style, key): the key names the sender for filtering (you, agent, agent:rN, human:p, user)."""
        role = value.get("role")
        if role == "assistant":
            return self.agent_name(self.agent), None, "agent"
        if role in ROLE_LABEL:
            return ROLE_LABEL[role], "dim" if role in ("thought", "system") else None, role
        origin = value.get("origin") if isinstance(value.get("origin"), dict) else {}
        principal = origin.get("principal")
        known = isinstance(principal, str) and principal != "undeclared"
        if known and self.owner is not None and principal == self.owner:
            return "You", "bold", "you"  # the workspace owner's principal, as the core recorded it
        holder = self.by_principal(principal) if known else None
        if holder is not None:  # the core-recorded sender is a resource the launcher tagged with that principal
            rid, r = holder
            what = f" (agent {plain(rid)})" if r.get("kind") == "agent" else ""
            return f"Message from {self.agent_name(rid)}{what}", "accent", "principal:" + principal
        if origin.get("kind") == "agent" or origin.get("agent"):  # only claimed: say so
            rid = origin.get("agent")
            name = self.agent_name(rid) if isinstance(rid, str) else plain(str(principal or "an agent"))
            claim = f" (claimed agent {plain(rid)})" if isinstance(rid, str) else " (claimed agent)"
            return (f"Message from {name}{claim}", "accent",
                    "agent:" + (rid if isinstance(rid, str) else str(principal)))
        if origin.get("kind") == "human" and not known:
            # As recorded, never inferred to be the viewer: an undeclared human, and the actor that sent it.
            actor = origin.get("actor")
            via = f"via {ACTORS.get(actor, plain(str(actor)))}, " if isinstance(actor, str) and actor else ""
            return f"Human ({via}undeclared)", "bold", "human:undeclared"
        if known:
            return "Message from " + plain(principal), "accent", "human:" + principal
        return "User", "bold", "user"  # no recorded origin: nothing assumed

    def sender_name(self, key):
        """A short name for a sender key, for the senders filter."""
        if key == "agent":
            return self.agent_name(self.agent)
        if key.startswith("agent:"):
            return self.agent_name(key[6:])
        if key.startswith("principal:"):
            holder = self.by_principal(key[10:])
            return self.agent_name(holder[0]) if holder else plain(key[10:])
        return {"you": "You", "user": "User (no origin)", "human:undeclared": "Human (undeclared)"}.get(key, plain(key.split(":", 1)[-1]))


def message_rows(eid, value, names, receipts, show_time=True, fold=None):
    sender, style, _ = names.sender(value)
    head = [sender]
    if show_time and when(value.get("at_ms")):
        head.append(when(value["at_ms"]))
    delivery = value.get("delivery")
    if isinstance(delivery, str) and delivery in receipts:
        head.append(RECEIPT.get(receipts[delivery], plain(str(receipts[delivery]))))
    if value.get("final") is False:
        head.append("writing…")
    # The sender line is selectable and copies the whole message (client.copy, bound to y in the pane).
    whole = copyable(text_of(value))
    rows = [{"type": "text", "focusable": True, "id": "m:" + eid, "text": " · ".join(head), "style": style or "bold",
             **({"copy": whole} if whole else {})}]
    quoted = value.get("quoted")
    if isinstance(quoted, dict) and quoted.get("label"):
        rows.append({"type": "text", "style": "dim", "text": "quoting " + clip(plain(str(quoted["label"])), 256) + " (claimed)"})
    body_style = "dim" if value.get("role") in ("thought", "system") else None
    body = content_rows(value.get("content"), body_style)
    if fold and len(body) > fold:  # compact view: long messages fold, the rest one expand away
        body = body[:fold] + [{"id": "f:" + eid, "text": f"… {len(body) - fold} more lines", "style": "dim",
                               "children": body[fold:], "expanded": False}]
    return rows + body


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
    output = ANSI.sub("", text_of(value))  # terminal output: copy the text, not its colour codes
    raw_input = value.get("input")
    copy = output or (raw_input if isinstance(raw_input, str) else json.dumps(raw_input, ensure_ascii=False, indent=1) if "input" in value else None)
    copy = copyable(copy) if copy else None
    return {"id": "t:" + eid, "text": f"{TOOL_ICON.get(status, '?')} " + clip(plain(str(value.get("title") or "tool")), 512),
            **({"copy": copy} if copy else {}),
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


def senders(entities, names):
    """Every sender seen in the conversation (for the filter), in first-seen order."""
    seen = {}
    for (kind, _), (_, value) in sorted(entities.items(), key=lambda kv: (kv[1][1].get("position", 0) if isinstance(kv[1][1], dict) else 0)):
        if kind == "message" and isinstance(value, dict) and value.get("role") not in ("thought", "system"):
            key = names.sender(value)[2]
            seen.setdefault(key, names.sender_name(key))
    return seen


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


def tokens_of(value):
    tokens = (value.get("usage") or {}).get("tokens") if isinstance(value.get("usage"), dict) else None
    total = sum(n for n in (tokens or {}).values() if type(n) is int) if isinstance(tokens, dict) else 0
    return total or None


def text_of(value):
    content = value.get("content") if isinstance(value.get("content"), list) else []
    return "\n".join(b["text"] for b in content if isinstance(b, dict) and isinstance(b.get("text"), str))


def summary_text(title, values, fields, tools=None):
    """The one-line stand-in for a hidden block: its title and the chosen fields, each only if known."""
    parts = [title]
    if "time" in fields:
        starts = [v.get("started_ms") for v in values if type(v.get("started_ms")) is int]
        ends = [v.get("ended_ms") for v in values if type(v.get("ended_ms")) is int]
        took = duration(min(starts), max(ends)) if starts and ends else None
        at = when(values[0].get("at_ms"))
        parts += [x for x in (took or at,) if x]
    if "tokens" in fields:
        total = sum(t for t in (tokens_of(v) for v in values) if t)
        parts += [f"{amount(total)} tokens"] if total else []
    if "tools" in fields and tools:
        parts.append(f"{tools} tool" + ("s" if tools != 1 else ""))
    if "lines" in fields:
        n = sum(text_of(v).count("\n") + 1 for v in values if text_of(v))
        parts += [f"{n} lines"] if n else []
    if "files" in fields:
        files = {loc["path"] for v in values for loc in (v.get("locations") or []) if isinstance(loc, dict) and isinstance(loc.get("path"), str)}
        parts += [f"{len(files)} files"] if files else []
    if "words" in fields:
        first = " ".join(text_of(values[0]).split()[:8])
        parts += [clip(plain(first), 160) + "…"] if first else []
    return "▸ " + " · ".join(parts)


def rows(entities, names, coverage=None, view=None):
    """Panel rows for an agent's conversation, oldest first, bounded to BUDGET encoded bytes (newest kept).
    `entities` maps (kind, id) to (rev, value) as in vqro_items.ItemView; `view` is view_settings()."""
    view = view or view_settings()
    compact = view["density"] == "compact"
    receipts = {eid: v.get("stage") for (kind, eid), (_, v) in entities.items()
                if kind == "receipt" and isinstance(v, dict) and isinstance(v.get("stage"), str)}
    timeline = sorted(((v.get("position"), kind, eid, v) for (kind, eid), (_, v) in entities.items()
                       if isinstance(v, dict) and type(v.get("position")) is int and kind in ("message", "tool", "plan", "compaction")),
                      key=lambda t: (t[0], t[1], t[2]))
    blocks, tools, hidden, last_sender = [], [], {}, None

    def flush_hidden():
        if hidden:
            blocks.append([{"type": "text", "style": "dim", "text": "· hidden: " + ", ".join(f"{n} {k}" for k, n in hidden.items())}])
            hidden.clear()

    def add(kind, rows_, title, values, eid, tool_count=None):
        """A block of a kind the view may show, summarise or hide."""
        mode = view.get(kind, "show")
        if mode == "hide":
            hidden[kind] = hidden.get(kind, 0) + (tool_count or 1)
            return
        flush_hidden()
        if mode == "summary":
            rows_ = [{"id": "s:" + eid, "text": summary_text(title, values, view["summary"], tool_count),
                      "style": "dim", "children": rows_, "expanded": False}]
        blocks.append(rows_)

    def end_tools():
        """A run of tool calls: one Activity group. Summary collapses it under a summary line; the group row itself
        already stands in for its calls."""
        if not tools:
            return
        mode = view["tools"]
        if mode == "hide":
            hidden["tools"] = hidden.get("tools", 0) + len(tools)
        else:
            flush_hidden()
            group = group_row(tools)
            if compact or mode == "summary":
                group["expanded"] = False
                for child in group["children"]:
                    child["expanded"] = False
            if mode == "summary":
                group["text"], group["style"] = summary_text("Activity", [v for _, v in tools], view["summary"], len(tools)), "dim"
            blocks.append([group])
        tools.clear()

    for _, kind, eid, value in timeline:
        if kind == "tool":
            tools.append((eid, value))
            continue
        end_tools()
        if kind == "message":
            if value.get("final") is not False and not content_rows(value.get("content")) \
                    and not (isinstance(value.get("delivery"), str) and value["delivery"] in receipts):
                continue  # a finished message with nothing to show and no delivery to report: no bare label
            label_, _, key = names.sender(value)
            role = value.get("role")
            show_time = not compact or key != last_sender
            last_sender = key
            body = message_rows(eid, value, names, receipts, show_time, FOLD_LINES if compact else None)
            if role == "thought":
                add("thinking", body, "Thinking", [value], eid)
            elif role == "system":
                add("system", body, "System", [value], eid)
            elif key in view["hidden_senders"]:
                flush_hidden()
                blocks.append([{"id": "h:" + eid, "text": "▸ " + label_ + " (sender hidden)", "style": "dim",
                                "children": body, "expanded": False}])
            else:
                flush_hidden()
                blocks.append(body)
        elif kind == "compaction":
            add("system", [{"type": "text", "style": "dim", "text": "· System · context compacted"}], "System · context compacted", [value], eid)
        else:  # plan: shown whole, generically
            flush_hidden()
            blocks.append([{"type": "header", "text": "Plan"}] + lines(json.dumps(value.get("entries", value), ensure_ascii=False, indent=1), "muted", 40))
    end_tools()
    flush_hidden()
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
    if coverage in ("tail", "state_only"):  # honest, not alarming: the agent says it may not have sent everything
        shown.insert(0, {"type": "text", "style": "dim", "text": "Showing the history this agent kept; anything earlier isn't shown."
                         if coverage == "tail" else "Showing the agent's current state only."})
    return shown
