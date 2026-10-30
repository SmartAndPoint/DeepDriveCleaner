"""Tests of debug-feed.sh: python3 -I -m unittest discover -s scripts/tests -p 'test_*.py'.

The zip holds only the app's Info.plist, and a fake sign_update prints a fixed
signature: no key, no keychain, no network.
"""
import os
import plistlib
import stat
import subprocess
import tempfile
import unittest
import zipfile

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
DEBUG_FEED = os.path.join(ROOT, "scripts", "debug-feed.sh")
SIGNATURE = "pV3o+Qx/7lYc2Hn0FRpqzA1Gg5sbC0ZtM9yQ1Vw8Rk3l/2Ff9JpTqX0aE6bD4cR+hNuIyKs8WvLm5OoGj1eCw=="
ITEM = "//*[local-name()='item']"


class DebugFeed(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.TemporaryDirectory()
        self.addCleanup(self.dir.cleanup)
        self.root = self.dir.name
        self.zip = os.path.join(self.root, "DeepDriveCleaner-0.8.0.zip")
        info = {"CFBundleVersion": "14", "CFBundleShortVersionString": "0.8.0", "LSMinimumSystemVersion": "26.0"}
        with zipfile.ZipFile(self.zip, "w") as z:
            z.writestr("DeepDriveCleaner.app/Contents/Info.plist", plistlib.dumps(info))
        self.length = os.path.getsize(self.zip)
        self.seen = os.path.join(self.root, "arguments")
        self.sign_update = os.path.join(self.root, "sign_update")
        with open(self.sign_update, "w") as f:
            f.write(f"#!/bin/sh\nprintf '%s\\n' \"$@\" > '{self.seen}'\n"
                    f"printf '%s\\n' 'sparkle:edSignature=\"{SIGNATURE}\" length=\"{self.length}\"'\n")
        os.chmod(self.sign_update, stat.S_IRWXU)
        self.out = os.path.join(self.root, "feed")

    def run_feed(self, *extra):
        env = {"PATH": "/usr/bin:/bin", "SPARKLE_SIGN_UPDATE": self.sign_update, "DDC_DEBUG_FEED_DIR": self.out}
        return subprocess.run([DEBUG_FEED, self.zip, *extra], env=env, capture_output=True, text=True)

    def xpath(self, expression):
        result = subprocess.run(
            ["xmllint", "--xpath", f"string({expression})", os.path.join(self.out, "appcast.xml")],
            capture_output=True, text=True,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        return result.stdout.removesuffix("\n")

    def test_renders_an_http_feed_on_127_0_0_1_from_the_app_in_the_zip(self):
        result = self.run_feed("--port", "8089")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(os.path.exists(os.path.join(self.out, "DeepDriveCleaner-0.8.0.zip")))
        enclosure = f"{ITEM}/*[local-name()='enclosure']"
        self.assertEqual(self.xpath(f"{enclosure}/@url"), "http://127.0.0.1:8089/DeepDriveCleaner-0.8.0.zip")
        self.assertEqual(self.xpath(f"{enclosure}/@*[local-name()='edSignature']"), SIGNATURE)
        self.assertEqual(self.xpath(f"{ITEM}/*[local-name()='version']"), "14")
        self.assertEqual(self.xpath(f"{ITEM}/*[local-name()='shortVersionString']"), "0.8.0")
        self.assertEqual(self.xpath(f"{ITEM}/*[local-name()='minimumSystemVersion']"), "26.0.0")

    def test_the_port_defaults_to_8000_and_a_debug_key_reaches_sign_update(self):
        key = os.path.join(self.root, "debug.key")
        with open(key, "w") as f:
            f.write("not a key\n")
        result = self.run_feed("--ed-key-file", key)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            self.xpath(f"{ITEM}/*[local-name()='enclosure']/@url"), "http://127.0.0.1:8000/DeepDriveCleaner-0.8.0.zip"
        )
        with open(self.seen) as f:
            self.assertEqual(f.read().splitlines()[:2], ["--ed-key-file", key])

    def test_refuses_a_zip_without_the_app(self):
        with zipfile.ZipFile(self.zip, "w") as z:
            z.writestr("Other.app/Contents/Info.plist", b"")
        result = self.run_feed()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("no DeepDriveCleaner.app/Contents/Info.plist", result.stderr)


if __name__ == "__main__":
    unittest.main()
