//! What Docker Desktop keeps in its disk image (story "Docker Desktop: what
//! is inside its disk image, and what a clean frees"): a hand-made `docker
//! system df -v --format '{{json .}}'` in the shape docker 29 prints, with
//! synthetic names and IDs, read through a fake `docker`. No real docker
//! runs here.

mod common;

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use common::fakes::FakeCommands;
use ddc_core::docker::{
    self, DF_TIMEOUT, DOCKER_SCHEMA_VERSION, DockerContents, DockerResult, Estimate, Group, SYSTEM_DF,
};
use ddc_core::exec::READY_TIMEOUT;
use ddc_core::rules::{DOCKER_DOWN, DOCKER_READY};
use serde_json::{Value, json};

fn probe_call() -> String {
    DOCKER_READY.args.join(" ")
}

fn df_call() -> String {
    SYSTEM_DF.join(" ")
}

fn image(repo: &str, tag: &str, containers: &str, unique: &str, size: &str) -> Value {
    json!({
        "Containers": containers, "CreatedAt": "2026-10-01 10:00:00 +0000 UTC", "CreatedSince": "5 weeks ago",
        "Digest": "", "ID": "sha256:0000", "Repository": repo, "SharedSize": "1.27GB", "Size": size,
        "Tag": tag, "UniqueSize": unique,
    })
}

fn container(size: &str) -> Value {
    json!({
        "Command": "\"run\"", "CreatedAt": "2026-10-01 10:00:00 +0000 UTC", "ID": "c0ffee", "Image": "example/web:1.0",
        "Labels": "", "LocalVolumes": "1", "Mounts": "vol-a", "Names": "example-web-1", "Networks": "bridge",
        "Platform": "linux/arm64", "Ports": "", "RunningFor": "5 weeks ago", "Size": size, "State": "exited",
        "Status": "Exited (0) 5 weeks ago",
    })
}

fn volume(name: &str, size: &str) -> Value {
    json!({
        "Availability": "N/A", "Driver": "local", "Group": "N/A", "Labels": "", "Links": "1",
        "Mountpoint": "/var/lib/docker/volumes/x/_data", "Name": name, "Scope": "local", "Size": size, "Status": "N/A",
    })
}

fn cache(kind: &str, in_use: &str, shared: &str, size: &str) -> Value {
    json!({
        "CacheType": kind, "CreatedAt": "2026-10-01 10:00:00 +0000 UTC", "CreatedSince": "5 weeks ago",
        "Description": "", "ID": "abc123", "InUse": in_use, "LastUsedAt": "", "LastUsedSince": "",
        "Parent": "", "Shared": shared, "Size": size, "UsageCount": "1",
    })
}

/// Four images (one tagged with a container, one dangling, one dangling
/// that a container uses, one tagged and unused), two containers, three
/// volumes (two docker could not size: `N/A`, and `-1B` as moby prints a
/// failed measure), six build-cache records.
fn df() -> Value {
    json!({
        "Images": [
            image("example/web", "1.0", "1", "1.234GB", "2.5GB"),
            image("<none>", "<none>", "0", "123.4MB", "1.39GB"),
            image("<none>", "<none>", "1", "10MB", "1.28GB"),
            image("example/db", "latest", "0", "1e+03kB", "1.27GB"),
        ],
        "Containers": [container("12.3kB"), container("0B")],
        "Volumes": [volume("vol-a", "1.5GB"), volume("vol-b", "N/A"), volume("vol-c", "-1B")],
        "BuildCache": [
            cache("regular", "false", "false", "512MB"),
            cache("internal", "false", "false", "1MB"),
            cache("frontend", "false", "false", "2MB"),
            cache("regular", "true", "false", "3MB"),
            cache("regular", "false", "true", "100MB"),
            cache("source.local", "false", "false", "4.5kB"),
        ],
    })
}

fn ready_docker(stdout: &str) -> FakeCommands {
    FakeCommands::default().answer("docker", &probe_call(), true, "29.1.2\n").answer("docker", &df_call(), true, stdout)
}

fn run(commands: &FakeCommands) -> DockerContents {
    docker::contents(commands, &AtomicBool::new(false)).expect("no cancel")
}

fn calls(commands: &FakeCommands) -> Vec<String> {
    commands.calls.borrow().clone()
}

