//! One project root with ten thousand and more siblings and many findings
//! directly inside it: the shape of `aa64c83` step 2, where every finding
//! re-read the same root. Venvs sit right under the root, which holds the
//! `requirements.txt`, so the lock-file check lists the root once per venv.

use crate::desc::{Desc, join};
use crate::rng::Rng;

pub fn desc(entries: u64, rng: &mut Rng) -> Desc {
    let mut d = Desc::new();
    let root = b"root";
    d.file(join(root, b".git/HEAD"), 0);
    d.file(join(root, b".git/index"), 0);
    d.file(join(root, b"requirements.txt"), 0);
    d.file(join(root, b"package.json"), 0);
    // A twentieth of the budget each for venvs and caches, three entries apiece.
    let findings = (entries / 20 / 3).max(1);
    for i in 0..findings {
        let venv = join(root, format!("venv{i:05}").as_bytes());
        d.text(join(&venv, b"pyvenv.cfg"), "home = /usr/bin\n");
        d.file(join(&venv, b"lib.py"), 1);
        d.plant(&venv, "python.venv");
        let cache = join(root, format!("m{i:05}/__pycache__").as_bytes());
        d.file(join(&cache, b"m.pyc"), *rng.pick(&[0, 1]));
        d.plant(&cache, "python.pycache");
    }
    let mut i = 0;
    while (d.len() as u64) < entries {
        d.file(join(root, format!("s{i:06}.txt").as_bytes()), 0);
        i += 1;
    }
    d
}
