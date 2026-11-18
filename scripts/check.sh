#!/bin/bash
# Release gate. Run `scripts/check.sh full` before installing.
#
#   scripts/check.sh quick   licence policy/notices + cargo build (zero warnings) + cargo test + Python unittests
#   scripts/check.sh full    quick + every opt-in private suite (tmux, providers, herdr-core), the e2e
#                            scripts, TUI smoke/scale probe, the default scale profile, and a leftover check
#
# Every step runs under `env -i` with private paths: no live vqro, hc/herdr-core or tmux server is
# touched (suites start their own private ones and stop only those). Prints a PASS/FAIL table and writes
# a JSON receipt to $CHECK_RECEIPT (default <target>/check-receipt.json); step logs go next to it.
# Needs: cargo, python3, /usr/bin/tmux 3.7b and ~/.local/bin/herdr-core for `full`.
set -u
MODE=${1:-quick}
case "$MODE" in quick|full) ;; *) echo "usage: $0 quick|full" >&2; exit 2 ;; esac
# Run inside our own transient user scope (a cgroup): every process this run starts stays in it, even
# daemons that reparent or rename themselves, and nothing of a concurrent run is in it. The leftover check
# then lists exactly our processes. systemd-run --user needs the user bus: XDG_RUNTIME_DIR (and
# DBUS_SESSION_BUS_ADDRESS); under `env -i` they are recovered from /run/user/<uid> when that is ours.
# Without a scope the leftover check is UNSCOPED: it only reports (it can match concurrent runs by other
# agents), never kills and never fails the gate.
if [ -z "${VQRO_CHECK_SCOPED:-}" ]; then
  if [ -z "${XDG_RUNTIME_DIR:-}" ] && [ -O "/run/user/$(id -u)" ]; then export XDG_RUNTIME_DIR=/run/user/$(id -u); fi
  if [ -z "${DBUS_SESSION_BUS_ADDRESS:-}" ] && [ -S "${XDG_RUNTIME_DIR:-/nonexistent}/bus" ]; then
    export DBUS_SESSION_BUS_ADDRESS="unix:path=$XDG_RUNTIME_DIR/bus"
  fi
  if command -v systemd-run >/dev/null 2>&1 && systemd-run --user --scope --quiet --collect -- true >/dev/null 2>&1; then
    VQRO_CHECK_SCOPED=1 exec systemd-run --user --scope --quiet --collect --unit="vqro-check-$(date +%s)-$$" -- "$0" "$@"
  fi
  echo "check: no systemd user scope (set XDG_RUNTIME_DIR and DBUS_SESSION_BUS_ADDRESS); the leftover check is UNSCOPED: report only, no kill, no failure" >&2
fi
CGROUP=
if [ -n "${VQRO_CHECK_SCOPED:-}" ]; then
  CGROUP=$(sed -n 's/^0:://p' /proc/self/cgroup)
  case "$CGROUP" in */vqro-check-*.scope) ;; *) CGROUP= ;; esac
fi
REPO=$(cd "$(dirname "$0")/.." && pwd)
TARGET=${CARGO_TARGET_DIR:-$REPO/target}
STAMP=$(date +%Y%m%d-%H%M%S)
LOGS=$TARGET/check-logs/$STAMP
RECEIPT=${CHECK_RECEIPT:-$TARGET/check-receipt.json}
mkdir -p "$LOGS"
# Short private root for anything that needs socket paths (the scale harness run dir).
RUN=$(mktemp -d /tmp/pchkXXXXXX)
CARGO=(env -i HOME="$HOME" PATH="$PATH" CARGO_TARGET_DIR="$TARGET")
CLEAN=(env -i HOME="$HOME" PATH=/usr/bin:/bin)
STEPS=$LOGS/steps.tsv
: > "$STEPS"
cd "$REPO" || exit 2

# Processes that belong to test roots: environment or a unix socket under a test root (tmux renames its
# server to "tmux: server", so its command line no longer shows the socket path).
snapshot() {
  python3 - "$RUN" <<'PY'
import os, sys
roots = [b"/tmp/pt", b"/tmp/pe2", b"/tmp/ppd", b"/tmp/pke", b"/tmp/phc", b"/tmp/pft", b"/tmp/ppe", b"/tmp/pev", b"/tmp/pscale", b"/tmp/vqro-tmux",
         b"/tmp/pnf", sys.argv[1].encode()]
socks = {}
for line in open("/proc/net/unix").read().splitlines()[1:]:
    f = line.split()
    if len(f) >= 8 and any(f[7].encode().startswith(r) for r in roots):
        socks[f[6]] = f[7]
me = os.getpid()
for pid in filter(str.isdigit, os.listdir("/proc")):
    if int(pid) in (me, os.getppid()):
        continue
    try:
        env = open(f"/proc/{pid}/environ", "rb").read()
        hit = any(r in env for r in roots)
        if not hit:
            for fd in os.listdir(f"/proc/{pid}/fd"):
                link = os.readlink(f"/proc/{pid}/fd/{fd}")
                if link.startswith("socket:[") and link[8:-1] in socks:
                    hit = True
                    break
        if hit:
            cmd = open(f"/proc/{pid}/cmdline", "rb").read().replace(b"\0", b" ").decode(errors="replace").strip()
            print(f"{pid}\t{cmd[:120]}")
    except OSError:
        pass
PY
}

