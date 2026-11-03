#!/usr/bin/env python3
"""Read-only Tasks terminal pane. Host facts via API, task facts via admitted service."""
import datetime as dt
import json
import os
import queue
import socket
import threading
import time
import unicodedata
from local_platform import account, local_account

FINAL = {"accepted", "done"}
FILTERS = ("waiting on me", "stalled", "blocked", "working", "idle", "done/accepted", "old")
DEFAULT = {"waiting on me", "stalled", "blocked"}


def clean(text):
    # Never let task/agent text become terminal control sequences (including bidi).
    return "".join(c if c.isprintable() and not 0x202a <= ord(c) <= 0x202e and not 0x2066 <= ord(c) <= 0x2069 else " " for c in str(text))


def clip(text, columns):
    result, used = [], 0
    for char in clean(text):
        size = 0 if unicodedata.combining(char) else 2 if unicodedata.east_asian_width(char) in "WF" else 1
        if used + size > columns:
            break
        if size or result:
            result.append(char)
        used += size
    return "".join(result)


def api(method, params=None):
    path = os.environ.get("VQRO_SOCKET_PATH")
    if not path:
        raise RuntimeError("host did not supply VQRO_SOCKET_PATH")
    frame = (json.dumps({"id": "tasks:view", "method": method, "params": params or {}}) + "\n").encode()
    if os.name == "nt":
        from windows_api import request
        reply = request(path, frame)
    else:
        with socket.socket(socket.AF_UNIX) as stream:
            stream.settimeout(6)
            stream.connect(path)
            stream.sendall(frame)
            data = bytearray()
            while b"\n" not in data:
                part = stream.recv(65536)
                if not part:
                    raise RuntimeError("API closed without reply")
                data.extend(part)
                if len(data) > 1024 * 1024:
                    raise RuntimeError("API reply exceeds view bound")
        reply = data.partition(b"\n")[0]
    value = json.loads(reply)
    if "error" in value:
        raise RuntimeError(value["error"].get("message", "API unavailable"))
    return value["result"]


def task_read(action, **fields):
    result = api("extension.action.invoke", {"service_id": "vqro.tasks.core", "payload": {"action": action, **fields}})
    if result.get("type") != "extension_action_invoked" or not result.get("value", {}).get("ok"):
        raise RuntimeError(result.get("value", {}).get("error", "Tasks provider unavailable"))
    return result["value"]["value"]


def exact(owner, agent):
    return owner and owner.get("class") == "agent" and agent and all(owner.get(k) == agent.get(k) for k in ("terminal_id", "agent", "execution")) and bool(agent.get("execution"))


def availability(owner, agents):
    if not owner or owner.get("class") != "agent":
        return "human"
    if agents is None:
        return "unknown"
    agent = agents.get(owner["terminal_id"])
    if agent is None:
        return "unavailable here"
    if not agent.get("execution"):
        return "unknown"
    return "current" if exact(owner, agent) else "unavailable here"


def debtors(owed):
    if not owed:
        return []
    if owed.get("role") == "acceptors":
        return owed.get("bindings", [])
    return [owed]


def human_debtor(binding):
    # Presentation only: this neither authorizes acceptance nor rewrites bindings.
    if binding.get("role") != "human_unverified" and binding.get("class") != "human_unverified":
        return False
    observed = account(binding)
    return (observed is None and not ({"uid", "sid"} & binding.keys())) or observed == local_account()


def durable_attention(task, placements):
    """Extension-owned durable categories; placement does not prove execution."""
    if task["state"] in ("accepted", "done"):
        return set()
    result = set()
    owed = task.get("owed_to") or {}
    if task.get("owed_action") in ("answer", "accept") and any(human_debtor(binding) for binding in debtors(owed)):
        result.add("human")
    if task["state"] == "blocked" or task.get("owed_action") == "acceptor_missing":
        result.add("blocked")
    if placements is not None:
        owner = task.get("owner") or {}
        if owner.get("class") == "agent" and owner.get("terminal_id") and owner["terminal_id"] not in placements:
            result.add("unavailable")
        bindings = debtors(owed) if owed.get("role") == "acceptors" else []
        if bindings and all(binding.get("class") == "agent" and binding.get("terminal_id") not in placements for binding in bindings):
            result.add("unavailable")
    return result


