"""The independent text client exposes the same owner-unset recovery notice."""
import importlib.util
from pathlib import Path
import unittest
path = Path(__file__).resolve().parents[1] / "extensions/clients/text.py"
spec = importlib.util.spec_from_file_location("owner_notice_text_client", path)
text = importlib.util.module_from_spec(spec)
spec.loader.exec_module(text)


class OwnerNotice(unittest.TestCase):
    def test_notice_is_visible_without_a_layout_and_does_not_guess_owner(self):
        client = text.TextClient.__new__(text.TextClient)
        client.active_layout = lambda: (None, None)
        client.info = {"owner_unset": True, "owner_notice": "Bootstrap explicitly, then reconnect"}
        lines = client.show()
        self.assertIn("OWNER UNSET", lines[0])
        self.assertIn("Bootstrap explicitly", lines[0])
        self.assertIn("no active layout", lines[1])
        client.info = {"owner_unset": False}
        self.assertEqual(client.show(), ["no active layout (try `p` and reset layout)"])
