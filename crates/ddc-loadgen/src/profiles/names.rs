//! Awkward names: NFC and NFD pairs, case pairs, spaces, newlines, control
//! characters, names that look like flags, and names that are not UTF-8.
//! A pair whose two spellings name one entry on this file system, and bytes
//! it refuses, are left out; the manifest records which (`fs_caps`).

use crate::desc::{Desc, FsCaps, join};
use crate::rng::Rng;

/// Names every file system accepts as distinct entries.
const PLAIN: &[&[u8]] = &[
    b"with space",
    b" leading space",
    b"trailing space ",
    b"new\nline",
    b"tab\there",
    b"\x01control",
    b"-rf",
    b"--help",
    b"-",
    b"*",
    b"?",
    b"[x]",
    b"back\\slash",
    b"quote\"s",
    b"apos'trophe",
    b"$HOME",
    b"emoji \xf0\x9f\x93\xa6",
    b"\xd0\xba\xd0\xb8\xd1\x80\xd0\xb8\xd0\xbb\xd0\xbb\xd0\xb8\xd1\x86\xd0\xb0",
    b".hidden",
    b"..dots",
];

pub fn fill(d: &mut Desc, entries: u64, rng: &mut Rng, caps: FsCaps) {
    let mut names: Vec<Vec<u8>> = PLAIN.iter().map(|n| n.to_vec()).collect();
    // `é` precomposed, and `e` + combining acute.
    names.push("caf\u{e9}".into());
    if !caps.normalization_insensitive {
        names.push("cafe\u{301}".into());
    }
    names.push(b"Readme".to_vec());
    if !caps.case_insensitive {
        names.push(b"README".to_vec());
    }
    // 255 bytes of two-byte characters and one ASCII letter.
    names.push(["\u{430}".repeat(127), "x".to_owned()].concat().into_bytes());
    if caps.invalid_utf8 {
        names.push(b"bad\xff\xfe".to_vec());
    }
    d.caps = Some(caps);
    let mut k = 0;
    while (d.len() as u64) < entries {
        let set = format!("names/s{k:04}").into_bytes();
        for name in &names {
            d.file(join(&set, name), *rng.pick(&[0, 1]));
            let project = join(&join(&set, b"p"), name);
            d.file(join(&project, b"package.json"), 0);
            d.file(join(&project, b"yarn.lock"), 0);
            d.file(join(&project, &[b"node_modules/", name.as_slice()].concat()), 1);
            d.plant(join(&project, b"node_modules"), "node.node_modules");
        }
        k += 1;
    }
}
