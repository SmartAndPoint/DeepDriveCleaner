"""Tests of make_baseline.py's coding agents' rows: python3 -I -m unittest discover -s bench -p 'test_*.py'.

A temporary home stands in for $HOME; nothing reads the real one.
"""
import contextlib
import io
import os
import sys
import tempfile
import unittest
from unittest import mock

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import make_baseline  # noqa: E402

PROJECT = ".claude/projects/-Users-example-proj"


def plant(home, rel, name="f"):
    os.makedirs(os.path.join(home, rel), exist_ok=True)
    with open(os.path.join(home, rel, name), "w") as f:
        f.write("x")


class AgentRows(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.home = os.path.realpath(self.tmp.name)
        self.patch = mock.patch.object(make_baseline, "H", self.home)
        self.patch.start()

    def tearDown(self):
        self.patch.stop()
        self.tmp.cleanup()

    def test_known_rows_name_the_eight_rules_with_their_class(self):
        plant(self.home, PROJECT)
        plant(self.home, ".claude/projects/-Users-example-other")
        rows = {rel: (rule, cls) for rel, rule, cls in make_baseline.known()}
        self.assertEqual(rows[".claude/debug"], ("agent.claude-code-scratch", "safe"))
        self.assertEqual(rows[".claude/paste-cache"], ("agent.claude-code-history", "review"))
        self.assertEqual(rows[".claude/file-history"], ("agent.claude-code-file-history", "review"))
        self.assertEqual(rows[PROJECT], ("agent.claude-code-transcripts", "review"))
        self.assertEqual(rows[".local/share/opencode/storage"], ("agent.opencode-data", "review"))
        agents = {rule for rule, _ in rows.values() if rule.startswith("agent.")}
        self.assertEqual(len(agents), 8)
        safe = [rel for rel, (rule, cls) in rows.items() if rule.startswith("agent.") and cls == "safe"]
        for name in ("paste-cache", "usage-data", "file-history", "projects", "shell-snapshots"):
            self.assertFalse([rel for rel in safe if name in rel], name)
        # Never the folder that holds auth.json and opencode.db.
        self.assertNotIn(".local/share/opencode", rows)

    def test_artifacts_in_sealed_folders_are_no_rows(self):
        for rel in (".claude/plugins/cache/p", ".cursor/extensions/vendor.ext-1.0.0", ".codex/worktrees/w1"):
            plant(self.home, rel, "package.json")
            plant(self.home, rel, "package-lock.json")
            plant(self.home, rel + "/node_modules")
        data = tempfile.TemporaryDirectory()
        self.addCleanup(data.cleanup)
        with open(os.path.join(data.name, "devartifacts-2026-10-17.txt"), "w") as f:
            for rel in (".claude/plugins/cache/p", ".cursor/extensions/vendor.ext-1.0.0", ".codex/worktrees/w1"):
                f.write(f"~/{rel}/node_modules\n")
        out = io.StringIO()
        with mock.patch.object(make_baseline, "HERE", data.name), contextlib.redirect_stdout(out), \
                contextlib.redirect_stderr(io.StringIO()):
            make_baseline.main()
        rows = [line.split("\t") for line in out.getvalue().splitlines()[2:]]
        found = {path: rule for path, _, rule, *_ in rows}
        self.assertEqual(found, {"~/.codex/worktrees/w1/node_modules": "node.node_modules"})


if __name__ == "__main__":
    unittest.main()
