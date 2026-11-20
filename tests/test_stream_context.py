"""Stream context is diagnostic caller metadata, validated before native/open-id admission."""
import sys
from pathlib import Path
import unittest
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "extensions"))
from vqro_provider import Provider, ProviderError


def opening():
    return {"stream": 7, "ref": "terminal", "incarnation": "one", "cols": 80, "rows": 24,
            "history_lines": 0, "window_bytes": 1024}


class StreamContext(unittest.IsolatedAsyncioTestCase):
    async def test_context_is_exact_and_bad_metadata_never_dispatches_or_consumes_id(self):
        provider = Provider("context", "fixed", {"terminal.output": {}})
        ledger = []
        async def opened(stream):
            ledger.append(tuple(getattr(stream, key, None) for key in
                                ("author", "actor", "principal", "initiator", "label", "cause", "correlation")))
            return "none"
        provider.open_stream = opened
        try:
            for invalid in ({"actor": 7}, {"author": False}, {"label": chr(8203)},
                            {"cause": False}, {"correlation": ""}):
                with self.assertRaises(ProviderError):
                    await provider._open(dict(opening(), **invalid))
            self.assertEqual(ledger, [])
            cause = {"kind": "request", "id": "local", "endpoint": "previous", "epoch": "old", "provenance": "asserted"}
            stream, result = await provider._open(dict(opening(), author="actual-connection", actor="extension:writer",
                    principal="bob", initiator="alice", label="diagnostic", cause=cause, correlation="thread"))
            self.assertEqual(result, {"initial": "none"})
            self.assertEqual(ledger, [("actual-connection", "extension:writer", "bob", "alice", "diagnostic", cause, "thread")])
            stream._finish()
        finally:
            for stream in list(provider.streams.values()):
                stream._finish()

    async def test_absent_context_stays_absent_no_principal_inference(self):
        provider = Provider("legacy", "fixed", {"terminal.output": {}})
        async def opened(stream):
            self.assertIsNone(stream.actor)
            self.assertIsNone(stream.initiator)
            self.assertIsNone(stream.cause)
            self.assertEqual(stream.principal, "old-principal")
            return "none"
        provider.open_stream = opened
        stream, _ = await provider._open(dict(opening(), principal="old-principal"))
        stream._finish()