#[test]
fn the_totals_and_the_estimate_follow_the_records() {
    let commands = ready_docker(&df().to_string());
    let contents = run(&commands);
    assert_eq!(contents.schema_version, DOCKER_SCHEMA_VERSION);
    assert_eq!(contents.ddc_version, env!("CARGO_PKG_VERSION"));
    assert_eq!(
        contents.result,
        DockerResult::Listed {
            images: Group { count: 4, bytes: Some(1_234_000_000 + 123_400_000 + 10_000_000 + 1_000_000) },
            containers: Group { count: 2, bytes: Some(12_300) },
            volumes: Group { count: 3, bytes: Some(1_500_000_000) },
            build_cache: Group { count: 6, bytes: Some(512_000_000 + 1_000_000 + 2_000_000 + 3_000_000 + 4_500) },
            estimate: Estimate {
                build_cache: 512_000_000 + 4_500,
                dangling_images: Some(123_400_000),
                bytes: Some(512_000_000 + 4_500 + 123_400_000),
            },
        }
    );
    // Exactly two calls: the clean's probe, then the one read.
    assert_eq!(calls(&commands), [format!("docker {}", probe_call()), format!("docker {}", df_call())]);
    assert_eq!(*commands.timeouts.borrow(), [READY_TIMEOUT, DF_TIMEOUT]);
}

#[test]
fn the_output_names_no_image_container_or_volume() {
    let contents = run(&ready_docker(&df().to_string()));
    let text = serde_json::to_string(&contents).unwrap();
    for name in ["example", "vol-a", "vol-b", "vol-c", "c0ffee", "sha256", "abc123", "/var/lib/docker"] {
        assert!(!text.contains(name), "{name} in {text}");
    }
}

#[test]
fn a_size_docker_does_not_know_leaves_its_group_and_the_estimate_unknown() {
    let mut d = df();
    d["Images"][1]["UniqueSize"] = "N/A".into();
    d["Containers"][0]["Size"] = "N/A".into();
    let DockerResult::Listed { images, containers, volumes, estimate, .. } = run(&ready_docker(&d.to_string())).result
    else {
        panic!("not listed");
    };
    assert_eq!(images, Group { count: 4, bytes: None });
    assert_eq!(containers, Group { count: 2, bytes: None });
    assert_eq!(volumes.bytes, Some(1_500_000_000), "a volume without a size is counted, not sized");
    assert_eq!(estimate, Estimate { build_cache: 512_004_500, dangling_images: None, bytes: None });

    // A tagged image without one leaves the estimate known.
    let mut d = df();
    d["Images"][0]["UniqueSize"] = "N/A".into();
    let DockerResult::Listed { estimate, .. } = run(&ready_docker(&d.to_string())).result else { panic!() };
    assert_eq!(estimate.bytes, Some(635_404_500));
}

#[test]
fn a_virtual_size_is_cut_and_empty_sections_count_none() {
    let mut d = df();
    d["Containers"][0]["Size"] = "12.3kB (virtual 1.2GB)".into();
    let DockerResult::Listed { containers, .. } = run(&ready_docker(&d.to_string())).result else { panic!() };
    assert_eq!(containers.bytes, Some(12_300));

    let empty = json!({"Images": null, "Containers": [], "Volumes": null, "BuildCache": []});
    let DockerResult::Listed { images, build_cache, estimate, .. } = run(&ready_docker(&empty.to_string())).result
    else {
        panic!()
    };
    assert_eq!((images, build_cache), (Group { count: 0, bytes: Some(0) }, Group { count: 0, bytes: Some(0) }));
    assert_eq!(estimate, Estimate { build_cache: 0, dangling_images: Some(0), bytes: Some(0) });
}

#[test]
fn what_ddc_cannot_read_is_unreadable_never_zero() {
    let bad_size = {
        let mut d = df();
        d["BuildCache"][0]["Size"] = "512 MB".into();
        d
    };
    let bad_flag = {
        let mut d = df();
        d["BuildCache"][0]["InUse"] = "no".into();
        d
    };
    let cache_without_size = {
        let mut d = df();
        d["BuildCache"][0]["Size"] = "N/A".into();
        d
    };
    let missing_field = {
        let mut d = df();
        d["Images"][0].as_object_mut().unwrap().remove("UniqueSize");
        d
    };
    for stdout in [
        "not json".to_owned(),
        "[]".to_owned(),
        String::new(),
        bad_size.to_string(),
        bad_flag.to_string(),
        cache_without_size.to_string(),
        missing_field.to_string(),
    ] {
        let commands = ready_docker(&stdout);
        let result = run(&commands).result;
        assert!(matches!(result, DockerResult::Unreadable { .. }), "{stdout:?}: {result:?}");
        assert_eq!(calls(&commands).len(), 2, "no third call");
    }
}

