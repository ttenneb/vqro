"""Spike: drive a tmux-compatible server through control mode (-C).

  python3 cm_spike.py tmux
  python3 cm_spike.py /path/to/rmux
"""
import os, subprocess, sys, threading, time

BIN = sys.argv[1] if len(sys.argv) > 1 else "tmux"
AGENT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "fake-agent.sh")
SOCK = ["-L", "primspike"]


def log(*a):
    print(f"[{time.strftime('%H:%M:%S')}]", *a, flush=True)


def unescape(s):
    # control mode octal-escapes bytes < 32 and backslash
    out, i = bytearray(), 0
    while i < len(s):
        if s[i] == "\\" and i + 3 < len(s) + 1 and s[i + 1:i + 4].isdigit():
            out.append(int(s[i + 1:i + 4], 8)); i += 4
        else:
            out += s[i].encode(); i += 1
    return bytes(out)


def main():
    subprocess.run([BIN, *SOCK, "kill-server"], capture_output=True)
    p = subprocess.Popen([BIN, *SOCK, "-C", "new-session", "-s", "spike", "-x", "120", "-y", "40"],
                         stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True, bufsize=1)
    raw = bytearray()
    notes = []

    def reader():
        for line in p.stdout:
            line = line.rstrip("\n")
            if line.startswith("%output "):
                _, pane, data = line.split(" ", 2)
                raw.extend(unescape(data))
            elif line.startswith("%"):
                notes.append(line)
                if not line.startswith(("%begin", "%end")):
                    log("NOTE", line[:150])

    threading.Thread(target=reader, daemon=True).start()

    def cmd(c):
        p.stdin.write(c + "\n"); p.stdin.flush(); time.sleep(0.4)

    time.sleep(0.8)
    cmd("refresh-client -B status:%*:#{pane_id}=#{pane_title}")  # format subscription
    cmd("send-keys -t spike 'echo hi-from-prim' Enter")
    time.sleep(0.8)
    log("output saw hi:", b"hi-from-prim" in raw)
    cmd("set-option -p -t spike @prim_role worker")
    cmd("display-message -p -t spike '#{@prim_role}'")
    cmd(f"split-window -t spike {AGENT}")
    time.sleep(5.5)
    log("OSC 7501 bytes in %output stream:", b"\x1b]7501" in raw)
    for st in (b"working", b"blocked", b"done"):
        log(f"  state={st.decode()} seen:", b"state=" + st in raw)
    cap = subprocess.run([BIN, *SOCK, "capture-pane", "-p", "-e", "-t", "spike"], capture_output=True)
    log("capture-pane -e contains 7501:", b"7501" in cap.stdout)
    cmd("kill-server")
    p.wait(timeout=5)
    log("notifications:", sorted({n.split(" ")[0] for n in notes}))


if __name__ == "__main__":
    main()
