"""The agent pane's conversation: panel rows from an agent's item-stream entities (agent-v0 section 4).

Pure rendering, runtime-neutral: the same rows for every agent, built only from what the stream carries. Senders come
from the recorded `origin`, never from text; anything the agent doesn't report (times, durations) is left out, never
guessed. Rows use base panel features (`wrap` text, `children`/`expanded`), so every client can show them.
"""
import json
import os
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
TOOL_LINES = 12  # lines of a tool's output shown before "… N more lines" (redesign mockup)
HIDDEN_ROWS, HIDDEN_BYTES = 200, 8 * 1024
ARGS_BYTES = 8 * 1024
LOCATION_BYTES = 4 * 1024  # and the locations: the output keeps at least the remaining 12 KiB  # of those, the extra arguments: the output keeps the rest
STEP_CHILD_BYTES = 24 * 1024  # one tool line's details; with its copy (TOOL_COPY_BYTES) a step stays well under 40 KiB
ACTIVITY_BYTES = 64 * 1024  # one Activity row's steps (newest kept), well inside the panel's BUDGET
# A tool's output lines and copy, kept small: a turn can have dozens of calls, and the panel's BUDGET is shared.
OUTPUT_LINE_BYTES, TOOL_COPY_BYTES = 1024, 8 * 1024  # the rest of an output or a thought, one expand away
STEP_BYTES = 300  # a thinking step inside an Activity shows this much of its first line
FRAME_BYTES = 8 * 1024  # a framed message's body; longer ones stay plain rows (the panel budget counts both forms)
OTHER_FOLD = 4  # another agent's (or person's) message folds after this many lines (reply-13)
THOUGHT_LINES = 2  # thinking shows its first lines, then "▸ more" (the human's choice, reply-13)
THOUGHT_BLOCKS = ("thought", "thinking")
TOOL_CALL_BLOCKS = ("toolCall", "tool_call", "tool_use")
PACK_WIDTH = 120  # short one-word output lines (a file listing) are packed several to a row up to this width
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


def copyable(text, limit=COPY_BYTES):
    """The copy payload for a row, or None when it would cost more of the pane's budget than it is worth."""
    return text if text and len(text.encode()) <= limit else None


def when(ms):
    return time.strftime("%H:%M", time.localtime(ms / 1000)) if type(ms) is int and ms > 0 else None


def duration(start, end):
    if type(start) is not int or type(end) is not int or end < start:
        return None
    seconds = (end - start) / 1000
    return f"{seconds:.1f}s" if seconds < 10 else f"{seconds:.0f}s" if seconds < 120 else f"{seconds / 60:.0f}m"


def plural(n):
    return "line" if n == 1 else "lines"


def lines(text, style=None, limit=None):
    """Wrapped text rows, one per line (control-safe); `limit` keeps the first lines and says how many more."""
    out = str(text).split("\n")
    more = len(out) - limit if limit and len(out) > limit else 0
    # focusable: the reader can move through the conversation, and `follow` keeps the newest row in view.
    rows = [{"type": "text", "wrap": True, "focusable": True, "text": clip(plain(line)), **({"style": style} if style else {})}
            for line in (out[:limit] if more else out)]
    if more:
        rows.append({"type": "text", "style": "dim", "text": f"… {more} more {plural(more)}"})
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
        if kind in THOUGHT_BLOCKS or kind in TOOL_CALL_BLOCKS:
            continue  # thinking is its own block (thought_rows); a tool call is its tool line, never a placeholder
        if kind == "text" and isinstance(block.get("text"), str) and not block["text"].strip():
            continue  # an empty text block shows nothing (Pi's empty system message left a bare "System" label)
        if kind == "text" and isinstance(block.get("text"), str):
            # Plain is the default; only an explicit markdown block is rendered as markdown (agent-v0 section 3.2).
            markdown = block.get("format") == "markdown" and not limit
            rows += markdown_rows(block["text"], style) if markdown else lines(block["text"], style, limit)
        elif kind in ("image", "audio"):
            rows.append({"type": "text", "style": "dim", "text": f"[{kind}: {plain(str(block.get('mime') or '?'))}]"})
        elif kind in ("resource_link", "resource"):
            rows.append({"type": "text", "style": "link" if kind == "resource_link" else "dim", "text": "[" + clip(plain(str(block.get("name") or block.get("uri") or kind)), 512) + "]"})
        else:
            rows.append({"type": "text", "style": "dim", "text": f"[{plain(str(kind))} content]"})
    return rows


def thought_text(value):
    """The reasoning a message carries: its thought blocks (an assistant message), or its text (a thought message)."""
    content = value.get("content") if isinstance(value.get("content"), list) else []
    if value.get("role") == "thought":
        return "\n".join(b["text"] for b in content if isinstance(b, dict) and isinstance(b.get("text"), str))
    blocks = [b for b in content if isinstance(b, dict) and b.get("type") in THOUGHT_BLOCKS]
    text = "\n".join(str(b.get("text") or b.get("thinking") or "") for b in blocks).strip()
    if not text and value.get("final") is True and any(b.get("thinkingSignature") for b in blocks):
        return "thinking · content not shared by the model"
    return text


