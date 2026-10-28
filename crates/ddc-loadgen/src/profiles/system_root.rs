//! A fake data volume for `ddc disk --system-root <tree>`: the areas of the
//! disk view under `System/Volumes/Data` (apps, the shared Library, macOS
//! assets, `/opt`, `/usr/local`, `/private` with per-user temporary folders,
//! crash dumps) and a `Users` folder with the home in it. No tool the view
//! asks (`diskutil`, `tmutil`, `simctl`, `brew`) exists in it.

use crate::desc::{Desc, join};
use crate::fixtures;
use crate::profiles::users;
use crate::rng::Rng;

pub const DATA: &[u8] = b"System/Volumes/Data";

/// The areas and how much of the tree each holds, in parts of 100.
const AREAS: &[(&str, u64)] = &[
    ("Applications", 8),
    ("Library/Caches", 4),
    ("Library/Updates", 2),
    ("Library/Developer/CoreSimulator/Caches", 2),
    ("System/Library/AssetsV2/com_apple_MobileAsset_Font7", 3),
    ("opt/homebrew/Cellar", 6),
    ("usr/local/lib", 2),
    ("private/var/db", 2),
    ("private/tmp", 2),
    ("cores", 1),
    ("Users/Shared", 2),
];

pub fn fill(d: &mut Desc, entries: u64, rng: &mut Rng) {
    let budget = entries.saturating_sub(d.len() as u64);
    let at = |rel: &str| join(DATA, rel.as_bytes());
    for (area, parts) in AREAS {
        let n = budget * parts / 100;
        // A few folders per area, so no single one is a flat list.
        for k in 0..4 {
            fixtures::payload(d, rng, &join(&at(area), format!("p{k}").as_bytes()), n / 4);
        }
    }
    d.file(at("private/var/vm/sleepimage"), 16);
    // Per-user temporary folders: `<2 chars>/<hash>/{C,T}`.
    let folders = budget * 16 / 100;
    let mut i = 0;
    while i * 24 < folders {
        let user = at(&format!("private/var/folders/{:02x}/h{i:06}", i % 256));
        for sub in [&b"C"[..], b"T"] {
            fixtures::payload(d, rng, &join(&user, sub), 10);
        }
        i += 1;
    }
    users::users(d, &at("Users"), entries, rng);
}
