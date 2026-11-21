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


if __name__ == "__main__":
    unittest.main()
