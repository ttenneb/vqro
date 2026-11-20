"""Generic causal links: same bounds in Python SDK; no native evidence is inferred."""
import json
from pathlib import Path
import sys
import unittest
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "extensions"))
from vqro_provider import Provider, ProviderError, causal_links, _FORMAT_RANGES


def link():
    return {"kind": "future", "id": "same-local", "provenance": "future", "extra": {"keep": True}}


class CausalShapes(unittest.TestCase):
    def test_qualified_open_fields_and_utf8_bounds(self):
        for cause in [link(), {"kind": "request", "id": "r", "endpoint": "old", "epoch": "prior", "provenance": "native"}, {"kind": "effect", "id": "e", "boot": "prior", "provenance": "adapter"}]:
            self.assertEqual(causal_links({"cause": cause, "correlation": "é" * 128}), (cause, "é" * 128))
        self.assertEqual(causal_links({}), (None, None))
        for params in [{"cause": None}, {"cause": False}, {"cause": {"kind": "effect", "id": "local", "provenance": "asserted"}}, {"cause": {"kind": "entity", "id": "local", "endpoint": "r", "provenance": "asserted"}}, {"cause": dict(link(), id="é" * 129)}, {"correlation": ""}, {"correlation": None}, {"correlation": "é" * 129}]:
            with self.subTest(params=params), self.assertRaises(ProviderError) as caught:
                causal_links(params)
            self.assertEqual(caught.exception.kind, "invalid")

    def test_known_strings_and_correlation_reject_controls(self):
        for field in ("kind", "id", "provenance", "endpoint", "epoch", "boot"):
            with self.assertRaises(ProviderError) as caught:
                causal_links({"cause": dict(link(), **{field: "line\ncontrol"})})
            self.assertEqual(caught.exception.kind, "invalid")
        for value in ("line\ncontrol", "tab\tcontrol", "nul\0control"):
            with self.assertRaises(ProviderError) as caught:
                causal_links({"correlation": value})
            self.assertEqual(caught.exception.kind, "invalid")

    def test_unicode_format_vectors_rejected_in_known_links_only(self):
        vectors = json.loads((Path(__file__).parent / "fixtures/text_controls.json").read_text())
        self.assertEqual([list(pair) for pair in _FORMAT_RANGES], vectors["format_ranges"])
        count = 0
        for first, last in vectors["format_ranges"]:
            for point in range(first, last + 1):
                text = chr(point)
                count += 1
                with self.assertRaises(ProviderError):
                    causal_links({"correlation": text})
                for field in ("kind", "id", "provenance", "endpoint", "epoch", "boot"):
                    with self.assertRaises(ProviderError):
                        causal_links({"cause": dict(link(), **{field: text})})
                self.assertEqual(causal_links({"cause": dict(link(), opaque=text)})[0]["opaque"], text)
        self.assertEqual(count, 170)

    def test_exact_bytes_and_container_depth(self):
        cause = dict(link(), padding="")
        base = len(json.dumps(cause, ensure_ascii=False, separators=(",", ":")).encode())
        cause["padding"] = "x" * (4096 - base)
        self.assertEqual(causal_links({"cause": cause})[0], cause)
        cause["padding"] += "x"
        with self.assertRaises(ProviderError) as caught:
            causal_links({"cause": cause})
        self.assertEqual(caught.exception.kind, "invalid")
        nested = None
        for _ in range(63):
            nested = [nested]
        cause = dict(link(), nested=nested)
        self.assertEqual(causal_links({"cause": cause})[0], cause)
        cause["nested"] = [nested]
        with self.assertRaises(ProviderError) as caught:
            causal_links({"cause": cause})
        self.assertEqual(caught.exception.kind, "invalid")


class CausalSdk(unittest.IsolatedAsyncioTestCase):
    async def test_all_format_labels_refused_before_id_or_handler_admission(self):
        provider = Provider("formats", "fixed", {"future.op": {}})
        ledger = []
        async def effect(ctx, args):
            ledger.append(ctx.label)
            return {"outcome": "observed"}
        provider.effects["future.op"] = effect
        params = {"effect": "same-id", "op": "future.op", "args": {}, "deadline_ms": 30000}
        for first, last in _FORMAT_RANGES:
            for point in range(first, last + 1):
                with self.assertRaises(ProviderError):
                    await provider._effect(dict(params, label=chr(point)))
        self.assertEqual(ledger, [])
        await provider._effect(dict(params, label="safe"))
        self.assertEqual(ledger, ["safe"], "refusals never consume the effect id")

    async def test_native_handler_gets_exact_links_and_invalid_metadata_never_dispatches(self):
        provider = Provider("causal", "fixed", {"future.op": {}})
        ledger = []
        async def effect(ctx, args):
            ledger.append((ctx.cause, ctx.correlation, args))
            return {"outcome": "observed", "data": {"cause": ctx.cause, "correlation": ctx.correlation}}
        provider.effects["future.op"] = effect
        params = {"effect": "e1", "op": "future.op", "args": {"opaque": True}, "deadline_ms": 30000}
        for invalid in [{"label": chr(8203)}, {"label": chr(8238)}, {"cause": None}, {"cause": {"kind": "effect", "id": "unqualified", "provenance": "asserted"}}, {"correlation": False}]:
            with self.assertRaises(ProviderError):
                await provider._effect(dict(params, **invalid))
        self.assertEqual(ledger, [])
        cause = link()
        result = await provider._effect(dict(params, cause=cause, correlation="caller-thread"))
        self.assertEqual(result["data"], {"cause": cause, "correlation": "caller-thread"})
        self.assertEqual(ledger, [(cause, "caller-thread", {"opaque": True})])
        await provider._effect(dict(params, effect="e2"))
        self.assertEqual(ledger[-1][:2], (None, None))


if __name__ == "__main__":
    unittest.main()
