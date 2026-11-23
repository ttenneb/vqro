"""Exercise build.rs compiler selection without building the terminal library."""
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]


@unittest.skipUnless(shutil.which("rustc"), "rustc required")
class ZigSelection(unittest.TestCase):
    def test_wrong_path_version_and_local_fallback(self):
        with tempfile.TemporaryDirectory(prefix="vqro-zig-") as directory:
            root = Path(directory)
            binary = root / "selection"
            source = root / "selection.rs"
            source.write_text((ROOT / "build.rs").read_text() + '''
#[test]
fn selection() {
    let vendor = std::path::Path::new(env!("VQRO_TEST_VENDOR"));
    let selected = find_zig(vendor);
    assert!(selected.ends_with(".local/zig-0.17.1/zig"));
}
''')
            env = dict(os.environ, VQRO_TEST_VENDOR=str(ROOT / "vendor/libghostty-vt"))
            subprocess.run(["rustc", "--edition=2021", "--test", str(source), "-o", str(binary)],
                           env=env, check=True, capture_output=True)
            fake = root / "bin"
            fake.mkdir()
            zig = fake / "zig"
            zig.write_text("#!/bin/sh\necho 0.15.2\n")
            zig.chmod(0o755)
            clean = dict(HOME=str(root), PATH=str(fake))
            wrong = subprocess.run([str(binary), "--nocapture"], env=clean, capture_output=True, text=True)
            self.assertNotEqual(wrong.returncode, 0)
            self.assertIn("needs Zig 0.17.1 (found Zig 0.15.2 at PATH zig)", wrong.stderr)
            self.assertNotIn("translate-c", wrong.stderr)
            local = root / ".local/zig-0.17.1/zig"
            local.parent.mkdir(parents=True)
            local.write_text("#!/bin/sh\necho 0.17.1\n")
            local.chmod(0o755)
            good = subprocess.run([str(binary)], env=clean, capture_output=True, text=True)
            self.assertEqual(good.returncode, 0, good.stdout + good.stderr)
