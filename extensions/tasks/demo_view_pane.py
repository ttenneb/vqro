#!/usr/bin/env python3
"""One-command check of a view pane: a pane that draws an extension view.

A view pane is a pane whose content is an extension view (here Tasks) instead of
a terminal. Its scope is the terminals of its parent box, derived from placement:
the view sits beside one agent pane, so it covers exactly that agent. The run
happens in a new private root and writes a full screen capture (text and PNG)
next to RESULT.json. No model calls, no installs, no human session.
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


def view_add_args(pane, direction):
    """Omitting --ratio opts new views into transient host auto-sizing."""
    return ["pane", "view", "add", pane, "vqro.tasks:tasks", "--direction", direction, "--focus"]


class ViewPaneDemo(ReadinessDemo):
    def _drain_pty(self, master):
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

    def locate(self, text, within=None):
        """The rightmost row where `text` appears, with `within` on the same row."""
        found = None
        for row, line in enumerate(self.screen().rows):
            cells = "".join(cell[0] for cell in line)
            if within is not None and within not in cells:
                continue
            if text in cells:
                found = row
        return found

    def wait_screen(self, text, within=None, seconds=30):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            if self.locate(text, within=within) is not None:
                return self.screen()
            time.sleep(0.2)
        raise RuntimeError(f"never rendered {text!r} within {within!r}")

    def tasks_document(self):
        documents = self.api("host.document.list", {"namespace": "vqro.tasks"})["result"]["documents"]
        return documents[0]["document"] if documents else None

    def wait_view(self, pane_id, view_id, seconds=25):
        """Wait until the served document publishes a view for the pane."""
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            document = self.tasks_document()
            if document and any(
                item.get("view", {}).get("pane_id") == pane_id
                and item.get("view", {}).get("view_id") == view_id
                for item in document["items"]
            ):
                return True
            self.cli("task", "list")  # an action forces a fresh document sample
            time.sleep(0.5)
        raise RuntimeError(f"{view_id} was never published for {pane_id}")

    def keep_log(self, name):
        shutil.copy(self.root / "client.log", self.root / "capture" / f"{name}.log")

    def run(self):
        config = self.root / "config/config.toml"
        # Keep normal UI defaults for the human visual gate.
        config.write_text("onboarding = false\n" + config.read_text())
        self.start()
        human = self.cli("workspace", "create", "--cwd", str(self.root / "work"), "--focus")[
            "result"
        ]["root_pane"]["pane_id"]
        self.remember_pane(human)
        a = self.cli("pane", "split", "--pane", human, "--direction", "right", "--no-focus")[
            "result"
        ]["pane"]["pane_id"]
        self.remember_pane(a)
        self.launch("view-a", a)
        self.agent("view-a", "view-a-identity", ["whoami"])
        self.human(human, "view-human-identity", ["whoami"])
        plan = {
            "nodes": [
                {"key": "root", "title": "View panes layout", "owner": "view-a", "done_when": CRITERION},
                {"key": "child", "parent": "root", "title": "Spike fix", "owner": "view-a", "done_when": CRITERION},
            ]
        }
        self.save("view-plan.json", plan)
        imported = self.human(human, "view-plan", ["plan", str(self.root / "view-plan.json"), "--op", "view-plan"])
        child = self.cli("task", "get", imported["tasks"]["child"])["task"]
        self.wait_view(a, "tasks")
        added = self.cli(*view_add_args(a, self.direction))
        view = added["result"]["view"]
        assert view["extension"] == "vqro.tasks" and view["view"] == "tasks", view
        self.wait_screen("Tasks · ", seconds=30)
        time.sleep(2)
        self.wait_screen(child["title"], seconds=30)
        time.sleep(2)
        capture_name = "view-stacked" if self.direction == "down" else "view-pane"
        text_path = demo_capture.capture(self.root / "client.log", self.root / "capture", capture_name)
        self.keep_log(capture_name)
        screen_text = Path(text_path).read_text()
        header = next((line for line in screen_text.splitlines() if "Tasks · " in line), "")
        return {
            "result": "passed",
            "agent_pane": a,
            "view_pane": view["pane_id"],
            "view": view,
            "model_calls": 0,
            "automatic_size": True,
            "show_agent_labels_on_pane_borders": False,
            "title_setting_default": True,
            "header": header.strip(),
            "row_visible": child["title"] in screen_text,
            "capture_text": str(text_path),
        }

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--vqro", required=True, type=Path)
    parser.add_argument("--pi", default=shutil.which("pi"), type=Path)
    parser.add_argument("--root", type=Path)
    parser.add_argument("--direction", choices=("right", "down"), default="right")
    args = parser.parse_args()
    if not args.pi:
        parser.error("an already-installed Pi executable is required")
    if args.root:
        args.root.mkdir(mode=0o700, parents=True, exist_ok=False)
        root = args.root.resolve()
    else:
        base = Path.home() / ".local/share/herdr/scratch"
        base.mkdir(parents=True, exist_ok=True)
        root = Path(tempfile.mkdtemp(prefix="view-pane-demo-", dir=base))
    if len(str(root / "api.sock").encode()) >= 108:
        parser.error("private root is too long for a Linux Unix socket")
    demo = ViewPaneDemo(root, args.vqro.resolve(), args.pi.resolve())
    demo.direction = args.direction
    print(f"Private view pane demo: {root}", flush=True)
    try:
        print(json.dumps(demo.run()), flush=True)
    finally:
        demo.cleanup()


if __name__ == "__main__":
    main()
