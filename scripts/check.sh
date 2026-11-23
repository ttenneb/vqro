#!/bin/bash
# Apple's /bin/bash is 3.2. Require 4.4: namerefs need 4.3, but empty arrays
# under set -u (e.g. strict=() below) are safe only since 4.4.
if (( BASH_VERSINFO[0] < 4 || (BASH_VERSINFO[0] == 4 && BASH_VERSINFO[1] < 4) )); then
  IFS=: read -r -a check_bash_dirs <<< "$PATH"
  for check_bash_dir in "${check_bash_dirs[@]}"; do
    check_bash=${check_bash_dir:-.}/bash
    if [ -x "$check_bash" ] && "$check_bash" -c '(( BASH_VERSINFO[0] > 4 || (BASH_VERSINFO[0] == 4 && BASH_VERSINFO[1] >= 4) ))' 2>/dev/null; then
      exec "$check_bash" "$0" "$@"
    fi
  done
  echo 'check: Bash 4.4+ is required; install a modern bash (e.g. brew install bash) and put it on PATH.' >&2
  exit 2
fi
# Release gate. Run `scripts/check.sh full` before installing.
#
#   scripts/check.sh quick   licence policy/notices + cargo build (zero warnings) + cargo test + Python unittests
#   scripts/check.sh full    quick + every opt-in private suite (tmux, providers, herdr-core), the e2e
#                            scripts, TUI smoke/scale probe, the default scale profile, and a leftover check
#   scripts/check.sh timing  only TUI scale, scale harness and bridge memory; strict budget grading,
#                            using prebuilt release vqro and an explicit VQRO_SCALE_TEST_EXE
#
# Every step runs under `env -i` with private paths: no live vqro, hc/herdr-core or tmux server is
# touched (suites start their own private ones and stop only those). Prints a PASS/FAIL table and writes
# a JSON receipt to $CHECK_RECEIPT (default <target>/check-receipt.json), plus an exclusive read-only
# check-receipt-<commit>-<stamp>.json sibling. Exit 77 means an intentional optional-dependency SKIP,
# never PASS; skips are non-failing and listed explicitly in the table, summary and receipt.
# Needs: cargo, python3, Bash 4.4+; Linux full harnesses pin /usr/bin/tmux 3.7b
# and ~/.local/bin/herdr-core. Non-Linux skips are recorded, never PASS.
set -u
if ! CHECK_SYSTEM=$(uname -s) || [ -z "$CHECK_SYSTEM" ]; then
  echo 'check: cannot determine platform: uname -s failed or returned empty output' >&2
  exit 2
fi
# Capture the caller before any subprocess/exec can outlive it during bootstrap.
CHECK_OWNER_PID=$PPID
CHECK_OWNER_BIRTH=
if [ -r "/proc/$CHECK_OWNER_PID/stat" ] && IFS= read -r CHECK_OWNER_STAT < "/proc/$CHECK_OWNER_PID/stat"; then
  read -r -a CHECK_OWNER_FIELDS <<< "${CHECK_OWNER_STAT##*) }"
  CHECK_OWNER_BIRTH=${CHECK_OWNER_FIELDS[19]:-}
fi
SKIP_EXIT=77
MODE=${1:-quick}
case "$MODE" in quick|full|timing) ;; *) echo "usage: $0 quick|full|timing" >&2; exit 2 ;; esac
if [ "$MODE" = timing ] && [ "$CHECK_SYSTEM" != Linux ]; then
  echo 'check: timing mode requires Linux (/proc, pidfds)' >&2
  exit 2
fi
# Full gates share a bounded pool, then run latency/RSS/scale probes without sibling steps.
CHECK_JOBS=${CHECK_JOBS:-6}
if ! [[ "$CHECK_JOBS" =~ ^[1-9][0-9]?$ ]] || (( CHECK_JOBS > 64 )); then
  echo "check: CHECK_JOBS must be an integer from 1 to 64" >&2; exit 2
fi
# Change only our own priority; do not raise an already-nicer caller's priority.
NICE=$(ps -o ni= -p "$$" 2>/dev/null | tr -d ' ')
if [[ "$NICE" =~ ^-?[0-9]+$ ]] && (( NICE < 10 )); then
  renice -n 10 -p "$$" >/dev/null 2>&1 || echo "check: could not lower CPU priority" >&2
