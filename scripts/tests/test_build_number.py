"""Tests of check-build-number.sh: python3 -I -m unittest discover -s scripts/tests -p 'test_*.py'.

Each test builds a throwaway repository with its own tags; nothing is fetched.
"""
import os
import subprocess
import tempfile
import unittest

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
CHECK = os.path.join(ROOT, "scripts", "check-build-number.sh")
PBXPROJ = "app/DeepDriveCleaner.xcodeproj/project.pbxproj"
# No user or system git configuration: no signing, no hooks, no templates.
GIT_ENV = {
    "PATH": "/usr/bin:/bin",
    "GIT_CONFIG_GLOBAL": "/dev/null",
    "GIT_CONFIG_NOSYSTEM": "1",
    "GIT_AUTHOR_NAME": "Test",
    "GIT_AUTHOR_EMAIL": "test@example.com",
    "GIT_COMMITTER_NAME": "Test",
    "GIT_COMMITTER_EMAIL": "test@example.com",
}


def pbxproj(debug, release=None):
    """The two build configurations' lines, as Xcode writes them."""
    release = debug if release is None else release
    return (
        "\t\t\t\tCURRENT_PROJECT_VERSION = %s;\n\t\t\t\tMARKETING_VERSION = 0.7.0;\n"
        "\t\t\t\tCURRENT_PROJECT_VERSION = %s;\n\t\t\t\tMARKETING_VERSION = 0.7.0;\n" % (debug, release)
    )


class BuildNumber(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.TemporaryDirectory()
        self.addCleanup(self.dir.cleanup)
        self.repo = self.dir.name
        self.git("init", "-q", "-b", "main")

    def git(self, *args):
        subprocess.run(["git", *args], cwd=self.repo, env=GIT_ENV, check=True, capture_output=True)

    def release(self, number, tag=None, release=None):
        """Commits the project at build `number`, and tags it when `tag` is given."""
        path = os.path.join(self.repo, PBXPROJ)
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "w") as f:
            f.write(pbxproj(number, release))
        self.git("add", PBXPROJ)
        self.git("commit", "-q", "--allow-empty", "-m", f"build {number}")
        if tag:
            self.git("tag", tag)

    def check(self):
        return subprocess.run([CHECK, "--no-fetch"], cwd=self.repo, env=GIT_ENV, capture_output=True, text=True)

    def test_passes_a_build_number_greater_than_the_last_tag(self):
        self.release(12, tag="v0.7.0")
        self.release(13)
        result = self.check()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("13, after 12 at v0.7.0", result.stdout)

    def test_refuses_a_build_number_equal_to_or_below_the_last_tag(self):
        self.release(12, tag="v0.7.0")
        for number in (12, 11):
            with self.subTest(number=number):
                self.release(number)
                result = self.check()
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(f"CURRENT_PROJECT_VERSION is {number}, not greater than 12 at v0.7.0", result.stderr)

    def test_the_highest_version_tag_counts_not_the_newest_name(self):
        self.release(9, tag="v0.9.0")
        self.release(12, tag="v0.10.0")
        self.release(11)
        result = self.check()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("not greater than 12 at v0.10.0", result.stderr)

    def test_passes_with_no_release_tag(self):
        self.release(1)
        result = self.check()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("no release tag yet", result.stdout)

    def test_refuses_when_debug_and_release_disagree(self):
        self.release(12, tag="v0.7.0")
        self.release(13, release=14)
        result = self.check()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("no single CURRENT_PROJECT_VERSION", result.stderr)


if __name__ == "__main__":
    unittest.main()