step() {  # step NAME CMD...: run, log, record
  local name=$1; shift
  local log="$LOGS/$name.log" start=$SECONDS rc
  printf '%-28s ' "$name"
  "$@" > "$log" 2>&1; rc=$?
  local secs=$((SECONDS - start)) result=PASS
  [ $rc -ne 0 ] && result=FAIL
  printf '%s  %4ss\n' "$result" "$secs"
  printf '%s\t%s\t%s\t%s\n' "$name" "$result" "$secs" "$log" >> "$STEPS"
}

no_warnings() {  # build everything; any compiler warning fails (cargo replays cached ones)
  "${CARGO[@]}" cargo build --all-targets 2>&1 | tee /dev/stderr | grep -q '^warning' && return 1
  "${CARGO[@]}" cargo build --all-targets >/dev/null 2>&1
}

scale_probe() {  # e2e_scale_herdr.py: default profile; no leftovers, no missed, no lost echo, no closed floods
  local out; out=$("${CLEAN[@]}" python3 tests/e2e_scale_herdr.py "$TARGET") || return 1
  echo "$out"
  echo "$out" | python3 -c '
import json, sys
t = sys.stdin.read(); r = json.loads(t[t.index("{"):t.rindex("}") + 1])
ok = not r.get("left_running") and r.get("missed") == 0 and r["echo_idle_during_floods"]["lost_after_5s"] == 0 and not r.get("stream_ends")
sys.exit(0 if ok else 1)'
}

scale_harness() {
  "${CARGO[@]}" VQRO_SCALE_RUN_DIR="$RUN" VQRO_SCALE_RECEIPT="$LOGS/scale-receipt.json" \
    cargo test --release --test scale -- --ignored --exact scale_load --nocapture
}

history_budget() {
  "${CARGO[@]}" cargo test --release --test history_budget -- --include-ignored --test-threads=1 --nocapture
}

tui_scale_probe() {
  "${CARGO[@]}" cargo build --release --bin vqro || return 1
  "${CLEAN[@]}" python3 tests/e2e_tui_scale.py "$TARGET" --receipt "$LOGS/tui-scale-receipt.json"
}

opt_in() {  # opt_in SUITE: every test of an integration suite, including the ignored (private) ones
  "${CARGO[@]}" cargo test --test "$1" -- --include-ignored --test-threads=1
}

# Processes of this run still alive: everything in our scope that is not this script or its live helpers
# (its descendants at this moment). Reparented or renamed daemons stay in the scope and are counted.
scoped_leftovers() {
  python3 - "/sys/fs/cgroup$CGROUP" "$$" <<'PY'
import os, sys
procs = open(os.path.join(sys.argv[1], "cgroup.procs")).read().split()
me, mine = int(sys.argv[2]), set()
def ancestors(pid):
    seen = []
    while pid > 1:
        seen.append(pid)
        try:
            pid = int(open(f"/proc/{pid}/stat").read().rsplit(")", 1)[1].split()[1])
        except (OSError, ValueError, IndexError):
            break
    return seen
for pid in map(int, procs):
    if me not in ancestors(pid) and pid != me:
        try:
            cmd = open(f"/proc/{pid}/cmdline", "rb").read().replace(b"\0", b" ").decode(errors="replace").strip()
        except OSError:
            continue
        print(f"{pid}\t{cmd[:120]}")
PY
}

BEFORE=$(snapshot | cut -f1 | sort)
if [ -n "${VQRO_CHECK_INJECT_LEAK:-}" ]; then
  # Self-test of the leftover check: one deliberately leaked, detached process of this run.
  ( setsid sleep 300 < /dev/null > /dev/null 2>&1 & )  # reparented once the subshell exits, like a real leak
