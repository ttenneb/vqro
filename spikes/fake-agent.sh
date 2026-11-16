#!/bin/sh
# Emits OSC 7501 program-status reports like an agent would.
osc() { printf '\033]7501;%s\033\\' "$1"; }
msg=$(printf 'which file?' | base64)
echo "fake-agent starting"
osc "state=working:app=fake"; echo "working"; sleep 1.5
osc "state=blocked:app=fake:kind=question:msg=$msg"; echo "blocked"; sleep 1.5
osc "state=done:app=fake"; echo "done"; sleep 1.5
