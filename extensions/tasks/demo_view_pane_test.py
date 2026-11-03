"""Cleanup must not ask terminal-only process APIs about extension views."""
import os
import unittest
from unittest.mock import Mock, patch


@unittest.skipUnless(os.name == "posix", "the process demo is Unix-only")
class ViewPaneCleanupTest(unittest.TestCase):
    def test_first_party_views_request_auto_sizing_in_each_direction(self):
        from demo_view_pane import view_add_args
        for direction in ("right", "down"):
            args = view_add_args("pane", direction)
            self.assertNotIn("--ratio", args)
            self.assertIn(direction, args)

    def test_stop_remembers_terminals_but_still_stops_with_a_view_present(self):
        from demo import Demo
        from demo_readiness import ReadinessDemo

        demo = ReadinessDemo.__new__(ReadinessDemo)
        demo.server = (17, 42)
        demo.cli = Mock(return_value={"result": {"panes": [
            {"pane_id": "terminal"},
            {"pane_id": "view", "content": "view"},
        ]}})
        demo.remember_pane = Mock()
        with patch("demo_readiness.birth", return_value=42), patch.object(Demo, "stop") as stop:
            demo.stop()
        demo.remember_pane.assert_called_once_with("terminal")
        stop.assert_called_once_with()


if __name__ == "__main__":
    unittest.main()
