"""The coverage floor (story "Coverage floor and property tests for the safety
invariants", DDC-005): line coverage per crate and per safety file never drops
below bench/coverage-floor.tsv.

Usage:
    python3 -I bench/coverage_floor.py check <summary.json> <floor.tsv>
    python3 -I bench/coverage_floor.py ratchet <summary.json> <floor.tsv>

<summary.json> is `cargo llvm-cov report --json --summary-only` (`make coverage`
writes target/coverage/summary.json). `check` fails when a crate or a file is
under its floor, when a row names a crate or file the summary does not have, or
when a safety file has no row. `ratchet` rewrites the TSV with each floor raised
to the measured value less its margin, never lowered; the owner runs it on
`main` from a green CI run's summary (`make coverage-ratchet`). Lowering a floor
is a hand edit of the TSV with a reason.

The TSV: `#` comment lines, then `kind  name  floor  reason` rows, where kind is
`crate` (name: the folder under crates/) or `file` (name: the path from the
repository root), floor a line-coverage percentage, and reason `-` or why the
floor is where it is. A safety file's floor under TARGET needs a reason.
"""
import math
import sys

HEADER = "kind\tname\tfloor\treason"
MARGIN = {"crate": 1.0, "file": 3.0}
TARGET = 90.0
# The cleanup's safety path (ADR "Cleanup safety model: risk classes, plan and
# confirmation"): each needs a row, so deleting one cannot drop its floor.
SAFETY = [
    "crates/ddc-core/src/exec/mod.rs",
    "crates/ddc-core/src/exec/delete.rs",
    "crates/ddc-core/src/exec/guard.rs",
    "crates/ddc-core/src/exec/preflight.rs",
    "crates/ddc-core/src/exec/journal.rs",
    "crates/ddc-core/src/plan.rs",
    "crates/ddc-core/src/rules/classify.rs",
]


def measured(summary):
    """{("crate", name) | ("file", path): (covered, count)} from the summary's files."""
    out = {}
    for f in summary["data"][0]["files"]:
        name = f["filename"].replace("\\", "/")
        at = name.rfind("/crates/")
        rel = name[at + 1 :] if at >= 0 else name
        if not rel.startswith("crates/"):
            continue
        lines = f["summary"]["lines"]
        n = (lines["covered"], lines["count"])
        out[("file", rel)] = n
        crate = ("crate", rel.split("/")[1])
        c = out.get(crate, (0, 0))
        out[crate] = (c[0] + n[0], c[1] + n[1])
    return out


def percent(n):
    return 100.0 * n[0] / n[1] if n[1] else 100.0


def parse(text):
    """(comment lines, rows as [kind, name, floor, reason]); raises ValueError."""
    comments, rows, header = [], [], False
    for i, line in enumerate(text.splitlines(), 1):
        if line.startswith("#") or not line.strip():
            if not header:
                comments.append(line)
            continue
        if not header:
            if line != HEADER:
                raise ValueError(f"line {i}: the header is {HEADER!r}")
            header = True
            continue
        cols = line.split("\t")
        if len(cols) != 4 or cols[0] not in MARGIN:
            raise ValueError(f"line {i}: kind<TAB>name<TAB>floor<TAB>reason, kind crate or file")
        try:
            floor = float(cols[2])
        except ValueError:
            raise ValueError(f"line {i}: the floor is a percentage, not {cols[2]!r}") from None
        rows.append([cols[0], cols[1], floor, cols[3]])
    if not header:
        raise ValueError(f"no header {HEADER!r}")
    return comments, rows


def check(summary, text):
    """The problems, one line each; empty when every floor holds."""
    have = measured(summary)
    _, rows = parse(text)
    problems = []
    named = {(k, n) for k, n, _, _ in rows}
    for path in SAFETY:
        if ("file", path) not in named:
            problems.append(f"file {path}: a safety file with no row")
    for kind, name, floor, reason in rows:
        if (kind, name) not in have:
            problems.append(f"{kind} {name}: not in the summary (renamed or removed? edit the row)")
            continue
        pct = percent(have[(kind, name)])
        if pct < floor:
            problems.append(f"{kind} {name}: {pct:.2f}% is under the floor {floor:.1f}%")
        if name in SAFETY and floor < TARGET and reason in ("", "-"):
            problems.append(f"file {name}: a floor under {TARGET:.0f}% needs a reason")
    return problems


def ratchet(summary, text):
    """The TSV with each floor raised to the measured value less its margin."""
    have = measured(summary)
    comments, rows = parse(text)
    out = list(comments) + [HEADER]
    for kind, name, floor, reason in rows:
        if (kind, name) in have:
            # One decimal, rounded down, so a floor never sits above what was measured.
            floor = max(floor, math.floor((percent(have[(kind, name)]) - MARGIN[kind]) * 10) / 10)
        out.append(f"{kind}\t{name}\t{floor:.1f}\t{reason}")
    return "\n".join(out) + "\n"


def main(argv):
    if len(argv) != 4 or argv[1] not in ("check", "ratchet"):
        print(__doc__.split("\n\n")[1], file=sys.stderr)
        return 2
    import json

    with open(argv[2]) as f:
        summary = json.load(f)
    with open(argv[3]) as f:
        text = f.read()
    try:
        if argv[1] == "ratchet":
            new = ratchet(summary, text)
            with open(argv[3], "w") as f:
                f.write(new)
            print(f"{argv[3]}: {'raised' if new != text else 'unchanged'}")
            return 0
        problems = check(summary, text)
    except ValueError as e:
        print(f"coverage_floor: {argv[3]}: {e}", file=sys.stderr)
        return 2
    for p in problems:
        print(f"coverage_floor: {p}", file=sys.stderr)
    if not problems:
        print(f"coverage_floor: every floor in {argv[3]} holds")
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
