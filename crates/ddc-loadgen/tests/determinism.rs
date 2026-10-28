//! The same profile, size, seed and file system capabilities give the same
//! manifest hash; another seed gives another tree.

use ddc_loadgen::desc::FsCaps;
use ddc_loadgen::manifest::manifest;
use ddc_loadgen::profiles::Profile;

const S: u64 = 10_000;
const APFS: FsCaps = FsCaps { case_insensitive: true, normalization_insensitive: true, invalid_utf8: false };

fn hash(p: Profile, seed: u64, caps: FsCaps) -> String {
    manifest(p.name(), seed, &p.desc(S, seed, caps)).hash()
}

#[test]
fn same_seed_gives_the_same_hash() {
    for p in Profile::ALL {
        assert_eq!(hash(p, 7, APFS), hash(p, 7, APFS), "{}", p.name());
    }
}

#[test]
fn another_seed_gives_another_hash() {
    for p in Profile::ALL {
        assert_ne!(hash(p, 7, APFS), hash(p, 8, APFS), "{}", p.name());
    }
}

#[test]
fn only_names_depends_on_the_file_system() {
    let linux = FsCaps { case_insensitive: false, normalization_insensitive: false, invalid_utf8: true };
    for p in Profile::ALL {
        let same = hash(p, 7, APFS) == hash(p, 7, linux);
        assert_eq!(same, p != Profile::Names, "{}", p.name());
    }
}

#[test]
fn every_profile_has_about_the_requested_size() {
    for p in Profile::ALL {
        let m = manifest(p.name(), 1, &p.desc(S, 1, APFS));
        assert!((S..S + S / 50).contains(&m.entries), "{}: {} entries", p.name(), m.entries);
    }
}