def hidden_rows(texts, style, line_bytes=LINE_BYTES):
    """The rows one expand away (the rest of a tool's output or of a thought), bounded so that one huge output can't
    crowd the conversation out of the panel's budget: HIDDEN_ROWS rows or HIDDEN_BYTES, then a count of the rest."""
    rows, size = [], 0
    for i, line in enumerate(texts):
        text = clip(plain(line), line_bytes)
        size += len(text.encode())
        if i >= HIDDEN_ROWS or size > HIDDEN_BYTES:
            rows.append({"type": "text", "style": "dim", "text": f"… {len(texts) - i} more {plural(len(texts) - i)} not shown here"})
            break
        rows.append({"type": "text", "wrap": True, "focusable": True, "style": style, "text": text})
    return rows


def thought_rows(eid, text, limit=THOUGHT_LINES):
    """Thinking as a dim block with a left rule: the first lines, then "▸ more" with the rest one expand away."""
    out = [line.replace("**", "") for line in text.split("\n") if line.strip()]  # thinking is plain text: no bold marks
    rows = [{"type": "text", "wrap": True, "focusable": True, "style": "dim", "text": "┊ " + clip(plain(line))}
            for line in out[:limit]]
    if len(out) > limit:
        rows.append({"id": "th:" + eid, "text": "more", "style": "dim", "expanded": False,  # clients draw the ▸
                     "children": hidden_rows(["┊ " + line for line in out[limit:]], "dim")})
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
            return self.agent_name(self.agent), "agent", "agent"
        if role in ROLE_LABEL:
            return ROLE_LABEL[role], "dim" if role in ("thought", "system") else None, role
        origin = value.get("origin") if isinstance(value.get("origin"), dict) else {}
        principal = origin.get("principal")
        known = isinstance(principal, str) and principal != "undeclared"
        if known and self.owner is not None and principal == self.owner:
            return "You", "you", "you"  # the workspace owner's principal, as the core recorded it
        holder = self.by_principal(principal) if known else None
        if holder is not None:  # the core-recorded sender is a resource the launcher tagged with that principal
            rid, r = holder
            what = f" (agent {plain(rid)})" if r.get("kind") == "agent" else ""
            return f"Message from {self.agent_name(rid)}{what}", "other_agent" if r.get("kind") == "agent" else "plain", "principal:" + principal
        if origin.get("kind") == "agent" or origin.get("agent"):  # only claimed: say so
            rid = origin.get("agent")
            name = self.agent_name(rid) if isinstance(rid, str) else plain(str(principal or "an agent"))
            claim = f" (claimed agent {plain(rid)})" if isinstance(rid, str) else " (claimed agent)"
            return (f"Message from {name}{claim}", "other_agent",
                    "agent:" + (rid if isinstance(rid, str) else str(principal)))
        if origin.get("kind") == "human" and not known:
            # As recorded, never inferred to be the viewer: an undeclared human, and the actor that sent it.
            actor = origin.get("actor")
            via = f"via {ACTORS.get(actor, plain(str(actor)))}, " if isinstance(actor, str) and actor else ""
            return f"Human ({via}undeclared)", "bold", "human:undeclared"
        if known:
            return "Message from " + plain(principal), "plain", "human:" + principal
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


def message_rows(eid, value, names, receipts, show_time=True, fold=None, thinking="show", fields=("time",)):
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
    reasoning = thought_text(value) if value.get("role") != "thought" else ""
    if reasoning and thinking == "show":
        body = thought_rows(eid, reasoning)
    elif reasoning and thinking == "summary":
        body = [{"id": "s:" + eid, "style": "dim", "expanded": False,
                 "children": hidden_rows(["┊ " + line for line in reasoning.split("\n") if line.strip()], "dim"),
                 "text": summary_text("Thinking", [value], fields)}]
    else:
        body = []  # hidden, or no reasoning
    body += content_rows(value.get("content"), body_style)
    if fold and len(body) > fold:  # long messages fold (compact view, other senders); the sender and quote stay
        body = body[:fold] + [{"id": "f:" + eid, "text": f"{len(body) - fold} more {plural(len(body) - fold)}", "style": "dim",
                               "children": body[fold:], "expanded": False}]
    return rows + body


def framed(eid, value, plain_rows, own=False, fold=OTHER_FOLD):
    """A message as a block on the card background (the human's relay-12 cards; feature `card`):
    - another sender's: framed too (`frame: true`), folding after OTHER_FOLD lines; clients without `card` draw today's
      framed block, and clients without `message`/`fold` the plain rows;
    - your own (`own`): a card without a frame, only on clients with `card` (`requires`), else the plain rows, so your
      messages never get the other senders' frame.
    Text-only messages up to FRAME_BYTES; anything else stays plain rows."""
    content = value.get("content") if isinstance(value.get("content"), list) else []
    if not content or any(not isinstance(b, dict) or b.get("type") != "text" or not isinstance(b.get("text"), str) for b in content):
        return plain_rows
    body = "\n\n".join(ANSI.sub("", b["text"]) for b in content).strip("\n")
    if not body or len(body.encode()) > FRAME_BYTES:
        return plain_rows
    head = plain_rows[0]
    title = head["text"] + "".join(" · " + r["text"] for r in plain_rows[1:2] if str(r.get("text", "")).startswith("quoting "))
    markdown = any(b.get("format") == "markdown" for b in content)
    return [{"type": "message", "id": head["id"], "text": title, "title": title, "body": body, "look": "card",
             **({"format": "markdown"} if markdown else {}), **({"fold": fold} if fold else {}), "focusable": True,
             **({} if own else {"frame": True}),
             **({"copy": head["copy"]} if head.get("copy") else {}), **({"style": head["style"]} if head.get("style") not in (None, "bold") else {}),
             "requires": ["message", "card"] + (["fold"] if fold else []) if own else ["message", "fold"],
             "fallback": plain_rows}]


