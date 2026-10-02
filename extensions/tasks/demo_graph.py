"""Real public CLI graph acceptance; Demo owns the private server/Pi lifecycle."""
import json
from pathlib import Path
import shlex
import time

CRITERION = "The observable result is recorded."


def run(demo):
    demo.start()
    demo.cli("plugin", "link", str(Path(__file__).resolve().parent))
    human = demo.cli("workspace", "create", "--cwd", str(demo.root / "work"), "--no-focus")["result"]["root_pane"]["pane_id"]
    a = demo.cli("pane", "split", "--pane", human, "--direction", "right", "--no-focus")["result"]["pane"]["pane_id"]
    b = demo.cli("pane", "split", "--pane", human, "--direction", "down", "--no-focus")["result"]["pane"]["pane_id"]
    demo.remember_pane(human)
    demo.launch("task-a", a)
    demo.launch("task-b", b)
    owner_a = demo.agent("task-a", "a-identity", ["whoami"])["caller"]
    owner_b = demo.agent("task-b", "b-identity", ["whoami"])["caller"]
    owner_human = demo.human(human, "human-identity", ["whoami"])["caller"]
    assert owner_a["class"] == owner_b["class"] == "agent" and owner_a != owner_b

    def get(task_id):
        return demo.cli("task", "get", task_id)["task"]

    def agent(name, token, args, expected=0):
        if args[0] in ("create", "decision"):
            args = [*args, "--done-when", CRITERION]
        return demo.agent(name, token, args, expected=expected)

    def change(name, token, task, action, text=None, expected=0, **flags):
        args = [action, task["id"]]
        if text is not None:
            if action == "report":
                text = "\n".join("> " + line + "\nEvidence: " + text for line in task["done_when"].split("\n"))
            args.append(text)
        args += ["--rev", str(task["revision"]), "--op", token]
        for key, value in flags.items():
            args += ["--" + key.replace("_", "-"), str(value)]
        result = agent(name, token, args, expected)
        return result["task"] if expected == 0 else result

    nodes = [{"key": "root", "title": "Approved task graph", "owner": owner_a, "done_when": "All child outcomes are verified.", "estimate_work_ms": 30000, "estimate_wait_ms": 10000, "brief_path": "work/brief.md"},
             {"key": "helper", "parent": "root", "title": "Helper proof", "owner": owner_b, "done_when": CRITERION},
             {"key": "peer", "parent": "root", "title": "Parallel lead proof", "owner": owner_a, "done_when": CRITERION},
             {"key": "after", "parent": "root", "title": "Verify both prerequisites", "owner": owner_a, "done_when": CRITERION, "depends_on": ["helper", "peer"]}]
    demo.save("plan.json", {"nodes": nodes})
    rejected = agent("task-a", "agent-root-plan-denied", ["plan", str(demo.root / "plan.json")], expected=1)
    assert "human approval" in rejected["stderr"]
    plan = demo.human(human, "human-plan", ["plan", str(demo.root / "plan.json"), "--op", "approved-plan"])
    assert demo.human(human, "human-plan-replay", ["plan", str(demo.root / "plan.json"), "--op", "approved-plan"]) == plan
    root, helper, peer, after = [get(plan["tasks"][key]) for key in ("root", "helper", "peer", "after")]
    ready = demo.cli("task", "ready")
    assert {helper["id"], peer["id"]}.issubset({row["id"] for row in ready["tasks"]})
    assert after["id"] not in {row["id"] for row in ready["tasks"]}
    demo.save("initial-ready.json", ready)
    root = change("task-a", "root-start", root, "start")
    rejected = change("task-a", "premature-start", after, "start", expected=1)
    assert "dependencies" in rejected["stderr"]
    rejected = change("task-b", "completion-cycle", helper, "depends", on=json.dumps([root["id"]]), expected=1)
    assert "cycle" in rejected["stderr"]
    demo.save("bad-plan.json", {"nodes": [{"key": "valid", "title": "valid", "done_when": CRITERION}, {"key": "bad", "title": "missing criteria"}]})
    before_count = len(demo.cli("task", "tree", "--json")["tasks"])
    agent("task-a", "atomic-import-denied", ["plan", str(demo.root / "bad-plan.json"), "--parent", root["id"], "--rev", str(root["revision"])], expected=1)
    assert len(demo.cli("task", "tree", "--json")["tasks"]) == before_count
    demo.save("addition.json", {"nodes": [{"key": "addition", "title": "Private addition", "done_when": CRITERION}]})
    rejected = agent("task-a", "foreign-subtree-denied", ["plan", str(demo.root / "addition.json"), "--parent", helper["id"], "--rev", str(helper["revision"])], expected=1)
    assert "not owned" in rejected["stderr"]
    assert len(demo.cli("task", "tree", "--json")["tasks"]) == before_count
    own = agent("task-a", "own-step", ["create", "Bounded private step", "--parent", root["id"], "--op", "own-step"])["task"]
    own = change("task-a", "own-start", own, "start")
    own = change("task-a", "own-done", own, "done", evidence="> " + CRITERION + "\nEvidence: step observed in private root")
    assert own["state"] == "done"
    helper = change("task-b", "b-start", helper, "start")
    helper = change("task-b", "b-block", helper, "block", "Need the check")
    helper = change("task-b", "b-resume", helper, "start")
    agent("task-b", "missing-report-evidence", ["report", helper["id"], "Trust me", "--rev", str(helper["revision"])], expected=1)
    helper = change("task-b", "b-report", helper, "report", "helper proof is visible")
    rejected = change("task-b", "self-accept-denied", helper, "accept", expected=1)
    assert "own work" in rejected["stderr"]
    helper = change("task-a", "parent-accept", helper, "accept")
    assert after["id"] not in {row["id"] for row in demo.cli("task", "ready")["tasks"]}
    helper = change("task-b", "b-done", helper, "done")
    peer = change("task-a", "peer-start", peer, "start")
    peer = change("task-a", "peer-report", peer, "report", "parallel lead proof is visible")
    peer = demo.human(human, "human-accept-peer", ["accept", peer["id"], "--rev", str(peer["revision"])])["task"]
    assert after["id"] in {row["id"] for row in demo.cli("task", "ready")["tasks"]}
    peer = change("task-a", "peer-done", peer, "done")
    after = change("task-a", "after-start", after, "start")
    after = change("task-a", "after-actuals", after, "update", actual_work_ms=1000, actual_wait_ms=500)
    after = change("task-a", "after-report", after, "report", "both prerequisite receipts inspected")
    after = demo.human(human, "human-accept-after", ["accept", after["id"], "--rev", str(after["revision"])])["task"]
    change("task-a", "after-done", after, "done")
    decision = agent("task-a", "a-decision", ["decision", "Keep the bounded graph?", "--parent", root["id"]])["task"]
    agent("task-a", "answer-denied", ["answer", decision["id"], "Yes", "--rev", "1"], expected=1)
    demo.human(human, "human-answer", ["answer", decision["id"], "Yes, keep the bounded graph", "--rev", "1"])
    demo.agent("task-a", "lost-reply", drop={"action": "create", "parent": root["id"], "title": "Lost reply child", "done_when": CRITERION, "operation_id": "lost-reply"})
    deadline = time.monotonic() + 8
    while time.monotonic() < deadline:
        receipt = demo.cli("task", "op", "lost-reply", "--caller", json.dumps(owner_a))
        if receipt["found"]:
            break
        time.sleep(.1)
    else:
        raise RuntimeError("lost reply receipt unavailable")
    lost = receipt["outcome"]["task"]
    replay = agent("task-a", "lost-replay", ["create", "Lost reply child", "--parent", root["id"], "--op", "lost-reply"])
    assert replay["task"]["id"] == lost["id"]
    demo.save("lost-reply-receipt.json", receipt)
    before = demo.cli("task", "tree", "--json")
    stable = lambda tree: [{key: value for key, value in row.items() if key != "elapsed_ms"} for row in tree["tasks"]]
    demo.save("before-restart.json", before)
    demo.cli("plugin", "disable", "vqro.tasks")
    demo.cli("task", "list", expected=1)
    marker = demo.root / "terminal-independent.txt"
    demo.cli("pane", "run", human, "printf '%s' provider-independent > " + shlex.quote(str(marker)))
    demo.wait_file(marker)
    demo.cli("plugin", "enable", "vqro.tasks")
    assert stable(before) == stable(demo.cli("task", "tree", "--json"))
    demo.stop()
    demo.start()
    assert stable(before) == stable(demo.cli("task", "tree", "--json"))
    old_report = demo.cli("task", "op", "b-report", "--caller", json.dumps(owner_b))
    assert old_report["outcome"]["task"]["state"] == "reported"
    demo.launch("task-replacement", a)
    replacement = demo.agent("task-replacement", "replacement-identity", ["whoami"])["caller"]
    assert replacement["terminal_id"] == owner_a["terminal_id"] and replacement["execution"] != owner_a["execution"]
    agent("task-replacement", "replacement-denied", ["start", lost["id"], "--rev", "1"], expected=1)
    refused = agent("task-replacement", "original-scope-retry-denied", ["create", "Lost reply child", "--parent", root["id"], "--op", "lost-reply", "--caller", json.dumps(owner_a)], expected=1)
    assert "original caller changed" in refused["stderr"]
    root = demo.human(human, "root-reassign", ["delegate", root["id"], "--owner", json.dumps(replacement), "--rev", str(root["revision"])])["task"]
    # Parent takeover preserves a never-handoff own step's status; no self-accept shortcut.
    lost = change("task-replacement", "own-step-takeover", lost, "delegate", owner=json.dumps(replacement))
    lost = change("task-replacement", "replacement-start", lost, "start")
    change("task-replacement", "replacement-own-done", lost, "done", evidence="> " + CRITERION + "\nEvidence: replacement rechecked visible result")
    root = change("task-replacement", "root-report", root, "report", "all child receipts and decision inspected")
    root = demo.human(human, "human-accept-root", ["accept", root["id"], "--rev", str(root["revision"])])["task"]
    change("task-replacement", "root-done", root, "done")
    final = demo.cli("task", "tree", "--json")
    assert len(final["tasks"]) == 7 and all(row["state"] == "done" and row["owed_to"] is None for row in final["tasks"])
    demo.save("final-tree.json", final)
    # Supporting deadline measurement via the real host, separate from 7-task acceptance.
    maximum = []
    for index in range(100):
        refs = [f"n{prior}" for prior in range(max(0, index - 4), index)]
        if 5 <= index < 15:
            refs.append("n0")
        maximum.append({"key": f"n{index}", "title": f"bounded {index}", "done_when": CRITERION, "depends_on": refs})
    demo.save("maximum-plan.json", {"nodes": maximum})
    started = time.monotonic()
    measured = demo.human(human, "maximum-plan", ["plan", str(demo.root / "maximum-plan.json"), "--op", "maximum-plan"])
    seconds = time.monotonic() - started
    assert measured["counts"] == {"tasks": 100, "dependencies": 400}
    demo.save("maximum-plan-measurement.json", {"nodes": 100, "edges": 400, "public_cli_round_trip_seconds": seconds,
                                                "plan_file_bytes": (demo.root / "maximum-plan.json").stat().st_size,
                                                "host_action_deadline_seconds": 2.5, "receipt": measured})
    result = {"result": "passed", "real_pi_agents": 2, "model_calls": 0, "tasks": 7,
              "atomic_plan_and_replay": True, "accepted_dependencies_release_ready": True,
              "parent_accepts_never_self_accept": True, "own_step_evidence_and_takeover": True,
              "decision_human_only": True, "lost_reply_original_scope": True,
              "durable_restart_fields_exact_elapsed_refreshed": True, "human_class": owner_human["class"],
              "separate_maximum_import_tasks": 100, "maximum_import_seconds": seconds}
    demo.save("RESULT.json", result)
    return result
