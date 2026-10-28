"""Compares a load table with the newest earlier one of the same size (read-only).

Usage:
    python3 -I bench/compare_load.py bench/load-2026-11-02-M.tsv
    python3 -I bench/compare_load.py new.tsv --against old.tsv

Story "Memory and CPU under load: peak memory, leaks and CPU time" (DDC-005).
Per (profile, size, command), the median over the runs of CPU time (cpu_ms)
and of peak memory (max_rss_bytes) is compared; wall time is not, since it
moves with the machine's other work. A metric more than 15% worse is flagged,
unless the change is below a small absolute floor (scheduler and allocator
noise on tiny runs). Exit 1 when anything is flagged, 0 otherwise.

Without --against, the earlier table is the newest bench/load-<date>-<size>.tsv
dated before the new one, in the new table's folder.
"""
import argparse
import os
import re
import statistics
import sys

WORSE = 0.15
# Below these, a change is noise, whatever its ratio.
FLOOR = {"cpu_ms": 10, "max_rss_bytes": 1 << 20}
METRICS = ("cpu_ms", "max_rss_bytes")
NAME = re.compile(r"^load-(\d{4}-\d{2}-\d{2})-([A-Z]+)\.tsv$")


def read(path):
    """Rows of a table as dicts, keyed by its header row."""
    header, rows = None, []
    with open(path) as f:
        for line in f:
            line = line.rstrip("\n")
            if not line or line.startswith("#"):
                continue
            cols = line.split("\t")
            if header is None:
                header = cols
                continue
            rows.append(dict(zip(header, cols)))
    return rows


def medians(rows):
    """{(profile, size, command): {metric: median}}."""
    runs = {}
    for r in rows:
        key = (r["profile"], r["size"], r["command"])
        for m in METRICS:
            runs.setdefault(key, {}).setdefault(m, []).append(float(r[m]))
    return {k: {m: statistics.median(v) for m, v in ms.items()} for k, ms in runs.items()}


def compare(old_rows, new_rows):
    """(flagged, notes): a line per metric more than 15% worse, and per key on one side only."""
    old, new = medians(old_rows), medians(new_rows)
    flagged, notes = [], []
    for key in sorted(new):
        if key not in old:
            notes.append("new: %s" % "/".join(key))
            continue
        for m in METRICS:
            a, b = old[key][m], new[key][m]
            if b > a * (1 + WORSE) and b - a >= FLOOR[m]:
                pct = (b / a - 1) * 100 if a else float("inf")
                flagged.append("%s %s: %g -> %g (+%.0f%%)" % ("/".join(key), m, a, b, pct))
    notes.extend("gone: %s" % "/".join(k) for k in sorted(old) if k not in new)
    return flagged, notes


def earlier(path):
    """The newest load-<date>-<size>.tsv beside `path`, dated before it."""
    folder, name = os.path.split(os.path.abspath(path))
    m = NAME.match(name)
    if not m:
        sys.exit("compare_load: %s is not named load-<date>-<size>.tsv; pass --against" % name)
    date, size = m.groups()
    found = []
    for other in os.listdir(folder):
        o = NAME.match(other)
        if o and o.group(2) == size and o.group(1) < date:
            found.append((o.group(1), other))
    return os.path.join(folder, max(found)[1]) if found else None


def main(argv):
    p = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    p.add_argument("new")
    p.add_argument("--against")
    a = p.parse_args(argv)
    old = a.against or earlier(a.new)
    if old is None:
        print("compare_load: no earlier table of this size; nothing to compare")
        return 0
    flagged, notes = compare(read(old), read(a.new))
    print("compare_load: %s against %s" % (a.new, old))
    for n in notes:
        print("  note: " + n)
    for f in flagged:
        print("  WORSE: " + f)
    if flagged:
        print("compare_load: %d metric(s) more than %d%% worse" % (len(flagged), WORSE * 100))
        return 1
    print("compare_load: no metric more than %d%% worse" % (WORSE * 100))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