PRIMARY = {"bash": "command", "shell": "command", "read": "path", "edit": "path", "write": "path", "grep": "pattern",
           "find": "pattern", "ls": "path"}


def primary_argument(name, args):
    """The argument that says what a tool call did, as text: bash's command, read's path (with its range), else the first
    short string argument. Returns (text, the keys it used)."""
    if not isinstance(args, dict):
        return (clip(plain(str(args)), 200), set()) if args not in (None, "") else ("", set())
    key = PRIMARY.get(str(name).lower())
    if key not in args or not isinstance(args[key], str):
        key = next((k for k, v in args.items() if isinstance(v, str) and v), None)
    if key is None:
        return "", set()
    text, used = args[key], {key}
    if str(name).lower() == "read" and type(args.get("offset")) is int:
        end = args["offset"] + args["limit"] - 1 if type(args.get("limit")) is int else None
        text += f" · lines {args['offset']}–{end}" if end else f" · from line {args['offset']}"
        used |= {"offset", "limit"}
    return clip(plain(text.split("\n")[0]), 200), used


FILE_ARGS = ("path", "file_path", "file", "filename")
OPEN_ITEMS = 4  # "Open … in editor" entries per tool call


def files_of(name, args, locations, cwd):
    """The files a tool call names, as (absolute path, line or None): its locations, then a path argument. Relative
    paths resolve against the agent's reported folder (`session.cwd`); without one they are left out."""
    found = []
    for loc in locations if isinstance(locations, list) else []:
        if isinstance(loc, dict) and isinstance(loc.get("path"), str):
            found.append((loc["path"], loc.get("line") if type(loc.get("line")) is int and loc["line"] > 0 else None))
    if isinstance(args, dict):
        key = next((k for k in FILE_ARGS if isinstance(args.get(k), str) and args[k].strip()), None)
        if key:
            first = args.get("offset") if str(name).lower() == "read" and type(args.get("offset")) is int and args["offset"] > 0 else None
            found.append((args[key].strip(), first))
    out = []
    for path, line in found:
        if "\n" in path or "\0" in path or len(path) > 4096:
            continue
        if not path.startswith(("/", "~")) and not (len(path) > 2 and path[1] == ":" and path[2] in "\\/"):  # relative
            if not isinstance(cwd, str) or not cwd:
                continue
            path = cwd.rstrip("/\\") + "/" + path
        if all(path != p for p, _ in out):
            out.append((path, line))
    return out[:OPEN_ITEMS]


def output_rows(text, limit=TOOL_LINES):
    """A tool's output as wrapped rows: short one-word lines packed several to a row; the first `limit` rows, then
    "… N more lines" with the rest one expand away."""
    out = ANSI.sub("", text).split("\n")
    while out and not out[-1].strip():
        out.pop()
    if len(out) > 3 and all(len(line) <= 40 and " " not in line.strip() for line in out[1:]):
        packed, row = [out[0]], ""
        for word in out[1:]:
            if row and len(row) + 2 + len(word) > PACK_WIDTH:
                packed.append(row)
                row = ""
            row = word if not row else row + "  " + word
        out = packed + ([row] if row else [])
    rows = [{"type": "text", "wrap": True, "focusable": True, "style": "muted", "text": clip(plain(line), OUTPUT_LINE_BYTES)}
            for line in out[:limit]]
    if len(out) > limit:
        rows.append({"text": f"… {len(out) - limit} more {plural(len(out) - limit)}", "style": "dim", "expanded": False,
                     "children": hidden_rows(out[limit:], "muted", OUTPUT_LINE_BYTES)})
    return rows


def bounded_children(children, limit=None, what="lines"):
    """One tool line's details (arguments, locations, output, other content) within STEP_CHILD_BYTES, in order, then a
    count of what was left out: a small entity with thousands of arguments must not outgrow the panel (#74)."""
    kept, size = [], 0
    for i, child in enumerate(children):
        size += len(json.dumps(child, ensure_ascii=False).encode()) + 2
        if size > (limit or STEP_CHILD_BYTES):
            left = len(children) - i
            noun = what[:-1] if left == 1 else what
            kept.append({"type": "text", "style": "dim", "text": f"… {left} more {noun} not shown here (pane limit)"})
            break
        kept.append(child)
    return kept


def output_lines(output):
    """How many lines a tool's output has (a trailing newline ends the last line; it doesn't start another)."""
    return len(ANSI.sub("", output).rstrip("\n").split("\n")) if output else 0


