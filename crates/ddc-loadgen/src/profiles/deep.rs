//! Depth: chains 200 levels deep with names of 1–3 bytes, half of them
//! inside a `node_modules`, and a branch of 255-byte names (`NAME_MAX`) at
//! most 3 levels deep. With the 160-byte root every path stays under
//! `PATH_MAX`: about 160 + 22 + 200·4 + 2 = 984 bytes at the deepest.

use crate::desc::{Desc, join};
use crate::rng::Rng;

const DEPTH: usize = 200;
// No `f`: each level holds a file named `f`.
const LETTERS: &[u8] = b"abcdeghijklmnopqrstuvwxyz";

pub fn desc(entries: u64, rng: &mut Rng) -> Desc {
    let mut d = Desc::new();
    for k in 0..3 {
        let mut at = format!("long/{}", "L".repeat(255)).into_bytes();
        for level in 0..3 {
            let name: String = std::iter::repeat_n(char::from(b'a' + k), 254).chain(std::iter::once('x')).collect();
            if level > 0 {
                at = join(&at, name.as_bytes());
            }
            d.file(join(&at, format!("f{k}").as_bytes()), 1);
        }
    }
    let mut k = 0;
    while (d.len() as u64) < entries {
        let base = format!("deep/c{k:02}").into_bytes();
        let mut at = if k % 2 == 0 {
            d.file(join(&base, b"package.json"), 0);
            d.file(join(&base, b"yarn.lock"), 0);
            d.plant(join(&base, b"node_modules"), "node.node_modules");
            join(&base, b"node_modules")
        } else {
            base
        };
        for _ in 0..DEPTH {
            let len = rng.range(1, 3) as usize;
            let name: Vec<u8> = (0..len).map(|_| *rng.pick(LETTERS)).collect();
            at = join(&at, &name);
            d.file(join(&at, b"f"), *rng.pick(&[0, 1]));
        }
        k += 1;
    }
    d
}
