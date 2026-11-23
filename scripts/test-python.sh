#!/bin/sh
# Unix bootstrap only. Selection policy lives in portable test_python.py.
# Even a hung bootstrap shim is killed and waited for before trying another.
set -u
resolver=$(CDPATH= cd -- "${0%/*}" && pwd)/test_python.py
probe= watchdog=
cleanup() {
    if [ -n "$probe" ]; then kill -KILL "$probe" 2>/dev/null || :; wait "$probe" 2>/dev/null || :; fi
    if [ -n "$watchdog" ]; then kill "$watchdog" 2>/dev/null || :; wait "$watchdog" 2>/dev/null || :; fi
}
trap 'cleanup; exit 143' HUP INT TERM
bootstrap() {
    "$1" -I -X utf8 "$resolver" &
    probe=$!
    (
        sleeper=
        trap '[ -z "$sleeper" ] || { kill "$sleeper" 2>/dev/null; wait "$sleeper" 2>/dev/null; }; exit 0' TERM INT
        /bin/sleep 15 & sleeper=$!
        wait "$sleeper"
        kill -KILL "$probe" 2>/dev/null
    ) &
    watchdog=$!
    wait "$probe"; result=$?
    probe=
    kill "$watchdog" 2>/dev/null || :
    wait "$watchdog" 2>/dev/null || :
    watchdog=
    return "$result"
}
if [ "${VQRO_PYTHON+x}" = x ]; then
    candidate=$VQRO_PYTHON
    case "$candidate" in */*) ;; *) candidate=$(command -v "$candidate" 2>/dev/null || true) ;; esac
    if [ -n "$candidate" ] && [ -x "$candidate" ] && bootstrap "$candidate"; then exit 0; fi
    echo 'tests: VQRO_PYTHON must name an executable Python >= 3.11 (bootstrap timeout is 15s)' >&2
    exit 2
fi
rest=${PATH:-}
while :; do
    directory=${rest%%:*}
    candidate=${directory:-.}/python3
    if [ -x "$candidate" ] && bootstrap "$candidate"; then exit 0; fi
    case "$rest" in *:*) rest=${rest#*:} ;; *) break ;; esac
done
echo 'tests: need Python >= 3.11 on PATH, or set VQRO_PYTHON' >&2
exit 2
