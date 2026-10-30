"""Tests of native-skips.sh: python3 -I -m unittest discover -s scripts/tests -p 'test_*.py'.

Story "Build matrix: every target built, tested and packaged from one tag" (DDC-007),
stage 2. Each case copies the script into a throwaway tree with its own
.config/native-skips/, so the committed lists are never read.
"""
import os
import shutil
import subprocess
import tempfile
import unittest

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
SCRIPT = os.path.join(ROOT, "scripts", "native-skips.sh")

LOG = """\
        PASS [   0.016s] ( 1/3) ddc-core::agents a_session
    stderr ───
    skipped a_session: deepdrivecleaner does not support linux yet
        PASS [   0.020s] ( 2/3) ddc-core::brew \x1b[1ma_formula\x1b[0m
    stderr ───
    skipped a_formula: deepdrivecleaner has no homebrew capability on linux
    skipped a_formula: deepdrivecleaner has no homebrew capability on linux
        PASS [   0.020s] ( 3/3) ddc-core::scanner walks
    stdout ───
    not skipped walks: a line that only mentions the word
────────────
     Summary [   1.000s] 3 tests run: 3 passed, 0 skipped
"""


class NativeSkips(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.root = tmp.name
        os.makedirs(os.path.join(self.root, "scripts"))
        os.makedirs(os.path.join(self.root, ".config", "native-skips"))
        self.script = os.path.join(self.root, "scripts", "native-skips.sh")
        shutil.copy(SCRIPT, self.script)
        self.log = os.path.join(self.root, "tests.log")
        with open(self.log, "w") as f:
            f.write(LOG)
        self.summary = os.path.join(self.root, "summary.md")

    def run_with(self, listed, os_name="linux"):
        with open(os.path.join(self.root, ".config", "native-skips", "linux.txt"), "w") as f:
            f.write(listed)
        env = {"PATH": "/usr/bin:/bin", "GITHUB_STEP_SUMMARY": self.summary}
        return subprocess.run(
            ["bash", self.script, os_name, self.log], env=env, capture_output=True, text=True
        )

    def test_the_listed_skips_pass_and_the_counts_reach_the_summary(self):
        r = self.run_with("# Linux\n\na_session\na_formula\n")
        self.assertEqual(r.returncode, 0, r.stderr)
        with open(self.summary) as f:
            summary = f.read()
        self.assertIn("3 tests run: 3 passed, 0 skipped", summary)
        self.assertIn("2 skip with a printed reason; .config/native-skips/linux.txt lists 2", summary)

    def test_a_new_skip_fails_with_the_difference(self):
        r = self.run_with("a_formula\n")
        self.assertEqual(r.returncode, 1)
        self.assertIn("+a_session", r.stdout)
        self.assertIn("the skips differ", r.stderr)

    def test_a_listed_test_that_ran_fails_with_the_difference(self):
        r = self.run_with("a_formula\na_session\nwalks\n")
        self.assertEqual(r.returncode, 1)
        self.assertIn("-walks", r.stdout)

    def test_an_os_without_a_list_fails(self):
        r = self.run_with("a_formula\na_session\n", os_name="windows")
        self.assertEqual(r.returncode, 1)
        self.assertIn("no list for windows", r.stderr)


if __name__ == "__main__":
    unittest.main()