def attention_scope():
    raw = os.environ.get("VQRO_ATTENTION_SCOPE")
    if not raw:
        return None
    value = json.loads(raw)
    if value.get("kind") not in ("workspace", "tab", "session", "all") or not isinstance(value.get("ids"), list) or not all(isinstance(key, str) for key in value["ids"]):
        raise ValueError("invalid attention scope")
    value["durable"] = True
    return value


def in_scope(task, placements, scope):
    if not scope or scope["kind"] == "all":
        return True
    placement = (placements or {}).get((task.get("owner") or {}).get("terminal_id"))
    if scope["kind"] == "session":
        return placement is None
    return placement is not None and placement[scope["kind"] + "_id"] in scope["ids"]


def label(owner, agents):
    if not owner:
        return "—"
    if owner.get("role") == "acceptors":
        names = []
        for binding in debtors(owner):
            status = availability(binding, agents)
            mark = "[stale here] " if status == "unavailable here" else "[unknown] " if status == "unknown" else ""
            names.append(mark + label(binding, agents))
        return "acceptors: " + (", ".join(names) or "unknown")
    if owner.get("role") == "human_unverified" or owner.get("class") == "human_unverified":
        # Accounts remain distinct; neither UID nor SID proves a human gesture.
        kind, value = account(owner) or ("account", "unknown")
        return "human (unverified)" if human_debtor(owner) else f"human (unverified, {kind} {value})"
    agent = (agents or {}).get(owner.get("terminal_id"))
    if exact(owner, agent):
        return clean(agent.get("name") or owner.get("agent") or "agent")
    # A pane whose agent Vqro cannot join still reads as the agent, with just enough
    # of the terminal to tell two of them apart; the full id is not for the panel.
    terminal = clean(owner.get("terminal_id", "?"))
    if terminal.startswith("term_"):
        terminal = terminal[len("term_"):]
    return clean(owner.get("agent", "unknown")) + " @" + terminal[-4:]


def recipient(task, tasks):
    if task.get("owed_action") == "answer":
        return task.get("creator")
    if task.get("owed_action") in ("accept", "acceptor_missing"):
        return task.get("owner")
    parent = tasks.get(task.get("parent"), {})
    return task.get("delegator") or parent.get("owner") or task.get("creator")


def attention_filters(selected):
    return selected - {"old"} == DEFAULT


def activity(agent):
    # Existing API "done" means idle-but-unseen (pane_agent_status), NOT task done.
    status = agent.get("agent_status")
    return "idle" if status == "done" else status if status in ("idle", "working", "blocked") else "unknown"


def categories(task, pending_parents, agents, idle_since, now):
    result = set()
    if task["state"] in FINAL:
        return {"done/accepted"}
    owed = debtors(task.get("owed_to"))
    if any(human_debtor(binding) for binding in owed) or (task.get("kind") == "decision" and task.get("owed_action") == "answer"):
        result.add("waiting on me")
    if task["state"] == "blocked" or task.get("owed_action") == "acceptor_missing":
        result.add("blocked")
    owner = task["owner"]
    if availability(owner, agents) == "current":
        agent = agents[owner["terminal_id"]]
        state = activity(agent)
        if state in ("working", "idle", "blocked"):
            result.add(state)
        children = task["id"] in pending_parents
        since = idle_since.get(owner.get("execution"))
        if state == "idle" and task["state"] in ("open", "in_progress") and not children and since is not None and now - since >= 60:
            result.add("stalled")
    return result


def stale_acceptors(task, agents):
    owed = task.get("owed_to") or {}
    bindings = debtors(owed) if owed.get("role") == "acceptors" else []
    return bool(bindings) and all(availability(binding, agents) == "unavailable here" for binding in bindings)


def since_label(value):
    if not value:
        return "—"
    try:
        stamp = dt.datetime.fromisoformat(value)
        if stamp.tzinfo is not None:
            return stamp.astimezone(dt.timezone.utc).strftime("%Y-%m-%d %H:%M:%SZ")
    except ValueError:
        pass
    return clean(value)


def old_final(task, days, wall):
    if task["state"] not in FINAL:
        return False
    try:
        return wall - dt.datetime.fromisoformat(task["updated_at"]).timestamp() > days * 86400
    except (ValueError, KeyError):
        return False


