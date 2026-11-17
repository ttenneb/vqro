"""Small runtime-neutral text helpers for bounded, control-safe panel content."""


def tail(text, size):
    wire = str(text).encode("utf-8", errors="replace")
    return wire[-size:].decode("utf-8", errors="ignore")


def plain(text):
    return "".join(f"\\x{ord(c):02x}" if ord(c) < 32 and c not in "\n\t" or 127 <= ord(c) < 160 else c
                   for c in str(text))


def label(value, size=512):
    return tail(plain(str(value)), size)
