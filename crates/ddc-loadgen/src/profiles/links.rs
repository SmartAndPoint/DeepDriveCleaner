//! Links: many hard links to the same files, inside and across findings;
//! symlink loops, dangling symlinks and symlinks out of the tree. Every
//! symlink target stays under 60 bytes, which ext4 stores without a block.

use crate::desc::{Desc, join};
use crate::rng::Rng;

pub fn desc(entries: u64, rng: &mut Rng) -> Desc {
    let mut d = Desc::new();
    d.symlink("links/loop/self", "../loop");
    d.symlink("links/loop/a", "b");
    d.symlink("links/loop/b", "a");
    d.symlink("links/out/root", "/");
    d.symlink("links/out/etc", "/etc");
    // A finding whose files all have their second name inside it.
    let inner = b"links/p/node_modules";
    d.file(b"links/p/package.json", 0);
    d.file(b"links/p/yarn.lock", 0);
    d.file(join(inner, b"a"), 2);
    d.hardlink(join(inner, b"b"), join(inner, b"a"));
    d.plant(inner, "node.node_modules");
    // A finding sharing a file with the outside.
    let shared = b"links/q/node_modules";
    d.file(b"links/q/package.json", 0);
    d.file(b"links/q/yarn.lock", 0);
    d.file(b"links/files/shared", 1);
    d.hardlink(join(shared, b"x"), b"links/files/shared");
    d.plant(shared, "node.node_modules");
    let mut i = 0;
    while (d.len() as u64) < entries {
        let file = format!("links/files/f{i:06}").into_bytes();
        d.file(&file, *rng.pick(&[0, 1]));
        for k in 0..rng.range(1, 4) {
            d.hardlink(format!("links/names/n{i:06}-{k}"), &file);
        }
        if rng.chance(20) {
            d.symlink(format!("links/dangling/d{i:06}"), format!("gone/{i}"));
        }
        if rng.chance(10) {
            d.symlink(format!("links/back/b{i:06}"), "../files");
        }
        i += 1;
    }
    d
}
