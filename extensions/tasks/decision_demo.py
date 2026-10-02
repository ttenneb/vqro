#!/usr/bin/env python3
"""Private two-Pi CLI decision demo; no credentials/model calls or real-session effects."""
import datetime as dt
import hashlib
import json
from pathlib import Path
import shlex
import sys
import tempfile
import time

from demo import Demo


def run(native, pi):
    scratch = Path.home() / ".local/share/herdr/scratch"
    root = Path(tempfile.mkdtemp(prefix="dq-cli-", dir=scratch))
    demo = Demo(root, native, pi)
    manifest = demo.package / "vqro-extension.toml"
    manifest.write_text(manifest.read_text().replace('command = ["python3", "provider.py"]', 'command = ["python3", "provider.py", "--decision-delay-seconds", "2"]'))
    images = json.loads((root / "IMAGES.json").read_text())
    images["package_sha256"]["vqro-extension.toml"] = hashlib.sha256(manifest.read_bytes()).hexdigest()
    images["private_delay_floor_seconds"] = 2
    demo.save("IMAGES.json", images)
    print(root, flush=True)

    def deadline(seconds):
        return (dt.datetime.now(dt.timezone.utc) + dt.timedelta(seconds=seconds)).isoformat(timespec="milliseconds")

    def filing(title, reversible, auto_at=None):
        options = [{"key": "keep", "label": "Keep local result"}, {"key": "change", "label": "Change local plan"}]
        args = ["decision", title, "--done-when", "Choice recorded.", "--options", json.dumps(options), "--default", "keep", "--reversible", str(reversible).lower(), "--waiting-cost", "Local work waits."]
        return args + (["--auto-at", auto_at] if auto_at else [])

    def human(token, args, expected=0, readable=False):
        # Generic neutral shell, not an outside-test-process actor flag.
        if "--op" in args and "--caller" not in args:
            args = [*args, "--caller", json.dumps(demo.human_binding)]
        out, err, status = [root / f"receipts/{token}.{suffix}" for suffix in ("out", "err", "status")]
        command = shlex.join(["vqro", "task", *args]) + f" > {shlex.quote(str(out))} 2> {shlex.quote(str(err))}; printf '%s' \"$?\" > {shlex.quote(str(status))}"
        demo.cli("pane", "run", human_pane, command)
        demo.wait_file(status)
        result = {"code": int(status.read_text()), "stdout": out.read_text(), "stderr": err.read_text()}
        demo.save(f"receipts/{token}.json", result)
        assert result["code"] == expected, (token, result)
        if expected or readable:
            return result
        value = json.loads(result["stdout"])
        if args == ["whoami"]:
            demo.human_binding = value["caller"]
        return value

    try:
        demo.start()
        demo.cli("plugin", "link", str(demo.package))
        first = demo.cli("workspace", "create", "--cwd", str(root / "work"), "--no-focus")["result"]["root_pane"]["pane_id"]
        second = demo.cli("pane", "split", "--pane", first, "--direction", "right", "--no-focus")["result"]["pane"]["pane_id"]
        human_pane = demo.cli("pane", "split", "--pane", second, "--direction", "down", "--no-focus")["result"]["pane"]["pane_id"]
        demo.cli("pane", "wait-output", human_pane, "--regex", r"\$", "--timeout", "5000")
        demo.remember_pane(human_pane)
        demo.launch("decision-a", first)
        demo.launch("decision-b", second)
        a = demo.agent("decision-a", "a-who", ["whoami"])["caller"]
        b = demo.agent("decision-b", "b-who", ["whoami"])["caller"]
        h = human("human-who", ["whoami"])["caller"]
        assert a["class"] == b["class"] == "agent" and a["execution"] != b["execution"]
        assert h["class"] == "human_unverified"
        manual = demo.agent("decision-a", "manual-file", filing("Human-only irreversible choice?", False))["task"]
        denied = demo.agent("decision-b", "agent-answer-denied", ["choose", manual["id"], "change", "--rev", "1"], expected=1)
        assert "human-only" in denied["stderr"]
        irreversible = demo.agent("decision-b", "irreversible-denied", filing("No automatic irreversible choice", False, deadline(8)), expected=1)
        assert "irreversible" in irreversible["stderr"]
        floor = demo.agent("decision-a", "floor-denied", filing("Too short", True, deadline(1)), expected=1)
        assert "human window" in floor["stderr"]
        automatic = demo.agent("decision-b", "auto-file", filing("Keep the reversible result?", True, deadline(8)))["task"]
        assert automatic["creator"] == b and automatic["state"] == "open"
        # No view/read/poll during the deadline interval.
        time.sleep(10)
        read_started = dt.datetime.now(dt.timezone.utc)
        applied = human("auto-get", ["get", automatic["id"]])["task"]
        resolution = applied["decision"]["resolution"]
        assert applied["state"] == "done" and resolution["by"] == "policy" and resolution["option"] == "keep"
        assert dt.datetime.fromisoformat(resolution["applied_at"]) < read_started and resolution["filer"] == b
        digest = human("digest-before-look", ["decisions", "--json"])
        assert [task["id"] for task in digest["needs_you_now"]] == [manual["id"]]
        assert digest["counts"] == {"auto": 1, "revert": 0} and len(digest["auto_since_look"]) == 1
        text = human("glance", ["decisions"], readable=True)["stdout"]
        assert "1 pending; 1 auto since last look" in text and "irreversible" in text and "applied late by" in text
        seen_args = ["decisions-seen", "--through", str(digest["ack_through"]), "--op", "original-last-look"]
        original_seen = human("seen", seen_args)
        assert human("digest-after-look", ["decisions", "--json"])["auto_since_look"] == []
        reverted = human("revert", ["revert", applied["id"], "--rev", str(applied["revision"])])["task"]
        assert reverted["state"] == "open" and reverted["decision"]["auto_at"] is None
        time.sleep(3)
        assert human("no-reapply", ["get", reverted["id"]])["task"]["state"] == "open"
        chosen = human("choose", ["choose", reverted["id"], "change", "--rev", str(reverted["revision"]), "--note", "Human changes the plan."])["task"]
        assert chosen["decision"]["resolution"]["by"] == "human"
        finality = human("human-final-denied", ["revert", chosen["id"], "--rev", str(chosen["revision"])], expected=1)
        assert "policy answer" in finality["stderr"]
        human("manual-answer", ["choose", manual["id"], "keep", "--rev", "1"])
        before_restart = human("log-before-restart", ["decision-log"])
        # One provider restart, including a due default while it was absent.
        cold = demo.agent("decision-a", "cold-file", filing("Catch up after provider restart?", True, deadline(8)))["task"]
        demo.cli("plugin", "disable", "vqro.tasks")
        time.sleep(10)
        demo.cli("plugin", "enable", "vqro.tasks")
        caught = human("restart-get", ["get", cold["id"]])["task"]
        assert caught["state"] == "done" and caught["decision"]["resolution"]["late_ms"] >= 1000
        assert human("seen-replay", seen_args) == original_seen
        after_restart = human("log-after-restart", ["decision-log"])
        assert after_restart["events"][:len(before_restart["events"])] == before_restart["events"]
        final = human("final-digest", ["decisions", "--json"])
        assert final["counts"] == {"auto": 2, "revert": 1} and len(final["auto_since_look"]) == 1
        human("final-look", ["decisions-seen", "--through", str(final["ack_through"])])
        assert human("no-unseen-auto", ["decisions", "--json"])["auto_since_look"] == []
        demo.save("RESULT.json", {"passed": True, "filer_a": a, "filer_b": b, "human": h, "idle_before_first_get": True, "revert_no_reapply": True, "human_answer_final": True, "one_provider_restart_catchup": True, "historical_ack_replay": True, "digest": final, "scope": "Linux trusted code, cooperative human, real two model-free Pis, no model turns or external rollback"})
    finally:
        demo.cleanup()
    print("PASS: two-Pi CLI file / human answer / idle default / guarded revert / log / digest / explicit seen / restart", flush=True)
    return root


if __name__ == "__main__":
    run(Path(sys.argv[1]), Path(sys.argv[2]) if len(sys.argv) > 2 else Path.home() / ".local/bin/pi")
