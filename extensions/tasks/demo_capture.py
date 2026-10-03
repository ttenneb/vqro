#!/usr/bin/env python3
"""Replay a captured TUI byte stream into text and a PNG.

The panel demo drives a real Vqro client in a PTY and keeps its raw output.
This turns that output into a screen so a reviewer can see exactly what the
human would see, without a screenshot tool or a new dependency: PIL and a
monospace font already exist on the machine. It is demo tooling, not product
code.
"""
import sys
from pathlib import Path

# xterm's default 16 colours plus the 256-colour cube, to match a dark theme.
BASE16 = [
    (0, 0, 0), (205, 49, 49), (13, 188, 121), (229, 229, 16),
    (36, 114, 200), (188, 63, 188), (17, 168, 205), (229, 229, 229),
    (102, 102, 102), (241, 76, 76), (35, 209, 139), (245, 245, 67),
    (59, 142, 234), (214, 112, 214), (41, 184, 219), (255, 255, 255),
]
DEFAULT_FG = (229, 229, 229)
DEFAULT_BG = (0, 0, 0)
FONT = "JetBrainsMono/JetBrainsMonoNLNerdFont-Regular.ttf"
FONT_BOLD = "JetBrainsMono/JetBrainsMonoNLNerdFont-Bold.ttf"


def color256(index):
    if index < 16:
        return BASE16[index]
    if index < 232:
        index -= 16
        levels = [0, 95, 135, 175, 215, 255]
        return (levels[index // 36], levels[(index // 6) % 6], levels[index % 6])
    level = 8 + (index - 232) * 10
    return (level, level, level)


class Screen:
    """A character grid with per-cell foreground, background and bold."""

    def __init__(self):
        self.rows = []
        self.row = 0
        self.col = 0
        self.fg = DEFAULT_FG
        self.bg = DEFAULT_BG
        self.bold = False

    def _ensure(self):
        while len(self.rows) <= self.row:
            self.rows.append([])
        line = self.rows[self.row]
        while len(line) < self.col:
            line.append((" ", DEFAULT_FG, DEFAULT_BG, False))

    def put(self, char):
        self._ensure()
        cell = (char, self.fg, self.bg, self.bold)
        line = self.rows[self.row]
        if self.col < len(line):
            line[self.col] = cell
        else:
            line.append(cell)
        self.col += 1

    def erase_line(self, mode):
        self._ensure()
        if mode == 0:
            del self.rows[self.row][self.col:]
        elif mode == 1:
            line = self.rows[self.row]
            for index in range(min(self.col + 1, len(line))):
                line[index] = (" ", DEFAULT_FG, DEFAULT_BG, False)
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
        return "\n".join("".join(cell[0] for cell in line).rstrip() for line in self.rows)


def apply_sgr(screen, params):
    if not params:
        params = [0]
    index = 0
    while index < len(params):
        param = params[index]
        if param == 0:
            screen.fg, screen.bg, screen.bold = DEFAULT_FG, DEFAULT_BG, False
        elif param == 1:
            screen.bold = True
        elif param == 22:
            screen.bold = False
        elif param == 39:
            screen.fg = DEFAULT_FG
        elif param == 49:
            screen.bg = DEFAULT_BG
        elif 30 <= param <= 37:
            screen.fg = BASE16[param - 30]
        elif 90 <= param <= 97:
            screen.fg = BASE16[8 + param - 90]
        elif 40 <= param <= 47:
            screen.bg = BASE16[param - 40]
        elif 100 <= param <= 107:
            screen.bg = BASE16[8 + param - 100]
        elif param in (38, 48) and index + 1 < len(params):
            target = "fg" if param == 38 else "bg"
            if params[index + 1] == 5 and index + 2 < len(params):
                setattr(screen, target, color256(params[index + 2]))
                index += 2
            elif params[index + 1] == 2 and index + 4 < len(params):
                setattr(screen, target, tuple(params[index + 2:index + 5]))
                index += 4
        index += 1


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
            elif final == "m":
                apply_sgr(screen, params)
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


def render_png(screen, path, cell=(9, 20)):
    from PIL import Image, ImageDraw, ImageFont

    font_root = Path.home() / ".local/share/fonts"
    regular = ImageFont.truetype(str(font_root / FONT), cell[1] - 3)
    bold = ImageFont.truetype(str(font_root / FONT_BOLD), cell[1] - 3)
    width, height = cell
    rows = len(screen.rows)
    cols = max((len(line) for line in screen.rows), default=1)
    image = Image.new("RGB", (width * cols + 8, height * rows + 8), DEFAULT_BG)
    draw = ImageDraw.Draw(image)
    for y, line in enumerate(screen.rows):
        for x in range(cols):
            if x < len(line) and line[x][2] != DEFAULT_BG:
                draw.rectangle([8 + width * x, 8 + height * y, 8 + width * (x + 1), 8 + height * (y + 1)], fill=line[x][2])
    for y, line in enumerate(screen.rows):
        for x in range(cols):
            if x >= len(line) or line[x][0] == " ":
                continue
            char, fg, _bg, is_bold = line[x]
            draw.text((8 + width * x, 8 + height * y + 1), char, font=bold if is_bold else regular, fill=fg)
    image.save(path)
    return path


def capture(log, out_dir, name="screen"):
    """Write the text and PNG views of a captured client log."""
    out_dir = Path(out_dir)
    out_dir.mkdir(parents=True, exist_ok=True)
    screen = replay(Path(log).read_bytes())
    text_path = out_dir / f"{name}.txt"
    text_path.write_text(screen.text())
    return text_path, render_png(screen, out_dir / f"{name}.png")


if __name__ == "__main__":
    print(*capture(sys.argv[1], sys.argv[2] if len(sys.argv) > 2 else "."))
