"""Self-tests for the Part A O12/O14 checkers: each must reject the false-pass counterexamples."""
import importlib
import unittest

import agent_conformance.cases  # noqa: F401  (load the shared registry first; then the row module)

m = importlib.import_module("agent_conformance.stream_bound_cases")


class NoReattachLoop(unittest.TestCase):
    def test_bounded_backoff_passes(self):
        self.assertTrue(m.no_reattach_loop([0.0, 2.0, 6.0]))
        self.assertTrue(m.no_reattach_loop([0.0]))

    def test_immediate_reattach_loop_fails(self):
        self.assertFalse(m.no_reattach_loop([0.0, 0.05, 0.1, 0.15, 0.2]), "a tight loop")
        self.assertFalse(m.no_reattach_loop([round(0.15 * i, 2) for i in range(150)]), "the observed production loop")
        self.assertFalse(m.no_reattach_loop([0.0, 1.5, 3.0, 4.5]), "too many opens in the window")
        self.assertTrue(m.no_reattach_loop([0.0, 0.15]), "one immediate retry then a stop is not a loop")
        self.assertFalse(m.no_reattach_loop([]), "no open at all is not evidence")


class CursorNeverAdvanced(unittest.TestCase):
    def test_same_or_reset_passes(self):
        self.assertTrue(m.cursor_never_advanced([None, {"epoch": "e", "seq": 1}], 1, "e"))

    def test_skipping_the_bad_record_fails(self):
        self.assertFalse(m.cursor_never_advanced([{"epoch": "e", "seq": 2}], 1, "e"), "advanced past the bad record")
        self.assertFalse(m.cursor_never_advanced([{"epoch": "other", "seq": 1}], 1, "e"), "foreign epoch")
        self.assertFalse(m.cursor_never_advanced([{"epoch": "e"}], 1, "e"), "malformed cursor")


class IncompatibilityReported(unittest.TestCase):
    def test_named_cause_and_limit_passes(self):
        content = [{"type": "text", "text": "This agent's history is larger than this pane can hold (4096 items, 4 MiB), so the conversation stopped updating."}]
        self.assertTrue(m.incompatibility_reported(content, ["larger|over_bound"], ["4096|4 mib"]))

    def test_silent_or_causeless_fails(self):
        self.assertFalse(m.incompatibility_reported([{"type": "text", "text": "Loading the conversation…"}], ["bad_record|malformed"]))
        self.assertFalse(m.incompatibility_reported([{"type": "text", "text": "The conversation stopped updating."}], ["bad_record|malformed"]),
                         "incompatible but no cause")
        self.assertFalse(m.incompatibility_reported([{"type": "text", "text": "stopped updating: too large"}], ["larger|too large"], ["4096"]),
                         "a bound cause without the limit")


class NoPartialCut(unittest.TestCase):
    def test_marker_absent_passes_present_fails(self):
        self.assertTrue(m.no_partial_cut([{"text": "O14-last-good"}], ["O14-PARTIAL-CUT-"]))
        self.assertFalse(m.no_partial_cut([{"text": "O14-PARTIAL-CUT-xxxx"}], ["O14-PARTIAL-CUT-"]))


class AdapterPreservesOrMarks(unittest.TestCase):
    def setUp(self):
        import hashlib
        self.text = "0:" + "x" * 70000
        self.bytes, self.sha = len(self.text.encode()), hashlib.sha256(self.text.encode()).hexdigest()

    def test_whole_or_split_or_marked_passes(self):
        self.assertTrue(m.adapter_preserves_or_marks([self.text], self.bytes, self.sha))
        self.assertTrue(m.adapter_preserves_or_marks([self.text[:40000], self.text[40000:]], self.bytes, self.sha))
        self.assertTrue(m.adapter_preserves_or_marks([self.text[:60000], "[message truncated: too large]"], self.bytes, self.sha))

    def test_silent_truncation_fails(self):
        self.assertFalse(m.adapter_preserves_or_marks([self.text[:60000]], self.bytes, self.sha), "silently cut at the bound")
        self.assertFalse(m.adapter_preserves_or_marks([self.text[:-1] + "y"], self.bytes, self.sha), "same length, altered text")
        self.assertFalse(m.adapter_preserves_or_marks([], self.bytes, self.sha), "nothing at all")


class ViewerPin(unittest.TestCase):
    """Only a kit-pinned viewer is conformance authority: missing or mismatched hashes refuse before any copy."""

    def run_install(self, manifest_extra, tamper=False):
        import asyncio, hashlib, tempfile
        from pathlib import Path
        from types import SimpleNamespace
        from agent_conformance.results import HarnessGap
        with tempfile.TemporaryDirectory() as tmp:
            source, root = Path(tmp) / "viewer", Path(tmp) / "root"
            source.mkdir(); (root / "ext").mkdir(parents=True)
            for name, body in (("agents.py", b"print('agents')\n"), ("vqro_items.py", b"ITEMS = 1\n")):
                (source / name).write_bytes(body)
            pins = {p.name: hashlib.sha256(p.read_bytes()).hexdigest() for p in source.glob("*.py")}
            if tamper: (source / "agents.py").write_bytes(b"print('edited')\n")
            calls = []
            async def call(method, params): calls.append(method); return {}
            ctx = SimpleNamespace(root=root, manifest=dict({"viewer_extensions": str(source)}, **manifest_extra(pins)),
                                  evidence=[], peer=SimpleNamespace(call=call))
            try:
                asyncio.run(m.install_production_viewer(ctx))
                return "installed", calls, sorted(p.name for p in (root / "ext").glob("*.py"))
            except HarnessGap as gap:
                return "gap: " + str(gap), calls, sorted(p.name for p in (root / "ext").glob("*.py"))

    def test_pinned_viewer_installs(self):
        result, calls, copied = self.run_install(lambda pins: {"viewer_revision": "kit-1", "viewer_sha256": pins})
        self.assertEqual((result, calls, copied), ("installed", ["workspace.apply"], ["agents.py", "vqro_items.py"]))

    def test_unpinned_or_edited_viewer_is_refused_before_copy(self):
        for extra, tamper in ((lambda pins: {}, False), (lambda pins: {"viewer_sha256": pins}, False),
                              (lambda pins: {"viewer_revision": "kit-1", "viewer_sha256": pins}, True)):
            result, calls, copied = self.run_install(extra, tamper)
            self.assertTrue(result.startswith("gap"), result)
            self.assertEqual((calls, copied), ([], []), "nothing copied or enabled")


if __name__ == "__main__":
    unittest.main()
