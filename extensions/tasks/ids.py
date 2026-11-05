"""Tasks-owned durable ID abbreviations; no authority or record mutation."""
import re

KINDS = {"tasks": "task_", "messages": "msg_"}
MIN_HEX = 6


def resolve(db, table, value):
    prefix = KINDS[table]
    if not isinstance(value, str) or not value or len(value) > 128:
        raise ValueError("invalid ID")
    # Legacy/exact IDs remain usable regardless of abbreviation syntax/length.
    if db.execute(f"SELECT 1 FROM {table} WHERE id=?", (value,)).fetchone():
        return value
    bare = value.removeprefix(prefix)
    if not re.fullmatch(r"[0-9a-fA-F]+", bare):
        raise ValueError(f"unknown {prefix[:-1]} ID: {value}")
    if len(bare) < MIN_HEX:
        raise ValueError(f"ID prefix requires at least {MIN_HEX} hex characters")
    # A hex-only parameter cannot introduce LIKE wildcards. IDs are lowercase.
    candidates = [row[0] for row in db.execute(
        f"SELECT id FROM {table} WHERE id LIKE ? ORDER BY id", (prefix + bare.lower() + "%",))]
    if not candidates:
        raise ValueError(f"unknown {prefix[:-1]} ID: {value}")
    if len(candidates) != 1:
        raise ValueError(f"ambiguous ID prefix {value}: " + ", ".join(candidates))
    return candidates[0]


def short_forms(values):
    """Shortest unique task prefix, at least eight hex digits, else the full ID."""
    values = set(values)
    ordered = sorted(value for value in values if re.fullmatch(r"task_[0-9a-f]+", value))
    result = {}
    for index, value in enumerate(ordered):
        neighbors = ordered[max(0, index - 1):index] + ordered[index + 1:index + 2]
        length = len("task_") + 8
        for neighbor in neighbors:
            shared = 0
            for left, right in zip(value, neighbor):
                if left != right:
                    break
                shared += 1
            length = max(length, shared + 1)
        result[value] = value[:length]
    return result
