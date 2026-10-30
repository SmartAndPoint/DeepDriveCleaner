"""Tests of gate-scope.sh and ci-green.sh --full: python3 -I -m unittest discover -s scripts/tests -p 'test_*.py'.

Story "Gate by what changed: load at night, one gate per release, app-only pushes skip
Rust" (DDC-005). Each case builds a throwaway repository; a fake gh first in PATH answers
from JSON files, so nothing reaches GitHub. The guard at the end fails when a gate input
names a path on .config/rust-free-paths.
"""
import json
import os
import re
import subprocess
import tempfile
import unittest

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
SCOPE = os.path.join(ROOT, "scripts", "gate-scope.sh")
CI_GREEN = os.path.join(ROOT, "scripts", "ci-green.sh")
LIST = os.path.join(ROOT, ".config", "rust-free-paths")
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
ZERO = "0" * 40

# A gh that answers `gh api <path>` from $FAKE_GH/<path with / and ? as _>.json, and
# `gh run list` from $FAKE_GH/run-list.json; it fails when $FAKE_GH/fail exists. --jq is
# applied with jq, as gh does.
FAKE_GH = """#!/bin/sh
[ -e "$FAKE_GH/fail" ] && { echo "gh: HTTP 502" >&2; exit 1; }
jq_filter=""
if [ "$1" = api ]; then
    file="$FAKE_GH/$(printf '%s' "$2" | tr '/?&=' '____').json"
    shift 2
elif [ "$1 $2" = "run list" ]; then
    file="$FAKE_GH/run-list.json"
    shift 2
else
    echo "fake gh: unexpected $*" >&2; exit 1
fi
while [ "$#" -gt 0 ]; do
    case "$1" in --jq) jq_filter="$2"; shift ;; esac
    shift
done
[ -f "$file" ] || { echo "gh: HTTP 404 ($file)" >&2; exit 1; }
if [ -n "$jq_filter" ]; then jq -r "$jq_filter" "$file"; else cat "$file"; fi
"""


def api_file(path):
    return re.sub(r"[/?&=]", "_", path) + ".json"


class Repo:
    """A throwaway repository with a bare origin."""

    def __init__(self, test):
        self.dir = tempfile.TemporaryDirectory()
        test.addCleanup(self.dir.cleanup)
        self.path = os.path.join(self.dir.name, "work")
        self.origin = os.path.join(self.dir.name, "origin.git")
        self.fake = os.path.join(self.dir.name, "fake-gh")
        os.makedirs(self.fake)
        bin_dir = os.path.join(self.dir.name, "bin")
        os.makedirs(bin_dir)
        gh = os.path.join(bin_dir, "gh")
        with open(gh, "w") as f:
            f.write(FAKE_GH)
        os.chmod(gh, 0o755)
        self.env = dict(GIT_ENV, PATH=bin_dir + ":" + GIT_ENV["PATH"], FAKE_GH=self.fake)
        subprocess.run(["git", "init", "-q", "--bare", "-b", "main", self.origin], env=GIT_ENV, check=True)
        os.makedirs(self.path)
        self.git("init", "-q", "-b", "main")
        self.git("remote", "add", "origin", self.origin)

    def git(self, *args):
        return subprocess.run(
            ["git", *args], cwd=self.path, env=self.env, check=True, capture_output=True, text=True
        ).stdout.strip()

    def write(self, rel, text="x\n"):
        full = os.path.join(self.path, rel)
        os.makedirs(os.path.dirname(full), exist_ok=True)
        with open(full, "w") as f:
            f.write(text)

    def commit(self, *paths, message="change"):
        """Writes each path (fresh content) and commits; returns the sha."""
        for rel in paths:
            self.write(rel, f"{rel} {message}\n")
        self.git("add", "-A")
        self.git("commit", "-q", "--allow-empty", "-m", message)
        return self.git("rev-parse", "HEAD")

    def scope(self, *args, stdin="", env=None):
        result = subprocess.run(
            [SCOPE, *args], cwd=self.path, env=dict(self.env, **(env or {})), input=stdin,
            capture_output=True, text=True,
        )
        return result.stdout.strip(), result

    def answer(self, path, payload):
        with open(os.path.join(self.fake, api_file(path)), "w") as f:
            json.dump(payload, f)


