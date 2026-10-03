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
            if column >= 0:
                found = (column, row)
        return found

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

    def focused_pane(self):
        panes = self.cli("pane", "list")["result"]["panes"]
        return next((pane["pane_id"] for pane in panes if pane["focused"]), None)

    def panel_document(self):
        documents = self.api("host.document.list", {"namespace": "vqro.tasks"})["result"]["documents"]
        return documents[0]["document"] if documents else None

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
        subtask = self.agent("panel-b", "b-subtask", ["create", "Nested check row", "--parent", child["id"],
                                                     "--done-when", CRITERION, "--op", "b-subtask"])["task"]
        subtask = self.agent("panel-b", "b-subtask-delegate", ["delegate", subtask["id"], "--owner", json.dumps(owner_a),
                                                              "--rev", str(subtask["revision"]), "--op", "b-subtask-delegate"])["task"]
        document = self.wait_panel_rows()

        # The panel opens on the pane the user is in (a stand-in until the
        # `vqro pane panel` toggle lands): focus A and read what Vqro drew.
        self.api("pane.focus", {"pane_id": a})
        assert self.focused_pane() == a, self.focused_pane()
        screen = self.wait_screen("owes unblock")
        self.cli("task", "list")
        screen = self.wait_screen(subtask["title"])
        self.wait_screen("panel-b")  # Vqro joins the live owner name, not the extension
        assert self.locate("0/3 done", screen=screen), "the panel header summary is missing"
        expanded_text, expanded_png = demo_capture.capture(self.root / "client.log", self.root / "capture", "panel-expanded")

        # A row marker collapses that row's subtasks; the header collapses the
        # whole panel to its summary line.
        marker = self.locate("⊘", within="└─")
        assert marker is not None, "the blocked row glyph is not on screen"
        self.click(marker[0] - 2, marker[1])
        self.wait_screen("▸ ")
        subtask_text, subtask_png = demo_capture.capture(self.root / "client.log", self.root / "capture", "panel-subtask")
        self.click(*self.locate("Tasks", within="⊟"))
        self.wait_screen("▸ ● Tasks")
        collapsed_text, collapsed_png = demo_capture.capture(self.root / "client.log", self.root / "capture", "panel-collapsed")
        self.click(*self.locate("Tasks", within="⊟"))
        self.wait_screen("▾ ● Tasks")

        # Clicking a row with a terminal link jumps to that pane.
        row = self.locate("owes unblock", within="└─")
        assert row is not None and row[0] > 120, f"the panel row is not on screen: {row}"
        before = self.focused_pane()
        assert before == a, (before, a)
        self.click(*row)
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline and self.focused_pane() != b:
            time.sleep(0.1)
        after = self.focused_pane()
        assert after == b, (before, after, b, row)
        self.wait_screen("you owe unblock")
        text_b, png_b = demo_capture.capture(self.root / "client.log", self.root / "capture", "panel-b")

        # The badge consumers still read the same document: a blocked badge is
        # admitted alongside the panel items.
        badges = [item["id"] for item in document["items"] if item["id"].startswith("context:attention.badge:")]
        assert any(":1:" in badge for badge in badges), badges
        root_panel = next(item for item in document["items"] if item["id"].startswith(f"panel:{a}:") and "task:" not in item["id"])
        child_panel = next(item for item in document["items"] if item["id"] == f"panel:{a}:task:{child['id']}")
        assert child_panel["detail"] == "owes unblock" and child_panel["terminal"]["terminal_id"] == owner_b["terminal_id"], child_panel
        assert child_panel["view"]["state"] == "blocked", child_panel

        expanded = Path(expanded_text).read_text()
        result = {"result": "passed", "real_pi_agents": 2, "model_calls": 0,
                  "contract": document["contract"], "panel_views": root_panel["view"],
                  "header_summary": root_panel["detail"],
                  "a_panel_shows_subtask": child["title"] in expanded and subtask["title"] in expanded,
                  "a_panel_row": {"label": child_panel["label"], "detail": child_panel["detail"],
                                  "state": child_panel["view"]["state"], "age": child_panel["view"].get("since") is not None},
                  "attention_badges_still_admitted": badges,
                  "collapse": {"panel": "▸ ● Tasks" in Path(collapsed_text).read_text(),
                               "subtask": "▸ " in Path(subtask_text).read_text()},
                  "click_focused_assignee": {"focused_before": before, "focused_after": after},
                  "b_panel_shows_own_debt": "you owe unblock" in Path(text_b).read_text(),
                  "captures": {"expanded": str(expanded_png), "collapsed": str(collapsed_png),
                               "subtask": str(subtask_png), "b_png": str(png_b),
                               "texts": {"expanded": str(expanded_text), "collapsed": str(collapsed_text),
                                         "subtask": str(subtask_text)}}}
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