def tool_row(eid, value, cwd=None):
    """One line per tool call (redesign): icon, name, its main argument; size and time on the right. Opening it shows
    the other arguments as "name value" lines and the output, wrapped and cut."""
    status = value.get("status") if isinstance(value.get("status"), str) else "unknown"
    name = plain(str(value.get("title") or "tool"))
    args = value.get("input")
    main, used = primary_argument(name, args)
    target_style = "link" if used & set(FILE_ARGS) or main.startswith(("http://", "https://")) else None
    state_style = "error" if status == "failed" else "ok" if status == "completed" else "info" if status == "running" else None
    if used & set(FILE_ARGS) and isinstance(cwd, str) and len(cwd) > 1 and main.startswith(cwd.rstrip("/") + "/"):
        main = main[len(cwd.rstrip("/")) + 1:]  # a path argument inside the agent's folder reads relative to it
    output = text_of(value)
    took = duration(value.get("started_ms"), value.get("ended_ms"))
    count = output_lines(output)
    children = []
    if isinstance(args, dict):
        for key, arg in args.items():
            if key in used or arg is None:
                continue
            shown = arg if isinstance(arg, str) else json.dumps(arg, ensure_ascii=False)
            children.append({"type": "text", "wrap": True, "style": "dim", "text": clip(plain(f"{key} {shown}"), 1024)})
    # The extra arguments get at most ARGS_BYTES, so the output always has room (prim-opus-review P3).
    children = bounded_children(children, ARGS_BYTES, "arguments")
    places = []
    for loc in value.get("locations") if isinstance(value.get("locations"), list) else []:
        if isinstance(loc, dict) and isinstance(loc.get("path"), str):
            line = f":{loc['line']}" if type(loc.get("line")) is int else ""
            places.append({"type": "text", "style": "link", "text": clip(plain(loc["path"] + line), 1024)})
    # Locations too get a bounded share before the output (24 long ones used to push it out: vqro-lead-secondary).
    children += bounded_children(places, LOCATION_BYTES, "locations")
    if output:
        children += output_rows(output)
    children += [r for r in content_rows([b for b in value.get("content") or [] if isinstance(b, dict) and b.get("type") != "text"])]
    children = bounded_children(children)
    raw_input = args if isinstance(args, str) else json.dumps(args, ensure_ascii=False, indent=1) if "input" in value else None
    copy = copyable(ANSI.sub("", output) or raw_input, TOOL_COPY_BYTES) if (output or raw_input) else None
    badges = [b for b in (f"{count} lines" if count > 1 else None, took) if b]
    # Open a file it names in an editor beside the pane: links.open (links extension), only on the human's click (AS10e).
    menu = [{"label": "Open " + clip(plain(path.rsplit("/", 1)[-1]), 80) + (f":{line}" if line else "") + " in editor",
             "command": "links.open", "args": {"link": {"kind": "path", "text": path, **({"line": line} if line else {})}}}
            for path, line in files_of(name, args, value.get("locations"), cwd)]
    # Inline buttons (feature `actions`): [open] the first file, [copy] the output; the menu has the same for others.
    actions = ([{"text": "open", "command": "links.open", "args": menu[0]["args"]}] if menu else []) + \
              ([{"text": "copy", "command": "client.copy"}] if copy else [])
    if copy:
        menu.append({"label": "Copy " + ("output" if output else "input"), "command": "client.copy"})
    return {"id": "t:" + eid, **({"menu": menu} if menu else {}), **({"actions": actions} if actions else {}), "text": f"{TOOL_ICON.get(status, '?')} {clip(name, 40)}" + (f"   {main}" if main else ""),
            # Aligned cells (feature columns, the panel's TOOL_COLUMNS); `text` is what other clients show.
            "columns": [f"{TOOL_ICON.get(status, '?')} {clip(name, 40)}", main],
            "column_styles": [state_style, target_style],
            "style": "plain",
            **({"copy": copy} if copy else {}),
            **({"badges": [{"text": b, "style": "dim"} for b in badges]} if badges else {}),
            # A failed call says why on its line (the first line of what it returned), else just "failed".
            **({"description": clip(plain(next((l.strip() for l in ANSI.sub("", output).split("\n") if l.strip()), "failed")), 200)}
               if status == "failed" else {}),
            **({"children": children} if children else {}), "expanded": False}


def thought_step(eid, text):
    """The agent's thinking between two tool calls, inside an Activity: one dim line (its start); when there is more
    than that line shows, all of it is one expand away."""
    out = [line.replace("**", "") for line in text.split("\n") if line.strip()]
    first = clip(plain(out[0].strip()), STEP_BYTES) if out else ""
    row = {"id": "th:" + eid, "style": "dim", "text": "┊ " + first}
    if len(out) > 1 or first != plain(out[0].strip()):
        row.update(expanded=False, children=hidden_rows(["┊ " + line for line in out], "dim"))
    return row