class Paths(unittest.TestCase):
    def classify(self, *paths):
        result = subprocess.run([SCOPE, "paths"], input="\n".join(paths) + "\n", capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        return result.stdout.strip()

    def test_app_sources_the_website_and_release_notes_skip(self):
        self.assertEqual(self.classify("app/DeepDriveCleaner/MenuView.swift"), "skip")
        self.assertEqual(self.classify("website/src/index.html", "docs/releases/0.9.0.md"), "skip")
        self.assertEqual(self.classify("website/ünïcode name.svg"), "skip")

    def test_ddckit_sources_answer_swift(self):
        self.assertEqual(self.classify("app/Packages/DDCKit/Sources/DDCKit/Scan.swift"), "swift")

    def test_paths_a_gate_step_reads_run_the_rust_tiers(self):
        for path in (
            "crates/ddc-core/src/lib.rs",
            "docs/scan.schema.json",
            "app/Packages/DDCKit/Tests/DDCKitTests/Fixtures/disk-report.json",
            "app/DeepDriveCleaner.xcodeproj/project.pbxproj",
            ".config/rust-free-paths",
            "scripts/gate-scope.sh",
            ".github/workflows/gate.yml",
            "Makefile",
            "README.md",
            "application/x",
            "apps/x",
        ):
            self.assertEqual(self.classify(path), "rust", path)

    def test_one_path_off_the_list_wins(self):
        self.assertEqual(self.classify("app/DeepDriveCleaner/A.swift", "crates/ddc-cli/src/main.rs"), "rust")

    def test_no_paths_run_the_rust_tiers(self):
        self.assertEqual(self.classify(), "rust")

    def test_a_usage_error_answers_rust(self):
        result = subprocess.run([SCOPE, "nonsense"], capture_output=True, text=True)
        self.assertEqual((result.returncode, result.stdout.strip()), (2, "rust"))


class Staged(unittest.TestCase):
    def setUp(self):
        self.repo = Repo(self)
        self.repo.commit("crates/a.rs", "app/DeepDriveCleaner/A.swift", message="base")

    def test_app_only_staged_skips(self):
        self.repo.write("app/DeepDriveCleaner/A.swift", "changed\n")
        self.repo.git("add", "-A")
        self.assertEqual(self.repo.scope("staged")[0], "skip")

    def test_a_crate_staged_runs_rust(self):
        self.repo.write("app/DeepDriveCleaner/A.swift", "changed\n")
        self.repo.write("crates/a.rs", "changed\n")
        self.repo.git("add", "-A")
        self.assertEqual(self.repo.scope("staged")[0], "rust")

    def test_unstaged_crate_changes_are_not_this_commits(self):
        self.repo.write("app/DeepDriveCleaner/A.swift", "changed\n")
        self.repo.git("add", "app")
        self.repo.write("crates/a.rs", "changed, not staged\n")
        self.assertEqual(self.repo.scope("staged")[0], "skip")

    def test_nothing_staged_runs_rust(self):
        self.assertEqual(self.repo.scope("staged")[0], "rust")


class Push(unittest.TestCase):
    def setUp(self):
        self.repo = Repo(self)
        self.base = self.repo.commit("crates/a.rs", "app/DeepDriveCleaner/A.swift", message="base")
        self.repo.git("push", "-q", "origin", "main")
        self.repo.git("fetch", "-q", "origin")

    def push(self, *lines):
        return self.repo.scope("push", stdin="".join(line + "\n" for line in lines))[0]

    def line(self, local_sha, remote_sha, ref="refs/heads/main"):
        return f"{ref} {local_sha} {ref} {remote_sha}"

    def test_an_app_change_on_main_skips(self):
        head = self.repo.commit("app/DeepDriveCleaner/A.swift")
        self.assertEqual(self.push(self.line(head, self.base)), "skip")

    def test_a_ddckit_change_answers_swift(self):
        head = self.repo.commit("app/Packages/DDCKit/Sources/DDCKit/S.swift")
        self.assertEqual(self.push(self.line(head, self.base)), "swift")

    def test_a_crate_change_runs_rust(self):
        self.repo.commit("app/DeepDriveCleaner/A.swift", message="one")
        head = self.repo.commit("crates/a.rs", message="two")
        self.assertEqual(self.push(self.line(head, self.base)), "rust")

    def test_a_rename_out_of_crates_runs_rust(self):
        self.repo.git("mv", "crates/a.rs", "app/DeepDriveCleaner/a.rs")
        self.repo.git("commit", "-q", "-m", "move")
        head = self.repo.git("rev-parse", "HEAD")
        self.assertEqual(self.push(self.line(head, self.base)), "rust")

    def test_a_new_branch_is_judged_from_its_merge_base_with_origin_main(self):
        self.repo.git("checkout", "-q", "-b", "gui")
        head = self.repo.commit("app/DeepDriveCleaner/B.swift")
        self.assertEqual(self.push(self.line(head, ZERO, "refs/heads/gui")), "skip")

    def test_a_new_branch_without_origin_main_runs_rust(self):
        self.repo.git("update-ref", "-d", "refs/remotes/origin/main")
        self.repo.git("checkout", "-q", "-b", "gui")
        head = self.repo.commit("app/DeepDriveCleaner/B.swift")
        self.assertEqual(self.push(self.line(head, ZERO, "refs/heads/gui")), "rust")

    def test_an_unknown_remote_sha_runs_rust(self):
        head = self.repo.commit("app/DeepDriveCleaner/A.swift")
        self.assertEqual(self.push(self.line(head, "1" * 40)), "rust")

    def test_a_tag_skips(self):
        self.repo.git("tag", "-a", "-m", "v0.9.0", "v0.9.0")
        tag = self.repo.git("rev-parse", "v0.9.0")
        self.assertEqual(self.push(self.line(tag, ZERO, "refs/tags/v0.9.0")), "skip")

    def test_a_deletion_alone_skips(self):
        self.assertEqual(self.push(self.line(ZERO, self.base, "refs/heads/old")), "skip")

    def test_a_tag_beside_a_branch_with_a_crate_change_runs_rust(self):
        head = self.repo.commit("crates/a.rs")
        self.repo.git("tag", "v0.9.0")
        self.assertEqual(
            self.push(self.line(head, self.base), self.line(head, ZERO, "refs/tags/v0.9.0")), "rust"
        )

    def test_a_new_branch_with_nothing_new_runs_rust(self):
        self.assertEqual(self.push(self.line(self.base, ZERO, "refs/heads/copy")), "rust")

    def test_empty_stdin_runs_rust(self):
        self.assertEqual(self.push(), "rust")

    def test_every_ref_counts(self):
        app = self.repo.commit("app/DeepDriveCleaner/A.swift")
        self.repo.git("checkout", "-q", "-b", "core", self.base)
        core = self.repo.commit("crates/a.rs")
        self.assertEqual(
            self.push(self.line(app, self.base), self.line(core, ZERO, "refs/heads/core")), "rust"
        )


class Ci(unittest.TestCase):
    """`ci <sha>` against the fake gh: the base is the newest ancestor whose push run
    passed gate, coverage and release-build."""

    REPO = "example/ddc"
    RUNS = f"repos/{REPO}/actions/workflows/gate.yml/runs?event=push&status=success&per_page=50"

    def setUp(self):
        self.repo = Repo(self)
        self.green = self.repo.commit("crates/a.rs", "app/DeepDriveCleaner/A.swift", message="green")
        self.runs = []

    def run_for(self, sha, jobs, run_id=None):
        """A successful push run of `sha`, newest first, with these job conclusions."""
        run_id = run_id or 1000 + len(self.runs)
        self.runs.insert(0, {"id": run_id, "head_sha": sha, "created_at": f"2026-10-30T10:{len(self.runs):02d}:00Z"})
        self.repo.answer(f"repos/{self.REPO}/actions/runs/{run_id}/jobs?filter=latest&per_page=100", {
            "jobs": [{"name": name, "conclusion": conclusion} for name, conclusion in jobs.items()]
        })

    def full(self, sha):
        self.run_for(sha, {"scope": "success", "gate": "success", "coverage": "success",
                           "release-build": "success", "app": "success"})

    def ci(self, head, event="push"):
        self.repo.answer(self.RUNS, {"workflow_runs": self.runs})
        out, self.result = self.repo.scope("ci", head, env={"GITHUB_EVENT_NAME": event, "GITHUB_REPOSITORY": self.REPO})
        return out

    def test_an_app_change_on_a_green_base_skips(self):
        self.full(self.green)
        head = self.repo.commit("app/DeepDriveCleaner/A.swift")
        self.assertEqual(self.ci(head), "skip")

    def test_a_chain_of_app_changes_finds_the_base_two_back(self):
        self.full(self.green)
        w = self.repo.commit("app/DeepDriveCleaner/A.swift", message="w")
        self.run_for(w, {"scope": "success", "gate": "skipped", "coverage": "skipped",
                         "release-build": "skipped", "app": "success"})
        x = self.repo.commit("website/index.html", message="x")
        self.assertEqual(self.ci(x), "skip")
        self.assertIn(f"base {self.green[:7]}", self.result.stderr)

    def test_a_green_run_that_skipped_its_rust_jobs_is_not_a_base(self):
        self.full(self.green)
        w = self.repo.commit("crates/a.rs", message="w")
        self.run_for(w, {"scope": "success", "gate": "skipped", "coverage": "skipped",
                         "release-build": "skipped", "app": "success"})
        x = self.repo.commit("app/DeepDriveCleaner/A.swift", message="x")
        self.assertEqual(self.ci(x), "rust")

    def test_a_base_whose_coverage_failed_is_not_a_base(self):
        self.full(self.green)
        y = self.repo.commit("crates/a.rs", message="y")
        # Listed with status=success only if the run passed; a failed coverage fails the
        # run, but a rerun of gate alone could still show it, so the jobs decide.
        self.run_for(y, {"gate": "success", "coverage": "failure", "release-build": "success"})
        x = self.repo.commit("app/DeepDriveCleaner/A.swift", message="x")
        self.assertEqual(self.ci(x), "rust")

    def test_a_crate_change_since_the_base_runs_rust(self):
        self.full(self.green)
        head = self.repo.commit("crates/a.rs")
        self.assertEqual(self.ci(head), "rust")

    def test_a_run_on_another_line_is_not_an_ancestor(self):
        self.repo.git("checkout", "-q", "-b", "other")
        other = self.repo.commit("crates/b.rs", message="other")
        self.full(other)
        self.repo.git("checkout", "-q", "main")
        head = self.repo.commit("app/DeepDriveCleaner/A.swift")
        self.assertEqual(self.ci(head), "rust")

    def test_a_rerun_of_a_green_commit_skips(self):
        self.full(self.green)
        self.assertEqual(self.ci(self.green), "skip")

    def test_no_runs_run_rust(self):
        head = self.repo.commit("app/DeepDriveCleaner/A.swift")
        self.assertEqual(self.ci(head), "rust")

    def test_a_gh_failure_runs_rust(self):
        self.full(self.green)
        head = self.repo.commit("app/DeepDriveCleaner/A.swift")
        open(os.path.join(self.repo.fake, "fail"), "w").close()
        self.assertEqual(self.ci(head), "rust")

    def test_pull_requests_and_dispatches_run_rust(self):
        self.full(self.green)
        head = self.repo.commit("app/DeepDriveCleaner/A.swift")
        self.assertEqual(self.ci(head, event="pull_request"), "rust")
        self.assertEqual(self.ci(head, event="workflow_dispatch"), "rust")


class CiGreenFull(unittest.TestCase):
    """`ci-green.sh --full` refuses a HEAD whose own run skipped its Rust jobs."""

    def setUp(self):
        self.repo = Repo(self)
        self.head = self.repo.commit("app/DeepDriveCleaner/A.swift")
        self.runs([(7, "push")])

    def runs(self, runs):
        """The fake ignores gh's filters: this list stands for HEAD's green runs, newest first."""
        with open(os.path.join(self.repo.fake, "run-list.json"), "w") as f:
            json.dump([{"databaseId": run_id, "event": event, "status": "completed", "conclusion": "success",
                        "url": f"https://example/{run_id}"} for run_id, event in runs], f)

    def jobs(self, conclusion, run_id=7):
        self.repo.answer(f"repos/{{owner}}/{{repo}}/actions/runs/{run_id}/jobs?filter=latest&per_page=100", {"jobs": [
            {"name": name, "conclusion": conclusion} for name in ("gate", "coverage", "release-build")
        ] + [{"name": "app", "conclusion": "success"}]})

    def ci_green(self, *args):
        return subprocess.run([CI_GREEN, *args], cwd=self.repo.path, env=self.repo.env, capture_output=True, text=True)

    def test_full_refuses_a_run_that_skipped_its_rust_jobs(self):
        self.jobs("skipped")
        result = self.ci_green("--full")
        self.assertEqual(result.returncode, 1)
        self.assertIn("gh workflow run gate.yml --ref main", result.stderr)

    def test_full_accepts_a_dispatched_run_of_head(self):
        self.runs([(7, "push"), (8, "workflow_dispatch")])
        self.jobs("skipped", 7)
        self.jobs("success", 8)
        self.assertEqual(self.ci_green("--full").returncode, 0)

    def test_full_passes_a_run_that_ran_them(self):
        self.jobs("success")
        self.assertEqual(self.ci_green("--full").returncode, 0)

    def test_without_full_the_run_conclusion_decides(self):
        self.jobs("skipped")
        self.assertEqual(self.ci_green().returncode, 0)


# Literals in gate inputs that name a listed path but not one in this repository, with why.
NOT_REPO_PATHS = {
    ("scripts/tests/test_build_number.py", "app/DeepDriveCleaner.xcodeproj/project.pbxproj"):
        "written into a throwaway repository, and off the list anyway",
}
# The scripts the gate's Python tests run, besides the tests themselves.
TESTED_SCRIPTS = ("check-build-number.sh", "check-signatures.sh", "debug-feed.sh", "render-appcast.sh", "gate-scope.sh",
                  # run by the Gate's release-build job
                  "build-cli.sh")
STRING = re.compile(r'"((?:[^"\\\n]|\\.)*)"|\'((?:[^\'\\\n]|\\.)*)\'')
# Literals joined into one path: Rust `.join("a").join("b")`, Python `join(ROOT, "a", "b")`.
JOINER = re.compile(r'^\s*(?:\)\s*\.join\(\s*|,\s*)$')


def tracked(*pathspecs):
    out = subprocess.run(["git", "ls-files", "-z", "--", *pathspecs], cwd=ROOT, capture_output=True, text=True, check=True)
    return [p for p in out.stdout.split("\0") if p]


def gate_recipes():
    """The recipe lines of every target `make gate` reaches."""
    deps, recipes, current = {}, {}, None
    with open(os.path.join(ROOT, "Makefile")) as f:
        for line in f:
            if line.startswith("\t") and current:
                recipes.setdefault(current, []).append(line)
                continue
            m = re.match(r"^([A-Za-z0-9_-]+)\s*:(?!=)(.*)$", line)
            if m and "=" not in m.group(2):
                current = m.group(1)
                deps.setdefault(current, []).extend(m.group(2).split())
            elif line.strip() and not line.startswith("#"):
                current = None
    seen, todo = set(), ["gate"]
    while todo:
        target = todo.pop()
        if target not in seen:
            seen.add(target)
            todo.extend(deps.get(target, []))
    return [line for target in seen for line in recipes.get(target, [])]


def candidates(text):
    """Path-like literals, with joined chains rebuilt, relative to the repository root."""
    found = []
    matches = list(STRING.finditer(text))
    i = 0
    while i < len(matches):
        parts = [matches[i].group(1) if matches[i].group(1) is not None else matches[i].group(2)]
        joined = re.search(r"join\([^()\"']*$", text[max(0, matches[i].start() - 40):matches[i].start()])
        j = i
        while j + 1 < len(matches) and JOINER.match(text[matches[j].end():matches[j + 1].start()]):
            j += 1
            parts.append(matches[j].group(1) if matches[j].group(1) is not None else matches[j].group(2))
        literal = "/".join(p.strip("/") for p in parts if p)
        if len(parts) > 1 or "/" in literal or joined:
            found.append(literal)
        i = j + 1
    return found


def normalize(literal):
    """Relative to the root: everything up to the last ../ goes, so a format! prefix too."""
    if "../" in literal:
        literal = literal.rsplit("../", 1)[1]
    while literal.startswith("./"):
        literal = literal[2:]
    return literal


class Guard(unittest.TestCase):
    def test_every_entry_names_a_tracked_path(self):
        files = tracked()
        with open(LIST) as f:
            entries = [line.strip().lstrip("!") for line in f if line.strip() and not line.startswith("#")]
        for entry in entries:
            hit = any(p.startswith(entry) for p in files) if entry.endswith("/") else entry in files
            self.assertTrue(hit, f".config/rust-free-paths names {entry}, which is not tracked")

    def test_no_gate_input_names_a_rust_free_path(self):
        # This file names listed paths on purpose, as cases for the classifier.
        sources = [p for p in tracked("crates", "bench", "scripts/tests", "packaging")
                   if p.endswith((".rs", ".py", ".sh", ".toml")) and p != "scripts/tests/test_gate_scope.py"]
        sources += [f"scripts/{name}" for name in TESTED_SCRIPTS]
        texts = {}
        for rel in sources:
            with open(os.path.join(ROOT, rel), encoding="utf-8", errors="replace") as f:
                texts[rel] = f.read()
        texts["Makefile (gate recipes)"] = "".join(
            " ".join(f'"{word}"' for word in re.split(r"[\s;|&()`'\"]+", line) if "/" in word) + "\n"
            for line in gate_recipes()
        )
        folders = {os.path.dirname(p) for p in tracked()}
        literals = []
        for rel, text in texts.items():
            for literal in candidates(text):
                path = normalize(literal)
                if not path or (rel, path) in NOT_REPO_PATHS:
                    continue
                literals.append((rel, path + "/" if path in folders else path))
        # gate-scope.sh is the one classifier; only literals under a listed top folder can
        # be on the list, so only those are asked one by one.
        with open(LIST) as f:
            tops = {line.strip().lstrip("!").split("/")[0] for line in f if line.strip() and not line.startswith("#")}
        literals = [(rel, path) for rel, path in literals if path.split("/")[0] in tops]
        self.assertTrue(literals, "the guard found no literal under a listed folder: it would pass blind")
        listed = [
            (rel, path) for rel, path in literals
            if subprocess.run([SCOPE, "paths"], input=path + "\n", capture_output=True, text=True).stdout.strip() != "rust"
        ]
        self.assertEqual(listed, [], "gate inputs that name a path on .config/rust-free-paths")


if __name__ == "__main__":
    unittest.main()