def project(tasks, agents, idle_since, selected, attention, collapsed, days, now, wall, all_tasks=False, scope=None, placements=None):
    """Pure cached projection; iterative traversal also handles maximum-depth trees."""
    visible = set()
    pending_parents = {t.get("parent") for t in tasks.values() if t["state"] not in FINAL}
    for key, task in tasks.items():
        if "old" not in selected and old_final(task, days, wall):
            continue
        if scope:
            if not in_scope(task, placements, scope):
                continue
            if scope.get("durable", True):
                if all_tasks or durable_attention(task, placements):
                    visible.add(key)
                continue
        if all_tasks or categories(task, pending_parents, agents, idle_since, now) & (selected - {"old"}) or (attention and task["state"] not in FINAL and (availability(task["owner"], agents) == "unavailable here" or stale_acceptors(task, agents))):
            visible.add(key)
    context = set(visible)
    for key in list(visible):
        seen = set()
        parent = tasks[key].get("parent")
        while parent in tasks and parent not in seen and parent not in context:
            seen.add(parent)
            context.add(parent)
            parent = tasks[parent].get("parent")
    children = {}
    for key in context:
        parent = tasks[key].get("parent")
        children.setdefault(parent if parent in context else None, []).append(key)
    rows, visited = [], set()
    stack = [(key, 0) for key in sorted(children.get(None, []), reverse=True)]
    while stack:
        key, depth = stack.pop()
        if key in visited:
            continue
        visited.add(key)
        rows.append((key, depth, key not in visible, bool(children.get(key))))
        if key not in collapsed:
            stack.extend((child, depth + 1) for child in sorted(children.get(key, []), reverse=True))
    return rows


def live_facts(model, now):
    observed_at = model.get("observed_at")
    if model.get("error") or observed_at is None or now - observed_at > 5:
        return None, {}
    return model["agents"], model["idle_since"]


class Reader(threading.Thread):
    def __init__(self, scope=None):
        super().__init__(daemon=True)
        self.scope = scope
        self.stop = threading.Event()
        self.requests = queue.Queue(maxsize=1)
        self.results = queue.Queue(maxsize=2)
        self.tasks, self.revision, self.idles = {}, None, {}

    def publish(self, value):
        try:
            self.results.put_nowait(value)
        except queue.Full:
            self.results.get_nowait()
            self.results.put_nowait(value)

    def refresh(self):
        revision = task_read("status")["revision"]
        if revision != self.revision:
            staged, after, total = {}, None, 0
            for _ in range(10001):
                page = task_read("list", **({"after": after} if after else {}))
                total += len(json.dumps(page).encode())
                for task in page["tasks"]:
                    staged[task["id"]] = task
                if len(staged) > 10000 or total > 64 * 1024 * 1024:
                    raise RuntimeError("task snapshot exceeds view bound")
                cursor = page["next_cursor"]
                if cursor is None:
                    break
                if not isinstance(cursor, str) or (after and cursor <= after):
                    raise RuntimeError("invalid task cursor")
                after = cursor
            else:
                raise RuntimeError("task pagination did not terminate")
            if task_read("status")["revision"] != revision:
                raise RuntimeError("Tasks changed during read; retaining previous snapshot")
            self.tasks, self.revision = staged, revision
        agents = {a["terminal_id"]: a for a in api("agent.list")["agents"]}
        placements = {pane["terminal_id"]: pane for pane in api("pane.list")["panes"]} if self.scope else None
        now, idles = time.monotonic(), {}
        for agent in agents.values():
            execution = agent.get("execution")
            if execution and activity(agent) == "idle":
                previous = self.idles.get(execution)
                seq = agent.get("state_change_seq")
                since = previous[0] if previous and previous[1] == seq and now - previous[2] <= 5 else now
                idles[execution] = (since, seq, now)
        self.idles = idles
        return {"tasks": self.tasks, "agents": agents, "placements": placements, "idle_since": {key: v[0] for key, v in idles.items()}, "error": None, "revision": revision, "observed_at": now}

    def run(self):
        due, delay = 0, 2
        while not self.stop.is_set():
            try:
                owner, target = self.requests.get(timeout=max(0, due - time.monotonic()))
                try:
                    agent = api("agent.get", {"target": target})["agent"]
                    if not exact(owner, agent):
                        raise RuntimeError("Owner not current/provable; no navigation")
                    api("agent.focus", {"target": agent["pane_id"]})
                    self.publish({"notice": "Focused exact owner (focus grants no task authority)"})
                except Exception as error:
                    self.publish({"notice": clean(error)})
            except queue.Empty:
                try:
                    self.publish(self.refresh())
                    delay = 2
                except Exception as error:
                    self.idles = {}
                    self.publish({"tasks": self.tasks, "agents": None, "idle_since": {}, "error": clean(error), "revision": self.revision})
                    delay = min(30, delay * 2)
                due = time.monotonic() + delay


