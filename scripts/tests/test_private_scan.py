"""Tests of private-scan.sh: python3 -I -m unittest discover -s scripts/tests -p 'test_*.py'.

Each case makes a throwaway git repository with its own .env and
.local/private-strings, so the maintainer's own files are never read.
"""
import os
import subprocess
import tempfile
import unittest

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
SCRIPT = os.path.join(ROOT, "scripts", "private-scan.sh")

SECRET = "s3cret-signing-identity (ABCDE12345)"
HOST = "lab-box-7"


class PrivateScan(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.repo = tmp.name
        self.git("init", "-q")
        self.write(".env", f'export DDC_SIGN_IDENTITY="{SECRET}"\nSHORT=abc\n# a comment\n')
        self.write(".local/private-strings", f"# machines\n\n{HOST}\n10.0.0.42\n")
        self.write("README.md", "start\n")
        self.git("add", "README.md")
        self.git("-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "start")

    def git(self, *args):
        subprocess.run(["git", *args], cwd=self.repo, check=True, capture_output=True)

    def write(self, rel, text):
        path = os.path.join(self.repo, rel)
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "w") as f:
            f.write(text)

    def scan(self, *args):
        env = {k: v for k, v in os.environ.items() if not k.startswith("DDC_PRIVATE_")}
        return subprocess.run([SCRIPT, *args], cwd=self.repo, env=env, capture_output=True, text=True)

    def stage(self, rel, text):
        self.write(rel, text)
        self.git("add", rel)

    def assert_hidden(self, result):
        for value in (SECRET, HOST, "10.0.0.42"):
            self.assertNotIn(value, result.stdout + result.stderr)

    def test_an_env_value_in_an_added_line_is_refused_by_its_name_only(self):
        self.stage("docs/notes.md", f"signed with {SECRET} here\n")
        result = self.scan("staged")
        self.assertEqual(result.returncode, 1)
        self.assertIn("docs/notes.md holds the value of DDC_SIGN_IDENTITY in .env", result.stderr)
        self.assert_hidden(result)

    def test_a_private_string_matches_as_a_word_ignoring_case(self):
        self.stage("a.sh", f"ssh {HOST.upper()} true\n")
        self.stage("b.sh", f"ssh {HOST}9 true\nping 10.0.0.420\n")
        result = self.scan("staged")
        self.assertEqual(result.returncode, 1)
        self.assertIn("a.sh holds line 3 of .local/private-strings", result.stderr)
        self.assertNotIn("b.sh", result.stderr)
        self.assert_hidden(result)

    def test_a_file_name_is_checked_too(self):
        self.stage(f"notes/{HOST}.md", "nothing private\n")
        result = self.scan("staged")
        self.assertEqual(result.returncode, 1)
        self.assertIn("a staged file name holds line 3", result.stderr)

    def test_only_added_lines_count(self):
        self.write("README.md", f"start {SECRET}\n")
        self.git("add", "README.md")
        self.git("-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "old")
        self.write("README.md", f"start {SECRET}\nmore\n")
        self.git("add", "README.md")
        self.assertEqual(self.scan("staged").returncode, 0)

    def test_short_values_and_clean_commits_pass(self):
        self.stage("c.txt", "abc and nothing else\n")
        result = self.scan("staged")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stderr, "")

    def test_a_commit_message_is_checked(self):
        self.write("msg", f"fix: run it on {HOST}\n")
        result = self.scan("message", os.path.join(self.repo, "msg"))
        self.assertEqual(result.returncode, 1)
        self.assertIn("the commit message holds line 3", result.stderr)
        self.assert_hidden(result)

    def test_without_the_local_files_nothing_is_checked(self):
        os.remove(os.path.join(self.repo, ".env"))
        os.remove(os.path.join(self.repo, ".local", "private-strings"))
        self.stage("d.txt", f"{SECRET} {HOST}\n")
        self.assertEqual(self.scan("staged").returncode, 0)

    def test_a_worktree_reads_the_main_checkouts_files(self):
        wt = os.path.join(self.repo, "..", os.path.basename(self.repo) + "-wt")
        self.git("worktree", "add", "-q", "--detach", wt)
        self.addCleanup(lambda: subprocess.run(["rm", "-rf", wt]))
        with open(os.path.join(wt, "e.txt"), "w") as f:
            f.write(f"{HOST}\n")
        subprocess.run(["git", "add", "e.txt"], cwd=wt, check=True)
        env = {k: v for k, v in os.environ.items() if not k.startswith("DDC_PRIVATE_")}
        result = subprocess.run([SCRIPT, "staged"], cwd=wt, env=env, capture_output=True, text=True)
        self.assertEqual(result.returncode, 1)
        self.assert_hidden(result)


if __name__ == "__main__":
    unittest.main()
