"""Coverage check for epic DDC-001 against the reference-machine baseline (read-only).

Usage:
    ./target/release/deepdrivecleaner du --rules ~ > /tmp/findings.tsv
    python3 -I bench/check_baseline.py bench/baseline-2026-10-17.tsv /tmp/findings.tsv

A baseline row is covered when a finding has the same rule id and its path
equals the row's path or one contains the other. Rows whose path no longer
exists are ignored; rows du cannot read are reported but are not misses.
Coverage = covered bytes / all bytes, both recomputed with `du -skx` today.
"""
import os
import subprocess
import sys

H = os.path.expanduser("~")


def du(path):
    r = subprocess.run(["du", "-skx", path], capture_output=True, text=True)
    out = r.stdout.split()
    return (int(out[0]) * 1024 if out else -1), (r.returncode == 0)


def nested(a, b):
    return a == b or a.startswith(b.rstrip("/") + "/") or b.startswith(a.rstrip("/") + "/")


def main(baseline_path, findings_path):
    findings = {}
    with open(findings_path) as f:
        for line in f:
            cols = line.rstrip("\n").split("\t")
            if len(cols) >= 3:
                findings.setdefault(cols[2], []).append(cols[0])
    covered = total = 0
    misses, unreadable, gone = [], [], 0
    with open(baseline_path) as f:
        for line in f:
            if line.startswith("#") or line.startswith("path\t"):
                continue
            path, _bytes, rule, _cls = line.rstrip("\n").split("\t")[:4]
            full = path.replace("~", H, 1)
            if not os.path.lexists(full):
                gone += 1
                continue
            size, complete = du(full)
            if size < 0:
                unreadable.append(path)
                continue
            total += size
            if any(nested(full, p) for p in findings.get(rule, [])):
                covered += size
            else:
                misses.append((size, path, rule))
    pct = 100.0 * covered / total if total else 100.0
    print(f"rows: covered {covered / 2**30:.1f} GiB of {total / 2**30:.1f} GiB = {pct:.1f}%")
    print(f"misses: {len(misses)}; unreadable (not misses): {len(unreadable)}; gone: {gone}")
    for size, path, rule in sorted(misses, reverse=True)[:40]:
        print(f"  MISS {size / 2**20:9.1f} MiB  {rule:28} {path}")
    for path in unreadable:
        print(f"  UNREADABLE {path}")
    return 0 if pct >= 90.0 and not misses else 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1], sys.argv[2]))