#[test]
fn when_docker_does_not_answer_it_says_why_and_reads_nothing() {
    let probe = probe_call();
    let said =
        |stderr: &str| FakeCommands::default().answer("docker", &probe, false, "").stderr_on("docker", &probe, stderr);
    let not_ready = |detail: &str| DockerResult::NotReady { detail: detail.to_owned() };
    for (commands, expected) in [
        (FakeCommands::default(), DockerResult::NotInstalled),
        (
            said("Cannot connect to the Docker daemon at unix:///x/docker.sock. Is the docker daemon running?\n"),
            not_ready(DOCKER_DOWN),
        ),
        (
            said(
                "failed to connect to the docker API at unix:///x/.docker/run/docker.sock; check if the path is correct and if the daemon is running\n",
            ),
            not_ready(DOCKER_DOWN),
        ),
        (said("context \"desktop-linux\" does not exist\n"), not_ready(DOCKER_READY.no_context)),
        (
            said("error during connect: the engine is updating\n"),
            not_ready("error during connect: the engine is updating"),
        ),
        (FakeCommands::default().error_on("docker", &probe, io::ErrorKind::TimedOut), not_ready(DOCKER_DOWN)),
        (FakeCommands::default().answer("docker", &probe, true, "\n"), not_ready(DOCKER_DOWN)),
    ] {
        assert_eq!(run(&commands).result, expected);
        assert!(calls(&commands).len() <= 1, "only the probe: {:?}", calls(&commands));
        assert!(!commands.ran(&format!("docker {}", df_call())));
    }
}

#[test]
fn a_read_that_fails_or_hangs_says_so() {
    let df = df_call();
    let failing = FakeCommands::default()
        .answer("docker", &probe_call(), true, "29.1.2\n")
        .answer("docker", &df, false, "")
        .stderr_on("docker", &df, "Error response from daemon: a disk usage operation is already running\n");
    assert_eq!(
        run(&failing).result,
        DockerResult::Failed { detail: "Error response from daemon: a disk usage operation is already running".into() }
    );
    assert_eq!(calls(&failing).len(), 2);

    // The engine went away between the probe and the read: no socket path.
    let gone = FakeCommands::default()
        .answer("docker", &probe_call(), true, "29.1.2\n")
        .answer("docker", &df, false, "")
        .stderr_on("docker", &df, "failed to connect to the docker API at unix:///x/.docker/run/docker.sock; check if the daemon is running\n");
    assert_eq!(run(&gone).result, DockerResult::Failed { detail: DOCKER_DOWN.into() });

    let hanging = FakeCommands::default().answer("docker", &probe_call(), true, "29.1.2\n").error_on(
        "docker",
        &df,
        io::ErrorKind::TimedOut,
    );
    assert_eq!(run(&hanging).result, DockerResult::TimedOut { seconds: DF_TIMEOUT.as_secs() });
    assert_eq!(calls(&hanging).len(), 2);
}

#[test]
fn a_cancel_during_the_probe_runs_no_read() {
    let cancel = Arc::new(AtomicBool::new(false));
    let set = Arc::clone(&cancel);
    let mut commands = ready_docker(&df().to_string());
    commands.on_call = Some(Box::new(move |_| set.store(true, Ordering::SeqCst)));
    assert_eq!(docker::contents(&commands, &cancel), None);
    assert_eq!(calls(&commands).len(), 1, "{:?}", calls(&commands));
}

#[test]
fn the_json_is_tagged_by_status() {
    let contents = run(&ready_docker(&df().to_string()));
    let v = serde_json::to_value(&contents).unwrap();
    assert_eq!(v["result"]["status"], "listed");
    assert_eq!(v["result"]["build_cache"]["count"], 6);
    assert_eq!(v["result"]["estimate"]["bytes"], 635_404_500);
    let v = serde_json::to_value(run(&FakeCommands::default())).unwrap();
    assert_eq!(v["result"], json!({"status": "not-installed"}));
}

#[test]
#[allow(clippy::disallowed_methods)] // DDC_UPDATE_SCHEMA regenerates the committed file
fn committed_docker_schema_is_current() {
    let schema = serde_json::to_string_pretty(&schemars::schema_for!(DockerContents)).unwrap() + "\n";
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/docker.schema.json");
    if std::env::var_os("DDC_UPDATE_SCHEMA").is_some() {
        std::fs::write(&path, &schema).unwrap();
    }
    let committed = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(committed == schema, "docs/docker.schema.json is stale: run DDC_UPDATE_SCHEMA=1 cargo test");
}
