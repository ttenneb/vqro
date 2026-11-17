#!/bin/bash
# Opt-in real-backend TUI smoke: a private herdr-core, a private prim core (native herdr), and the real
# `prim tui` in a private tmux harness, with a restricted PATH. Leaves nothing running.
# Usage: tests/e2e_tui_smoke.sh <prim binary>
# Checks: new tab + input echo; input after a 30 MB output flood; core restart -> reconnect, reattach and
# input; quit while the core cannot start; no leftover processes (tmux servers matched by socket path).
set -u
P=${1:?prim binary}
REPO=$(cd "$(dirname "$0")/.." && pwd)
HC=$HOME/.local/bin/herdr-core
R=$(mktemp -d /tmp/ptsXXXXXX)
mkdir -p "$R"/{home,st,cfg,xst,run,bin,ext}
chmod 700 "$R/run"
cp "$REPO"/extensions/*.py "$REPO"/extensions/defaults.json "$R/ext/"
for t in python3 sh cat ls env head yes; do ln -sf "$(command -v $t)" "$R/bin/$t"; done
cat > "$R/env.sh" <<EOF
export HOME=$R/home PATH=$R/bin TERM=xterm-256color SHELL=/bin/sh PRIM_STATE_DIR=$R/st PRIM_SOCKET=$R/p.sock PRIM_EXTENSIONS_DIR=$R/ext XDG_CONFIG_HOME=$R/cfg XDG_STATE_HOME=$R/xst XDG_RUNTIME_DIR=$R/run HC_SESSION=smoke HC_SOCKET=$R/h.sock PRIM_HERDR="$HC connect"
EOF
printf '. %s/env.sh; exec %s tui\n' "$R" "$P" > "$R/tui.sh"
T="/usr/bin/tmux -S $R/t.sock"
pass=0; fail=0
check() { if [ "$2" = 1 ]; then echo "PASS $1"; pass=$((pass+1)); else echo "FAIL $1"; fail=$((fail+1)); fi; }
seen() { $T capture-pane -p | grep -c -- "$1"; }
cores() { for pid in $(pgrep -f "$P serve"); do tr '\0' '\n' < /proc/$pid/environ 2>/dev/null | grep -q "PRIM_SOCKET=$R/p.sock" && echo $pid; done; }
# Everything of this run: environment mentions $R, or (tmux renames its server) a unix socket under $R.
ours() {
  python3 - "$R" <<'PY'
import os, sys
root = sys.argv[1].encode()
paths = {}
for line in open("/proc/net/unix").read().splitlines()[1:]:
    f = line.split()
    if len(f) >= 8 and f[7].encode().startswith(root):
        paths[f[6]] = f[7]
for pid in filter(str.isdigit, os.listdir("/proc")):
    if int(pid) == os.getpid():
        continue
    try:
        if root in open(f"/proc/{pid}/environ", "rb").read():
            print(pid); continue
        for fd in os.listdir(f"/proc/{pid}/fd"):
            link = os.readlink(f"/proc/{pid}/fd/{fd}")
            if link.startswith("socket:[") and link[8:-1] in paths:
                print(pid); break
    except OSError:
        pass
PY
}

env -i /usr/bin/tmux -S "$R/t.sock" new-session -d -x 120 -y 30 "/bin/bash $R/tui.sh"; sleep 4
$T send-keys C-g; sleep 0.4; $T send-keys n; sleep 2
$T send-keys -l 'echo smoke-A'; $T send-keys Enter; sleep 1
check "new tab + input echo" $([ "$(seen smoke-A)" -ge 1 ] && echo 1 || echo 0)
$T send-keys -l 'yes flood | head -c 30000000; echo flood-done'; $T send-keys Enter; sleep 6
$T send-keys -l 'echo smoke-F'; $T send-keys Enter; sleep 2
check "input after 30 MB output flood" $([ "$(seen smoke-F)" -ge 1 ] && echo 1 || echo 0)
for pid in $(cores); do kill "$pid"; done; sleep 5
$T send-keys -l 'echo smoke-B'; $T send-keys Enter; sleep 2
check "core restart: reconnect + reattach + input" $([ "$(seen smoke-B)" -ge 1 ] && echo 1 || echo 0)
# Failed reconnect: the state dir cannot be written, so the auto-started core cannot start.
chmod 500 "$R/st"; for pid in $(cores); do kill "$pid"; done; sleep 4
$T send-keys C-g; sleep 0.4; $T send-keys q; sleep 2
check "quit while core cannot start" $($T list-sessions >/dev/null 2>&1 && echo 0 || echo 1)
chmod 700 "$R/st"
# Cleanup: tmux, prim cores, private hc terminals and core.
$T kill-server 2>/dev/null; for pid in $(cores); do kill "$pid"; done; sleep 1
env -i bash -c ". $R/env.sh; for t in \$($HC ls --all --json | python3 -c 'import json,sys; print(\" \".join(x[\"address\"] for x in json.load(sys.stdin)[\"terminals\"]))'); do $HC kill \$t >/dev/null 2>&1; $HC rm \$t >/dev/null 2>&1; done; $HC server stop >/dev/null"
for _ in 1 2 3 4 5; do [ -z "$(ours)" ] && break; sleep 1; done
left=$(ours | tr '\n' ' ')
check "no leftover processes" $([ -z "$left" ] && echo 1 || echo 0)
[ -n "$left" ] && { echo "leftover: $left"; kill -9 $left 2>/dev/null; }
rm -rf "$R"
echo "passed $pass failed $fail"
[ "$fail" = 0 ]
