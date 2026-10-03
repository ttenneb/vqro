"""Screen-capture replay tests: real escape traffic, no terminal needed."""
import unittest

import demo_capture


class CaptureTests(unittest.TestCase):
    def test_replay_moves_writes_and_styles_cells(self):
        screen = demo_capture.replay(
            b"\x1b[2J\x1b[1;1Hplain \x1b[1mBOLD\x1b[0m\x1b[3;5Hright \x1b[38;2;255;0;0mred")
        self.assertEqual("plain BOLD", screen.rows[0][0][0] + "".join(cell[0] for cell in screen.rows[0][1:]))
        self.assertEqual((229, 229, 229), screen.rows[0][0][1])
        self.assertTrue(screen.rows[0][6][3])
        self.assertEqual("right red", screen.text().split("\n")[2].strip())
        self.assertEqual("red", "".join(cell[0] for cell in screen.rows[2][10:13]))
        self.assertEqual((255, 0, 0), screen.rows[2][10][1])

    def test_unknown_sequences_are_skipped_not_printed(self):
        # OSC hyperlink, charset selection, a private mode and a stray sequence.
        screen = demo_capture.replay(
            b"\x1b]8;;https://example.test\x1b\\link\x1b]8;;\x1b\\\x1b(B\x1b[?25l\x1b[>7u\x1b[5;1Hdone")
        text = screen.text()
        self.assertIn("link", text)
        self.assertNotIn("example.test", text)
        self.assertNotIn("?", text)
        self.assertEqual("done", text.split("\n")[4])

    def test_erase_and_scroll_ops_keep_the_grid_bounded(self):
        screen = demo_capture.replay(b"first\r\nsecond\x1b[1A\x1b[K\x1b[2K\x1b[10;1Hlast")
        lines = screen.text().split("\n")
        self.assertEqual("last", lines[9].strip())
        self.assertEqual("", lines[0].strip())
        self.assertEqual("second", lines[1].strip())


if __name__ == "__main__":
    unittest.main()
