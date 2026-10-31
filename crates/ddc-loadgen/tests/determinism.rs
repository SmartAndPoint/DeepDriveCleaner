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
fn only_names_and_mixed_depend_on_the_file_system() {
    let linux = FsCaps { case_insensitive: false, normalization_insensitive: false, invalid_utf8: true };
    for p in Profile::ALL {
        let same = hash(p, 7, APFS) == hash(p, 7, linux);
        assert_eq!(same, !matches!(p, Profile::Names | Profile::Mixed), "{}", p.name());
    }
}

#[test]
fn every_profile_has_about_the_requested_size() {
    for p in Profile::ALL {
        let m = manifest(p.name(), 1, &p.desc(S, 1, APFS));
        assert!((S..S + S / 50).contains(&m.entries), "{}: {} entries", p.name(), m.entries);
    }
}

/// The S trees of seed 1, pinned: a change to a profile, the description or
/// the manifest changes a hash, and the change is then deliberate. Update the
/// table with `ddc-loadgen manifest <profile>` (APFS capabilities).
const PINNED: &[(Profile, &str)] = &[
    (Profile::Projects, "8aa6e9516b6f830c"),
    (Profile::Wide, "8756587ab5d78167"),
    (Profile::WideRoot, "1277cc49407eed0c"),
    (Profile::Deep, "ff9ec73e270608ca"),
    (Profile::DeepPathMax, "648cd98930695a03"),
    (Profile::Users, "72f8e6cf186a7e13"),
    (Profile::Links, "48fa1a46fe3a8adb"),
    (Profile::Names, "58769f53aca9df23"),
    (Profile::SystemRoot, "7936cdf86a065216"),
    (Profile::Hostile, "653c3bec918eead1"),
    (Profile::Mixed, "5160d88da614d858"),
];

#[test]
fn the_s_hashes_are_pinned() {
    assert_eq!(PINNED.len(), Profile::ALL.len(), "a profile without a pinned hash");
    for (p, want) in PINNED {
        assert_eq!(hash(*p, 1, APFS), *want, "{}", p.name());
    }
}