fi
echo "vqro check ($MODE) at $(git describe --always --dirty 2>/dev/null); logs $LOGS"
step licenses "${CARGO[@]}" python3 scripts/license_check.py --check
step build-zero-warnings no_warnings
step cargo-test "${CARGO[@]}" cargo test
step python-unittest "${CLEAN[@]}" python3 -m unittest discover -s tests -p 'test_*.py'
if [ "$MODE" = full ]; then
  "${CARGO[@]}" cargo build --bins >/dev/null 2>&1
  for suite in tmux tmux_timeout tmux_provider herdr_provider provider_wire provider_errors undo_attribution core_storm fd_lifecycle accept_limits; do
    step "opt-in:$suite" opt_in "$suite"
  done
  step history-budget history_budget
  step e2e-providers "${CLEAN[@]}" python3 tests/e2e_providers.py "$TARGET"
  step e2e-tui-providers "${CLEAN[@]}" python3 tests/e2e_tui_providers.py "$TARGET"
  step e2e-tui-paste-dropdown "${CLEAN[@]}" python3 tests/e2e_tui_paste_dropdown.py "$TARGET"
  step e2e-tui-keys "${CLEAN[@]}" python3 tests/e2e_tui_keys.py "$TARGET"
  step e2e-herdr-drift "${CLEAN[@]}" python3 tests/e2e_herdr_drift.py "$TARGET" --receipt "$LOGS/herdr-drift-receipt.json"
  step e2e-scale-herdr scale_probe
  step e2e-tui-smoke "${CLEAN[@]}" bash tests/e2e_tui_smoke.sh "$TARGET/debug/vqro"
  step e2e-pi-provider "${CLEAN[@]}" python3 tests/pi_provider_e2e.py --run --large-workspace --probe-terminal-invoke --vqro "$TARGET/debug/vqro" --hc "$HOME/.local/bin/herdr-core"
  step e2e-agents-ui "${CLEAN[@]}" python3 tests/pi_provider_e2e.py --run --agents-ui --large-workspace --probe-terminal-invoke --vqro "$TARGET/debug/vqro" --hc "$HOME/.local/bin/herdr-core"
  step pi-bridge-memory "${CLEAN[@]}" python3 tests/pi_bridge_memory.py --report "$LOGS/pi-memory-receipt.json"
  step e2e-sidebar-scale "${CLEAN[@]}" python3 tests/e2e_sidebar_scale.py "$TARGET/debug/vqro" --receipt "$LOGS/sidebar-scale-receipt.json"
  step e2e-tui-scale-budgets tui_scale_probe
  step scale-harness scale_harness
fi
sleep 2
if [ -n "$CGROUP" ]; then
  SCOPE="cgroup $CGROUP"
  LEFT=$(scoped_leftovers | cut -f1 | tr '\n' ' ')
else
  SCOPE="unscoped heuristic (test roots; can match concurrent runs; report only, never killed or failed)"
  LEFT=$(comm -13 <(echo "$BEFORE") <(snapshot | cut -f1 | sort) | tr '\n' ' ')
fi
LEFT=${LEFT% }
if [ -n "$LEFT" ]; then
  for pid in $LEFT; do printf '%s %s\n' "$pid" "$(tr '\0' ' ' < /proc/$pid/cmdline 2>/dev/null | cut -c1-100)"; done > "$LOGS/leftovers.log"
  if [ -n "$CGROUP" ]; then
    printf '%-28s FAIL  pids %s (see %s)\n' "no-leftover-processes" "$LEFT" "$LOGS/leftovers.log"
    printf 'no-leftover-processes\tFAIL\t0\t%s\n' "$LOGS/leftovers.log" >> "$STEPS"
    # Recorded; now clean up what this run leaked (attributable only because they are in our scope).
    kill -9 $LEFT 2>/dev/null
  else
    # Unscoped: these may belong to other agents' concurrent runs. Report, never kill, never fail.
    printf '%-28s UNSCOPED  possible leftovers %s (may be other runs; see %s)\n' "no-leftover-processes" "$LEFT" "$LOGS/leftovers.log"
    printf 'no-leftover-processes\tUNSCOPED\t0\t%s\n' "$LOGS/leftovers.log" >> "$STEPS"
  fi
elif [ -z "$CGROUP" ]; then
  printf '%-28s UNSCOPED  none matched\n' "no-leftover-processes"
  printf 'no-leftover-processes\tUNSCOPED\t0\t-\n' >> "$STEPS"
else
  printf '%-28s PASS\n' "no-leftover-processes"
  printf 'no-leftover-processes\tPASS\t0\t-\n' >> "$STEPS"
fi
rm -rf "$RUN"

python3 - "$STEPS" "$RECEIPT" "$MODE" "$(git describe --always --dirty 2>/dev/null)" "$LOGS" "$LEFT" "$SCOPE" <<'PY'
import json, sys, time
steps_file, receipt, mode, commit, logs, left, scope = sys.argv[1:8]
steps = [dict(zip(("name", "result", "seconds", "log"), l.split("\t"))) for l in open(steps_file).read().splitlines() if l]
for s in steps:
    s["seconds"] = int(s["seconds"])
failed = [s["name"] for s in steps if s["result"] not in ("PASS", "UNSCOPED")]
r = {"kind": "vqro.check-receipt", "version": 1, "mode": mode, "commit": commit, "finished_unix": int(time.time()),
     "result": "fail" if failed else "pass", "failed": failed, "steps": steps, "leftover_pids": left.split(), "leftover_scope": scope, "logs": logs}
scale = logs + "/scale-receipt.json"
try:
    sr = json.load(open(scale))
    r["scale"] = {"result": sr["result"], "exceeded": sr["exceeded"], "commit": sr["commit"]}
except (OSError, ValueError, KeyError):
    pass
open(receipt, "w").write(json.dumps(r, indent=1) + "\n")
print(f"{'PASS' if not failed else 'FAIL'}: {len(steps) - len(failed)}/{len(steps)} steps; receipt {receipt}")
sys.exit(1 if failed else 0)
PY
