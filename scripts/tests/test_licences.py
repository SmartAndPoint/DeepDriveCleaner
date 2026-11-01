"""The app's licence text: python3 -I -m unittest discover -s scripts/tests -p 'test_*.py'.

ADR "Licensing: AGPL-3.0 with a commercial licence for the product, MIT for agent
guidance and report schemas". The About panel shows NSHumanReadableCopyright; the
Rust structure test checks the rest, but may not read the app's Rust-free files.
"""
import os
import re
import unittest

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))


class AboutPanel(unittest.TestCase):
    def test_the_about_panel_names_the_agpl(self):
        # Read as text: the plist holds build-setting placeholders that are not XML.
        with open(os.path.join(ROOT, "app", "DeepDriveCleaner", "Info.plist"), encoding="utf-8") as f:
            match = re.search(r"<key>NSHumanReadableCopyright</key>\s*<string>([^<]*)</string>", f.read())
        self.assertIsNotNone(match, "Info.plist has NSHumanReadableCopyright")
        text = match.group(1)
        self.assertIn("GNU Affero General Public License 3.0", text)
        self.assertNotIn("PolyForm", text)


if __name__ == "__main__":
    unittest.main()