def run(screen):
    import curses
    curses.curs_set(0)
    curses.mousemask(curses.BUTTON1_PRESSED | curses.BUTTON4_PRESSED | getattr(curses, "BUTTON5_PRESSED", 0))
    curses.mouseinterval(0)
    screen.timeout(100)
    scope = attention_scope()
    reader = Reader(scope)
    reader.start()
    model = {"tasks": {}, "agents": None, "idle_since": {}, "error": "Loading Tasks…", "revision": None}
    selected, attention, collapsed, days, scroll, all_tasks = (set(DEFAULT) if not scope else {"waiting on me", "blocked"}), True, set(), 7, 0, False
    notice = "Read only · click owner · q closes · idle observed locally"
    rows, pending_parents, dirty, projection_due = [], set(), True, 0
    try:
        while True:
            try:
                while True:
                    update = reader.results.get_nowait()
                    if "notice" in update:
                        notice = update["notice"]
                    else:
                        model, dirty = update, True
                        pending_parents = {t.get("parent") for t in model["tasks"].values() if t["state"] not in FINAL}
            except queue.Empty:
                pass
            now = time.monotonic()
            agents, idle_since = live_facts(model, now)
            if dirty or now >= projection_due:
                rows = project(model["tasks"], agents, idle_since, selected, attention, collapsed, days, now, time.time(), all_tasks, scope, model.get("placements") if agents is not None else None)
                dirty, projection_due = False, now + 1
            height, width = screen.getmaxyx()
            screen.erase()
            hits = []
            def draw(y, x, text, attr=0):
                if 0 <= y < height and 0 <= x < width - 1:
                    try:
                        screen.addstr(y, x, clip(text, width - x - 1), attr)
                    except curses.error:
                        pass
            def button(y, x, text, action):
                draw(y, x, text, curses.A_BOLD)
                hits.append((y, x, x + len(text), action))
            draw(0, 0, "Tasks · " + (f"{scope['kind']} {', '.join(scope['ids'])} · " + ("durable attention" if scope.get("durable", True) and not all_tasks else "scoped filters") if scope else "read-only · idle = observed lower bound"), curses.A_BOLD)
            y, x = 1, 0
            for text, action in [("[needs attention]", "default"), ("[all tasks]", "all"), (f"age {days}d", None), ("[-]", "age-"), ("[+]", "age+")]:
                if x and x + len(text) >= width:
                    y, x = y + 1, 0
                if action:
                    button(y, x, text, action)
                else:
                    draw(y, x, text)
                x += len(text) + 1
            y, x = y + 1, 0
            for name in FILTERS:
                text = f"[{'x' if name in selected else ' '}] {name}  "
                if x and x + len(text) >= width:
                    y, x = y + 1, 0
                button(y, x, text, name)
                x += len(text)
            status_y, body_y = y + 1, y + 2
            stale = model["error"] or ("Live owner facts stale / unknown" if agents is None else None)
            draw(status_y, 0, "UNAVAILABLE / STALE: " + stale if stale else f"revision {model['revision']} · {len(rows)} rows · {len(model['tasks'])} tasks")
            page = max(1, (height - body_y - 1) // 4)
            scroll = max(0, min(scroll, max(0, len(rows) - page)))
            for offset, (task_id, depth, context, has_children) in enumerate(rows[scroll:scroll + page]):
                task = model["tasks"][task_id]
                line = body_y + offset * 4
                indent = min(depth, 10) * 2
                attr = curses.A_DIM if context else 0
                marker = "+" if task_id in collapsed else "−" if has_children else "·"
                flags = categories(task, pending_parents, agents, idle_since, now)
                state = f" [{task['state']}]" + (" [acceptor missing]" if task.get("owed_action") == "acceptor_missing" else " [stale acceptors]" if stale_acceptors(task, agents) else " [stalled]" if "stalled" in flags else "")
                title = clip(task["title"], max(1, width - indent - len(state) - 4))
                draw(line, indent, marker + " " + title + state, attr)
                owner = task["owner"]
                avail = availability(owner, agents)
                owner_label = label(owner, agents)[:18]
                state = activity((agents or {}).get(owner.get("terminal_id"), {}))
                idle_start = idle_since.get(owner.get("execution"))
                detail = avail
                if avail == "current":
                    detail = ("blocked (agent prompt)" if state == "blocked" else state) + (f" ≥{int(now - idle_start)}s observed" if idle_start is not None else "")
                owner_label = owner_label[:max(4, width - indent - len(detail) - 13)]
                draw(line + 1, indent + 2, "owner: " + owner_label + " (" + detail + ")", attr | (curses.A_UNDERLINE if avail == "current" else 0))
                action = task.get("owed_action") or "—"
                room = max(2, (width - indent - len(action) - 20) // 2)
                debtor = clip(label(task.get("owed_to"), agents), room)
                creditor = clip(label(recipient(task, model["tasks"]), agents), room)
                move = f"move: {debtor} {action} → for {creditor}" if task.get("owed_to") else "move: none (debt discharged)"
                draw(line + 2, indent + 2, move, attr)
                since = "since: " + since_label(task.get("owed_since")) if task.get("owed_to") else "final at: " + since_label(task.get("updated_at"))
                draw(line + 3, indent + 2, since + (" · awaiting children" if task_id in pending_parents else ""), attr)
            draw(height - 1, 0, notice)
            screen.refresh()
            key = screen.getch()
            if key in (ord('q'), 27):
                break
            if key in (curses.KEY_DOWN, ord('j')):
                scroll += 1
            elif key in (curses.KEY_UP, ord('k')):
                scroll -= 1
            elif key == curses.KEY_NPAGE:
                scroll += page
            elif key == curses.KEY_PPAGE:
                scroll -= page
            elif key == curses.KEY_MOUSE:
                try:
                    _, mx, my, _, state = curses.getmouse()
                except curses.error:
                    continue
                if state & curses.BUTTON4_PRESSED:
                    scroll -= 3
                elif state & getattr(curses, "BUTTON5_PRESSED", 0):
                    scroll += 3
                elif state & (curses.BUTTON1_PRESSED | curses.BUTTON1_CLICKED):
                    for row, left, right, action in hits:
                        if row == my and left <= mx < right:
                            if action == "default":
                                selected, attention, scroll, all_tasks = (set(DEFAULT) if not scope else {"waiting on me", "blocked"}), True, 0, False
                                if scope:
                                    scope["durable"] = True
                            elif action == "all":
                                selected = (set(FILTERS) - {"old"}) | (selected & {"old"})
                                attention, scroll, all_tasks = False, 0, True
                            elif action == "age-":
                                days = max(1, days - 1)
                            elif action == "age+":
                                days = min(365, days + 1)
                            else:
                                if scope and action != "old":
                                    scope["durable"] = False
                                selected.symmetric_difference_update({action})
                                if action != "old":
                                    all_tasks = False
                                attention = attention_filters(selected) and not all_tasks
                                scroll = 0
                            dirty = True
                    if body_y <= my < body_y + page * 4 and scroll + (my - body_y) // 4 < len(rows):
                        task_id, depth, _, has_children = rows[scroll + (my - body_y) // 4]
                        part = (my - body_y) % 4
                        if part == 0 and mx <= min(depth, 10) * 2 + 1 and has_children:
                            collapsed.symmetric_difference_update({task_id})
                            dirty = True
                        elif part == 1:
                            owner = model["tasks"][task_id]["owner"]
                            if availability(owner, agents) != "current":
                                notice = "Owner not current/provable here; no navigation"
                            else:
                                try:
                                    reader.requests.put_nowait((owner, agents[owner["terminal_id"]]["pane_id"]))
                                    notice = "Checking exact owner…"
                                except queue.Full:
                                    notice = "Navigation already pending"
    finally:
        reader.stop.set()


if __name__ == "__main__":
    import curses
    curses.wrapper(run)
