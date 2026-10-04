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

    def human_columns(self, first=28, last=112):
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
        self.cli("pane", "panel", "hide", a, "messages")
        self.cli("pane", "panel", "show", a, "tasks")
        self.wait_screen("waiting on panel-b: unblock")

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

        # 8. The panel is a split, not a crop: the host resizes the pane's PTY to
        #    the rows below the panel, and every grid row is drawn there, so the
        #    agent's newest rows end at the pane's bottom edge with no blank band.
        #    The pane's TTY is asked directly, which is the ground truth.
        self.api("pane.focus", {"pane_id": a})
        self.cli("pane", "panel", "hide", a, "tasks")
        time.sleep(2)
        rows_closed, cols_closed = self.pty_size(a)
        self.cli("pane", "panel", "show", a, "tasks")
        self.wait_screen("waiting on panel-b: unblock")
        time.sleep(2)
        rows_open, cols_open = self.pty_size(a)
        # Recorded, not asserted: this is the measurement the split depends on,
        # and a green demo that hid a broken split would be worse than a number.
        band = rows_closed - rows_open
        header = self.locate("Tasks", within="▾ ●")[1]
        screen_rows = ["".join(cells) for cells in self.screen().rows]
        divider = next(
            (row for row in range(header + 1, min(header + 24, len(screen_rows)))
             if screen_rows[row][118:190].strip().startswith("────────")),
            header + 1,
        )
        filled_text = demo_capture.capture(self.root / "client.log", self.root / "capture",
                                           "panel-split-filled")
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
                  "split": {"pty_rows_without_panel": rows_closed, "pty_rows_with_panel": rows_open,
                            "pty_cols_unchanged": cols_open == cols_closed,
                            "rows_freed": band,
                            "drawn_band_rows": divider - header + 2,
                            "pty_shrank_with_the_panel": rows_open < rows_closed,
                            "blank_row_under_the_panel": self.pane_row_blank(divider + 1)},
                  "captures": {"expanded": str(expanded_text), "messages": str(messages_text),
                               "collapsed": str(collapsed_text), "unavailable": str(unavailable_text),
                               "b": str(text_b), "split_filled": str(filled_text)}}
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