def group_row(steps, cwd=None):
    """A run of tool calls as one Activity line (Pi style, collapsed): count, total time, failures; each tool with its
    status on the right. The agent's thinking between its calls (steps with no text of their own) sits inside, in order."""
    tools = [(eid, value) for eid, value in steps if "__thought__" not in value]
    children = []
    for eid, value in steps:
        if "__thought__" in value:
            children.append(thought_step(eid, value["__thought__"]))
            continue
        row = tool_row(eid, value, cwd)
        output = text_of(value)
        state_style, target_style = row["column_styles"]
        row["column_styles"] = [None, target_style, state_style, "dim", "dim"]
        row["columns"] = [clip(plain(str(value.get("title") or "tool")), 40), row["columns"][1],
                          TOOL_ICON.get(value.get("status"), "?") + " " + str(value.get("status") or "unknown"),
                          duration(value.get("started_ms"), value.get("ended_ms")) or "—",
                          f"{len(output.encode('utf-8'))} B" if output else "—"]
        row.pop("badges", None)  # status/time/size are cells, not distant right-aligned chips
        children.append(row)
    # One Activity is one row, which the panel budget can't split: keep its newest steps within ACTIVITY_BYTES and say
    # how many earlier ones are left out (the counts and badges above still cover all of them).
    kept, size = [], 0
    for child in reversed(children):
        size += len(json.dumps(child, ensure_ascii=False).encode()) + 2
        if kept and size > ACTIVITY_BYTES:
            break
        kept.insert(0, child)
    if len(kept) < len(children):
        left = len(children) - len(kept)
        kept.insert(0, {"type": "text", "style": "dim",
                        "text": f"… {left} earlier {'step' if left == 1 else 'steps'} not shown here (pane limit)"})
    children = [{"id": "gh:" + tools[0][0], "text": "Tool · Target · Status · Time · Output · Actions",
                 "columns": ["Tool", "Target", "Status", "Time", "Output"],
                 "detail": "Actions" + " " * len("[copy]"),  # align with the start of [open] [copy], not its end
                 "style": "heading", "focusable": False}] + kept
    starts = [v.get("started_ms") for _, v in tools if type(v.get("started_ms")) is int]
    ends = [v.get("ended_ms") for _, v in tools if type(v.get("ended_ms")) is int]
    took = duration(min(starts), max(ends)) if starts and ends else None
    failed = sum(1 for _, v in tools if v.get("status") == "failed")
    summary = [f"{len(tools)} tool" + ("s" if len(tools) != 1 else "")] + ([took] if took else []) + ([f"{failed} failed"] if failed else [])
    tally = {}
    for _, value in tools:
        key = (TOOL_ICON.get(value.get("status"), "?"), plain(str(value.get("title") or "tool"))[:20])
        tally[key] = tally.get(key, 0) + 1
    badges = [{"text": f"{icon} {name}" + (f" ×{n}" if n > 1 else ""), "style": "error" if icon == "✗" else "ok" if icon == "✓" else "info" if icon == "…" else "dim"}
              for (icon, name), n in list(tally.items())[:6]]
    run = tools[0][0]
    table = [{"text": "Table", "command": "agents.tools", "args": {"run": run}}]  # this turn's tools as a table
    return {"id": "g:" + run, "text": "Activity · " + " · ".join(summary), "badges": badges,
            "actions": table, "menu": [{"label": "Tools of this turn as a table…", **{k: table[0][k] for k in ("command", "args")}}],
            "child_table": {"columns": ACTIVITY_COLUMNS, "max_width": 112},
            "children": children, "expanded": False}


ACTIVITY_COLUMNS = [{"width": 12}, {"width": "fill", "min_width": 16},
                    {"width": 12, "priority": 3}, {"width": 7, "align": "right", "priority": 1},
                    {"width": 9, "align": "right", "priority": 2}]

# The conversation panel's column layout: only rows carrying `columns` (tool lines) use it; no header (no labels).
TOOL_COLUMNS = [{"width": 12}, {"width": "fill"}]
TABLE_BYTES = 512 * 1024  # one tools table popup, well under the core's 1 MiB live item


def is_step(kind, value):
    """An assistant message with no text of its own (thinking, tool calls): a step of the work, not a reply."""
    return kind == "message" and value.get("role") == "assistant" and value.get("final") is not False \
        and not content_rows(value.get("content"))


def run_goes_on(kind, eid, value):
    """Does a run of tool calls go on past this (non-tool) timeline entity? Yes for a step of the work and for an
    entity of another kind with nothing to show. One rule for rows() and tool_runs(), so a [Table] always lists exactly
    its Activity's tools (review P2: an empty notice split the Activity but not the table's run)."""
    return is_step(kind, value) or (kind not in ("message", "plan", "compaction") and not entity_rows(kind, eid, value))


def tool_runs(entities):
    """The runs of tool calls rows() draws as one Activity each, keyed by the run's first tool id: the same grouping
    (a step with no text continues a run; any other message, plan, compaction or shown entity ends it)."""
    timeline = sorted(((v.get("position"), kind, eid, v) for (kind, eid), (_, v) in entities.items()
                       if isinstance(v, dict) and type(v.get("position")) is int
                       and (kind in ("message", "tool", "plan", "compaction") or generic_kind(kind))),
                      key=lambda t: (t[0], t[1], t[2]))
    runs, run = {}, []
    for _, kind, eid, value in timeline:
        if kind == "tool":
            run.append((eid, value))
            continue
        if run_goes_on(kind, eid, value):
            continue
        if run:
            runs[run[0][0]] = run
        run = []
    if run:
        runs[run[0][0]] = run
    return runs


