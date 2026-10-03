# Your first ten minutes with Vqro

You need `vqro`, Pi and Python 3 already on PATH, and your usual Pi model setup. No environment exports or per-agent configuration are needed. This is ordinary use; automated demos isolate HOME/config/sockets separately.

Pi that Vqro starts uses a **Vqro-only profile**: your usual auth, models and settings, but without the Herdr-coupled packages (`pi-input-gate`, `pi-tasking-herdr-adapter`, `rpiv-todo`), so a Vqro-started Pi does not load the adapter that contacts Herdr. This is a profile, not a sandbox: a deliberate fallback or `VQRO_NO_BUNDLED_DEFAULTS=1` loads your global Pi config. Vqro never writes your `~/.pi/agent` settings or packages; the profile references your `auth.json` and `models-store.json`, so Pi's normal token and catalog refresh can still update those files.

## 1. Open Vqro

<!-- launch -->
```sh
vqro
```

Keep it running. Create a workspace with **New** if needed. Run the next block **in a human shell pane inside Vqro**, not in the outside terminal: task writes need its current caller identity. Agent starts open their own panes and keep focus in your shell. Existing names are refused; choose fresh names if you already have task-a/task-b.

The native Tasks panel should already be empty and working, not **Tasks unavailable**, even before the first core action. To see the empty full Tasks pane, run this in your human shell, then select that shell again:

<!-- human-empty -->
```sh
vqro task tree
```

## 2. Start two agents and import a small plan

<!-- human-setup -->
```sh
vqro agent start task-a
vqro agent start task-b
cat > first-plan.json <<'JSON'
{"nodes":[
  {"key":"root","title":"Try the small-team workflow","owner":"task-a","done_when":"The decision and message are recorded."},
  {"key":"work","parent":"root","title":"Try delegated work","owner":"task-a","done_when":"The blocker is recorded."}
]}
JSON
vqro task plan first-plan.json > first-plan-result.json
```

The block writes `first-plan.json` and `first-plan-result.json`; use a fresh workspace directory, not one containing files with those names. Names resolve to exact current agent executions; the stored plan contains bindings, not names. Import is human-approved, not permission for an agent to start automatically. The files below share this workspace directory.

## 3. Ask task-a to run this block

Paste the block into **task-a's Pi** and ask it to execute it (do not run it in your human shell). It starts its root, delegates the child by name, asks a two-option human question, and records a message. Only a human can answer the decision.

<!-- agent-a -->
```sh
ROOT=$(python3 -c 'import json; print(json.load(open("first-plan-result.json"))["tasks"]["root"])')
WORK=$(python3 -c 'import json; print(json.load(open("first-plan-result.json"))["tasks"]["work"])')
vqro task start "$ROOT" --rev 1
vqro task delegate "$WORK" --to task-b --rev 1
vqro task decision 'Keep this bounded plan?' --parent "$ROOT" --done-when 'The human choice is recorded.' --options '[{"key":"keep","label":"Keep"},{"key":"change","label":"Change"}]' --default keep --reversible true --waiting-cost 'Work waits.' > first-decision.json
vqro message send task-b "task $WORK assigned; inspect vqro task get $WORK"
```

## 4. Ask task-b to run this block

<!-- agent-b -->
```sh
WORK=$(python3 -c 'import json; print(json.load(open("first-plan-result.json"))["tasks"]["work"])')
vqro task start "$WORK" --rev 2
vqro task block "$WORK" 'Waiting for the human choice.' --rev 3
vqro message claim
```

The Tasks badge now shows durable human/blocked attention. A claim confirms transport, not understanding or accepted work. With default activation off, messages do not start agent turns; an explicitly opted-in Pi adapter can wake/follow up.

## 5. Answer in your human shell

<!-- human-answer -->
```sh
DECISION=$(python3 -c 'import json; print(json.load(open("first-decision.json"))["task"]["id"])')
vqro task answer "$DECISION" keep --rev 1
vqro task tree
```

`tree` opens the full Tasks pane; select your shell again before typing more shell commands. The decision closes; task-b's explicit blocker remains until it resumes. Reporting work is not acceptance: task-a accepts its child's result, then reports its root for the human to accept. Use `vqro --skill` for those ownership/evidence conventions.

Run these fresh-state blocks once. Revisions deliberately protect against stale writes; inspect current state before changing it again. If a write or launch says its outcome is unknown, inspect the original operation/terminal first—never blindly repeat it, especially after a name is rebound.
