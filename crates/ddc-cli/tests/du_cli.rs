//! `ddc du` against `/usr/bin/du -skx` on a temporary tree.

use std::fs;
use std::path::Path;
use std::process::Command;

fn du_kib(path: &Path) -> u64 {
    let out = Command::new("/usr/bin/du").arg("-skx").arg(path).output().unwrap();
    String::from_utf8(out.stdout).unwrap().split_whitespace().next().unwrap().parse().unwrap()
}

#[test]
fn ddc_du_matches_system_du() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    for i in 0..30usize {
        let dir = root.join(format!("d{}/e{}", i % 4, i % 3));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(format!("f{i}")), vec![7u8; 3000 * i + 1]).unwrap();
    }
    fs::hard_link(root.join("d0/e0/f0"), root.join("d1/hl")).unwrap();
    std::os::unix::fs::symlink("d0", root.join("link")).unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_deepdrivecleaner"))
        .env_clear()
        .env("HOME", &root)
        .env("PATH", "")
        .arg("du")
        .arg(&root)
        .output()
        .unwrap();

    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8(out.stdout).unwrap();
    let (kib, path) = stdout.trim_end().split_once('\t').unwrap();
    assert_eq!(path, root.to_str().unwrap());
    assert_eq!(kib.parse::<u64>().unwrap(), du_kib(&root));
}
