//! Chains of folders up to the `PATH_MAX` boundary, each inside a
//! `node_modules`. `PATH_MAX` (1024 on macOS) counts the NUL: the deepest
//! readable folder has a 1023-byte path, and its child, at 1025 bytes, fails
//! to open with `ENAMETOOLONG`. The scan counts that child as one error,
//! lists nothing below it, and the finding is incomplete; the files of the
//! last readable folder still count.

use crate::desc::{Desc, join};
use crate::rng::Rng;
use crate::target::ROOT_LEN;

const PATH_MAX: usize = 1024;
/// Tree-relative length of the deepest readable folder.
const DEEPEST: usize = PATH_MAX - 1 - ROOT_LEN - "/".len();

pub fn fill(d: &mut Desc, entries: u64, rng: &mut Rng) {
    let mut k = 0;
    while (d.len() as u64) < entries {
        let project = format!("dpm/c{k:06}").into_bytes();
        d.file(join(&project, b"package.json"), 0);
        d.file(join(&project, b"yarn.lock"), 0);
        let mut at = join(&project, b"node_modules");
        d.plant(&at, "node.node_modules");
        while at.len() < DEEPEST {
            // `/x` adds 2 bytes; one `/xx` evens out an odd remainder.
            let name: &[u8] = if (DEEPEST - at.len()) % 2 == 1 { b"xx" } else { b"x" };
            at = join(&at, name);
            d.file(join(&at, b"f"), *rng.pick(&[0, 1]));
        }
        assert_eq!(at.len(), DEEPEST);
        let child = join(&at, b"y");
        d.file(join(&child, b"f"), 1);
        d.dir(join(&child, b"z"));
        d.unreadable.push(child);
        k += 1;
    }
}
