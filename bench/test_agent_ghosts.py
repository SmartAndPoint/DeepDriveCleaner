"""Tests of demo/agent-ghosts.sh's refusals: python3 -I -m unittest discover -s bench -p 'test_*.py'.

A temporary folder stands in for $HOME; nothing reads or writes the real one. Each case is
refused before the script makes anything, so nothing is built and nothing is left behind.
"""
import os
import subprocess
import tempfile
import unittest

SCRIPT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "demo", "agent-ghosts.sh")


class Refusals(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.home = os.path.join(self.tmp.name, "home")
        self.outside = os.path.join(self.tmp.name, "outside")
        os.makedirs(self.home)
        os.makedirs(self.outside)

    def run_script(self, *args, cwd=None):
        env = {"PATH": "/usr/bin:/bin", "HOME": self.home}
        return subprocess.run(
            [SCRIPT, *args], cwd=cwd or self.outside, env=env, capture_output=True, text=True, timeout=60
        )

    def assert_refused(self, result, *never_made):
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("agent-ghosts:", result.stderr)
        for path in never_made:
            self.assertFalse(os.path.lexists(path), f"{path} was made")

    def test_a_folder_inside_the_home_is_refused_and_not_made(self):
        target = os.path.join(self.home, "x", "y")
        self.assert_refused(self.run_script(target), os.path.join(self.home, "x"))
        self.assertEqual(os.listdir(self.home), [])

    def test_a_relative_folder_inside_the_home_is_refused_and_not_made(self):
        self.assert_refused(self.run_script("x/y", cwd=self.home), os.path.join(self.home, "x"))
        self.assertEqual(os.listdir(self.home), [])

    def test_a_symlink_into_the_home_is_refused_and_not_followed(self):
        link = os.path.join(self.outside, "link")
        os.symlink(self.home, link)
        self.assert_refused(self.run_script(os.path.join(link, "new")), os.path.join(self.home, "new"))
        self.assertEqual(os.listdir(self.home), [])

    def test_a_climbing_path_is_refused_and_not_made(self):
        target = os.path.join(self.outside, "new", "..", "..", "home", "x")
        self.assert_refused(self.run_script(target), os.path.join(self.outside, "new"), os.path.join(self.home, "x"))

    def test_stop_of_a_folder_that_does_not_exist_makes_none(self):
        target = os.path.join(self.outside, "never")
        self.assert_refused(self.run_script(target, "stop"), target)


if __name__ == "__main__":
    unittest.main()
