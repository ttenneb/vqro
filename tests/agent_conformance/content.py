"""Valid, deterministic media blocks; no model or filesystem/network resources."""
import base64
import io
import struct
import wave
import zlib

from .results import check


def png():
    def chunk(tag, data):
        return struct.pack(">I", len(data)) + tag + data + struct.pack(">I", zlib.crc32(tag + data))
    # PNG signature, one 8-bit RGBA transparent pixel, no ancillary metadata.
    data = b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", 1, 1, 8, 6, 0, 0, 0))
    return data + chunk(b"IDAT", zlib.compress(b"\x00\x00\x00\x00\x00")) + chunk(b"IEND", b"")


def wav():
    target = io.BytesIO()
    with wave.open(target, "wb") as output:
        output.setnchannels(1)
        output.setsampwidth(2)
        output.setframerate(8000)
        output.writeframes(b"\x00\x00" * 8)
    return target.getvalue()


def compare_content(expected, actual, *, sender_header=False):
    """Compare literal blocks without prescribing where attribution is framed.

    The actual native ledger is never changed. A declared sender header may be
    text-only prefix blocks, or a prefix inside text; original block boundaries/order
    and data must survive. D15 separately checks the header's authority/content.
    """
    check(isinstance(actual, list), "native fixture content oracle is not a block array", component="fixture")
    original = actual
    extra = len(actual) - len(expected)
    if sender_header and extra > 0 and all(block.get("type") == "text" for block in actual[:extra]):
        actual = actual[extra:]
    check([block.get("type") for block in actual] == [block["type"] for block in expected],
          "typed content was reordered, dropped, flattened or concatenated")
    for before, after in zip(expected, actual):
        for field, value in before.items():
            if field == "text" and before["type"] == "text" and sender_header:
                check(isinstance(after.get(field), str) and after[field].endswith(value),
                      "literal text changed inside its declared sender frame")
            else:
                check(after.get(field) == value, f"native {before['type']}.{field} changed")
    return {"extra_sender_blocks": len(original) - len(expected)}


def examples():
    return {"text": {"type": "text", "text": "D6-conforming-literal"},
            "image": {"type": "image", "mime": "image/png", "data": base64.b64encode(png()).decode()},
            "audio": {"type": "audio", "mime": "audio/wav", "data": base64.b64encode(wav()).decode()},
            "resource_link": {"type": "resource_link", "uri": "https://example.invalid/vqro-fixture", "name": "fixture"},
            "resource": {"type": "resource", "uri": "https://example.invalid/vqro-inline", "mime": "text/plain", "text": "inline fixture"}}
