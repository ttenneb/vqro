#!/usr/bin/python3
"""Minimal fake of `tmux -C attach-session` for deterministic reply-timing tests (not a tmux).

Invoked as the tmux binary: `fake -N -S SOCKET -f /dev/null -C attach-session -t SESSION`.
The client clears its environment, so settings come from `SOCKET.conf` (JSON):
  {"slow": {"list-panes": 0.6}}  -- sleep before answering that command; like a busy tmux, every
                                     later reply waits too (commands are answered in order).
  {"attach_delay": 0.3}          -- sleep before the attach block (a slow start or a busy host).
  {"spawn_gone": true}         -- provider fixture: a session exists, new-window succeeds,
                                     but list-panes sees no pane (already exited after sizing).
Every control command line is appended to `SOCKET.log`.
"""
import json
import shlex
import sys
import time

socket = sys.argv[sys.argv.index("-S") + 1]
try:
    with open(socket + ".conf") as f:
        conf = json.load(f)
except FileNotFoundError:
    conf = {}
if conf.get("spawn_gone") and "-C" not in sys.argv:
    if "list-sessions" in sys.argv:
        print("work")
    elif "-V" in sys.argv:
        print("tmux 3.7b")
    elif "show-options" in sys.argv:
        print("/fixture/default-shell")
    else:
        sys.exit(2)
    sys.exit(0)

slow = conf.get("slow", {})
out = sys.stdout.buffer
number = 0


def block(lines, error=False):
    global number
    number += 1
    stamp = int(time.time())
    out.write(f"%begin {stamp} {number} 1\n".encode())
    for line in lines:
        out.write(line.encode() + b"\n")
    out.write(f"%{'error' if error else 'end'} {stamp} {number} 1\n".encode())
    out.flush()


time.sleep(conf.get("attach_delay", 0))
block([])  # the attach block
options = {}
for raw in sys.stdin.buffer:
    line = raw.decode().strip()
    if not line:
        continue
    with open(socket + ".log", "a") as log:
        log.write(line + "\n")
    args = shlex.split(line)
    command = args[0]
    if command in slow:
        time.sleep(slow[command])
    if command == "display-message":
        text = args[-1]
        if "#{pid}" in text:
            nonce = "0" * 32 if conf.get("spawn_gone") else options.get('@vqro_control_instance', '')
            text = f"4242:1700000000:{nonce}"
        block([text])
    elif command == "set-option":
        name, value = args[-2], args[-1]
        if "-o" in args and name in options:
            block([f"already set: {name}"], error=True)
        else:
            options[name] = value
            block([])
    elif command == "new-window" and conf.get("spawn_gone"):
        block(["%1:@1"])
    elif command == "list-panes":
        block([] if conf.get("spawn_gone") else ["%0", "%1"])
    else:
        block([])