def tools_table(run, title, cwd=None):
    """A turn's tools as a table popup (mockup Screen C): labelled columns, Show/Tool facets, sorts, search; each row is
    the tool line (opens in place, [open]/[copy]). Bounded to TABLE_BYTES, in order, then a count of the rest."""
    items, size = [], 0
    for i, (eid, value) in enumerate(run):
        row = tool_row(eid, value, cwd)
        status = value.get("status") if isinstance(value.get("status"), str) else "unknown"
        name = clip(plain(str(value.get("title") or "tool")), 40)
        output = text_of(value)
        lines_ = output_lines(output)
        took = duration(value.get("started_ms"), value.get("ended_ms")) or ""
        # A table row is one line: its cells, plus its menu (open/copy) and y to copy. Details stay in the conversation
        # (nested rows would also be counted by the facets), and inline buttons would cover the right-hand cells.
        for key in ("badges", "children", "actions", "expanded"):
            row.pop(key, None)
        state_style, target_style = row["column_styles"]
        row["column_styles"] = [state_style, None, target_style, "dim", "dim"]
        row.update(columns=[TOOL_ICON.get(status, "?"), name, row["columns"][1], f"{lines_} {plural(lines_)}" if lines_ else "—", took],
                   facets={"status": status, "tool": name},
                   sort={"name": name.lower(), "output": lines_,
                         **({"time": value["ended_ms"] - value["started_ms"]} if type(value.get("started_ms")) is int
                            and type(value.get("ended_ms")) is int else {})})
        size += len(json.dumps(row, ensure_ascii=False).encode())
        if size > TABLE_BYTES:
            items.append({"type": "text", "style": "dim", "text": f"… {len(run) - i} more tools not shown here (pane limit)"})
            break
        items.append(row)
    failed = sum(1 for _, v in run if v.get("status") == "failed")
    starts = [v.get("started_ms") for _, v in run if type(v.get("started_ms")) is int]
    ends = [v.get("ended_ms") for _, v in run if type(v.get("ended_ms")) is int]
    took = duration(min(starts), max(ends)) if starts and ends else None
    right = " · ".join([f"{len(run)} tool" + ("s" if len(run) != 1 else "")] + ([f"{failed} failed"] if failed else []) + ([took] if took else []))
    return {"title": title, "title_right": right, "search": True, "requires": ["menu"],
            "columns": [{"label": "", "width": 1}, {"label": "Tool", "width": 10}, {"label": "What", "width": "fill"},
                        {"label": "Output", "width": 10, "align": "right", "priority": 2, "min_width": 6},
                        {"label": "Time", "width": 7, "align": "right", "priority": 1, "min_width": 4}],
            # Every status agent-v0 defines, so none counts only under All (review P3); labels as the tool lines show them.
            "facets": [{"key": "status", "label": "Show", "values": [{"value": "completed", "label": "✓ done"},
                                                                     {"value": "failed", "label": "✗ failed"},
                                                                     {"value": "running", "label": "… running"},
                                                                     {"value": "pending", "label": "pending"},
                                                                     {"value": "cancelled", "label": "cancelled"}]},
                       {"key": "tool", "label": "Tool"}],
            "sorts": [{"key": "time", "label": "time", "desc": True}, {"key": "output", "label": "output", "desc": True},
                      {"key": "name", "label": "name"}],
            "items": items or [{"type": "text", "style": "dim", "text": "This turn's tools are no longer in the conversation."}]}


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


# The standard entity kinds (agent-v0 section 4): each has its own view, or is facts shown elsewhere (state, request,
# session, terminal, run, association). Other kinds are "shown generically" (entity_rows), except `_` kinds, which are
# an implementation's own bookkeeping.
KNOWN_KINDS = ("message", "tool", "plan", "compaction", "receipt", "state", "run", "terminal", "request", "session",
               "association")


def generic_kind(kind):
    return isinstance(kind, str) and kind not in KNOWN_KINDS and not kind.startswith("_")
SEVERITY_STYLE = {"warn": "warn", "error": "error"}  # agent-v0: any other severity is as if absent
ENTITY_LINES = 12  # an entity of another kind shows at most this many lines of what it says


def severity_style(value):
    """The style for a reported `severity`: only a known string chooses one; anything else (absent, unknown, a list or
    an object) is no severity, never an error (vqro-lead-secondary, #97)."""
    severity = value.get("severity") if isinstance(value, dict) else None
    return SEVERITY_STYLE.get(severity, "dim") if isinstance(severity, str) else "dim"


def entity_rows(kind, eid, value, who="Agent"):
    """An entity of a kind the pane has no special view for (a `notice`, a kind added later): its `text` and typed
    `content`, styled by `severity`, with the kind named unless it is a notice. Hidden metadata (`native`, ids) is
    never shown and never stands in for text: an entity with nothing to say shows nothing."""
    style = severity_style(value)
    body = lines(value["text"], style, ENTITY_LINES) if isinstance(value.get("text"), str) and value["text"].strip() else []
    if isinstance(value.get("content"), list):
        body += content_rows(value["content"], style, ENTITY_LINES)
    if not body:
        return []
    # Named for the agent that sent it, so it can't pass for one of the pane's own lines (review P3).
    label = "· " + clip(plain(str(who)), 60) + " " + ("notice" if kind == "notice" else clip(plain(str(kind)), 40)) + ": "
    return [dict(body[0], id=f"e:{clip(plain(str(kind)), 40)}:{eid}", text=label + str(body[0].get("text", "")))] + body[1:]


