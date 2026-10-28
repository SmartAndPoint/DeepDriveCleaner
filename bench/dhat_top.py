"""The top allocation sites of a dhat heap profile, per entry (read-only).

Usage:
    python3 -I bench/dhat_top.py target/dhat/dhat-M-mixed.json --entries 100041

Story "Memory and CPU under load: peak memory, leaks and CPU time" (DDC-005).
A dhat program point is a whole stack; its site here is the innermost frame in
our code: a file of crates/ddc-core or crates/ddc-cli, or a `ddc_` function where
the line is unknown. dhat keeps the last three components of a path, so a bare
`src/scan/walker.rs` counts as ours when that file exists in one of the two
crates and the frame's function is not the standard library's. A frame at
column 0 is skipped: its line is the enclosing function's signature, and the
real call site is the next frame outward. A stack with no such frame goes to its first frame outside the
standard library and the allocator. Points with one site are added up. Two tables
of the top 10 sites: by bytes live at the global peak (`gb`), and by total bytes
allocated over the run (`tb`), each also divided by the entries.
"""
import argparse
import json
import os
import re
import sys

TOP = 10
ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
CRATES = ("crates/ddc-core", "crates/ddc-cli")
# The global allocator's own frame (`ddc_core::heap`) is no site.
ALLOCATOR = "crates/ddc-core/src/heap.rs"
# Functions of the allocator, the standard library and the profiler itself.
LIBRARY = re.compile(r"^<?(alloc|core|std|hashbrown|dhat)\b|^__rust|^\[root\]|^alloc_impl|^try_allocate_in|^finish_grow")
# `0x1234: name (file:line:col)`, or a bare name.
FRAME = re.compile(r"^(?:0x[0-9a-f]+: )?(?P<name>.*?)(?: \((?P<file>[^()]*?):(?P<line>\d+):(?P<col>\d+)\))?$")
STD_FILE = re.compile(r"^(alloc|core|std)/|^src/|/dhat-[0-9.]+/|^dhat-")
OURS = re.compile(r"(?:^|/)(ddc-(?:core|cli)/src/.*)$")


def parse(frame):
    """(name, file, line, col); file is None when dhat has none."""
    m = FRAME.match(frame)
    file = m.group("file")
    return m.group("name"), None if file in (None, "???") else file, m.group("line"), m.group("col")


def ours(frame, root=ROOT):
    """The frame's place in our code, `crates/<crate>/src/...:line`, or None."""
    place = _ours(frame, root)
    return None if place and place.startswith(ALLOCATOR + ":") else place


def _ours(frame, root):
    name, file, line, col = parse(frame)
    if LIBRARY.search(name) or col == "0" and file:
        return None
    if file:
        m = OURS.search(file)
        if m:
            return "crates/%s:%s" % (m.group(1), line)
        if file.startswith("src/"):
            for crate in CRATES:
                if os.path.isfile(os.path.join(root, crate, file)):
                    return "%s/%s:%s" % (crate, file, line)
    if name.lstrip("<").startswith("ddc_"):
        return name
    return None


def site(frames, root=ROOT):
    """The site a program point is charged to."""
    for f in frames:
        place = ours(f, root)
        if place:
            return place
    for f in frames:
        name, file, line, _ = parse(f)
        # The standard library's files are `alloc/src/..`, `std/src/..`, or bare `src/..`.
        if not LIBRARY.search(name) and not (file and STD_FILE.search(file)):
            return "%s (%s:%s)" % (name, file, line) if file else name
    return parse(frames[0])[0] if frames else "[unknown]"


def sites(profile, root=ROOT):
    """{site: {"gb": .., "tb": .., "tbk": ..}} over every program point."""
    table = profile["ftbl"]
    out = {}
    for pp in profile["pps"]:
        s = site([table[i] for i in pp["fs"]], root)
        acc = out.setdefault(s, {"gb": 0, "tb": 0, "tbk": 0})
        for k in acc:
            acc[k] += pp.get(k, 0)
    return out


def top(by_site, key, entries, n=TOP):
    """Lines of the top `n` sites by `key`, with bytes per entry."""
    ranked = sorted(by_site.items(), key=lambda kv: (-kv[1][key], kv[0]))[:n]
    lines = []
    for s, v in ranked:
        per = v[key] / entries if entries else float("nan")
        lines.append("%14d %10.1f %10d  %s" % (v[key], per, v["tbk"], s))
    return lines


def main(argv):
    p = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    p.add_argument("profile")
    p.add_argument("--entries", type=int, required=True)
    a = p.parse_args(argv)
    with open(a.profile) as f:
        profile = json.load(f)
    by_site = sites(profile)
    total_gb = sum(v["gb"] for v in by_site.values())
    total_tb = sum(v["tb"] for v in by_site.values())
    print("%s: %d entries, %d bytes live at the global peak (%.1f per entry), %d allocated in total (%.1f per entry)"
          % (a.profile, a.entries, total_gb, total_gb / a.entries, total_tb, total_tb / a.entries))
    for key, title in (("gb", "bytes live at the global peak"), ("tb", "total bytes allocated")):
        print("\ntop %d sites by %s:" % (TOP, title))
        print("%14s %10s %10s  %s" % ("bytes", "per entry", "blocks", "site"))
        for line in top(by_site, key, a.entries):
            print(line)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
