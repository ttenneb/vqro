"""Explicit durable archive export, never an automatic response to quota pressure.

A preexisting locator is read/verified, never overwritten. A failed or uncertain
write retains both source evidence and any partial destination for inspection.
"""
import hashlib
import json
import os
from pathlib import Path

MAX_ARCHIVE_BYTES = 9 * 1024 * 1024


def canonical(value):
    return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode()


def bundle(archive, records):
    body_hash = hashlib.sha256(canonical(records)).hexdigest()
    manifest = {"archive": archive, "content_hash": body_hash,
                "records": [{"delivery": row["delivery"], "version": row["version"]} for row in records]}
    return {"manifest": manifest, "records": records}


def verify(locator, expected):
    with open(locator, "rb") as source:
        data = source.read(MAX_ARCHIVE_BYTES + 1)
    if len(data) > MAX_ARCHIVE_BYTES:
        raise ValueError("archive exceeds bounded read")
    if data != canonical(expected):
        raise ValueError("archive bytes differ; retain source evidence and inspect destination")
    return hashlib.sha256(data).hexdigest()


def export(destination, archive, records):
    directory = Path(destination)
    if not directory.is_absolute() or not directory.is_dir():
        raise ValueError("archive destination must be an explicit existing absolute directory")
    if not archive or any(character not in "0123456789abcdef" for character in archive):
        raise ValueError("invalid archive identifier")
    document = bundle(archive, records)
    data = canonical(document)
    if len(data) > MAX_ARCHIVE_BYTES:
        raise ValueError("archive exceeds declared export bound")
    path = directory / (archive + ".json")
    try:
        fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    except FileExistsError:
        # Confirm an uncertain prior export's exact bytes and durability; never
        # overwrite it, and never mistake a readback alone for durable creation.
        verify(path, document)
        fd = os.open(path, os.O_RDONLY)
        try:
            os.fsync(fd)
        finally:
            os.close(fd)
    else:
        with os.fdopen(fd, "wb") as output:
            output.write(data)
            output.flush()
            os.fsync(output.fileno())
    # No claim of durable export until the directory entry is confirmed too.
    fd = os.open(directory, os.O_RDONLY | getattr(os, "O_DIRECTORY", 0))
    try:
        os.fsync(fd)
    finally:
        os.close(fd)
    return {"locator": str(path), "manifest_hash": verify(path, document), "archive": archive}
