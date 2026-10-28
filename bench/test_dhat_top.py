"""Tests of dhat_top.py: python3 -I -m unittest discover -s bench -p 'test_*.py'."""
import contextlib
import io
import json
import os
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import dhat_top  # noqa: E402

FTBL = [
    "[root]",
    "0x1: alloc (dhat-0.3.3/src/lib.rs:1176:9)",
    "0x2: <alloc::raw_vec::RawVecInner>::try_allocate_in (src/raw_vec/mod.rs:466:47)",
    "0x3: <std::path::Path>::_join (std/src/path.rs:3082:28)",
    "0x4: {closure#0} (src/scan/walker.rs:315:33)",
    "0x5: write_last_scan (ddc-core/src/store.rs:60:16)",
    "0x6: ddc_core::report::scan_inner (???:0:0)",
    "0x7: rayon_core::registry::main_loop (rayon-core/src/registry.rs:1:1)",
    "0x8: std::rt::lang_start (std/src/rt.rs:1:1)",
    "0x9: __rust_alloc (ddc-core/src/heap.rs:13:1)",
    "0xa: try_allocate_in<alloc::alloc::Global> (ddc-core/src/store.rs:59:0)",
    "0xb: grow (ddc-core/src/scan/walker.rs:120:0)",
    "0xc: <std::sys::fs::File>::open (src/scan/walker.rs:9:9)",
]
WALKER = "crates/ddc-core/src/scan/walker.rs:315"


def profile():
    return {
        "dhatFileVersion": 2,
        "mode": "rust-heap",
        "ftbl": FTBL,
        "pps": [
            # Two stacks through the same line of ours add up.
            {"tb": 1000, "tbk": 10, "gb": 400, "fs": [1, 2, 3, 4, 6, 8]},
            {"tb": 500, "tbk": 5, "gb": 100, "fs": [1, 3, 4, 8]},
            {"tb": 9000, "tbk": 90, "gb": 50, "fs": [1, 2, 10, 5, 6, 8]},
            # None of ours: the first frame outside the library.
            {"tb": 200, "tbk": 2, "gb": 300, "fs": [1, 2, 7, 8]},
        ],
    }


class DhatTop(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.TemporaryDirectory()
        self.addCleanup(self.dir.cleanup)
        self.root = self.dir.name
        os.makedirs(os.path.join(self.root, "crates/ddc-core/src/scan"))
        open(os.path.join(self.root, "crates/ddc-core/src/scan/walker.rs"), "w").close()

    def test_a_stack_is_charged_to_its_innermost_frame_of_ours(self):
        frames = lambda *i: [FTBL[n] for n in i]  # noqa: E731
        self.assertEqual(dhat_top.site(frames(1, 2, 3, 4, 6), self.root), WALKER)
        # The global allocator's frame is skipped.
        self.assertEqual(dhat_top.site(frames(9, 1, 4), self.root), WALKER)
        # A crate path dhat kept whole, and a `ddc_` name without a line.
        self.assertEqual(dhat_top.site(frames(1, 2, 5, 6), self.root), "crates/ddc-core/src/store.rs:60")
        # A column-0 frame names the enclosing signature, a library name is not ours
        # even in a file of ours: both go to the next frame outward.
        self.assertEqual(dhat_top.site(frames(1, 10, 5), self.root), "crates/ddc-core/src/store.rs:60")
        self.assertEqual(dhat_top.site(frames(1, 11, 12, 4), self.root), WALKER)
        self.assertEqual(dhat_top.site(frames(1, 2, 6), self.root), "ddc_core::report::scan_inner")
        # `src/raw_vec/mod.rs` is not a file of ours.
        self.assertEqual(
            dhat_top.site(frames(1, 2, 7, 8), self.root),
            "rayon_core::registry::main_loop (rayon-core/src/registry.rs:1)",
        )
        self.assertEqual(dhat_top.site(frames(1, 2), self.root), "alloc")

    def test_sites_add_up_and_rank_by_each_key(self):
        by_site = dhat_top.sites(profile(), self.root)
        self.assertEqual(by_site[WALKER], {"gb": 500, "tb": 1500, "tbk": 15})
        gb = dhat_top.top(by_site, "gb", 100)
        self.assertIn(WALKER, gb[0])
        self.assertIn(" 5.0 ", gb[0])
        tb = dhat_top.top(by_site, "tb", 100)
        self.assertIn("store.rs:60", tb[0])
        self.assertEqual(len(dhat_top.top(by_site, "tb", 100, n=2)), 2)

    def test_main_prints_both_tables(self):
        with tempfile.TemporaryDirectory() as d:
            path = os.path.join(d, "dhat.json")
            with open(path, "w") as f:
                json.dump(profile(), f)
            out = io.StringIO()
            with contextlib.redirect_stdout(out):
                self.assertEqual(dhat_top.main([path, "--entries", "100"]), 0)
        text = out.getvalue()
        self.assertIn("850 bytes live at the global peak (8.5 per entry)", text)
        self.assertIn("top 10 sites by total bytes allocated", text)


if __name__ == "__main__":
    unittest.main()
