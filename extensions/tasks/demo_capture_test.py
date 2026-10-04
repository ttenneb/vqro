"""Screen-capture replay tests: real escape traffic, no terminal needed."""
import unittest

import demo_capture


class CaptureTests(unittest.TestCase):
    def test_replay_moves_and_writes_cells(self):
        screen = demo_capture.replay(b"\x1b[2J\x1b[1;1Hplain \x1b[1mBOLD\x1b[0m\x1b[3;5Hright")
        self.assertEqual("plain BOLD", "".join(screen.rows[0]))
        self.assertEqual("right", "".join(screen.rows[2]).strip())

    def test_unknown_sequences_are_skipped_not_printed(self):
        # OSC hyperlink, charset selection, a private mode, styling, a stray one.
        screen = demo_capture.replay(
            b"\x1b]8;;https://example.test\x1b\\link\x1b]8;;\x1b\\\x1b(B"
            b"\x1b[?25l\x1b[>7u\x1b[38;2;255;0;0mred\x1b[0m\x1b[5;1Hdone")
        text = screen.text()
        self.assertIn("link", text)
        self.assertIn("red", text)
        self.assertNotIn("example.test", text)
        self.assertNotIn("?", text)
        self.assertEqual("done", text.split("\n")[4])

    def test_erase_ops_keep_the_grid_readable(self):
        screen = demo_capture.replay(b"first\r\nsecond\x1b[1A\x1b[K\x1b[2K\x1b[10;1Hlast")
        lines = screen.text().split("\n")
        self.assertEqual("last", lines[9].strip())
        self.assertEqual("", lines[0].strip())
        self.assertEqual("second", lines[1].strip())


if __name__ == "__main__":
    unittest.main()