fi
REPO=$(cd "$(dirname "$0")/.." && pwd)
VQRO_PYTHON=$(/bin/sh "$REPO/scripts/test-python.sh") || exit 2
export VQRO_PYTHON
# A transient service owns this run's cgroup, including reparented/session-detached daemons.
# Unlike a scope, its native main-process lifetime tears down all members on interruption.
# The supervisor also watches the original launcher/parent with pidfds, including startup races.
# Under env -i recover only the user bus; never replay a failed service launch unscoped.
# Without a manager the leftover check remains UNSCOPED/report-only: never kill other runs.
if [ "$CHECK_SYSTEM" = Linux ] && [ -z "${VQRO_CHECK_SUPERVISED:-}" ]; then
  if [ -z "${XDG_RUNTIME_DIR:-}" ] && [ -O "/run/user/$(id -u)" ]; then export XDG_RUNTIME_DIR=/run/user/$(id -u); fi
  if [ -z "${DBUS_SESSION_BUS_ADDRESS:-}" ] && [ -S "${XDG_RUNTIME_DIR:-/nonexistent}/bus" ]; then
    export DBUS_SESSION_BUS_ADDRESS="unix:path=$XDG_RUNTIME_DIR/bus"
  fi
  if command -v systemd-run >/dev/null 2>&1 && systemctl --user show --property=Version >/dev/null 2>&1; then
    exec "$VQRO_PYTHON" -I "$REPO/scripts/check_lifetime.py" --owner "$CHECK_OWNER_PID" "$CHECK_OWNER_BIRTH" -- "$REPO/scripts/check.sh" "$@"
  fi
  echo "check: no systemd user manager (set XDG_RUNTIME_DIR and DBUS_SESSION_BUS_ADDRESS); the leftover check is UNSCOPED: report only, no kill, no failure" >&2
fi
# A private alias also covers overrides whose basename is not python3.
PYTHON_BIN=$(mktemp -d /tmp/vqro-python-XXXXXX) || exit 2
trap 'rm -rf "$PYTHON_BIN"' EXIT
ln -s "$VQRO_PYTHON" "$PYTHON_BIN/python3" || exit 2
export PATH="$PYTHON_BIN:$PATH"
CGROUP=
if [ "$CHECK_SYSTEM" = Linux ] && [ -n "${VQRO_CHECK_SUPERVISED:-}" ]; then
  CGROUP=$(sed -n 's/^0:://p' /proc/self/cgroup)
  case "$CGROUP" in */vqro-check-*.service) [ "${CGROUP##*/}" = "${VQRO_CHECK_UNIT:-}" ] || CGROUP= ;; *) CGROUP= ;; esac
fi
TARGET=${CARGO_TARGET_DIR:-$REPO/target}
STAMP=$(date +%Y%m%d-%H%M%S)-$$
LOGS=$TARGET/check-logs/$STAMP
RECEIPT=${CHECK_RECEIPT:-$TARGET/check-receipt.json}
mkdir -p "$LOGS"
# Short private root for anything that needs socket paths (the scale harness run dir).
RUN=$(mktemp -d /tmp/pchkXXXXXX)
CARGO=(env -i HOME="$HOME" PATH="$PATH" VQRO_PYTHON="$VQRO_PYTHON" CARGO_TARGET_DIR="$TARGET")
CLEAN_PATH=/usr/bin:/bin
# Portable suites may resolve tmux from PATH (e.g. Homebrew); pinned Linux harnesses do not.
if [ "$CHECK_SYSTEM" != Linux ]; then
  CHECK_TMUX=$(command -v tmux || :)
  if [ -n "$CHECK_TMUX" ]; then CLEAN_PATH="$(dirname "$CHECK_TMUX"):$CLEAN_PATH"; fi
fi
CLEAN_PATH="$(dirname "$VQRO_PYTHON"):$CLEAN_PATH"
# Keep the Bash that passed bootstrap first (even without optional tmux), so an older bash beside the chosen Python
# or tmux can never shadow it. Only PYTHON_BIN, private and holding python3 alone, precedes it.
if [ "$CHECK_SYSTEM" != Linux ]; then CLEAN_PATH="$(dirname "$BASH"):$CLEAN_PATH"; fi
CLEAN_PATH="$PYTHON_BIN:$CLEAN_PATH"
CLEAN=(env -i HOME="$HOME" PATH="$CLEAN_PATH" VQRO_PYTHON="$VQRO_PYTHON")
# An explicit compiler selection must survive every clean build environment.
if [ -n "${ZIG:-}" ]; then CARGO+=("ZIG=$ZIG"); CLEAN+=("ZIG=$ZIG"); fi
STEPS=$LOGS/steps.tsv
: > "$STEPS"
cd "$REPO" || exit 2

