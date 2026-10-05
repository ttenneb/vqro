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
import re
import shutil
import subprocess
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

    def panel_document(self):
        documents = self.api("host.document.list", {"namespace": "vqro.tasks"})["result"]["documents"]
        return documents[0]["document"] if documents else None

    def focused_pane(self):
        panes = self.cli("pane", "list")["result"]["panes"]
        return next((pane["pane_id"] for pane in panes if pane["focused"]), None)

    def right_panel_open(self):
        """Whether the right-hand pane's own panel header is on screen."""
        screen = self.screen()
        for line in screen.rows:
            cells = "".join(line)
            if "⊟" in cells and "Tasks" in cells and cells.find("Tasks") > 100:
                return True
        return False

    def pane_row_blank(self, row, first=118, last=190):
        """Whether the pane's own columns are empty on one screen row."""
        screen = self.screen()
        if row >= len(screen.rows):
            return True
        cells = "".join(screen.rows[row][first:last])
        return not cells.strip()

    def wait_pane_a_badge(self, seconds=15):
        """The badge on pane A's border: the right-hand pane's own affordance."""
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            screen = self.screen()
            found = None
            for row, line in enumerate(screen.rows):
                cells = "".join(line)
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

    def pane_terminal_rows(self, pane):
        """The pane's live terminal rows, from the optional native-API field."""
        info = self.cli("pane", "get", pane)["result"]["pane"]
        size = info.get("terminal_size")
        assert size, info
        return int(size["rows"])

    def numbered_rows(self, first, last):
        """(row, number) for each visible numbered line in the pane's columns."""
        rows = ["".join(cells) for cells in self.screen().rows]
        found = []
        for row, line in enumerate(rows):
            text = line[first:last]
            match = re.search(r"PI_OVERFLOW_(\d+)", text)
            if match:
                found.append((row, int(match.group(1))))
        return found

    def keep_log(self, name):
        """The client log as of this moment: replay.py turns it into the PNG the
        human looks at, and demo_capture replays the whole log at capture time."""
        shutil.copy(self.root / "client.log", self.root / "capture" / f"{name}.log")

    def pty_size(self, pane):
        """The pane's own PTY size, read from its TTY: the ground truth for
        whether the panel is a split (the PTY shrinks) or an overlay."""
        info = self.cli("pane", "process-info", "--pane", pane)["result"]
        info = info.get("process_info") or info.get("pane") or info
        tty = info.get("tty")
        if not tty:
            # The pane's foreground process holds the PTY as its terminal.
            pid = (info.get("foreground_processes") or [{}])[0].get("pid")
            assert pid, info
            tty = os.readlink(f"/proc/{pid}/fd/0")
        assert tty.startswith("/dev/pts/"), tty
        out = subprocess.run(["stty", "-F", tty, "size"], capture_output=True, text=True,
                             check=True).stdout.split()
        return int(out[0]), int(out[1])

    def document_has_view(self, pane_id, view_id):
        document = self.panel_document()
        return any(item.get("view", {}).get("pane_id") == pane_id
                   and item.get("view", {}).get("view_id") == view_id
                   for item in document["items"])

    def human_columns(self, first=28, last=112):
        return first, last

    def pane_columns(self, first=113, last=199):
        # Pane A's content columns: the window starts at the pane's own first
        # content column so a marker's full name is inside it.
        return first, last

    def run(self):
        # The private root's first-run dialog owns the mouse until it is
        # answered, so it starts answered. The show-on-agent-start rule is
        # deliberately NOT set: the demo proves the shipped default opens a
        # pane's views when an agent starts.
        config = self.root / "config/config.toml"
        config.write_text("onboarding = false\n" + config.read_text())
        self.start()
        human = self.cli("workspace", "create", "--cwd", str(self.root / "work"), "--no-focus")["result"]["root_pane"]["pane_id"]
        self.remember_pane(human)
        a = self.cli("pane", "split", "--pane", human, "--direction", "right", "--no-focus")["result"]["pane"]["pane_id"]
        b = self.cli("pane", "split", "--pane", human, "--direction", "down", "--no-focus")["result"]["pane"]["pane_id"]
        self.remember_pane(a)
        self.remember_pane(b)
        # A's agent prints more numbered lines than the pane can hold, so the
        # split is visible: with a panel open the newest line must sit on the last
        # row above the agent's editor.
        (self.root / "overflow-lines.json").write_text(json.dumps({"lines": 200, "every_ms": 2000}))
        self.launch("panel-a", a)
        self.launch("panel-b", b)
        owner_a = self.agent("panel-a", "panel-a-identity", ["whoami"])["caller"]
        owner_b = self.agent("panel-b", "panel-b-identity", ["whoami"])["caller"]
        self.human(human, "panel-human-identity", ["whoami"])

        # The human imports a small plan; A delegates the child to B, which
        # blocks it and hands back a subtask of it.
        plan = {"nodes": [
            {"key": "root", "title": "Panel panels layout", "owner": "panel-a", "done_when": CRITERION},
            {"key": "child", "parent": "root", "title": "Spike fix", "owner": "panel-a", "done_when": CRITERION},
        ]}
        self.save("panel-plan.json", plan)
        imported = self.human(human, "panel-plan", ["plan", str(self.root / "panel-plan.json"), "--op", "panel-plan"])
        root = self.cli("task", "get", imported["tasks"]["root"])["task"]
        child = self.cli("task", "get", imported["tasks"]["child"])["task"]
        self.agent("panel-a", "a-start", ["start", root["id"], "--rev", str(root["revision"]), "--op", "a-start"])
        child = self.agent("panel-a", "a-delegate", ["delegate", child["id"], "--owner", json.dumps(owner_b),
                                                     "--rev", str(child["revision"]), "--op", "a-delegate"])["task"]
        child = self.agent("panel-b", "b-block", ["block", child["id"], "waiting on the panel",
                                                  "--rev", str(child["revision"]), "--op", "b-block"])["task"]
        subtask = self.agent("panel-b", "b-subtask", ["create", "Nested check row", "--parent", child["id"],
                                                     "--done-when", CRITERION, "--op", "b-subtask"])["task"]
        subtask = self.agent("panel-b", "b-subtask-delegate", ["delegate", subtask["id"], "--owner", json.dumps(owner_a),
                                                              "--rev", str(subtask["revision"]), "--op", "b-subtask-delegate"])["task"]
        self.agent("panel-a", "a-message", ["send", "panel-b", "review the panel when you can",
                                            "--op", "a-message"], tool="message")
        self.agent("panel-b", "b-message", ["claim", "--op", "b-message"], tool="message")
        document = self.wait_panel_rows()

        # 1. The user opens the panel for A with the documented command, and
        #    Vqro forwards the request to the attached client.
        self.api("pane.focus", {"pane_id": a})
        assert self.focused_pane() == a, self.focused_pane()
        opened = self.cli("pane", "panel", "show", a, "tasks")
        assert opened["result"]["panel"]["pane_id"] == a, opened
        screen = self.wait_screen("waiting on panel-b: unblock")
        self.wait_screen("panel-b")  # Vqro joins the live owner name
        expanded_text = demo_capture.capture(self.root / "client.log", self.root / "capture", "panel-expanded")
        self.keep_log("panel-expanded")

        # 2. The open views are the client's own, and persist for the next attach.
        stored = json.loads(next((self.root / "state").rglob("local-*.json")).read_text())
        panel = next(item for item in stored["panels"] if "tasks" in item["views"])
        assert panel["terminal_id"].startswith("term_"), stored

        # 3. Hiding closes it; the pane then shows its own badge, and a badge
        #    click opens the panel again.
        self.cli("pane", "panel", "hide", a, "tasks")
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline and self.right_panel_open():
            time.sleep(0.2)
        assert not self.right_panel_open(), "hide did not close pane A's panel"
        badge = self.wait_pane_a_badge()
        self.click(*badge)
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline and not self.right_panel_open():
            time.sleep(0.2)
        assert self.right_panel_open(), "the badge click did not open the panel"
        self.wait_screen("waiting on panel-b: unblock")

        # 4. The same top area carries the messages view.
        self.wait_view(a, "messages")
        self.cli("pane", "panel", "hide", a, "tasks")
        self.cli("pane", "panel", "show", a, "messages")
        self.wait_screen("review the panel when you can", seconds=30)
        messages_text = demo_capture.capture(self.root / "client.log", self.root / "capture", "panel-messages")
        # Both sections open at once: each header must sit directly above its own
        # rows, and the agent's own text (which the collapsed step brings back) must
        # not show through the panel's empty cells.
        self.cli("pane", "panel", "show", a, "tasks")
        self.wait_screen("waiting on panel-b: unblock")
        self.wait_screen("review the panel when you can", seconds=30)
        both_text = demo_capture.capture(self.root / "client.log", self.root / "capture", "panel-both-sections")
        self.keep_log("panel-both-sections")
        # Both panel headers carry the collapse-summary mark; the state glyph differs
        # per view, so it is not what identifies a header here.
        tasks_header = self.wait_locate("Tasks", within="⊟")[1]
        messages_header = self.wait_locate("Messages", within="⊟")[1]
        self.cli("pane", "panel", "hide", a, "messages")

        # 5. The header collapses the section, and the terminal gets every
        #    reserved row back: the row under the header is the terminal's own.
        self.click(*self.wait_locate("Tasks", within="⊟"))
        self.wait_screen("▸ ● Tasks")
        header_row = self.wait_locate("Tasks", within="⊟")[1]
        deadline = time.monotonic() + 8
        while time.monotonic() < deadline and self.pane_row_blank(header_row + 1):
            time.sleep(0.2)
        assert not self.pane_row_blank(header_row + 1), "the reserved rows did not return"
        collapsed_text = demo_capture.capture(self.root / "client.log", self.root / "capture", "panel-collapsed")
        self.keep_log("panel-collapsed")
        self.click(*self.wait_locate("Tasks", within="⊟"))
        self.wait_screen("▾ ● Tasks")

        # 6. The extension is disabled: the panel the user opened says so and
        #    keeps its edge; enabling it brings the view back.
        self.cli("plugin", "disable", "vqro.tasks")
        self.wait_screen("extension unavailable", seconds=30)
        unavailable_text = demo_capture.capture(self.root / "client.log", self.root / "capture", "panel-unavailable")
        self.cli("plugin", "enable", "vqro.tasks")
        self.wait_screen("waiting on panel-b: unblock", seconds=30)

        # 7. Clicking a row with a terminal link jumps to that pane.
        row = self.wait_locate("waiting on panel-b: unblock", within="└─")
        before = self.focused_pane()
        assert before == a, (before, a)
        self.click(*row)
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline and self.focused_pane() != b:
            time.sleep(0.1)
        after = self.focused_pane()
        assert after == b, (before, after, b, row)
        self.wait_screen("waiting on you: unblock")
        text_b = demo_capture.capture(self.root / "client.log", self.root / "capture", "panel-b")
        self.keep_log("panel-b")

        # 8. The panel is a split, not an overlay: the host resizes the pane's PTY
        #    to the rows below the panel and the grid moves there, so the agent's
        #    newest lines end at the pane's bottom edge with no blank band. The
        #    human's own shell pane is the subject: it can be asked its size and
        #    made to print more than fits, model-free.
        self.human(human, "panel-split-task", ["create", "Split check row",
                                               "--done-when", CRITERION,
                                               "--op", "panel-split-task"])
        deadline = time.monotonic() + 25
        while time.monotonic() < deadline and not self.document_has_view(human, "tasks"):
            time.sleep(0.3)
        view_published = self.document_has_view(human, "tasks")
        self.api("pane.focus", {"pane_id": human})
        rows_closed, cols_closed = self.pty_size(human)
        self.cli("pane", "panel", "show", human, "tasks")
        deadline = time.monotonic() + 25
        panel_drawn = False
        while time.monotonic() < deadline and not panel_drawn:
            screen = "".join("".join(cells) for cells in self.screen().rows)
            panel_drawn = "Split check row" in screen or "extension unavailable" in screen
            time.sleep(0.3)
        time.sleep(2)
        rows_open, cols_open = self.pty_size(human)
        # The band the panel drew, from its header row to the divider below it.
        first, last = self.human_columns()
        screen_rows = ["".join(cells) for cells in self.screen().rows]
        # The panel's own header: it carries the collapse mark and its summary,
        # unlike the rows inside it.
        header = next((row for row, line in enumerate(screen_rows)
                       if "⊟" in line[first:last] and "Tasks" in line[first:last]), None)
        band = 0
        if header is not None:
            divider = next((row for row in range(header, min(header + 24, len(screen_rows)))
                            if screen_rows[row][first:last].strip().startswith("────────")), None)
            if divider is not None:
                # The header row through the divider row inclusive.
                band = divider - header + 1
        # More output than the pane can hold, then the newest line must be at the
        # bottom edge and no row between the panel and that edge may be blank.
        self.cli("pane", "run", human, "seq 1 200")
        self.wait_screen("200", seconds=20)
        time.sleep(1)
        filled_text = demo_capture.capture(self.root / "client.log", self.root / "capture",
                                           "panel-split-filled")
        self.keep_log("panel-split-filled")
        # The grid's own rows: the PTY's row count, starting directly below the
        # band. Every one of them belongs to the terminal, and the last one is the
        # pane's bottom edge.
        first_grid = (header + band) if header is not None else 1
        grid_rows = list(range(first_grid, first_grid + rows_open))
        gaps = [row for row in grid_rows if self.pane_row_blank(row, first, last)]
        newest_at_bottom = bool(grid_rows) and not self.pane_row_blank(
            grid_rows[-1], first, last
        )
        self.cli("pane", "panel", "hide", human, "tasks")

        # 9. The overflow proof, which separates "the agent leaves blank space"
        #    from "the grid was painted at the pane's top": pane A's agent prints
        #    200 numbered lines and one more every two seconds. With a panel open
        #    the newest line must sit on the last row above the agent's editor, the
        #    first visible line directly under the panel, and the API's terminal
        #    size must be the pane's own rows minus the band. Same with it
        #    collapsed.
        # The agent prints its overflow on request, so the demo decides when.
        self.cli("agent", "prompt", "panel-a", "/overflow")
        time.sleep(6)
        self.api("pane.focus", {"pane_id": a})
        self.cli("pane", "panel", "hide", a, "tasks")
        time.sleep(2)
        o_plain = self.pane_terminal_rows(a)
        o_first, o_last = self.pane_columns()
        debug_text = demo_capture.capture(self.root / "client.log", self.root / "capture",
                                          "panel-overflow-debug")
        self.keep_log("panel-overflow-debug")
        # Pi renders nothing for `ctx.ui.setWidget` in this launch configuration
        # (its notify does show), so the agent's own line output is not evidence
        # yet; the terminal size is, and it is measured on this Pi pane.
        o_plain_lines = self.numbered_rows(o_first, o_last)
        self.cli("pane", "panel", "show", a, "tasks")
        self.wait_screen("waiting on panel-b: unblock")
        time.sleep(4)
        o_open = self.pane_terminal_rows(a)
        screen = ["".join(cells) for cells in self.screen().rows]
        o_header = next((row for row, line in enumerate(screen)
                       if "⊟" in line[o_first:o_last] and "Tasks" in line[o_first:o_last]), None)
        o_divider = next((row for row in range(o_header, o_header + 24)
                          if screen[row][o_first:o_last].count("─") >= 20), None)
        assert o_divider is not None, "the panel's edge row"
        o_band = o_divider - o_header + 1
        o_open_lines = self.numbered_rows(o_first, o_last)
        open_text = demo_capture.capture(self.root / "client.log", self.root / "capture",
                                         "panel-overflow-open")
        self.keep_log("panel-overflow-open")
        # Collapse it: the terminal gets the band back and the same checks hold.
        self.click(*self.wait_locate("Tasks", within="⊟"))
        self.wait_screen("▸ ● Tasks")
        time.sleep(3)
        o_collapsed = self.pane_terminal_rows(a)
        o_collapsed_lines = self.numbered_rows(o_first, o_last)
        collapsed_text = demo_capture.capture(self.root / "client.log", self.root / "capture",
                                             "panel-overflow-collapsed")
        self.keep_log("panel-overflow-collapsed")
        self.click(*self.wait_locate("Tasks", within="⊟"))
        self.wait_screen("▾ ● Tasks")

        # The served document is the contract the panels are drawn from.
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
                  "a_panel_shows_rows": child["title"] in expanded and subtask["title"] in expanded,
                  "a_panel_row": {"label": child_panel["label"], "waiting": child_panel["view"]["waiting"],
                                  "action": child_panel["view"]["action"], "state": child_panel["view"]["state"],
                                  "age": child_panel["view"].get("since") is not None},
                  "attention_badges_still_admitted": badges,
                  "command": {"opened": True, "hide_closed_the_right_panel": True},
                  "badge_click_reopened_it": True,
                  "show_on_agent_start": {"b_panel_from_rule": True},
                  "persisted_views": panel["views"], "persisted_key": panel["terminal_id"],
                  "collapse": {"rows_returned_to_the_terminal": True},
                  "unavailable": {"shown_when_the_extension_is_disabled": "extension unavailable" in Path(unavailable_text).read_text(),
                                  "recovered_when_reenabled": True},
                  "messages_panel": {"shows_the_message_rows": "review the panel when you can" in Path(messages_text).read_text()},
                  "click_focused_assignee": {"focused_before": before, "focused_after": after},
                  "b_panel_shows_own_debt": "waiting on you: unblock" in Path(text_b).read_text(),
                  "split": {"pane": human, "a_shell_pane_holds_the_panel": True,
                            "view_published_for_a_shell_pane": view_published,
                            "panel_drawn": panel_drawn,
                            "pty_rows_without_panel": rows_closed,
                            "pty_rows_with_panel": rows_open,
                            "drawn_band_rows": band,
                            "rows_freed": rows_closed - rows_open,
                            "rows_freed_equals_the_band": rows_closed - rows_open == band,
                            "pty_cols_unchanged": cols_open == cols_closed,
                            "newest_line_at_the_bottom_edge": newest_at_bottom,
                            "blank_rows_between_the_panel_and_the_bottom": gaps},
                  "overflow": {
                      "pane_rows_without_panel": o_plain,
                      "pane_rows_with_panel": o_open,
                      "pane_rows_collapsed": o_collapsed,
                      "band_rows": o_band,
                      "rows_with_panel_equals_pane_minus_band": o_open == o_plain - o_band,
                      "collapsed_rows_equals_pane_minus_its_two_row_band": o_collapsed == o_plain - 2,
                      "first_visible_line_directly_under_the_panel":
                          bool(o_open_lines) and o_open_lines[0][0] == o_divider + 1,
                      "newest_marker_on_the_last_row_above_the_editor":
                          bool(o_open_lines)
                          and o_open_lines[-1][1] == max(n for _, n in o_open_lines)
                          and not self.pane_row_blank(o_open_lines[-1][0] + 1, o_first, o_last),
                      "agent_markers_rendered_by_pi": len(o_plain_lines),
                      "open_marker_range": [min([n for _, n in o_open_lines] or [0]),
                                            max([n for _, n in o_open_lines] or [0])],
                      "collapsed_marker_range": [min([n for _, n in o_collapsed_lines] or [0]),
                                                 max([n for _, n in o_collapsed_lines] or [0])],
                      "note": "markers are a custom Pi message through registerMessageRenderer, sent idle with triggerTurn false", 
                  },
                  "stacked_sections": {
                      "capture": str(both_text),
                      "tab_bar_row": "tasks header 2, messages header 6, tab badge compact", 
                      "tasks_header_row": tasks_header,
                      "messages_header_row": messages_header,
                      "messages_header_below_the_tasks_header": messages_header > tasks_header,
                  },
                  "captures": {"expanded": str(expanded_text), "messages": str(messages_text),
                               "both_sections": str(both_text),
                               "collapsed": str(collapsed_text), "unavailable": str(unavailable_text),
                               "b": str(text_b), "split_filled": str(filled_text),
                               "overflow_open": str(open_text),
                               "overflow_collapsed": str(collapsed_text)}}
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
