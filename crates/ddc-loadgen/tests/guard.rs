//! The generator builds only in a marked temporary folder or on its own
//! disk image, whatever `$HOME` says.

use std::fs;

use ddc_core::platform::procs;
use ddc_loadgen::target::{self, Facts, Image, MARKER, Refusal, TEMP_MAX, decide};

const S: u64 = 10_000;

fn facts() -> Facts {
    Facts { target_dev: 5, home_dev: 1, root_dev: 2, marked_temp: false, image: None, entries: S }
}

#[test]
fn decision_table() {
    let image =
        |mounted: &str| Some(Image { mounted: mounted.to_owned(), expected: "/dev/disk9s1".to_owned(), marked: true });
    let cases = [
        (Facts { marked_temp: true, target_dev: 1, ..facts() }, Ok(())),
        (Facts { marked_temp: true, ..facts() }, Ok(())),
        (Facts { image: image("/dev/disk9s1"), ..facts() }, Ok(())),
        (Facts { target_dev: 1, ..facts() }, Err(Refusal::Unmarked)),
        (Facts { target_dev: 2, ..facts() }, Err(Refusal::Unmarked)),
        (facts(), Err(Refusal::Unmarked)),
        (
            Facts { image: image("/dev/disk3s5"), ..facts() },
            Err(Refusal::WrongDevice { mounted: "/dev/disk3s5".into(), expected: "/dev/disk9s1".into() }),
        ),
        (Facts { image: image("/dev/disk9s1"), target_dev: 1, ..facts() }, Err(Refusal::ImageOnSystemVolume)),
        (Facts { image: image("/dev/disk9s1"), target_dev: 2, ..facts() }, Err(Refusal::ImageOnSystemVolume)),
        // A volume `load-image.sh` did not mark, though it is the attached device.
        (
            Facts { image: Some(Image { marked: false, ..image("/dev/disk9s1").unwrap() }), ..facts() },
            Err(Refusal::UnmarkedImage),
        ),
        // An image takes any size.
        (Facts { image: image("/dev/disk9s1"), entries: 10_000_000, ..facts() }, Ok(())),
        // An image claim is checked as an image even in a marked folder.
        (
            Facts { image: image("/dev/disk3s5"), marked_temp: true, target_dev: 1, ..facts() },
            Err(Refusal::WrongDevice { mounted: "/dev/disk3s5".into(), expected: "/dev/disk9s1".into() }),
        ),
    ];
    for (f, want) in cases {
        assert_eq!(decide(&f), want, "{f:?}");
    }
    // A marked temporary folder takes S trees only, on macOS.
    let large = Facts { marked_temp: true, entries: S + 1, ..facts() };
    let want = match TEMP_MAX {
        Some(max) => Err(Refusal::TooLargeForTemp { entries: S + 1, max }),
        None => Ok(()),
    };
    assert_eq!(decide(&large), want);
}

#[test]
fn a_marked_folder_is_accepted() {
    let dir = target::marked().unwrap();
    assert_eq!(decide(&target::facts(dir.path(), None, S).unwrap()), Ok(()));
}

#[test]
fn a_temporary_folder_without_the_marker_is_refused() {
    let dir = tempfile::Builder::new().prefix(target::PREFIX).tempdir().unwrap();
    assert_eq!(decide(&target::facts(dir.path(), None, S).unwrap()), Err(Refusal::Unmarked));
    fs::write(dir.path().join(MARKER), "another-nonce").unwrap();
    assert_eq!(decide(&target::facts(dir.path(), None, S).unwrap()), Err(Refusal::Unmarked));
}

#[test]
fn a_marked_folder_inside_another_folder_is_refused() {
    let outer = target::marked().unwrap();
    let inner = outer.path().join(format!("{}abc", target::PREFIX));
    fs::create_dir(&inner).unwrap();
    fs::write(inner.join(MARKER), "abc").unwrap();
    assert_eq!(decide(&target::facts(&inner, None, S).unwrap()), Err(Refusal::Unmarked));
}

#[test]
fn the_real_home_and_the_boot_volume_are_refused() {
    let home = procs::real_home().expect("a home");
    // Only facts are read here: nothing is written to either folder.
    assert_eq!(decide(&target::facts(&home, None, S).unwrap()), Err(Refusal::Unmarked));
    assert_eq!(decide(&target::facts(std::path::Path::new("/"), None, S).unwrap()), Err(Refusal::Unmarked));
}

#[test]
fn a_mount_that_is_not_the_attached_image_is_refused() {
    let dir = target::marked().unwrap();
    let f = target::facts(dir.path(), Some("/dev/disk-not-attached"), S).unwrap();
    assert!(matches!(decide(&f), Err(Refusal::WrongDevice { .. })), "{f:?}");
}

/// The volume of a marked temporary folder is not the generator's image:
/// its root (the Data volume) holds no marker.
#[test]
fn an_unmarked_volume_is_not_an_image() {
    let dir = target::marked().unwrap();
    let mounted = ddc_core::platform::statfs::statfs(dir.path()).unwrap().mounted_from;
    let f = target::facts(dir.path(), Some(&mounted), S).unwrap();
    assert!(matches!(f.image, Some(Image { marked: false, .. })), "{f:?}");
    // Refused first for its volume; an unmarked image elsewhere is the decision table's.
    assert_eq!(decide(&f), Err(Refusal::ImageOnSystemVolume));
    // The folder itself holds a marker, which is what an image's root must hold.
    assert!(target::holds_marker(dir.path()));
}

/// `clean` removes only the generator's own folders, locked ones included,
/// and leaves an unmarked or mismatched `ddc-load-*` folder alone.
#[test]
fn clean_removes_only_marked_folders() {
    use std::os::unix::fs::PermissionsExt as _;

    let tmp = tempfile::tempdir().unwrap();
    let tmp = tmp.path().canonicalize().unwrap();
    let mark = |name: &str, nonce: Option<&str>| {
        let dir = tmp.join(name);
        fs::create_dir_all(dir.join("tree/sealed")).unwrap();
        if let Some(n) = nonce {
            fs::write(dir.join(MARKER), n).unwrap();
        }
        dir
    };
    let ours = mark("ddc-load-abc", Some("abc"));
    fs::set_permissions(ours.join("tree/sealed"), fs::Permissions::from_mode(0o000)).unwrap();
    let unmarked = mark("ddc-load-def", None);
    let mismatched = mark("ddc-load-ghi", Some("xyz"));
    let other = mark("unrelated", Some("unrelated"));

    let removed = target::clean_in(&tmp).unwrap();
    assert_eq!(removed, std::slice::from_ref(&ours));
    assert!(!ours.exists());
    for kept in [unmarked, mismatched, other] {
        assert!(kept.exists(), "{}", kept.display());
    }
}
