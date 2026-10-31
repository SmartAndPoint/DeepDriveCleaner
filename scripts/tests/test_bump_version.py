"""Tests of bump-version.sh: python3 -I -m unittest discover -s scripts/tests -p 'test_*.py'.

Each test builds a throwaway repository with the files a version lives in.
"""
import os
import subprocess
import tempfile
import unittest

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
BUMP = os.path.join(ROOT, "scripts", "bump-version.sh")
PBXPROJ = "app/DeepDriveCleaner.xcodeproj/project.pbxproj"
GIT_ENV = {
    "PATH": "/usr/bin:/bin",
    "GIT_CONFIG_GLOBAL": "/dev/null",
    "GIT_CONFIG_NOSYSTEM": "1",
    "GIT_AUTHOR_NAME": "Test",
    "GIT_AUTHOR_EMAIL": "test@example.com",
    "GIT_COMMITTER_NAME": "Test",
    "GIT_COMMITTER_EMAIL": "test@example.com",
}
CARGO = '[workspace.package]\nversion = "0.13.0"\nedition = "2024"\n\n[workspace.dependencies]\nclap = { version = "4.6" }\n'
LOCK = (
    '[[package]]\nname = "clap"\nversion = "0.13.0"\n\n'
    '[[package]]\nname = "ddc-cli"\nversion = "0.13.0"\n\n'
    '[[package]]\nname = "ddc-core"\nversion = "0.13.0"\n'
)
FUZZ_LOCK = '[[package]]\nname = "ddc-core"\nversion = "0.13.0"\n'
PBX = (
    "\t\t\t\tCURRENT_PROJECT_VERSION = 20;\n\t\t\t\tMARKETING_VERSION = 0.13.0;\n"
    "\t\t\t\tCURRENT_PROJECT_VERSION = 20;\n\t\t\t\tMARKETING_VERSION = 0.13.0;\n"
)


class BumpVersion(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.repo = self.tmp.name
        for path, text in [("Cargo.toml", CARGO), ("Cargo.lock", LOCK), ("fuzz/Cargo.lock", FUZZ_LOCK), (PBXPROJ, PBX)]:
            full = os.path.join(self.repo, path)
            os.makedirs(os.path.dirname(full), exist_ok=True)
            with open(full, "w") as f:
                f.write(text)
        self.git("init", "-q")
        self.git("add", ".")
        self.git("commit", "-q", "-m", "0.13.0")
        self.git("tag", "v0.13.0")
        self.git("commit", "-q", "--allow-empty", "-m", "feat: after the tag")

    def tearDown(self):
        self.tmp.cleanup()

    def git(self, *args):
        subprocess.run(["git", *args], cwd=self.repo, env=GIT_ENV, check=True)

    def bump(self, arg):
        return subprocess.run([BUMP, arg], cwd=self.repo, env=GIT_ENV, capture_output=True, text=True)

    def read(self, path):
        with open(os.path.join(self.repo, path)) as f:
            return f.read()

    def test_minor_writes_every_place_and_raises_the_build(self):
        result = self.bump("minor")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('version = "0.14.0"', self.read("Cargo.toml"))
        self.assertIn('clap = { version = "4.6" }', self.read("Cargo.toml"))
        lock = self.read("Cargo.lock")
        self.assertIn('name = "ddc-cli"\nversion = "0.14.0"', lock)
        self.assertIn('name = "ddc-core"\nversion = "0.14.0"', lock)
        self.assertIn('name = "clap"\nversion = "0.13.0"', lock)
        self.assertIn('name = "ddc-core"\nversion = "0.14.0"', self.read("fuzz/Cargo.lock"))
        pbx = self.read(PBXPROJ)
        self.assertEqual(pbx.count("MARKETING_VERSION = 0.14.0;"), 2)
        self.assertEqual(pbx.count("CURRENT_PROJECT_VERSION = 21;"), 2)
        self.assertIn("feat: after the tag", result.stdout)

    def test_patch_major_and_an_explicit_version(self):
        self.assertEqual(self.bump("patch").returncode, 0)
        self.assertIn('version = "0.13.1"', self.read("Cargo.toml"))
        self.assertEqual(self.bump("major").returncode, 0)
        self.assertIn('version = "1.0.0"', self.read("Cargo.toml"))
        self.assertEqual(self.bump("1.2.3").returncode, 0)
        self.assertIn('version = "1.2.3"', self.read("Cargo.toml"))
        self.assertEqual(self.read(PBXPROJ).count("CURRENT_PROJECT_VERSION = 23;"), 2)

    def test_a_version_not_above_or_already_tagged_is_refused(self):
        for arg in ("0.13.0", "0.12.9", "next"):
            result = self.bump(arg)
            self.assertNotEqual(result.returncode, 0, arg)
        self.git("tag", "v0.13.1")
        result = self.bump("patch")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("tag v0.13.1 exists", result.stderr)
        self.assertIn('version = "0.13.0"', self.read("Cargo.toml"))


if __name__ == "__main__":
    unittest.main()
