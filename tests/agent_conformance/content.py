"""Valid, deterministic media blocks; no model or filesystem/network resources."""
import base64
import io
import struct
import wave
import zlib


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


def examples():
    return {"text": {"type": "text", "text": "D6-conforming-literal"},
            "image": {"type": "image", "mime": "image/png", "data": base64.b64encode(png()).decode()},
            "audio": {"type": "audio", "mime": "audio/wav", "data": base64.b64encode(wav()).decode()},
            "resource_link": {"type": "resource_link", "uri": "https://example.invalid/vqro-fixture", "name": "fixture"},
            "resource": {"type": "resource", "uri": "https://example.invalid/vqro-inline", "mime": "text/plain", "text": "inline fixture"}}
