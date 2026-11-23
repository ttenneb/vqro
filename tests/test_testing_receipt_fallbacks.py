"""Real error/size producer paths retain the fixed RPC caveat; no cores or children run."""
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import Mock, patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "extensions"))
import testing_integration as integration
import testing_perf as perf
import testing_setup as setup
import vqro_testing as shared


class ReceiptFallbackTests(unittest.TestCase):
    def snapshot(self):
        return {"epoch": "private", "entries": {"extension/pi": {"version": 1, "value": {
            "argv": ["python3", "pi.py"], "enabled": True}}}}

    def assert_bounded_error(self, result):
        self.assertEqual(result["verdict"], "error")
        self.assertIn(shared.RPC_RULE_NOTICE, result["not_covered"])
        self.assertLessEqual(len(json.dumps(result, ensure_ascii=False, indent=2).encode()), shared.RECEIPT_BYTES)
        self.assertEqual(result["counts"]["fail"], 1)

    def test_shared_unit_size_fallback_retains_bounded_notice(self):
        result = shared.receipt([shared.row("status", "pi", "pass", "running")], source="live", started_at=shared.utc_now(),
                                started=shared.time.monotonic(), fingerprint_value={"padding": "x" * shared.RECEIPT_BYTES},
                                inputs=[], not_covered=[shared.RPC_RULE_NOTICE], extension_count=1)
        self.assert_bounded_error(result)
        self.assertIn("receipt exceeds size bound", result["rows"][0]["evidence"])

    def test_real_integration_and_performance_size_fallbacks_retain_notice(self):
        for module in (integration, perf):
            with self.subTest(suite=module.__name__), tempfile.TemporaryDirectory(prefix="receipt-size-") as temporary:
                root = Path(temporary)
                state, extensions = root / "state", root / "extensions"
                state.mkdir(); extensions.mkdir()
                (state / "workspace.json").write_text(json.dumps(self.snapshot()))
                args = module.parser().parse_args(["--offline", "--state-dir", str(state), "--extensions-dir", str(extensions), "--root", str(root / "run")])
                store = Mock()
                store.context.return_value = None
                with patch.object(module, "fingerprint", return_value={"padding": "x" * shared.RECEIPT_BYTES}), \
                     patch.object(setup, "SetupStore", return_value=store), \
                     patch.object(module, "Sandbox", side_effect=RuntimeError("private constructor fault")), \
                     patch.object(module.subprocess, "Popen", side_effect=AssertionError("must never launch a child")):
                    result = module.run(args)
                self.assert_bounded_error(result)
                self.assertIn("receipt exceeds size bound", result["rows"][0]["evidence"])
                self.assertTrue(any(shared.RPC_RULE_NOTICE in row.get("text", "") for row in module.panel(result)["items"]))

    def test_real_parent_replacement_receipts_retain_the_known_source_notice(self):
        for module, command in ((integration, "test.integration"), (perf, "test.perf")):
            for payload in (None, "not json", "[]", '{"kind":"vqro.test-receipt"}'):
                with self.subTest(suite=module.__name__, payload=payload), tempfile.TemporaryDirectory(prefix="receipt-parent-") as temporary:
                    root = Path(temporary)
                    state, extensions = root / "state", root / "extensions"
                    state.mkdir(); extensions.mkdir()
                    handlers = {}
                    ext = Mock()
                    ext.info = {"socket": "/private/unused.sock", "state_dir": str(state)}
                    ext.command.side_effect = lambda name, title: lambda fn: handlers.setdefault(name, fn)
                    child = Mock(pid=123456)
                    child.wait.return_value = 1
                    store = Mock()
                    store.context.return_value = {}
                    with patch("vqro_ext.Extension", return_value=ext), patch.object(module, "setup_scan", return_value=(self.snapshot(), {})), \
                         patch.object(module, "SetupStore", return_value=store), patch.object(module.subprocess, "Popen", return_value=child), \
                         patch.object(module.threading, "Thread") as thread, patch.object(module, "write_receipt") as written, \
                         patch.dict(os.environ, {"VQRO_EXTENSIONS_DIR": str(extensions)}):
                        module.extension_main()
                        started = handlers[command]({})
                        if payload is not None:
                            Path(started["receipt"]).write_text(payload)
                        thread.call_args.kwargs["target"]()
                        result = written.call_args.args[0]
                        self.assert_bounded_error(result)
                        panel = ext.publish.call_args.args[1]
                        self.assertTrue(any(shared.RPC_RULE_NOTICE in row.get("text", "") for row in panel["items"]))


if __name__ == "__main__":
    unittest.main()
