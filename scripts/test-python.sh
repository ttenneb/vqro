#!/bin/sh
# Test tooling only. Print one absolute Python >= 3.11 path; never use Apple's 3.9.
set -u
usable() {
    [ -x "$1" ] && "$1" -I -c 'import sys; sys.exit(sys.version_info < (3, 11))' >/dev/null 2>&1
}
if [ "${VQRO_PYTHON+x}" = x ]; then
    candidate=$VQRO_PYTHON
    case "$candidate" in */*) ;; *) candidate=$(command -v "$candidate" 2>/dev/null || true) ;; esac
    if usable "$candidate"; then
        case "$candidate" in /*) printf '%s\n' "$candidate" ;; *) printf '%s/%s\n' "$PWD" "$candidate" ;; esac
        exit 0
    fi
    echo 'tests: VQRO_PYTHON must name an executable Python >= 3.11' >&2
    exit 2
fi
rest=${PATH:-}
while :; do
    directory=${rest%%:*}
    candidate=${directory:-.}/python3
    if usable "$candidate"; then
        case "$candidate" in /*) printf '%s\n' "$candidate" ;; *) printf '%s/%s\n' "$PWD" "$candidate" ;; esac
        exit 0
    fi
    case "$rest" in *:*) rest=${rest#*:} ;; *) break ;; esac
done
echo 'tests: need Python >= 3.11 on PATH, or set VQRO_PYTHON' >&2
exit 2
