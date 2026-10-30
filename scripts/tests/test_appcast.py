"""Tests of render-appcast.sh: python3 -I -m unittest discover -s scripts/tests -p 'test_*.py'.

A fake sign_update prints a fixed signature, so no key and no keychain are read, and
nothing touches the network.
"""
import os
import stat
import subprocess
import tempfile
import unittest

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
RENDER = os.path.join(ROOT, "scripts", "render-appcast.sh")

SIGNATURE = "pV3o+Qx/7lYc2Hn0FRpqzA1Gg5sbC0ZtM9yQ1Vw8Rk3l/2Ff9JpTqX0aE6bD4cR+hNuIyKs8WvLm5OoGj1eCw=="
LENGTH = 4096
TEAM = "8MSDJTBW2D"
OTHER_TEAM = "ABCDE12345"
URL = "https://github.com/SmartAndPoint/DeepDriveCleaner/releases/download/v0.8.0/DeepDriveCleaner-0.8.0.zip"
NOTES = "https://github.com/SmartAndPoint/DeepDriveCleaner/releases/tag/v0.8.0"
ITEM = "//*[local-name()='item']"


class RenderAppcast(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.TemporaryDirectory()
        self.addCleanup(self.dir.cleanup)
        self.root = self.dir.name
        self.zip = os.path.join(self.root, "DeepDriveCleaner-0.8.0.zip")
        with open(self.zip, "wb") as f:
            f.write(b"\0" * LENGTH)
        self.sign_update = os.path.join(self.root, "sign_update")
        with open(self.sign_update, "w") as f:
            f.write(f"#!/bin/sh\nprintf '%s\\n' 'sparkle:edSignature=\"{SIGNATURE}\" length=\"{LENGTH}\"'\n")
        os.chmod(self.sign_update, stat.S_IRWXU)
        self.out = os.path.join(self.root, "appcast.xml")

    def render(self, *extra, version="14", team=TEAM, sign_update=None):
        args = [
            RENDER, "--zip", self.zip, "--version", version, "--short-version", "0.8.0",
            "--min-macos", "26.0", "--url", URL, "--notes-url", NOTES, "--team", team, "--out", self.out,
            *extra,
        ]
        env = {"PATH": "/usr/bin:/bin", "SPARKLE_SIGN_UPDATE": sign_update or self.sign_update}
        return subprocess.run(args, env=env, capture_output=True, text=True)

    def previous(self, version="13", team=TEAM):
        """The previous release's appcast, rendered by the script itself."""
        result = self.render(version=version, team=team)
        self.assertEqual(result.returncode, 0, result.stderr)
        path = os.path.join(self.root, f"previous-{version}-{team}.xml")
        os.replace(self.out, path)
        return path

    def xpath(self, expression):
        result = subprocess.run(
            ["xmllint", "--xpath", f"string({expression})", self.out], capture_output=True, text=True
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        # xmllint ends the string with a newline.
        return result.stdout.removesuffix("\n")

    def test_renders_one_signed_item_that_xmllint_accepts(self):
        result = self.render()
        self.assertEqual(result.returncode, 0, result.stderr)
        lint = subprocess.run(["xmllint", "--noout", self.out], capture_output=True, text=True)
        self.assertEqual(lint.returncode, 0, lint.stderr)
        enclosure = f"{ITEM}/*[local-name()='enclosure']"
        self.assertEqual(self.xpath(f"{enclosure}/@*[local-name()='edSignature']"), SIGNATURE)
        self.assertEqual(self.xpath(f"{enclosure}/@length"), str(LENGTH))
        self.assertEqual(self.xpath(f"{enclosure}/@url"), URL)
        self.assertEqual(self.xpath(f"{enclosure}/@type"), "application/octet-stream")
        self.assertEqual(self.xpath(f"{ITEM}/*[local-name()='version']"), "14")
        self.assertEqual(self.xpath(f"{ITEM}/*[local-name()='shortVersionString']"), "0.8.0")
        self.assertEqual(self.xpath(f"{ITEM}/*[local-name()='minimumSystemVersion']"), "26.0.0")
        self.assertEqual(self.xpath(f"{ITEM}/*[local-name()='fullReleaseNotesLink']"), NOTES)
        self.assertEqual(self.xpath(f"{ITEM}/*[local-name()='team']"), TEAM)
        self.assertEqual(self.xpath(f"count({ITEM})"), "1")
        self.assertEqual(self.xpath(f"count({ITEM}/*[local-name()='informationalUpdate'])"), "0")
        # No second request for notes.
        self.assertEqual(self.xpath(f"count({ITEM}/*[local-name()='releaseNotesLink'])"), "0")
        with open(self.out) as f:
            text = f.read()
        self.assertIn('xmlns:sparkle="http://www.andymatuschak.org/xml-namespaces/sparkle"', text)
        self.assertIn("xmlns:ddc=", text)

    def test_http_is_refused_without_local(self):
        local = "http://127.0.0.1:8000/DeepDriveCleaner-0.8.0.zip"
        result = self.render("--url", local)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("must be an https URL", result.stderr)
        self.assertFalse(os.path.exists(self.out))

    def test_local_takes_http_on_127_0_0_1_only(self):
        local = "http://127.0.0.1:8000/DeepDriveCleaner-0.8.0.zip"
        notes = "http://127.0.0.1:8000/"
        result = self.render("--local", "--url", local, "--notes-url", notes)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.xpath(f"{ITEM}/*[local-name()='enclosure']/@url"), local)
        other = self.render("--local", "--url", "http://example.com:8000/x.zip", "--notes-url", notes)
        self.assertNotEqual(other.returncode, 0)
        https = self.render("--local", "--notes-url", notes)
        self.assertNotEqual(https.returncode, 0)

    def test_an_ed_key_file_is_handed_to_sign_update(self):
        key = os.path.join(self.root, "debug.key")
        with open(key, "w") as f:
            f.write("not a key\n")
        seen = os.path.join(self.root, "arguments")
        recording = os.path.join(self.root, "sign_update_recording")
        with open(recording, "w") as f:
            f.write(f"#!/bin/sh\nprintf '%s\\n' \"$@\" > '{seen}'\n"
                    f"printf '%s\\n' 'sparkle:edSignature=\"{SIGNATURE}\" length=\"{LENGTH}\"'\n")
        os.chmod(recording, stat.S_IRWXU)
        result = self.render("--ed-key-file", key, sign_update=recording)
        self.assertEqual(result.returncode, 0, result.stderr)
        with open(seen) as f:
            self.assertEqual(f.read().splitlines(), ["--ed-key-file", key, self.zip])
        missing = self.render("--ed-key-file", os.path.join(self.root, "none.key"), sign_update=recording)
        self.assertNotEqual(missing.returncode, 0)

    def test_a_greater_version_after_the_previous_one_renders(self):
        previous = self.previous(version="13")
        result = self.render("--previous", previous, version="14")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.xpath(f"{ITEM}/*[local-name()='version']"), "14")

    def test_refuses_a_version_not_greater_than_the_previous_one(self):
        previous = self.previous(version="14")
        for version in ("14", "13"):
            with self.subTest(version=version):
                result = self.render("--previous", previous, version=version)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("not greater than the previous release's 14", result.stderr)
                self.assertFalse(os.path.exists(self.out))

    def test_refuses_a_team_change_without_the_flag(self):
        previous = self.previous(version="13", team=OTHER_TEAM)
        result = self.render("--previous", previous)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn(f"the previous release by {OTHER_TEAM}", result.stderr)
        self.assertFalse(os.path.exists(self.out))

    def test_a_team_change_renders_information_only_with_no_enclosure(self):
        previous = self.previous(version="13", team=OTHER_TEAM)
        result = self.render("--previous", previous, "--team-change")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.xpath(f"count({ITEM}/*[local-name()='informationalUpdate'])"), "1")
        self.assertEqual(self.xpath(f"count({ITEM}/*[local-name()='enclosure'])"), "0")
        self.assertEqual(self.xpath(f"{ITEM}/*[local-name()='team']"), TEAM)
        self.assertEqual(self.xpath(f"{ITEM}/*[local-name()='fullReleaseNotesLink']"), NOTES)

    def test_refuses_the_team_change_flag_without_a_team_change(self):
        self.assertNotEqual(self.render("--team-change").returncode, 0)
        previous = self.previous(version="13")
        result = self.render("--previous", previous, "--team-change")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("same team", result.stderr)

    def test_refuses_a_signature_whose_length_is_not_the_zip(self):
        with open(self.zip, "ab") as f:
            f.write(b"\0")
        result = self.render()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("not the zip's size", result.stderr)
        self.assertFalse(os.path.exists(self.out))

    def test_refuses_without_sign_update(self):
        result = self.render(sign_update=os.path.join(self.root, "missing"))
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("sign_update", result.stderr)

    def test_refuses_values_that_could_break_the_xml(self):
        for args in (
            {"version": "14 "},
            {"version": "0.8.0"},
            {"team": "8msdjtbw2d"},
        ):
            with self.subTest(args=args):
                self.assertNotEqual(self.render(**args).returncode, 0)
        result = subprocess.run(
            [RENDER, "--zip", self.zip, "--version", "14", "--short-version", "0.8.0", "--min-macos", "26.0",
             "--url", URL + "?a=1&b=2", "--notes-url", NOTES, "--team", TEAM, "--out", self.out],
            env={"PATH": "/usr/bin:/bin", "SPARKLE_SIGN_UPDATE": self.sign_update}, capture_output=True, text=True,
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(os.path.exists(self.out))


if __name__ == "__main__":
    unittest.main()
