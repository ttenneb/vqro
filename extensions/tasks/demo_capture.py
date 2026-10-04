"""Replay a captured TUI byte stream into a plain-text screen.

The panel demo drives a real Vqro client in a PTY, keeps its raw output, and
needs to read the screen it drew: where a row is, what a badge says, and what
the terminal shows after a collapse. This is that reader, and nothing else - a
reviewer who wants an image can run the screenshot helper kept in
`~/.local/share/herdr/scratch/pilot-owner-ui-capture/replay.py`, which renders
the same replay to a PNG. Demo tooling, not product code.
"""
import sys
from pathlib import Path


class Screen:
    """A character grid."""

    def __init__(self):
        self.rows = []
        self.row = 0
        self.col = 0

    def _ensure(self):
        while len(self.rows) <= self.row:
            self.rows.append([])
        line = self.rows[self.row]
        while len(line) < self.col:
            line.append(" ")

    def put(self, char):
        self._ensure()
        line = self.rows[self.row]
        if self.col < len(line):
            line[self.col] = char
        else:
            line.append(char)
        self.col += 1

    def erase_line(self, mode):
        self._ensure()
        if mode == 0:
            del self.rows[self.row][self.col:]
        elif mode == 1:
            line = self.rows[self.row]
            for index in range(min(self.col + 1, len(line))):
                line[index] = " "
        else:
            self.rows[self.row] = []

    def erase_display(self, mode):
        self._ensure()
        if mode in (2, 3):
            self.rows = [[] for _ in range(self.row + 1)]
        elif mode == 0:
            self.rows[self.row] = self.rows[self.row][: self.col]
            self.rows = self.rows[: self.row + 1]

    def text(self):
        return "\n".join("".join(line).rstrip() for line in self.rows)


def csi_params(raw):
    raw = bytes(byte for byte in raw if byte in b"0123456789;:")
    out = []
    for part in raw.split(b";"):
        part = part.replace(b":", b";").split(b";")[0]
        out.append(int(part) if part.isdigit() else 0)
    return out


def replay(data):
    """Replay a terminal byte stream. Unknown sequences are skipped, never read
    as text, so one unsupported escape cannot smear the screen."""
    screen = Screen()
    index = 0
    total = len(data)
    while index < total:
        if data[index] != 0x1B:
            length = 1
            lead = data[index]
            if lead >= 0xF0:
                length = 4
            elif lead >= 0xE0:
                length = 3
            elif lead >= 0xC0:
                length = 2
            try:
                char = data[index:index + length].decode()
            except UnicodeDecodeError:
                index += 1
                continue
            index += length
            if char == "\n":
                screen.row += 1
            elif char == "\r":
                screen.col = 0
            elif char == "\t":
                screen.col += 8 - (screen.col % 8)
            elif char in "\x07\x0e\x0f":
                continue
            elif char.isprintable():
                screen.put(char)
            continue
        following = data[index + 1:index + 2]
        if following == b"[":
            end = index + 2
            while end < total and not 0x40 <= data[end] <= 0x7E:
                end += 1
            if end >= total:
                break
            final = chr(data[end])
            params = csi_params(data[index + 2:end])
            index = end + 1
            arg = params[0] if params else 0
            if final in "Hf":
                screen.row = max(0, (params[0] if params else 1) - 1)
                screen.col = max(0, (params[1] if len(params) > 1 else 1) - 1)
            elif final == "A":
                screen.row = max(0, screen.row - max(1, arg))
            elif final == "B":
                screen.row += max(1, arg)
            elif final == "C":
                screen.col += max(1, arg)
            elif final == "D":
                screen.col = max(0, screen.col - max(1, arg))
            elif final == "E":
                screen.row += max(1, arg)
                screen.col = 0
            elif final == "F":
                screen.row = max(0, screen.row - max(1, arg))
                screen.col = 0
            elif final == "G":
                screen.col = max(0, arg - 1)
            elif final == "d":
                screen.row = max(0, arg - 1)
            elif final == "J":
                screen.erase_display(arg)
            elif final == "K":
                screen.erase_line(arg)
            continue
        if following == b"]":
            end = index + 2
            while end < total:
                if data[end] == 0x07:
                    end += 1
                    break
                if data[end] == 0x1B and data[end + 1:end + 2] == b"\\":
                    end += 2
                    break
                end += 1
            index = end
            continue
        if following in (b"(", b")", b"#", b"%"):
            index += 3
            continue
        index += 2
    return screen


def capture(log, out_dir, name="screen"):
    """Write the plain-text screen of a captured client log."""
    out_dir = Path(out_dir)
    out_dir.mkdir(parents=True, exist_ok=True)
    screen = replay(Path(log).read_bytes())
    text_path = out_dir / f"{name}.txt"
    text_path.write_text(screen.text())
    return text_path


if __name__ == "__main__":
    print(capture(sys.argv[1], sys.argv[2] if len(sys.argv) > 2 else "."))
