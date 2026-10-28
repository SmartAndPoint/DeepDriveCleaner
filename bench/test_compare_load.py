"""Tests of compare_load.py: python3 -I -m unittest discover -s bench -p 'test_*.py'."""
import contextlib
import io
import os
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import compare_load  # noqa: E402

COLUMNS = "profile size seed entries command run wall_ms user_ms sys_ms cpu_ms max_rss_bytes threads_max exit"


def table(rows, wall=1.0, cpu=1.0, rss=1.0):
    """A table of `rows` (profile, command, cpu_ms, max_rss_bytes, wall_ms), each scaled."""
    lines = ["# date=2026-10-29 commit=x model=m ncpu=10 load_start=0.5 load_end=0.6 mode=enforce seed=1"]
    lines.append("\t".join(COLUMNS.split()))
    for i, (profile, command, c, r, w) in enumerate(rows):
        user = int(c * cpu * 0.8)
        cols = [profile, "M", "1", "100000", command, str(i % 3 + 1), str(int(w * wall)), str(user)]
        cols += [str(int(c * cpu) - user), str(int(c * cpu)), str(int(r * rss)), "11", "0"]
        lines.append("\t".join(cols))
    return "\n".join(lines) + "\n"


# Three runs of two commands on one profile.
BASE = [
    ("mixed", "scan", 4000, 200_000_000, 900),
    ("mixed", "scan", 4100, 201_000_000, 950),
    ("mixed", "scan", 3900, 199_000_000, 880),
    ("mixed", "clean-dry-run", 2000, 150_000_000, 500),
    ("mixed", "clean-dry-run", 2050, 151_000_000, 510),
    ("mixed", "clean-dry-run", 1990, 149_000_000, 490),
]


class Compare(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.TemporaryDirectory()
        self.addCleanup(self.dir.cleanup)

    def write(self, name, text):
        path = os.path.join(self.dir.name, name)
        with open(path, "w") as f:
            f.write(text)
        return path

    def run_main(self, *argv):
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            code = compare_load.main(list(argv))
        return code, out.getvalue()

    def test_a_seeded_20_percent_regression_is_flagged(self):
        old = self.write("load-2026-10-01-M.tsv", table(BASE))
        for kind in ("cpu", "rss"):
            new = self.write("new-%s.tsv" % kind, table(BASE, **{kind: 1.2}))
            code, out = self.run_main(new, "--against", old)
            self.assertEqual(code, 1, out)
            metric = "cpu_ms" if kind == "cpu" else "max_rss_bytes"
            self.assertIn("mixed/M/scan %s" % metric, out)
            self.assertIn("mixed/M/clean-dry-run %s" % metric, out)

    def test_an_unchanged_rerun_passes(self):
        old = self.write("load-2026-10-01-M.tsv", table(BASE))
        new = self.write("load-2026-10-02-M.tsv", table(BASE))
        code, out = self.run_main(new, "--against", old)
        self.assertEqual(code, 0, out)
        self.assertNotIn("WORSE", out)

    def test_a_wall_time_only_change_is_ignored(self):
        old = self.write("load-2026-10-01-M.tsv", table(BASE))
        new = self.write("load-2026-10-02-M.tsv", table(BASE, wall=3.0))
        self.assertEqual(self.run_main(new, "--against", old)[0], 0)

    def test_the_median_ignores_one_slow_run(self):
        slow = list(BASE)
        slow[0] = ("mixed", "scan", 9000, 200_000_000, 2000)
        old = self.write("a.tsv", table(BASE))
        new = self.write("b.tsv", table(slow))
        self.assertEqual(self.run_main(new, "--against", old)[0], 0)

    def test_a_change_below_the_floor_is_noise(self):
        tiny = [("wide", "scan", 20, 10_000_000, 30)]
        old = self.write("a.tsv", table(tiny))
        new = self.write("b.tsv", table(tiny, cpu=1.4, rss=1.05))
        self.assertEqual(self.run_main(new, "--against", old)[0], 0)

    def test_the_earlier_table_is_the_newest_before_of_the_same_size(self):
        self.write("load-2026-09-01-M.tsv", table(BASE))
        newest = self.write("load-2026-10-01-M.tsv", table(BASE))
        self.write("load-2026-10-15-S.tsv", table(BASE))
        self.write("load-2026-11-01-M.tsv", table(BASE))
        new = self.write("load-2026-10-20-M.tsv", table(BASE, cpu=1.2))
        self.assertEqual(compare_load.earlier(new), newest)
        code, out = self.run_main(new)
        self.assertEqual(code, 1, out)
        self.assertIn(newest, out)

    def test_the_first_table_has_nothing_to_compare(self):
        new = self.write("load-2026-10-20-M.tsv", table(BASE))
        self.assertEqual(self.run_main(new)[0], 0)

    def test_commands_on_one_side_only_are_notes(self):
        old = self.write("a.tsv", table(BASE[:3]))
        new = self.write("b.tsv", table(BASE[3:]))
        code, out = self.run_main(new, "--against", old)
        self.assertEqual(code, 0, out)
        self.assertIn("note: new: mixed/M/clean-dry-run", out)
        self.assertIn("note: gone: mixed/M/scan", out)


if __name__ == "__main__":
    unittest.main()
