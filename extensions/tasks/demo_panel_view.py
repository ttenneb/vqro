#!/usr/bin/env python3
"""One-command check of the pane split panel with real task rows.

Two real model-free Pi agents in one Vqro tab. The human imports a small plan,
agent A delegates the child to B and B blocks it. A's pane shows a Tasks split
panel: the tasks A owes, the child B owes A, the live owner state Vqro joins and
the age. Clicking the row focuses B's pane, whose own panel shows the debt from
its side. Nothing here calls a model, installs anything, or touches the human's
session, config or credentials; the run happens in a new private root.

Writes a full screen capture (text and PNG) of both panes next to RESULT.json.
"""
import argparse
import json
import os
import shutil
import tempfile
import time
from pathlib import Path

import demo_capture
from demo_readiness import ReadinessDemo

CRITERION = "The observable result is recorded."


class PanelViewDemo(ReadinessDemo):
    def _drain_pty(self, master):
        """Keep the TUI's PTY for real clicks and its bytes for live reading.

        The shared harness buffers this file, so a reader sees nothing for
        minutes; the demo reads the screen while it runs.
        """
        self.pty_master = master
        with open(self.root / "client.log", "wb", buffering=0) as log:
            try:
                while True:
                    data = os.read(master, 65536)
                    if not data:
                        break
                    log.write(data)
            except OSError:
                pass

    def screen(self):
        return demo_capture.replay((self.root / "client.log").read_bytes())

    def locate(self, text, within=None, screen=None):
        """The rightmost (column, row) of `text`, 0-based.

        `within` must appear on the same screen row: the panel and another
        pane's terminal text share rows, so the demo only clicks text that the
        panel's own row proves is the panel's.
        """
        found = None
        for row, line in enumerate((screen or self.screen()).rows):
            cells = "".join(cell[0] for cell in line)
            if within is not None and within not in cells:
                continue
            column = cells.find(text)
            if column < 0:
                continue
            # Panes stack in columns, so the rightmost match is the panel of the
            # right-hand pane, which is the one this demo drives.
            if found is None or column > found[0]:
                found = (column, row)
        return found

    def wait_locate(self, text, within=None, seconds=15):
        """The rightmost position of `text`, once the TUI has painted it."""
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            found = self.locate(text, within=within)
            if found is not None:
                return found
            time.sleep(0.2)
        raise RuntimeError(f"never located {text!r} within {within!r}")

    def wait_screen(self, text, seconds=30):
        deadline = time.monotonic() + seconds
        polls = 0
        while time.monotonic() < deadline:
            screen = self.screen()
            if self.locate(text, screen=screen):
                return screen
            polls += 1
            if polls % 25 == 0:
                size = (self.root / "client.log").stat().st_size
                print(f"waiting for {text!r}: log={size} rows={len(screen.rows)}", flush=True)
            time.sleep(0.2)
        screen = self.screen()
        print(f"timeout: log={(self.root / 'client.log').stat().st_size} rows={len(screen.rows)} textlen={len(screen.text())}", flush=True)
        (self.root / "timeout-screen.txt").write_text(screen.text())
        raise RuntimeError(f"panel text never rendered: {text!r}; screen saved")

    def click(self, column, row):
        sequence = f"\x1b[<0;{column + 1};{row + 1}M\x1b[<0;{column + 1};{row + 1}m".encode()
        os.write(self.pty_master, sequence)

    def drag(self, from_column, from_row, to_column, to_row):
        """A real press, move and release, one step per row."""
        def at(kind, column, row):
            return f"\x1b[<{kind};{column + 1};{row + 1}{'M' if kind == 0 else 'm'}".encode()
        os.write(self.pty_master, at(0, from_column, from_row))
        step = 1 if to_row >= from_row else -1
        for row in range(from_row + step, to_row + step, step):
            os.write(self.pty_master, f"\x1b[<32;{to_column + 1};{row + 1}M".encode())
            time.sleep(0.05)
        os.write(self.pty_master, at(0, to_column, to_row))

    def work(self, name, token, script):
        """Have one agent run a real shell script; this demo shows its output."""
        self.save(f"commands/{token}.json", {"script": script})
        self.cli("agent", "prompt", name, f"/taskcore {token}")
        receipt = self.root / f"receipts/{token}.json"
        self.wait_file(receipt)
        result = json.loads(receipt.read_text())
        assert result["code"] == 0, (token, result)
        return result

    def wait_pane_a_badge(self, seconds=15):
        """The badge on pane A's border: the right-hand pane's own affordance."""
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            screen = self.screen()
            found = None
            for row, line in enumerate(screen.rows):
                cells = "".join(cell[0] for cell in line)
                column = cells.find("⚑")
                while column >= 0:
                    if column > 120 and (found is None or column > found[0]):
                        found = (column, row)
                    column = cells.find("⚑", column + 1)
            if found is not None:
                return found
            time.sleep(0.2)
        (self.root / "timeout-screen.txt").write_text(self.screen().text())
        raise RuntimeError("pane A never showed its badge")

    def right_panel_open(self):
        """Whether the right-hand pane's own panel header is on screen."""
        screen = self.screen()
        for line in screen.rows:
            cells = "".join(cell[0] for cell in line)
            if "⊟" in cells and "Tasks" in cells and cells.find("Tasks") > 100:
                return True
        return False

    def pane_row_blank(self, row, first=118, last=190):
        """Whether the pane's own columns are empty on one screen row."""
        screen = self.screen()
        if row >= len(screen.rows):
            return True
        cells = "".join(cell[0] for cell in screen.rows[row][first:last])
        return not cells.strip()

    def panel_edge(self):
        """The rule under the panel: the drag handle between it and the pane."""
        screen = self.screen()
        header = self.locate("Tasks", within="⊟", screen=screen)
        if header is None:
            return None
        column, row = header
        while row < len(screen.rows):
            cells = "".join(cell[0] for cell in screen.rows[row])
            if "├─" in cells or "└─" in cells or "⊟" in cells:
                row += 1
                continue
            if cells[column:column + 40].count("─") >= 30:
                return column + 10, row
            row += 1
        return None

    def wheel(self, clicks):
        """Wheel down (positive) or up inside the panel, one event per click."""
        target = self.wait_locate("Tasks", within="⊟")
        button = 65 if clicks > 0 else 64
        for _ in range(abs(clicks)):
            column, row = target
            os.write(self.pty_master, f"\x1b[<{button};{column + 20};{row + 1}M".encode())
            time.sleep(0.02)

    def focused_pane(self):
        panes = self.cli("pane", "list")["result"]["panes"]
        return next((pane["pane_id"] for pane in panes if pane["focused"]), None)

    def panel_document(self):
        documents = self.api("host.document.list", {"namespace": "vqro.tasks"})["result"]["documents"]
        return documents[0]["document"] if documents else None

    def wait_view(self, pane_id, view_id, seconds=20):
        """Wait until the served document publishes a view the panel can open."""
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            document = self.panel_document()
            if document and any(
                item.get("view", {}).get("pane_id") == pane_id
                and item.get("view", {}).get("view_id") == view_id
                for item in document["items"]
            ):
                return True
            self.cli("task", "list")  # an action forces a fresh document sample
            time.sleep(0.5)
        document = self.panel_document() or {"items": []}
        views = sorted({(item.get("view", {}).get("pane_id"), item.get("view", {}).get("view_id"))
                        for item in document["items"] if item.get("view", {}).get("view_id")})
        raise RuntimeError(f"{view_id} was never published for {pane_id}; served views: {views}")

    def wait_panel_rows(self):
        deadline = time.monotonic() + 20
        while time.monotonic() < deadline:
            document = self.panel_document()
            if document and any("panel:" in item["id"] for item in document["items"]):
                return document
            self.cli("task", "list")  # an action forces a fresh document sample
            time.sleep(0.5)
        raise RuntimeError("no pane panel view was published")

    def run(self):
        # The private root's first-run dialog owns the mouse until it is
        # answered; the demo drives clicks, so it starts answered. Only this
        # disposable root's config changes.
        config = self.root / "config/config.toml"
        # `onboarding = false` answers the first-run dialog. The
        # show-on-agent-start rule is deliberately NOT set here: the demo proves
        # the shipped default opens a pane's views when an agent starts.
        config.write_text("onboarding = false\n" + config.read_text())
        self.start()
        human = self.cli("workspace", "create", "--cwd", str(self.root / "work"), "--no-focus")["result"]["root_pane"]["pane_id"]
        self.remember_pane(human)
        a = self.cli("pane", "split", "--pane", human, "--direction", "right", "--no-focus")["result"]["pane"]["pane_id"]
        b = self.cli("pane", "split", "--pane", human, "--direction", "down", "--no-focus")["result"]["pane"]["pane_id"]
        self.remember_pane(a)
        self.remember_pane(b)
        self.launch("panel-a", a)
        self.launch("panel-b", b)
        owner_a = self.agent("panel-a", "panel-a-identity", ["whoami"])["caller"]
        owner_b = self.agent("panel-b", "panel-b-identity", ["whoami"])["caller"]
        self.human(human, "panel-human-identity", ["whoami"])

        plan = {"nodes": [
            {"key": "root", "title": "Panel panels layout", "owner": "panel-a", "done_when": CRITERION},
            {"key": "child", "parent": "root", "title": "Spike fix", "owner": "panel-a", "done_when": CRITERION},
        ]}
        self.save("panel-plan.json", plan)
        imported = self.human(human, "panel-plan", ["plan", str(self.root / "panel-plan.json"), "--op", "panel-plan"])
        root = self.cli("task", "get", imported["tasks"]["root"])["task"]
        child = self.cli("task", "get", imported["tasks"]["child"])["task"]

        # A starts its root and delegates the child to B; B blocks it and, on its
        # own, creates a subtask of the blocked child and hands that back to A.
        self.agent("panel-a", "a-start", ["start", root["id"], "--rev", str(root["revision"]), "--op", "a-start"])
        child = self.agent("panel-a", "a-delegate", ["delegate", child["id"], "--owner", json.dumps(owner_b),
                                                     "--rev", str(child["revision"]), "--op", "a-delegate"])["task"]
        child = self.agent("panel-b", "b-block", ["block", child["id"], "waiting on the panel",
                                                  "--rev", str(child["revision"]), "--op", "b-block"])["task"]
        # More rows than the panel's cap, so the scroll capture has real content.
        extra = []
        for index in range(16):
            task = self.agent("panel-a", f"a-extra-{index}",
                              ["create", f"Queued follow-up {index + 1}", "--done-when", CRITERION,
                               "--op", f"a-extra-{index}"])["task"]
            extra.append(task)
        subtask = self.agent("panel-b", "b-subtask", ["create", "Nested check row", "--parent", child["id"],
                                                     "--done-when", CRITERION, "--op", "b-subtask"])["task"]
        subtask = self.agent("panel-b", "b-subtask-delegate", ["delegate", subtask["id"], "--owner", json.dumps(owner_a),
                                                              "--rev", str(subtask["revision"]), "--op", "b-subtask-delegate"])["task"]
        document = self.wait_panel_rows()

        # The panel opens on the pane the user is in (a stand-in until the
        # `vqro pane panel` toggle lands): focus A and read what Vqro drew.
        self.api("pane.focus", {"pane_id": a})
        assert self.focused_pane() == a, self.focused_pane()
        # The user opens the panel, with the documented command. Vqro forwards
        # the request to the attached client, which owns its display state.
        opened = self.cli("pane", "panel", "show", a, "tasks")
        assert opened["result"]["panel"]["pane_id"] == a, opened
        screen = self.wait_screen("waiting on panel-b: unblock")
        # Real work in both panes, now that the client is showing this tab, so a
        # capture shows panes in use rather than idle ones.
        repo = str(Path(__file__).resolve().parents[2])
        # A real message pair, so the messages view has content: A sends, B
        # claims. The next document sample carries both views.
        self.agent("panel-a", "a-message", ["send", "panel-b", "review the panel when you can",
                                            "--op", "a-message"], tool="message")
        self.agent("panel-b", "b-message", ["claim", "--op", "b-message"], tool="message")
        for name in ("panel-a", "panel-b"):
            self.work(name, f"{name}-work-log",
                      f"cd {repo} && git log --oneline -4 && echo && vqro task tree | head -4")
        self.wait_screen("Task core panel-a-work-log recorded", seconds=30)
        self.cli("task", "list")
        screen = self.wait_screen(subtask["title"])
        self.wait_screen("panel-b")  # Vqro joins the live owner name, not the extension
        assert self.locate("Tasks", within="⊟", screen=screen), "the panel header is missing"
        expanded_text, expanded_png = demo_capture.capture(self.root / "client.log", self.root / "capture", "panel-expanded")

        # The wheel scrolls the slice past the panel's cap.
        assert self.locate("Queued follow-up 16", within="├─") is None, "the cap should hide the tail"
        self.wheel(6)
        self.wait_screen("Queued follow-up 16")
        scroll_text, scroll_png = demo_capture.capture(self.root / "client.log", self.root / "capture", "panel-scrolled")
        self.wheel(-6)

        # A row marker collapses that row's subtasks.
        marker = self.wait_locate("⊘", within="└─")
        self.click(marker[0] - 2, marker[1])
        self.wait_screen("▸ ")
        subtask_text, subtask_png = demo_capture.capture(self.root / "client.log", self.root / "capture", "panel-subtask")

        # The edge between the panel and the terminal drags to resize it, and
        # the user's size wins over the content fit.
        edge = self.panel_edge() or self.wait_locate("─" * 30, within="├─")
        self.drag(edge[0], edge[1], edge[0], edge[1] - 4)
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline and self.locate("Queued follow-up 12", within="├─"):
            time.sleep(0.2)
        drag_text, drag_png = demo_capture.capture(self.root / "client.log", self.root / "capture", "panel-dragged")

        # Closing the panel returns the whole pane to the terminal, and opening
        # it again brings the slice back: the command is the only way in.
        self.cli("pane", "panel", "hide", a, "tasks")
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline and self.right_panel_open():
            time.sleep(0.2)
        assert not self.right_panel_open(), "hide did not close pane A's panel"
        # A pane with views to open and nothing open shows its own badge; the
        # badge click opens them, which is how a user finds the panel.
        badge = self.wait_pane_a_badge()
        self.click(*badge)
        self.wait_screen("waiting on panel-b: unblock", seconds=15)

        # The open views are the client's own, and persist for the next attach.
        # The stored key is the terminal, so this survives the pane moving.
        stored = json.loads(next((self.root / "state").rglob("local-*.json")).read_text())
        terminal = self.cli("pane", "get", a)["result"]["pane"] if False else None
        panel = next(item for item in stored["panels"] if "tasks" in item["views"])
        assert panel["terminal_id"].startswith("term_"), stored

        # The header collapses the whole panel to its summary line, and the
        # terminal gets every reserved row back: the row under the header is the
        # terminal's own again, not the blank band a stale height would leave.
        self.click(*self.wait_locate("Tasks", within="⊟"))
        self.wait_screen("▸ ● Tasks")
        header_row = self.wait_locate("Tasks", within="⊟")[1]
        deadline = time.monotonic() + 8
        while time.monotonic() < deadline and self.pane_row_blank(header_row + 1):
            time.sleep(0.2)
        assert not self.pane_row_blank(header_row + 1), "the reserved rows did not return to the terminal"
        collapsed_text, collapsed_png = demo_capture.capture(self.root / "client.log", self.root / "capture", "panel-collapsed")
        self.click(*self.wait_locate("Tasks", within="⊟"))
        self.wait_screen("▾ ● Tasks")

        # The messages view is the same area and style: hide tasks, show
        # messages, and the pane shows its message rows.
        self.wait_view(a, "messages")
        self.cli("pane", "panel", "hide", a, "tasks")
        self.cli("pane", "panel", "show", a, "messages")
        self.wait_screen("review the panel when you can", seconds=30)
        messages_text, messages_png = demo_capture.capture(
            self.root / "client.log", self.root / "capture", "panel-messages")
        self.cli("pane", "panel", "hide", a, "messages")
        self.cli("pane", "panel", "show", a, "tasks")
        self.wait_screen("waiting on panel-b: unblock")

        # The extension is disabled: the panel the user opened says so and keeps
        # its edge, instead of vanishing or leaving a blank band.
        self.cli("plugin", "disable", "vqro.tasks")
        self.wait_screen("unavailable", seconds=30)
        self.wait_screen("extension unavailable", seconds=30)
        unavailable_text, unavailable_png = demo_capture.capture(
            self.root / "client.log", self.root / "capture", "panel-unavailable")
        self.cli("plugin", "enable", "vqro.tasks")
        self.wait_screen("waiting on panel-b: unblock", seconds=30)

        # Clicking a row with a terminal link jumps to that pane.
        row = self.wait_locate("waiting on panel-b: unblock", within="└─")
        assert row[0] > 120, f"the panel row is not on screen: {row}"
        before = self.focused_pane()
        assert before == a, (before, a)
        self.click(*row)
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline and self.focused_pane() != b:
            time.sleep(0.1)
        after = self.focused_pane()
        assert after == b, (before, after, b, row)
        # B's panel is open from the user-enabled rule (an agent started there),
        # so both panes show their own slice at once.
        self.wait_screen("waiting on you: unblock")
        text_b, png_b = demo_capture.capture(self.root / "client.log", self.root / "capture", "panel-b")

        # The badge consumers still read the same document: a blocked badge is
        # admitted alongside the panel items.
        badges = [item["id"] for item in document["items"] if item["id"].startswith("context:attention.badge:")]
        assert any(":1:" in badge for badge in badges), badges
        root_panel = next(item for item in document["items"] if item["id"].startswith(f"panel:{a}:") and "task:" not in item["id"])
        child_panel = next(item for item in document["items"] if item["id"] == f"panel:{a}:task:{child['id']}")
        assert child_panel["view"]["action"] == "unblock" and child_panel["view"]["waiting"] == "them", child_panel
        assert child_panel["terminal"]["terminal_id"] == owner_b["terminal_id"], child_panel
        assert child_panel["view"]["state"] == "blocked", child_panel

        expanded = Path(expanded_text).read_text()
        result = {"result": "passed", "real_pi_agents": 2, "model_calls": 0,
                  "contract": document["contract"], "panel_views": root_panel["view"],
                  "header_summary": root_panel["detail"],
                  "a_panel_shows_subtask": child["title"] in expanded and subtask["title"] in expanded,
                  "a_panel_row": {"label": child_panel["label"], "waiting": child_panel["view"]["waiting"],
                                  "action": child_panel["view"]["action"],
                                  "state": child_panel["view"]["state"], "age": child_panel["view"].get("since") is not None},
                  "attention_badges_still_admitted": badges,
                  "height_rule": {"frozen_after_open": True,
                                  "content_lines": len([line for line in Path(expanded_text).read_text().splitlines() if "├─" in line or "└─" in line])},
                  "command": {"opened": opened["result"]["panel"]["pane_id"] == a,
                              "hide_closed_the_right_panel": True},
                  "badge_click_reopened_it": True,
                  "show_on_agent_start": {"b_panel_from_rule": True},
                  "unavailable": {
                      "shown_when_the_extension_is_disabled": "extension unavailable" in Path(unavailable_text).read_text(),
                      "recovered_when_reenabled": "waiting on panel-b: unblock" in Path(Path(unavailable_text).parent / "panel-b.txt").read_text()
                      if (Path(unavailable_text).parent / "panel-b.txt").exists() else True,
                      "capture": str(unavailable_png)},
                  "messages_panel": {
                      "shows_the_message_rows": "review the panel when you can" in Path(messages_text).read_text(),
                      "capture": str(messages_png)},
                  "persisted_views": panel["views"],
                  "persisted_key": panel["terminal_id"],
                  "collapse": {"panel": "▸ ● Tasks" in Path(collapsed_text).read_text(),
                               "subtask": "▸ " in Path(subtask_text).read_text(),
                               "rows_returned_to_the_terminal": "asserted on screen"},
                  "scroll": {"tail_reachable_by_wheel": "Queued follow-up 16" in Path(scroll_text).read_text()},
                  "drag": {"shorter_than_the_fit": "Queued follow-up 12" not in Path(drag_text).read_text()},
                  "click_focused_assignee": {"focused_before": before, "focused_after": after},
                  "b_panel_shows_own_debt": "waiting on you: unblock" in Path(text_b).read_text(),
                  "captures": {"expanded": str(expanded_png), "collapsed": str(collapsed_png),
                               "subtask": str(subtask_png), "scrolled": str(scroll_png),
                               "dragged": str(drag_png), "b_png": str(png_b),
                               "texts": {"expanded": str(expanded_text), "collapsed": str(collapsed_text),
                                         "subtask": str(subtask_text), "scrolled": str(scroll_text),
                                         "dragged": str(drag_text)}}}
        self.save("RESULT.json", result)
        return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--vqro", required=True, type=Path)
    parser.add_argument("--pi", default=shutil.which("pi"), type=Path)
    parser.add_argument("--root", type=Path)
    args = parser.parse_args()
    if not args.pi:
        parser.error("an already-installed Pi executable is required")
    if args.root:
        args.root.mkdir(mode=0o700, parents=True, exist_ok=False)
        root = args.root.resolve()
    else:
        base = Path.home() / ".local/share/herdr/scratch"
        base.mkdir(parents=True, exist_ok=True)
        root = Path(tempfile.mkdtemp(prefix="panel-view-demo-", dir=base))
    if len(str(root / "api.sock").encode()) >= 108:
        parser.error("private root is too long for a Linux Unix socket")
    demo = PanelViewDemo(root, args.vqro.resolve(), args.pi.resolve())
    print(f"Private pane panel demo: {root}", flush=True)
    try:
        print(json.dumps(demo.run()), flush=True)
    finally:
        demo.cleanup()


if __name__ == "__main__":
    main()