def notice_rows(notices, who="Agent"):
    """Live `notice` records (agent-v0 section 4: not entities, no seq), newest last, as the pane received them."""
    out = []
    for at, value in notices:
        text = value.get("text") if isinstance(value, dict) else value if isinstance(value, str) else None
        if isinstance(text, str) and text.strip():
            style = severity_style(value)
            out.append({"type": "text", "wrap": True, "style": style,
                        "text": "· " + clip(plain(str(who)), 60) + " notice" + (" · " + when(at) if when(at) else "") + ": "
                                + clip(plain(text.strip()), NOTICE_BYTES)})
    return out


NOTICE_BYTES = 512


def home_short(path):
    """A folder as people write it: the home folder as ~ (of this extension's host, where the agent's folder is)."""
    home = os.path.expanduser("~")
    if home not in ("", "/", "~") and (path == home or path.startswith(home.rstrip("/") + "/")):
        return "~" + path[len(home.rstrip("/")):]
    return path


def facts_footer(facts, name):
    """One line of the agent's own facts (agent-v0 "Optional view facts"): folder, name, model, context, tokens, cost.
    Only what the agent reports; an absent fact is left out, never guessed or derived."""
    session = facts.get("session") if isinstance(facts.get("session"), dict) else {}
    usage = facts.get("usage") if isinstance(facts.get("usage"), dict) else {}
    parts = []
    if isinstance(session.get("cwd"), str):
        parts.append(home_short(session["cwd"]))
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
    return " · ".join(parts)  # a row with children: clients draw its ▸/▾


