//! `ddc disk docker` end to end (story "Docker Desktop: what is inside its
//! disk image, and what a clean frees"): a temporary HOME and data
//! directory, and `docker` a shell script first on `PATH` that logs its
//! calls and prints a fixture. Nothing real is run. The scan, the
//! background scan and a dry run never call it.

#![cfg(unix)]

use ddc_core::platform::Canonical as _;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

use ddc_core::docker::{DockerContents, DockerResult};
use ddc_core::rules::DOCKER_DOWN;
use ddc_loadgen::isolate::ClearEnv as _;
use serde_json::json;

mod common;
mod contract;

struct Fx {
    _tmp: tempfile::TempDir,
    root: PathBuf,
}

impl Fx {
    /// `docker` answers its probe and `system df -v` while `{root}/down`
    /// does not exist; with it, the probe fails as docker 29 words it.
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let fx = Self { root: tmp.path().canonical().unwrap(), _tmp: tmp };
        let r = fx.root.display().to_string();
        let df = json!({
            "Images": [
                {"Containers": "0", "CreatedAt": "", "CreatedSince": "", "Digest": "", "ID": "sha256:0",
                 "Repository": "<none>", "SharedSize": "0B", "Size": "123.4MB", "Tag": "<none>", "UniqueSize": "123.4MB"},
            ],
            "Containers": [],
            "Volumes": [{"Availability": "N/A", "Driver": "local", "Group": "N/A", "Labels": "", "Links": "0",
                         "Mountpoint": "/m", "Name": "vol-a", "Scope": "local", "Size": "1.5GB", "Status": "N/A"}],
            "BuildCache": [
                {"CacheType": "regular", "CreatedAt": "", "CreatedSince": "", "Description": "", "ID": "a",
                 "InUse": "false", "LastUsedAt": "", "LastUsedSince": "", "Parent": "", "Shared": "false",
                 "Size": "512MB", "UsageCount": "1"},
            ],
        });
        fs::write(fx.root.join("df.json"), df.to_string()).unwrap();
        fs::create_dir_all(fx.root.join("bin")).unwrap();
        let docker = fx.root.join("bin/docker");
        fs::write(
            &docker,
            format!(
                r#"#!/bin/sh
echo "$*" >> {r}/docker-calls
if [ -f {r}/down ]; then
  echo "failed to connect to the docker API at unix:///x/docker.sock; check if the daemon is running" >&2; exit 1
fi
case "$*" in
  "--context desktop-linux version --format {{{{.Server.Version}}}}") echo 29.1.2 ;;
  "--context desktop-linux system df -v --format {{{{json .}}}}") /bin/cat {r}/df.json ;;
  *) exit 1 ;;
esac
"#
            ),
        )
        .unwrap();
        common::set_mode(&docker, 0o755);
        // Docker Desktop's folder, where each platform's catalog looks for it.
        for rel in
            ["home/Library/Containers/com.docker.docker/Data/vms/0/Docker.raw", "home/.docker/desktop/vms/0/data.raw"]
        {
            let path = fx.root.join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, vec![7u8; 50_000]).unwrap();
        }
        fx
    }

    fn ddc(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_deepdrivecleaner"))
            .clear_env()
            .home_env(self.root.join("home"))
            .env("PATH", self.root.join("bin"))
            .env("NO_COLOR", "1")
            .args(args)
            .arg("--data-dir")
            .arg(self.root.join("data"))
            .stdin(Stdio::null())
            .output()
            .unwrap()
    }

    fn calls(&self) -> Vec<String> {
        fs::read_to_string(self.root.join("docker-calls")).unwrap_or_default().lines().map(str::to_owned).collect()
    }
}

#[test]
fn the_query_runs_the_probe_and_one_read_and_prints_its_json() {
    if common::unsupported("the_query_runs_the_probe_and_one_read_and_prints_its_json") {
        return;
    }
    let fx = Fx::new();
    let out = fx.ddc(&["disk", "docker", "--json"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let contents: DockerContents = contract::holds("docker", &out.stdout);
    let DockerResult::Listed { images, volumes, build_cache, estimate, .. } = contents.result else {
        panic!("{contents:?}")
    };
    assert_eq!((images.count, images.bytes), (1, Some(123_400_000)));
    assert_eq!((volumes.count, volumes.bytes), (1, Some(1_500_000_000)));
    assert_eq!((build_cache.count, build_cache.bytes), (1, Some(512_000_000)));
    assert_eq!(estimate.bytes, Some(635_400_000));
    assert_eq!(
        fx.calls(),
        [
            "--context desktop-linux version --format {{.Server.Version}}",
            "--context desktop-linux system df -v --format {{json .}}"
        ]
    );
    assert!(!String::from_utf8_lossy(&out.stdout).contains("vol-a"), "no volume is named");

    let text = fx.ddc(&["disk", "docker"]);
    assert!(text.status.success());
    let text = String::from_utf8_lossy(&text.stdout);
    for line in ["build cache", "A clean frees about", "docker image prune -a", "docker volume prune"] {
        assert!(text.contains(line), "{line:?} not in {text}");
    }
}

#[test]
fn docker_not_answering_exits_0_with_the_reason() {
    if common::unsupported("docker_not_answering_exits_0_with_the_reason") {
        return;
    }
    let fx = Fx::new();
    fs::write(fx.root.join("down"), "").unwrap();
    let out = fx.ddc(&["disk", "docker", "--json"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let contents: DockerContents = contract::holds("docker", &out.stdout);
    assert_eq!(contents.result, DockerResult::NotReady { detail: DOCKER_DOWN.to_owned() });
    assert_eq!(fx.calls().len(), 1, "only the probe: {:?}", fx.calls());

    let text = fx.ddc(&["disk", "docker"]);
    assert!(text.status.success());
    assert!(String::from_utf8_lossy(&text.stdout).contains("Start Docker Desktop to see what is inside."));
}

#[test]
fn no_scan_and_no_dry_run_calls_docker() {
    if common::unsupported("no_scan_and_no_dry_run_calls_docker") {
        return;
    }
    let fx = Fx::new();
    for args in [&["scan", "--json"][..], &["scan", "--background", "--no-notify"], &["clean", "--dry-run"]] {
        let out = fx.ddc(args);
        assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }
    let scan = fx.ddc(&["scan", "--json"]);
    assert!(String::from_utf8_lossy(&scan.stdout).contains("vm.docker-desktop"), "the fixture has the finding");
    assert_eq!(fx.calls(), Vec::<String>::new());
}
