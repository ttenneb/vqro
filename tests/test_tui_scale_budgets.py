"""The TUI scale probe enforces its performance budgets (pure check; the probe itself runs in check.sh full)."""
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
import e2e_tui_scale as probe  # noqa: E402


class Budgets(unittest.TestCase):
    within = {"cpu_pct_one_core": 18, "rss_peak_mib": 38.6, "key_p99_ms": 81.8,
              "background_fps_peak_complete_second": 55, "fps_peak_complete_second": 94}

    def test_measured_results_pass_with_margin(self):
        self.assertEqual(probe.exceeded(self.within), [])
        for name, (limit, worst) in probe.BUDGETS.items():
            self.assertGreaterEqual(limit, worst, name)

    def test_any_measure_over_its_limit_fails(self):
        for name, (limit, _) in probe.BUDGETS.items():
            self.assertEqual(probe.exceeded({**self.within, name: limit + 1}), [name])

    def test_a_missing_measurement_is_not_a_pass(self):
        for name in probe.BUDGETS:
            self.assertIn(name, probe.exceeded({k: v for k, v in self.within.items() if k != name}))


if __name__ == "__main__":
    unittest.main()