def rows(entities, names, coverage=None, view=None, cwd=None):
    """Panel rows for an agent's conversation, oldest first, bounded to BUDGET encoded bytes (newest kept).
    `entities` maps (kind, id) to (rev, value) as in vqro_items.ItemView; `view` is view_settings(); `cwd` is the
    agent's reported folder (relative file paths in tool calls resolve against it)."""
    view = view or view_settings()
    compact = view["density"] == "compact"
    receipts = {eid: v.get("stage") for (kind, eid), (_, v) in entities.items()
                if kind == "receipt" and isinstance(v, dict) and isinstance(v.get("stage"), str)}
    timeline = sorted(((v.get("position"), kind, eid, v) for (kind, eid), (_, v) in entities.items()
                       if isinstance(v, dict) and type(v.get("position")) is int
                       and (kind in ("message", "tool", "plan", "compaction") or generic_kind(kind))),
                      key=lambda t: (t[0], t[1], t[2]))
    # Entities of other kinds without a position (a notice, say) are state, not timeline: shown after it, in id order.
    unplaced = sorted((kind, eid, v) for (kind, eid), (_, v) in entities.items()
                      if isinstance(v, dict) and type(v.get("position")) is not int and generic_kind(kind))
    blocks, tools, hidden, last_sender = [], [], {}, None
    tail = {"agent": None, "work": None}  # the last block, if it is the agent's message / its unlabelled work

    def work(rows_, value=None):
        """Work starts a labelled agent block immediately, even before a reply exists.
        The eventual reply replaces that provisional label, never moves the Activity."""
        if blocks and (tail["agent"] == len(blocks) - 1 or tail["work"] == len(blocks) - 1):
            blocks[-1].extend(rows_)
        else:
            value = value or {}
            header = message_rows("work:" + rows_[0].get("id", "activity"),
                                  {"role": "assistant", "at_ms": value.get("at_ms", value.get("started_ms"))},
                                  names, {}, thinking="hide")[0]
            blocks.append([header] + rows_)
            tail["work"] = len(blocks) - 1

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

    def other_kind(kind, eid, value):
        """A kind this pane has no view of its own for (a notice, a later kind): what it says (entity_rows)."""
        rows_ = entity_rows(kind, eid, value, names.agent_name(names.agent))
        if rows_:
            end_tools()
            flush_hidden()
            blocks.append(rows_)

    def end_tools():
        """A run of tool calls: one Activity group. Summary collapses it under a summary line; the group row itself
        already stands in for its calls."""
        if not tools:
            return
        if all("__thought__" in v for _, v in tools):  # thinking steps with no tool call: just the thinking
            if view["thinking"] != "hide":
                flush_hidden()
                work([row for eid, v in tools for row in thought_rows(eid, v["__thought__"])])
            tools.clear()
            return
        mode = view["tools"]
        if mode == "hide":
            hidden["tools"] = hidden.get("tools", 0) + sum(1 for _, v in tools if "__thought__" not in v)
            thoughts = [(eid, v["__thought__"]) for eid, v in tools if "__thought__" in v]
            if thoughts:  # hiding tools never hides thinking the Thinking setting shows (the steps were kept for it)
                flush_hidden()
                work([thought_step(eid, text) for eid, text in thoughts])
        else:
            flush_hidden()
            group = group_row(tools, cwd)  # collapsed by default (the human's choice, reply-13)
            if mode == "summary":
                real = [v for _, v in tools if "__thought__" not in v]
                group["text"], group["style"] = summary_text("Activity", real, view["summary"], len(real)), "dim"
            work([group], next(value for _, value in tools if "__thought__" not in value))
        tools.clear()

    for _, kind, eid, value in timeline:
        if kind == "tool":
            tools.append((eid, value))
            continue
        if is_step(kind, value):
            # A step of the agent's work with no text of its own (thinking and tool calls): it continues the current
            # Activity instead of starting a labelled block.
            if thought_text(value) and view["thinking"] != "hide":
                tools.append(("~" + eid, {"__thought__": thought_text(value)}))
            continue
        if run_goes_on(kind, eid, value):
            continue  # nothing to show: it doesn't end the Activity either (the same rule as tool_runs)
        end_tools()
        if kind == "message":
            if value.get("final") is not False and not content_rows(value.get("content")) and not thought_text(value) \
                    and not (isinstance(value.get("delivery"), str) and value["delivery"] in receipts):
                continue  # a finished message with nothing to show and no delivery to report: no bare label
            label_, _, key = names.sender(value)
            role = value.get("role")
            show_time = not compact or key != last_sender
            last_sender = key
            other = role not in ("thought", "system") and key not in ("agent", "you", "user", "human:undeclared")
            fold = min(OTHER_FOLD, FOLD_LINES) if other and compact else OTHER_FOLD if other else FOLD_LINES if compact else None
            body = message_rows(eid, value, names, receipts, show_time, fold, view["thinking"], view["summary"])
            if role == "thought":  # a separate thinking entity: the block alone, under the agent's last label
                add("thinking", thought_rows(eid, thought_text(value)), "Thinking", [value], eid)
            elif role == "system":
                add("system", body, "System", [value], eid)
            elif key in view["hidden_senders"]:
                flush_hidden()
                blocks.append([{"id": "h:" + eid, "text": label_ + " (sender hidden)", "style": "dim",
                                "children": body, "expanded": False}])
            else:
                flush_hidden()
                # Another sender's message: a framed block that folds after 4 lines (the human's Q3, reply-13); the
                # plain rows (folded the same way) for clients without the frame.
                if key == "agent" and role == "assistant" and blocks and tail["work"] == len(blocks) - 1:
                    blocks[-1] = body[:1] + blocks[-1][1:] + body[1:]  # replace the provisional label in place
                    tail["agent"], tail["work"] = len(blocks) - 1, None
                elif key == "agent" and role == "assistant":
                    blocks.append(body)
                    tail["agent"] = len(blocks) - 1
                elif other:
                    blocks.append(framed(eid, value, body))
                elif role == "user":  # your own message: a card, no frame (relay-12)
                    blocks.append(framed(eid, value, body, own=True, fold=FOLD_LINES if compact else None))
                else:
                    blocks.append(body)
        elif kind == "compaction":
            add("system", [{"type": "text", "style": "dim", "text": "· System · context compacted"}], "System · context compacted", [value], eid)
        elif kind == "plan":  # shown whole, generically
            flush_hidden()
            blocks.append([{"type": "header", "text": "Plan", "style": "heading"}] + lines(json.dumps(value.get("entries", value), ensure_ascii=False, indent=1), "muted", 40))
        else:
            other_kind(kind, eid, value)
    end_tools()
    for kind, eid, value in unplaced:
        other_kind(kind, eid, value)
    flush_hidden()
    out, size, dropped = [], 0, 0
    for block in reversed(blocks):
        cost = len(json.dumps(block, ensure_ascii=False).encode())
        if out and size + cost > BUDGET:
            # The block that doesn't fit (a whole agent turn can be large) keeps its newest rows that do.
            part = []
            for row in reversed(block):
                row_cost = len(json.dumps(row, ensure_ascii=False).encode()) + 2
                if size + row_cost > BUDGET:
                    break
                part.insert(0, row)
                size += row_cost
            if part:
                out.append(part)
            dropped = len(blocks) - len(out) + (1 if part else 0)
            break
        out.append(block)
        size += cost
    # A blank line between blocks: senders and activity read as separate blocks (mockup), not one stream of rows.
    shown = [row for i, block in enumerate(reversed(out)) for row in ([{"type": "text", "text": ""}] if i else []) + block]
    while len(shown) > 1 and len(json.dumps(shown, ensure_ascii=False).encode()) > BUDGET:
        del shown[:max(1, len(shown) // 8)]  # one block larger than the budget: keep its newest rows
        dropped = dropped or 1
    if dropped:
        shown.insert(0, {"type": "text", "style": "dim", "text": "… earlier conversation not shown here (pane limit)"})
    # Coverage is decided from what the agent DECLARED (agent-v0 §4): a tail or state-only snapshot is never shown as
    # complete, in neutral words. Nothing here claims history was cut: positions are not evidence of that.
    timeline = any(isinstance(v, dict) and type(v.get("position")) is int for (_, _), (_, v) in entities.items())
    if coverage == "state_only":
        shown.insert(0, {"type": "text", "style": "dim", "text": "The agent sent only its current state, not its conversation."})
    elif coverage == "tail":
        shown.insert(0, {"type": "text", "style": "dim", "text": "Showing the recent history this agent reported." if timeline
                         else "No messages in the recent history this agent reported."})
    return shown
