"""Tests of the cask template: python3 -I -m unittest discover -s scripts/tests -p 'test_*.py'.

The template is rendered the way publish-release.sh renders it (sed over the two
placeholders) and checked with `ruby -c`. `brew style` runs only when
DDC_TEST_BREW_STYLE=1, since it needs Homebrew; publish-release.sh runs it on every
publish.
"""
import os
import re
import shutil
import subprocess
import tempfile
import unittest

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
TEMPLATE = os.path.join(ROOT, "packaging", "homebrew", "deepdrivecleaner.rb.tmpl")
VERSION = "0.8.0"
SHA = "0" * 64
BUNDLE_ID = "com.smartandpoint.deepdrivecleaner"


class CaskTemplate(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.TemporaryDirectory()
        self.addCleanup(self.dir.cleanup)
        # Under Casks/, as publish-release.sh writes it: `brew style` applies the cask cops there.
        casks = os.path.join(self.dir.name, "Casks")
        os.mkdir(casks)
        self.cask = os.path.join(casks, "deepdrivecleaner.rb")
        rendered = subprocess.run(
            ["sed", "-e", f"s/__VERSION__/{VERSION}/g", "-e", f"s/__SHA256__/{SHA}/g", TEMPLATE],
            check=True, capture_output=True, text=True,
        ).stdout
        with open(self.cask, "w") as f:
            f.write(rendered)
        self.text = rendered

    def stanza_lines(self):
        """The cask's lines without comments or blank lines, stripped."""
        return [l.strip() for l in self.text.splitlines() if l.strip() and not l.strip().startswith("#")]

    def test_the_placeholders_are_all_substituted(self):
        self.assertNotIn("__", self.text)
        self.assertIn(f'version "{VERSION}"', self.text)
        self.assertIn(f'sha256 "{SHA}"', self.text)

    def test_the_cask_says_the_app_updates_itself(self):
        lines = self.stanza_lines()
        self.assertIn("auto_updates true", lines)
        # Homebrew's stanza order: after livecheck, before depends_on.
        self.assertLess(lines.index("end"), lines.index("auto_updates true"))  # livecheck's block ends first
        first_depends = next(i for i, l in enumerate(lines) if l.startswith("depends_on "))
        self.assertEqual(lines.index("auto_updates true") + 1, first_depends)

    def test_zap_trashes_sparkles_cache_and_http_storage_in_alphabetical_order(self):
        block = re.search(r"trash:\s*\[(.*?)\]", self.text, re.S)
        self.assertIsNotNone(block)
        paths = re.findall(r'"([^"]+)"', block.group(1))
        self.assertIn(f"~/Library/Caches/{BUNDLE_ID}", paths)
        self.assertIn(f"~/Library/HTTPStorages/{BUNDLE_ID}", paths)
        self.assertEqual(paths, sorted(paths))
        self.assertEqual(len(paths), len(set(paths)))

    def test_the_rendered_cask_is_valid_ruby(self):
        ruby = shutil.which("ruby")
        if ruby is None:
            self.skipTest("no ruby")
        result = subprocess.run([ruby, "-c", self.cask], capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)

    @unittest.skipUnless(os.environ.get("DDC_TEST_BREW_STYLE") == "1", "set DDC_TEST_BREW_STYLE=1 to run brew style")
    def test_the_rendered_cask_passes_brew_style(self):
        env = dict(os.environ, HOMEBREW_NO_AUTO_UPDATE="1")
        result = subprocess.run(["brew", "style", self.cask], capture_output=True, text=True, env=env)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)


if __name__ == "__main__":
    unittest.main()
