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


class Tree:
    """A throwaway tree with the script and its own lists."""

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


class NativeSkips(Tree, unittest.TestCase):
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


MOUNT_LOG = LOG.replace(
    "        PASS [   0.020s] ( 3/3) ddc-core::scanner walks\n",
    "        PASS [   0.020s] ( 3/3) ddc-core::scanner walks\n"
    "        PASS [   0.001s] ( 4/4) ddc-core::mounts a_btrfs_mount_is_btrfs\n"
    "    stderr ───\n"
    "    skipped a_btrfs_mount_is_btrfs: no btrfs mount (DDC_MOUNT_BTRFS unset)\n",
)

GROUPED = "# Linux\na_session\na_formula\n# [environment] needs the CI mounts\na_btrfs_mount_is_btrfs\n"


class Groups(Tree, unittest.TestCase):
    """The environment group (story "Linux CI mounts: a filesystem matrix job with root-only steps")."""

    def setUp(self):
        super().setUp()
        with open(self.log, "w") as f:
            f.write(MOUNT_LOG)

    def run_mode(self, listed, *args, log=None):
        with open(os.path.join(self.root, ".config", "native-skips", "linux.txt"), "w") as f:
            f.write(listed)
        if log is not None:
            with open(self.log, "w") as f:
                f.write(log)
        env = {"PATH": "/usr/bin:/bin"}
        return subprocess.run(["bash", self.script, *args], env=env, capture_output=True, text=True)

    def test_a_mount_skip_in_the_environment_group_passes(self):
        r = self.run_with(GROUPED)
        self.assertEqual(r.returncode, 0, r.stderr)

    def test_a_mount_skip_outside_the_environment_group_fails(self):
        r = self.run_with("a_session\na_formula\na_btrfs_mount_is_btrfs\n")
        self.assertEqual(r.returncode, 1)
        self.assertIn("a mount skip outside the environment group", r.stderr)

    def test_another_group_ends_the_environment_group(self):
        r = self.run_with("# [environment] mounts\n# [capability] lacks\na_session\na_formula\na_btrfs_mount_is_btrfs\n")
        self.assertEqual(r.returncode, 1)
        self.assertIn("a mount skip outside the environment group", r.stderr)

    def test_an_environment_test_skipped_for_another_reason_fails(self):
        r = self.run_with("a_formula\n# [environment] mounts\na_session\na_btrfs_mount_is_btrfs\n")
        self.assertEqual(r.returncode, 1)
        self.assertIn("for another reason than a missing mount", r.stderr)

    def test_the_filter_names_the_environment_group(self):
        r = self.run_mode(GROUPED + "a_second_mount_test\n", "--filter", "linux")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(r.stdout.strip(), "test(=a_btrfs_mount_is_btrfs) | test(=a_second_mount_test)")

    def test_an_empty_environment_group_filters_nothing(self):
        r = self.run_mode("a_session\n", "--filter", "linux")
        self.assertEqual(r.stdout.strip(), "none()")

    def test_the_mounts_run_passes_when_every_environment_test_passed(self):
        log = "        PASS [   0.001s] (1/1) ddc-core::mounts a_btrfs_mount_is_btrfs\n"
        r = self.run_mode(GROUPED, "--mounts", "linux", self.log, log=log)
        self.assertEqual(r.returncode, 0, r.stderr)

    def test_the_mounts_run_fails_on_any_skip(self):
        r = self.run_mode(GROUPED, "--mounts", "linux", self.log)
        self.assertEqual(r.returncode, 1)
        self.assertIn("skipped in the mounts job", r.stderr)

    def test_the_mounts_run_fails_when_an_environment_test_did_not_run(self):
        log = "        PASS [   0.001s] (1/1) ddc-core::mounts another_test\n"
        r = self.run_mode(GROUPED, "--mounts", "linux", self.log, log=log)
        self.assertEqual(r.returncode, 1)
        self.assertIn("did not pass in the mounts job: a_btrfs_mount_is_btrfs", r.stderr)

    def test_a_usage_error_exits_2(self):
        self.assertEqual(self.run_mode(GROUPED, "--mounts", "linux").returncode, 2)


if __name__ == "__main__":
    unittest.main()
