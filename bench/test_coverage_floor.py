"""Tests of coverage_floor.py: python3 -I -m unittest discover -s bench -p 'test_*.py'."""
import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import coverage_floor  # noqa: E402

ROOT = "/Users/x/DeepDriveCleaner/"


def summary(files):
    """A llvm-cov summary of {path from the root: (covered, count)}."""
    rows = [{"filename": ROOT + p, "summary": {"lines": {"covered": c, "count": n}}} for p, (c, n) in files.items()]
    return {"data": [{"files": rows}]}


# Every safety file at 95%, and one other core file and one CLI file.
FILES = {p: (95, 100) for p in coverage_floor.SAFETY}
FILES["crates/ddc-core/src/scan/walker.rs"] = (80, 100)
FILES["crates/ddc-cli/src/main.rs"] = (40, 50)


def floor(rows):
    lines = ["# a comment", coverage_floor.HEADER]
    lines += ["\t".join(map(str, r)) for r in rows]
    return "\n".join(lines) + "\n"


SAFE = [("file", p, "92.0", "-") for p in coverage_floor.SAFETY]


class Check(unittest.TestCase):
    def test_every_floor_holds(self):
        # ddc-core: 7 * 95 + 80 over 800 lines = 93.1%.
        text = floor([("crate", "ddc-core", "93.0", "-"), ("crate", "ddc-cli", "80.0", "-")] + SAFE)
        self.assertEqual(coverage_floor.check(summary(FILES), text), [])

    def test_a_crate_under_its_floor_fails(self):
        text = floor([("crate", "ddc-core", "93.2", "-")] + SAFE)
        [p] = coverage_floor.check(summary(FILES), text)
        self.assertIn("crate ddc-core: 93.12% is under the floor 93.2%", p)

    def test_a_file_under_its_floor_fails(self):
        files = dict(FILES)
        files["crates/ddc-core/src/exec/guard.rs"] = (91, 100)
        [p] = coverage_floor.check(summary(files), floor(SAFE))
        self.assertIn("exec/guard.rs: 91.00% is under the floor 92.0%", p)

    def test_a_safety_file_needs_a_row(self):
        [p] = coverage_floor.check(summary(FILES), floor(SAFE[1:]))
        self.assertIn("exec/mod.rs: a safety file with no row", p)

    def test_a_safety_floor_under_the_target_needs_a_reason(self):
        rows = [("file", p, "85.0", "-") for p in coverage_floor.SAFETY[:1]] + SAFE[1:]
        [p] = coverage_floor.check(summary(FILES), floor(rows))
        self.assertIn("needs a reason", p)
        rows = [("file", p, "85.0", "SIGKILL paths") for p in coverage_floor.SAFETY[:1]] + SAFE[1:]
        self.assertEqual(coverage_floor.check(summary(FILES), floor(rows)), [])

    def test_a_row_the_summary_lacks_fails(self):
        [p] = coverage_floor.check(summary(FILES), floor(SAFE + [("crate", "ddc-gone", "50.0", "-")]))
        self.assertIn("crate ddc-gone: not in the summary", p)

    def test_a_malformed_floor_is_an_error(self):
        with self.assertRaisesRegex(ValueError, "line 3: the floor is a percentage"):
            coverage_floor.check(summary(FILES), floor([("crate", "ddc-core", "high", "-")]))
        with self.assertRaisesRegex(ValueError, "the header is"):
            coverage_floor.check(summary(FILES), "crate\tddc-core\t90\t-\n")


class Ratchet(unittest.TestCase):
    def test_raises_by_the_margin_and_never_lowers(self):
        text = floor([("crate", "ddc-core", "50.0", "-"), ("crate", "ddc-cli", "85.0", "kept")] + SAFE)
        new = coverage_floor.ratchet(summary(FILES), text)
        _, rows = coverage_floor.parse(new)
        got = {(k, n): (f, r) for k, n, f, r in rows}
        # 93.125 - 1, rounded down to a tenth.
        self.assertEqual(got[("crate", "ddc-core")], (92.1, "-"))
        # 80% measured is under the old 85: kept, not lowered.
        self.assertEqual(got[("crate", "ddc-cli")], (85.0, "kept"))
        # 95 - 3 = 92, the old floor.
        self.assertEqual(got[("file", "crates/ddc-core/src/plan.rs")], (92.0, "-"))
        self.assertTrue(new.startswith("# a comment\n" + coverage_floor.HEADER + "\n"))
        # The ratchet hides no drop: the crate under its kept floor still fails.
        [p] = coverage_floor.check(summary(FILES), new)
        self.assertIn("crate ddc-cli: 80.00% is under the floor 85.0%", p)
        self.assertEqual(coverage_floor.ratchet(summary(FILES), new), new)


if __name__ == "__main__":
    unittest.main()
