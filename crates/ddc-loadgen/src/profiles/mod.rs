//! One module per profile. Each turns a size and a seed into a [`Desc`]
//! with about that many entries; the same arguments give the same tree.

mod deep;
mod deep_path_max;
pub mod hostile;
mod links;
mod names;
mod projects;
mod system_root;
mod users;
mod wide;
mod wide_root;

use crate::desc::{Desc, FsCaps};
use crate::manifest::fnv1a;
use crate::rng::Rng;

/// The parts of `mixed` and their shares of it, in percent, after the
/// reference machine's home: mostly projects and app caches, some very wide
/// and very deep folders. `system-root` has a home of its own and `hostile`
/// changes the tree while it is scanned, so neither is a part.
const MIXED: &[(Profile, u64)] = &[
    (Profile::Projects, 35),
    (Profile::Users, 20),
    (Profile::Wide, 10),
    (Profile::WideRoot, 8),
    (Profile::Deep, 10),
    (Profile::DeepPathMax, 5),
    (Profile::Links, 7),
    (Profile::Names, 5),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Profile {
    Projects,
    Wide,
    WideRoot,
    Deep,
    DeepPathMax,
    Users,
    Links,
    Names,
    SystemRoot,
    Hostile,
    Mixed,
}

impl Profile {
    pub const ALL: [Profile; 11] = [
        Profile::Projects,
        Profile::Wide,
        Profile::WideRoot,
        Profile::Deep,
        Profile::DeepPathMax,
        Profile::Users,
        Profile::Links,
        Profile::Names,
        Profile::SystemRoot,
        Profile::Hostile,
        Profile::Mixed,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Profile::Projects => "projects",
            Profile::Wide => "wide",
            Profile::WideRoot => "wide-root",
            Profile::Deep => "deep",
            Profile::DeepPathMax => "deep-path-max",
            Profile::Users => "users",
            Profile::Links => "links",
            Profile::Names => "names",
            Profile::SystemRoot => "system-root",
            Profile::Hostile => "hostile",
            Profile::Mixed => "mixed",
        }
    }

    /// The tree for `entries`, `seed` and the file system's `caps` (only
    /// `names` and `mixed` read them).
    pub fn desc(self, entries: u64, seed: u64, caps: FsCaps) -> Desc {
        // Each profile draws its own stream from one seed.
        let mut rng = Rng::new(seed ^ fnv1a(self.name().as_bytes()));
        let mut d = Desc::new();
        match self {
            Profile::Mixed => {
                // Each part fills the tree up to its share of the total.
                let mut upto = 0;
                for (part, percent) in MIXED {
                    upto += entries * percent / 100;
                    part.fill(&mut d, upto, &mut rng, caps);
                }
            }
            part => part.fill(&mut d, entries, &mut rng, caps),
        }
        d.finish()
    }

    /// Adds this profile's shape to `d` until it holds `entries` in all.
    /// The profiles use separate top-level folders, so they combine.
    fn fill(self, d: &mut Desc, entries: u64, rng: &mut Rng, caps: FsCaps) {
        match self {
            Profile::Projects => projects::fill(d, entries, rng),
            Profile::Wide => wide::fill(d, entries, rng),
            Profile::WideRoot => wide_root::fill(d, entries, rng),
            Profile::Deep => deep::fill(d, entries, rng),
            Profile::DeepPathMax => deep_path_max::fill(d, entries, rng),
            Profile::Users => users::fill(d, entries, rng),
            Profile::Links => links::fill(d, entries, rng),
            Profile::Names => names::fill(d, entries, rng, caps),
            Profile::SystemRoot => system_root::fill(d, entries, rng),
            Profile::Hostile => hostile::fill(d, entries, rng),
            Profile::Mixed => unreachable!("mixed is not a part of itself"),
        }
    }
}
