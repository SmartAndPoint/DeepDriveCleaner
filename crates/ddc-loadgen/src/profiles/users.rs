//! Thousands of per-user folders, each with caches and small files: the
//! shape of `/Users`, `/private/var/folders` and other system trees. One of
//! them is the home, with a fixture for every home-anchored rule.

use crate::desc::{Desc, join};
use crate::fixtures;
use crate::rng::Rng;

pub const HOME: &[u8] = b"Users/load";

pub fn desc(entries: u64, rng: &mut Rng) -> Desc {
    let mut d = Desc::new();
    d.home = Some(HOME.to_vec());
    d.dir(HOME);
    for rule in fixtures::home_rules() {
        fixtures::home(&mut d, HOME, rule, rng);
    }
    home_projects(&mut d, rng);
    let mut i = 0;
    while (d.len() as u64) < entries {
        let user = format!("Users/u{i:05}").into_bytes();
        for app in 0..rng.range(1, 3) {
            fixtures::payload(&mut d, rng, &join(&user, format!("Library/Caches/com.app{app}").as_bytes()), 4);
        }
        fixtures::payload(&mut d, rng, &join(&user, b".cache/tool"), 3);
        let cache = join(&user, b"Projects/p/__pycache__");
        d.file(join(&cache, b"m.pyc"), 1);
        d.plant(&cache, "python.pycache");
        if i % 10 == 0 {
            let tagged = join(&user, b"Library/Caches/tagged");
            d.text(join(&tagged, b"CACHEDIR.TAG"), fixtures::TAG);
            d.plant(&tagged, "generic.cachedir-tag");
        }
        i += 1;
    }
    d
}

/// Project artifacts in the home folder itself. Under a dot folder or
/// `Library` they belong to an app and go to review. A `.git` in the home
/// does not make the home a project root, so its fresh index does not keep
/// `code/target`, whose own manifest is old.
fn home_projects(d: &mut Desc, rng: &mut Rng) {
    let at = |rel: &str| join(HOME, rel.as_bytes());
    d.file(at(".tool/x/uv.lock"), 0);
    d.text(at(".tool/x/.venv/pyvenv.cfg"), "home = /usr/bin\n");
    d.plant(at(".tool/x/.venv"), "python.venv");

    let app = at("Library/Application Support/x");
    d.file(join(&app, b"package.json"), 0);
    d.file(join(&app, b"yarn.lock"), 0);
    fixtures::payload(d, rng, &join(&app, b"node_modules"), 4);
    d.plant(join(&app, b"node_modules"), "node.node_modules");

    d.file(at(".git/HEAD"), 0);
    d.file(at(".git/index"), 0);
    d.fresh(at(".git/index"));
    d.file(at("code/Cargo.toml"), 0);
    fixtures::payload(d, rng, &at("code/target"), 6);
    d.plant(at("code/target"), "rust.target");
}
