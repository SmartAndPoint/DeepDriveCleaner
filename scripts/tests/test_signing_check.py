"""Tests of check-signatures.sh: python3 -I -m unittest discover -s scripts/tests -p 'test_*.py'.

A fake codesign first in PATH answers for each file, so these run anywhere and sign
nothing. The case with real signatures signs a fixture ad hoc (`codesign -s -`) and runs
only with DDC_RUN_CODESIGN_TESTS=1, which `make app-test` sets: the pass case needs the
Developer ID and runs on a real build, in scripts/build-app.sh.
"""
import os
import shutil
import stat
import subprocess
import tempfile
import unittest

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
CHECK = os.path.join(ROOT, "scripts", "check-signatures.sh")
TEAM = "8MSDJTBW2D"
# Any Mach-O file will do; it is copied, never run.
MACH_O = "/usr/bin/true"

# A codesign that reads the answer for a file from the file named after it plus
# ".codesign": the fixture says how each one is "signed".
FAKE_CODESIGN = """#!/bin/sh
for last; do :; done
answer="$last.codesign"
[ -f "$answer" ] || { echo "$last: code object is not signed at all" >&2; exit 1; }
cat "$answer" >&2
"""

DEVELOPER_ID = f"""Identifier=com.example
CodeDirectory v=20500 size=1 flags=0x10000(runtime) hashes=1+7 location=embedded
TeamIdentifier={TEAM}
"""
AD_HOC = """Identifier=com.example
CodeDirectory v=20400 size=1 flags=0x2(adhoc) hashes=1+7 location=embedded
TeamIdentifier=not set
"""
NO_RUNTIME = f"""Identifier=com.example
CodeDirectory v=20500 size=1 flags=0x0(none) hashes=1+7 location=embedded
TeamIdentifier={TEAM}
"""

APP_BINARY = "Fixture.app/Contents/MacOS/Fixture"
CLI = "Fixture.app/Contents/Helpers/deepdrivecleaner"
SPARKLE = "Fixture.app/Contents/Frameworks/Sparkle.framework/Versions/B/Sparkle"
AUTOUPDATE = "Fixture.app/Contents/Frameworks/Sparkle.framework/Versions/B/Autoupdate"


class Fixture:
    """A bundle of Mach-O copies, plus files the check must leave alone."""

    def __init__(self, test):
        self.dir = tempfile.TemporaryDirectory()
        test.addCleanup(self.dir.cleanup)
        self.root = self.dir.name
        self.app = os.path.join(self.root, "Fixture.app")
        for binary in (APP_BINARY, CLI, SPARKLE, AUTOUPDATE):
            path = self.path(binary)
            os.makedirs(os.path.dirname(path), exist_ok=True)
            shutil.copyfile(MACH_O, path)
            os.chmod(path, stat.S_IRWXU)
        # Not Mach-O: never checked.
        with open(os.path.join(self.app, "Contents", "Info.plist"), "w") as f:
            f.write("<plist/>\n")
        # A link to a Mach-O file: its target is checked where it lives.
        os.symlink("Versions/B/Sparkle", os.path.join(self.app, "Contents/Frameworks/Sparkle.framework/Sparkle"))

    def path(self, relative):
        return os.path.join(self.root, relative)


class SigningCheckWithAFakeCodesign(unittest.TestCase):
    def setUp(self):
        self.fixture = Fixture(self)
        self.bin = os.path.join(self.fixture.root, "bin")
        os.mkdir(self.bin)
        with open(os.path.join(self.bin, "codesign"), "w") as f:
            f.write(FAKE_CODESIGN)
        os.chmod(os.path.join(self.bin, "codesign"), stat.S_IRWXU)

    def sign(self, relative, answer):
        # Not Mach-O, so the check passes over it.
        path = self.fixture.path(relative) + ".codesign"
        with open(path, "w") as f:
            f.write(answer)
        self.addCleanup(os.remove, path)

    def check(self):
        env = {"PATH": f"{self.bin}:/usr/bin:/bin"}
        return subprocess.run([CHECK, self.fixture.app, TEAM], env=env, capture_output=True, text=True)

    def test_passes_when_every_mach_o_file_has_the_team_and_the_runtime(self):
        for binary in (APP_BINARY, CLI, SPARKLE, AUTOUPDATE):
            self.sign(binary, DEVELOPER_ID)
        result = self.check()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("4 Mach-O files", result.stdout)

    def test_fails_on_one_file_signed_ad_hoc_inside_the_framework(self):
        for binary in (APP_BINARY, CLI, SPARKLE):
            self.sign(binary, DEVELOPER_ID)
        self.sign(AUTOUPDATE, AD_HOC)
        result = self.check()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn(f"{self.fixture.path(AUTOUPDATE)}: not signed by team {TEAM}", result.stderr)
        self.assertIn(f"{self.fixture.path(AUTOUPDATE)}: no hardened runtime", result.stderr)
        self.assertNotIn(self.fixture.path(SPARKLE), result.stderr)

    def test_fails_without_the_hardened_runtime(self):
        for binary in (APP_BINARY, SPARKLE, AUTOUPDATE):
            self.sign(binary, DEVELOPER_ID)
        self.sign(CLI, NO_RUNTIME)
        result = self.check()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn(f"{self.fixture.path(CLI)}: no hardened runtime", result.stderr)

    def test_fails_on_an_unsigned_file(self):
        for binary in (APP_BINARY, CLI, AUTOUPDATE):
            self.sign(binary, DEVELOPER_ID)
        result = self.check()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn(f"{self.fixture.path(SPARKLE)}: not signed", result.stderr)

    def test_fails_on_a_bundle_with_no_mach_o_file(self):
        for binary in (APP_BINARY, CLI, SPARKLE, AUTOUPDATE):
            os.remove(self.fixture.path(binary))
        result = self.check()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("no Mach-O file", result.stderr)


@unittest.skipUnless(
    os.environ.get("DDC_RUN_CODESIGN_TESTS") == "1", "signs a fixture ad hoc: set DDC_RUN_CODESIGN_TESTS=1 (make app-test)"
)
class SigningCheckWithRealSignatures(unittest.TestCase):
    def test_fails_on_a_framework_signed_ad_hoc(self):
        fixture = Fixture(self)
        # The framework's two binaries, each as a file: the fixture's bundles are not
        # complete enough for codesign to sign them as bundles.
        for path in (AUTOUPDATE, SPARKLE):
            subprocess.run(
                ["codesign", "--force", "--sign", "-", "--options", "runtime", fixture.path(path)],
                check=True, capture_output=True,
            )
        result = subprocess.run([CHECK, fixture.app, TEAM], capture_output=True, text=True)
        self.assertNotEqual(result.returncode, 0)
        # Signed ad hoc with the runtime: only the team is missing.
        self.assertIn(f"{fixture.path(SPARKLE)}: not signed by team {TEAM}", result.stderr)
        self.assertIn(f"{fixture.path(AUTOUPDATE)}: not signed by team {TEAM}", result.stderr)
        self.assertNotIn(f"{fixture.path(SPARKLE)}: no hardened runtime", result.stderr)


if __name__ == "__main__":
    unittest.main()
