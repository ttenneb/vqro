#!/usr/bin/env python3
"""Test fixture: print the bytes each keypress sends, as hex, one key per line.
Optional argument: `kitty` (pushes kitty keyboard flag 1, "disambiguate") or `mok` (xterm modifyOtherKeys 2).
Ctrl-D (0x04) on its own ends it, restoring the terminal."""
import os, sys, termios, tty

mode = sys.argv[1] if len(sys.argv) > 1 else ""
fd = sys.stdin.fileno()
saved = termios.tcgetattr(fd)
tty.setraw(fd)
out = sys.stdout
try:
    if mode == "kitty":
        out.write("\x1b[>1u")
    elif mode == "mok":
        out.write("\x1b[>4;2m")
    out.write(f"KEYECHO-READY {mode}\r\n")
    out.flush()
    while True:
        data = os.read(fd, 1024)
        if not data or data == b"\x04":
            break
        out.write("KEY " + data.hex() + "\r\n")
        out.flush()
finally:
    if mode == "kitty":
        out.write("\x1b[<u")
    elif mode == "mok":
        out.write("\x1b[>4m")
    out.flush()
    termios.tcsetattr(fd, termios.TCSADRAIN, saved)
