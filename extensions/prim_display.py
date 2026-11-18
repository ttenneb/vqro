"""Small runtime-neutral text helpers for bounded, control-safe panel content."""


def key_labels(text, setting=None):
    """Display key ids/hints only; unknown modes use full names, stored ids never change.

    Callers read doc/settings/tui.key_labels. Parity with the native formatter is
    checked against tests/fixtures/key_labels.json; unknown/future tokens stay literal.
    """
    import re
    named = {"Up", "Down", "Left", "Right", "Home", "End", "PageUp", "PageDown", "Insert", "Delete",
             "Tab", "BackTab", "Enter", "Esc", "Space", "Backspace", "PgUp/PgDn"}
    short = setting == "short"
    def token_label(match):
        token = rest = match.group()
        prefix = ""
        while True:
            found = next(((a, b, c) for a, b, c in (
                ("C-", "C-", "Ctrl-"), ("Ctrl-", "C-", "Ctrl-"),
                ("M-", "M-", "Alt-"), ("Alt-", "M-", "Alt-"),
                ("S-", "S-", "Shift-"), ("Shift-", "S-", "Shift-")) if rest.startswith(a)), None)
            if found is None:
                break
            original, compact, full = found
            prefix += compact if short else full
            rest = rest[len(original):]
        function = rest.startswith("F") and rest[1:].isascii() and rest[1:].isdigit() and 1 <= int(rest[1:]) <= 12
        if not (len(rest) == 1 or rest in named or function or not rest and prefix):
            return token
        if not short:
            rest = {"BackTab": "Shift-Tab", "Esc": "Escape", "PgUp/PgDn": "PageUp/PageDown"}.get(rest, rest)
        return prefix + rest
    return re.sub(r"\S+", token_label, str(text))


def tail(text, size):
    wire = str(text).encode("utf-8", errors="replace")
    return wire[-size:].decode("utf-8", errors="ignore")


def plain(text):
    return "".join(f"\\x{ord(c):02x}" if ord(c) < 32 and c not in "\n\t" or 127 <= ord(c) < 160 else c
                   for c in str(text))


def label(value, size=512):
    return tail(plain(str(value)), size)