# Processes that belong to test roots: environment or a unix socket under a test root (tmux renames its
# server to "tmux: server", so its command line no longer shows the socket path).
snapshot() {
  [ "$CHECK_SYSTEM" = Linux ] || return 0
  python3 - "$RUN" <<'PY'
import os, sys
roots = [b"/tmp/pt", b"/tmp/pe2", b"/tmp/ppd", b"/tmp/pps", b"/tmp/pst", b"/tmp/pke", b"/tmp/phc", b"/tmp/pft", b"/tmp/ppe", b"/tmp/pev", b"/tmp/pscale", b"/tmp/vqro-tmux",
         b"/tmp/pnf", b"/tmp/pws", b"/tmp/ptw", b"/tmp/prex", sys.argv[1].encode()]
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

# Registration order remains the public table/receipt order, independent of execution order.
STEP_NAMES=()
NEXT_RESULT=0
step() {  # Capture argv as an array: paths/arguments never pass through eval or a shell string.
  local index=${#STEP_NAMES[@]} name=$1; shift
  STEP_NAMES+=("$name")
  declare -ga "CHECK_STEP_$index"
  local -n command="CHECK_STEP_$index"
  command=("$@")
}
timing_python() {
  # Also safe for direct/unscoped gates: never depend on supervisor sanitization.
  /usr/bin/env -i HOME="$HOME" PATH="$PYTHON_BIN:$(dirname "$VQRO_PYTHON"):/usr/bin:/bin" \
    VQRO_GATE_SLOT_DIR="${VQRO_GATE_SLOT_DIR:-}" VQRO_GATE_SLOT_HELD="${VQRO_GATE_SLOT_HELD:-}" \
    VQRO_GATE_EXCLUSIVE="${VQRO_GATE_EXCLUSIVE:-0}" python3 -I "$@"
}
timed() {
  case "$1" in e2e-tui-scale-budgets|scale-harness|pi-bridge-memory) return 0 ;; *) return 1 ;; esac
}
# Keep Linux admission/commands unchanged; these harnesses depend on Linux-only isolation,
# /proc measurements/cleanup, or an intentionally pinned /usr/bin/tmux, not PATH tmux.
platform_skip_reason() {
  [ "$CHECK_SYSTEM" != Linux ] || return 1
  case "$1" in
    systemd-supervision) echo 'systemd supervision requires Linux' ;;
    opt-in:tmux|opt-in:tmux_provider|opt-in:undo_attribution|opt-in:provider_wire)
      echo 'pinned /usr/bin/tmux harness requires Linux' ;;
    opt-in:herdr_provider|opt-in:fd_lifecycle|opt-in:accept_limits|e2e-providers|e2e-rex|e2e-tui-providers|e2e-tui-paste-dropdown|e2e-tui-sidebar-tree|e2e-tui-pick-search|e2e-tui-keys|e2e-tui-pane-basics|e2e-same-setup|e2e-tui-workspaces|e2e-scale-herdr|e2e-text-client|e2e-opentui|e2e-tui-smoke|e2e-item-streams)
      echo 'harness requires Linux /proc and/or pinned /usr/bin/tmux' ;;
    e2e-pi-provider|e2e-pi-rust-default|e2e-agents-ui|e2e-codex-launcher|e2e-agent-stop)
      echo 'private Linux namespaces and /proc are required' ;;
    e2e-testing-unit|e2e-testing-integration|e2e-testing-perf|e2e-testing-finish-gate)
      echo 'testing harness requires Linux /proc and Unix process groups' ;;
    pi-bridge-memory|e2e-tui-scale-budgets|scale-harness)
      echo 'timing measurements and contention monitor require Linux /proc and pidfds' ;;
    *) return 1 ;;
  esac
}
platform_skip() { echo "SKIP: $*"; return "$SKIP_EXIT"; }
run_step() {
  local index=$1 name=${STEP_NAMES[$1]} start=$SECONDS rc
  local -n command="CHECK_STEP_$index"
  local log="$LOGS/$name.log" result=PASS
  local skip_reason
  if skip_reason=$(platform_skip_reason "$name"); then
    platform_skip "$skip_reason" > "$log" 2>&1; rc=$?
  elif timed "$name"; then
    local pid facts="$LOGS/$name-contention.json" strict=()
    if [ "$MODE" = timing ] || [ "${VQRO_GATE_EXCLUSIVE:-0}" = 1 ]; then strict=(--strict); fi
    ( "${command[@]}" ) > "$log" 2>&1 & pid=$!
    timing_python "$REPO/scripts/timing_contention.py" --pid "$pid" --output "$facts" >> "$log" 2>&1
    wait "$pid"; rc=$?
    timing_python "$REPO/scripts/timing_contention.py" --output "$facts" --grade "$rc" "${strict[@]}" >> "$log" 2>&1; rc=$?
  else
    "${command[@]}" > "$log" 2>&1; rc=$?
  fi
  case "$rc" in 0) result=PASS ;; "$SKIP_EXIT") result=SKIP ;; *) result=FAIL ;; esac
  # A separate atomic result per worker; only the parent appends the ordered receipt.
  printf '%s\t%s\t%s\t%s\n' "$name" "$result" "$((SECONDS-start))" "$log" > "$LOGS/step-$index.tmp"
  mv "$LOGS/step-$index.tmp" "$LOGS/step-$index.tsv"
}
show_ready() {
  local name result seconds log
  while [ "$NEXT_RESULT" -lt "${#STEP_NAMES[@]}" ] && [ -f "$LOGS/step-$NEXT_RESULT.tsv" ]; do
    IFS=$'\t' read -r name result seconds log < "$LOGS/step-$NEXT_RESULT.tsv"
    printf '%-28s %s  %4ss\n' "$name" "$result" "$seconds"
    cat "$LOGS/step-$NEXT_RESULT.tsv" >> "$STEPS"
    NEXT_RESULT=$((NEXT_RESULT+1))
  done
}
sensitive() {
  case "$1" in
    # The default Rust suite includes the inventory core-lock hold-time assertion too.
    cargo-test|history-budget|scale-harness|e2e-testing-perf|e2e-tui-scale-budgets|e2e-sidebar-scale|e2e-item-streams|pi-bridge-memory|opt-in:core_storm|opt-in:accept_limits|opt-in:fd_lifecycle|e2e-scale-herdr) return 0 ;;
    *) return 1 ;;
  esac
}
BUILD_STEP=build-zero-warnings
run_steps() {
  local index prefix=0
  # Timing mode does not build or run unrelated suites. Missing prebuilt artifacts
  # are explicit step failures; no cold build is hidden inside the timing window.
  if [ "$MODE" = timing ]; then
    for ((index=0; index<${#STEP_NAMES[@]}; index++)); do run_step "$index"; show_ready; done
    return
  fi
  # Build first: every step registered up to and including the build (found by name, never by position, so adding
  # a step before it can't move the build into the concurrent phase) runs serially, so every concurrent suite sees
  # complete binaries. --all-targets includes --bins.
  for ((index=0; index<${#STEP_NAMES[@]}; index++)); do
    if [ "${STEP_NAMES[$index]}" = "$BUILD_STEP" ]; then prefix=$((index+1)); break; fi
  done
  if [ "$prefix" -eq 0 ]; then
    echo "check: no $BUILD_STEP step registered" >&2
    # A nonzero runner status alone is not the gate result: leave explicit failure rows for
    # the receipt, including a guard row when there were no registered steps at all.
    if [ "${#STEP_NAMES[@]}" -eq 0 ]; then STEP_NAMES=("$BUILD_STEP"); fi
    for ((index=0; index<${#STEP_NAMES[@]}; index++)); do
      printf 'not run: required %s step is not registered\n' "$BUILD_STEP" > "$LOGS/${STEP_NAMES[$index]}.log"
      printf '%s\tFAIL\t0\t%s\n' "${STEP_NAMES[$index]}" "$LOGS/${STEP_NAMES[$index]}.log" > "$LOGS/step-$index.tsv"
    done
    show_ready
    return 2
  fi
  for ((index=0; index<prefix; index++)); do run_step "$index"; show_ready; done
  local build_result=FAIL ignored
  if ! IFS=$'\t' read -r ignored build_result ignored < "$LOGS/step-$((prefix-1)).tsv"; then
    build_result=FAIL
    printf 'build stopped before recording its result\n' > "$LOGS/$BUILD_STEP.log"
    printf '%s\tFAIL\t0\t%s\n' "$BUILD_STEP" "$LOGS/$BUILD_STEP.log" > "$LOGS/step-$((prefix-1)).tsv"
  fi
  if [ "$build_result" != PASS ]; then
    for ((index=prefix; index<${#STEP_NAMES[@]}; index++)); do
      printf 'not run: build failed\n' > "$LOGS/${STEP_NAMES[$index]}.log"
      printf '%s\tFAIL\t0\t%s\n' "${STEP_NAMES[$index]}" "$LOGS/${STEP_NAMES[$index]}.log" > "$LOGS/step-$index.tsv"
    done
    show_ready
    return 1
  fi
  if [ "$MODE" = quick ]; then
    for ((index=prefix; index<${#STEP_NAMES[@]}; index++)); do run_step "$index"; show_ready; done
    return
  fi
  echo "check: independent phase (CHECK_JOBS=$CHECK_JOBS); timing/scale probes run serially afterward"
  for ((index=prefix; index<${#STEP_NAMES[@]}; index++)); do
    sensitive "${STEP_NAMES[$index]}" && continue
    # Count running jobs, not unreaped statuses: even instant failures cannot lose a pool slot.
    while [ "$(jobs -pr | wc -l)" -ge "$CHECK_JOBS" ]; do wait -n || :; show_ready; done
    run_step "$index" &
    show_ready
  done
  wait
  # A worker stopped before publishing its result is a failed step, never a missing/green row.
  for ((index=prefix; index<${#STEP_NAMES[@]}; index++)); do
    sensitive "${STEP_NAMES[$index]}" && continue
    if [ ! -f "$LOGS/step-$index.tsv" ]; then
      printf 'check worker stopped before recording its result\n' >> "$LOGS/${STEP_NAMES[$index]}.log"
      printf '%s\tFAIL\t0\t%s\n' "${STEP_NAMES[$index]}" "$LOGS/${STEP_NAMES[$index]}.log" > "$LOGS/step-$index.tsv"
    fi
  done
  show_ready
  echo "check: final serial timing/scale phase (independent workers drained)"
  for ((index=prefix; index<${#STEP_NAMES[@]}; index++)); do
    if sensitive "${STEP_NAMES[$index]}"; then run_step "$index"; show_ready; fi
  done
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
  local executable record="$LOGS/scale-artifact.json" helper="$REPO/scripts/scale_artifact.py"
  if [ "$MODE" = timing ]; then
    if [ -z "${VQRO_SCALE_TEST_EXE:-}" ]; then
      echo "FAIL timing: prebuild the release scale test and pass its path (VQRO_SCALE_TEST_EXE)" >&2
      return 1
    fi
    # Stable Cargo --no-run can compile. Timing admission does not invoke it at all.
    executable=$(timing_python "$helper" validate --manifest "$REPO/Cargo.toml" --target-dir "$TARGET" \
      --executable "$VQRO_SCALE_TEST_EXE" --record "$record") || return 1
  else
    # Full gates already permit builds. Keep the package/target selection evidence,
    # rather than guessing among stale hashed executables in a long-lived target.
    local messages="$LOGS/scale-prebuild.jsonl"
    "${CARGO[@]}" cargo test --release --test scale --no-run --message-format=json > "$messages" \
      || { cat "$messages"; return 1; }
    executable=$(timing_python "$helper" select --manifest "$REPO/Cargo.toml" --target-dir "$TARGET" \
      --record "$record" < "$messages") || return 1
  fi
  "${CARGO[@]}" VQRO_SCALE_RUN_DIR="$RUN" VQRO_SCALE_RECEIPT="$LOGS/scale-receipt.json" VQRO_SCALE_DEFER_TIMING_GRADE=1 \
    "$executable" --ignored --exact scale_load --nocapture || return 1
  # Cargo/test success is required too: a stale receipt can never excuse a panic.
  timing_python - "$LOGS/scale-receipt.json" <<'PY'
import json, sys
r = json.load(open(sys.argv[1]))
sys.exit({"pass": 0, "budget": 78}.get(r.get("failure_class"), 1))
PY
}

history_budget() {
  "${CARGO[@]}" cargo test --release --test history_budget -- --include-ignored --test-threads=1 --nocapture
}

item_streams() {  # agent-v0 §4 regression gate (P3): 200 agents x 3 viewers + the Python SDK viewer + stalled, restart, session switch
  "${CARGO[@]}" cargo build --release --bin vqro --bin vqro-fake-items --bin vqro-items-viewer || return 1
  "${CLEAN[@]}" CARGO_TARGET_DIR="$TARGET" python3 tests/e2e_item_streams.py "$LOGS/item-streams" --agents 200 --rate 2 --timescale 0.25 --py-viewer
}

tui_scale_probe() {
  if [ "$MODE" != timing ]; then "${CARGO[@]}" cargo build --release --bin vqro || return 1; fi
  "${CLEAN[@]}" python3 tests/e2e_tui_scale.py "$TARGET" --receipt "$LOGS/tui-scale-receipt.json"
}

libghostty_release_safe() {  # the terminal engines with libghostty-vt's safety checks on (Zig ReleaseSafe): the
  # known crash traces (P0-1 and P0-2) and 2000 fuzz rounds per engine. Its own target directory,
  # so the main build keeps its ReleaseFast library.
  "${CARGO[@]}" CARGO_TARGET_DIR="$TARGET/release-safe" LIBGHOSTTY_VT_OPTIMIZE=ReleaseSafe VQRO_FUZZ_ITERS=2000 \
    cargo test --lib -- engines_survive tui::ghostty
}

opt_in() {  # opt_in SUITE: every test of an integration suite, including the ignored (private) ones
  "${CARGO[@]}" cargo test --test "$1" -- --include-ignored --test-threads=1
}

# Processes of this run still alive, excluding this script, its supervisor ancestors and live helpers
# (its descendants at this moment). Reparented or renamed daemons stay in the cgroup and are counted.
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
supervisors = set(ancestors(me))
for pid in map(int, procs):
    if me not in ancestors(pid) and pid not in supervisors:
        try:
            cmd = open(f"/proc/{pid}/cmdline", "rb").read().replace(b"\0", b" ").decode(errors="replace").strip()
        except OSError:
            continue
        print(f"{pid}\t{cmd[:120]}")
PY
}

BEFORE=$(snapshot | cut -f1 | sort)
if [ "$CHECK_SYSTEM" = Linux ] && [ -n "${VQRO_CHECK_INJECT_LEAK:-}" ]; then
  # Self-test of the leftover check: one deliberately leaked, detached process of this run.
  ( setsid sleep 300 < /dev/null > /dev/null 2>&1 & )  # reparented once the subshell exits, like a real leak
fi
echo "vqro check ($MODE) at $(git describe --always --dirty 2>/dev/null); logs $LOGS"
if [ "$CHECK_SYSTEM" != Linux ]; then step systemd-supervision platform_skip 'systemd supervision requires Linux'; fi
if [ "$MODE" != timing ]; then
step licenses "${CARGO[@]}" python3 scripts/license_check.py --check
step vendor-libghostty "${CARGO[@]}" python3 scripts/check_vendor.py
step manifest-deps "${CLEAN[@]}" python3 scripts/check_manifest_deps.py
step build-zero-warnings no_warnings
step cargo-test "${CARGO[@]}" cargo test
step python-unittest "${CLEAN[@]}" python3 -m unittest discover -s tests -p 'test_*.py'
windows_check() {  # native Windows compiles with zero warnings (type check only; Zig cross-builds libghostty-vt
  # for windows-gnu, MSVC needs the Windows SDK). Not Windows conformance: nothing runs on Windows here.
  local installed
  if ! installed=$(env -i HOME="$HOME" PATH="$PATH" rustup target list --installed); then
    echo "FAIL Windows check: could not obtain installed targets from rustup" >&2
    return 1
  fi
  env -i PATH="$PATH" grep -qx x86_64-pc-windows-gnu <<<"$installed" \
    || { echo "SKIP Windows check: rustup target x86_64-pc-windows-gnu not installed"; return "$SKIP_EXIT"; }
  local cargo=("${CARGO[@]}" CARGO_TARGET_DIR="$TARGET/windows" cargo check --target x86_64-pc-windows-gnu --all-targets)
  "${cargo[@]}" 2>&1 | env -i PATH="$PATH" tee /dev/stderr | env -i PATH="$PATH" grep -q '^warning' && return 1
  "${cargo[@]}" >/dev/null 2>&1
}
step windows-check windows_check
opentui_model() {  # the OpenTUI trial client's model against the shared fixtures, its card colour and roles; needs Bun, not its packages
  local bun; bun=$(command -v bun) || { echo "SKIP OpenTUI trial client model: bun not found"; return "$SKIP_EXIT"; }
  (cd extensions/clients/opentui && env -i HOME="$HOME" PATH="$(dirname "$bun"):/usr/bin:/bin" "$bun" test src/conformance.test.ts src/cardcolor.test.ts src/roles.test.ts)
}
step opentui-model opentui_model
fi
if [ "$MODE" = full ]; then
  for suite in tmux tmux_timeout tmux_provider herdr_provider provider_wire provider_errors undo_attribution core_storm fd_lifecycle accept_limits operational_crash; do
    step "opt-in:$suite" opt_in "$suite"
  done
  step history-budget history_budget
  step libghostty-release-safe libghostty_release_safe
  step e2e-testing-unit "${CLEAN[@]}" CARGO_TARGET_DIR="$TARGET" python3 tests/e2e_testing_unit.py
  step e2e-testing-integration "${CLEAN[@]}" CARGO_TARGET_DIR="$TARGET" python3 tests/e2e_testing_integration.py
  step e2e-testing-perf "${CLEAN[@]}" CARGO_TARGET_DIR="$TARGET" python3 tests/e2e_testing_perf.py
  step e2e-testing-finish-gate "${CLEAN[@]}" CARGO_TARGET_DIR="$TARGET" python3 tests/e2e_testing_finish_gate.py
  step e2e-providers "${CLEAN[@]}" python3 tests/e2e_providers.py "$TARGET"
  step e2e-rex "${CLEAN[@]}" python3 tests/e2e_rex.py "$TARGET"  # optional: SKIP without /usr/bin/rex
  step e2e-coordination-review "${CLEAN[@]}" python3 tests/e2e_coordination_review.py --binary "$TARGET/debug/vqro"
  step e2e-tui-providers "${CLEAN[@]}" python3 tests/e2e_tui_providers.py "$TARGET"
  step e2e-tui-paste-dropdown "${CLEAN[@]}" python3 tests/e2e_tui_paste_dropdown.py "$TARGET"
  step e2e-tui-sidebar-tree "${CLEAN[@]}" python3 tests/e2e_tui_sidebar_tree.py "$TARGET"
  step e2e-tui-pick-search "${CLEAN[@]}" python3 tests/e2e_tui_pick_search.py "$TARGET"
  step e2e-tui-keys "${CLEAN[@]}" python3 tests/e2e_tui_keys.py "$TARGET"
  step e2e-tui-pane-basics "${CLEAN[@]}" python3 tests/e2e_tui_pane_basics.py "$TARGET"
  step e2e-same-setup "${CLEAN[@]}" python3 tests/e2e_same_setup.py "$TARGET"
  step e2e-workspace-tabs "${CLEAN[@]}" python3 tests/e2e_workspace_tabs.py "$TARGET"
  step e2e-tui-workspaces "${CLEAN[@]}" python3 tests/e2e_tui_workspaces.py "$TARGET"
  step e2e-herdr-drift "${CLEAN[@]}" python3 tests/e2e_herdr_drift.py "$TARGET" --receipt "$LOGS/herdr-drift-receipt.json"
  step e2e-scale-herdr scale_probe
  step e2e-text-client "${CLEAN[@]}" python3 tests/e2e_text_client.py "$TARGET"
  step e2e-opentui "${CLEAN[@]}" python3 tests/e2e_opentui.py "$TARGET"
  step e2e-tui-smoke "${CLEAN[@]}" bash tests/e2e_tui_smoke.sh "$TARGET/debug/vqro"
  step e2e-pi-provider "${CLEAN[@]}" python3 tests/pi_provider_e2e.py --run --large-workspace --probe-terminal-invoke --vqro "$TARGET/debug/vqro" --hc "$HOME/.local/bin/herdr-core"
  step e2e-pi-rust-default "${CLEAN[@]}" python3 tests/pi_provider_e2e.py --run --rust-bridge --agents-extension --large-workspace --probe-terminal-invoke --vqro "$TARGET/debug/vqro" --hc "$HOME/.local/bin/herdr-core"
  step e2e-agents-ui "${CLEAN[@]}" python3 tests/pi_provider_e2e.py --run --agents-ui --large-workspace --probe-terminal-invoke --vqro "$TARGET/debug/vqro" --hc "$HOME/.local/bin/herdr-core"
  step e2e-codex-launcher "${CLEAN[@]}" python3 tests/codex_launcher_e2e.py --run --vqro "$TARGET/debug/vqro" --bridge "$TARGET/debug/vqro-bridge-codex" --hc "$HOME/.local/bin/herdr-core"
  step e2e-claude-launcher "${CLEAN[@]}" python3 tests/claude_launcher_e2e.py --run --vqro "$TARGET/debug/vqro" --bridge "$TARGET/debug/vqro-bridge-claude" --hc "$HOME/.local/bin/herdr-core"
  step e2e-agent-stop "${CLEAN[@]}" python3 tests/e2e_agent_stop.py --run --vqro "$TARGET/debug/vqro" --hc "$HOME/.local/bin/herdr-core"
  step e2e-agent-origin "${CLEAN[@]}" python3 tests/e2e_agent_origin.py --vqro "$TARGET/debug/vqro"
  step e2e-claude-hook "${CLEAN[@]}" python3 tests/claude_hook_e2e.py --vqro "$TARGET/debug/vqro" --hook "$TARGET/debug/vqro-claude-hook"
fi
if [ "$MODE" = full ] || [ "$MODE" = timing ]; then
  step pi-bridge-memory "${CLEAN[@]}" python3 tests/pi_bridge_memory.py --report "$LOGS/pi-memory-receipt.json"
  if [ "$MODE" = full ]; then
    step e2e-sidebar-scale "${CLEAN[@]}" python3 tests/e2e_sidebar_scale.py "$TARGET/debug/vqro" --receipt "$LOGS/sidebar-scale-receipt.json"
  fi
  step e2e-tui-scale-budgets tui_scale_probe
  if [ "$MODE" = full ]; then step e2e-item-streams item_streams; fi
  step scale-harness scale_harness
fi
run_steps
sleep 2
if [ "$CHECK_SYSTEM" != Linux ]; then
  SCOPE="unavailable: /proc leftover scan requires Linux"
  LEFT=
elif [ -n "$CGROUP" ]; then
  SCOPE="cgroup $CGROUP"
  LEFT=$(scoped_leftovers | cut -f1 | tr '\n' ' ')
else
  SCOPE="unscoped heuristic (test roots; can match concurrent runs; report only, never killed or failed)"
  LEFT=$(comm -13 <(echo "$BEFORE") <(snapshot | cut -f1 | sort) | tr '\n' ' ')
fi
LEFT=${LEFT% }
if [ "$CHECK_SYSTEM" != Linux ]; then
  printf '%-28s SKIP  /proc leftover scan requires Linux\n' "no-leftover-processes"
  printf 'no-leftover-processes\tSKIP\t0\t-\n' >> "$STEPS"
elif [ -n "$LEFT" ]; then
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

timing_python - "$STEPS" "$RECEIPT" "$MODE" "$(git describe --always --dirty 2>/dev/null)" "$LOGS" "$LEFT" "$SCOPE" "$STAMP" "$REPO" "$TARGET" "$(git rev-parse HEAD 2>/dev/null)" <<'PY'
import hashlib, json, os, sys, time
from pathlib import Path
steps_file, receipt, mode, commit, logs, left, scope, stamp = sys.argv[1:9]
steps = [dict(zip(("name", "result", "seconds", "log"), l.split("\t"))) for l in open(steps_file).read().splitlines() if l]
for s in steps:
    s["seconds"] = int(s["seconds"])
    if s["result"] == "SKIP": s["result"] = "skip"
    facts = Path(logs) / (s["name"] + "-contention.json")
    if facts.exists():
        s["contention"] = json.loads(facts.read_text())
        s["reason"] = s["contention"].get("grading_reason")
skipped = [s["name"] for s in steps if s["result"] == "skip"]
failed = [s["name"] for s in steps if s["result"] not in ("PASS", "UNSCOPED", "skip")]
r = {"kind": "vqro.check-receipt", "version": 1, "mode": mode, "commit": commit, "finished_unix": int(time.time()),
     "result": "fail" if failed else "pass", "failed": failed, "skipped": skipped, "steps": steps, "leftover_pids": left.split(), "leftover_scope": scope, "logs": logs}
# This identifies what the caller graded, not which revision produced a binary.
# No new build/cache framework: names, hashes and mtimes make the attestation auditable.
checkout = Path(sys.argv[9]).resolve() if len(sys.argv) > 9 else Path.cwd().resolve()
target = Path(sys.argv[10]).resolve() if len(sys.argv) > 10 else Path(logs).parent.parent.resolve()
checkout_commit = sys.argv[11] if len(sys.argv) > 11 else None
def artifact(path):
    try:
        digest = hashlib.sha256()
        with path.open("rb") as source:
            for chunk in iter(lambda: source.read(1024 * 1024), b""):
                digest.update(chunk)
            stat = os.fstat(source.fileno())
        return {"path": str(path), "sha256": digest.hexdigest(), "mtime_unix_ns": stat.st_mtime_ns}
    except OSError as error:
        return {"path": str(path), "unavailable": str(error)}
try:
    selected_scale = json.loads((Path(logs) / "scale-artifact.json").read_text())
    if selected_scale.get("kind") != "vqro.scale-test-artifact" or not isinstance(selected_scale.get("path"), str):
        selected_scale = None
except (OSError, ValueError, AttributeError):
    selected_scale = None
r["build_provenance"] = {"assurance": "caller-attested, not revision-bound",
    "checkout": str(checkout), "checkout_commit": checkout_commit,
    "release_vqro": artifact(target / "release/vqro"),
    "scale_test_selection": selected_scale,
    "scale_test_artifacts": [artifact(Path(selected_scale["path"]))] if selected_scale else []}
scale = logs + "/scale-receipt.json"
try:
    sr = json.load(open(scale))
    r["scale"] = {"result": sr["result"], "exceeded": sr["exceeded"], "commit": sr["commit"]}
except (OSError, ValueError, KeyError):
    pass
latest = Path(receipt)
# Keep Git's full label in the data; tag path separators must not create subdirectories.
label = commit.replace("/", "_") or "unknown"
immutable = latest.with_name(f"check-receipt-{label}-{stamp}.json")
payload = json.dumps(r, indent=1) + "\n"
with immutable.open("x") as archive:
    archive.write(payload)
# Preserve the caller's umask/read permissions, removing only write permission.
immutable.chmod(immutable.stat().st_mode & 0o555)
if latest != immutable: latest.write_text(payload)
passed = len(steps) - len(failed) - len(skipped)
print(f"{'PASS' if not failed else 'FAIL'}: {passed}/{len(steps)}, {len(skipped)} skipped; receipt {receipt}; immutable {immutable}")
sys.exit(1 if failed else 0)
PY
