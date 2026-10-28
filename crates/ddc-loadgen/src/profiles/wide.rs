//! Single folders with very many entries: a flat folder of files outside
//! any finding, and a flat `node_modules`.

use crate::desc::{Desc, join};
use crate::rng::Rng;

pub fn desc(entries: u64, rng: &mut Rng) -> Desc {
    let mut d = Desc::new();
    let half = entries / 2;
    for i in 0..half {
        d.file(format!("wide/flat/f{i:07}"), *rng.pick(&[0, 0, 0, 1]));
    }
    let base = b"wide/app";
    d.file(join(base, b"package.json"), 0);
    d.file(join(base, b"yarn.lock"), 0);
    for i in 0..entries.saturating_sub(d.len() as u64) {
        d.file(join(base, format!("node_modules/m{i:07}").as_bytes()), *rng.pick(&[0, 1]));
    }
    d.plant(join(base, b"node_modules"), "node.node_modules");
    d
}
