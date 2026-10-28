//! One module per profile. Each turns a size and a seed into a [`Desc`]
//! with about that many entries; the same arguments give the same tree.

mod deep;
mod links;
mod names;
mod projects;
mod users;
mod wide;
mod wide_root;

use crate::desc::{Desc, FsCaps};
use crate::manifest::fnv1a;
use crate::rng::Rng;

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Profile {
    Projects,
    Wide,
    WideRoot,
    Deep,
    Users,
    Links,
    Names,
}

impl Profile {
    pub const ALL: [Profile; 7] = [
        Profile::Projects,
        Profile::Wide,
        Profile::WideRoot,
        Profile::Deep,
        Profile::Users,
        Profile::Links,
        Profile::Names,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Profile::Projects => "projects",
            Profile::Wide => "wide",
            Profile::WideRoot => "wide-root",
            Profile::Deep => "deep",
            Profile::Users => "users",
            Profile::Links => "links",
            Profile::Names => "names",
        }
    }

    /// The tree for `entries`, `seed` and the file system's `caps` (only
    /// `names` reads them).
    pub fn desc(self, entries: u64, seed: u64, caps: FsCaps) -> Desc {
        // Each profile draws its own stream from one seed.
        let mut rng = Rng::new(seed ^ fnv1a(self.name().as_bytes()));
        let d = match self {
            Profile::Projects => projects::desc(entries, &mut rng),
            Profile::Wide => wide::desc(entries, &mut rng),
            Profile::WideRoot => wide_root::desc(entries, &mut rng),
            Profile::Deep => deep::desc(entries, &mut rng),
            Profile::Users => users::desc(entries, &mut rng),
            Profile::Links => links::desc(entries, &mut rng),
            Profile::Names => names::desc(entries, &mut rng, caps),
        };
        d.finish()
    }
}
